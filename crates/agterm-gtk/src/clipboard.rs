//! libghostty's clipboard requests served from GDK's clipboards, after upstream's `GhosttyCallbacks.swift`.
//!
//! - A text read always completes, with an empty `text/plain` when there is nothing: an `UNAVAILABLE` answer never
//!   starts the request, so an OSC 52 reader would wait for a reply that never comes. GDK reads are asynchronous, so
//!   the request completes from the read's callback, and only if its surface still exists.
//! - A paste never prompts: libghostty's unsafe-paste confirmation completes as confirmed.
//! - A program's own access (OSC 52, Kitty) under an `ask` policy is denied until the prompt arrives (Task 15), through
//!   the protocol's refusal: completing it unconfirmed would make libghostty ask again forever.
//! - A write keeps every representation: the callback is void and libghostty reports success right after it.

use agterm_ghostty::{
    ClipboardConfirm, ClipboardContent, ClipboardKind, ClipboardRead, ClipboardReadResult, PendingRequest,
};
use gtk::prelude::*;
use gtk::{gdk, glib};

use crate::surface;

const TEXT: &str = "text/plain";
/// GDK's text type; offered first because some readers take the first match and garble UTF-8 under bare text/plain.
const TEXT_UTF8: &str = "text/plain;charset=utf-8";

/// GDK's clipboard for a libghostty one. The selection clipboard is the primary selection on Linux.
fn gdk_clipboard(kind: ClipboardKind) -> Option<gdk::Clipboard> {
    let display = gdk::Display::default()?;
    Some(match kind {
        ClipboardKind::Standard => display.clipboard(),
        ClipboardKind::Selection | ClipboardKind::Primary => display.primary_clipboard(),
    })
}

fn wants_text(read: &ClipboardRead) -> bool {
    read.mimes.iter().any(|mime| mime == TEXT)
}

/// Whether `read` starts: it must want text or a listing (only text is served), from a live surface.
pub fn read_result(read: &ClipboardRead) -> ClipboardReadResult {
    if surface::lookup(read.surface).is_none() || gdk_clipboard(read.clipboard).is_none() {
        ClipboardReadResult::Unsupported
    } else if wants_text(read) || read.list {
        ClipboardReadResult::Started
    } else {
        ClipboardReadResult::Unavailable
    }
}

/// What completes a read: the text when text was asked for (empty when there is none), and the listing.
fn read_completion(read: &ClipboardRead, text: Option<&str>) -> (Vec<ClipboardContent>, Vec<&'static str>) {
    let contents = if wants_text(read) {
        vec![ClipboardContent {
            mime: TEXT.to_owned(),
            data: text.unwrap_or_default().as_bytes().to_vec(),
        }]
    } else {
        Vec::new()
    };
    let available = if read.list && text.is_some() {
        vec![TEXT]
    } else {
        Vec::new()
    };
    (contents, available)
}

/// Read the clipboard and complete `pending` when the read lands.
pub fn start_read(read: ClipboardRead, pending: PendingRequest) {
    let Some(clipboard) = gdk_clipboard(read.clipboard) else {
        complete_read(&read, pending, None);
        return;
    };
    clipboard.read_text_async(None::<&gtk::gio::Cancellable>, move |result| {
        let text = match result {
            Ok(text) => text.map(String::from),
            Err(err) => {
                tracing::debug!(%err, "clipboard read failed; answering empty");
                None
            }
        };
        complete_read(&read, pending, text.as_deref());
    });
}

fn complete_read(read: &ClipboardRead, pending: PendingRequest, text: Option<&str>) {
    let Some(surface) = surface::lookup(read.surface) else {
        // the surface is gone, and freeing it discarded the request
        return;
    };
    let (contents, available) = read_completion(read, text);
    surface.complete_clipboard_request(pending, &contents, &available, false);
}

/// Answer a request libghostty held back: a paste or a listing completes, a program's access is denied for now.
pub fn confirm(confirm: &ClipboardConfirm, pending: PendingRequest) {
    let Some(surface) = surface::lookup(confirm.surface) else {
        return;
    };
    if confirm.request.is_protocol() {
        tracing::info!(request = ?confirm.request, "clipboard access by a program denied (no prompt yet)");
        surface.deny_clipboard_request(pending);
        return;
    }
    let available: Vec<&str> = confirm.available.iter().map(String::as_str).collect();
    surface.complete_clipboard_request(pending, &confirm.contents, &available, true);
}

/// The content provider for a write: one per representation, text offered under both text types.
fn provider(contents: &[ClipboardContent]) -> gdk::ContentProvider {
    let providers: Vec<gdk::ContentProvider> = contents
        .iter()
        .map(|content| {
            let bytes = glib::Bytes::from(&content.data);
            if content.mime == TEXT {
                gdk::ContentProvider::new_union(&[
                    gdk::ContentProvider::for_bytes(TEXT_UTF8, &bytes),
                    gdk::ContentProvider::for_bytes(TEXT, &bytes),
                ])
            } else {
                gdk::ContentProvider::for_bytes(&content.mime, &bytes)
            }
        })
        .collect();
    gdk::ContentProvider::new_union(&providers)
}

/// Write `contents` to `kind`. A write that needs the user's confirmation is dropped until the prompt arrives.
pub fn write(kind: ClipboardKind, contents: &[ClipboardContent], confirm: bool) {
    if confirm {
        tracing::info!("clipboard write by a program dropped (no prompt yet)");
        return;
    }
    let Some(clipboard) = gdk_clipboard(kind) else {
        return;
    };
    if let Err(err) = clipboard.set_content(Some(&provider(contents))) {
        tracing::warn!(%err, "clipboard write failed");
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use agterm_ghostty::SurfaceKey;

    use super::*;

    fn read(mimes: &[&str], list: bool) -> ClipboardRead {
        ClipboardRead {
            surface: SurfaceKey::new(NonZeroUsize::MIN),
            clipboard: ClipboardKind::Standard,
            mimes: mimes.iter().map(|m| (*m).to_owned()).collect(),
            list,
        }
    }

    #[test]
    fn a_text_read_always_answers_with_text() {
        let (contents, available) = read_completion(&read(&[TEXT], false), Some("hi"));
        assert_eq!(
            contents,
            [ClipboardContent {
                mime: TEXT.into(),
                data: b"hi".to_vec()
            }]
        );
        assert!(available.is_empty());
        let (empty, _) = read_completion(&read(&[TEXT], false), None);
        assert_eq!(
            empty,
            [ClipboardContent {
                mime: TEXT.into(),
                data: Vec::new()
            }]
        );
    }

    #[test]
    fn a_listing_names_text_only_when_there_is_some() {
        let (contents, available) = read_completion(&read(&[], true), Some("x"));
        assert!(contents.is_empty());
        assert_eq!(available, [TEXT]);
        let (_, none) = read_completion(&read(&[], true), None);
        assert!(none.is_empty());
    }

    #[test]
    fn only_text_is_served() {
        assert!(wants_text(&read(&["image/png", TEXT], false)));
        assert!(!wants_text(&read(&["image/png"], false)));
    }
}
