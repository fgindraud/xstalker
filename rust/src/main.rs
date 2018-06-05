#![deny(deprecated)]
extern crate mio;
extern crate tokio;

use tokio::prelude::*;

#[derive(Debug)]
pub struct ActiveWindowMetadata {
    title: Option<String>,
    class: Option<String>,
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
    use std::os::unix::io::{AsRawFd, RawFd};
    use tokio;
    use tokio::prelude::*;
    use tokio::reactor::PollEvented2 as PollEvented;

    pub use ActiveWindowMetadata;

    pub struct Stalker {
        connection: xcb::Connection,
        root_window: xcb::Window,
        non_static_atoms: NonStaticAtoms,
    }

    pub struct ActiveWindowStream {
        inner: PollEvented<Stalker>,
    }

    impl Stalker {
        pub fn new() -> Self {
            // Xcb Boilerplate TODO error handling ?
            let (conn, screen_num) = xcb::Connection::connect(None).unwrap();
            let root_window = {
                let setup = conn.get_setup();
                let screen = setup.roots().nth(screen_num as usize).unwrap();
                screen.root()
            };

            // Get useful non static atoms for later.
            let non_static_atoms = NonStaticAtoms::new(&conn);

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

        pub fn get_active_window_metadata(&self) -> ActiveWindowMetadata {
            let w = self.get_active_window().unwrap();
            // Requests
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
            // Process replies
            let title = title.get();
            let class = class.get().map(|mut text| match text.find('\0') {
                Some(offset) => {
                    text.truncate(offset);
                    text
                }
                None => text,
            });
            ActiveWindowMetadata {
                title: title,
                class: class,
            }
        }

        pub fn process_events(&self) -> bool {
            let mut active_window_changed = false;
            while let Some(event) = self.connection.poll_for_event() {
                let rt = event.response_type();
                if rt == xcb::PROPERTY_NOTIFY {
                    let event: &xcb::PropertyNotifyEvent = unsafe { xcb::cast_event(&event) };
                    if event.window() == self.root_window
                        && event.atom() == self.non_static_atoms.active_window
                        && event.state() == xcb::PROPERTY_NEW_VALUE as u8
                    {
                        active_window_changed = true;
                    }
                }
            }
            active_window_changed
        }

        pub fn active_window_stream(self) -> ActiveWindowStream {
            ActiveWindowStream::new(self)
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

    /// Cookie: ongoing request for a text property
    struct GetTextPropertyCookie<'a> {
        cookie: xcb::GetPropertyCookie<'a>,
        non_static_atoms: &'a NonStaticAtoms,
    }

    impl<'a> GetTextPropertyCookie<'a> {
        /// Retrieve the text property as a String, or None if error.
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

    impl AsRawFd for Stalker {
        fn as_raw_fd(&self) -> RawFd {
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
            mio::unix::EventedFd(&self.as_raw_fd()).register(poll, token, interest, opts)
        }

        fn reregister(
            &self,
            poll: &mio::Poll,
            token: mio::Token,
            interest: mio::Ready,
            opts: mio::PollOpt,
        ) -> io::Result<()> {
            mio::unix::EventedFd(&self.as_raw_fd()).reregister(poll, token, interest, opts)
        }

        fn deregister(&self, poll: &mio::Poll) -> io::Result<()> {
            mio::unix::EventedFd(&self.as_raw_fd()).deregister(poll)
        }
    }

    impl ActiveWindowStream {
        fn new(stalker: Stalker) -> Self {
            ActiveWindowStream {
                inner: PollEvented::new(stalker),
            }
        }
    }

    impl Stream for ActiveWindowStream {
        type Item = ActiveWindowMetadata;
        type Error = io::Error;

        fn poll(&mut self) -> Poll<Option<Self::Item>, io::Error> {
            // FIXME this works, but its a mess
            // TODO stream for events, and build upon that ?
            // TODO for ActiveWindowStream: add initial value ? or feed it manually before starting
            // tokio ?

            // Check if readable
            match self.inner.poll_read_ready(mio::Ready::readable()) {
                Ok(Async::Ready(_)) => (),
                Ok(Async::NotReady) => return Ok(Async::NotReady),
                Err(e) => return Err(e),
            }
            // Read all events
            let active_window_changed = self.inner.get_ref().process_events();
            // Reset read flag, will be set again if data arrives on socket
            self.inner.clear_read_ready(mio::Ready::readable());

            if active_window_changed {
                // get_active_window_metadata requests replies are all consumed
                Ok(Async::Ready(Some(
                    self.inner.get_ref().get_active_window_metadata(),
                )))
            } else {
                Ok(Async::NotReady)
            }
        }
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

    let end = file.seek(SeekFrom::End(0));
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
