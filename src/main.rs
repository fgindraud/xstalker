#![deny(deprecated)]
extern crate tokio;
use std::io;
use std::time;

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
    fn categories(&self) -> Vec<&str> {
        let mut categories: Vec<&str> = self.filters
            .iter()
            .map(|(category, _)| category.as_str())
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

use std::collections::HashMap;

struct CategoryDurationCounter {
    current_category: Option<String>,
    last_category_update: time::Instant,
    duration_by_category: HashMap<String, time::Duration>,
}
impl CategoryDurationCounter {
    fn new(categories: &Vec<&str>, initial_category: Option<&str>) -> Self {
        println!("Initial category: {:?}", initial_category);
        CategoryDurationCounter {
            current_category: initial_category.map(|s| String::from(s)),
            last_category_update: time::Instant::now(),
            duration_by_category: categories
                .iter()
                .map(|&s| (String::from(s), time::Duration::new(0, 0)))
                .collect(),
        }
    }

    fn category_changed(&mut self, category: Option<&str>) {
        println!("Category change: {:?}", category);
        let now = time::Instant::now();
        if let Some(ref current_category) = self.current_category {
            let mut category_duration = self.duration_by_category
                .get_mut(current_category.as_str())
                .unwrap();
            *category_duration += now.duration_since(self.last_category_update)
        }
        self.current_category = category.map(|s| String::from(s));
        self.last_category_update = now
    }
}

/// Database
use std::fs::File;

fn setup_database_for_categories(filename: &str, categories: &Vec<&str>) -> io::Result<()> {
    use std::fs::OpenOptions;
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(filename)?;
    // Check file header
    let f = io::BufReader::new(f);
    let s = String::new();

    Ok(())
}

struct Database {
    file: File,
}
impl Database {
    pub fn new(filename: &str) -> io::Result<Self> {
        use std::fs::OpenOptions;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(filename)?;
        Ok(Database { file: file })
    }

    pub fn write_to_disk(&mut self) {
        println!("Write to disk")
    }
}

fn get_last_line(file: &mut std::fs::File) -> String {
    use std::io::{Seek, SeekFrom};

    let _end = file.seek(SeekFrom::End(0));
    String::from("")
}

fn main() -> io::Result<()> {
    // Timing
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

    // Setup entities
    let active_window_changes = ActiveWindowChanges::new()?;

    let category_duration_couter = CategoryDurationCounter::new(
        &classifier.categories(),
        classifier.classify(&active_window_changes.get_current_metadata()?),
    );

    let db = Database::new("test")?;

    // Shared state in Rc<RefCell>: single threaded, needs mutability
    use std::cell::RefCell;
    use std::rc::Rc;
    let classifier = Rc::new(classifier);
    let category_duration_couter = Rc::new(RefCell::new(category_duration_couter));
    let db = Rc::new(RefCell::new(db));

    // Create a tokio runtime to act as an event loop.
    // Single threaded is enough.
    // TODO support signals using tokio_signal crate ?
    use tokio::prelude::*;
    use tokio::runtime::current_thread::Runtime;
    let mut runtime = Runtime::new()?;
    {
        // React to active window changes
        let category_duration_couter = Rc::clone(&category_duration_couter);
        let classifier = Rc::clone(&classifier);
        let task = active_window_changes
            .for_each(move |active_window| {
                category_duration_couter
                    .borrow_mut()
                    .category_changed(classifier.classify(&active_window));
                Ok(())
            })
            .map_err(|err| panic!("ActiveWindowChanges listener failed: {}", err));
        runtime.spawn(task);
    }
    {
        // Periodically write database to file
        let db = Rc::clone(&db);
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
