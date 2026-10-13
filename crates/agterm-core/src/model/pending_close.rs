//! Soft close: a closed session or workspace leaves the visible tree but keeps its terminals for a grace window, in
//! which an undo puts it back (upstream's `AppStore+PendingClose.swift`).
//!
//! A soft close moves the sessions, live surfaces included, into a pending record the store owns, emits
//! `session.closed` at once, records the Open Recent entries and asks the frontend to arm the record's grace timer
//! ([`Outbox::pending_closes`](super::Outbox::pending_closes)). It writes no snapshot and drops the window's armed
//! debounced write, so the file keeps the closed sessions until the record resolves. An undo reinserts the same
//! objects where they were, announces them with `session.created`, drops their Open Recent entries and saves. The
//! timer's [`AppStore::finalize_pending_close`] tears their terminals down exactly as a hard close does, drops them
//! from the recency order (kept until then for the undo) and saves; it emits nothing, the close having been announced
//! when it happened, and its Open Recent entries stay.

use std::collections::HashSet;
use std::time::Duration;

use uuid::Uuid;

use super::outbox::WatermarkRemoval;
use super::recent_closed::ClosedSessionPlace;
use super::session::Session;
use super::store::{AppStore, Location};
use super::workspace::Workspace;

/// How long a soft-closed session can be brought back (upstream's `pendingCloseGraceInterval`).
pub const PENDING_CLOSE_GRACE: Duration = Duration::from_secs(3);

/// A soft close's grace timer for the caller to arm: when it fires, [`AppStore::finalize_pending_close`] on
/// `window`'s store with `record`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingCloseTimer {
    pub window: Uuid,
    pub record: Uuid,
    pub grace: Duration,
}

/// What the latest undoable close hid (upstream's `PendingCloseKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingCloseKind {
    Session,
    Sessions,
    Workspace,
}

/// The latest undoable close, for the frontend to present (upstream's `PendingCloseSummary`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingCloseSummary {
    /// The pending-close record, for [`AppStore::undo_pending_close`].
    pub id: Uuid,
    pub kind: PendingCloseKind,
    /// The session's display name, "N sessions", or the workspace's name.
    pub title: String,
}

/// One soft-closed session still inside its undo window, with the workspace it came from (upstream's
/// `PendingCloseMember`). Its panes still own their processes, so an inventory of live panes counts it.
#[derive(Debug, Clone, Copy)]
pub struct PendingCloseMember<'a> {
    pub session: &'a Session,
    pub workspace_id: Uuid,
    pub workspace_name: &'a str,
}

/// One session of a pending session close and where it sat.
#[derive(Debug)]
struct PendingSessionClose {
    session: Session,
    workspace_id: Uuid,
    workspace_name: String,
    workspace_index: usize,
    session_index: usize,
    /// Its Open Recent entry, dropped by an undo.
    recent_id: Uuid,
}

/// One or more sessions closed as one undoable operation.
#[derive(Debug)]
struct PendingSessionsClose {
    /// In tree order.
    sessions: Vec<PendingSessionClose>,
    /// The session an undo selects.
    selected_session_id: Option<Uuid>,
}

/// A closed workspace with every session it held.
#[derive(Debug)]
struct PendingWorkspaceClose {
    workspace: Workspace,
    workspace_index: usize,
    /// The selection an undo restores: the active session when it lived here.
    selected_session_id: Option<Uuid>,
    /// Whether the workspace was in the focus set, so an undo marks it again. The filter's on/off state is not
    /// recorded: it is current window state (see [`AppStore::mark_focus_member`]).
    focus_member: bool,
}

#[derive(Debug)]
enum PendingCloseRecord {
    Sessions(PendingSessionsClose),
    Workspace(PendingWorkspaceClose),
}

impl PendingCloseRecord {
    /// The sessions the record holds, with the workspace each came from.
    fn members(&self) -> Vec<PendingCloseMember<'_>> {
        match self {
            Self::Sessions(close) => close
                .sessions
                .iter()
                .map(|member| PendingCloseMember {
                    session: &member.session,
                    workspace_id: member.workspace_id,
                    workspace_name: &member.workspace_name,
                })
                .collect(),
            Self::Workspace(close) => close
                .workspace
                .sessions
                .iter()
                .map(|session| PendingCloseMember {
                    session,
                    workspace_id: close.workspace.id(),
                    workspace_name: &close.workspace.name,
                })
                .collect(),
        }
    }

    fn holds_session(&self, id: Uuid) -> bool {
        match self {
            Self::Sessions(close) => close.sessions.iter().any(|member| member.session.id() == id),
            Self::Workspace(close) => close.workspace.session(id).is_some(),
        }
    }

    fn session_count(&self) -> usize {
        match self {
            Self::Sessions(close) => close.sessions.len(),
            Self::Workspace(close) => close.workspace.sessions.len(),
        }
    }

    fn into_sessions(self) -> Vec<Session> {
        match self {
            Self::Sessions(close) => close.sessions.into_iter().map(|member| member.session).collect(),
            Self::Workspace(close) => close.workspace.sessions,
        }
    }

    fn summary(&self, id: Uuid) -> PendingCloseSummary {
        let (kind, title) = match self {
            Self::Sessions(close) => match close.sessions.as_slice() {
                [only] => (PendingCloseKind::Session, only.session.display_name()),
                sessions => (PendingCloseKind::Sessions, format!("{} sessions", sessions.len())),
            },
            Self::Workspace(close) => (PendingCloseKind::Workspace, close.workspace.name.clone()),
        };
        PendingCloseSummary { id, kind, title }
    }
}

/// A store's undoable closes, oldest first (upstream's `pendingCloseRecords` with `pendingCloseOrder`).
#[derive(Debug, Default)]
pub(crate) struct PendingCloses {
    records: Vec<(Uuid, PendingCloseRecord)>,
}

impl PendingCloses {
    fn push(&mut self, id: Uuid, record: PendingCloseRecord) {
        self.records.push((id, record));
    }

    fn take(&mut self, id: Uuid) -> Option<PendingCloseRecord> {
        let index = self.records.iter().position(|(candidate, _)| *candidate == id)?;
        Some(self.records.remove(index).1)
    }

    fn latest(&self) -> Option<Uuid> {
        self.records.last().map(|(id, _)| *id)
    }

    fn ids(&self) -> Vec<Uuid> {
        self.records.iter().map(|(id, _)| *id).collect()
    }

    /// The newest record matching `matches`.
    fn newest_where(&self, matches: impl Fn(&PendingCloseRecord) -> bool) -> Option<Uuid> {
        self.records
            .iter()
            .rev()
            .find(|(_, record)| matches(record))
            .map(|(id, _)| *id)
    }
}

impl Session {
    /// Drops what a launch restore armed for the surface factory (upstream's `clearPendingRestoreOverrides`). An undo
    /// reinserts this very object, so an override armed at launch and never consumed would otherwise fire when the
    /// restored surface is built; the persisted override stays and fires on the next launch.
    pub fn clear_pending_restore_overrides(&mut self) {
        self.restore.pending_command = None;
        self.restore.pending_foreground = None;
        self.split_restore.pending_command = None;
        self.split_restore.pending_foreground = None;
    }
}

impl AppStore {
    // MARK: - Reads

    /// Every session an undoable close holds, oldest close first.
    #[must_use]
    pub fn pending_close_members(&self) -> Vec<PendingCloseMember<'_>> {
        self.pending
            .records
            .iter()
            .flat_map(|(_, record)| record.members())
            .collect()
    }

    /// The live object of a session an undoable close holds; `None` for a visible or finalized one.
    #[must_use]
    pub fn pending_close_session(&self, id: Uuid) -> Option<&Session> {
        self.pending_close_members()
            .into_iter()
            .find(|member| member.session.id() == id)
            .map(|member| member.session)
    }

    /// [`AppStore::pending_close_session`] for its surface slots.
    pub fn pending_close_session_mut(&mut self, id: Uuid) -> Option<&mut Session> {
        self.pending
            .records
            .iter_mut()
            .flat_map(|(_, record)| -> Box<dyn Iterator<Item = &mut Session> + '_> {
                match record {
                    PendingCloseRecord::Sessions(close) => {
                        Box::new(close.sessions.iter_mut().map(|member| &mut member.session))
                    }
                    PendingCloseRecord::Workspace(close) => Box::new(close.workspace.sessions.iter_mut()),
                }
            })
            .find(|session| session.id() == id)
    }

    /// The latest undoable close, which a bare undo restores; `None` when nothing can be undone.
    #[must_use]
    pub fn pending_close_summary(&self) -> Option<PendingCloseSummary> {
        self.pending.records.last().map(|(id, record)| record.summary(*id))
    }

    /// Whether a pending close holds workspace `id`: the workspace itself, or a session closed from it, whose undo
    /// would put the workspace back.
    #[must_use]
    pub fn pending_holds_workspace(&self, id: Uuid) -> bool {
        self.pending.records.iter().any(|(_, record)| match record {
            PendingCloseRecord::Sessions(close) => close.sessions.iter().any(|member| member.workspace_id == id),
            PendingCloseRecord::Workspace(close) => close.workspace.id() == id,
        })
    }

    /// The sessions a pending close still holds: absent from the tree, their terminals alive, and put back by an
    /// undo. A restore unions them with the tree's ids so it never builds a second object under one id.
    #[must_use]
    pub fn pending_held_session_ids(&self) -> HashSet<Uuid> {
        self.pending_close_members()
            .into_iter()
            .map(|member| member.session.id())
            .collect()
    }

    /// The newest pending close holding session `id`.
    pub(crate) fn pending_close_id_containing_session(&self, id: Uuid) -> Option<Uuid> {
        self.pending.newest_where(|record| record.holds_session(id))
    }

    /// The newest pending close a restore of workspace `workspace` consumes: the workspace's own close, or a session
    /// close of one of `sessions` made FROM that workspace. A session moved to another workspace and closed there was
    /// a deliberate close, so that one is left alone.
    pub(crate) fn pending_close_id_for_workspace(&self, workspace: Uuid, sessions: &HashSet<Uuid>) -> Option<Uuid> {
        self.pending.newest_where(|record| match record {
            PendingCloseRecord::Workspace(close) => close.workspace.id() == workspace,
            PendingCloseRecord::Sessions(close) => close
                .sessions
                .iter()
                .any(|member| member.workspace_id == workspace && sessions.contains(&member.session.id())),
        })
    }

    /// A workspace to stand in for one a restore needs but the tree no longer holds, described as newly as possible:
    /// a pending close of that workspace, else its Open Recent entry, else `name`, the caller's older copy.
    pub(crate) fn rebuilt_workspace_shell(&self, id: Uuid, name: &str) -> Workspace {
        let pending = self.pending.records.iter().rev().find_map(|(_, record)| match record {
            PendingCloseRecord::Workspace(close) if close.workspace.id() == id => {
                Some((close.workspace.name.clone(), close.workspace.is_expanded))
            }
            _ => None,
        });
        let (name, expanded) = pending
            .or_else(|| self.recent_closed.workspace_shell(id))
            .unwrap_or_else(|| (name.to_owned(), true));
        let mut shell = Workspace::with_id(id, name);
        shell.is_expanded = expanded;
        shell
    }

    /// The index of workspace `id`, a shell rebuilt at `index` (clamped) when the tree lost it.
    pub(crate) fn workspace_index_or_shell(&mut self, id: Uuid, index: usize, name: &str) -> usize {
        if let Some(existing) = self.workspace_index(id) {
            return existing;
        }
        let at = index.min(self.workspaces.len());
        let shell = self.rebuilt_workspace_shell(id, name);
        self.workspaces.insert(at, shell);
        at
    }

    // MARK: - Closing

    /// Drops the window's pending debounced write without writing (upstream's `cancelPendingSave`); an immediate
    /// write asked for earlier in the same mutation still lands.
    pub fn cancel_pending_save(&mut self) {
        self.outbox.cancel_debounced();
    }

    /// Hides a session from the tree, keeping its terminals for [`PENDING_CLOSE_GRACE`]: an undo restores it, the
    /// grace timer makes it final. When it was selected, the next selection is the hard close's. False when no
    /// session matches.
    pub fn soft_close_session(&mut self, id: Uuid) -> bool {
        let Some(at) = self.location(id) else {
            return false;
        };
        let was_active = self.selected_session_id == Some(id);
        self.release_leaving_session(id);
        self.close_timed_hud(id);
        let workspace_id = self.workspaces[at.workspace].id();
        let workspace_name = self.workspaces[at.workspace].name.clone();
        let mut session = self.workspaces[at.workspace].sessions.remove(at.session);
        self.emit_session_closed(&session, workspace_id);
        session.clear_pending_restore_overrides();
        let close_id = Uuid::new_v4();
        let place = ClosedSessionPlace {
            workspace_id,
            workspace_name: &workspace_name,
            workspace_index: at.workspace,
            session_index: at.session,
        };
        self.record_recent_closed_session(&session, &place, close_id);
        let close = PendingSessionClose {
            session,
            workspace_id,
            workspace_name,
            workspace_index: at.workspace,
            session_index: at.session,
            recent_id: close_id,
        };
        self.pending.push(
            close_id,
            PendingCloseRecord::Sessions(PendingSessionsClose {
                sessions: vec![close],
                selected_session_id: Some(id),
            }),
        );
        if was_active {
            let target = self.close_reselection_target(at);
            self.reselect_after_removal(target);
        } else {
            self.prune_sidebar_selection();
        }
        self.arm_pending_close(close_id);
        true
    }

    /// Hides several sessions as one undoable close: one grace timer, one undo restoring all of them. One distinct
    /// id is [`AppStore::soft_close_session`]. False when none of them exists.
    pub fn soft_close_sessions(&mut self, ids: &[Uuid]) -> bool {
        let targets: HashSet<Uuid> = ids.iter().copied().collect();
        if targets.len() < 2 {
            return ids.first().is_some_and(|id| self.soft_close_session(*id));
        }
        let previous = self.selected_session_id;
        for id in &targets {
            if self.location(*id).is_some() {
                self.release_leaving_session(*id);
            }
            self.close_timed_hud(*id);
        }
        let closes = self.take_closing_sessions(&targets);
        if closes.is_empty() {
            return false;
        }
        for close in &closes {
            let place = ClosedSessionPlace {
                workspace_id: close.workspace_id,
                workspace_name: &close.workspace_name,
                workspace_index: close.workspace_index,
                session_index: close.session_index,
            };
            self.record_recent_closed_session(&close.session, &place, close.recent_id);
            self.emit_session_closed(&close.session, close.workspace_id);
        }
        let removing_active = previous.is_some_and(|id| targets.contains(&id));
        if removing_active {
            let target = self.batch_close_target(previous, &closes);
            self.reselect_after_removal(target);
        } else {
            self.prune_sidebar_selection();
        }
        let selected_session_id = if removing_active {
            previous
        } else {
            closes.first().map(|close| close.session.id())
        };
        let close_id = Uuid::new_v4();
        self.pending.push(
            close_id,
            PendingCloseRecord::Sessions(PendingSessionsClose {
                sessions: closes,
                selected_session_id,
            }),
        );
        self.arm_pending_close(close_id);
        true
    }

    /// Takes the `targets` out of the tree, last first so the recorded indices stay the pre-close ones, and returns
    /// them in tree order.
    fn take_closing_sessions(&mut self, targets: &HashSet<Uuid>) -> Vec<PendingSessionClose> {
        let places: Vec<(Location, Uuid, String)> = self
            .workspaces
            .iter()
            .enumerate()
            .flat_map(|(workspace, entry)| {
                entry
                    .sessions
                    .iter()
                    .enumerate()
                    .filter(|(_, session)| targets.contains(&session.id()))
                    .map(move |(session, _)| (Location { workspace, session }, entry.id(), entry.name.clone()))
            })
            .collect();
        let mut closes: Vec<PendingSessionClose> = places
            .into_iter()
            .rev()
            .map(|(at, workspace_id, workspace_name)| {
                let mut session = self.workspaces[at.workspace].sessions.remove(at.session);
                session.clear_pending_restore_overrides();
                PendingSessionClose {
                    session,
                    workspace_id,
                    workspace_name,
                    workspace_index: at.workspace,
                    session_index: at.session,
                    recent_id: Uuid::new_v4(),
                }
            })
            .collect();
        closes.reverse();
        closes
    }

    /// The selection after a batch closed the active session: the hard close's pick at the active session's slot,
    /// shifted up by the batch members removed before it in its workspace; else the first session left.
    fn batch_close_target(&self, previous: Option<Uuid>, closes: &[PendingSessionClose]) -> Option<Uuid> {
        let active = previous.and_then(|id| closes.iter().find(|close| close.session.id() == id));
        active
            .and_then(|close| {
                let removed_before = closes
                    .iter()
                    .filter(|other| {
                        other.workspace_index == close.workspace_index && other.session_index < close.session_index
                    })
                    .count();
                self.close_reselection_target(Location {
                    workspace: close.workspace_index,
                    session: close.session_index - removed_before,
                })
            })
            .or_else(|| {
                self.workspaces
                    .iter()
                    .find_map(|workspace| workspace.sessions.first())
                    .map(Session::id)
            })
    }

    /// Hides a workspace and every session in it, keeping their terminals for [`PENDING_CLOSE_GRACE`]. The last
    /// workspace cannot be closed, as with [`AppStore::remove_workspace`]. False when it is the last or unknown.
    pub fn soft_remove_workspace(&mut self, id: Uuid) -> bool {
        if !self.can_remove_workspace() {
            return false;
        }
        let Some(index) = self.workspace_index(id) else {
            return false;
        };
        let leaving: Vec<Uuid> = self.workspaces[index].sessions.iter().map(Session::id).collect();
        for session in leaving {
            self.release_leaving_session(session);
        }
        let visible = self.workspaces.remove(index);
        self.forget_fresh_workspace(id);
        for session in &visible.sessions {
            self.emit_session_closed(session, id);
        }
        if visible.sessions.is_empty() {
            self.schedule_tree_changed();
        }
        let (mut workspace, folded_member) = self.folding_pending_closes(visible);
        for session in &mut workspace.sessions {
            session.clear_pending_restore_overrides();
        }
        let selected = self
            .selected_session_id
            .filter(|selected| workspace.session(*selected).is_some());
        // a folded record's membership is the only copy left: its own close dropped it, and the session undo that
        // rebuilt this workspace as a shell does not mark it again
        let focus_member = self.focus.ids.contains(&id) || folded_member;
        self.drop_focus_member(id);
        if selected.is_some() {
            let target = self.workspace_removal_target(index);
            self.reselect_after_removal(target);
        } else {
            self.prune_sidebar_selection();
        }
        let close_id = Uuid::new_v4();
        self.record_recent_closed_workspace(&workspace, selected, focus_member, close_id);
        self.pending.push(
            close_id,
            PendingCloseRecord::Workspace(PendingWorkspaceClose {
                workspace,
                workspace_index: index,
                selected_session_id: selected,
                focus_member,
            }),
        );
        self.arm_pending_close(close_id);
        true
    }

    /// Absorbs every pending close of the same workspace into the copy closing now, dropping the superseded records.
    /// An undone session close rebuilds a missing workspace as a shell, and closing that shell during the earlier
    /// record's grace would leave two records sharing one workspace id and one Open Recent entry, which the newer
    /// snapshot overwrites. The copy closing now leads; the superseded records' sessions follow. Returns whether any
    /// absorbed record was a focus-set member.
    fn folding_pending_closes(&mut self, workspace: Workspace) -> (Workspace, bool) {
        let mut folded = workspace;
        let mut focus_member = false;
        for id in self.pending.ids() {
            let absorbs = matches!(
                self.pending.records.iter().find(|(candidate, _)| *candidate == id),
                Some((_, PendingCloseRecord::Workspace(close))) if close.workspace.id() == folded.id()
            );
            if !absorbs {
                continue;
            }
            let Some(PendingCloseRecord::Workspace(close)) = self.pending.take(id) else {
                continue;
            };
            let present: HashSet<Uuid> = folded.sessions.iter().map(Session::id).collect();
            let (absorbed, duplicates): (Vec<Session>, Vec<Session>) = close
                .workspace
                .sessions
                .into_iter()
                .partition(|session| !present.contains(&session.id()));
            discard(duplicates);
            folded.sessions.extend(absorbed);
            focus_member |= close.focus_member;
        }
        (folded, focus_member)
    }

    /// Hands a new record's grace timer to the frontend and keeps the file holding the closed sessions.
    fn arm_pending_close(&mut self, id: Uuid) {
        self.outbox.pending_closes.push((id, PENDING_CLOSE_GRACE));
        self.cancel_pending_save();
    }

    // MARK: - Undo

    /// Brings back the latest pending close, or the record `id`, and saves. Whether anything came back.
    pub fn undo_pending_close(&mut self, id: Option<Uuid>) -> bool {
        self.undo_pending_close_selecting(id, None)
    }

    /// [`AppStore::undo_pending_close`] selecting `selecting` afterwards when the record held it (a reopen of one
    /// member of a batch close).
    pub fn undo_pending_close_selecting(&mut self, id: Option<Uuid>, selecting: Option<Uuid>) -> bool {
        let Some(close_id) = id.or_else(|| self.pending.latest()) else {
            return false;
        };
        let Some(record) = self.pending.take(close_id) else {
            return false;
        };
        match record {
            PendingCloseRecord::Sessions(close) => {
                let recent: Vec<Uuid> = close.sessions.iter().map(|member| member.recent_id).collect();
                self.restore_pending_sessions(close, selecting);
                for id in recent {
                    self.remove_recent_closed_item(id);
                }
            }
            PendingCloseRecord::Workspace(close) => {
                self.restore_pending_workspace(close);
                self.remove_recent_closed_item(close_id);
            }
        }
        self.save();
        true
    }

    fn restore_pending_sessions(&mut self, close: PendingSessionsClose, requested: Option<Uuid>) {
        let ids: Vec<Uuid> = close.sessions.iter().map(|member| member.session.id()).collect();
        let held = |id: Option<Uuid>| id.filter(|id| ids.contains(id));
        let target = held(requested)
            .or_else(|| held(close.selected_session_id))
            .or_else(|| ids.first().copied());
        for member in close.sessions {
            self.restore_pending_session(member);
        }
        if target.is_some() {
            self.reselect_after_removal(target);
        }
    }

    /// Puts one session back at its recorded place, rebuilding its workspace as a shell when the tree lost it.
    fn restore_pending_session(&mut self, close: PendingSessionClose) {
        let workspace_index =
            self.workspace_index_or_shell(close.workspace_id, close.workspace_index, &close.workspace_name);
        let id = close.session.id();
        let sessions = &mut self.workspaces[workspace_index].sessions;
        let at = close.session_index.min(sessions.len());
        sessions.insert(at, close.session);
        self.emit_session_created(id, close.workspace_id);
    }

    /// Puts a workspace back. A shell a session undo or an Open Recent reopen rebuilt under its id is merged into
    /// instead, keeping the shell's name, expansion and slot (anything changed on it since is newer); a second
    /// workspace under one id would strand its sessions behind the first.
    fn restore_pending_workspace(&mut self, close: PendingWorkspaceClose) {
        let PendingWorkspaceClose {
            workspace,
            workspace_index,
            selected_session_id,
            focus_member,
        } = close;
        let workspace_id = workspace.id();
        let first = workspace.sessions.first().map(Session::id);
        if let Some(existing) = self.workspace_index(workspace_id) {
            let live: HashSet<Uuid> = self.sessions().map(Session::id).collect();
            let (restored, duplicates): (Vec<Session>, Vec<Session>) = workspace
                .sessions
                .into_iter()
                .partition(|session| !live.contains(&session.id()));
            discard(duplicates);
            let ids: Vec<Uuid> = restored.iter().map(Session::id).collect();
            self.workspaces[existing].sessions.extend(restored);
            for id in ids {
                self.emit_session_created(id, workspace_id);
            }
        } else {
            let at = workspace_index.min(self.workspaces.len());
            let ids: Vec<Uuid> = workspace.sessions.iter().map(Session::id).collect();
            self.workspaces.insert(at, workspace);
            for id in &ids {
                self.emit_session_created(*id, workspace_id);
            }
            if ids.is_empty() {
                self.schedule_tree_changed();
            }
        }
        // before the reselect, so a restored member is inside the set when the focus check runs, and before the
        // early return, so an empty workspace's row is not left filtered out
        if focus_member {
            self.mark_focus_member(workspace_id);
        }
        let Some(target) = selected_session_id.or(first) else {
            return;
        };
        self.reselect_after_removal(Some(target));
    }

    // MARK: - Finalizing

    /// Makes the record `record` final: its sessions' terminals are torn down and they leave the recency order; then
    /// saves. A record already undone or finalized is a no-op, so a late timer is harmless.
    pub fn finalize_pending_close(&mut self, record: Uuid) {
        let Some(record) = self.pending.take(record) else {
            return;
        };
        self.hard_finalize_pending_sessions(record.into_sessions());
        self.save();
    }

    /// Makes every pending close final (quit, window close).
    pub fn finalize_all_pending_closes(&mut self) {
        for id in self.pending.ids() {
            self.finalize_pending_close(id);
        }
    }

    /// Makes one held session's close final, leaving the rest of its record and every other record undoable. False
    /// when no pending close holds the session.
    pub fn finalize_pending_close_of_session(&mut self, session: Uuid) -> bool {
        let Some(index) = self
            .pending
            .records
            .iter()
            .position(|(_, record)| record.holds_session(session))
        else {
            return false;
        };
        let (record_id, record) = &mut self.pending.records[index];
        if record.session_count() < 2 {
            let record_id = *record_id;
            self.finalize_pending_close(record_id);
            return true;
        }
        let removed = match record {
            PendingCloseRecord::Sessions(close) => take_batch_member(close, session),
            PendingCloseRecord::Workspace(close) => {
                let position = close.workspace.sessions.iter().position(|held| held.id() == session);
                if close.selected_session_id == Some(session) {
                    close.selected_session_id = None;
                }
                position.map(|position| close.workspace.sessions.remove(position))
            }
        };
        self.hard_finalize_pending_sessions(removed.into_iter().collect());
        self.save();
        true
    }

    /// Tears the sessions' terminals down, drops them from the recency order and deletes their rendered text, as a
    /// hard close does.
    fn hard_finalize_pending_sessions(&mut self, sessions: Vec<Session>) {
        for mut session in sessions {
            session.teardown_surfaces();
            self.outbox
                .removed_watermarks
                .push(WatermarkRemoval::Session(session.id()));
            self.session_recency.remove(session.id());
        }
    }
}

/// Takes `session` out of a batch close; a member closed after it in the same workspace was recorded one slot too
/// far down, which its undo would otherwise keep.
fn take_batch_member(close: &mut PendingSessionsClose, session: Uuid) -> Option<Session> {
    let position = close
        .sessions
        .iter()
        .position(|member| member.session.id() == session)?;
    let member = close.sessions.remove(position);
    for other in &mut close.sessions {
        if other.workspace_id == member.workspace_id && other.session_index > member.session_index {
            other.session_index -= 1;
        }
    }
    if close.selected_session_id == Some(session) {
        close.selected_session_id = None;
    }
    Some(member.session)
}

/// Frees sessions a restore found already in the tree: a second object under one id must not keep terminals alive.
fn discard(sessions: Vec<Session>) {
    for mut session in sessions {
        session.teardown_surfaces();
    }
}
