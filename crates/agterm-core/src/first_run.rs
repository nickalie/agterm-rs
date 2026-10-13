//! The first-run welcome and the quit confirmation, host-free (upstream's `FirstRunWelcome.swift`, `QuitPrompt.swift`
//! and `QuitReason.swift`): the decisions and the copy; the dialogs live in the frontend.

use std::path::Path;

use crate::paths::Env;
use crate::persist::RestoreMode;

/// The one-time pointer at the optional extras, shown on a first launch only (upstream's `FirstRunWelcome`).
pub struct FirstRunWelcome;

impl FirstRunWelcome {
    /// Files a previous launch leaves in the state directory. The control socket is not one: it is bound before the
    /// decision is read, so its presence says nothing about earlier launches.
    pub const PRIOR_STATE_NAMES: [&str; 3] = ["settings.json", "workspaces.json", "windows"];

    pub const TITLE: &str = "Welcome to agterm";

    pub const MESSAGE: &str =
        "agterm ships optional extras. These two install from here, and all of them from the Help \
menu at any time:

The agent skill teaches Claude Code and Codex to drive agterm over its control socket.

The agent status hooks make an agent session report active, blocked or completed in the sidebar.

The command line tool puts agtermctl on your PATH. A Homebrew install already has it, so that one is only for a direct \
download.";

    pub const SKILL_OPTION: &str = "Install the agent skill";

    pub const HOOKS_OPTION: &str = "Install the agent status hooks";

    /// The variable that shows the welcome on a UI-test launch, which otherwise suppresses it.
    pub const UI_TEST_SHOW: &str = "AGTERM_UITEST_SHOW_WELCOME";

    /// Whether `directory` holds state a previous launch left. Read it before the launch writes anything: the first
    /// launch seeds a window and saves it at once.
    #[must_use]
    pub fn has_prior_state(directory: &Path) -> bool {
        Self::PRIOR_STATE_NAMES.iter().any(|name| directory.join(name).exists())
    }

    /// Whether the welcome is due: never twice, and never to a user who ran agterm before. `welcome_shown` covers a
    /// state directory carrying the flag, `has_prior_state` an upgrade from a build whose settings predate it.
    #[must_use]
    pub fn is_due(welcome_shown: Option<bool>, has_prior_state: bool) -> bool {
        welcome_shown != Some(true) && !has_prior_state
    }

    /// Whether a UI-test launch keeps the welcome away (upstream's `WelcomeAlert.isSuppressedForUITest`): every such
    /// launch starts on a fresh state directory and would otherwise open a dialog before its first assertion.
    #[must_use]
    pub fn is_suppressed(env: &dyn Env) -> bool {
        crate::is_ui_test_launch(env) && env.var(Self::UI_TEST_SHOW).is_none()
    }
}

/// What asked the app to quit (upstream's `QuitReason`, whose `AppleEvent` reasons have no Linux counterpart: the
/// desktop ends a session with a signal or a logout instead).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitReason {
    /// The user chose Quit.
    User,
    /// `SIGTERM`: a shutdown, a logout or a supervisor stopping the app.
    Signal,
    /// The last window closed.
    LastWindowClosed,
}

impl QuitReason {
    /// Whether the quit asks first (upstream's `applicationShouldTerminate`): only a user's Quit with windows open,
    /// outside a UI-test launch. A system quit must not wait on a dialog, and closing the last window already chose.
    #[must_use]
    pub fn needs_confirmation(self, open_windows: usize, ui_test_launch: bool) -> bool {
        self == Self::User && open_windows > 0 && !ui_test_launch
    }
}

/// The quit confirmation (upstream's `QuitPrompt` and the alert of `AppDelegate.applicationShouldTerminate`).
pub struct QuitPrompt;

impl QuitPrompt {
    pub const TITLE: &str = "Quit Agterm?";

    /// The informative line, naming how many windows and sessions the quit closes; `mode` is the launch's active
    /// restore mode, under which `live` keeps the shells running.
    #[must_use]
    pub fn message(windows: usize, sessions: usize, mode: RestoreMode) -> String {
        let window_clause = if windows == 1 {
            "1 window".to_owned()
        } else {
            format!("{windows} windows")
        };
        let session_clause = if sessions == 1 {
            "1 session".to_owned()
        } else {
            format!("{sessions} sessions")
        };
        if mode == RestoreMode::Live {
            format!("This closes {window_clause} and {session_clause}.")
        } else {
            format!("This closes {window_clause} and {session_clause}, ending all running shells.")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::MapEnv;

    #[test]
    fn the_welcome_is_due_on_a_fresh_state_directory_only() {
        assert!(FirstRunWelcome::is_due(None, false));
        assert!(FirstRunWelcome::is_due(Some(false), false), "a false flag still shows");
        assert!(!FirstRunWelcome::is_due(Some(true), false));
        assert!(
            !FirstRunWelcome::is_due(None, true),
            "an upgrading user's settings predate the flag"
        );
    }

    #[test]
    fn an_empty_or_missing_directory_has_no_prior_state() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!FirstRunWelcome::has_prior_state(dir.path()));
        assert!(!FirstRunWelcome::has_prior_state(&dir.path().join("absent")));
    }

    #[test]
    fn any_saved_state_file_counts_as_prior_state() {
        for name in FirstRunWelcome::PRIOR_STATE_NAMES {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join(name), b"").unwrap();
            assert!(FirstRunWelcome::has_prior_state(dir.path()), "{name}");
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("windows")).unwrap();
        assert!(FirstRunWelcome::has_prior_state(dir.path()));
    }

    /// The socket is bound before the decision is read, so counting it would suppress the welcome on the very launch
    /// that owes it.
    #[test]
    fn the_control_socket_is_not_prior_state() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("agterm.sock"), b"").unwrap();
        std::fs::write(dir.path().join("agterm.sock.lock"), b"").unwrap();
        assert!(!FirstRunWelcome::has_prior_state(dir.path()));
    }

    #[test]
    fn a_ui_test_launch_suppresses_the_welcome_unless_asked() {
        assert!(!FirstRunWelcome::is_suppressed(&MapEnv::new()));
        let ui_test = MapEnv::new()
            .with("AGTERM_STATE_DIR", "/tmp/s")
            .with("AGTERM_UITEST_FORCE_SIDEBAR_VISIBLE", "1");
        assert!(FirstRunWelcome::is_suppressed(&ui_test));
        assert!(!FirstRunWelcome::is_suppressed(
            &ui_test.with(FirstRunWelcome::UI_TEST_SHOW, "1")
        ));
    }

    #[test]
    fn the_message_names_every_extra_and_offers_the_two_installable_ones() {
        assert!(FirstRunWelcome::MESSAGE.contains("agent skill"));
        assert!(FirstRunWelcome::MESSAGE.contains("agent status hooks"));
        assert!(FirstRunWelcome::MESSAGE.contains("command line tool"));
        assert!(FirstRunWelcome::SKILL_OPTION.contains("agent skill"));
        assert!(FirstRunWelcome::HOOKS_OPTION.contains("agent status hooks"));
    }

    #[test]
    fn only_a_users_quit_with_open_windows_asks() {
        assert!(QuitReason::User.needs_confirmation(1, false));
        assert!(!QuitReason::User.needs_confirmation(0, false));
        assert!(
            !QuitReason::User.needs_confirmation(2, true),
            "a UI-test launch never asks"
        );
        assert!(!QuitReason::Signal.needs_confirmation(2, false));
        assert!(!QuitReason::LastWindowClosed.needs_confirmation(1, false));
    }

    #[test]
    fn the_quit_message_counts_and_names_what_it_ends() {
        assert_eq!(
            QuitPrompt::message(1, 1, RestoreMode::None),
            "This closes 1 window and 1 session, ending all running shells."
        );
        assert_eq!(
            QuitPrompt::message(2, 5, RestoreMode::Rerun),
            "This closes 2 windows and 5 sessions, ending all running shells."
        );
        assert_eq!(
            QuitPrompt::message(3, 1, RestoreMode::Live),
            "This closes 3 windows and 1 session."
        );
    }
}
