#![deny(deprecated)]
extern crate mio;
extern crate xcb; // for xcb_stalker

use std;
use std::io;
use std::os::unix::io::{AsRawFd, RawFd};
use tokio::prelude::*;
use tokio::reactor::PollEvented2 as PollEvented; // Tokio is changing interfaces, temporary

pub use super::ActiveWindowMetadata;

// Main struct
pub struct Stalker {
    connection: xcb::Connection,
    root_window: xcb::Window,
    non_static_atoms: NonStaticAtoms,
}

/// Store non static useful atoms
struct NonStaticAtoms {
    active_window: xcb::Atom,
    utf8_string: xcb::Atom,
    compound_text: xcb::Atom,
}

pub struct ActiveWindowChanges {
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
                    && reply.value_len() == 1 && reply.format() == 32 =>
            {
                // Not pretty. Assumes that xcb::Window is an u32
                let buf: &[xcb::Window] = reply.value();
                Some(buf[0])
            }
            _ => None,
        }
    }
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
                        return std::str::from_utf8(reply.value())
                            .ok()
                            .map(|text| String::from(text))
                    }
                    atom => eprintln!("get_text_property: unsupported atom reply: {}", atom),
                }
            }
        }
        None
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

impl ActiveWindowChanges {
    pub fn new() -> Self {
        ActiveWindowChanges {
            inner: PollEvented::new(Stalker::new()),
        }
    }
    // TODO to be the main struct
    // TODO evented on xcb::connection should be better
    // manual method to get ActiveWindowMetadata for init
}

impl Stream for ActiveWindowChanges {
    type Item = ActiveWindowMetadata;
    type Error = io::Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, io::Error> {
        // FIXME this works, but its a mess
        // TODO stream for events, and build upon that ?
        // TODO for ActiveWindowChanges: add initial value ? or feed it manually before starting
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
