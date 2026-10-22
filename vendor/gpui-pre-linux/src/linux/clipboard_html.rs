//! An HTML representation of a clipboard write (an ngterm patch): GPUI's `ClipboardItem` carries text and images
//! only, while a terminal copies a selection as `text/html` too (Ghostty's `copy_to_clipboard:html` and `:mixed`).
//!
//! [`with_clipboard_html`] installs the HTML for the clipboard writes its closure makes on this thread (the platform's
//! `write_to_clipboard` / `write_to_primary` take it): the Wayland data source and the X11 selection owner then offer
//! `text/html` beside the item's text, or instead of it. Nothing changes for a write outside the closure.

use std::cell::RefCell;

/// The MIME type (and X11 target) the HTML is offered under.
pub const HTML_MIME_TYPE: &str = "text/html";

/// The HTML a clipboard write offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardHtml {
    /// The `text/html` data.
    pub html: String,
    /// Whether the item's text is offered too (`false`: `text/html` alone).
    pub with_text: bool,
}

thread_local! {
    static PENDING: RefCell<Option<ClipboardHtml>> = const { RefCell::new(None) };
}

/// Runs `write` (a `write_to_clipboard` or `write_to_primary`) with `html` offered beside, or instead of, its text.
pub fn with_clipboard_html<R>(html: ClipboardHtml, write: impl FnOnce() -> R) -> R {
    let previous = PENDING.replace(Some(html));
    let result = write();
    PENDING.set(previous);
    result
}

/// The HTML the clipboard write being made offers, taken once.
#[cfg_attr(not(any(feature = "wayland", feature = "x11")), allow(dead_code))]
pub(crate) fn take_clipboard_html() -> Option<ClipboardHtml> {
    PENDING.take()
}
