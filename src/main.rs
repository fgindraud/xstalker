#![deny(deprecated)]
extern crate chrono;
extern crate tokio;
use std::cell::RefCell;
use std::fmt;
use std::io;
use std::path::Path;
use std::time;
use tokio::prelude::*;

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
    // TODO read rules from simple language ?
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

fn run_daemon(
    classifier: Classifier,
    db_file: &Path,
    db_write_interval: time::Duration,
    time_window_size: time::Duration,
) -> Result<(), io::Error> {
    // Setup state
    let mut db = Database::open(db_file, classifier.categories())?;
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

    let db = RefCell::new(db);
    let duration_counter = RefCell::new(duration_counter);
    let window_start = RefCell::new(window_start);

    // Create a tokio runtime to implement an event loop.
    // Single threaded is enough.
    // TODO support signals using tokio_signal crate ?
    let all_category_changes = {
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
        active_window_changes
            .for_each(|active_window| {
                println!("task_handle_window_change");
                let category = classifier.classify(&active_window);
                duration_counter.borrow_mut().category_changed(category);
                Ok(())
            })
            .map_err(|err| panic!("ActiveWindowChanges listener failed:\n{}", err))
    };
    let all_db_writes = {
        // Periodically write database to file
        tokio::timer::Interval::new(time::Instant::now() + db_write_interval, db_write_interval)
            .for_each(|_instant| {
                println!("task_write_db");
                write_durations_to_disk(
                    &mut db.borrow_mut(),
                    &duration_counter.borrow(),
                    &window_start.borrow(),
                ).unwrap();
                Ok(())
            })
            .map_err(|err| panic!("Write to database file failed:\n{}", err))
    };
    let all_time_window_changes = {
        // Periodically change time window
        tokio::timer::Interval::new(
            time::Instant::now() + duration_to_next_window_change,
            time_window_size,
        ).for_each(|_instant| {
            println!("task_new_time_window");
            change_time_window(
                &mut db.borrow_mut(),
                &mut duration_counter.borrow_mut(),
                &mut window_start.borrow_mut(),
                time_window_size,
            ).unwrap();
            Ok(())
        })
            .map_err(|err| panic!("Change time window failed:\n{}", err))
    };

    let mut runtime = tokio::runtime::current_thread::Runtime::new()?;
    runtime
        .block_on(all_category_changes.join3(all_db_writes, all_time_window_changes))
        .expect("tokio runtime failure");
    Ok(())
}

fn main() -> Result<(), DebugAsDisplay<String>> {
    // Config TODO from args
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

    run_daemon(
        classifier,
        Path::new("test"),
        db_write_interval,
        time_window_size,
    ).map_err(|err| DebugAsDisplay(err.to_string()))
}

/** If main returns Result<_, E>, E will be printed with fmt::Debug.
 * By wrapping T in this structure, it will be printed nicely with fmt::Display.
 */
struct DebugAsDisplay<T>(T);
impl<T: fmt::Display> fmt::Debug for DebugAsDisplay<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        self.0.fmt(f)
    }
}
