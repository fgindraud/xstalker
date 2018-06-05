extern crate mio;
extern crate tokio;

struct ActiveWindowMetadata {
    // TODO optionals to handle missing data or failures ?
    title: String,
    class: String,
}

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

/// Xcb interface
mod xcb_stalker {
    extern crate xcb;
    use mio;
    use std;
    use std::io;
    use std::os::unix::io::AsRawFd;

    pub struct Stalker {
        connection: xcb::Connection,
        root_window: xcb::Window,
        non_static_atoms: NonStaticAtoms,
    }

    impl Stalker {
        pub fn new() -> Self {
            let (conn, screen_num) = xcb::Connection::connect(None).unwrap();
            let root_window = {
                let setup = conn.get_setup();
                let screen = setup.roots().nth(screen_num as usize).unwrap();
                screen.root()
            };
            let non_static_atoms = NonStaticAtoms::new(&conn);
            println!(
                "non_static_atoms: str={} utf8={} compound={}",
                xcb::ATOM_STRING,
                non_static_atoms.utf8_string,
                non_static_atoms.compound_text
            );

            // Listen to property changes for root window.
            // This is where the active window property is maintained.
            let values = [(xcb::CW_EVENT_MASK, xcb::EVENT_MASK_PROPERTY_CHANGE)];
            xcb::change_window_attributes(&conn, root_window, &values);
            conn.flush();

            Stalker {
                connection: conn,
                root_window: root_window,
                non_static_atoms: non_static_atoms,
            }
        }

        fn get_active_window(&self) -> Option<xcb::Window> {
            let cookie = xcb::get_property(
                &self.connection,
                false,
                self.root_window,
                self.non_static_atoms.active_window,
                xcb::ATOM_WINDOW,
                0,
                (std::mem::size_of::<xcb::Window>() / 4) as u32,
            );
            match &cookie.get_reply() {
                Ok(reply)
                    if reply.type_() == xcb::ATOM_WINDOW && reply.bytes_after() == 0
                        && reply.value_len() == 1
                        && reply.format() == 32 =>
                {
                    // Not pretty. Assumes that xcb::Window is an u32
                    let buf: &[xcb::Window] = reply.value();
                    Some(buf[0])
                }
                _ => None,
            }
        }

        pub fn handle_events(&self) {
            // TODO tokio-ify
            while let Some(event) = self.connection.wait_for_event() {
                let rt = event.response_type();
                if rt == xcb::PROPERTY_NOTIFY {
                    let event: &xcb::PropertyNotifyEvent = unsafe { xcb::cast_event(&event) };
                    if event.window() == self.root_window
                        && event.atom() == self.non_static_atoms.active_window
                        && event.state() == xcb::PROPERTY_NEW_VALUE as u8
                    {
                        let w = self.get_active_window().unwrap();
                        let (title, class) = {
                            let title = get_text_property(
                                &self.connection,
                                w,
                                xcb::ATOM_WM_NAME,
                                &self.non_static_atoms,
                            );
                            let class = get_text_property(
                                &self.connection,
                                w,
                                xcb::ATOM_WM_CLASS,
                                &self.non_static_atoms,
                            );
                            (title.get(), class.get())
                        };
                        // Class contains two strings split by \0 TODO get first half ?
                        println!("active_window = '{:?}' '{:?}' {:x}", title, class, w);
                    }
                }
            }
        }
    }

    /// Store non static useful atoms
    struct NonStaticAtoms {
        active_window: xcb::Atom,
        utf8_string: xcb::Atom,
        compound_text: xcb::Atom,
    }

    impl NonStaticAtoms {
        /// Get values from server
        fn new(conn: &xcb::Connection) -> Self {
            let active_window_cookie = xcb::intern_atom(&conn, true, "_NET_ACTIVE_WINDOW");
            let utf8_string_cookie = xcb::intern_atom(&conn, true, "UTF8_STRING");
            let compound_text_cookie = xcb::intern_atom(&conn, true, "COMPOUND_TEXT");
            NonStaticAtoms {
                active_window: active_window_cookie.get_reply().unwrap().atom(),
                utf8_string: utf8_string_cookie.get_reply().unwrap().atom(),
                compound_text: compound_text_cookie.get_reply().unwrap().atom(),
            }
        }
    }

    /// Launch a request for a text property
    fn get_text_property<'a>(
        conn: &'a xcb::Connection,
        window: xcb::Window,
        atom: xcb::Atom,
        non_static_atoms: &'a NonStaticAtoms,
    ) -> GetTextPropertyCookie<'a> {
        GetTextPropertyCookie {
            cookie: xcb::get_property(conn, false, window, atom, xcb::ATOM_ANY, 0, 1024),
            non_static_atoms: non_static_atoms,
        }
    }

    struct GetTextPropertyCookie<'a> {
        cookie: xcb::GetPropertyCookie<'a>,
        non_static_atoms: &'a NonStaticAtoms,
    }

    impl<'a> GetTextPropertyCookie<'a> {
        fn get(&self) -> Option<String> {
            if let Ok(reply) = self.cookie.get_reply() {
                if reply.format() == 8 && reply.bytes_after() == 0 && reply.value_len() > 0 {
                    match reply.type_() {
                        atom if [
                            xcb::ATOM_STRING,
                            self.non_static_atoms.utf8_string,
                            self.non_static_atoms.compound_text,
                        ].contains(&atom) =>
                        {
                            std::str::from_utf8(reply.value())
                                .ok()
                                .map(|text| String::from(text))
                        }
                        atom => {
                            eprintln!("get_text_property: unsupported atom reply: {}", atom);
                            None
                        }
                    }
                } else {
                    None
                }
            } else {
                None
            }
        }
    }

    impl std::os::unix::io::AsRawFd for Stalker {
        fn as_raw_fd(&self) -> std::os::unix::io::RawFd {
            let raw_handle = self.connection.get_raw_conn();
            unsafe { xcb::ffi::xcb_get_file_descriptor(raw_handle) }
        }
    }

    impl mio::Evented for Stalker {
        fn register(
            &self,
            poll: &mio::Poll,
            token: mio::Token,
            interest: mio::Ready,
            opts: mio::PollOpt,
        ) -> io::Result<()> {
            println!("Registered!");
            mio::unix::EventedFd(&self.as_raw_fd()).register(poll, token, interest, opts)
        }

        fn reregister(
            &self,
            poll: &mio::Poll,
            token: mio::Token,
            interest: mio::Ready,
            opts: mio::PollOpt,
        ) -> io::Result<()> {
            println!("Reregistered!");
            mio::unix::EventedFd(&self.as_raw_fd()).reregister(poll, token, interest, opts)
        }

        fn deregister(&self, poll: &mio::Poll) -> io::Result<()> {
            println!("Deregistered!");
            mio::unix::EventedFd(&self.as_raw_fd()).deregister(poll)
        }
    }
}

// interesting:
// tokio_core pollevented
// https://github.com/tokio-rs/tokio-core/issues/63
// https://github.com/rust-lang-nursery/futures-rs/issues/702
//
// TODO maybe mio::Evented on xcb::connection instead ?
// TODO add a future or stream to handle updates ?

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

    let end = file.seek(SeekFrom::End(0));
    String::from("")
}

fn main() {
    // Test classifier
    let mut classifier = Classifier::new();
    classifier.append_filter(&"coding", |md| md.class == "konsole");
    classifier.append_filter(&"unknown", |_| true);

    {
        // File manip test
        let db = TimeSliceDatabase::new("test").expect("failed to create database");
        println!("test: {}", db.file.metadata().unwrap().len());
    }

    let stalker = xcb_stalker::Stalker::new();
    stalker.handle_events();

    // TODO wrap file descriptor for tokio

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
