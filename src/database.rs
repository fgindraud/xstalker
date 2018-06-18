use super::UniqueCategories;
use chrono;
use std;
use std::fs;
use std::fs::File;
use std::io;
use std::io::{BufRead, Read, Seek, Write};
use std::path::Path;
use std::str::FromStr;
use std::time;

// io::Error with InvalidData is used for DB formatting errors. Shorten creation.
fn bad_data<E>(error: E) -> io::Error
where
    E: Into<Box<std::error::Error + Send + Sync>>,
{
    io::Error::new(io::ErrorKind::InvalidData, error)
}

fn is_subset_of<A, B>(subset: &[A], superset: &[B]) -> bool
where
    A: PartialEq<B>,
{
    subset
        .into_iter()
        .all(|a_element| superset.into_iter().any(|b_element| a_element == b_element))
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
    file: File,
    last_line_start_offset: usize,
    file_len: usize,
    categories: UniqueCategories,
}

/// Time windows are timezone aware, in system local timezone.
pub type DatabaseTime = chrono::DateTime<chrono::Local>;

impl Database {
    /** Open a database.
     * If the database does not exist, create a new one.
     * If the database exist and is compatible (contains the requested categories), use it.
     * If it exists but is not compatible, add the new categories. TODO
     */
    pub fn open(path: &Path, classifier_categories: UniqueCategories) -> io::Result<Self> {
        match fs::OpenOptions::new().read(true).write(true).open(path) {
            Ok(f) => {
                let mut reader = io::BufReader::new(f);
                let (db_categories, header_len) = Database::parse_categories(&mut reader)?;
                if is_subset_of(&classifier_categories, &db_categories) {
                    let (last_line_start_offset, file_len) =
                        Database::scan_db_entries(&mut reader, header_len, db_categories.len())?;
                    Ok(Database {
                        file: reader.into_inner(),
                        last_line_start_offset: last_line_start_offset,
                        file_len: file_len,
                        categories: db_categories,
                    })
                } else {
                    unimplemented!()
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
        let mut f = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)?;
        let header = format!("time_window\t{}\n", classifier_categories.join("\t"));
        f.write_all(header.as_bytes())?;
        Ok(Database {
            file: f,
            last_line_start_offset: header.len(),
            file_len: header.len(),
            categories: classifier_categories,
        })
    }

    /// Get database categories, ordered by column index.
    pub fn categories(&self) -> &UniqueCategories {
        &self.categories
    }

    /// Parse header line, return categories and header line len.
    fn parse_categories(reader: &mut io::BufReader<File>) -> io::Result<(UniqueCategories, usize)> {
        let mut header = String::new();
        let header_len = reader.read_line(&mut header)?;
        // Line must exist, must be '\n'-terminated, must contain at least 'time' header.
        if header_len == 0 {
            return Err(bad_data("No header line"));
        }
        if header.pop() != Some('\n') {
            return Err(bad_data("Header line is not newline terminated"));
        }
        let mut elements = header.split('\t');
        if let Some(_time_header) = elements.next() {
            match UniqueCategories::from_unique(elements.map(|s| s.into()).collect()) {
                Ok(categories) => Ok((categories, header_len)),
                Err(e) => Err(bad_data(e)),
            }
        } else {
            Err(bad_data("Header has no field"))
        }
    }

    /** Check db entries, return (last_line_start_offset, file_len)
     * Assume reader cursor is at start of second line.
     */
    fn scan_db_entries(
        reader: &mut io::BufReader<File>,
        header_len: usize,
        nb_categories: usize,
    ) -> io::Result<(usize, usize)> {
        let mut line = String::new();
        let mut line_nb = 2; // Start at line 2
        let mut offset = header_len;
        let mut prev_line_len = 0;
        loop {
            let line_len = reader.read_line(&mut line)?;
            // Entry line must be either empty, or be '\n'-terminated and have the right fields
            if line_len == 0 {
                return Ok((offset, offset + prev_line_len));
            }
            if line.pop() != Some('\n') {
                return Err(bad_data(format!(
                    "Line {}: Not newline terminated",
                    line_nb
                )));
            }
            let nb_fields = line.split('\t').count();
            if nb_fields != nb_categories + 1 {
                return Err(bad_data(format!(
                    "Line {}: expected {} fields, got {}: {:?}",
                    line_nb,
                    nb_categories + 1,
                    nb_fields,
                    line
                )));
            }
            line.clear(); // Reset buffer for next line (read_line appends content)
            line_nb += 1;
            offset += prev_line_len;
            prev_line_len = line_len;
        }
    }

    /** Parse the last entry of the database file.
     * If entry is correct: return time window start and duration for categories.
     * If entry is empty: return None.
     * If entry is incorrect: error.
     */
    pub fn get_last_entry(&mut self) -> io::Result<Option<(DatabaseTime, Vec<time::Duration>)>> {
        self.file
            .seek(io::SeekFrom::Start(self.last_line_start_offset as u64))?;
        let mut line = String::new();
        let line_len = self.file.read_to_string(&mut line)?;
        if line_len == 0 {
            // Empty database is ok.
            return Ok(None);
        }
        // If line exists, it must be '\n'-terminated, must contain time + categories durations
        if line.pop() != Some('\n') {
            return Err(bad_data("Entry is not newline terminated"));
        }
        let mut elements = line.split('\t');
        if let Some(time_window_text) = elements.next() {
            let time_window = DatabaseTime::from_str(time_window_text)
                .map_err(|err| bad_data(format!("Cannot parse time window: {}", err)))?;
            // Read durations of entry
            let mut durations = Vec::with_capacity(self.categories.len());
            for s in elements {
                let seconds = u64::from_str(s)
                    .map_err(|err| bad_data(format!("Cannot parse category duration: {}", err)))?;
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
        } else {
            Err(bad_data("Entry is empty"))
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
        // Write line to end of file, removing any excess data
        self.file
            .seek(io::SeekFrom::Start(self.last_line_start_offset as u64))?;
        self.file.write_all(line.as_bytes())?;
        self.file_len = self.last_line_start_offset + line.len();
        self.file.set_len(self.file_len as u64)?;
        self.file.sync_all() // May be costly, but we do not call that often...
    }

    /// Move the last line cursor to the next line, locking the current last line content.
    pub fn lock_last_entry(&mut self) {
        self.last_line_start_offset = self.file_len
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
    last_category_update: time::Instant,
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
            last_category_update: time::Instant::now(),
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

    /** Record a change in active window.
     * Duration for the previous category is accumulated to the table, if not undefined.
     * Assumes that the category name is in the set given to new().
     */
    pub fn category_changed(&mut self, category: Option<String>) {
        let now = time::Instant::now();
        if let Some(index) = self.current_category_index {
            self.durations[index] += now.duration_since(self.last_category_update)
        }
        self.current_category_index = category.map(|ref s| {
            self.categories
                .iter()
                .enumerate()
                .find(|(_i, category_name)| *category_name == s)
                .expect("category name is unknown")
                .0
        });
        self.last_category_update = now
    }
}
