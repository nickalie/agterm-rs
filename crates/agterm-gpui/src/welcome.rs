//! The first-launch welcome (upstream's `WelcomeAlert.swift` over `agtermCore.FirstRunWelcome`; port of the GTK build's
//! `welcome.rs`): an alert over the frontmost window offering the agent skill and the status hooks, both checked, with
//! Later and Install. It is due on a launch that finds no state of a previous one and has not shown it before, and is
//! marked shown before it opens, so a cancelled or failed install cannot bring it back. A UI-test launch keeps it away
//! unless asked for. (The quit confirmation the GTK module also holds comes with the app menu's Quit, Task 26.)

use agterm_core::first_run::FirstRunWelcome;
use agterm_core::paths::ProcessEnv;
use agterm_core::settings_model::SettingsChange;
use gpui_kit::{App, Window};

use crate::modal::{AlertButton, AlertCheck, AlertSpec, open_alert};
use crate::state::AppState;

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

impl AppState {
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
