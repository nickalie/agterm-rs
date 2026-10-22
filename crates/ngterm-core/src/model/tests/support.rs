//! Shared fixtures of the model tests.

use uuid::Uuid;

use crate::model::{AppStore, NewSession, Session};

/// Adds a selected session at `cwd` to `workspace`.
pub(super) fn add(store: &mut AppStore, workspace: Uuid, cwd: &str) -> Uuid {
    store
        .add_session(workspace, NewSession::at_cwd(cwd))
        .expect("the workspace exists")
}

/// Adds a background (unselected) session at `cwd` to `workspace`.
pub(super) fn add_background(store: &mut AppStore, workspace: Uuid, cwd: &str) -> Uuid {
    let spec = NewSession {
        select: false,
        ..NewSession::at_cwd(cwd)
    };
    store.add_session(workspace, spec).expect("the workspace exists")
}

/// The session ids of `workspace`, in order.
pub(super) fn ids(store: &AppStore, workspace: Uuid) -> Vec<Uuid> {
    store
        .workspace(workspace)
        .map(|workspace| workspace.sessions.iter().map(Session::id).collect())
        .unwrap_or_default()
}
