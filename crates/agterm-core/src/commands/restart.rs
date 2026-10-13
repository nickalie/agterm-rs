//! `session.restart` (upstream's `dispatchSessionRestart` in `ControlDispatcher.swift` and `resolveRestartTarget` in
//! `agterm/Control/ControlServer+Restart.swift`): replaces one LIVE pane's shell in place, running the caller's line
//! or the pane's foreground program again ([`crate::restart_replay`]).
//!
//! Live sessions come with their own task, so this answers every refusal upstream makes up to and including the live
//! check, in upstream's order: a local pane without a live shell answers
//! "session.restart needs Live sessions mode; this pane has no live shell to replace". The kill, the wait for the old
//! program, the rebuilt surface and the [`agterm_proto::result::ControlRestartReceipt`] join with live sessions.

use agterm_proto::command::SessionCmd;
use agterm_proto::{Command, Request};

use super::surface::{no_split_pane, not_realized, pane_surface};
use super::{Outcome, args_of, not_found, parse_pane, session_store, unknown_pane_id};
use crate::Core;
use crate::command_restore::has_control_character;
use crate::error::{ControlError, Noun};
use crate::model::{PaneAddress, Session, StatusPane};
use crate::resolve::ACTIVE;
use crate::restart_replay::MAX_COMMAND_BYTES;

/// The pane the dispatcher checked, by role or token. The line it checked (`None` replays the foreground program)
/// joins with the restart flow.
#[derive(Debug)]
struct RestartOptions<'a> {
    pane: Option<StatusPane>,
    pane_id: Option<&'a str>,
}

/// Answers `session.restart`.
pub(crate) fn dispatch(core: &mut Core, request: &Request) -> Outcome {
    let args = args_of(request);
    let options = parse(args.command.as_deref(), args.pane.as_deref(), args.pane_id.as_deref())?;
    let target = request.target.as_deref();
    let (store, id) = session_store(core, target, args.window.as_deref())?;
    let session = store
        .session(id)
        .ok_or_else(|| not_found(Noun::Session, target.unwrap_or(ACTIVE)))?;
    let pane = resolve_pane(session, &options)?;
    let live = pane_surface(session, pane).is_some_and(agterm_host::Surface::is_live_session);
    if !live {
        return Err(ControlError::rejected(
            "session.restart needs Live sessions mode; this pane has no live shell to replace",
        ));
    }
    // a live pane's kill and rebuild arrive with live sessions
    Err(ControlError::NotSupported(Command::Session(SessionCmd::Restart)))
}

/// The dispatcher's checks: the line is a shell line, never rewritten, refused only when blank, carrying a control
/// character or over the cap; a pane must be named, by token or role, since a default would restart a shell the
/// caller never addressed. An empty token counts as absent.
fn parse<'a>(
    command: Option<&str>,
    pane: Option<&str>,
    pane_id: Option<&'a str>,
) -> Result<RestartOptions<'a>, ControlError> {
    if let Some(command) = command {
        if command.trim_matches(is_swift_whitespace).is_empty() {
            return Err(ControlError::rejected(
                "session.restart takes a non-empty command, or none to replay",
            ));
        }
        if has_control_character(command) {
            return Err(ControlError::rejected("command must not contain control characters"));
        }
        if command.len() > MAX_COMMAND_BYTES {
            return Err(ControlError::rejected(format!(
                "command too long (max {MAX_COMMAND_BYTES} bytes)"
            )));
        }
    }
    let pane = parse_pane(pane)?;
    if pane == Some(StatusPane::Scratch) {
        return Err(scratch_refused());
    }
    let pane_id = pane_id.filter(|token| !token.is_empty());
    if pane.is_none() && pane_id.is_none() {
        return Err(ControlError::rejected("session.restart requires --pane-id or --pane"));
    }
    Ok(RestartOptions { pane, pane_id })
}

/// The pane a restart addresses, with upstream's refusals in its order. A token that resolves to nothing is refused
/// even beside `--pane`: the role may by now name a different terminal than the one the caller meant to end.
fn resolve_pane(session: &Session, options: &RestartOptions<'_>) -> Result<StatusPane, ControlError> {
    if let Some(host) = session.remote_host() {
        return Err(ControlError::rejected(format!(
            "session.restart needs a local pane; this session runs on {host}"
        )));
    }
    if let Some(token) = options.pane_id
        && session.pane_role_for_token(token).is_none()
    {
        return Err(unknown_pane_id(token));
    }
    let pane = match session.pane_address(options.pane_id, options.pane) {
        PaneAddress::UnknownToken(token) => return Err(unknown_pane_id(&token)),
        PaneAddress::Pane(None | Some(StatusPane::Scratch)) => return Err(scratch_refused()),
        PaneAddress::Pane(Some(pane)) => pane,
    };
    if pane == StatusPane::Right && !session.split_surface.is_present() {
        return Err(no_split_pane());
    }
    if pane_surface(session, pane).is_none() {
        return Err(not_realized());
    }
    Ok(pane)
}

fn scratch_refused() -> ControlError {
    ControlError::rejected("session.restart does not address the scratch pane")
}

/// Swift's `CharacterSet.whitespaces`: tab and the Unicode space separators, not line breaks.
fn is_swift_whitespace(c: char) -> bool {
    c == '\t'
        || (c.is_whitespace() && !matches!(c, '\n' | '\u{b}' | '\u{c}' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}'))
}
