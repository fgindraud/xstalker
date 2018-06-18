#![deny(deprecated)]
extern crate chrono;
extern crate tokio;
use std::cell::RefCell;
use std::error;
use std::fmt;
use std::path::Path;
use std::time;
use tokio::prelude::*;

#[derive(Debug)]
pub struct ErrorMessage {
    message: String,
    inner: Option<Box<error::Error>>,
}
impl ErrorMessage {
    pub fn new<M: Into<String>>(message: M) -> Self {
        ErrorMessage {
            message: message.into(),
            inner: None,
        }
    }
    pub fn with_cause<M, E>(message: M, cause: E) -> Self
    where
        M: Into<String>,
        E: error::Error + 'static,
    {
        ErrorMessage {
            message: message.into(),
            inner: Some(Box::new(cause)),
        }
    }
}
impl fmt::Display for ErrorMessage {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.message.fmt(f)
    }
}
impl error::Error for ErrorMessage {
    fn description(&self) -> &str {
        self.message.as_str()
    }
    fn cause(&self) -> Option<&error::Error> {
        self.inner.as_ref().map(|b| b.as_ref())
    }
}

#[derive(Debug)]
pub struct ActiveWindowMetadata {
    title: Option<String>,
    class: Option<String>,
}

/// Database time recording
mod database;
use database::{CategoryDurationCounter, Database, DatabaseTime};

/// Xcb interface
mod xcb_stalker;
use xcb_stalker::ActiveWindowChanges;

/// Classifier: determines the category based on active window metadata.
trait Classifier {
    /// Returns the set of all categories defined in the classifier.
    fn categories(&self) -> Result<Vec<String>, String>;

    /// Returns the category name for the metadata, or None if not matched.
    fn classify(&self, metadata: &ActiveWindowMetadata) -> Result<Option<String>, String>;
}

/** TestClassifier: stores rules used to determine categories for time spent.
 * Rules are stored in an ordered list.
 * The first matching rule in the list chooses the category.
 * A category can appear in multiple rules.
 */
struct TestClassifier {
    filters: Vec<(String, Box<Fn(&ActiveWindowMetadata) -> bool>)>,
}
impl TestClassifier {
    /// Create a new classifier with no rules.
    fn new() -> Self {
        let mut classifier = TestClassifier {
            filters: Vec::new(),
        };
        classifier.append_filter(&"coding", |md| {
            md.class
                .as_ref()
                .map(|class| class == "konsole")
                .unwrap_or(false)
        });
        classifier.append_filter(&"unknown", |_| true);
        classifier
    }
    /// Add a rule at the end of the list, for the given category.
    fn append_filter<F>(&mut self, category: &str, filter: F)
    where
        F: 'static + Fn(&ActiveWindowMetadata) -> bool,
    {
        self.filters
            .push((String::from(category), Box::new(filter)));
    }
}
impl Classifier for TestClassifier {
    fn categories(&self) -> Result<Vec<String>, String> {
        let mut categories: Vec<String> = self.filters
            .iter()
            .map(|(category, _)| category.clone())
            .collect();
        categories.sort();
        categories.dedup();
        Ok(categories)
    }

    fn classify(&self, metadata: &ActiveWindowMetadata) -> Result<Option<String>, String> {
        Ok(self.filters
            .iter()
            .find(|(_category, filter)| filter(metadata))
            .map(|(category, _filter)| category.clone()))
    }
}

fn write_durations_to_disk(
    db: &mut Database,
    duration_counter: &CategoryDurationCounter,
    window_start: &DatabaseTime,
) -> Result<(), String> {
    db.rewrite_last_entry(window_start, duration_counter.durations())
        .map_err(|e| format!("Failed to write database file: {}", e))
}

fn change_time_window(
    db: &mut Database,
    duration_counter: &mut CategoryDurationCounter,
    window_start: &mut DatabaseTime,
    time_window_size: time::Duration,
) -> Result<(), String> {
    // Flush current durations values
    write_durations_to_disk(db, duration_counter, window_start)?;
    // Create a new time window
    db.lock_last_entry();
    duration_counter.reset_durations();
    *window_start = *window_start + chrono::Duration::from_std(time_window_size).unwrap();
    Ok(())
}

fn run_daemon(
    classifier: &Classifier,
    db_file: &Path,
    db_write_interval: time::Duration,
    time_window_size: time::Duration,
) -> Result<(), String> {
    // Setup state
    let classifier_categories = classifier.categories()?;
    let mut db = Database::open(db_file, classifier_categories)
        .map_err(|e| format!("Unable to open database '{}':\n{}", db_file.display(), e))?;
    let mut duration_counter = CategoryDurationCounter::new(db.categories());
    let active_window_changes =
        ActiveWindowChanges::new().map_err(|e| format!("Unable to start event listener:\n{}", e))?;

    // Determine current time window
    let now = DatabaseTime::from(time::SystemTime::now());
    let window_start = {
        if let Some((time, durations)) = db.get_last_entry()
            .map_err(|e| format!("Unable to read last database entry:\n{}", e))?
        {
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

    // Set initial category
    {
        let initial_metadata = active_window_changes
            .get_current_metadata()
            .map_err(|e| format!("Unable to get window metadata:\n{}", e))?;
        let initial_category = classifier.classify(&initial_metadata)?;
        duration_counter.category_changed(initial_category);
    }

    // Wrap shared state in RefCell: cannot prove with type that mutations are exclusive.
    let db = RefCell::new(db);
    let duration_counter = RefCell::new(duration_counter);
    let window_start = RefCell::new(window_start);

    // Listen to active window changes.
    let all_category_changes = active_window_changes
        .map_err(|e| format!("Window metadata listener failed:\n{}", e))
        .for_each(|active_window| {
            println!("task_handle_window_change");
            let category = classifier.classify(&active_window)?;
            duration_counter.borrow_mut().category_changed(category);
            Ok(())
        });

    // Periodically write database to file
    let all_db_writes =
        tokio::timer::Interval::new(time::Instant::now() + db_write_interval, db_write_interval)
            .map_err(|e| format!("Timer error: {}", e))
            .for_each(|_instant| {
                println!("task_write_db");
                write_durations_to_disk(
                    &mut db.borrow_mut(),
                    &duration_counter.borrow(),
                    &window_start.borrow(),
                )
            });

    // Periodically change time window
    let all_time_window_changes = tokio::timer::Interval::new(
        time::Instant::now() + duration_to_next_window_change,
        time_window_size,
    ).map_err(|e| format!("Timer error: {}", e))
        .for_each(|_instant| {
            println!("task_new_time_window");
            change_time_window(
                &mut db.borrow_mut(),
                &mut duration_counter.borrow_mut(),
                &mut window_start.borrow_mut(),
                time_window_size,
            )
        });

    // Create a tokio runtime to implement an event loop.
    // Single threaded is enough.
    // TODO support signals using tokio_signal crate ?
    let mut runtime = tokio::runtime::current_thread::Runtime::new()
        .map_err(|e| format!("Unable to create tokio runtime:\n{}", e))?;
    runtime
        .block_on(all_category_changes.join3(all_db_writes, all_time_window_changes))
        .map(|(_, _, _)| ())
}

fn main() -> Result<(), ShowErrorTraceback<ErrorMessage>> {
    // Config TODO from args
    // use clap crate ?
    let time_window_size = time::Duration::from_secs(3600);
    let db_write_interval = time::Duration::from_secs(10);

    // Setup test classifier
    let classifier = TestClassifier::new();

    run_daemon(
        &classifier,
        Path::new("test"),
        db_write_interval,
        time_window_size,
    ).map_err(|err| ShowErrorTraceback(ErrorMessage::new(err)))
}

/** If main returns Result<_, E>, E will be printed with fmt::Debug.
 * Wrap an Error in this to print a newline delimited error message.
 */
struct ShowErrorTraceback<T: error::Error>(T);
impl<T: error::Error> fmt::Debug for ShowErrorTraceback<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", &self.0)?;
        for err in Traceback(self.0.cause()) {
            write!(f, ":\n{}", err)?;
        }
        Ok(())
    }
}

/// Iterate on error causes
struct Traceback<'a>(Option<&'a error::Error>);
impl<'a> Iterator for Traceback<'a> {
    type Item = &'a error::Error;
    fn next(&mut self) -> Option<Self::Item> {
        let current = self.0;
        self.0 = match &current {
            Some(err) => err.cause(),
            None => None,
        };
        current
    }
}
