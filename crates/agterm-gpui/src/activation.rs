//! Raising the frontmost window for a launch that forwarded here, with what it handed over (plan > Solution Overview
//! > Single instance; `docs/spikes/20261014-gpui-layer-shell.md`, point 4).
//!
//! - Wayland: GPUI consumes `XDG_ACTIVATION_TOKEN` for its first toplevel only and has no call that activates a
//!   window with a token from elsewhere, so this binds `xdg_activation_v1` on GPUI's own `wl_display` (a second event
//!   queue over the same connection, from the window's raw handles) and calls `activate(token, surface)`. A GPUI patch
//!   (`Window::activate_with_token`) replaces it later (plan > Technical Details > GPUI patches, 2).
//! - X11: GPUI's `activate_window` sends `_NET_ACTIVE_WINDOW` with time 0, which a window manager preventing focus
//!   stealing may refuse, so this sends it with the startup id's `_TIME`, sets `_NET_WM_USER_TIME` and ends the
//!   startup sequence (`remove: ID=…`), on a connection of its own to the same display (GPUI patch 3 later).
//! - Without a token or a startup id, GPUI's own `activate_window`; headless, nothing.

use std::error::Error;
use std::ffi::c_void;

use gpui_kit::{App, Window};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};

use crate::instance::Activation;

type Failure = Box<dyn Error + Send + Sync>;

/// Raises `window` for `activation`, by the display server GPUI runs on.
pub fn activate(window: &mut Window, cx: &App, activation: &Activation) {
    let (token, startup_id) = (activation.token.as_deref(), activation.startup_id.as_deref());
    // GtkApplication sends one value under both keys; each server prefers its own
    let outcome = match cx.compositor_name() {
        "Wayland" => raise(window, token.or(startup_id), wayland::activate),
        "X11" => raise(window, startup_id.or(token), x11::activate),
        backend => {
            tracing::info!(backend, "activation: no display server to raise a window on");
            return;
        }
    };
    if let Err(error) = outcome {
        tracing::warn!(%error, "activation: the window was not raised");
    } else {
        tracing::info!(
            token = token.is_some(),
            startup_id = startup_id.is_some(),
            "activation: window raised"
        );
    }
}

/// This process's own launch opened `window`, its first: on X11 the launch's startup sequence ends here (GPUI handles
/// neither `DESKTOP_STARTUP_ID` nor startup notification); on Wayland GPUI has already activated its first toplevel
/// with the launch's token.
pub fn launched(window: &mut Window, cx: &App, activation: &Activation) {
    let Some(startup_id) = activation.startup_id.as_deref() else {
        return;
    };
    if cx.compositor_name() != "X11" {
        return;
    }
    match x11::complete_startup(window, startup_id) {
        Ok(()) => tracing::info!("activation: the launch's startup sequence ended"),
        Err(error) => tracing::warn!(%error, "activation: the launch's startup sequence did not end"),
    }
}

/// Raises `window` with what the launch `handed` over through `with`, or GPUI's own request without it.
fn raise(
    window: &mut Window,
    handed: Option<&str>,
    with: fn(&Window, &str) -> Result<(), Failure>,
) -> Result<(), Failure> {
    if let Some(handed) = handed {
        with(window, handed)
    } else {
        window.activate_window();
        Ok(())
    }
}

/// The X server time a startup id carries (`…_TIME<n>`, as GDK reads it); `None` without one.
#[must_use]
pub fn startup_time(startup_id: &str) -> Option<u32> {
    let after = &startup_id[startup_id.rfind("_TIME")? + "_TIME".len()..];
    let digits = after.bytes().take_while(u8::is_ascii_digit).count();
    after[..digits].parse().ok()
}

/// The startup-notification message ending the sequence `startup_id` (startup-notification spec: a value with a
/// space, a quote or a backslash is quoted, the latter two escaped).
#[must_use]
pub fn remove_message(startup_id: &str) -> String {
    if startup_id.bytes().any(|byte| matches!(byte, b' ' | b'"' | b'\\')) {
        let escaped = startup_id.replace('\\', "\\\\").replace('"', "\\\"");
        format!("remove: ID=\"{escaped}\"")
    } else {
        format!("remove: ID={startup_id}")
    }
}

/// A startup-notification message as the 20-byte client message payloads it travels in: NUL-terminated, the last
/// payload zero-padded.
#[must_use]
pub fn message_chunks(message: &str) -> Vec<[u8; 20]> {
    let mut bytes = message.as_bytes().to_vec();
    bytes.push(0);
    bytes
        .chunks(20)
        .map(|chunk| {
            let mut payload = [0; 20];
            payload[..chunk.len()].copy_from_slice(chunk);
            payload
        })
        .collect()
}

mod wayland {
    use wayland_backend::client::{Backend, ObjectId};
    use wayland_client::globals::{GlobalListContents, registry_queue_init};
    use wayland_client::protocol::{wl_registry, wl_surface::WlSurface};
    use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
    use wayland_protocols::xdg::activation::v1::client::xdg_activation_v1::XdgActivationV1;

    use super::{Failure, HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle, Window, c_void};

    /// The second event queue's state: no event of these objects matters here.
    struct Queue;

    impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Queue {
        fn event(
            _: &mut Self,
            _: &wl_registry::WlRegistry,
            _: wl_registry::Event,
            _: &GlobalListContents,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<XdgActivationV1, ()> for Queue {
        fn event(
            _: &mut Self,
            _: &XdgActivationV1,
            _: <XdgActivationV1 as Proxy>::Event,
            (): &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    /// Activates `window` with `token` on GPUI's own Wayland connection (see the module docs).
    pub(super) fn activate(window: &Window, token: &str) -> Result<(), Failure> {
        let RawDisplayHandle::Wayland(display) = HasDisplayHandle::display_handle(window)?.as_raw() else {
            return Err("not a Wayland window".into());
        };
        let RawWindowHandle::Wayland(surface) = HasWindowHandle::window_handle(window)?.as_raw() else {
            return Err("not a Wayland window".into());
        };
        // SAFETY: GPUI's live `wl_display`, which outlives this call (the window holds the connection); a backend
        // made from a foreign display does not take ownership of it and never closes it.
        let backend = unsafe { Backend::from_foreign_display(display.display.as_ptr().cast()) };
        let connection = Connection::from_backend(backend);
        let (globals, mut queue) = registry_queue_init::<Queue>(&connection)?;
        let activation: XdgActivationV1 = globals.bind(&queue.handle(), 1..=1, ())?;
        let surface_ptr = surface.surface.as_ptr().cast::<c_void>().cast();
        // SAFETY: the window's live `wl_surface` proxy on that same connection, alive for the whole call.
        let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), surface_ptr) }?;
        let surface = WlSurface::from_id(&connection, id)?;
        activation.activate(token.to_owned(), &surface);
        activation.destroy();
        queue.roundtrip(&mut Queue)?;
        // a registry has no destructor request: without this one more stays on the compositor per activation (where
        // the compositor has `wl_fixes`; GPUI patch 2 replaces this whole binding)
        globals.destroy();
        Ok(())
    }
}

mod x11 {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{
        AtomEnum, ClientMessageEvent, ConnectionExt as _, CreateWindowAux, EventMask, PropMode, WindowClass,
    };
    use x11rb::rust_connection::RustConnection;
    use x11rb::wrapper::ConnectionExt as _;
    use x11rb::{COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT};

    use super::{Failure, HasWindowHandle, RawWindowHandle, Window, message_chunks, remove_message, startup_time};

    /// `_NET_ACTIVE_WINDOW`'s source indication for an application.
    const SOURCE_APPLICATION: u32 = 1;

    /// A connection of its own to GPUI's X display, aimed at one of GPUI's windows.
    struct Target {
        conn: RustConnection,
        root: u32,
        window: u32,
    }

    impl Target {
        fn of(window: &Window) -> Result<Self, Failure> {
            let window = match HasWindowHandle::window_handle(window)?.as_raw() {
                RawWindowHandle::Xcb(handle) => handle.window.get(),
                RawWindowHandle::Xlib(handle) => u32::try_from(handle.window)?,
                _ => return Err("not an X11 window".into()),
            };
            let (conn, screen) = x11rb::connect(None)?;
            let root = conn
                .setup()
                .roots
                .get(screen)
                .ok_or("the display has no such screen")?
                .root;
            Ok(Self { conn, root, window })
        }

        fn atom(&self, name: &str) -> Result<u32, Failure> {
            Ok(self.conn.intern_atom(false, name.as_bytes())?.reply()?.atom)
        }

        /// `_NET_WM_USER_TIME`: the launch's event time, which a window manager weighs against focus stealing.
        fn set_user_time(&self, time: u32) -> Result<(), Failure> {
            let user_time = self.atom("_NET_WM_USER_TIME")?;
            self.conn
                .change_property32(PropMode::REPLACE, self.window, user_time, AtomEnum::CARDINAL, &[time])?;
            Ok(())
        }

        /// Asks the window manager to raise and focus the window (`_NET_ACTIVE_WINDOW`, from an application).
        fn request_active(&self, time: u32) -> Result<(), Failure> {
            let message = self.atom("_NET_ACTIVE_WINDOW")?;
            let active = ClientMessageEvent::new(32, self.window, message, [SOURCE_APPLICATION, time, 0, 0, 0]);
            let to_window_manager = EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY;
            self.conn.send_event(false, self.root, to_window_manager, active)?;
            Ok(())
        }

        /// Ends the startup sequence `startup_id`, from a window of its own as libstartup-notification sends it.
        fn end_sequence(&self, startup_id: &str) -> Result<(), Failure> {
            let (begin, more) = (self.atom("_NET_STARTUP_INFO_BEGIN")?, self.atom("_NET_STARTUP_INFO")?);
            let messenger = self.conn.generate_id()?;
            let aux = CreateWindowAux::new().override_redirect(1);
            self.conn.create_window(
                COPY_DEPTH_FROM_PARENT,
                messenger,
                self.root,
                -100,
                -100,
                1,
                1,
                0,
                WindowClass::INPUT_ONLY,
                COPY_FROM_PARENT,
                &aux,
            )?;
            for (index, payload) in message_chunks(&remove_message(startup_id)).into_iter().enumerate() {
                let kind = if index == 0 { begin } else { more };
                let event = ClientMessageEvent::new(8, messenger, kind, payload);
                self.conn
                    .send_event(false, self.root, EventMask::PROPERTY_CHANGE, event)?;
            }
            self.conn.destroy_window(messenger)?;
            Ok(())
        }
    }

    /// Raises `window` for the launch `startup_id` names and ends its startup sequence (see the module docs).
    pub(super) fn activate(window: &Window, startup_id: &str) -> Result<(), Failure> {
        let target = Target::of(window)?;
        let time = startup_time(startup_id);
        if let Some(time) = time {
            target.set_user_time(time)?;
        }
        target.request_active(time.unwrap_or(0))?;
        target.end_sequence(startup_id)?;
        target.conn.sync()?;
        Ok(())
    }

    /// This launch's own first window exists: its user time is the launch's, and the sequence ends (what GDK does
    /// when a GTK app maps its first window; a window manager focuses a new window by itself).
    pub(super) fn complete_startup(window: &Window, startup_id: &str) -> Result<(), Failure> {
        let target = Target::of(window)?;
        if let Some(time) = startup_time(startup_id) {
            target.set_user_time(time)?;
        }
        target.end_sequence(startup_id)?;
        target.conn.sync()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_startup_time_is_the_last_time_suffix() {
        assert_eq!(startup_time("launcher-1-host_TIME12345"), Some(12345));
        assert_eq!(startup_time("a_TIME1_TIME22x"), Some(22));
        assert_eq!(startup_time("no-time"), None);
        assert_eq!(startup_time("x_TIME"), None);
        assert_eq!(startup_time("x_TIME99999999999"), None, "beyond 32 bits");
    }

    #[test]
    fn the_remove_message_quotes_only_what_needs_it() {
        assert_eq!(remove_message("a-1_TIME5"), "remove: ID=a-1_TIME5");
        assert_eq!(remove_message("a b"), "remove: ID=\"a b\"");
        assert_eq!(remove_message("q\"\\"), "remove: ID=\"q\\\"\\\\\"");
    }

    #[test]
    fn a_message_travels_nul_terminated_in_padded_payloads() {
        let chunks = message_chunks("remove: ID=0123456789");
        assert_eq!(chunks.len(), 2);
        assert_eq!(&chunks[0], b"remove: ID=012345678");
        assert_eq!(&chunks[1][..2], b"9\0");
        assert!(chunks[1][2..].iter().all(|&byte| byte == 0));
        // a message filling its payloads exactly still gets its NUL
        let exact = message_chunks(&"x".repeat(20));
        assert_eq!(exact.len(), 2);
        assert_eq!(exact[1], [0; 20]);
    }
}
