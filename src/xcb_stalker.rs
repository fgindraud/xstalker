#![deny(deprecated)]
extern crate mio;
extern crate xcb; // for xcb_stalker

use std;
use std::io;
use std::os::unix::io::AsRawFd;
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
}

/// Store non static useful atoms (impl detail of Stalker).
struct NonStaticAtoms {
    active_window: xcb::Atom,
    utf8_string: xcb::Atom,
    compound_text: xcb::Atom,
}

/// Ongoing request for a text property (impl detail of Stalker).
struct GetTextPropertyCookie<'a> {
    cookie: xcb::GetPropertyCookie<'a>,
    non_static_atoms: &'a NonStaticAtoms,
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
        let non_static_atoms = NonStaticAtoms::new(&conn)?;

        // Listen to property changes for root window.
        // This is where the active window property is maintained.
        let values = [(xcb::CW_EVENT_MASK, xcb::EVENT_MASK_PROPERTY_CHANGE)];
        xcb::change_window_attributes(&conn, root_window, &values);
        conn.flush();
        conn.has_error().map_err(conn_to_io_error)?;

        Ok(Stalker {
            connection: conn,
            root_window: root_window,
            non_static_atoms: non_static_atoms,
        })
    }

    /// Get the current active window metadata.
    fn get_active_window_metadata(&self) -> io::Result<ActiveWindowMetadata> {
        let w = self.get_active_window()?;
        // Requests
        let title = self.get_text_property(w, xcb::ATOM_WM_NAME);
        let class = self.get_text_property(w, xcb::ATOM_WM_CLASS);
        // Process replies
        let title = title.get_reply();
        let class = class.get_reply().map(|mut text| match text.find('\0') {
            Some(offset) => {
                text.truncate(offset);
                text
            }
            None => text,
        });
        Ok(ActiveWindowMetadata {
            title: title,
            class: class,
        })
    }

    /// Process all pending events.
    /// Return true if the active window metadata has changed, and must be queried again.
    fn process_events(&self) -> bool {
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

    /// Impl detail: get active window id.
    /// Not finding the property is an error.
    fn get_active_window(&self) -> io::Result<xcb::Window> {
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

    /// Request a text property, returning a handle on the request.
    fn get_text_property<'a>(
        &'a self,
        window: xcb::Window,
        atom: xcb::Atom,
    ) -> GetTextPropertyCookie<'a> {
        GetTextPropertyCookie {
            cookie: xcb::get_property(
                &self.connection,
                false,
                window,
                atom,
                xcb::ATOM_ANY,
                0,
                1024,
            ),
            non_static_atoms: &self.non_static_atoms,
        }
    }
}

impl NonStaticAtoms {
    /// Get values from server
    fn new(conn: &xcb::Connection) -> io::Result<Self> {
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

impl<'a> GetTextPropertyCookie<'a> {
    /// Retrieve the text property as a String, or None if error.
    /// TODO better handling of unknown atom ? warning ?
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
    pub fn new() -> io::Result<Self> {
        Ok(ActiveWindowChanges {
            inner: PollEvented::new(Stalker::new()?),
        })
    }

    /// Request the current metadata, irrespective of the stream state.
    /// This can be used for initialisation, before the first change.
    pub fn get_current_metadata(&self) -> io::Result<ActiveWindowMetadata> {
        self.inner.get_ref().get_active_window_metadata()
    }
}

/// Asynchronous Stream implementation.
impl Stream for ActiveWindowChanges {
    type Item = ActiveWindowMetadata;
    type Error = io::Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, io::Error> {
        // Check if readable (this also registers the fd once).
        match self.inner.poll_read_ready(mio::Ready::readable()) {
            Ok(Async::Ready(_)) => (),
            Ok(Async::NotReady) => return Ok(Async::NotReady),
            Err(e) => return Err(e),
        }
        // Read all events
        let active_window_changed = self.inner.get_ref().process_events();

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
