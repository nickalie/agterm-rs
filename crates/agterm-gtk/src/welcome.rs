//! The first-launch welcome (upstream's `WelcomeAlert.swift` over `agtermCore.FirstRunWelcome`) and the quit
//! confirmation (`AppDelegate.applicationShouldTerminate` over `QuitPrompt`): libadwaita alert dialogs over the
//! frontmost window.

use adw::prelude::*;
use agterm_core::first_run::{FirstRunWelcome, QuitPrompt, QuitReason};
use agterm_core::paths::ProcessEnv;
use agterm_core::settings_model::SettingsChange;
use gtk::gio;

use crate::state::{self, AppState};

/// Shows the welcome once, marking it shown first so a cancelled or failed install cannot bring it back on the next
/// launch. Suppressed on a UI-test launch unless asked for.
pub fn present_once(state: &mut AppState) {
    if FirstRunWelcome::is_suppressed(&ProcessEnv) {
        return;
    }
    state.core.change_settings(SettingsChange::WelcomeShown);
    state.settle();
    let Some(parent) = state.frontmost_gtk_window() else {
        return;
    };
    let (dialog, skill, hooks) = welcome_dialog();
    dialog.choose(Some(&parent), None::<&gio::Cancellable>, move |response| {
        if response == "install" {
            install(skill.is_active(), hooks.is_active());
        }
    });
}

/// The alert and its two option checkboxes, both on (upstream's `makeAlert`).
fn welcome_dialog() -> (adw::AlertDialog, gtk::CheckButton, gtk::CheckButton) {
    let checkbox = |label: &str, id: &str| gtk::CheckButton::builder().label(label).active(true).name(id).build();
    let skill = checkbox(FirstRunWelcome::SKILL_OPTION, "welcome-skill-checkbox");
    let hooks = checkbox(FirstRunWelcome::HOOKS_OPTION, "welcome-hooks-checkbox");
    let options = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .build();
    options.append(&skill);
    options.append(&hooks);
    let dialog = adw::AlertDialog::new(Some(FirstRunWelcome::TITLE), Some(FirstRunWelcome::MESSAGE));
    dialog.set_widget_name("welcome");
    dialog.set_extra_child(Some(&options));
    dialog.add_responses(&[("later", "Later"), ("install", "Install")]);
    dialog.set_response_appearance("install", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("install"));
    dialog.set_close_response("later");
    (dialog, skill, hooks)
}

/// Runs the picked installers. The agent integrations (the skill and the status hooks installers) arrive with their
/// own task; until then the pick is only recorded in the log.
fn install(skill: bool, hooks: bool) {
    tracing::info!(
        skill,
        hooks,
        "welcome: the agent integration installers are not built yet"
    );
}

/// The user asked to quit (the app menu's Quit): confirmed while windows are open, naming what the quit closes, then
/// the app quits through its ordinary quit save. A signal or the last window closing never asks.
pub fn request_quit(state: &AppState) {
    let (windows, sessions) = state.core.library().open_counts();
    let ui_test = agterm_core::is_ui_test_launch(&ProcessEnv);
    let parent = state.frontmost_gtk_window();
    let (true, Some(parent)) = (QuitReason::User.needs_confirmation(windows, ui_test), parent) else {
        quit(state);
        return;
    };
    let message = QuitPrompt::message(windows, sessions, state.core.restore_launch().active);
    let dialog = quit_dialog(&message);
    let poster = state.poster().clone();
    dialog.choose(Some(&parent), None::<&gio::Cancellable>, move |response| {
        if response == "quit" {
            state::post(&poster, |state: &mut AppState| quit(state));
        }
    });
}

fn quit_dialog(message: &str) -> adw::AlertDialog {
    let dialog = adw::AlertDialog::new(Some(QuitPrompt::TITLE), Some(message));
    dialog.add_responses(&[("cancel", "Cancel"), ("quit", "Quit")]);
    dialog.set_response_appearance("quit", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("quit"));
    dialog.set_close_response("cancel");
    dialog
}

fn quit(state: &AppState) {
    if let Some(app) = state.app() {
        app.quit();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gtk_test::on_gtk_thread;

    #[test]
    fn the_welcome_offers_both_installs_checked_and_later_dismisses() {
        on_gtk_thread(|| {
            let (dialog, skill, hooks) = welcome_dialog();
            assert!(skill.is_active() && hooks.is_active());
            assert_eq!(skill.widget_name(), "welcome-skill-checkbox");
            assert_eq!(dialog.heading().as_deref(), Some("Welcome to agterm"));
            assert_eq!(dialog.response_label("install"), "Install");
            assert_eq!(dialog.response_label("later"), "Later");
            assert_eq!(dialog.close_response(), "later");
        });
    }

    #[test]
    fn the_quit_dialog_names_what_it_closes_and_defaults_to_quit() {
        on_gtk_thread(|| {
            let dialog = quit_dialog("This closes 1 window and 2 sessions.");
            assert_eq!(dialog.heading().as_deref(), Some("Quit Agterm?"));
            assert_eq!(dialog.body(), "This closes 1 window and 2 sessions.");
            assert_eq!(dialog.default_response().as_deref(), Some("quit"));
            assert_eq!(dialog.close_response(), "cancel");
        });
    }
}
