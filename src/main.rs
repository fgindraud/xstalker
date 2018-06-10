#![deny(deprecated)]
extern crate tokio;
use std::io;

#[derive(Debug)]
pub struct ActiveWindowMetadata {
    title: Option<String>,
    class: Option<String>,
}

/// Xcb interface
mod xcb_stalker;
use xcb_stalker::ActiveWindowChanges;

/// Classifier: stores filters used to determine category of time slice
struct Classifier {
    filters: Vec<(String, Box<Fn(&ActiveWindowMetadata) -> bool>)>,
}

impl Classifier {
    fn new() -> Self {
        Classifier {
            filters: Vec::new(),
        }
    }
    fn append_filter<F>(&mut self, category: &str, filter: F)
    where
        F: 'static + Fn(&ActiveWindowMetadata) -> bool,
    {
        self.filters
            .push((String::from(category), Box::new(filter)));
    }
    fn categories(&self) -> Vec<String> {
        let mut categories: Vec<String> = self.filters
            .iter()
            .map(|(category, _)| category.clone())
            .collect();
        categories.sort();
        categories.dedup();
        categories
    }
    fn classify(&self, metadata: &ActiveWindowMetadata) -> Option<&str> {
        for (category, filter) in self.filters.iter() {
            if filter(metadata) {
                return Some(&category);
            }
        }
        None
    }
}

/*
 * File format:
 * date\tcat0\tcat1...
 * [start hour, ISO machin]\t[nb_sec cat0]\t...
 *
 * TODO
 * parsing iso 8601: chrono crate TODO
 * two interval streams:
 * - one for write_to_disk,
 * - one for time slice interval
 *
 * two structs:
 * one for managing the file (lookup last line, etc).
 * one for the current interval time slices
 *
 * At startup, look header.
 * New category: add, rewrite file
 * Removed category: add to set, with 0 (will not be incremented as no filter gives it)
 */

/// Database
use std::collections::HashMap;
use std::fs::File;
use std::time::Duration;
struct TimeSliceDatabase {
    file: File,
    current_category: (),
    duration_by_category_current_interval: HashMap<String, Duration>,
}
impl TimeSliceDatabase {
    pub fn new(filename: &str) -> io::Result<Self> {
        use std::fs::OpenOptions;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(filename)?;
        Ok(TimeSliceDatabase {
            file: file,
            current_category: (),
            duration_by_category_current_interval: HashMap::new(),
        })
    }

    pub fn set_initial_category(&mut self, category: Option<&str>) {
        println!("Initial category: {:?}", category)
    }

    pub fn category_changed(&mut self, category: Option<&str>) {
        println!("Category change: {:?}", category)
    }

    pub fn store(&mut self) {
        println!("Write to disk")
    }
}

fn get_last_line(file: &mut std::fs::File) -> String {
    use std::io::{Seek, SeekFrom};

    let _end = file.seek(SeekFrom::End(0));
    String::from("")
}

fn main() {
    // Test classifier
    let mut classifier = Classifier::new();
    classifier.append_filter(&"coding", |md| {
        md.class
            .as_ref()
            .map(|class| class == "konsole")
            .unwrap_or(false)
    });
    classifier.append_filter(&"unknown", |_| true);

    let db = TimeSliceDatabase::new("test").expect("failed to create database");

    // Shared state in Rc<RefCell>: single threaded, needs mutability
    use std::cell::RefCell;
    use std::rc::Rc;
    let db = Rc::new(RefCell::new(db));

    // Create a tokio runtime to act as an event loop.
    // Single threaded is enough.
    use tokio::prelude::*;
    use tokio::runtime::current_thread::Runtime;
    let mut runtime = Runtime::new().expect("unable to create tokio runtime");
    {
        // React to active window changes
        let db = Rc::clone(&db);
        let active_window_changes = ActiveWindowChanges::new().unwrap();
        db.borrow_mut().set_initial_category(
            classifier.classify(&active_window_changes.get_current_metadata().unwrap()),
        );
        let task = active_window_changes
            .for_each(move |active_window| {
                //println!("ActiveWindowMetadata = {:?}", active_window);
                db.borrow_mut()
                    .category_changed(classifier.classify(&active_window));
                Ok(())
            })
            .map_err(|err| panic!("ActiveWindowChanges listener failed: {}", err));
        runtime.spawn(task);
    }
    {
        // Periodically write database to file
        use std::time::{Duration, Instant};
        use tokio::timer::Interval;
        let db = Rc::clone(&db);
        let interval = Duration::from_secs(10);
        let task = Interval::new(Instant::now() + interval, interval)
            .for_each(move |_instant| {
                db.borrow_mut().store();
                Ok(())
            })
            .map_err(|err| panic!("Write to file task failed: {}", err));
        runtime.spawn(task);
    }
    runtime.run().expect("tokio runtime failure")
}
