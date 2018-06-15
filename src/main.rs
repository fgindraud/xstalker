#![deny(deprecated)]
extern crate chrono;
extern crate tokio;
use std::cell::RefCell;
use std::fs;
use std::fs::File;
use std::io;
use std::io::{BufRead, Read, Seek, Write};
use std::path::Path;
use std::rc::Rc;
use std::str::FromStr;
use std::time;
use tokio::prelude::*;

#[derive(Debug)]
pub struct ActiveWindowMetadata {
    title: Option<String>,
    class: Option<String>,
}

/// Xcb interface
mod xcb_stalker;
use xcb_stalker::ActiveWindowChanges;

/** Classifier: stores rules used to determine categories for time spent.
 * Rules are stored in an ordered list.
 * The first matching rule in the list chooses the category.
 * A category can appear in multiple rules.
 */
struct Classifier {
    filters: Vec<(String, Box<Fn(&ActiveWindowMetadata) -> bool>)>,
}

impl Classifier {
    /// Create a new classifier with no rules.
    fn new() -> Self {
        Classifier {
            filters: Vec::new(),
        }
    }
    /// Add a rule at the end of the list, for the given category.
    fn append_filter<F>(&mut self, category: &str, filter: F)
    where
        F: 'static + Fn(&ActiveWindowMetadata) -> bool,
    {
        self.filters
            .push((String::from(category), Box::new(filter)));
    }
    /// Return the list of all defined categories, unique.
    fn categories(&self) -> Vec<&str> {
        let mut categories: Vec<&str> = self.filters
            .iter()
            .map(|(category, _)| category.as_str())
            .collect();
        categories.sort();
        categories.dedup();
        categories
    }
    /// Determine the category for the given window metadata.
    fn classify(&self, metadata: &ActiveWindowMetadata) -> Option<&str> {
        self.filters
            .iter()
            .find(|(_category, filter)| filter(metadata))
            .map(|(category, _filter)| category.as_str())
    }
}

// Shorter io::Error creation
fn bad_data<E>(error: E) -> io::Error
where
    E: Into<Box<std::error::Error + Send + Sync>>,
{
    io::Error::new(io::ErrorKind::InvalidData, error)
}

fn has_unique_elements<T>(sequence: &[T]) -> bool
where
    T: PartialEq<T>,
{
    sequence.into_iter().all(|tested_element| {
        sequence
            .into_iter()
            .filter(|element| *element == tested_element)
            .count() == 1
    })
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
 * Time spent in each categories is stored by time window.
 * Time spent is stored in seconds.
 * TODO document format
 */
struct Database {
    file: File,
    last_line_start_offset: usize,
    file_len: usize,
    categories: Vec<String>,
}

/// Time windows are timezone aware, in system local timezone.
type DatabaseTime = chrono::DateTime<chrono::Local>;

impl Database {
    /// Open a database
    pub fn open(path: &Path, classifier_categories: Vec<&str>) -> io::Result<Self> {
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
                    // TODO add categories at the end, rewrite db with 0s in new categories
                    unimplemented!()
                }
            }
            Err(ref e) if e.kind() == io::ErrorKind::NotFound => {
                Database::create_new(path, classifier_categories)
            }
            Err(e) => Err(e),
        }
    }

    /// Create a new database
    pub fn create_new(path: &Path, classifier_categories: Vec<&str>) -> io::Result<Self> {
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
            categories: classifier_categories
                .into_iter()
                .map(|s| s.into())
                .collect(),
        })
    }

    /// Get database categories, in order
    pub fn categories(&self) -> &Vec<String> {
        &self.categories
    }

    /// Parse header line, return categories and header line len.
    fn parse_categories(reader: &mut io::BufReader<File>) -> io::Result<(Vec<String>, usize)> {
        let mut header = String::new();
        let header_len = reader.read_line(&mut header)?;
        // Line must exist, must be '\n'-terminated, must contain at least 'time' header.
        if header_len == 0 {
            return Err(bad_data("database has no header line"));
        }
        if header.pop() != Some('\n') {
            return Err(bad_data("database header line is not newline terminated"));
        }
        let mut elements = header.split('\t');
        if let Some(_time_header) = elements.next() {
            let categories: Vec<String> = elements.map(|s| s.into()).collect();
            if has_unique_elements(&categories) {
                Ok((categories, header_len))
            } else {
                Err(bad_data("database categories must be unique"))
            }
        } else {
            Err(bad_data("database header has no field"))
        }
    }

    /// Check db entries, return (last_line_start_offset, file_len)
    /// Assume reader cursor is at start of second line.
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
                    "database entry at line {}: not newline terminated",
                    line_nb
                )));
            }
            if line.split('\t').count() != nb_categories + 1 {
                return Err(bad_data(format!(
                    "database entry at line {}: field count mismatch",
                    line_nb
                )));
            }
            line_nb += 1;
            offset += prev_line_len;
            prev_line_len = line_len;
        }
    }

    /// Parse the last entry of the database file.
    /// If entry is correct: return time window start and duration for categories.
    /// If entry is empty: return None.
    /// If entry is incorrect: error.
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
            return Err(bad_data("database: last entry: not newline terminated"));
        }
        let mut elements = line.split('\t');
        if let Some(time_window_text) = elements.next() {
            let time_window = DatabaseTime::from_str(time_window_text).map_err(|err| {
                bad_data(format!(
                    "database: last entry: cannot parse time window: {}",
                    err
                ))
            })?;
            // Read durations of entry
            let mut durations = Vec::with_capacity(self.categories.len());
            for s in elements {
                let seconds = u64::from_str(s).map_err(|err| {
                    bad_data(format!(
                        "database: last entry: cannot parse category duration: {}",
                        err
                    ))
                })?;
                durations.push(time::Duration::from_secs(seconds))
            }
            if durations.len() != self.categories.len() {
                return Err(bad_data("database: last entry: field count mismatch"));
            }
            Ok(Some((time_window, durations)))
        } else {
            Err(bad_data("database: last entry is empty"))
        }
    }

    /// Rewrite the last entry in the database, return the entry len including newline.
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
        // Update db file
        self.file
            .seek(io::SeekFrom::Start(self.last_line_start_offset as u64))?;
        self.file.write_all(line.as_bytes())?;
        self.file_len = self.last_line_start_offset + line.len();
        self.file.set_len(self.file_len as u64)?;
        self.file.sync_all()
    }

    pub fn lock_last_entry(&mut self) {
        self.last_line_start_offset = self.file_len
    }
}

struct CategoryDurationCounter {
    current_category_index: Option<usize>, // Index in duration_by_category
    last_category_update: time::Instant,
    categories: Vec<String>,
    durations: Vec<time::Duration>,
}

impl CategoryDurationCounter {
    /// Create a new time tracking structure.
    /// Starts with no defined category.
    pub fn new(categories: &[String]) -> Self {
        CategoryDurationCounter {
            current_category_index: None,
            last_category_update: time::Instant::now(),
            categories: categories.into_iter().cloned().collect(),
            durations: std::iter::repeat(time::Duration::new(0, 0))
                .take(categories.len())
                .collect(),
        }
    }

    pub fn durations(&self) -> &Vec<time::Duration> {
        &self.durations
    }

    pub fn set_durations(&mut self, durations: Vec<time::Duration>) {
        self.durations = durations
    }

    pub fn reset_durations(&mut self) {
        for mut d in &mut self.durations {
            *d = time::Duration::new(0, 0)
        }
    }

    pub fn category_changed(&mut self, category: Option<&str>) {
        println!("Category change: {:?}", category);
        let now = time::Instant::now();
        if let Some(index) = self.current_category_index {
            self.durations[index] += now.duration_since(self.last_category_update)
        }
        self.current_category_index = category.map(|ref s| {
            self.categories
                .iter()
                .enumerate()
                .find(|(_i, category_name)| category_name == s)
                .unwrap()
                .0
        });
        self.last_category_update = now
    }
}

fn write_durations_to_disk(
    db: &mut Database,
    duration_counter: &CategoryDurationCounter,
    window_start: &DatabaseTime,
) -> io::Result<()> {
    println!("Write to disk");

    db.rewrite_last_entry(window_start, duration_counter.durations())
}

fn change_time_window(
    db: &mut Database,
    duration_counter: &mut CategoryDurationCounter,
    window_start: &mut DatabaseTime,
    time_window_size: time::Duration,
) -> io::Result<()> {
    // Flush current durations values
    write_durations_to_disk(db, duration_counter, window_start)?;
    // Create a new time window
    db.lock_last_entry();
    duration_counter.reset_durations();
    *window_start = *window_start + chrono::Duration::from_std(time_window_size).unwrap();
    Ok(())
}

fn main() -> io::Result<()> {
    // TODO wrap in function that takes config, and return io::Result<()>
    // main should parse args and print errors

    // Config
    let time_window_size = time::Duration::from_secs(3600);
    let db_write_interval = time::Duration::from_secs(10);

    // Setup test classifier
    let mut classifier = Classifier::new();
    classifier.append_filter(&"coding", |md| {
        md.class
            .as_ref()
            .map(|class| class == "konsole")
            .unwrap_or(false)
    });
    classifier.append_filter(&"unknown", |_| true);

    // Setup state
    let mut db = Database::open(Path::new("test"), classifier.categories())?;
    let mut duration_counter = CategoryDurationCounter::new(db.categories());

    // Determine boundary of time windows
    let now = DatabaseTime::from(time::SystemTime::now());

    let window_start = {
        if let Some((time, durations)) = db.get_last_entry()? {
            if time <= now && now < time + chrono::Duration::from_std(time_window_size).unwrap() {
                // We are still in the time window of the last entry, resume the window.
                duration_counter.set_durations(durations);
                time
            } else {
                // Outside of last entry time window: create a new window.
                // This includes the case where now < time (timezone change, system clock adjustement).
                db.lock_last_entry();
                now
            }
        } else {
            // No last entry: create new window.
            now
        }
    };
    let duration_to_next_window_change = time_window_size
        - chrono::Duration::to_std(&now.signed_duration_since(window_start)).unwrap();

    // State is shared between tasks in tokio.
    // Rc = single thread, RefCell for mutability when needed.
    let db = Rc::new(RefCell::new(db));
    let duration_counter = Rc::new(RefCell::new(duration_counter));
    let window_start = Rc::new(RefCell::new(window_start));

    // Create a tokio runtime to implement an event loop.
    // Single threaded is enough.
    // TODO support signals using tokio_signal crate ?
    let mut runtime = tokio::runtime::current_thread::Runtime::new()?;
    {
        let duration_counter = Rc::clone(&duration_counter);
        // Listen to active window changes.
        // On each window change, update the duration_counter
        let active_window_changes = ActiveWindowChanges::new()?;
        // Get initial category
        {
            let metadata = active_window_changes.get_current_metadata()?;
            let category = classifier.classify(&metadata);
            duration_counter.borrow_mut().category_changed(category);
        }
        // Asynchronous stream of reactions to changes
        let task = active_window_changes
            .for_each(move |active_window| {
                let category = classifier.classify(&active_window);
                duration_counter.borrow_mut().category_changed(category);
                Ok(())
            })
            .map_err(|err| panic!("ActiveWindowChanges listener failed:\n{}", err));
        runtime.spawn(task);
    }
    {
        let db = Rc::clone(&db);
        let duration_counter = Rc::clone(&duration_counter);
        let window_start = Rc::clone(&window_start);
        // Periodically write database to file
        let task = tokio::timer::Interval::new(
            time::Instant::now() + db_write_interval,
            db_write_interval,
        ).for_each(move |_instant| {
            write_durations_to_disk(
                &mut db.borrow_mut(),
                &duration_counter.borrow(),
                &window_start.borrow(),
            ).unwrap();
            Ok(())
        })
            .map_err(|err| panic!("Write to database file failed:\n{}", err));
        runtime.spawn(task);
    }
    {
        let db = Rc::clone(&db);
        let duration_counter = Rc::clone(&duration_counter);
        let window_start = Rc::clone(&window_start);
        // Periodically change time window (TODO write to file before)
        let task = tokio::timer::Interval::new(
            time::Instant::now() + duration_to_next_window_change,
            time_window_size,
        ).for_each(move |_instant| {
            change_time_window(
                &mut db.borrow_mut(),
                &mut duration_counter.borrow_mut(),
                &mut window_start.borrow_mut(),
                time_window_size,
            ).unwrap();
            Ok(())
        })
            .map_err(|err| panic!("Change time window failed:\n{}", err));
        runtime.spawn(task);
    }
    Ok(runtime.run().expect("tokio runtime failure"))
}
