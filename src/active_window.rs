use crate::ActiveWindowMetadata;
use anyhow::{Context, Error};
use star::{FdEventType, WaitFdEvent};
use std::future::Future;
use std::os::unix::io::AsRawFd;

/// Listener for changes of the active window using xcb.
/// Owns the connection to the X server.
pub struct ActiveWindowWatcher {
    connection: xcb::Connection,
    root_window: xcb::Window,
    text_atoms: TextAtoms,
    active_window: CachedProperty<xcb::Window>,
    active_window_title: CachedProperty<Option<String>>,
    active_window_class: CachedProperty<Option<String>>,
}

impl ActiveWindowWatcher {
    /// Create and configure a new listener.
    pub fn new() -> Result<Self, Error> {
        let (connection, screen_id) =
            xcb::Connection::connect(None).with_context(|| "xcb_connect")?;
        let root_window = {
            let setup = connection.get_setup();
            let screen = setup.roots().nth(screen_id as usize).unwrap();
            screen.root()
        };

        // Cache some non-static atoms
        let text_atoms_request = TextAtoms::request(&connection);
        let active_window_atom_request = request_atom(&connection, "_NET_ACTIVE_WINDOW");

        // Always listen to property changes for root window.
        // This is where the active window property is maintained.
        enable_property_change_notifications(&connection, root_window);

        let mut active_window = CachedProperty::new(root_window, active_window_atom_request()?);
        active_window.reload(&connection)()?;
        let active_window_value = active_window.cached_value.unwrap();
        enable_property_change_notifications(&connection, active_window_value);

        let mut active_window_title = CachedProperty::new(active_window_value, xcb::ATOM_WM_NAME);
        let mut active_window_class = CachedProperty::new(active_window_value, xcb::ATOM_WM_CLASS);
        let text_atoms = text_atoms_request()?;
        let title_req = active_window_title.request(&connection, &text_atoms);
        let class_req = active_window_class.request(&connection, &text_atoms);
        active_window_title.cached_value = Some(title_req());
        active_window_class.cached_value = Some(class_req());

        Ok(ActiveWindowWatcher {
            connection,
            root_window,
            text_atoms,
            active_window,
            active_window_title,
            active_window_class,
        })
    }

    /// Get metadata, assume everything is cached (not in transient state).
    pub fn cached_metadata(&self) -> ActiveWindowMetadata {
        ActiveWindowMetadata {
            id: self.active_window.cached_value.unwrap(),
            title: self.active_window_title.cached_value.clone().unwrap(),
            class: self.active_window_class.cached_value.clone().unwrap(),
        }
    }

    /// Return a future which gives the next window metadata, when it changes.
    pub fn active_window_change<'w>(
        &'w mut self,
    ) -> impl Future<Output = Result<ActiveWindowMetadata, Error>> + 'w {
        async move {
            loop {
                WaitFdEvent::new(
                    self.connection.as_raw_fd(),
                    FdEventType::IN | FdEventType::ERR,
                )
                .await;

                let old_metadata = self.cached_metadata();

                // Invalidate if changes
                while let Some(event) = self.connection.poll_for_event() {
                    if event.response_type() == xcb::PROPERTY_NOTIFY {
                        let event: &xcb::PropertyNotifyEvent = unsafe { xcb::cast_event(&event) };
                        self.active_window.handle_event(&event);
                        self.active_window_title.handle_event(&event);
                        self.active_window_class.handle_event(&event);
                    }
                }

                if !self.active_window.cached_value.is_some() {
                    let old = old_metadata.id;
                    self.active_window.reload(&self.connection)()?;
                    let new = self.active_window.cached_value.unwrap();

                    // May invalidate dependent properties
                    self.active_window_title.change_window(new);
                    self.active_window_class.change_window(new);

                    // Update notification status, but ensure root_window is always notified
                    if new != old && old != self.root_window {
                        disable_property_change_notifications(&self.connection, old);
                    }
                    if new != old && new != self.root_window {
                        enable_property_change_notifications(&self.connection, new);
                    }
                }

                let reload_title = self
                    .active_window_title
                    .reload_if_invalid(&self.connection, &self.text_atoms);
                let reload_class = self
                    .active_window_class
                    .reload_if_invalid(&self.connection, &self.text_atoms);
                reload_title();
                reload_class();

                let new_metadata = self.cached_metadata();
                if new_metadata != old_metadata {
                    return Ok(new_metadata);
                }
            }
        }
    }
}

fn enable_property_change_notifications(connection: &xcb::Connection, w: xcb::Window) {
    let masks = [(xcb::CW_EVENT_MASK, xcb::EVENT_MASK_PROPERTY_CHANGE)];
    xcb::change_window_attributes(connection, w, &masks);
}
fn disable_property_change_notifications(connection: &xcb::Connection, w: xcb::Window) {
    let masks = [(xcb::CW_EVENT_MASK, xcb::NONE)];
    xcb::change_window_attributes(connection, w, &masks);
}

fn request_atom<'a>(
    connection: &'a xcb::Connection,
    name: &'a str,
) -> impl FnOnce() -> Result<xcb::Atom, Error> + 'a {
    let cookie = xcb::intern_atom(connection, true, name);
    move || {
        cookie
            .get_reply()
            .with_context(|| format!("xcb_intern_atom: {}", name))
            .map(|r| r.atom())
    }
}

/// For a given property, store cached value, validity status, and property location.
/// This is used as a tool to track which value is invalidated by events _transiently_.
struct CachedProperty<T> {
    window: xcb::Window,
    atom: xcb::Atom,
    cached_value: Option<T>,
}

impl<T: Default> CachedProperty<T> {
    fn new(window: xcb::Window, atom: xcb::Atom) -> Self {
        CachedProperty {
            window,
            atom,
            cached_value: None,
        }
    }
}

impl<T> CachedProperty<T> {
    fn handle_event(&mut self, event: &xcb::PropertyNotifyEvent) {
        if (event.window(), event.atom()) == (self.window, self.atom) {
            self.cached_value = None
        }
    }

    fn change_window(&mut self, new_window: xcb::Window) {
        if self.window != new_window {
            self.window = new_window;
            self.cached_value = None;
        }
    }
}

impl CachedProperty<xcb::Window> {
    fn reload<'a>(
        &'a mut self,
        connection: &'a xcb::Connection,
    ) -> impl FnOnce() -> Result<(), Error> + 'a {
        let cookie = xcb::get_property(
            connection,
            false,
            self.window,
            self.atom,
            xcb::ATOM_WINDOW,
            0,
            (std::mem::size_of::<xcb::Window>() / 4) as u32,
        );
        move || {
            let reply = cookie
                .get_reply()
                .with_context(|| "xcb_get_property(window): failed")?;
            if reply.type_() == xcb::ATOM_WINDOW
                && reply.bytes_after() == 0
                && reply.value_len() == 1
                && reply.format() as usize == std::mem::size_of::<xcb::Window>() * 8
            {
                let window: xcb::Window = reply.value()[0];
                self.cached_value = Some(window);
                Ok(())
            } else {
                Err(Error::msg("xcb_get_property(window): invalid reply"))
            }
        }
    }
}

/// Contains cached non-standard atoms seen to represent text.
struct TextAtoms {
    utf8_string: xcb::Atom,
    compound_text: xcb::Atom,
}

impl TextAtoms {
    fn request<'a>(connection: &'a xcb::Connection) -> impl FnOnce() -> Result<Self, Error> + 'a {
        let utf8_string_request = request_atom(connection, "UTF8_STRING");
        let compound_text_request = request_atom(connection, "COMPOUND_TEXT");
        move || {
            Ok(TextAtoms {
                utf8_string: utf8_string_request()?,
                compound_text: compound_text_request()?,
            })
        }
    }
}

impl CachedProperty<Option<String>> {
    /// Ignore errors, treat them as "no property".
    fn request<'a>(
        &self,
        connection: &'a xcb::Connection,
        atoms: &'a TextAtoms,
    ) -> impl FnOnce() -> Option<String> + 'a {
        let cookie = xcb::get_property(
            connection,
            false,
            self.window,
            self.atom,
            xcb::ATOM_ANY,
            0,
            1024,
        );
        move || {
            let reply = cookie.get_reply().ok()?;
            if reply.format() == 8 && reply.bytes_after() == 0 && reply.value_len() > 0 {
                let text_atoms = [xcb::ATOM_STRING, atoms.utf8_string, atoms.compound_text];
                if text_atoms.contains(&reply.type_()) {
                    let text = std::str::from_utf8(reply.value()).ok()?;
                    let text = text.split('\0').next().unwrap();
                    Some(String::from(text))
                } else {
                    dbg!(reply.type_()); // Log unknown text atom
                    None
                }
            } else {
                None
            }
        }
    }

    fn reload_if_invalid<'a>(
        &'a mut self,
        connection: &'a xcb::Connection,
        atoms: &'a TextAtoms,
    ) -> impl FnOnce() + 'a {
        let request = match self.cached_value.is_some() {
            true => None,
            false => Some(self.request(connection, atoms)),
        };
        move || {
            if let Some(request) = request {
                self.cached_value = Some(request())
            }
        }
    }
}
