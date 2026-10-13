//! Duplicating a session (upstream's `AppStore+Duplicate.swift`).

use std::path::Path;

use uuid::Uuid;

use super::session::Session;
use super::store::{AppStore, NewSession};

impl Session {
    /// Where a local shell started for this session runs: `path` itself, except that a remote session's path names
    /// a directory on its host, so it is used only when it exists here too, else `home` (upstream's
    /// `localWorkingDirectory`).
    #[must_use]
    pub fn local_working_directory(&self, path: &str, home: &str) -> String {
        if self.remote_host().is_none() || Path::new(path).is_dir() {
            path.to_owned()
        } else {
            home.to_owned()
        }
    }
}

impl AppStore {
    /// Duplicates a session: a fresh shell in the same workspace, right after the source, rooted at the source's
    /// focused-pane cwd. Only the directory carries over: an automatic name, no split, scratch, status, flag or
    /// command. Selected, as a new session is. `None` when no session matches.
    pub fn duplicate_session(&mut self, id: Uuid, home: &str) -> Option<Uuid> {
        let session = self.session(id)?;
        let cwd = session.local_working_directory(session.focused_cwd(), home);
        let location = self.session_location(id)?;
        self.add_session(
            location.workspace,
            NewSession {
                at: Some(location.index + 1),
                ..NewSession::at_cwd(cwd)
            },
        )
    }
}
