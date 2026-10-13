//! libghostty's clipboard requests served from GDK's clipboards, after upstream's `GhosttyCallbacks.swift`, and the
//! prompt that gates a program's own clipboard access, after upstream's `ClipboardPromptController.swift`.
//!
//! - A text read always completes, with an empty `text/plain` when there is nothing: an `UNAVAILABLE` answer never
//!   starts the request, so an OSC 52 reader would wait for a reply that never comes. GDK reads are asynchronous, so
//!   the request completes from the read's callback, and only if its surface still exists.
//! - A paste never prompts: libghostty's unsafe-paste confirmation completes as confirmed.
//! - A program's own access (OSC 52, Kitty) that libghostty holds for confirmation (an `ask` policy) goes to the
//!   prompt: allowed, it completes as confirmed; denied, through the protocol's refusal (completing it unconfirmed
//!   would make libghostty ask again forever).
//! - A write keeps every representation: the callback is void and libghostty reports success right after it. One that
//!   needs confirmation is written once the prompt allows it.
//!
//! The prompt: one app-run [`ClipboardPromptPolicy`] remembers a "don't ask again this session" choice per direction
//! across every window and terminal. Without one, an `adw::AlertDialog` asks over the requesting terminal's window.
//! Requests from the same terminal for the same direction wait behind its one open prompt, so a program looping OSC
//! 52 cannot stack dialogs, while another terminal always gets its own, so one Allow never authorizes another
//! terminal's access. The prompt is shown off the libghostty callback, after upstream's main-queue hop.

use std::cell::RefCell;
use std::collections::HashMap;

use adw::prelude::*;
use agterm_core::clipboard_policy::{ClipboardAccess, ClipboardDecision, ClipboardPromptPolicy};
use agterm_ghostty::{
    ClipboardConfirm, ClipboardContent, ClipboardKind, ClipboardRead, ClipboardReadResult, ClipboardRequestKind,
    PendingRequest, SurfaceKey,
};
use gtk::{gdk, gio, glib};

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
    clipboard.read_text_async(None::<&gio::Cancellable>, move |result| {
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
        // the surface is gone: the request cannot be completed and its record leaks (see PendingRequest)
        return;
    };
    let (contents, available) = read_completion(read, text);
    surface.complete_clipboard_request(pending, &contents, &available, false);
}

/// The direction of a program's own clipboard access; `None` for the user's paste or a listing.
fn access(request: ClipboardRequestKind) -> Option<ClipboardAccess> {
    match request {
        ClipboardRequestKind::Osc52Read | ClipboardRequestKind::KittyRead => Some(ClipboardAccess::Read),
        ClipboardRequestKind::Osc52Write | ClipboardRequestKind::KittyWrite => Some(ClipboardAccess::Write),
        ClipboardRequestKind::Paste | ClipboardRequestKind::List => None,
    }
}

/// Answer a request libghostty held back: a paste or a listing completes, a program's access goes to the prompt.
pub fn confirm(confirm: ClipboardConfirm, pending: PendingRequest) {
    let Some(surface) = surface::lookup(confirm.surface) else {
        return;
    };
    let Some(access) = access(confirm.request) else {
        let available: Vec<&str> = confirm.available.iter().map(String::as_str).collect();
        surface.complete_clipboard_request(pending, &confirm.contents, &available, true);
        return;
    };
    let key = confirm.surface;
    glib::idle_add_local_once(move || {
        request(access, Some(key), move |allowed| {
            finish_confirmation(&confirm, pending, allowed);
        });
    });
}

/// Completes or denies a held request once the prompt decided. A surface closed meanwhile freed the request with it,
/// so there is nothing left to answer.
fn finish_confirmation(confirm: &ClipboardConfirm, pending: PendingRequest, allowed: bool) {
    let Some(surface) = surface::lookup(confirm.surface) else {
        return;
    };
    if allowed {
        let available: Vec<&str> = confirm.available.iter().map(String::as_str).collect();
        surface.complete_clipboard_request(pending, &confirm.contents, &available, true);
    } else {
        surface.deny_clipboard_request(pending);
    }
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

/// Write `contents` to `kind`: at once (synchronously, so a read right after sees it) unless the write needs the
/// user's confirmation, then once the prompt for the requesting surface allows it. A confirmation with no surface to
/// ask for is dropped, as upstream.
pub fn write(surface: Option<SurfaceKey>, kind: ClipboardKind, contents: Vec<ClipboardContent>, confirm: bool) {
    if contents.is_empty() {
        return;
    }
    if !confirm {
        set_clipboard(kind, &contents);
        return;
    }
    let Some(key) = surface else {
        return;
    };
    glib::idle_add_local_once(move || {
        request(ClipboardAccess::Write, Some(key), move |allowed| {
            if allowed {
                set_clipboard(kind, &contents);
            }
        });
    });
}

fn set_clipboard(kind: ClipboardKind, contents: &[ClipboardContent]) {
    let Some(clipboard) = gdk_clipboard(kind) else {
        return;
    };
    if let Err(err) = clipboard.set_content(Some(&provider(contents))) {
        tracing::warn!(%err, "clipboard write failed");
    }
}

/// One open prompt per direction and requesting surface; a request with no surface coalesces by direction alone.
/// Surface keys are never reused, so a prompt cannot answer for a surface created after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct PromptKey {
    access: ClipboardAccess,
    requester: Option<SurfaceKey>,
}

type Waiter = Box<dyn FnOnce(bool)>;

/// An open prompt: its id (so a late signal of an earlier dialog answers nothing) and who waits on it.
struct OpenPrompt {
    id: u64,
    waiters: Vec<Waiter>,
}

/// What became of a request.
enum Admission {
    /// A remembered choice answers it now: call the waiter with it.
    Decided(Waiter, bool),
    /// A new prompt with this id must be shown.
    Prompt(u64),
    /// It waits behind the open prompt for its key.
    Queued,
}

/// The session's remembered choices and the open prompts. Waiters are handed back, never called inside, so no
/// borrow of the prompts is held while they run.
#[derive(Default)]
struct Prompts {
    policy: ClipboardPromptPolicy,
    open: HashMap<PromptKey, OpenPrompt>,
    next_id: u64,
}

impl Prompts {
    fn admit(&mut self, key: PromptKey, waiter: Waiter) -> Admission {
        match self.policy.decision(key.access) {
            ClipboardDecision::Allow => return Admission::Decided(waiter, true),
            ClipboardDecision::Deny => return Admission::Decided(waiter, false),
            ClipboardDecision::Prompt => {}
        }
        if let Some(open) = self.open.get_mut(&key) {
            open.waiters.push(waiter);
            return Admission::Queued;
        }
        self.next_id += 1;
        let id = self.next_id;
        self.open.insert(
            key,
            OpenPrompt {
                id,
                waiters: vec![waiter],
            },
        );
        Admission::Prompt(id)
    }

    /// The user answered prompt `id` of `key`: remembers the choice when asked to and hands back its waiters. A prompt
    /// already answered hands back none.
    fn answer(&mut self, key: PromptKey, id: u64, allowed: bool, remember: bool) -> Vec<Waiter> {
        if self.open.get(&key).is_none_or(|open| open.id != id) {
            return Vec::new();
        }
        if remember {
            self.policy.remember(key.access, allowed);
        }
        self.open.remove(&key).map(|open| open.waiters).unwrap_or_default()
    }
}

thread_local! {
    static PROMPTS: RefCell<Prompts> = RefCell::new(Prompts::default());
}

/// Resolves a program's clipboard access: at once with a remembered session choice, else after the prompt for
/// `requester` (upstream's `ClipboardPromptController.request`). `completion` runs on the main thread with the
/// decision.
fn request(access: ClipboardAccess, requester: Option<SurfaceKey>, completion: impl FnOnce(bool) + 'static) {
    let key = PromptKey { access, requester };
    let admission = PROMPTS.with(|prompts| prompts.borrow_mut().admit(key, Box::new(completion)));
    match admission {
        Admission::Decided(waiter, allowed) => waiter(allowed),
        Admission::Prompt(id) => present_prompt(key, id),
        Admission::Queued => {}
    }
}

fn answer(key: PromptKey, id: u64, allowed: bool, remember: bool) {
    let waiters = PROMPTS.with(|prompts| prompts.borrow_mut().answer(key, id, allowed, remember));
    for waiter in waiters {
        waiter(allowed);
    }
}

const ALLOW: &str = "allow";
const DENY: &str = "deny";

/// Upstream's heading and message for a direction.
fn prompt_text(access: ClipboardAccess) -> (&'static str, &'static str) {
    match access {
        ClipboardAccess::Read => (
            "Allow a program to read the clipboard?",
            "A program running in the terminal is trying to read your clipboard contents, which could expose \
             passwords or other copied data.",
        ),
        ClipboardAccess::Write => (
            "Allow a program to set the clipboard?",
            "A program running in the terminal is trying to replace your clipboard contents.",
        ),
    }
}

/// Shows the prompt over the requesting terminal's window, else the active one: Allow (the default, as upstream's
/// first button), Deny (also closing the dialog) and "Don't ask again this session".
fn present_prompt(key: PromptKey, id: u64) {
    let (heading, body) = prompt_text(key.access);
    let dialog = adw::AlertDialog::new(Some(heading), Some(body));
    dialog.add_responses(&[(DENY, "Deny"), (ALLOW, "Allow")]);
    dialog.set_response_appearance(ALLOW, adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some(ALLOW));
    dialog.set_close_response(DENY);
    dialog.set_extra_child(Some(&gtk::CheckButton::with_label("Don't ask again this session")));
    dialog.connect_response(None, move |dialog, response| {
        let remember = dialog
            .extra_child()
            .and_downcast::<gtk::CheckButton>()
            .is_some_and(|check| check.is_active());
        answer(key, id, response == ALLOW, remember);
    });
    // a dialog closed without a response (its window went away) denies, so its waiters are not stranded; after a
    // response the prompt is answered already and this releases nothing
    dialog.connect_closed(move |_| answer(key, id, false, false));
    dialog.present(prompt_parent(key.requester).as_ref());
}

/// The widget a prompt attaches to: the requesting terminal while it is in a window, else the active window.
fn prompt_parent(requester: Option<SurfaceKey>) -> Option<gtk::Widget> {
    let terminal = requester
        .and_then(surface::lookup)
        .map(|terminal| terminal.widget().clone().upcast::<gtk::Widget>())
        .filter(|widget| widget.root().is_some());
    terminal.or_else(|| {
        gio::Application::default()
            .and_downcast::<gtk::Application>()
            .and_then(|app| app.active_window())
            .map(Cast::upcast)
    })
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;
    use std::rc::Rc;

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

    #[test]
    fn only_a_programs_own_access_is_gated() {
        assert_eq!(access(ClipboardRequestKind::Osc52Read), Some(ClipboardAccess::Read));
        assert_eq!(access(ClipboardRequestKind::KittyRead), Some(ClipboardAccess::Read));
        assert_eq!(access(ClipboardRequestKind::Osc52Write), Some(ClipboardAccess::Write));
        assert_eq!(access(ClipboardRequestKind::KittyWrite), Some(ClipboardAccess::Write));
        assert_eq!(access(ClipboardRequestKind::Paste), None);
        assert_eq!(access(ClipboardRequestKind::List), None);
    }

    fn key(access: ClipboardAccess, surface: usize) -> PromptKey {
        PromptKey {
            access,
            requester: NonZeroUsize::new(surface).map(SurfaceKey::new),
        }
    }

    /// A waiter that records its answer.
    fn recorder(log: &Rc<RefCell<Vec<(&'static str, bool)>>>, name: &'static str) -> Waiter {
        let log = Rc::clone(log);
        Box::new(move |allowed| log.borrow_mut().push((name, allowed)))
    }

    #[test]
    fn requests_of_one_surface_and_direction_wait_behind_one_prompt() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut prompts = Prompts::default();
        let read = key(ClipboardAccess::Read, 1);
        let Admission::Prompt(id) = prompts.admit(read, recorder(&log, "first")) else {
            panic!("the first request prompts");
        };
        assert!(matches!(
            prompts.admit(read, recorder(&log, "second")),
            Admission::Queued
        ));
        // another surface, or the other direction, gets a prompt of its own
        assert!(matches!(
            prompts.admit(key(ClipboardAccess::Read, 2), recorder(&log, "other surface")),
            Admission::Prompt(_)
        ));
        assert!(matches!(
            prompts.admit(key(ClipboardAccess::Write, 1), recorder(&log, "write")),
            Admission::Prompt(_)
        ));
        for waiter in prompts.answer(read, id, true, false) {
            waiter(true);
        }
        assert_eq!(*log.borrow(), [("first", true), ("second", true)]);
        // not remembered: the next request prompts again
        assert!(matches!(
            prompts.admit(read, recorder(&log, "third")),
            Admission::Prompt(_)
        ));
    }

    #[test]
    fn a_remembered_choice_answers_without_a_prompt() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut prompts = Prompts::default();
        let write = key(ClipboardAccess::Write, 1);
        let Admission::Prompt(id) = prompts.admit(write, recorder(&log, "asked")) else {
            panic!("the first request prompts");
        };
        assert_eq!(prompts.answer(write, id, false, true).len(), 1);
        let Admission::Decided(waiter, allowed) = prompts.admit(key(ClipboardAccess::Write, 7), recorder(&log, "x"))
        else {
            panic!("a remembered deny decides");
        };
        assert!(!allowed);
        waiter(allowed);
        assert_eq!(*log.borrow(), [("x", false)]);
        // the other direction still prompts
        assert!(matches!(
            prompts.admit(key(ClipboardAccess::Read, 1), recorder(&log, "read")),
            Admission::Prompt(_)
        ));
    }

    #[test]
    fn a_stale_answer_releases_nothing() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut prompts = Prompts::default();
        let read = key(ClipboardAccess::Read, 0);
        let Admission::Prompt(id) = prompts.admit(read, recorder(&log, "first")) else {
            panic!("the first request prompts");
        };
        assert_eq!(prompts.answer(read, id, true, false).len(), 1);
        let Admission::Prompt(next) = prompts.admit(read, recorder(&log, "second")) else {
            panic!("a new prompt after the answer");
        };
        assert!(
            prompts.answer(read, id, false, true).is_empty(),
            "the old dialog's late close"
        );
        assert_eq!(
            prompts.policy.decision(ClipboardAccess::Read),
            ClipboardDecision::Prompt
        );
        assert_eq!(prompts.answer(read, next, true, false).len(), 1);
    }

    #[test]
    fn the_prompt_says_what_upstream_says() {
        assert_eq!(
            prompt_text(ClipboardAccess::Read),
            (
                "Allow a program to read the clipboard?",
                "A program running in the terminal is trying to read your clipboard contents, which could expose \
                 passwords or other copied data."
            )
        );
        assert_eq!(
            prompt_text(ClipboardAccess::Write).0,
            "Allow a program to set the clipboard?"
        );
    }
}
