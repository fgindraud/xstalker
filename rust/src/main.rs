#![deny(deprecated)]
extern crate tokio;

#[derive(Debug)]
pub struct ActiveWindowMetadata {
    title: Option<String>,
    class: Option<String>,
}

// TODO: define trait for stream of ActiveWindowMetadata
// make xcb_stalker return a box<trait obj> to make it independent from other stuff

/// Xcb interface
mod xcb_stalker;

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
 * At startup, look header.
 * New category: add, rewrite file
 * Removed category: add to set, with 0 (will not be incremented as no filter gives it)
 *
 * Every change from xcb: process all events, then get new category.
 * if changed from before: change_time_slice(old_cat, new_cat)
 *
 * Every tick (60s): dump
 * if now() > time_last_line + dump_interval: new line
 * else: rewrite last line
 *
 * start: get init category from xcb
 * start time slice
 */

/// Database
use std::collections::HashMap;
use std::fs::File;
use std::time::Duration;
struct TimeSliceDatabase {
    file: File,
    duration_by_category_current_interval: HashMap<String, Duration>,
}
impl TimeSliceDatabase {
    pub fn new(filename: &str) -> Result<Self, std::io::Error> {
        use std::fs::OpenOptions;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open("test")?;
        Ok(TimeSliceDatabase {
            file: file,
            duration_by_category_current_interval: HashMap::new(),
        })
    }
    pub fn start_time_slice(category: &str) {
        unimplemented!();
    }
    pub fn end_time_slice(category: &str) {
        unimplemented!();
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

    {
        // File manip test
        let db = TimeSliceDatabase::new("test").expect("failed to create database");
        println!("test: {}", db.file.metadata().unwrap().len());
    }

    let stalker = xcb_stalker::Stalker::new();
    //stalker.handle_events();

    // Shared state in Rc<RefCell>: single threaded, needs mutability
    use std::cell::RefCell;
    use std::rc::Rc;
    let counter = Rc::new(RefCell::new(0)); // Needs to be cloned explicitely

    // Create a tokio runtime to act as an event loop.
    // Single threaded is enough.
    use tokio::prelude::*;
    use tokio::runtime::current_thread::Runtime;
    let mut runtime = Runtime::new().expect("unable to create tokio runtime");
    {
        let counter = Rc::clone(&counter);
        let task = stalker
            .active_window_stream()
            .for_each(move |active_window| {
                // debug / test code
                println!("ActiveWindowMetadata = {:?}", active_window);
                let mut counter = counter.borrow_mut();
                *counter += 1;
                Ok(())
            })
            .map_err(|err| panic!("crash"));
        runtime.spawn(task);
    }
    {
        // Periodically write counter value
        use std::time::{Duration, Instant};
        use tokio::timer::Interval;
        let counter = Rc::clone(&counter);
        let store_data_task = Interval::new(Instant::now(), Duration::from_secs(1))
            .for_each(move |instant| {
                println!("counter {}", counter.borrow());
                Ok(())
            })
            .map_err(|err| panic!("store_data_task failed: {:?}", err));
        runtime.spawn(store_data_task);
    }
    runtime.run().expect("tokio runtime failure")
}
