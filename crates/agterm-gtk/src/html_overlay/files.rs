//! Files never reach a page's script: a dropped or pasted file gives the script its contents, so a drag or paste that
//! carries files is refused while text and links still go through; in a terminal the same paste gives only a path
//! (upstream's `carriesFiles` refusals in `HtmlOverlayView`).
//!
//! GTK names a drag's or the clipboard's formats, not their contents. The file portal's types and a file manager's
//! copied-files type always carry files; a `text/uri-list`, which a file manager outside a sandbox offers alone and
//! from which `WebKitGTK` makes the page's files, is read first and goes through only when it lists no local file.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use webkit6::prelude::*;
use webkit6::{EDITING_COMMAND_PASTE, EDITING_COMMAND_PASTE_AS_PLAIN_TEXT, WebView};

/// Types that always carry files: the file portal's, and the one a file manager's copy offers.
const FILE_TYPES: [&str; 3] = [
    "application/vnd.portal.filetransfer",
    "application/vnd.portal.files",
    "x-special/gnome-copied-files",
];
const URI_LIST: &str = "text/uri-list";
/// A longer address list is not read to the end: it is taken to carry files.
const URI_LIST_LIMIT: usize = 1 << 20;

/// What an offer's formats say about files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FileOffer {
    /// Nothing that can carry a file.
    None,
    /// Files for sure.
    Files,
    /// An address list, which carries files when it lists a local one.
    UriList,
}

/// What `formats` say about files; an in-process offer of files names their type.
pub(super) fn file_offer(formats: &gdk::ContentFormats) -> FileOffer {
    if formats.contains_type(gdk::FileList::static_type())
        || formats.contains_type(gio::File::static_type())
        || FILE_TYPES.iter().any(|mime| formats.contain_mime_type(mime))
    {
        FileOffer::Files
    } else if formats.contain_mime_type(URI_LIST) {
        FileOffer::UriList
    } else {
        FileOffer::None
    }
}

/// Whether a `text/uri-list` lists a local file: a `file:` address, or an entry that is no address at all (a bare
/// path), which is refused rather than guessed at. Comment lines and blank lines list nothing.
pub(super) fn uri_list_carries_files(list: &str) -> bool {
    list.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .any(|line| scheme(line).is_none_or(|scheme| scheme.eq_ignore_ascii_case("file")))
}

/// The scheme an address starts with (RFC 3986: a letter, then letters, digits, `+`, `-` or `.`, then a colon).
fn scheme(address: &str) -> Option<&str> {
    let (scheme, _) = address.split_once(':')?;
    let mut chars = scheme.chars();
    let valid = chars.next().is_some_and(|first| first.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    valid.then_some(scheme)
}

/// Whether the address list `stream` gives carries files; one that cannot be read whole, or is too long, does.
async fn read_carries_files(stream: Result<(gio::InputStream, glib::GString), glib::Error>) -> bool {
    let Ok((stream, _)) = stream else { return true };
    let mut bytes = Vec::new();
    loop {
        let Ok(chunk) = stream.read_bytes_future(64 * 1024, glib::Priority::DEFAULT).await else {
            return true;
        };
        if chunk.is_empty() {
            break;
        }
        bytes.extend_from_slice(&chunk);
        if bytes.len() > URI_LIST_LIMIT {
            return true;
        }
    }
    String::from_utf8(bytes).map_or(true, |list| uri_list_carries_files(&list))
}

// MARK: - drops

/// The drag being looked at and, once its address list is read, whether it carries files.
type DragVerdict = Rc<RefCell<Option<(gdk::Drop, Option<bool>)>>>;

/// Refuses a drag carrying files anywhere on `root`, ahead of the web view's own drop target (upstream's
/// `draggingEntered` and `performDragOperation`). A drag offering an address list is read while it moves; its drop
/// goes on to the page only once the list is known to hold no local file.
pub(super) fn refuse_file_drops(root: &gtk::Box) {
    let target = gtk::DropTargetAsync::new(None, gdk::DragAction::all());
    target.set_propagation_phase(gtk::PropagationPhase::Capture);
    let verdict = DragVerdict::default();
    let seen = Rc::clone(&verdict);
    target.connect_accept(move |_, drop| match file_offer(&drop.formats()) {
        FileOffer::None => {
            seen.borrow_mut().take();
            false
        }
        FileOffer::Files => {
            seen.borrow_mut().take();
            true
        }
        FileOffer::UriList => {
            *seen.borrow_mut() = Some((drop.clone(), None));
            read_drag(drop.clone(), Rc::clone(&seen));
            true
        }
    });
    target.connect_drag_enter(|_, _, _, _| gdk::DragAction::empty());
    target.connect_drag_motion(|_, _, _, _| gdk::DragAction::empty());
    let left = Rc::clone(&verdict);
    target.connect_drag_leave(move |_, _| {
        left.borrow_mut().take();
    });
    target.connect_drop(move |_, drop, _, _| {
        let links_only = matches!(verdict.borrow_mut().take(), Some((seen, Some(false))) if &seen == drop);
        if links_only {
            // not handled: the drop goes on to the web view's own target
            return false;
        }
        drop.finish(gdk::DragAction::empty());
        true
    });
    root.add_controller(target);
}

/// Reads `drop`'s address list and records whether it carries files, unless another drag took its place meanwhile.
fn read_drag(drop: gdk::Drop, verdict: DragVerdict) {
    glib::spawn_future_local(async move {
        let carries = read_carries_files(drop.read_future(&[URI_LIST], glib::Priority::DEFAULT).await).await;
        if let Some((seen, known)) = verdict.borrow_mut().as_mut()
            && *seen == drop
        {
            *known = Some(carries);
        }
    });
}

// MARK: - pastes

/// Refuses a paste carrying files into `web_view` (upstream's `paste:`, `pasteAsPlainText:` and
/// `readSelectionFromPasteboard:`): the paste keys over a clipboard of files beep, and over an address list wait for
/// it to be read, then paste only when it lists no local file; a middle click over a primary selection that may carry
/// files beeps. Added after the page's input reporter, so a refused key still counts as user activity.
pub(super) fn refuse_file_pastes(web_view: &WebView) {
    let input = gtk::EventControllerLegacy::new();
    input.set_propagation_phase(gtk::PropagationPhase::Capture);
    let view = web_view.downgrade();
    input.connect_event(move |_, event| match view.upgrade() {
        Some(web_view) => refuse_paste(&web_view, event),
        None => glib::Propagation::Proceed,
    });
    web_view.add_controller(input);
}

fn refuse_paste(web_view: &WebView, event: &gdk::Event) -> glib::Propagation {
    if let Some(key) = event.downcast_ref::<gdk::KeyEvent>() {
        if event.event_type() != gdk::EventType::KeyPress {
            return glib::Propagation::Proceed;
        }
        return match paste_command(key) {
            Some(command) => paste(web_view, command),
            None => glib::Propagation::Proceed,
        };
    }
    let middle = event
        .downcast_ref::<gdk::ButtonEvent>()
        .is_some_and(|button| button.button() == gdk::BUTTON_MIDDLE);
    if middle && file_offer(&web_view.primary_clipboard().formats()) != FileOffer::None {
        if event.event_type() == gdk::EventType::ButtonPress {
            web_view.error_bell();
        }
        return glib::Propagation::Stop;
    }
    glib::Propagation::Proceed
}

/// The editing command a key pastes with: Ctrl+V and Shift+Insert paste, Ctrl+Shift+V pastes as plain text. A
/// partial match counts, as it does for GTK's shortcuts: the same key under a layout without Latin letters.
fn paste_command(key: &gdk::KeyEvent) -> Option<&'static glib::GStr> {
    let control = gdk::ModifierType::CONTROL_MASK;
    let shift = gdk::ModifierType::SHIFT_MASK;
    let matches =
        |keyval: gdk::Key, modifiers: gdk::ModifierType| key.matches(keyval, modifiers) != gdk::KeyMatch::None;
    if matches(gdk::Key::v, control | shift) {
        Some(EDITING_COMMAND_PASTE_AS_PLAIN_TEXT)
    } else if matches(gdk::Key::v, control) || matches(gdk::Key::Insert, shift) {
        Some(EDITING_COMMAND_PASTE)
    } else {
        None
    }
}

/// A paste key: through to the page over a clipboard without files, refused over one of files, and over an address
/// list held until the list is read, then pasted only when it lists no local file.
fn paste(web_view: &WebView, command: &'static glib::GStr) -> glib::Propagation {
    let clipboard = web_view.clipboard();
    match file_offer(&clipboard.formats()) {
        FileOffer::None => glib::Propagation::Proceed,
        FileOffer::Files => {
            web_view.error_bell();
            glib::Propagation::Stop
        }
        FileOffer::UriList => {
            let view = web_view.downgrade();
            glib::spawn_future_local(async move {
                let carries =
                    read_carries_files(clipboard.read_future(&[URI_LIST], glib::Priority::DEFAULT).await).await;
                let Some(web_view) = view.upgrade() else { return };
                if carries {
                    web_view.error_bell();
                } else {
                    web_view.execute_editing_command(command);
                }
            });
            glib::Propagation::Stop
        }
    }
}

/// Whether the context menu's paste items go: the clipboard may carry files, which the menu cannot read first.
pub(super) fn clipboard_may_carry_files(web_view: &WebView) -> bool {
    file_offer(&web_view.clipboard().formats()) != FileOffer::None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_list_carries_files_when_it_lists_a_local_one() {
        assert!(!uri_list_carries_files("https://example.com/a\r\nhttp://b.test/\r\n"));
        assert!(!uri_list_carries_files("# a comment\n\nmailto:a@b.test\n"));
        assert!(uri_list_carries_files(
            "https://example.com/\r\nfile:///home/u/secret.txt\r\n"
        ));
        assert!(uri_list_carries_files("FILE:///etc/passwd"));
        assert!(uri_list_carries_files("/home/u/secret.txt"), "a bare path is refused");
        assert!(uri_list_carries_files("1abc:/x"), "no valid scheme");
        assert!(!uri_list_carries_files(""));
    }

    #[test]
    fn formats_say_whether_an_offer_carries_files() {
        let of = |mimes: &[&str]| file_offer(&gdk::ContentFormats::new(mimes));
        assert_eq!(of(&["text/plain", "text/html"]), FileOffer::None);
        assert_eq!(of(&["text/uri-list", "text/plain"]), FileOffer::UriList);
        assert_eq!(of(&["text/uri-list", "application/vnd.portal.files"]), FileOffer::Files);
        assert_eq!(of(&["application/vnd.portal.filetransfer"]), FileOffer::Files);
        assert_eq!(of(&["x-special/gnome-copied-files", "text/plain"]), FileOffer::Files);
        let local = gdk::ContentFormatsBuilder::new()
            .add_type(gdk::FileList::static_type())
            .build();
        assert_eq!(file_offer(&local), FileOffer::Files);
    }
}
