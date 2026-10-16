//! Reset Live Sessions… and the reopen after its quit (upstream's `LiveResetCoordinator.swift` dialogs and
//! `LiveResetRelauncher.swift`; the GTK build's `zmx_runtime.rs` reset part, whose session host install and live
//! capabilities live in `session_host.rs` here).
//!
//! - **The menu item** (the app menu's closing section, between Settings… and Quit, as upstream's Quit group), shown
//!   only while live sessions are both configured and running ([`Core::live_reset_menu_visible`]), asks the core first
//!   ([`AppState::reset_live_sessions`]): a refusal shows upstream's informational alert ([`refusal_alert`]), otherwise
//!   its warning alert ([`confirm_alert`]: Cancel first, the default and Escape's; Reset destructive). Reset has the
//!   core select again ([`AppState::confirm_live_reset`]): a refusal now shows, else the app quits at once, without the
//!   quit prompt (upstream's `armablePending` skipping `applicationShouldTerminate`).
//! - **The reopen**: the quit (this one, or `zmx.reset --force`'s after its reply) arms the reset marker for the next
//!   launch ([`Core::arm_live_reset_on_quit`], `app.rs`'s shutdown) and starts the relauncher ([`spawn_relauncher`]): a
//!   detached `/bin/sh` that waits for this process to exit and starts the same executable with the app's environment,
//!   so the new instance runs on the same state directory and socket and consumes the marker. Upstream opens its bundle
//!   with `open -n`; Linux has no counterpart, and a launch before the exit would only forward to this instance.
//!
//! [`Core::live_reset_menu_visible`]: agterm_core::Core::live_reset_menu_visible
//! [`Core::arm_live_reset_on_quit`]: agterm_core::Core::arm_live_reset_on_quit

use std::path::Path;
use std::process::{Command, Stdio};

use agterm_core::{ResetPrompt, ResetRefusal};
use gpui_kit::{App, Window};

use crate::modal::{AlertSpec, open_alert};
use crate::state::{AppState, Poster, post};

/// The script the relauncher runs: wait for `$1` to exit (at most `$3` polls of 0.2 s), then start `$2` with the
/// environment the app ran with. Arguments travel positionally, never inside the script text.
const RELAUNCH_SCRIPT: &str = r#"i=0
while kill -0 "$1" 2>/dev/null; do
  i=$((i + 1))
  [ "$i" -ge "$3" ] && exit 1
  sleep 0.2
done
exec "$2"
"#;
/// Polls of 0.2 s before the waiter gives up without launching (upstream's `maxWaits`).
const RELAUNCH_MAX_WAITS: u32 = 300;

/// What Reset Live Sessions… shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResetAlert {
    /// The confirmation, with what it resets.
    Confirm(ResetPrompt),
    /// Why it cannot run.
    Refused(ResetRefusal),
}

/// The Reset button's index in [`confirm_alert`].
pub const RESET: usize = 1;

/// Upstream's warning alert: Cancel first, the default and the close response, then Reset drawn destructive.
#[must_use]
pub fn confirm_alert(prompt: &ResetPrompt) -> AlertSpec {
    AlertSpec::confirm(prompt.title.clone(), prompt.body.clone(), "Reset")
}

/// Upstream's informational alert for a refused reset.
#[must_use]
pub fn refusal_alert(refusal: ResetRefusal) -> AlertSpec {
    AlertSpec::info("Reset Live Sessions", refusal.user_message())
}

/// Opens `alert` over `window`; a confirmation's Reset posts [`AppState::confirm_live_reset`] to `poster`.
pub fn present(alert: &ResetAlert, poster: Poster, window: &mut Window, cx: &mut App) {
    match alert {
        ResetAlert::Confirm(prompt) => {
            open_alert(
                window,
                confirm_alert(prompt),
                move |answer, _, _| {
                    if answer.button == RESET {
                        post(&poster, AppState::confirm_live_reset);
                    }
                },
                cx,
            );
        }
        ResetAlert::Refused(refusal) => {
            open_alert(window, refusal_alert(*refusal), |_, _, _| {}, cx);
        }
    }
}

impl AppState {
    /// The app menu's Reset Live Sessions… (upstream's `LiveResetCoordinator.runFromMenu` up to its dialog): the
    /// refusal or the confirmation, for the frontend to show over the frontmost window.
    pub fn reset_live_sessions(&mut self) {
        if self.is_quitting() {
            return;
        }
        let alert = match self.core.prepare_live_reset() {
            Ok(prompt) => ResetAlert::Confirm(prompt),
            Err(refusal) => ResetAlert::Refused(refusal),
        };
        self.ask_live_reset(alert);
    }

    /// The confirmation's Reset: the core selects again and holds the selection for the quit, which runs now without
    /// the quit prompt; a refusal now is shown instead.
    pub fn confirm_live_reset(&mut self) {
        if self.is_quitting() {
            return;
        }
        match self.core.confirm_live_reset() {
            Ok(()) => {
                tracing::info!("the live sessions reset is confirmed: quitting to reopen");
                self.request_quit();
            }
            Err(refusal) => self.ask_live_reset(ResetAlert::Refused(refusal)),
        }
    }
}

/// Starts the waiter that reopens the app once this process exited, after a Live sessions reset quit; it inherits the
/// app's environment, so the new instance runs on the same state directory and socket. Whether it started.
pub fn spawn_relauncher() -> bool {
    let Ok(executable) = std::env::current_exe() else {
        tracing::error!("could not resolve the app executable to reopen after the live sessions reset");
        return false;
    };
    match relaunch_command(&executable, std::process::id()).spawn() {
        Ok(_) => {
            tracing::info!(executable = %executable.display(), "the app reopens once this process exits");
            true
        }
        Err(error) => {
            tracing::error!(%error, "could not start the relauncher");
            false
        }
    }
}

/// The waiter's command for `executable` after process `pid` exits, detached from the app's process group.
#[must_use]
pub fn relaunch_command(executable: &Path, pid: u32) -> Command {
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(RELAUNCH_SCRIPT)
        .arg("agterm-live-reset")
        .arg(pid.to_string())
        .arg(executable)
        .arg(RELAUNCH_MAX_WAITS.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    command
}

#[cfg(test)]
pub(crate) mod tests;
