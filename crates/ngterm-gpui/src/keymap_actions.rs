//! The frontend half of the built-in actions: what `Core::perform_builtin` leaves to the frontend (upstream's
//! `AppActions` dialogs and the sidebar's inline rename), run from a keymap chord (port of
//! `agterm-gtk/src/keymap_actions.rs`).
//!
//! Each needs the window, the sidebar or a dialog: full screen and the delete confirmation are carried out here on the
//! registered window ([`crate::keymap_dispatch::register_window`]); the window close and Settings… go to the model;
//! the session close confirmation, the session and workspace renames, Delete Workspace and Open Directory… go to the
//! window's sidebar ([`WindowView::sidebar`]); Rename Window and Delete Window are alerts over their window
//! ([`crate::modal`]).

use gpui_kit::App;
use ngterm_core::app_actions::FrontendAction;
use ngterm_core::pane_env::uuid_string;
use ngterm_proto::command::WindowCmd;
use ngterm_proto::{Args, Command, Request};
use uuid::Uuid;

use crate::keymap_dispatch::{self, KeyOrigin};
use crate::sidebar::Sidebar;
use crate::state::{self, AppState, Poster};
use crate::window::WindowView;

/// Runs `action` in its window; `close_grace` is the soft-close setting a delete confirmation names.
pub fn perform(poster: &Poster, action: FrontendAction, close_grace: bool, cx: &mut App) {
    match action {
        FrontendAction::ToggleFullscreen(window) => {
            if let Some(handle) = keymap_dispatch::window_of(KeyOrigin::Window(window)) {
                let _ = handle.update(cx, |_, window, _| window.toggle_fullscreen());
            }
        }
        FrontendAction::DeleteWindow { window, name, sessions } => {
            delete_window(poster, window, &name, sessions, close_grace, cx);
        }
        FrontendAction::CloseWindow(window) => {
            state::post(poster, move |state: &mut AppState| state.close_requested(window));
        }
        FrontendAction::OpenSettings(window) => {
            state::post(poster, move |state: &mut AppState| state.open_settings(window));
        }
        FrontendAction::ConfirmCloseSession { window, session } => {
            with_sidebar(window, cx, |sidebar, root, cx| {
                sidebar.close_sessions(vec![session], root, cx);
            });
        }
        FrontendAction::RenameSession { window, session: id }
        | FrontendAction::RenameWorkspace { window, workspace: id } => {
            with_sidebar(window, cx, |sidebar, root, cx| sidebar.begin_rename(id, root, cx));
        }
        FrontendAction::DeleteWorkspace { window, workspace } => {
            with_sidebar(window, cx, |sidebar, root, cx| {
                sidebar.delete_workspace(workspace, root, cx);
            });
        }
        FrontendAction::OpenDirectory { window, workspace } => {
            with_sidebar(window, cx, |sidebar, root, cx| {
                sidebar.open_directory(workspace, root, cx);
            });
        }
        FrontendAction::RenameWindow { window, name } => rename_window(poster, window, &name, cx),
    }
}

/// Runs `run` on the sidebar of model window `window`, when that window is open.
fn with_sidebar(
    window: Uuid,
    cx: &mut App,
    run: impl FnOnce(&mut Sidebar, &mut gpui_kit::Window, &mut gpui_kit::Context<Sidebar>),
) {
    let Some(handle) =
        keymap_dispatch::window_of(KeyOrigin::Window(window)).and_then(|handle| handle.downcast::<WindowView>())
    else {
        return;
    };
    let ran = handle.update(cx, |view, root, cx| {
        view.sidebar().clone().update(cx, |sidebar, cx| run(sidebar, root, cx));
    });
    if ran.is_err() {
        tracing::debug!(%window, "the window closed before its sidebar action ran");
    }
}

/// Delete Window: confirmed while it holds sessions (upstream's `deleteActiveWindow`), then `window.delete`.
fn delete_window(poster: &Poster, window: Uuid, name: &str, sessions: usize, close_grace: bool, cx: &mut App) {
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
    let Some(handle) = keymap_dispatch::window_of(KeyOrigin::Window(window)) else {
        return;
    };
    let spec = crate::modal::AlertSpec::confirm(
        format!("Delete “{name}”?"),
        delete_body(sessions, close_grace),
        "Delete",
    );
    let shown = handle.update(cx, |_, window, cx| {
        crate::modal::open_alert(
            window,
            spec,
            move |answer, _, _| {
                if answer.button == 1 {
                    delete();
                }
            },
            cx,
        );
    });
    if shown.is_err() {
        tracing::debug!(%window, "the window closed before its delete confirmation");
    }
}

/// Rename Window's alert (upstream's `renameActiveWindow`, the GTK build's dialog): a field holding the name, all of it
/// selected; OK (Return) commits it through `window.rename`, Cancel (Escape) keeps the name.
#[must_use]
pub fn rename_alert(name: &str) -> crate::modal::AlertSpec {
    crate::modal::AlertSpec {
        heading: "Rename Window".to_owned(),
        body: "Enter a new name for this window.".to_owned(),
        buttons: vec![
            crate::modal::AlertButton::cancel("Cancel"),
            crate::modal::AlertButton::plain("OK").as_default().suggested(),
        ],
        checks: Vec::new(),
        field: Some(name.to_owned()),
    }
}

/// Rename Window: [`rename_alert`] over the window, committed through `window.rename`.
fn rename_window(poster: &Poster, window: Uuid, name: &str, cx: &mut App) {
    let Some(handle) = keymap_dispatch::window_of(KeyOrigin::Window(window)) else {
        return;
    };
    let poster = poster.clone();
    let spec = rename_alert(name);
    let shown = handle.update(cx, |_, root, cx| {
        crate::modal::open_alert(
            root,
            spec,
            move |answer, _, _| {
                let Some(name) = answer.text.filter(|_| answer.button == 1) else {
                    return;
                };
                state::post(&poster, move |state: &mut AppState| {
                    state.window_command(WindowCmd::Rename, window, Some(name));
                });
            },
            cx,
        );
    });
    if shown.is_err() {
        tracing::debug!(%window, "the window closed before its rename");
    }
}

/// Upstream's `confirmDelete` text.
#[must_use]
pub fn delete_body(sessions: usize, close_grace: bool) -> String {
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

    #[test]
    fn a_window_request_targets_the_window_by_id() {
        let window = Uuid::from_u128(7);
        let request = window_request(WindowCmd::Rename, window, Some("w".into()));
        assert_eq!(request.cmd, Command::Window(WindowCmd::Rename));
        assert_eq!(request.target, Some(uuid_string(window)));
        assert_eq!(request.args.and_then(|args| args.name).as_deref(), Some("w"));
    }
}
