//! `session.restart` (upstream's `dispatchSessionRestart` in `ControlDispatcher.swift` and `resolveRestartTarget` in
//! `agterm/Control/ControlServer+Restart.swift`): replaces one LIVE pane's shell in place, running the caller's line
//! or the pane's foreground program again ([`crate::restart_replay`]).
//!
//! This answers every refusal upstream makes up to and including the live check, in upstream's order (a local pane
//! without a live shell answers "session.restart needs Live sessions mode; this pane has no live shell to replace"),
//! then hands the live pane to [`super::restart_live`].

use agterm_proto::{Request, Response};
use uuid::Uuid;

use super::restart_live::{self, Job};
use super::surface::{no_split_pane, not_realized, pane_surface};
use super::{args_of, not_found, parse_pane, session_store, unknown_pane_id};
use crate::Core;
use crate::command_restore::has_control_character;
use crate::dispatch::Dispatched;
use crate::error::{ControlError, Noun};
use crate::live::daemon_name;
use crate::model::{PaneAddress, Session, StatusPane};
use crate::resolve::ACTIVE;
use crate::restart_replay::MAX_COMMAND_BYTES;

/// The pane and the line the dispatcher checked: the pane by role or token, the line `None` to replay the foreground
/// program.
#[derive(Debug)]
struct RestartOptions<'a> {
    pane: Option<StatusPane>,
    pane_id: Option<&'a str>,
    command: Option<&'a str>,
}

/// Starts `session.restart`.
pub(crate) fn begin(core: &mut Core, request: &Request) -> Dispatched {
    match target_job(core, request) {
        Ok(job) => restart_live::begin(core, job),
        Err(error) => Dispatched::Done(Response::from(error)),
    }
}

/// The live pane a restart replaces, after every refusal the dispatcher and `resolveRestartTarget` make.
fn target_job(core: &mut Core, request: &Request) -> Result<Job, ControlError> {
    let args = args_of(request);
    let options = parse(args.command.as_deref(), args.pane.as_deref(), args.pane_id.as_deref())?;
    let target = request.target.as_deref();
    let (store, id) = session_store(core, target, args.window.as_deref())?;
    let session = store
        .session(id)
        .ok_or_else(|| not_found(Noun::Session, target.unwrap_or(ACTIVE)))?;
    let pane = resolve_pane(session, &options)?;
    let core_ref: &Core = core;
    let session = core_ref
        .library()
        .store_for_session(id)
        .and_then(|store| store.session(id))
        .ok_or_else(not_realized)?;
    let surface = pane_surface(session, pane).ok_or_else(not_realized)?;
    let token = surface.pane_token().to_owned();
    let identity = Uuid::parse_str(&token).map_err(|_| not_realized())?;
    // a pane whose terminal is not up yet counts by the daemon it attaches to (`Core::pane_backed`)
    if !core_ref.pane_backed(session, pane) || core_ref.live.host().is_none() {
        return Err(ControlError::rejected(
            "session.restart needs Live sessions mode; this pane has no live shell to replace",
        ));
    }
    if !surface.is_realized() {
        // a dormant Live pane attaches to its daemon now; a starting one is not up yet either (upstream's unrealized
        // view answers the same)
        core.wake_session(id);
        return Err(not_realized());
    }
    Ok(Job {
        session: id,
        pane,
        token,
        identity,
        daemon: daemon_name(identity),
        command: options.command.map(str::to_owned),
    })
}

/// The dispatcher's checks: the line is a shell line, never rewritten, refused only when blank, carrying a control
/// character or over the cap; a pane must be named, by token or role, since a default would restart a shell the
/// caller never addressed. An empty token counts as absent.
fn parse<'a>(
    command: Option<&'a str>,
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
    Ok(RestartOptions { pane, pane_id, command })
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
