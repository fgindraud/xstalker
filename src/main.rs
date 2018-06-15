#![deny(deprecated)]
extern crate chrono;
extern crate tokio;
use std::cell::RefCell;
use std::io;
use std::path::Path;
use std::rc::Rc;
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

fn main() -> io::Result<()> {
    // TODO error handling ?
    // wrap in function that takes config, and return io::Result<()>
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
