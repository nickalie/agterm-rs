//! What the core and the frontend exchange about on-screen windows: the state the frontend reports for `window.list`
//! ([`WindowState`]), the work a window command leaves for the frontend ([`WindowRequest`]), and the host-free size
//! clamp of `window.resize` (upstream's `WindowGeometry.clampSize`).
//!
//! A window is on screen exactly while its store is open: every request ends with a commit after which the frontend
//! builds a window for each newly opened store and drops the window of each closed one, so a command checks
//! [`crate::model::WindowLibrary::is_open`] where upstream asks its `WindowRegistry`.

use uuid::Uuid;

/// A width and height in points.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WindowSize {
    pub width: i64,
    pub height: i64,
}

/// The smallest frame a window takes (upstream's `.frame(minWidth: 640, minHeight: 400)` on the window content).
pub const MIN_WINDOW_SIZE: WindowSize = WindowSize {
    width: 640,
    height: 400,
};

/// Clamps each dimension of `requested` into `[min, max]` (upstream's `WindowGeometry.clampSize`): an oversized request
/// shrinks to `max`, an undersized one grows to `min`. Without a known `max` only the minimum applies; a `max` below
/// the minimum wins, as upstream's `min(max(value, lo), hi)`.
#[must_use]
pub fn clamp_size(requested: WindowSize, min: WindowSize, max: Option<WindowSize>) -> WindowSize {
    let clamp = |value: i64, low: i64, high: Option<i64>| {
        let raised = value.max(low);
        high.map_or(raised, |high| raised.min(high))
    };
    WindowSize {
        width: clamp(requested.width, min.width, max.map(|max| max.width)),
        height: clamp(requested.height, min.height, max.map(|max| max.height)),
    }
}

/// What the frontend reports about one on-screen window, for `window.list` (upstream's window cache flags) and for
/// the commands that act on its current state. A window the frontend has not reported, or no longer shows, has none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WindowState {
    pub fullscreen: bool,
    /// Maximized, not full screen.
    pub zoomed: bool,
    /// Minimized, where the windowing system reads it back (X11); `None` where it cannot (Wayland's xdg-shell has no
    /// minimized state for a client), which `window.list` reports as not minimized.
    pub minimized: Option<bool>,
    /// The size of the display the window is on, the bound of `window.resize`; `None` while unknown.
    pub display: Option<WindowSize>,
}

/// What a window command asks of a window on screen, applied by the frontend after the commit that opened it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowRequest {
    /// Raise it and give it the keyboard, restoring it when minimized (upstream's `WindowRegistry.raise`).
    Raise(Uuid),
    /// Resize its frame to the already clamped size.
    Resize { window: Uuid, size: WindowSize },
    /// Maximize it, or restore it when maximized (upstream's `NSWindow.zoom`).
    ToggleZoom(Uuid),
    /// Enter or leave full screen.
    ToggleFullscreen(Uuid),
    /// Minimize it (`true`) or restore it (`false`).
    Minimize { window: Uuid, minimized: bool },
    /// Move its top-left corner to (`x`, `y`) on `display` (default: its current one); only from a host claiming
    /// `window-geometry`.
    Move {
        window: Uuid,
        x: i64,
        y: i64,
        display: Option<i64>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(width: i64, height: i64) -> WindowSize {
        WindowSize { width, height }
    }

    #[test]
    fn a_size_in_range_is_kept() {
        assert_eq!(
            clamp_size(size(900, 600), MIN_WINDOW_SIZE, Some(size(1920, 1080))),
            size(900, 600)
        );
        assert_eq!(clamp_size(size(900, 600), MIN_WINDOW_SIZE, None), size(900, 600));
    }

    #[test]
    fn an_oversized_request_shrinks_to_the_display_and_an_undersized_one_grows_to_the_minimum() {
        assert_eq!(
            clamp_size(size(5000, 100), MIN_WINDOW_SIZE, Some(size(1920, 1080))),
            size(1920, 400)
        );
        assert_eq!(clamp_size(size(1, 5000), MIN_WINDOW_SIZE, None), size(640, 5000));
    }

    #[test]
    fn a_display_smaller_than_the_minimum_bounds_the_size() {
        assert_eq!(
            clamp_size(size(900, 600), MIN_WINDOW_SIZE, Some(size(320, 240))),
            size(320, 240)
        );
    }
}
