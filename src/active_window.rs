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
    get_text_property_context: GetTextPropertyContext,
    active_window: CachedProperty<xcb::Window>,
    active_window_title: CachedProperty<String>,
    active_window_class: CachedProperty<String>,
}

impl ActiveWindowWatcher {
    /// Create and configure a new listener.
    pub fn new() -> Result<Self, Error> {
        // Xcb Boilerplate
        let (connection, screen_id) =
            xcb::Connection::connect(None).with_context(|| "xcb_connect")?;
        let root_window = {
            let setup = connection.get_setup();
            let screen = setup.roots().nth(screen_id as usize).unwrap();
            screen.root()
        };

        let get_text_property_context = GetTextPropertyContext::setup(&connection)?;

        // Listen to property changes for root window.
        // This is where the active window property is maintained.
        enable_property_change_notifications(&connection, root_window);

        let mut active_window: CachedProperty<xcb::Window> = CachedProperty::invalid(
            root_window,
            request_atom(&connection, "_NET_ACTIVE_WINDOW")()?,
        );

        active_window.refresh(&connection)()?;
        enable_property_change_notifications(&connection, active_window.value);

        let mut active_window_title: CachedProperty<String> =
            CachedProperty::invalid(active_window.value, xcb::ATOM_WM_NAME);
        let mut active_window_class: CachedProperty<String> =
            CachedProperty::invalid(active_window.value, xcb::ATOM_WM_CLASS);

        let title_req = active_window_title.refresh(&connection, &get_text_property_context);
        let class_req = active_window_class.refresh(&connection, &get_text_property_context);
        title_req()?;
        class_req()?;

        Ok(ActiveWindowWatcher {
            connection,
            root_window,
            get_text_property_context,
            active_window,
            active_window_title,
            active_window_class,
        })
    }

    fn cached_metadata(&self) -> ActiveWindowMetadata {
        ActiveWindowMetadata {
            id: self.active_window.value as _,
            title: self.active_window_title.value.clone(),
            class: self.active_window_class.value.clone(),
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

                while let Some(event) = self.connection.poll_for_event() {
                    if event.response_type() == xcb::PROPERTY_NOTIFY {
                        let event: &xcb::PropertyNotifyEvent = unsafe { xcb::cast_event(&event) };
                        self.active_window.maybe_invalidate(&event);
                        self.active_window_title.maybe_invalidate(&event);
                        self.active_window_class.maybe_invalidate(&event);
                    }
                }

                let old_metadata = self.cached_metadata();

                if !self.active_window.valid {
                    let old = self.active_window.value;
                    self.active_window.refresh(&self.connection)()?;
                    let new = self.active_window.value;

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

                let refresh_title = self
                    .active_window_title
                    .refresh_if_needed(&self.connection, &self.get_text_property_context);
                let refresh_class = self
                    .active_window_class
                    .refresh_if_needed(&self.connection, &self.get_text_property_context);
                refresh_title()?;
                refresh_class()?;

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

struct CachedProperty<T> {
    window: xcb::Window,
    atom: xcb::Atom,
    value: T,
    valid: bool,
}

impl<T: Default> CachedProperty<T> {
    fn invalid(window: xcb::Window, atom: xcb::Atom) -> Self {
        CachedProperty {
            window,
            atom,
            value: T::default(),
            valid: false,
        }
    }
}

impl<T> CachedProperty<T> {
    fn maybe_invalidate(&mut self, event: &xcb::PropertyNotifyEvent) {
        if (event.window(), event.atom()) == (self.window, self.atom) {
            self.valid = false
        }
    }

    fn change_window(&mut self, new_window: xcb::Window) {
        if self.window != new_window {
            self.window = new_window;
            self.valid = false;
        }
    }
}

impl CachedProperty<xcb::Window> {
    fn refresh<'a>(
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
                self.value = reply.value()[0];
                self.valid = true;
                Ok(())
            } else {
                Err(Error::msg("xcb_get_property(active_window): invalid reply"))
            }
        }
    }
}

/// Contains cached non-standard atoms seen to represent text.
struct GetTextPropertyContext {
    utf8_string: xcb::Atom,
    compound_text: xcb::Atom,
}

impl GetTextPropertyContext {
    fn setup(connection: &xcb::Connection) -> Result<Self, Error> {
        let utf8_string_request = request_atom(connection, "UTF8_STRING");
        let compound_text_request = request_atom(connection, "COMPOUND_TEXT");
        Ok(GetTextPropertyContext {
            utf8_string: utf8_string_request()?,
            compound_text: compound_text_request()?,
        })
    }
}

impl CachedProperty<String> {
    fn refresh<'a>(
        &'a mut self,
        connection: &'a xcb::Connection,
        context: &'a GetTextPropertyContext,
    ) -> impl FnOnce() -> Result<(), Error> + 'a {
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
            let reply = cookie.get_reply()?;
            if reply.format() == 8 && reply.bytes_after() == 0 && reply.value_len() > 0 {
                let text_atoms = [xcb::ATOM_STRING, context.utf8_string, context.compound_text];
                if text_atoms.contains(&reply.type_()) {
                    let text = std::str::from_utf8(reply.value())?;
                    let text = text.split('\0').next().unwrap();
                    self.value = String::from(text);
                    self.valid = true;
                    Ok(())
                } else {
                    Err(Error::msg(format!(
                        "get_text_property: invalid atom {}",
                        reply.type_()
                    )))
                }
            } else {
                Err(Error::msg("get_text_property: format"))
            }
        }
    }

    fn refresh_if_needed<'a>(
        &'a mut self,
        connection: &'a xcb::Connection,
        context: &'a GetTextPropertyContext,
    ) -> impl FnOnce() -> Result<(), Error> + 'a {
        let request = match self.valid {
            true => None,
            false => Some(self.refresh(connection, context)),
        };
        move || {
            if let Some(request) = request {
                request()?;
            }
            Ok(())
        }
    }
}
