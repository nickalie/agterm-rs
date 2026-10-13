//! The GTK half of the built-in actions: what [`Core::perform_builtin`] leaves to the frontend (upstream's `AppActions`
//! dialogs and the sidebar's inline rename), run from a keymap chord or the app menu.
//!
//! [`Core::perform_builtin`]: agterm_core::Core::perform_builtin

use adw::prelude::*;
use agterm_core::app_actions::FrontendAction;
use agterm_core::pane_env::uuid_string;
use agterm_proto::command::WindowCmd;
use agterm_proto::{Args, Command, Request};
use gtk::gio;
use uuid::Uuid;

use crate::state::{self, AppState, Poster};
use crate::windows::Windows;

/// Runs `action` in its window; `close_grace` is the soft-close setting a delete confirmation names.
pub fn perform(windows: &Windows, poster: &Poster, action: FrontendAction, close_grace: bool) {
    match action {
        FrontendAction::CloseWindow(window) => {
            state::post(poster, move |state: &mut AppState| state.close_requested(window));
        }
        FrontendAction::ConfirmCloseSession { window, session } => {
            if let Some(ui) = windows.get(window) {
                ui.sidebar().close_sessions(vec![session]);
            }
        }
        FrontendAction::RenameWindow { window, name } => rename_window(windows, poster, window, &name),
        FrontendAction::DeleteWindow { window, name, sessions } => {
            delete_window(windows, poster, window, &name, sessions, close_grace);
        }
        FrontendAction::RenameSession { window, session: id }
        | FrontendAction::RenameWorkspace { window, workspace: id } => {
            if let Some(ui) = windows.get(window) {
                ui.sidebar().begin_rename(id);
            }
        }
        FrontendAction::DeleteWorkspace { window, workspace } => {
            if let Some(ui) = windows.get(window) {
                ui.sidebar().delete_workspace(workspace);
            }
        }
        FrontendAction::OpenDirectory { window, workspace } => {
            if let Some(ui) = windows.get(window) {
                ui.sidebar().open_directory(workspace);
            }
        }
        FrontendAction::ToggleFullscreen(window) => {
            if let Some(ui) = windows.get(window) {
                let gtk_window = ui.gtk_window();
                gtk_window.set_fullscreened(!gtk_window.is_fullscreen());
            }
        }
        other => later_task(&other),
    }
}

/// The palettes and the theme picker arrive with their own task (34); their chords already reach this point.
fn later_task(action: &FrontendAction) {
    tracing::info!(?action, "this action's UI is not built yet");
}

/// Rename Window: a dialog prefilled with the name (upstream's `renameActiveWindow` alert), committed through
/// `window.rename`.
fn rename_window(windows: &Windows, poster: &Poster, window: Uuid, name: &str) {
    let Some(ui) = windows.get(window) else { return };
    let dialog = adw::AlertDialog::new(Some("Rename Window"), Some("Enter a new name for this window."));
    let entry = gtk::Entry::builder().text(name).activates_default(true).build();
    dialog.set_extra_child(Some(&entry));
    dialog.add_responses(&[("cancel", "Cancel"), ("ok", "OK")]);
    dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("ok"));
    dialog.set_close_response("cancel");
    let poster = poster.clone();
    let field = entry.clone();
    // answered from the response, never from `closed`, which libadwaita emits first
    dialog.choose(Some(ui.gtk_window()), None::<&gio::Cancellable>, move |response| {
        if response != "ok" {
            return;
        }
        let name = field.text().to_string();
        state::post(&poster, move |state: &mut AppState| {
            state.window_command(WindowCmd::Rename, window, Some(name));
        });
    });
    entry.grab_focus();
}

/// Delete Window: confirmed while it holds sessions (upstream's `deleteActiveWindow`), then `window.delete`.
fn delete_window(windows: &Windows, poster: &Poster, window: Uuid, name: &str, sessions: usize, close_grace: bool) {
    let poster = poster.clone();
    let delete = move || {
        state::post(&poster, move |state: &mut AppState| {
            state.window_command(WindowCmd::Delete, window, None);
        });
    };
    if sessions == 0 {
        delete();
        return;
    }
    let Some(ui) = windows.get(window) else { return };
    let dialog = adw::AlertDialog::new(
        Some(&format!("Delete “{name}”?")),
        Some(&delete_body(sessions, close_grace)),
    );
    dialog.add_responses(&[("cancel", "Cancel"), ("confirm", "Delete")]);
    dialog.set_response_appearance("confirm", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.choose(Some(ui.gtk_window()), None::<&gio::Cancellable>, move |response| {
        if response == "confirm" {
            delete();
        }
    });
}

/// Upstream's `confirmDelete` text.
fn delete_body(sessions: usize, close_grace: bool) -> String {
    let suffix = if close_grace {
        "after a short undo window."
    } else {
        "immediately. You can reopen it from File > Open Recent."
    };
    if sessions == 1 {
        format!("This closes its session {suffix}")
    } else {
        format!("This closes {sessions} sessions {suffix}")
    }
}

/// A `window.*` request for window `window`, as the dialogs commit them.
#[must_use]
pub fn window_request(cmd: WindowCmd, window: Uuid, name: Option<String>) -> Request {
    Request {
        cmd: Command::Window(cmd),
        target: Some(uuid_string(window)),
        args: Some(Args {
            name,
            ..Args::default()
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delete_confirmation_uses_upstreams_words() {
        assert_eq!(
            delete_body(1, true),
            "This closes its session after a short undo window."
        );
        assert_eq!(
            delete_body(3, false),
            "This closes 3 sessions immediately. You can reopen it from File > Open Recent."
        );
    }
}
