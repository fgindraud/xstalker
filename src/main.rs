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
use std::io::BufRead;
use std::io::Seek;

/// Read database file header, return categories if found
fn read_current_database_categories(mut file: File) -> io::Result<(File, Option<Vec<String>>)> {
    file.seek(io::SeekFrom::Start(0))?;
    let mut file = io::BufReader::new(file);
    let mut first_line = String::new();
    file.read_line(&mut first_line)?;
    let categories: Vec<String> = first_line
        .split('\t')
        .skip(1)
        .map(|s| String::from(s))
        .collect();
    Ok((
        file.into_inner(),
        if categories.is_empty() {
            None
        } else {
            Some(categories)
        },
    ))
}

fn setup_database_for_categories(filename: &str, categories: &Vec<&str>) -> io::Result<()> {
    use std::fs::OpenOptions;
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(filename)?;
    let (f, db_categories) = read_current_database_categories(f)?;
    println!("Categories: {:?}", db_categories);
    // Check file header
    Ok(())
}

struct Database {
    file: File,
}
impl Database {
    pub fn new(filename: &str, categories: &Vec<&str>) -> io::Result<Self> {
        use std::fs::OpenOptions;
        match File::open(filename) {
            Ok(f) => unimplemented!(),
            Err(ref e) if e.kind() == io::ErrorKind::NotFound => {
                // Create a new database, print header
                let f = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .open(filename)?;
                // TODO
                Ok(Database { file: f })
            }
            Err(e) => Err(e),
        }
    }

    pub fn write_to_disk(&mut self) {
        println!("Write to disk")
    }
}

fn main() -> io::Result<()> {
    use std::cell::RefCell;
    use std::rc::Rc;

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
    let (db, active_window_changes, category_duration_couter) = {
        let categories = classifier.categories();

        // Initial state
        let db = Database::new("test", &categories)?;
        let active_window_changes = ActiveWindowChanges::new()?;
        let category_duration_couter = CategoryDurationCounter::new(
            &categories,
            classifier.classify(&active_window_changes.get_current_metadata()?),
        );

        // Wrap in Rc for shared ownership when passed to tokio.
        // Rc = single thread, RefCell for mutability when needed.
        (
            Rc::new(RefCell::new(db)),
            active_window_changes,
            Rc::new(RefCell::new(category_duration_couter)),
        )
    };
    let classifier = Rc::new(classifier);

    // Create a tokio runtime to implement an event loop.
    // Single threaded is enough.
    // TODO support signals using tokio_signal crate ?
    use tokio::prelude::*;
    use tokio::runtime::current_thread::Runtime;
    let mut runtime = Runtime::new()?;
    {
        let category_duration_couter = Rc::clone(&category_duration_couter);
        let classifier = Rc::clone(&classifier);
        // React to active window changes
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
