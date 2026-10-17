//! A window's state as the window manager reports it (an agterm-rs patch): GPUI tracks an X11 window's
//! `_NET_WM_STATE` for itself but gives no read-back of its minimized state, which `window.list` needs, and while the
//! window is minimized it reads it as not maximized, though the window manager keeps it maximized, and the frame saved
//! for its next launch needs that. The X11 client notes both each time it reads the window's `_NET_WM_STATE` (at
//! creation and on every change of the property) and forgets the window when it goes; [`x11_window_minimized`] and
//! [`x11_window_maximized`] read them by the window's X id, which the app has from the window's raw handle. Wayland's
//! xdg-shell gives a client no minimized state, so a Wayland window reads `None`.

use std::cell::RefCell;
use std::collections::HashMap;

/// An X11 window's `_NET_WM_STATE`, as far as the patch reads it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct X11State {
    /// `_NET_WM_STATE_HIDDEN`.
    hidden: bool,
    /// `_NET_WM_STATE_MAXIMIZED_VERT` and `_NET_WM_STATE_MAXIMIZED_HORZ`, minimized or not.
    maximized: bool,
}

thread_local! {
    static STATES: RefCell<HashMap<u32, X11State>> = RefCell::new(HashMap::new());
}

/// Whether X11 window `window` of this thread's X11 client is minimized (`_NET_WM_STATE_HIDDEN`, which a window
/// manager sets on an iconified window); `None` for a window the client does not know.
pub fn x11_window_minimized(window: u32) -> Option<bool> {
    STATES.with_borrow(|states| states.get(&window).map(|state| state.hidden))
}

/// Whether X11 window `window` of this thread's X11 client is maximized both ways, also while it is minimized (GPUI's
/// own `is_maximized` reads false then); `None` for a window the client does not know.
pub fn x11_window_maximized(window: u32) -> Option<bool> {
    STATES.with_borrow(|states| states.get(&window).map(|state| state.maximized))
}

/// Records X11 window `window`'s state as last read from its `_NET_WM_STATE`.
#[cfg(feature = "x11")]
pub(crate) fn note_x11_state(window: u32, hidden: bool, maximized: bool) {
    STATES.with_borrow_mut(|states| states.insert(window, X11State { hidden, maximized }));
}

/// Forgets X11 window `window`, which is going.
#[cfg(feature = "x11")]
pub(crate) fn forget_x11_window(window: u32) {
    STATES.with_borrow_mut(|states| states.remove(&window));
}

#[cfg(all(test, feature = "x11"))]
mod tests {
    use super::*;

    #[test]
    fn a_noted_window_reads_back_until_it_is_forgotten() {
        assert_eq!(x11_window_minimized(7), None, "unknown");
        assert_eq!(x11_window_maximized(7), None, "unknown");
        note_x11_state(7, false, false);
        assert_eq!(x11_window_minimized(7), Some(false));
        note_x11_state(7, true, true);
        assert_eq!(x11_window_minimized(7), Some(true));
        assert_eq!(x11_window_maximized(7), Some(true), "maximized while minimized");
        forget_x11_window(7);
        assert_eq!(x11_window_minimized(7), None);
        assert_eq!(x11_window_maximized(7), None);
    }
}
