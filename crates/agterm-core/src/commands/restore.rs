//! `session.restore`: pins the shell line a session's pane re-runs on the next launch. Validation and its messages
//! follow `dispatchSessionRestore` in upstream's `ControlDispatcher.swift`; the effect follows `setSessionRestore` in
//! `agterm/Control/ControlServer+SessionActions.swift`. A pin is shell code and is never rewritten: it is refused only
//! when absent, carrying a control character, or over the storage cap.
//!
//! `restore.mode`, `restore.capture` and `restore.clear`: the restore policy and the app-global captures (upstream's
//! `ControlDispatcher+Zmx.swift` `restoreMode` parse and `ControlServer.swift`'s `readRestoreMode`, `setRestoreMode`,
//! `captureRestoreCommands` and `clearRestoreCommands`). Each capture and clear is acknowledged only once every open
//! window's write landed: the write is the whole effect.

use agterm_proto::command::RestoreCmd;
use agterm_proto::{ControlResult, Request};
use uuid::Uuid;

use super::{Outcome, args_of, not_found, parse_pane, session_store, unknown_pane_id};
use crate::Core;
use crate::command_restore::has_control_character;
use crate::error::{ControlError, Noun};
use crate::model::Session;
use crate::model::{PaneAddress, StatusPane};
use crate::pane_env::uuid_string;
use crate::persist::RestoreMode;
use crate::resolve::ACTIVE;
use crate::restore::capture::{CaptureKind, capture_foreground_commands};

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
    let mode = core.restore_launch().active;
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
    let previous = match pane {
        StatusPane::Right => session.split_restore.command.clone(),
        _ => session.restore.command.clone(),
    };
    // a pin is re-typed on every launch, so it is acknowledged only once it is on disk: written now, and put back
    // when the write fails (upstream's `saveChecked` rollback)
    if store.set_restore_command(pin.value(), pane, id) && !saved_now(core, id) {
        if let Some(store) = core
            .library()
            .window_holding_session(id)
            .and_then(|window| core.library_mut().store_mut(window))
        {
            store.set_restore_command(previous, pane, id);
        }
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

/// Writes the window holding session `id` now; whether it landed.
fn saved_now(core: &mut Core, id: Uuid) -> bool {
    core.library()
        .window_holding_session(id)
        .is_some_and(|window| core.save_window_checked(window))
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

/// Answers `restore.mode`, `restore.capture` and `restore.clear`; `None` is a bare ok.
pub(crate) fn dispatch_group(
    core: &mut Core,
    cmd: RestoreCmd,
    request: &Request,
) -> Result<Option<ControlResult>, ControlError> {
    match cmd {
        RestoreCmd::Mode => mode(core, args_of(request).mode.as_deref()).map(Some),
        RestoreCmd::Capture => capture(core).map(Some),
        RestoreCmd::Clear => clear(core).map(|()| None),
    }
}

/// `restore.mode`: the policy, or the mode for the NEXT launch written first. The mode parses strictly, unlike the
/// settings' tolerant decode: a typo must not select `none`, whose next launch reaps every live daemon.
fn mode(core: &mut Core, raw: Option<&str>) -> Result<ControlResult, ControlError> {
    if let Some(raw) = raw {
        let mode = RestoreMode::from_wire(raw)
            .ok_or_else(|| ControlError::rejected(format!("invalid restore mode: {raw}")))?;
        if !core.save_restore_mode(mode) {
            return Err(ControlError::rejected(format!(
                "could not save the restore mode; settings keep {}",
                core.settings().effective_restore_mode().wire_name()
            )));
        }
    }
    Ok(ControlResult {
        restore: Some(core.restore_status()),
        ..ControlResult::default()
    })
}

/// `restore.capture`: every open pane's foreground program captured now, for the exit that never reaches the quit
/// capture (a force quit, a crash). Only with `rerun` configured for the next launch.
fn capture(core: &mut Core) -> Result<ControlResult, ControlError> {
    let configured = core.settings().effective_restore_mode();
    if configured != RestoreMode::Rerun {
        return Err(ControlError::rejected(format!(
            "restore.capture requires rerun mode; configured restore mode is {}",
            configured.wire_name()
        )));
    }
    let ids = core.library().open_ids();
    let reader = core.restore.reader();
    let mut captured = 0;
    for id in &ids {
        if let (Some(reader), Some(store)) = (&reader, core.library.store_mut(*id)) {
            captured += capture_foreground_commands(store.sessions_mut(), reader, CaptureKind::OnDemand, &|| true);
        }
    }
    let plural = if captured == 1 { "" } else { "s" };
    if !core.save_open_windows_checked() {
        return Err(ControlError::rejected(format!(
            "captured {captured} pane{plural} but at least one window's save failed; failed windows keep their argv \
             in memory until they save successfully"
        )));
    }
    Ok(ControlResult {
        count: Some(i64::try_from(captured).unwrap_or(i64::MAX)),
        text: Some(format!("captured {captured} pane{plural}")),
        ..ControlResult::default()
    })
}

/// `restore.clear`: every open pane's captured command dropped, the armed ones included (the socket can take this
/// before the later windows' terminals spawn); the `session.restore` pins stay.
fn clear(core: &mut Core) -> Result<(), ControlError> {
    for id in core.library().open_ids() {
        if let Some(store) = core.library.store_mut(id) {
            store
                .sessions_mut()
                .for_each(Session::clear_captured_foreground_commands);
        }
    }
    if !core.save_open_windows_checked() {
        return Err(ControlError::rejected(
            "cleared every open pane but at least one window's save failed; those windows keep their captured \
             commands on disk until they save successfully",
        ));
    }
    Ok(())
}

impl Core {
    /// The `restore.mode` read-back.
    #[must_use]
    pub fn restore_status(&self) -> agterm_proto::ControlRestoreStatus {
        self.restore_launch().status(self.settings().effective_restore_mode())
    }

    /// Writes every open window now (upstream's `saveAllOpenChecked`); whether every write landed.
    fn save_open_windows_checked(&mut self) -> bool {
        // every window is written, a failed one included
        let mut saved = true;
        for id in self.library().open_ids() {
            saved &= self.save_window_checked(id);
        }
        saved
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
