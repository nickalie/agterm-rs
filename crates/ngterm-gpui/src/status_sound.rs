//! Status sounds and the display's alert bell (upstream's `StatusSoundPlayer.swift`; port of the GTK build's
//! `status_sound.rs`). Linux has none of the named macOS system sounds upstream plays, so every name, `default` and
//! `beep` included, sounds the display's alert bell, as the GTK build's `gdk::Display::beep` did; the names are still
//! validated against upstream's list by the core. Upstream throttles each clip on its own name ([`SoundThrottle`]);
//! here every name rings the same bell, so the throttle keys on the bell itself, and a burst of `session.status
//! --sound` calls, whatever names they give, rings it at most once per window.
//!
//! The bell, as GDK rings it: on Wayland the `xdg_system_bell_v1` global on GPUI's own connection, rung for the window
//! (a compositor without it has no bell to ring); on X11 the core protocol's `Bell` on a connection of its own to the
//! same display. A terminal's BEL rings nothing: upstream's `GHOSTTY_ACTION_RING_BELL` is not a notification source
//! and neither upstream nor the GTK build sounds it.

use std::cell::RefCell;
use std::error::Error;
use std::time::Instant;

use gpui_kit::{App, Window};
use ngterm_core::sound_throttle::SoundThrottle;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};

type Failure = Box<dyn Error + Send + Sync>;

/// The one throttle key: every status sound is the same bell.
const BELL: &str = "bell";

thread_local! {
    static THROTTLE: RefCell<SoundThrottle> = RefCell::new(SoundThrottle::default());
}

/// Whether the bell may ring for a sound at `now`: any status sound within the throttle window of the last ring is
/// suppressed, whatever its name.
#[must_use]
pub fn allow(throttle: &mut SoundThrottle, now: Instant) -> bool {
    throttle.allow(BELL, now)
}

/// Plays status sound `name` through `ring` (the display's bell) unless the bell rang within the throttle window.
pub fn play(name: &str, ring: impl FnOnce()) {
    if THROTTLE.with(|throttle| allow(&mut throttle.borrow_mut(), Instant::now())) {
        ring();
    } else {
        tracing::debug!(name, "status sound throttled");
    }
}

/// Rings the display's alert bell for `window`, by the display server GPUI runs on; headless, nothing.
pub fn ring(window: &Window, cx: &App) {
    let rung = match cx.compositor_name() {
        "Wayland" => wayland::ring(window),
        "X11" => x11::ring(),
        backend => {
            tracing::debug!(backend, "no display to sound the alert bell on");
            return;
        }
    };
    if let Err(error) = rung {
        tracing::debug!(%error, "the alert bell did not ring");
    }
}

mod wayland {
    use wayland_backend::client::{Backend, ObjectId};
    use wayland_client::globals::{GlobalListContents, registry_queue_init};
    use wayland_client::protocol::{wl_registry, wl_surface::WlSurface};
    use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
    use wayland_protocols::xdg::system_bell::v1::client::xdg_system_bell_v1::XdgSystemBellV1;

    use super::{Failure, HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle, Window};

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

    impl Dispatch<XdgSystemBellV1, ()> for Queue {
        fn event(
            _: &mut Self,
            _: &XdgSystemBellV1,
            _: <XdgSystemBellV1 as Proxy>::Event,
            (): &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    /// Rings the compositor's bell for `window` on GPUI's own Wayland connection (a second event queue over it, as
    /// `activation.rs` binds `xdg_activation_v1`).
    pub(super) fn ring(window: &Window) -> Result<(), Failure> {
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
        let bound = globals.bind::<XdgSystemBellV1, _, _>(&queue.handle(), 1..=1, ());
        let outcome = match bound {
            Ok(bell) => {
                let surface_ptr = surface.surface.as_ptr().cast();
                // SAFETY: the window's live `wl_surface` proxy on that same connection, alive for the whole call.
                let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), surface_ptr) }?;
                let surface = WlSurface::from_id(&connection, id)?;
                bell.ring(Some(&surface));
                bell.destroy();
                Ok(())
            }
            Err(error) => Err(format!("the compositor has no system bell: {error}").into()),
        };
        queue.roundtrip(&mut Queue)?;
        globals.destroy();
        outcome
    }
}

mod x11 {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::ConnectionExt as _;

    use super::Failure;

    /// Rings the X server's bell at its base volume, on a connection of its own to the display GPUI runs on.
    pub(super) fn ring() -> Result<(), Failure> {
        let (conn, _) = x11rb::connect(None)?;
        conn.bell(0)?;
        conn.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn different_names_share_the_one_bell_throttle() {
        let mut throttle = SoundThrottle::default();
        let t0 = Instant::now();
        assert!(allow(&mut throttle, t0), "Ping rings");
        assert!(
            !allow(&mut throttle, t0 + Duration::from_millis(10)),
            "Hero right after does not"
        );
        assert!(allow(&mut throttle, t0 + Duration::from_millis(200)));
    }

    #[test]
    fn a_burst_rings_the_bell_once() {
        let mut rung = 0;
        play("Ping", || rung += 1);
        play("Hero", || rung += 1);
        play("default", || rung += 1);
        assert_eq!(rung, 1, "one bell for the burst, whatever the names");
    }
}
