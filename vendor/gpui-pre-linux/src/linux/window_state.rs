//! A window's minimized state as the window manager reports it (an agterm-rs patch): GPUI tracks an X11 window's
//! `_NET_WM_STATE_HIDDEN` for itself but gives no read-back of it, which `window.list` needs. The X11 client notes the
//! state each time it reads the window's `_NET_WM_STATE` (at creation and on every change of the property) and forgets
//! the window when it goes; [`x11_window_minimized`] reads it by the window's X id, which the app has from the
//! window's raw handle. Wayland's xdg-shell gives a client no such state, so a Wayland window reads `None`.

use std::cell::RefCell;
use std::collections::HashMap;

thread_local! {
    static HIDDEN: RefCell<HashMap<u32, bool>> = RefCell::new(HashMap::new());
}

/// Whether X11 window `window` of this thread's X11 client is minimized (`_NET_WM_STATE_HIDDEN`, which a window
/// manager sets on an iconified window); `None` for a window the client does not know.
pub fn x11_window_minimized(window: u32) -> Option<bool> {
    HIDDEN.with_borrow(|hidden| hidden.get(&window).copied())
}

/// Records X11 window `window`'s state as last read from its `_NET_WM_STATE`.
#[cfg(feature = "x11")]
pub(crate) fn note_x11_minimized(window: u32, minimized: bool) {
    HIDDEN.with_borrow_mut(|hidden| hidden.insert(window, minimized));
}

/// Forgets X11 window `window`, which is going.
#[cfg(feature = "x11")]
pub(crate) fn forget_x11_window(window: u32) {
    HIDDEN.with_borrow_mut(|hidden| hidden.remove(&window));
}

#[cfg(all(test, feature = "x11"))]
mod tests {
    use super::*;

    #[test]
    fn a_noted_window_reads_back_until_it_is_forgotten() {
        assert_eq!(x11_window_minimized(7), None, "unknown");
        note_x11_minimized(7, false);
        assert_eq!(x11_window_minimized(7), Some(false));
        note_x11_minimized(7, true);
        assert_eq!(x11_window_minimized(7), Some(true));
        forget_x11_window(7);
        assert_eq!(x11_window_minimized(7), None);
    }
}
