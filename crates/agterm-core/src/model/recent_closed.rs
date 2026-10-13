//! Open Recent: recording closed sessions and workspaces, and reopening them into a store (upstream's
//! `AppStore+RecentClosed.swift`; the library half, `WindowLibrary+RecentClosed.swift`, is in `window_library.rs`).
//!
//! The list itself is upstream's `RecentClosedStore`, one file every window records into. Here the library owns an
//! in-memory copy ([`RecentClosedList`]) that it shares with each open store, so a record or a removal is visible to
//! every store and to the library at once, as a write to the shared file is upstream; each change also goes out
//! through the store's outbox ([`RecentClosedChange`]) for `persist` to apply to `recent-closed.json` with the same
//! rules. A store outside a library keeps a list of its own.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use uuid::Uuid;

use super::outbox::RecentClosedChange;
use super::session::Session;
use super::snapshot::{session_from_snapshot, session_snapshot, workspace_from_snapshot, workspace_snapshot};
use super::store::AppStore;
use super::workspace::Workspace;
use crate::persist::recent_closed::{record_recent_closed, reference_date};
use crate::persist::{
    RECENT_CLOSED_LIMIT, RecentClosedItem, RecentClosedKind, RecentClosedSession, RecentClosedWorkspace,
};

/// The Open Recent entries, newest first, shared between a library and its open stores.
#[derive(Debug, Default)]
pub(crate) struct RecentClosedList(Rc<RefCell<Vec<RecentClosedItem>>>);

impl RecentClosedList {
    /// Another handle onto the same list.
    pub(crate) fn share(&self) -> Self {
        Self(Rc::clone(&self.0))
    }

    pub(crate) fn items(&self) -> Vec<RecentClosedItem> {
        self.0.borrow().clone()
    }

    pub(crate) fn item(&self, id: Uuid) -> Option<RecentClosedItem> {
        self.0.borrow().iter().find(|item| item.id == id).cloned()
    }

    pub(crate) fn first(&self) -> Option<RecentClosedItem> {
        self.0.borrow().first().cloned()
    }

    /// Replaces the entries (the launch load), keeping at most the limit.
    pub(crate) fn replace(&self, mut items: Vec<RecentClosedItem>) {
        items.truncate(RECENT_CLOSED_LIMIT);
        *self.0.borrow_mut() = items;
    }

    pub(crate) fn record(&self, item: RecentClosedItem) {
        record_recent_closed(&mut self.0.borrow_mut(), item, RECENT_CLOSED_LIMIT);
    }

    pub(crate) fn remove(&self, id: Uuid) {
        self.0.borrow_mut().retain(|item| item.id != id);
    }

    pub(crate) fn clear(&self) {
        self.0.borrow_mut().clear();
    }

    /// The name and expansion the newest workspace entry with snapshot `id` records.
    pub(crate) fn workspace_shell(&self, id: Uuid) -> Option<(String, bool)> {
        self.0
            .borrow()
            .iter()
            .filter_map(|item| item.workspace.as_ref())
            .find(|workspace| workspace.snapshot.id == id)
            .map(|workspace| {
                (
                    workspace.snapshot.name.clone(),
                    !workspace.snapshot.collapsed.unwrap_or(false),
                )
            })
    }
}

/// What one reopen achieved (upstream's `RecentClosedRestore`). `complete` is false when a member of the entry's
/// snapshot ended up in no window: consuming the entry would then lose that session for good.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecentClosedRestore {
    pub restored: bool,
    pub complete: bool,
}

impl RecentClosedRestore {
    const FAILED: Self = Self {
        restored: false,
        complete: false,
    };
    const COMPLETE: Self = Self {
        restored: true,
        complete: true,
    };
}

/// Where a closed session sat, for its Open Recent entry.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ClosedSessionPlace<'a> {
    pub(crate) workspace_id: Uuid,
    pub(crate) workspace_name: &'a str,
    pub(crate) workspace_index: usize,
    pub(crate) session_index: usize,
}

impl AppStore {
    /// The Open Recent entries, newest first.
    #[must_use]
    pub fn recent_closed_items(&self) -> Vec<RecentClosedItem> {
        self.recent_closed.items()
    }

    // MARK: - Recording

    /// Records a closed session as entry `id`; a remote one is never written to disk, so it records nothing.
    pub(crate) fn record_recent_closed_session(&mut self, session: &Session, place: &ClosedSessionPlace<'_>, id: Uuid) {
        if !session.is_persistable() {
            return;
        }
        self.record_recent_closed(RecentClosedItem {
            id,
            kind: RecentClosedKind::Session,
            title: session.display_name(),
            subtitle: Some(place.workspace_name.to_owned()),
            closed_at: reference_date::now(),
            session: Some(RecentClosedSession {
                workspace_id: place.workspace_id,
                workspace_name: place.workspace_name.to_owned(),
                workspace_index: place.workspace_index,
                session_index: place.session_index,
                snapshot: session_snapshot(session),
            }),
            workspace: None,
        });
    }

    /// Records a closed workspace as entry `id`. The snapshot leaves remote sessions out, so the subtitle's count and the selection
    /// describe what a reopen restores. `focus_member` is captured before the close drops the membership.
    pub(crate) fn record_recent_closed_workspace(
        &mut self,
        workspace: &Workspace,
        selected: Option<Uuid>,
        focus_member: bool,
        id: Uuid,
    ) {
        let snapshot = workspace_snapshot(workspace);
        let count = snapshot.sessions.len();
        let restorable = selected.filter(|id| snapshot.sessions.iter().any(|session| session.id == *id));
        self.record_recent_closed(RecentClosedItem {
            id,
            kind: RecentClosedKind::Workspace,
            title: workspace.name.clone(),
            subtitle: Some(format!("{count} session{}", if count == 1 { "" } else { "s" })),
            closed_at: reference_date::now(),
            session: None,
            workspace: Some(RecentClosedWorkspace {
                snapshot,
                selected_session_id: restorable,
                focus_member: Some(focus_member),
            }),
        });
    }

    fn record_recent_closed(&mut self, item: RecentClosedItem) {
        self.recent_closed.record(item.clone());
        self.outbox
            .record_recent_closed(RecentClosedChange::Record(Box::new(item)));
    }

    /// Drops the Open Recent entry `id` (an undo brought its session back).
    pub(crate) fn remove_recent_closed_item(&mut self, id: Uuid) {
        self.recent_closed.remove(id);
        self.outbox.record_recent_closed(RecentClosedChange::Remove(id));
    }

    // MARK: - Reopening

    /// Reopens `item` into this store: a session or workspace still pending its close is undone, one already in the
    /// tree is selected, anything else is rebuilt from the snapshot with nothing armed. Whether anything was restored.
    /// The entry itself stays; the library's reopen consumes it.
    pub fn restore_recent_closed(&mut self, item: &RecentClosedItem) -> bool {
        self.restore_recent_closed_with(item, &HashSet::new()).restored
    }

    /// [`AppStore::restore_recent_closed`], never rebuilding a session in `occupied_elsewhere`: another open window
    /// holds it, and a second object under one id makes every id-keyed lookup answer with whichever window sorts
    /// first.
    pub fn restore_recent_closed_with(
        &mut self,
        item: &RecentClosedItem,
        occupied_elsewhere: &HashSet<Uuid>,
    ) -> RecentClosedRestore {
        match item.kind {
            RecentClosedKind::Session => item.session.as_ref().map_or(RecentClosedRestore::FAILED, |recent| {
                self.restore_recent_session(recent, occupied_elsewhere)
            }),
            RecentClosedKind::Workspace => item.workspace.as_ref().map_or(RecentClosedRestore::FAILED, |recent| {
                self.restore_recent_workspace(recent, occupied_elsewhere)
            }),
        }
    }

    fn restore_recent_session(
        &mut self,
        recent: &RecentClosedSession,
        occupied_elsewhere: &HashSet<Uuid>,
    ) -> RecentClosedRestore {
        if self.restore_or_select_existing_recent_session(recent) {
            return RecentClosedRestore::COMPLETE;
        }
        // the library routes an entry to the window holding its session, so an occupied id here means that lookup
        // missed: refuse rather than build the second object
        if occupied_elsewhere.contains(&recent.snapshot.id) {
            return RecentClosedRestore::FAILED;
        }
        let index = self.workspace_index_or_shell(recent.workspace_id, recent.workspace_index, &recent.workspace_name);
        let session = session_from_snapshot(&recent.snapshot, false);
        let id = session.id();
        let workspace_id = self.workspaces[index].id();
        let sessions = &mut self.workspaces[index].sessions;
        let at = recent.session_index.min(sessions.len());
        sessions.insert(at, session);
        self.emit_session_created(id, workspace_id);
        self.reselect_after_removal(Some(id));
        self.save();
        RecentClosedRestore::COMPLETE
    }

    /// Undoes the pending close holding the session (selecting it), or selects it when it is in the tree.
    fn restore_or_select_existing_recent_session(&mut self, recent: &RecentClosedSession) -> bool {
        let id = recent.snapshot.id;
        if let Some(pending) = self.pending_close_id_containing_session(id) {
            return self.undo_pending_close_selecting(Some(pending), Some(id));
        }
        if self.session(id).is_none() {
            return false;
        }
        self.select_session(Some(id));
        true
    }

    fn restore_recent_workspace(
        &mut self,
        recent: &RecentClosedWorkspace,
        occupied_elsewhere: &HashSet<Uuid>,
    ) -> RecentClosedRestore {
        if self.restore_or_select_existing_recent_workspace(recent, occupied_elsewhere) {
            return self.workspace_restore_outcome(recent, occupied_elsewhere);
        }
        let mut workspace = workspace_from_snapshot(&recent.snapshot);
        // a member may live on elsewhere (moved to another workspace pending its own close, or to another window):
        // its original object is alive there, so rebuild everything except it
        let taken = self.taken_session_ids(occupied_elsewhere);
        workspace.sessions.retain(|session| !taken.contains(&session.id()));
        let workspace_id = workspace.id();
        let ids: Vec<Uuid> = workspace.sessions.iter().map(Session::id).collect();
        // a reopen appends, like an editor's recent projects, rather than reshuffling the current workspaces
        self.workspaces.push(workspace);
        for id in &ids {
            self.emit_session_created(*id, workspace_id);
        }
        if ids.is_empty() {
            self.schedule_tree_changed();
        }
        if recent.focus_member == Some(true) {
            self.mark_focus_member(workspace_id);
        }
        // an empty workspace has nothing to select, and deselecting would strand the live session
        let selection = recent
            .selected_session_id
            .filter(|id| ids.contains(id))
            .or_else(|| ids.first().copied());
        if selection.is_some() {
            self.reselect_after_removal(selection);
        }
        self.save();
        self.workspace_restore_outcome(recent, occupied_elsewhere)
    }

    /// Undoes every pending close of the workspace (not just the newest: a session left pending would be rebuilt
    /// beside its live original), then merges the snapshot's missing members into the workspace when it is in the
    /// tree, or selects a member when every one of them is already placed. False sends the caller to the rebuild.
    fn restore_or_select_existing_recent_workspace(
        &mut self,
        recent: &RecentClosedWorkspace,
        occupied_elsewhere: &HashSet<Uuid>,
    ) -> bool {
        let workspace_id = recent.snapshot.id;
        let session_ids: HashSet<Uuid> = recent.snapshot.sessions.iter().map(|session| session.id).collect();
        while let Some(pending) = self.pending_close_id_for_workspace(workspace_id, &session_ids) {
            if !self.undo_pending_close(Some(pending)) {
                break;
            }
        }
        if let Some(index) = self.workspace_index(workspace_id) {
            self.merge_recent_workspace(index, recent, occupied_elsewhere);
            return true;
        }
        let placed = self.taken_session_ids(occupied_elsewhere);
        // selecting one overlapping member while another lives only in the snapshot would let the caller consume
        // the entry holding it; decline, so the rebuild restores the remainder
        if !session_ids.iter().all(|id| placed.contains(id)) {
            return false;
        }
        let Some(existing) = self.sessions().map(Session::id).find(|id| session_ids.contains(id)) else {
            return false;
        };
        self.select_session(Some(existing));
        true
    }

    /// Rebuilds the snapshot's members the tree, the pending closes and the other windows do not hold into the
    /// workspace at `index` (a reopened session's shell holds just that one), then selects.
    fn merge_recent_workspace(
        &mut self,
        index: usize,
        recent: &RecentClosedWorkspace,
        occupied_elsewhere: &HashSet<Uuid>,
    ) {
        let taken = self.taken_session_ids(occupied_elsewhere);
        let missing: Vec<Session> = recent
            .snapshot
            .sessions
            .iter()
            .filter(|session| !taken.contains(&session.id))
            .map(|session| session_from_snapshot(session, false))
            .collect();
        let workspace_id = self.workspaces[index].id();
        let ids: Vec<Uuid> = missing.iter().map(Session::id).collect();
        self.workspaces[index].sessions.extend(missing);
        for id in ids {
            self.emit_session_created(id, workspace_id);
        }
        if recent.focus_member == Some(true) {
            self.mark_focus_member(workspace_id);
        }
        let workspace = &self.workspaces[index];
        let target = recent
            .selected_session_id
            .filter(|id| workspace.session(*id).is_some())
            .or_else(|| workspace.sessions.first().map(Session::id));
        if target.is_some() {
            self.select_session(target);
        }
        self.save();
    }

    /// Whether every member of the snapshot now exists somewhere: here, in a pending close here, or in another
    /// window.
    fn workspace_restore_outcome(
        &self,
        recent: &RecentClosedWorkspace,
        occupied_elsewhere: &HashSet<Uuid>,
    ) -> RecentClosedRestore {
        let placed = self.taken_session_ids(occupied_elsewhere);
        RecentClosedRestore {
            restored: true,
            complete: recent
                .snapshot
                .sessions
                .iter()
                .all(|session| placed.contains(&session.id)),
        }
    }

    /// The session ids no restore may rebuild: the tree's, the pending closes', and `occupied_elsewhere`.
    fn taken_session_ids(&self, occupied_elsewhere: &HashSet<Uuid>) -> HashSet<Uuid> {
        let mut taken: HashSet<Uuid> = self.sessions().map(Session::id).collect();
        taken.extend(self.pending_held_session_ids());
        taken.extend(occupied_elsewhere.iter().copied());
        taken
    }
}
