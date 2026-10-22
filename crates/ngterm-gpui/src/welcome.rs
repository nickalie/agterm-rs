//! The first-launch welcome (upstream's `WelcomeAlert.swift` over `agtermCore.FirstRunWelcome`; port of the GTK build's
//! `welcome.rs`): an alert over the frontmost window offering the agent skill and the status hooks, both checked, with
//! Later and Install. It is due on a launch that finds no state of a previous one and has not shown it before, and is
//! marked shown before it opens, so a cancelled or failed install cannot bring it back. A UI-test launch keeps it away
//! unless asked for.
//!
//! The quit confirmation lives here too, as in the GTK build (upstream's `AppDelegate.applicationShouldTerminate` over
//! `QuitPrompt`): the app menu's Quit asks over the frontmost window while windows are open, naming what it closes.

use gpui_kit::{App, Window};
use ngterm_core::first_run::{FirstRunWelcome, QuitPrompt, QuitReason};
use ngterm_core::paths::ProcessEnv;
use ngterm_core::settings_model::SettingsChange;

use crate::modal::{AlertButton, AlertCheck, AlertSpec, open_alert};
use crate::state::{AppState, Poster, post};

/// The Install button's index in [`alert`].
pub const INSTALL: usize = 1;

/// The welcome's alert (upstream's `makeAlert`): both installs checked, Later (Escape) then Install (Return).
#[must_use]
pub fn alert() -> AlertSpec {
    AlertSpec {
        heading: FirstRunWelcome::TITLE.to_owned(),
        body: FirstRunWelcome::MESSAGE.to_owned(),
        buttons: vec![
            AlertButton::cancel("Later").with_id("welcome-later"),
            AlertButton::plain("Install")
                .as_default()
                .suggested()
                .with_id("welcome-install"),
        ],
        checks: vec![
            AlertCheck::new(FirstRunWelcome::SKILL_OPTION)
                .checked()
                .with_id("welcome-skill-checkbox"),
            AlertCheck::new(FirstRunWelcome::HOOKS_OPTION)
                .checked()
                .with_id("welcome-hooks-checkbox"),
        ],
        field: None,
    }
}

/// Opens the welcome over `window`; Install runs the checked installers over it.
pub fn present(window: &mut Window, cx: &mut App) {
    let handle = window.window_handle();
    open_alert(
        window,
        alert(),
        move |answer, _, cx| {
            if answer.button == INSTALL {
                let picked = |index: usize| answer.checks.get(index).copied().unwrap_or(false);
                crate::integrations::run_picked(handle, picked(0), picked(1), cx);
            }
        },
        cx,
    );
}

/// The Quit button's index in [`quit_alert`].
pub const QUIT: usize = 1;

/// What a user's Quit asks first (upstream's `applicationShouldTerminate`): the message naming what it closes while
/// windows are open, nothing on a UI-test launch (`ui_test`) or with no window open.
#[must_use]
pub fn quit_prompt(core: &ngterm_core::Core, ui_test: bool) -> Option<String> {
    let (windows, sessions) = core.library().open_counts();
    QuitReason::User
        .needs_confirmation(windows, ui_test)
        .then(|| QuitPrompt::message(windows, sessions, core.restore_launch().active))
}

/// The quit confirmation (the GTK build's `quit_dialog`): Cancel (Escape), then Quit drawn destructive and the default.
#[must_use]
pub fn quit_alert(message: &str) -> AlertSpec {
    AlertSpec {
        heading: QuitPrompt::TITLE.to_owned(),
        body: message.to_owned(),
        buttons: vec![
            AlertButton::cancel("Cancel").with_id("quit-cancel"),
            AlertButton::plain("Quit")
                .as_default()
                .destructive()
                .with_id("quit-confirm"),
        ],
        checks: Vec::new(),
        field: None,
    }
}

/// Opens the quit confirmation saying `message` over `window`; Quit posts the quit to `poster`, which quits through
/// the ordinary quit save.
pub fn present_quit(message: &str, poster: Poster, window: &mut Window, cx: &mut App) {
    open_alert(
        window,
        quit_alert(message),
        move |answer, _, _| {
            if answer.button == QUIT {
                post(&poster, AppState::request_quit);
            }
        },
        cx,
    );
}

impl AppState {
    /// The app menu's Quit: confirmed first while windows are open (the frontend asks over the frontmost window), else
    /// the app quits at once. A signal or the last window closing never asks.
    pub fn quit_from_menu(&mut self) {
        if self.is_quitting() {
            return;
        }
        match quit_prompt(&self.core, ngterm_core::is_ui_test_launch(&ProcessEnv)) {
            Some(message) => self.ask_quit(message),
            None => self.request_quit(),
        }
    }

    /// The welcome is due at this launch: marked shown first, then handed to the frontend, unless a UI-test launch
    /// keeps it away.
    pub fn request_welcome(&mut self) {
        if self.is_quitting() || FirstRunWelcome::is_suppressed(&ProcessEnv) {
            return;
        }
        self.core.change_settings(SettingsChange::WelcomeShown);
        self.settle();
        self.show_welcome();
    }
}

#[cfg(test)]
mod tests;
