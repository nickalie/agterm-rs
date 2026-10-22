//! `session.status`, `session.flag`, `session.context` and `session.seen`: the per-session metadata commands.
//! Validation and its messages follow the `.sessionStatus` arm and `dispatchSessionContext` in upstream's
//! `ControlDispatcher.swift`; the effects follow `setSessionStatus`, `setSessionFlag`, `setSessionContext` and
//! `markSessionSeen` in `agterm/Control/ControlServer+SessionActions.swift`.
//!
//! Status sounds are queued for the frontend ([`Core::take_status_sounds`]); upstream resolves a name with
//! `NSSound(named:)`, here a name is known when it is `default`, `beep` or one of upstream's standard names.

use ngterm_proto::command::SessionCmd;
use ngterm_proto::{Args, Command, ControlResult, Request};

use super::{Outcome, ToggleMode, args_of, in_placement_store, not_found, parse_pane, session_store, with_id};
use crate::Core;
use crate::error::{ControlError, Noun};
use crate::model::{
    AgentIndicator, AgentStatus, STANDARD_SOUND_NAMES, StatusPane, StatusShape, StatusWriteResult, is_known_sound,
    is_valid_color_hex, validate_context,
};
use crate::resolve::ACTIVE;

/// Answers a session metadata command; `None` is a bare `ok` (`session.flag --mode clear`).
pub(crate) fn dispatch(
    core: &mut Core,
    cmd: SessionCmd,
    request: &Request,
) -> Result<Option<ControlResult>, ControlError> {
    let args = args_of(request);
    let target = request.target.as_deref();
    let window = args.window.as_deref();
    match cmd {
        SessionCmd::Status => status(core, target, args).map(Some),
        SessionCmd::Flag => flag(core, target, window, args.mode.as_deref().unwrap_or("toggle")),
        SessionCmd::Context => {
            let context = parse_context(args)?;
            let (store, id) = session_store(core, target, window)?;
            // unchanged is a no-op: no save, no event
            store.set_context(context, id);
            Ok(Some(with_id(id)))
        }
        SessionCmd::Seen => {
            let (store, id) = session_store(core, target, window)?;
            store.clear_unseen(id);
            Ok(Some(with_id(id)))
        }
        other => Err(ControlError::NotSupported(Command::Session(other))),
    }
}

// MARK: - status

/// A `session.status` write with its arguments checked (upstream's `ControlSessionStatusUpdate`).
struct StatusUpdate<'a> {
    status: AgentStatus,
    color: Option<String>,
    shape: Option<StatusShape>,
    pane: Option<StatusPane>,
    pane_id: Option<&'a str>,
    /// The per-call sound, empty counting as none.
    sound: Option<&'a str>,
}

impl<'a> StatusUpdate<'a> {
    /// Checks the status, then the color, the shape and the pane, in upstream's order, so the error a caller sees for
    /// several bad arguments is upstream's.
    fn parse(args: &'a Args) -> Result<Self, ControlError> {
        let status = args
            .status
            .as_deref()
            .and_then(AgentStatus::from_wire)
            .ok_or_else(|| ControlError::rejected("invalid status"))?;
        if let Some(color) = args.color.as_deref()
            && !is_valid_color_hex(color)
        {
            return Err(ControlError::rejected("invalid color (expected #rrggbb)"));
        }
        let shape = args
            .shape
            .as_deref()
            .map(|raw| {
                StatusShape::from_wire(raw).ok_or_else(|| {
                    ControlError::rejected(format!("invalid shape: {raw} ({})", StatusShape::valid_names_list()))
                })
            })
            .transpose()?;
        Ok(Self {
            status,
            color: args.color.clone(),
            shape,
            pane: parse_pane(args.pane.as_deref())?,
            pane_id: args.pane_id.as_deref(),
            sound: args.sound.as_deref().filter(|sound| !sound.is_empty()),
        })
    }
}

/// `session.status`: a fresh indicator per call, so an override the call leaves out is gone. An unknown sound is
/// refused before the target resolves and leaves the status unchanged; a write another pane's block refuses plays
/// nothing. A sound plays on apply: the per-call one, else the configured blocked sound on a transition INTO
/// `blocked`.
fn status(core: &mut Core, target: Option<&str>, args: &Args) -> Outcome {
    let update = StatusUpdate::parse(args)?;
    if let Some(sound) = update.sound
        && !is_known_sound(sound)
    {
        let hint = STANDARD_SOUND_NAMES.join(", ");
        return Err(ControlError::rejected(format!(
            "unknown sound: {sound} (use 'default', 'beep', or one of: {hint})"
        )));
    }
    let blocked_default = core.settings().blocked_status_sound_name.clone();
    let (store, id) = session_store(core, target, args.window.as_deref())?;
    let session = store
        .session(id)
        .ok_or_else(|| not_found(Noun::Session, target.unwrap_or(ACTIVE)))?;
    let was_blocked = session.agent_indicator.status == AgentStatus::Blocked;
    // a live pane token wins over the role named beside it; an unknown one falls back to that role
    let status_pane = update
        .pane_id
        .and_then(|token| session.pane_role_for_token(token))
        .or(update.pane);
    let indicator = AgentIndicator {
        status: update.status,
        blink: args.blink.unwrap_or(false),
        auto_reset: args.auto_reset.unwrap_or(false),
        color: update.color,
        shape: update.shape,
        status_pane,
    };
    if let StatusWriteResult::Refused { owner } = store.apply_control_status(indicator, id) {
        return Err(ControlError::rejected(format!(
            "blocked status owned by pane {} (write from that pane to change it)",
            owner.as_str()
        )));
    }
    let blocked_default = blocked_default.as_deref().filter(|_| !was_blocked);
    if let Some(sound) = update.status.effective_sound(update.sound, blocked_default) {
        core.play_status_sound(sound.to_owned());
    }
    Ok(with_id(id))
}

// MARK: - flag and context

/// `session.flag`: `on`, `off` or `toggle` against the session's flag, idempotent; `clear` ignores the target and
/// unflags every session of the `--window` (else frontmost) window, answering no id. The target resolves before an
/// unknown mode is refused, as upstream's app side checks it.
fn flag(
    core: &mut Core,
    target: Option<&str>,
    window: Option<&str>,
    mode: &str,
) -> Result<Option<ControlResult>, ControlError> {
    if mode == "clear" {
        in_placement_store(core, window, |store| {
            store.clear_flags();
            Ok(())
        })?;
        return Ok(None);
    }
    let (store, id) = session_store(core, target, window)?;
    let toggle =
        ToggleMode::parse(Some(mode)).ok_or_else(|| ControlError::rejected(format!("invalid flag mode: {mode}")))?;
    let flagged = store.session(id).is_some_and(|session| session.flagged);
    store.set_flag(toggle.desired_value(flagged), id);
    Ok(Some(with_id(id)))
}

/// `session.context`'s value: `set` takes the text, trimmed and validated, `clear` takes none. An invalid value is
/// refused, never normalized, so the previous context stands.
fn parse_context(args: &Args) -> Result<Option<String>, ControlError> {
    match args.mode.as_deref().unwrap_or_default() {
        "set" => {
            let text = args
                .text
                .as_deref()
                .ok_or_else(|| ControlError::rejected("session.context set requires text"))?;
            validate_context(text).map(Some).map_err(ControlError::rejected)
        }
        "clear" if args.text.is_some() => Err(ControlError::rejected("session.context clear takes no text")),
        "clear" => Ok(None),
        mode => Err(ControlError::rejected(format!(
            "invalid context mode: {mode} (set|clear)"
        ))),
    }
}
