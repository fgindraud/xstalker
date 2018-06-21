use super::UniqueCategories;
use chrono;
use std;
use std::fs;
use std::fs::File;
use std::io;
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, Write};
use std::path::Path;
use std::time;

// io::Error with InvalidData is used for DB formatting errors. Shorten creation.
fn bad_data<E>(error: E) -> io::Error
where
    E: Into<Box<std::error::Error + Send + Sync>>,
{
    io::Error::new(io::ErrorKind::InvalidData, error)
}

// TODO WIP

struct LineCounts {
    last_line_start_offset: usize,
    last_line_len: usize,
    line_nb: usize,
}

impl LineCounts {
    fn new() -> Self {
        LineCounts {
            last_line_start_offset: 0,
            last_line_len: 0,
            line_nb: 0,
        }
    }
    fn advance(&mut self, line_len: usize) {
        self.last_line_start_offset += self.last_line_len;
        self.last_line_len = line_len;
        self.line_nb += 1;
    }
    fn ignore_last_line(&mut self) {
        self.last_line_start_offset += self.last_line_len;
        self.last_line_len = 0;
    }
    fn cursor(&self) -> usize {
        self.last_line_start_offset + self.last_line_len
    }
}

struct LineCounted<F> {
    f: F,
    counts: LineCounts,
}

fn seek_to_offset<F: Seek>(f: &mut F, offset: usize) -> io::Result<()> {
    f.seek(io::SeekFrom::Start(offset as u64)).map(|_| ())
}

impl<F: Seek> LineCounted<F> {
    /// Wrap file, assuming its cursor is at the start
    fn new(f: F) -> Self {
        LineCounted {
            f: f,
            counts: LineCounts::new(),
        }
    }
    /// Wrap file, resetting its cursor
    fn reset_file(mut f: F) -> io::Result<Self> {
        seek_to_offset(&mut f, 0)?;
        Ok(LineCounted::new(f))
    }
    /// Unwraps inner file object
    fn into_inner(self) -> F {
        self.f
    }

    /// Read a line. Stores content into buf.
    fn read_line(&mut self, buf: &mut String) -> io::Result<()>
    where
        F: BufRead,
    {
        buf.clear(); // BufRead read_line appends, remove old content
        let line_len = self.f.read_line(buf)?;
        self.counts.advance(line_len);
        Ok(())
    }
    /// Write data as a line (should end with \n, not appended).
    fn write_line(&mut self, line: &[u8]) -> io::Result<()>
    where
        F: Write,
    {
        self.f.write_all(line)?;
        self.counts.advance(line.len());
        Ok(())
    }

    // TODO if BufWrite writer for Iter<Item=&str>

    /// Read from last line to end of file.
    /// Does not change counts.
    fn read_from_last_line(&mut self, buf: &mut String) -> io::Result<()>
    where
        F: Read,
    {
        seek_to_offset(&mut self.f, self.counts.last_line_start_offset)?;
        self.f.read_to_string(buf).map(|_| ())
    }
}
impl LineCounted<File> {
    /// Rewrite last line content.
    fn rewrite_last_line(&mut self, line: &[u8]) -> io::Result<()> {
        seek_to_offset(&mut self.f, self.counts.last_line_start_offset)?;
        self.f.write_all(line)?;
        self.counts.last_line_len = line.len();
        self.f.set_len(self.counts.cursor() as u64)?; // Trim file len
        self.f.sync_all() // May be costly, but we do not call that often...
    }
}
impl<F: Read + Seek> LineCounted<BufReader<F>> {
    /// Unwrap the BufReader object, keeping the counts
    fn unbuffered(self) -> io::Result<LineCounted<F>> {
        // Seek to tracked cursor, as BufReader may have read further
        let mut f = self.f.into_inner();
        seek_to_offset(&mut f, self.counts.cursor())?;
        Ok(LineCounted {
            f: f,
            counts: self.counts,
        })
    }
}
impl<F: Write> LineCounted<BufWriter<F>> {
    /// Unwrap the BufWriter object, keeping the counts
    fn unbuffered(self) -> io::Result<LineCounted<F>> {
        Ok(LineCounted {
            f: self.f.into_inner()?,
            counts: self.counts,
        })
    }
}

/** Time spent Database.
 * Time spent in each categories is stored by time window, in seconds.
 *
 * Database is a text file with a header line, and one entry for each subsequent lines.
 * Each line is tab-separated into columns.
 * The first column is the time window start, in rfc3339 format.
 * The next columns represent the time spent in each category, in seconds (integer).
 * The header line contain the category name for each columns.
 * Each category must be uniquely named.
 */
pub struct Database {
    file: LineCounted<File>,
    categories: UniqueCategories,
}

/// Time windows are timezone aware, in system local timezone.
pub type DatabaseTime = chrono::DateTime<chrono::Local>;

impl Database {
    /** Open a database.
     * If the database does not exist, create a new one.
     * If the database exist and is compatible (contains the requested categories), use it.
     * If it exists but is not compatible, add the new categories.
     */
    pub fn open(path: &Path, classifier_categories: UniqueCategories) -> io::Result<Self> {
        match fs::OpenOptions::new().read(true).write(true).open(path) {
            Ok(f) => {
                let mut reader = LineCounted::new(BufReader::new(f));
                let mut db_categories = Database::parse_categories(&mut reader)?;
                let nb_missing_categories = db_categories.extend(classifier_categories);
                if nb_missing_categories == 0 {
                    // Can reuse the database as it is
                    reader.counts.ignore_last_line(); // Skip header
                    Database::scan_entries(&mut reader, db_categories.len())?;
                    Ok(Database {
                        file: reader.unbuffered()?,
                        categories: db_categories,
                    })
                } else {
                    // Put file content in memory
                    let mut reader = reader.into_inner(); // Destroy counts
                    let mut entry_lines = String::new();
                    reader.read_to_string(&mut entry_lines)?;
                    // Rewrite file TODO scan ?
                    let entry_suffix: String = std::iter::repeat("\t0")
                        .take(nb_missing_categories)
                        .collect();
                    let mut writer = LineCounted::reset_file(BufWriter::new(reader.into_inner()))?;
                    writer.write_line(
                        format!("time_window\t{}\n", db_categories.join("\t")).as_bytes(),
                    )?;
                    for entry in entry_lines.lines() {
                        writer.write_line(format!("{}{}\n", entry, entry_suffix).as_bytes())?;
                    }
                    Ok(Database {
                        file: writer.unbuffered()?,
                        categories: db_categories,
                    })
                }
            }
            Err(ref e) if e.kind() == io::ErrorKind::NotFound => {
                Database::create_new(path, classifier_categories)
            }
            Err(e) => Err(e),
        }
    }

    /** Create a new empty database with the specified categories.
     * Creates parent directories if needed.
     */
    pub fn create_new(path: &Path, classifier_categories: UniqueCategories) -> io::Result<Self> {
        if let Some(dir) = path.parent() {
            fs::DirBuilder::new().recursive(true).create(dir)?
        }
        let f = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)?;
        let mut f = LineCounted::new(f);
        f.write_line(format!("time_window\t{}\n", classifier_categories.join("\t")).as_bytes())?;
        f.counts.ignore_last_line(); // Skip header
        Ok(Database {
            file: f,
            categories: classifier_categories,
        })
    }

    /// Get database categories, ordered by column index.
    pub fn categories(&self) -> &UniqueCategories {
        &self.categories
    }

    /// Parse header line, return categories and header line len.
    fn parse_categories(reader: &mut LineCounted<BufReader<File>>) -> io::Result<UniqueCategories> {
        let mut header = String::new();
        reader.read_line(&mut header)?;
        // Line must exist, must be '\n'-terminated, must contain at least 'time' header.
        match header.pop() {
            Some('\n') => {
                let mut elements = header.split('\t');
                match elements.next() {
                    Some(_time_header) => UniqueCategories::from_unique(
                        elements.map(|s| s.into()).collect(),
                    ).map_err(|e| bad_data(e)),
                    None => Err(bad_data("Header has no field")),
                }
            }
            None => Err(bad_data("No header line")),
            _ => Err(bad_data("Header line is not newline terminated")),
        }
    }

    /// Check db entries. Assume reader cursor is at first entry line.
    fn scan_entries(
        reader: &mut LineCounted<BufReader<File>>,
        nb_categories: usize,
    ) -> io::Result<()> {
        let mut line = String::new();
        loop {
            reader.read_line(&mut line)?;
            // Entry line must be either empty, or be '\n'-terminated and have the right fields
            match line.pop() {
                Some('\n') => {
                    // Check field count
                    let nb_fields = line.split('\t').count();
                    if nb_fields != nb_categories + 1 {
                        return Err(bad_data(format!(
                            "Line {}: expected {} fields, got {}: {:?}",
                            reader.counts.line_nb + 1,
                            nb_categories + 1,
                            nb_fields,
                            line
                        )));
                    }
                }
                None => return Ok(()), // Empty last line
                _ => {
                    return Err(bad_data(format!(
                        "Line {}: Not newline terminated",
                        reader.counts.line_nb + 1
                    )))
                }
            }
        }
    }

    /** Parse the last entry of the database file.
     * If entry is correct: return time window start and duration for categories.
     * If entry is empty: return None.
     * If entry is incorrect: error.
     */
    pub fn get_last_entry(&mut self) -> io::Result<Option<(DatabaseTime, Vec<time::Duration>)>> {
        let mut line = String::new();
        self.file.read_from_last_line(&mut line)?;

        // If line exists, it must be '\n'-terminated, must contain time + categories durations
        match line.pop() {
            Some('\n') => {
                let mut elements = line.split('\t');
                match elements.next() {
                    Some(time_window_text) => {
                        let time_window: DatabaseTime = time_window_text
                            .parse()
                            .map_err(|err| bad_data(format!("Cannot parse time window: {}", err)))?;
                        // Read durations of entry
                        let mut durations = Vec::with_capacity(self.categories.len());
                        for s in elements {
                            let seconds: u64 = s.parse().map_err(|err| {
                                bad_data(format!("Cannot parse category duration: {}", err))
                            })?;
                            durations.push(time::Duration::from_secs(seconds))
                        }
                        if durations.len() != self.categories.len() {
                            return Err(bad_data(format!(
                                "Durations: expected {} fields, got {}",
                                self.categories.len(),
                                durations.len()
                            )));
                        }
                        Ok(Some((time_window, durations)))
                    }
                    None => Err(bad_data("Entry is empty")),
                }
            }
            None => Ok(None), // Empty database
            _ => Err(bad_data("Entry is not newline terminated")),
        }
    }

    /// Rewrite the last entry in the database.
    pub fn rewrite_last_entry(
        &mut self,
        window_start: &DatabaseTime,
        durations: &[time::Duration],
    ) -> io::Result<()> {
        // Build line text
        let mut line = window_start.to_rfc3339();
        for d in durations {
            use std::fmt::Write;
            write!(&mut line, "\t{}", d.as_secs()).unwrap();
        }
        line.push('\n');
        self.file.rewrite_last_line(line.as_bytes())
    }

    /// Move the last line cursor to the next line, locking the current last line content.
    pub fn lock_last_entry(&mut self) {
        self.file.counts.ignore_last_line()
    }
}

/** Category duration counter.
 * Stores durations for each category in memory.
 * This is used to store the durations for the current time window.
 * Changes in active window are recorded in this structure.
 * Asynchronously, the accumulated durations are written to the database.
 */
pub struct CategoryDurationCounter {
    current_category_index: Option<usize>, // Index for categories / durations
    last_recorded: time::Instant,          // Last time where durations were stored in durations vec
    categories: UniqueCategories,
    durations: Vec<time::Duration>,
}

impl CategoryDurationCounter {
    /** Create a new counter for the given categories.
     * All durations are initialized to 0.
     * The current category is set to undefined.
     */
    pub fn new(categories: UniqueCategories) -> Self {
        let zeroed_durations = std::iter::repeat(time::Duration::new(0, 0))
            .take(categories.len())
            .collect();
        CategoryDurationCounter {
            current_category_index: None,
            last_recorded: time::Instant::now(),
            categories: categories,
            durations: zeroed_durations,
        }
    }

    /// Access accumulated durations. durations[i] is duration for categories[i].
    pub fn durations(&self) -> &Vec<time::Duration> {
        &self.durations
    }

    /// Set values for all durations. For resuming a time window from database.
    pub fn set_durations(&mut self, durations: Vec<time::Duration>) {
        assert_eq!(durations.len(), self.categories.len());
        self.durations = durations
    }

    /// Set all durations to 0. For time window change.
    pub fn reset_durations(&mut self) {
        for mut d in &mut self.durations {
            *d = time::Duration::new(0, 0)
        }
    }

    /// Record duration for current category from last_recorded to timestamp.
    pub fn record_current_duration(&mut self, timestamp: time::Instant) {
        if let Some(index) = self.current_category_index {
            self.durations[index] += timestamp.duration_since(self.last_recorded)
        }
        self.last_recorded = timestamp;
    }

    /** Record a change in active window.
     * Store durations for the previous category up to now, then changes current category.
     * Assumes that the category name is in the set given to new().
     */
    pub fn category_changed<S: AsRef<str>>(
        &mut self,
        category: Option<S>,
        timestamp: time::Instant,
    ) {
        self.record_current_duration(timestamp);
        self.current_category_index = category.map(|s| {
            self.categories
                .iter()
                .enumerate()
                .find(|(_i, category_name)| category_name.as_str() == s.as_ref())
                .expect("category name is unknown")
                .0
        });
    }
}
