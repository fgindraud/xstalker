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

fn elapsed_is_less_than(
    new: &chrono::DateTime<chrono::Local>,
    old: &chrono::DateTime<chrono::Local>,
    duration: time::Duration,
) -> bool {
    *old + chrono::Duration::from_std(duration).unwrap() > *new
}

/** Database.
 * TODO document format
 * Time spent is stored in seconds.
 */
struct Database {
    file: File,
    last_line_start_offset: usize,
    categories: Vec<String>,
}

impl Database {
    /// Open a database
    pub fn open(path: &Path, classifier_categories: Vec<&str>) -> io::Result<Self> {
        match fs::OpenOptions::new().read(true).write(true).open(path) {
            Ok(f) => {
                let mut reader = io::BufReader::new(f);
                let (db_categories, header_len) = Database::parse_categories(&mut reader)?;
                if is_subset_of(&classifier_categories, &db_categories) {
                    let last_line_start_offset =
                        Database::scan_db_entries(&mut reader, header_len, db_categories.len())?;
                    Ok(Database {
                        file: reader.into_inner(),
                        last_line_start_offset: last_line_start_offset,
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
        let header = format!("time_slice\t{}\n", classifier_categories.join("\t"));
        f.write_all(header.as_bytes())?;
        Ok(Database {
            file: f,
            last_line_start_offset: header.len(),
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

    /// Check db entries, return last_line_start_offset
    /// Assume reader cursor is at start of second line.
    fn scan_db_entries(
        reader: &mut io::BufReader<File>,
        header_len: usize,
        nb_categories: usize,
    ) -> io::Result<usize> {
        let mut line = String::new();
        let mut line_nb = 2; // Start at line 2
        let mut offset = header_len;
        let mut prev_line_len = 0;
        loop {
            let line_len = reader.read_line(&mut line)?;
            // Entry line must be either empty, or be '\n'-terminated and have the right fields
            if line_len == 0 {
                return Ok(offset);
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
    /// If entry is correct: return time slice start and duration for categories.
    /// If entry is empty: return None.
    /// If entry is incorrect: error.
    pub fn get_last_entry(
        &mut self,
    ) -> io::Result<Option<(chrono::DateTime<chrono::Local>, Vec<time::Duration>)>> {
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
            return Err(bad_data("database last line is not newline terminated"));
        }
        let mut elements = line.split('\t');
        if let Some(time_slice_text) = elements.next() {
            // Read entry time field
            let time_slice = chrono::DateTime::from_str(time_slice_text).map_err(|err| {
                bad_data(format!("database: cannot parse last line time: {}", err))
            })?;
            // Read durations of entry
            let mut durations = Vec::with_capacity(self.categories.len());
            for s in elements {
                let seconds = u64::from_str(s).map_err(|err| {
                    bad_data(format!(
                        "database: cannot parse last line category duration: {}",
                        err
                    ))
                })?;
                durations.push(time::Duration::from_secs(seconds))
            }
            if durations.len() != self.categories.len() {
                return Err(bad_data("database last line: field count mismatch"));
            }
            Ok(Some((time_slice, durations)))
        } else {
            Err(bad_data("database header has no field"))
        }
    }

    pub fn write_to_disk(&mut self) {
        println!("Write to disk")
    }
}

struct CategoryDurationCounter {
    current_category_index: Option<usize>, // Index in duration_by_category
    last_category_update: time::Instant,
    duration_by_category: Vec<(String, time::Duration)>,
}

impl CategoryDurationCounter {
    /// Create a new time tracking structure.
    /// Starts with no defined category.
    pub fn new(categories: &[String]) -> Self {
        CategoryDurationCounter {
            current_category_index: None,
            last_category_update: time::Instant::now(),
            duration_by_category: categories
                .into_iter()
                .map(|s| (s.clone(), time::Duration::new(0, 0)))
                .collect(),
        }
    }

    pub fn set_durations(&mut self, durations: &[time::Duration]) {
        for ((_category, ref mut stored_duration), duration) in
            self.duration_by_category.iter_mut().zip(durations)
        {
            *stored_duration = *duration
        }
    }

    pub fn category_changed(&mut self, category: Option<&str>) {
        println!("Category change: {:?}", category);
        let now = time::Instant::now();
        if let Some(index) = self.current_category_index {
            self.duration_by_category[index].1 += now.duration_since(self.last_category_update)
        }
        self.current_category_index = category.map(|ref s| {
            self.duration_by_category
                .iter()
                .enumerate()
                .find(|(_i, (category_name, _duration))| category_name == s)
                .unwrap()
                .0
        });
        self.last_category_update = now
    }
}

fn main() -> io::Result<()> {
    // Config
    let time_slice_interval = time::Duration::from_secs(3600);
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

    if let Some((time, durations)) = db.get_last_entry()? {
        duration_counter.set_durations(&durations);
        // TODO check time difference
        // if small enough, define start of next time slice as time + interval
        // if not, define it as now + interval
    }

    // State is shared between tasks in tokio.
    // Rc = single thread, RefCell for mutability when needed.
    let db = Rc::new(RefCell::new(db));
    let duration_counter = Rc::new(RefCell::new(duration_counter));

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
            .map_err(|err| panic!("ActiveWindowChanges listener failed: {}", err));
        runtime.spawn(task);
    }
    {
        let db = Rc::clone(&db);
        // Periodically write database to file
        let task = tokio::timer::Interval::new(
            time::Instant::now() + db_write_interval,
            db_write_interval,
        ).for_each(move |_instant| {
            db.borrow_mut().write_to_disk();
            Ok(())
        })
            .map_err(|err| panic!("Write to file task failed: {}", err));
        runtime.spawn(task);
    }
    Ok(runtime.run().expect("tokio runtime failure"))
}
