//! A program's own clipboard access (OSC 52, the Kitty clipboard protocol) under `clipboard-read` / `clipboard-write`
//! and the prompt that gates it (upstream's `ClipboardPromptController.swift`, the GTK build's `clipboard.rs`).
//!
//! - A read is answered from the system clipboard or the primary selection (Ghostty's "selection" clipboard is the
//!   primary on Linux), text only; a listing of the types needs no consent. The pane's reader waits for the answer, so
//!   every read is answered: allowed with the text (none when the clipboard holds none), refused as denied.
//! - A write keeps the text representation (the GTK build served text only too).
//! - `allow` performs, `deny` refuses silently, `ask` (the default for reads) goes to the prompt. One app-run
//!   [`ClipboardPromptPolicy`] remembers a "this session" choice per direction across every terminal. Requests from
//!   the same terminal for the same direction wait behind its one open prompt, so a program looping OSC 52 cannot stack
//!   prompts, while another terminal always gets its own (one Allow never authorizes another terminal's access).
//! - The prompt is upstream's warning alert over the requesting terminal's window ([`crate::modal::open_alert`]):
//!   the heading and message for the direction, Deny (Escape) and Allow (the default, Return), and the "Don't ask
//!   again this session" check, which remembers the answer for the direction. A terminal with no window to ask in is
//!   refused.

use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::{App, ClipboardItem, Context, Global};
use ngterm_core::clipboard_policy::{ClipboardAccess, ClipboardDecision, ClipboardPromptPolicy};
use ngterm_core::ghostty_config::ClipboardAccess as ConfigAccess;
use ngterm_term::{ClipboardAnswer, ClipboardLocation, ClipboardReadRequest, ClipboardWrite, PaneEvent, ReadScope};

use super::{Shared, Terminal};

const TEXT: &str = "text/plain";

/// The prompt's buttons, left to right: Deny, then Allow.
const DENY: &str = "Deny";
const ALLOW: &str = "Allow";
/// The prompt's check, which remembers the answer for the session (upstream's suppression button).
pub const REMEMBER: &str = "Don't ask again this session";

/// The prompt for `access` (upstream's `presentPrompt`).
#[must_use]
pub fn prompt_alert(access: ClipboardAccess) -> crate::modal::AlertSpec {
    let (heading, body) = prompt_text(access);
    crate::modal::AlertSpec {
        heading: heading.to_owned(),
        body: body.to_owned(),
        buttons: vec![
            crate::modal::AlertButton::cancel(DENY),
            crate::modal::AlertButton::plain(ALLOW).as_default().suggested(),
        ],
        checks: vec![crate::modal::AlertCheck::new(REMEMBER)],
        field: None,
    }
}

/// Upstream's heading and message for a direction.
#[must_use]
pub fn prompt_text(access: ClipboardAccess) -> (&'static str, &'static str) {
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

/// One open prompt per direction and requesting terminal (its surface key, never reused).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct PromptKey {
    access: ClipboardAccess,
    terminal: u64,
}

type Waiter = Box<dyn FnOnce(bool, &mut App)>;

struct OpenPrompt {
    id: u64,
    waiters: Vec<Waiter>,
}

/// What became of a request.
enum Admission {
    Decided(Waiter, bool),
    Prompt(u64),
    Queued,
}

/// The session's remembered choices and the open prompts.
#[derive(Default)]
pub struct ClipboardPrompts {
    policy: ClipboardPromptPolicy,
    open: HashMap<PromptKey, OpenPrompt>,
    next_id: u64,
}

impl Global for ClipboardPrompts {}

impl ClipboardPrompts {
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

    /// The user answered prompt `id` of `key`: remembers the choice when asked to and hands back its waiters.
    fn answer(&mut self, key: PromptKey, id: u64, allowed: bool, remember: bool) -> Vec<Waiter> {
        if self.open.get(&key).is_none_or(|open| open.id != id) {
            return Vec::new();
        }
        if remember {
            self.policy.remember(key.access, allowed);
        }
        self.open.remove(&key).map(|open| open.waiters).unwrap_or_default()
    }

    /// The remembered decision for `access` (tests).
    #[must_use]
    pub fn decision(&self, access: ClipboardAccess) -> ClipboardDecision {
        self.policy.decision(access)
    }
}

/// Handles a pane event that is a program's clipboard access; whether it was one.
pub(super) fn handle(shared: &Rc<Shared>, event: &PaneEvent, cx: &mut Context<Terminal>) -> bool {
    match event {
        PaneEvent::ClipboardWrite(write) => {
            on_write(shared, write, cx);
            true
        }
        PaneEvent::ClipboardRead(read) => {
            on_read(shared, *read, cx);
            true
        }
        _ => false,
    }
}

fn read_text(location: ClipboardLocation, cx: &App) -> Option<String> {
    let item = match location {
        ClipboardLocation::Standard => cx.read_from_clipboard(),
        ClipboardLocation::Selection | ClipboardLocation::Primary => cx.read_from_primary(),
    };
    item.and_then(|item| item.text())
}

fn on_read(shared: &Rc<Shared>, read: ClipboardReadRequest, cx: &mut App) {
    let answer = move |shared: &Shared, allowed: bool, cx: &App| {
        let answer = if allowed {
            ClipboardAnswer::Text(read_text(read.location, cx))
        } else {
            ClipboardAnswer::Denied
        };
        if let Some(pane) = shared.pane() {
            pane.answer_clipboard_read(read.id, answer);
        }
    };
    if read.scope == ReadScope::ListOnly {
        answer(shared, true, cx);
        return;
    }
    match shared.config().clipboard.read {
        ConfigAccess::Allow => answer(shared, true, cx),
        ConfigAccess::Deny => answer(shared, false, cx),
        ConfigAccess::Ask => {
            let requester = Rc::clone(shared);
            request(
                ClipboardAccess::Read,
                shared,
                cx,
                Box::new(move |allowed, cx| answer(&requester, allowed, cx)),
            );
        }
    }
}

fn on_write(shared: &Rc<Shared>, write: &ClipboardWrite, cx: &mut App) {
    let Some(text) = write
        .contents
        .iter()
        .find(|content| content.mime == TEXT || content.mime.starts_with("text/plain;"))
        .map(|content| String::from_utf8_lossy(&content.data).into_owned())
    else {
        return;
    };
    let location = write.location;
    let perform = move |allowed: bool, cx: &mut App| {
        if !allowed {
            return;
        }
        let item = ClipboardItem::new_string(text);
        match location {
            ClipboardLocation::Standard => cx.write_to_clipboard(item),
            ClipboardLocation::Selection | ClipboardLocation::Primary => cx.write_to_primary(item),
        }
    };
    match shared.config().clipboard.write {
        ConfigAccess::Allow => perform(true, cx),
        ConfigAccess::Deny => {}
        ConfigAccess::Ask => request(ClipboardAccess::Write, shared, cx, Box::new(perform)),
    }
}

/// Resolves a program's clipboard access: at once with a remembered session choice, else after the prompt.
fn request(access: ClipboardAccess, shared: &Shared, cx: &mut App, waiter: Waiter) {
    let key = PromptKey {
        access,
        terminal: shared.key(),
    };
    let admission = cx.default_global::<ClipboardPrompts>().admit(key, waiter);
    match admission {
        Admission::Decided(waiter, allowed) => waiter(allowed, cx),
        Admission::Prompt(id) => present(key, id, cx),
        Admission::Queued => {}
    }
}

fn answer(key: PromptKey, id: u64, allowed: bool, remember: bool, cx: &mut App) {
    let waiters = cx
        .default_global::<ClipboardPrompts>()
        .answer(key, id, allowed, remember);
    for waiter in waiters {
        waiter(allowed, cx);
    }
}

/// Shows the prompt over the requesting terminal's window; none to show it in refuses.
fn present(key: PromptKey, id: u64, cx: &mut App) {
    let spec = prompt_alert(key.access);
    let allow = spec.buttons.iter().position(|button| button.label.as_ref() == ALLOW);
    let shown = super::keyboard::window_of(key.terminal).and_then(|handle| {
        handle
            .update(cx, |_, window, cx| {
                crate::modal::open_alert(
                    window,
                    spec,
                    move |reply, _, cx| answer(key, id, Some(reply.button) == allow, reply.checked, cx),
                    cx,
                );
            })
            .ok()
    });
    if shown.is_none() {
        tracing::info!(target: "ngterm::surface", terminal = key.terminal, "no window to ask in: clipboard access refused");
        answer(key, id, false, false, cx);
    }
}
