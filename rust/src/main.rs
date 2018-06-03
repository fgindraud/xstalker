extern crate tokio;

struct ActiveWindowMetadata {
    title: String,
    class: String,
}

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

fn main() {
    // Test classifier
    let mut classifier = Classifier::new();
    classifier.append_filter(&"coding", |md| md.class == "konsole");
    classifier.append_filter(&"unknown", |_| true);

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
    {
        // TEST periodically increment counter
        use std::time::{Duration, Instant};
        use tokio::timer::Interval;
        let counter = Rc::clone(&counter);
        let increment = Interval::new(Instant::now(), Duration::from_secs(3))
            .for_each(move |instant| {
                let mut c = counter.borrow_mut();
                *c += 1;
                Ok(())
            })
            .map_err(|err| panic!("increment failed: {:?}", err));
        runtime.spawn(increment);
    }
    runtime.run().expect("tokio runtime failure")
}
