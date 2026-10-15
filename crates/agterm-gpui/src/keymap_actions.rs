//! The frontend half of the built-in actions: what `Core::perform_builtin` leaves to the frontend (upstream's
//! `AppActions` dialogs and the sidebar's inline rename), run from a keymap chord (port of
//! `agterm-gtk/src/keymap_actions.rs`).
//!
//! Each needs the window, the sidebar or a dialog: full screen and the delete confirmation are carried out here on the
//! registered window ([`crate::keymap_dispatch::register_window`]); the window close, the session close confirmation,
//! the renames, Delete Workspace and Open Directory wait for the window shell, the sidebar and the dialogs that carry
//! them (plan Tasks 18, 19, 21, 26), and are logged until then.

use agterm_core::app_actions::FrontendAction;
use agterm_core::pane_env::uuid_string;
use agterm_proto::command::WindowCmd;
use agterm_proto::{Args, Command, Request};
use gpui_kit::{App, PromptButton, PromptLevel};
use uuid::Uuid;

use crate::keymap_dispatch::{self, KeyOrigin};
use crate::state::{self, AppState, Poster};

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
        other => tracing::info!(
            ?other,
            "this action waits for the window shell, the sidebar or its dialog"
        ),
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
    let heading = format!("Delete “{name}”?");
    let body = delete_body(sessions, close_grace);
    let answer = handle.update(cx, |_, window, cx| {
        window.prompt(
            PromptLevel::Warning,
            &heading,
            Some(&body),
            &[PromptButton::cancel("Cancel"), PromptButton::ok("Delete")],
            cx,
        )
    });
    let Ok(answer) = answer else { return };
    cx.spawn(async move |_| {
        if matches!(answer.await, Ok(1)) {
            delete();
        }
    })
    .detach();
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
