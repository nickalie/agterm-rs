//! The persisted per-pane restore-command pin behind `session.restore` (upstream's `AppStore+Restore.swift`). The
//! state lives on the session's [`PaneRestore`](super::PaneRestore) pair; this is its one mutation point.

use uuid::Uuid;

use super::{AppStore, StatusPane};

impl AppStore {
    /// Sets pane `pane`'s persisted restore pin: `None` is no pin (auto-capture), `""` pinned to nothing (a plain
    /// shell), else the shell line the next launch runs. Saves at once on a change, so a pin survives a crash; an
    /// unchanged value writes nothing. The pending slots stay untouched: a write during this run must not execute
    /// during this run, only an app-bootstrap restore arms the pin.
    ///
    /// Whether the value changed: false when it was already in place, for an unknown session and for the scratch
    /// pane, which is never restored (the command refuses both first). A caller that must know the change reached
    /// disk writes the window at once and sets the previous value back when that fails, as `session.restore` does
    /// (upstream's `saveChecked` and rollback).
    pub fn set_restore_command(&mut self, value: Option<String>, pane: StatusPane, id: Uuid) -> bool {
        let Some(session) = self.session_mut(id) else {
            return false;
        };
        let restore = match pane {
            StatusPane::Left => &mut session.restore,
            StatusPane::Right => &mut session.split_restore,
            StatusPane::Scratch => return false,
        };
        if restore.command == value {
            return false;
        }
        restore.command = value;
        self.save();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{NewSession, SaveRequest};

    fn store_with_session() -> (AppStore, Uuid) {
        let mut store = AppStore::new();
        let workspace = store.add_workspace("work");
        let id = store
            .add_session(workspace, NewSession::at_cwd("/tmp"))
            .expect("a session");
        let _ = store.take_outbox();
        (store, id)
    }

    #[test]
    fn a_pin_lands_on_its_pane_and_saves_only_on_a_change() {
        let (mut store, id) = store_with_session();
        assert!(store.set_restore_command(Some("echo hi".to_owned()), StatusPane::Left, id));
        assert_eq!(store.take_outbox().save, SaveRequest::Now);
        assert!(!store.set_restore_command(Some("echo hi".to_owned()), StatusPane::Left, id));
        assert_eq!(store.take_outbox().save, SaveRequest::None);
        assert!(store.set_restore_command(Some(String::new()), StatusPane::Right, id));
        let session = store.session(id).expect("the session");
        assert_eq!(session.restore.command.as_deref(), Some("echo hi"));
        assert_eq!(session.split_restore.command.as_deref(), Some(""));
        assert_eq!(session.restore.pending_command, None);
        assert!(store.set_restore_command(None, StatusPane::Left, id));
        assert_eq!(store.session(id).and_then(|s| s.restore.command.clone()), None);
    }

    #[test]
    fn the_scratch_and_an_unknown_session_are_refused() {
        let (mut store, id) = store_with_session();
        assert!(!store.set_restore_command(Some("x".to_owned()), StatusPane::Scratch, id));
        assert!(!store.set_restore_command(Some("x".to_owned()), StatusPane::Left, Uuid::new_v4()));
        assert_eq!(store.take_outbox().save, SaveRequest::None);
    }
}
