//! Clipboard requests between libghostty and the host.
//!
//! libghostty asks the host to read a clipboard (a paste, an OSC 52 or Kitty read), to confirm a request it will not
//! complete on its own (an unsafe paste, a protocol read or write under an `ask` policy) and to write one. A request
//! that stays open is a [`PendingRequest`], which only [`crate::Surface::complete_clipboard_request`] or
//! [`crate::Surface::deny_clipboard_request`] on the surface it came from can close.

use std::ffi::{c_char, c_void};
use std::ptr::NonNull;

use agterm_ghostty_sys as sys;

use crate::action::{SurfaceKey, owned};

/// Which clipboard a request names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClipboardKind {
    /// The regular clipboard.
    Standard,
    /// The selection clipboard (copy-on-select): X11's and Wayland's primary selection on Linux.
    Selection,
    /// ghostty's distinct primary clipboard, which no embedder serves.
    Primary,
}

impl ClipboardKind {
    pub(crate) fn from_raw(raw: sys::ghostty_clipboard_e) -> Self {
        match raw {
            sys::GHOSTTY_CLIPBOARD_SELECTION => Self::Selection,
            sys::GHOSTTY_CLIPBOARD_PRIMARY => Self::Primary,
            _ => Self::Standard,
        }
    }
}

/// What asked for the clipboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClipboardRequestKind {
    /// A paste binding.
    Paste,
    Osc52Read,
    Osc52Write,
    KittyRead,
    KittyWrite,
    /// A mode 5522 paste event listing the available types.
    List,
}

impl ClipboardRequestKind {
    pub(crate) fn from_raw(raw: sys::ghostty_clipboard_request_e) -> Self {
        match raw {
            sys::GHOSTTY_CLIPBOARD_REQUEST_OSC_52_READ => Self::Osc52Read,
            sys::GHOSTTY_CLIPBOARD_REQUEST_OSC_52_WRITE => Self::Osc52Write,
            sys::GHOSTTY_CLIPBOARD_REQUEST_KITTY_READ => Self::KittyRead,
            sys::GHOSTTY_CLIPBOARD_REQUEST_KITTY_WRITE => Self::KittyWrite,
            sys::GHOSTTY_CLIPBOARD_REQUEST_LIST => Self::List,
            _ => Self::Paste,
        }
    }

    /// A program's own clipboard access (OSC 52, the Kitty protocol), as opposed to the user's paste.
    #[must_use]
    pub fn is_protocol(self) -> bool {
        !matches!(self, Self::Paste | Self::List)
    }
}

/// One representation of clipboard contents. Binary-safe: `data` need not be text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardContent {
    pub mime: String,
    pub data: Vec<u8>,
}

/// How the host answers a read request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClipboardReadResult {
    /// The read starts; the host completes it later through the [`PendingRequest`] it is handed.
    Started,
    /// Nothing the request wants is available; libghostty drops the request.
    Unavailable,
    /// The host cannot serve this clipboard at all.
    Unsupported,
}

impl ClipboardReadResult {
    pub(crate) fn raw(self) -> sys::ghostty_clipboard_read_result_e {
        match self {
            Self::Started => sys::GHOSTTY_CLIPBOARD_READ_STARTED,
            Self::Unavailable => sys::GHOSTTY_CLIPBOARD_READ_UNAVAILABLE,
            Self::Unsupported => sys::GHOSTTY_CLIPBOARD_READ_UNSUPPORTED,
        }
    }
}

/// A read libghostty asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardRead {
    pub surface: SurfaceKey,
    pub clipboard: ClipboardKind,
    /// The representations wanted; text requests ask for `text/plain`.
    pub mimes: Vec<String>,
    /// Also answer which types are available.
    pub list: bool,
}

/// A request libghostty will not complete without the host's confirmation, with what it would deliver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardConfirm {
    pub surface: SurfaceKey,
    pub request: ClipboardRequestKind,
    pub contents: Vec<ClipboardContent>,
    pub available: Vec<String>,
    /// The Kitty protocol's requester name, if it gave one.
    pub name: Option<String>,
    /// Whether the decision may be remembered for the session (a Kitty request with a password).
    pub can_remember: bool,
}

/// An open request libghostty keeps alive until the host completes or denies it. Dropping one leaks libghostty's
/// small request record; it is never freed twice.
#[derive(Debug)]
pub struct PendingRequest {
    pub(crate) surface: SurfaceKey,
    pub(crate) state: NonNull<c_void>,
}

impl PendingRequest {
    /// The surface the request belongs to.
    #[must_use]
    pub fn surface(&self) -> SurfaceKey {
        self.surface
    }
}

/// Copies `len` representations out of a C array.
///
/// # Safety
///
/// `items` is null or valid for `len` entries whose `mime` is null or NUL-terminated and whose `data` is null or
/// valid for `len` bytes, all for the duration of the call.
pub(crate) unsafe fn copy_contents(
    items: *const sys::ghostty_clipboard_content_s,
    len: usize,
) -> Vec<ClipboardContent> {
    if items.is_null() || len == 0 {
        return Vec::new();
    }
    // SAFETY: non-null and valid for len entries per the caller's contract.
    let items = unsafe { std::slice::from_raw_parts(items, len) };
    items
        .iter()
        .filter_map(|item| {
            // SAFETY: null or NUL-terminated per the caller's contract.
            let mime = unsafe { owned(item.mime) }?;
            let data = if item.data.is_null() || item.len == 0 {
                Vec::new()
            } else {
                // SAFETY: non-null and valid for item.len bytes per the caller's contract.
                unsafe { std::slice::from_raw_parts(item.data.cast::<u8>(), item.len) }.to_vec()
            };
            Some(ClipboardContent { mime, data })
        })
        .collect()
}

/// Copies `len` strings out of a C array, skipping nulls.
///
/// # Safety
///
/// `items` is null or valid for `len` entries, each null or NUL-terminated, for the duration of the call.
pub(crate) unsafe fn copy_strings(items: *const *const c_char, len: usize) -> Vec<String> {
    if items.is_null() || len == 0 {
        return Vec::new();
    }
    // SAFETY: non-null and valid for len entries per the caller's contract.
    let items = unsafe { std::slice::from_raw_parts(items, len) };
    items
        .iter()
        // SAFETY: each entry null or NUL-terminated per the caller's contract.
        .filter_map(|item| unsafe { owned(*item) })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::ffi::CString;

    use super::*;

    #[test]
    fn request_kinds_tell_protocol_access_from_a_paste() {
        assert!(!ClipboardRequestKind::Paste.is_protocol());
        assert!(!ClipboardRequestKind::List.is_protocol());
        for raw in [
            sys::GHOSTTY_CLIPBOARD_REQUEST_OSC_52_READ,
            sys::GHOSTTY_CLIPBOARD_REQUEST_OSC_52_WRITE,
            sys::GHOSTTY_CLIPBOARD_REQUEST_KITTY_READ,
            sys::GHOSTTY_CLIPBOARD_REQUEST_KITTY_WRITE,
        ] {
            assert!(ClipboardRequestKind::from_raw(raw).is_protocol(), "{raw}");
        }
        assert_eq!(
            ClipboardRequestKind::from_raw(sys::GHOSTTY_CLIPBOARD_REQUEST_PASTE),
            ClipboardRequestKind::Paste
        );
    }

    #[test]
    fn clipboard_kinds_decode() {
        assert_eq!(
            ClipboardKind::from_raw(sys::GHOSTTY_CLIPBOARD_STANDARD),
            ClipboardKind::Standard
        );
        assert_eq!(
            ClipboardKind::from_raw(sys::GHOSTTY_CLIPBOARD_SELECTION),
            ClipboardKind::Selection
        );
        assert_eq!(
            ClipboardKind::from_raw(sys::GHOSTTY_CLIPBOARD_PRIMARY),
            ClipboardKind::Primary
        );
    }

    #[test]
    fn contents_are_copied_binary_safe() {
        let mime = CString::new("image/png").expect("no NUL");
        let data = [0_u8, 159, 146, 150];
        let items = [sys::ghostty_clipboard_content_s {
            mime: mime.as_ptr(),
            data: data.as_ptr().cast(),
            len: data.len(),
        }];
        // SAFETY: one valid entry, alive across the call.
        let copied = unsafe { copy_contents(items.as_ptr(), items.len()) };
        assert_eq!(
            copied,
            [ClipboardContent {
                mime: "image/png".into(),
                data: data.to_vec()
            }]
        );
        // SAFETY: a null array is allowed.
        assert!(unsafe { copy_contents(std::ptr::null(), 3) }.is_empty());
    }

    #[test]
    fn strings_skip_nulls() {
        let text = CString::new("text/plain").expect("no NUL");
        let items = [text.as_ptr(), std::ptr::null()];
        // SAFETY: two valid entries, alive across the call.
        assert_eq!(unsafe { copy_strings(items.as_ptr(), items.len()) }, ["text/plain"]);
    }
}
