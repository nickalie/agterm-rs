//! `session.restore`: pins the shell line a session's pane re-runs on the next launch. Validation and its messages
//! follow `dispatchSessionRestore` in upstream's `ControlDispatcher.swift`; the effect follows `setSessionRestore` in
//! `agterm/Control/ControlServer+SessionActions.swift`. A pin is shell code and is never rewritten: it is refused only
//! when absent, carrying a control character, or over the storage cap.

use agterm_proto::{ControlResult, Request};

use super::{Outcome, args_of, not_found, parse_pane, session_store, unknown_pane_id};
use crate::Core;
use crate::command_restore::has_control_character;
use crate::error::{ControlError, Noun};
use crate::model::{PaneAddress, StatusPane};
use crate::pane_env::uuid_string;
use crate::persist::RestoreMode;
use crate::resolve::ACTIVE;

/// The longest pin, in UTF-8 bytes (upstream's `ControlRestoreOverride.maxCommandBytes`): it persists in the window's
/// snapshot, so the cap guards the file, not the display.
pub(crate) const MAX_COMMAND_BYTES: usize = 1024;

/// What a `session.restore` writes (upstream's `ControlRestoreOverride`).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Pin {
    /// `set`: this shell line, typed verbatim on the next launch.
    Command(String),
    /// `none`: pinned to nothing, a plain shell.
    Nothing,
    /// `clear`: no pin, back to the captured foreground.
    Unpin,
}

impl Pin {
    /// The persisted value: the line, `""` for nothing, `None` for no pin.
    fn value(&self) -> Option<String> {
        match self {
            Self::Command(command) => Some(command.clone()),
            Self::Nothing => Some(String::new()),
            Self::Unpin => None,
        }
    }
}

/// Answers `session.restore`: the mode, then the pane selector, then the target; a `--pane-id` resolves against the
/// session's live terminals, and an unknown one with no `--pane` beside it is refused rather than pinning the main
/// pane by accident.
pub(crate) fn dispatch(core: &mut Core, request: &Request) -> Outcome {
    let args = args_of(request);
    let pin = parse_pin(args.mode.as_deref(), args.command.as_deref())?;
    let pane = parse_pane(args.pane.as_deref())?;
    let mode = launch_restore_mode(core);
    let target = request.target.as_deref();
    let (store, id) = session_store(core, target, args.window.as_deref())?;
    let session = store
        .session(id)
        .ok_or_else(|| not_found(Noun::Session, target.unwrap_or(ACTIVE)))?;
    let pane = match session.pane_address(args.pane_id.as_deref(), pane) {
        PaneAddress::UnknownToken(token) => return Err(unknown_pane_id(&token)),
        PaneAddress::Pane(resolved) => resolved.unwrap_or(StatusPane::Left),
    };
    match pane {
        StatusPane::Scratch => return Err(ControlError::rejected("the scratch terminal is never restored")),
        StatusPane::Right if !session.split.exists => return Err(ControlError::rejected("session has no split")),
        _ => {}
    }
    if !store.set_restore_command(pin.value(), pane, id) {
        return Err(ControlError::rejected(
            "failed to save the restore override, the previous value is still in effect",
        ));
    }
    let text = (pin != Pin::Unpin && mode != RestoreMode::Rerun)
        .then(|| format!("saved for rerun mode; active restore mode is {}", mode.wire_name()));
    Ok(ControlResult {
        id: Some(uuid_string(id)),
        text,
        pane: Some(pane.as_str().to_owned()),
        ..ControlResult::default()
    })
}

/// The `set`|`none`|`clear` mode and its command. A control character would smuggle an extra line or an escape
/// sequence into the shell the pin is typed into, so the whole class is refused, tab included.
fn parse_pin(mode: Option<&str>, command: Option<&str>) -> Result<Pin, ControlError> {
    match mode.unwrap_or_default() {
        "set" => {
            let command = command.ok_or_else(|| ControlError::rejected("session.restore set requires a command"))?;
            if has_control_character(command) {
                return Err(ControlError::rejected("command must not contain control characters"));
            }
            if command.len() > MAX_COMMAND_BYTES {
                return Err(ControlError::rejected(format!(
                    "command too long (max {MAX_COMMAND_BYTES} bytes)"
                )));
            }
            Ok(Pin::Command(command.to_owned()))
        }
        "none" => Ok(Pin::Nothing),
        "clear" => Ok(Pin::Unpin),
        other => Err(ControlError::rejected(format!(
            "invalid restore mode: {other} (set|none|clear)"
        ))),
    }
}

/// The restore mode this launch runs in (upstream's `launchRestoreMode`): the settings' mode, with `live` falling back
/// to fresh shells, since this build has no live sessions yet. The restore-mode work replaces it with the launch
/// decision recorded at startup.
fn launch_restore_mode(core: &Core) -> RestoreMode {
    match core.settings().effective_restore_mode() {
        RestoreMode::Live => RestoreMode::None,
        mode => mode,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_command_is_a_pin_to_nothing_and_metacharacters_pass() {
        assert_eq!(parse_pin(Some("set"), Some("")), Ok(Pin::Command(String::new())));
        assert_eq!(Pin::Command(String::new()).value(), Some(String::new()));
        assert_eq!(
            parse_pin(Some("set"), Some("cd x && claude --resume 'y' | cat $HOME")),
            Ok(Pin::Command("cd x && claude --resume 'y' | cat $HOME".to_owned()))
        );
        let at_cap = "é".repeat(MAX_COMMAND_BYTES / 2);
        assert_eq!(parse_pin(Some("set"), Some(&at_cap)), Ok(Pin::Command(at_cap.clone())));
        assert_eq!(
            parse_pin(Some("set"), Some(&format!("{at_cap}x"))),
            Err(ControlError::rejected("command too long (max 1024 bytes)"))
        );
    }
}
