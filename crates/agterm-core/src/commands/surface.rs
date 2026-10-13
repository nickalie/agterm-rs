//! Terminal I/O on a session's panes: `session.text`, `copy`, `paste` and `selectall` here, `session.type` in
//! [`super::typing`], `session.search` in [`super::search`], `surface.cursor` and `surface.zoom` in
//! [`super::surface_target`]. Validation follows upstream's `ControlDispatcher.swift`; the effects follow
//! `agterm/Control/ControlServer+SurfaceIO.swift` (`readSessionText`, `copySelection`, `surfaceBindingAction`).

use std::time::Duration;

use agterm_host::TextScope;
use agterm_proto::command::{BrowserCmd, ConfigCmd, QuickCmd, SessionCmd};
use agterm_proto::{Args, Command, ControlResult, Request};
use uuid::Uuid;

use super::{Outcome, args_of, parse_pane, parse_surface_pane, session_store, unknown_pane_id, with_id};
use crate::Core;
use crate::dispatch::Dispatched;
use crate::error::ControlError;
use crate::model::{PaneAddress, SearchFollowUp, SearchReport, Session, StatusKeystroke, StatusPane};

/// How long a command waiting on the frontend sleeps between probes (upstream's 0.03 s main-actor polls).
pub const POLL_INTERVAL: Duration = Duration::from_millis(30);

impl Core {
    /// A terminal reported on its search (upstream's `wireSearchCallbacks`): the report lands on the session whose
    /// pane carries `token`, the pane its search pins. A second start while a search is open ends the search on the
    /// pinned pane instead, whose end report then clears the state. A session or token gone since is skipped. Commit
    /// afterwards.
    pub fn report_search(&mut self, session: Uuid, token: &str, report: SearchReport) {
        let Some(session) = self
            .library_mut()
            .store_for_session_mut(session)
            .and_then(|store| store.session_mut(session))
        else {
            return;
        };
        let Some(pane) = session.pane_role_for_token(token) else {
            return;
        };
        if let SearchFollowUp::EndOn(owner) = session.search.apply(pane, report) {
            run_binding(session, owner, "end_search");
        }
    }

    /// A physical keystroke in the pane carrying `token` (upstream's `keyDown` → `onUserInputClearsStatus` →
    /// `wireStatusClear`): clears the session's status to idle when that pane owns it and the keystroke clears it under
    /// the live Status reset setting. The pane is the token's role now, so a promoted survivor clears as the left
    /// pane. Returns whether it cleared; commit afterwards when it did.
    pub fn clear_status_on_keystroke(&mut self, session: Uuid, token: &str, keystroke: StatusKeystroke) -> bool {
        let reset = self.settings().effective_status_reset();
        let Some(store) = self.library_mut().store_for_session_mut(session) else {
            return false;
        };
        let Some(pane) = store
            .session(session)
            .and_then(|found| found.pane_role_for_token(token))
        else {
            return false;
        };
        store.clear_status_on_input(session, pane, keystroke, reset)
    }
}

/// Starts a command that may wait on the frontend; `None` for every other command.
pub(crate) fn deferred(core: &mut Core, request: &Request) -> Option<Dispatched> {
    match request.cmd {
        Command::Session(SessionCmd::Type) => Some(super::typing::begin(core, request)),
        Command::Session(SessionCmd::Search) => Some(super::search::begin(core, request)),
        Command::Session(SessionCmd::Swap) => Some(super::panes::begin_swap(core, request)),
        Command::Quick(QuickCmd::Type | QuickCmd::Text) => Some(super::quick::begin(core, request)),
        Command::Config(ConfigCmd::Reload) => Some(super::config::begin_reload(core)),
        Command::Browser(BrowserCmd::Clear) => Some(super::browser::begin_clear(core, request)),
        _ => None,
    }
}

/// Answers a terminal I/O command of the session group that answers at once.
pub(crate) fn dispatch_session(core: &mut Core, cmd: SessionCmd, request: &Request) -> Outcome {
    let args = args_of(request);
    let target = request.target.as_deref();
    let window = args.window.as_deref();
    match cmd {
        SessionCmd::Text => text(core, target, args),
        SessionCmd::Copy => copy(core, target, window),
        SessionCmd::Paste => {
            let pane = parse_pane(args.pane.as_deref())?;
            binding_action(core, target, window, pane, "paste_from_clipboard")
        }
        SessionCmd::SelectAll => binding_action(core, target, window, None, "select_all"),
        // `session.type` and `session.search` start through `deferred`
        other => Err(ControlError::NotSupported(Command::Session(other))),
    }
}

// MARK: - shared pane access

pub(crate) fn not_realized() -> ControlError {
    ControlError::rejected("session not realized")
}

pub(crate) fn no_split_pane() -> ControlError {
    ControlError::rejected("session has no split pane")
}

pub(crate) fn no_scratch() -> ControlError {
    ControlError::rejected("session has no scratch terminal")
}

/// The terminal in pane `pane`, if its slot holds one.
pub(crate) fn pane_surface(session: &Session, pane: StatusPane) -> Option<&dyn agterm_host::Surface> {
    session.pane_slot(pane).get()
}

/// Whether pane `pane` holds a realized terminal.
pub(crate) fn pane_realized(session: &Session, pane: StatusPane) -> bool {
    pane_surface(session, pane).is_some_and(agterm_host::Surface::is_realized)
}

/// Runs a binding action on pane `pane`'s terminal; false when the slot is empty or the terminal not realized.
pub(crate) fn run_binding(session: &mut Session, pane: StatusPane, action: &str) -> bool {
    session
        .pane_slot_mut(pane)
        .get_mut()
        .is_some_and(|surface| surface.binding_action(action))
}

/// A named pane that must exist: the split pane and the scratch need a terminal in their slots.
pub(crate) fn existing_pane(session: &Session, pane: StatusPane) -> Result<StatusPane, ControlError> {
    match pane {
        StatusPane::Left => Ok(StatusPane::Left),
        StatusPane::Right if session.split_surface.is_present() => Ok(StatusPane::Right),
        StatusPane::Right => Err(no_split_pane()),
        StatusPane::Scratch if session.scratch_surface.is_present() => Ok(StatusPane::Scratch),
        StatusPane::Scratch => Err(no_scratch()),
    }
}

// MARK: - session.text

/// The extent `session.text` reads (upstream's `parseBufferExtent`): `--all` and `--lines` are exclusive, and a
/// line count must be positive.
pub(crate) fn buffer_extent(args: &Args) -> Result<(bool, Option<usize>), ControlError> {
    let all = args.all.unwrap_or(false);
    match args.lines {
        Some(_) if all => Err(ControlError::rejected("use either --all or --lines, not both")),
        Some(lines) if lines <= 0 => Err(ControlError::rejected("--lines must be greater than 0")),
        lines => Ok((all, lines.map(|lines| usize::try_from(lines).unwrap_or(usize::MAX)))),
    }
}

/// `session.text`: a pane's buffer as plain text, the visible screen by default, screen and scrollback with `--all`,
/// or the last `--lines` content lines. With neither `--pane-id` nor `--pane` the on-screen pane is read; the reply
/// names the pane read.
fn text(core: &mut Core, target: Option<&str>, args: &Args) -> Outcome {
    let (all, lines) = buffer_extent(args)?;
    let pane = parse_surface_pane(args.pane.as_deref())?;
    let (store, id) = session_store(core, target, args.window.as_deref())?;
    let session = store.session(id).ok_or_else(not_realized)?;
    let pane = match session.pane_address(args.pane_id.as_deref(), pane) {
        PaneAddress::UnknownToken(token) => return Err(unknown_pane_id(&token)),
        PaneAddress::Pane(Some(pane)) => existing_pane(session, pane)?,
        PaneAddress::Pane(None) => session.on_screen_pane(),
    };
    let surface = pane_surface(session, pane)
        .filter(|surface| surface.is_realized())
        .ok_or_else(not_realized)?;
    let scope = if all || lines.is_some() {
        TextScope::All
    } else {
        TextScope::Screen
    };
    let text = surface
        .read_text(scope)
        .ok_or_else(|| ControlError::rejected("failed to read surface buffer"))?;
    let text = match lines {
        Some(lines) => last_content_lines(&text, lines),
        None => text,
    };
    Ok(ControlResult {
        pane: Some(pane.as_str().to_owned()),
        ..ControlResult::text(text)
    })
}

/// The last `count` lines of `text` once the trailing whitespace-only grid rows are dropped, so `--lines` counts
/// content, not padding.
pub(crate) fn last_content_lines(text: &str, count: usize) -> String {
    let mut rows: Vec<&str> = text.split('\n').collect();
    while rows.last().is_some_and(|row| row.trim_matches([' ', '\t']).is_empty()) {
        rows.pop();
    }
    let start = rows.len().saturating_sub(count);
    rows[start..].join("\n")
}

// MARK: - selection

/// `session.copy`: the main pane's selection as text, never through the clipboard.
fn copy(core: &mut Core, target: Option<&str>, window: Option<&str>) -> Outcome {
    let (store, id) = session_store(core, target, window)?;
    let session = store.session(id).ok_or_else(not_realized)?;
    let surface = pane_surface(session, session.addressable_pane())
        .filter(|surface| surface.is_realized())
        .ok_or_else(not_realized)?;
    let text = surface
        .read_selection()
        .ok_or_else(|| ControlError::rejected("no selection"))?;
    Ok(ControlResult::text(text))
}

/// `session.paste` and `session.selectall`: a libghostty binding action on one pane, the main one unless `pane`
/// names another (upstream's `surfaceBindingAction`).
fn binding_action(
    core: &mut Core,
    target: Option<&str>,
    window: Option<&str>,
    pane: Option<StatusPane>,
    action: &str,
) -> Outcome {
    let (store, id) = session_store(core, target, window)?;
    let session = store.session_mut(id).ok_or_else(not_realized)?;
    let pane = match pane {
        None | Some(StatusPane::Left) => session.addressable_pane(),
        Some(pane) => existing_pane(session, pane)?,
    };
    if !run_binding(session, pane, action) {
        return Err(not_realized());
    }
    Ok(with_id(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_count_content_rows_from_the_end() {
        let screen = "one\ntwo\nthree\n   \n\t\n";
        assert_eq!(last_content_lines(screen, 2), "two\nthree");
        assert_eq!(last_content_lines(screen, 9), "one\ntwo\nthree");
        assert_eq!(last_content_lines("  \n \n", 3), "");
    }

    #[test]
    fn the_extent_refuses_both_and_a_non_positive_count() {
        let args = |all: Option<bool>, lines: Option<i64>| Args {
            all,
            lines,
            ..Args::default()
        };
        assert_eq!(buffer_extent(&args(None, None)), Ok((false, None)));
        assert_eq!(buffer_extent(&args(Some(true), None)), Ok((true, None)));
        assert_eq!(buffer_extent(&args(None, Some(3))), Ok((false, Some(3))));
        assert_eq!(
            buffer_extent(&args(Some(true), Some(0))),
            Err(ControlError::rejected("use either --all or --lines, not both"))
        );
        assert_eq!(
            buffer_extent(&args(None, Some(0))),
            Err(ControlError::rejected("--lines must be greater than 0"))
        );
    }
}
