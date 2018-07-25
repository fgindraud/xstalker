#![deny(deprecated)]
extern crate mio;
extern crate xcb; // for xcb_stalker

use std;
use std::io;
use std::os::unix::io::AsRawFd;
use std::time;
use tokio::prelude::*;
use tokio::reactor::PollEvented2 as PollEvented; // Tokio is changing interfaces, temporary

/// This is the type used to output information about the active window.
/// Defined in main.
pub use super::ActiveWindowMetadata;

/// Listener for changes of the active window using xcb.
/// Owns the connection to the X server.
struct Stalker {
    connection: xcb::Connection,
    root_window: xcb::Window,
    non_static_atoms: NonStaticAtoms,
    current_active_window: xcb::Window,
}

/// Store non static useful atoms (impl detail of Stalker).
struct NonStaticAtoms {
    active_window: xcb::Atom,
    utf8_string: xcb::Atom,
    compound_text: xcb::Atom,
}

fn conn_to_io_error(err: xcb::ConnError) -> io::Error {
    use self::io::{Error, ErrorKind};
    use self::xcb::ConnError::*;
    match err {
        Connection | ClosedFdPassingFailed => {
            Error::new(ErrorKind::Other, "Xcb connection io error")
        }
        ClosedExtNotSupported => Error::new(ErrorKind::NotFound, "Xcb extension unsupported"),
        ClosedMemInsufficient => Error::new(ErrorKind::Other, "Xcb mem insufficient"),
        ClosedReqLenExceed => Error::new(ErrorKind::InvalidData, "Xcb request length exceeded"),
        ClosedParseErr => Error::new(ErrorKind::InvalidInput, "Xcb invalid DISPLAY"),
        ClosedInvalidScreen => Error::new(ErrorKind::InvalidInput, "Xcb invalid screen"),
    }
}

/// Get active window id. Error if not found.
fn get_active_window(
    connection: &xcb::Connection,
    root_window: xcb::Window,
    active_window_atom: xcb::Atom,
) -> io::Result<xcb::Window> {
    let cookie = xcb::get_property(
        connection,
        false,
        root_window,
        active_window_atom,
        xcb::ATOM_WINDOW,
        0,
        (std::mem::size_of::<xcb::Window>() / 4) as u32,
    );
    match &cookie.get_reply() {
        Ok(reply)
            if reply.type_() == xcb::ATOM_WINDOW
                && reply.bytes_after() == 0
                && reply.value_len() == 1
                && reply.format() == 32 =>
        {
            // Not pretty. Assumes that xcb::Window is an u32
            let buf: &[xcb::Window] = reply.value();
            Ok(buf[0])
        }
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::Other,
            "xcb_get_property(active_window): invalid reply",
        )),
        Err(_) => Err(io::Error::new(
            io::ErrorKind::Other,
            "xcb_get_property(active_window): failure",
        )),
    }
}

/// Enable notifications for property changes on window w
fn enable_property_change_notifications(connection: &xcb::Connection, w: xcb::Window) {
    let values = [(xcb::CW_EVENT_MASK, xcb::EVENT_MASK_PROPERTY_CHANGE)];
    xcb::change_window_attributes(connection, w, &values);
}
/// Disable notifications for property changes on window w
fn disable_property_change_notifications(connection: &xcb::Connection, w: xcb::Window) {
    let values = [(xcb::CW_EVENT_MASK, xcb::NONE)];
    xcb::change_window_attributes(connection, w, &values);
}

impl Stalker {
    /// Create and configure a new listener.
    fn new() -> io::Result<Self> {
        // Xcb Boilerplate
        let (conn, screen_num) = xcb::Connection::connect(None).map_err(conn_to_io_error)?;
        let root_window = {
            let setup = conn.get_setup();
            let screen = setup.roots().nth(screen_num as usize).unwrap();
            screen.root()
        };

        // Get useful non static atoms for later.
        let non_static_atoms = NonStaticAtoms::read_from_conn(&conn)?;

        let active_window = get_active_window(&conn, root_window, non_static_atoms.active_window)?;

        // Listen to its title changes
        enable_property_change_notifications(&conn, active_window);

        // Listen to property changes for root window.
        // This is where the active window property is maintained.
        enable_property_change_notifications(&conn, root_window);

        conn.flush();
        conn.has_error().map_err(conn_to_io_error)?;

        Ok(Stalker {
            connection: conn,
            root_window: root_window,
            non_static_atoms: non_static_atoms,
            current_active_window: active_window,
        })
    }

    /// Get the current active window metadata, and timestamp of change.
    fn get_active_window_metadata(&self) -> io::Result<(ActiveWindowMetadata, time::Instant)> {
        // Timestamp from xcb is unusable
        let timestamp = time::Instant::now();
        // Requests
        let title = self.get_text_property(self.current_active_window, xcb::ATOM_WM_NAME);
        let class = self.get_text_property(self.current_active_window, xcb::ATOM_WM_CLASS);
        // Process replies
        let title = title.get_reply();
        let class = class.get_reply().map(|mut text| match text.find('\0') {
            Some(offset) => {
                text.truncate(offset);
                text
            }
            None => text,
        });
        Ok((
            ActiveWindowMetadata {
                title: title,
                class: class,
            },
            timestamp,
        ))
    }

    /// Process all pending events, update cached data (active_window).
    /// Return true if the active window metadata has changed, and must be queried again.
    fn process_events(&mut self) -> io::Result<bool> {
        let mut active_window_changed = false;
        let mut active_window_title_changed = false;
        // Process all events, gather changes.
        while let Some(event) = self.connection.poll_for_event() {
            let rt = event.response_type();
            if rt == xcb::PROPERTY_NOTIFY {
                let event: &xcb::PropertyNotifyEvent = unsafe { xcb::cast_event(&event) };
                if event.window() == self.root_window
                    && event.atom() == self.non_static_atoms.active_window
                    && event.state() == xcb::PROPERTY_NEW_VALUE as u8
                {
                    println!("DEBUG: prop change active_window on root");
                    active_window_changed = true;
                }
                if event.window() == self.current_active_window
                    && event.atom() == xcb::ATOM_WM_NAME
                    && event.state() == xcb::PROPERTY_NEW_VALUE as u8
                {
                    println!("DEBUG: prop change title on active_window");
                    active_window_title_changed = true;
                }
            }
        }
        // Get new active window
        if active_window_changed {
            let new_active_window = self.get_active_window()?;
            if new_active_window != self.current_active_window {
                if self.current_active_window != self.root_window {
                    // We do not want to disable notifications for root !
                    disable_property_change_notifications(
                        &self.connection,
                        self.current_active_window,
                    )
                }
                enable_property_change_notifications(&self.connection, new_active_window);
                self.current_active_window = new_active_window;
                return Ok(true);
            }
        }
        // Active window did not actually change. Check if active window title changed.
        Ok(active_window_title_changed)
    }

    // Short wrappers
    fn get_active_window(&self) -> io::Result<xcb::Window> {
        get_active_window(
            &self.connection,
            self.root_window,
            self.non_static_atoms.active_window,
        )
    }
    fn get_text_property<'a>(
        &'a self,
        w: xcb::Window,
        atom: xcb::Atom,
    ) -> GetTextPropertyCookie<'a> {
        get_text_property(&self.connection, &self.non_static_atoms, w, atom)
    }
}

impl NonStaticAtoms {
    /// Get values from server
    fn read_from_conn(conn: &xcb::Connection) -> io::Result<Self> {
        let to_error = |_| io::Error::new(io::ErrorKind::Other, "xcb_intern_atom");
        let active_window_cookie = xcb::intern_atom(&conn, true, "_NET_ACTIVE_WINDOW");
        let utf8_string_cookie = xcb::intern_atom(&conn, true, "UTF8_STRING");
        let compound_text_cookie = xcb::intern_atom(&conn, true, "COMPOUND_TEXT");
        Ok(NonStaticAtoms {
            active_window: active_window_cookie.get_reply().map_err(to_error)?.atom(),
            utf8_string: utf8_string_cookie.get_reply().map_err(to_error)?.atom(),
            compound_text: compound_text_cookie.get_reply().map_err(to_error)?.atom(),
        })
    }
}

/// Request a text property, returning a handle on the request.
fn get_text_property<'a>(
    connection: &'a xcb::Connection,
    non_static_atoms: &'a NonStaticAtoms,
    window: xcb::Window,
    atom: xcb::Atom,
) -> GetTextPropertyCookie<'a> {
    GetTextPropertyCookie {
        cookie: xcb::get_property(connection, false, window, atom, xcb::ATOM_ANY, 0, 1024),
        non_static_atoms: non_static_atoms,
    }
}

/// Ongoing request for a text property (impl detail of Stalker).
struct GetTextPropertyCookie<'a> {
    cookie: xcb::GetPropertyCookie<'a>,
    non_static_atoms: &'a NonStaticAtoms,
}

impl<'a> GetTextPropertyCookie<'a> {
    /// Retrieve the text property as a String, or None if error.
    fn get_reply(&self) -> Option<String> {
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

/// Polling support for the listener: just use the underlying file descriptor.
impl mio::Evented for Stalker {
    fn register(
        &self,
        poll: &mio::Poll,
        token: mio::Token,
        interest: mio::Ready,
        opts: mio::PollOpt,
    ) -> io::Result<()> {
        mio::unix::EventedFd(&self.connection.as_raw_fd()).register(poll, token, interest, opts)
    }

    fn reregister(
        &self,
        poll: &mio::Poll,
        token: mio::Token,
        interest: mio::Ready,
        opts: mio::PollOpt,
    ) -> io::Result<()> {
        mio::unix::EventedFd(&self.connection.as_raw_fd()).reregister(poll, token, interest, opts)
    }

    fn deregister(&self, poll: &mio::Poll) -> io::Result<()> {
        mio::unix::EventedFd(&self.connection.as_raw_fd()).deregister(poll)
    }
}

/// Asynchronous stream producing ActiveWindowMetadata when active window changes.
pub struct ActiveWindowChanges {
    inner: PollEvented<Stalker>,
}

impl ActiveWindowChanges {
    /// Create a new stream.
    /// No tokio reactor is specified, so the Stalker will be registered lazily at first use.
    pub fn new() -> io::Result<Self> {
        Ok(ActiveWindowChanges {
            inner: PollEvented::new(Stalker::new()?),
        })
    }

    /// Request the current metadata, irrespective of the stream state.
    /// This can be used for initialisation, before the first change.
    pub fn get_current_metadata(&self) -> io::Result<(ActiveWindowMetadata, time::Instant)> {
        self.inner.get_ref().get_active_window_metadata()
    }
}

/// Asynchronous Stream implementation.
impl Stream for ActiveWindowChanges {
    type Item = (ActiveWindowMetadata, time::Instant);
    type Error = io::Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, io::Error> {
        // Check if there is inbound data (xcb events to process)
        match self.inner.poll_read_ready(mio::Ready::readable()) {
            Ok(Async::Ready(_)) => (),
            Ok(Async::NotReady) => return Ok(Async::NotReady),
            Err(e) => return Err(e),
        }
        // Read all events
        let active_window_changed = self.inner.get_mut().process_events()?;

        // Reset read flag, will be set again if data arrives on socket
        self.inner.clear_read_ready(mio::Ready::readable())?;

        if active_window_changed {
            // get_active_window_metadata requests replies are all consumed
            Ok(Async::Ready(Some(self.inner
                .get_ref()
                .get_active_window_metadata()?)))
        } else {
            Ok(Async::NotReady)
        }
    }
}
