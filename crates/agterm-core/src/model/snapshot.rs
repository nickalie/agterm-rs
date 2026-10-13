//! The model ↔ snapshot boundary (upstream's `AppStore+Snapshot.swift` and `AppStore.restore(from:launchRestore:)`):
//! building a [`Snapshot`] from a window's live tree, and rebuilding the tree from one.
//!
//! `launch_restore` marks an app-bootstrap restore, the only path that arms anything executable: the captured
//! foreground commands and the sticky `session.restore` override go to each pane's transient pending slots, which a
//! snapshot never serializes. Every other rebuild (reopening a closed window mid-run) arms nothing.

use std::collections::HashSet;

use uuid::Uuid;

use super::background::PaneBackgrounds;
use super::pane::{SplitAxis, clamp_split_ratio};
use super::recency::{RECENCY_LIMIT, RecencyStack};
use super::session::{Session, SessionSeed};
use super::store::{AppStore, SIDEBAR_WIDTH_DEFAULT, clamp_sidebar_width};
use super::workspace::{SidebarMode, Workspace};
use crate::persist::{SessionSnapshot, Snapshot, WorkspaceSnapshot};

impl AppStore {
    /// The window's tree as persisted: remote sessions are left out, the focus set in tree order (both focus keys
    /// omitted while nothing is marked), the recency list without the sessions left out.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        let workspaces: Vec<WorkspaceSnapshot> = self.workspaces.iter().map(workspace_snapshot).collect();
        let focus_ids: Vec<Uuid> = self
            .workspaces
            .iter()
            .map(Workspace::id)
            .filter(|id| self.focus.ids.contains(id))
            .collect();
        let persistable: HashSet<Uuid> = workspaces
            .iter()
            .flat_map(|workspace| workspace.sessions.iter().map(|session| session.id))
            .collect();
        let recency: Vec<Uuid> = self
            .session_recency
            .items()
            .iter()
            .copied()
            .filter(|id| persistable.contains(id))
            .collect();
        Snapshot {
            selected_session_id: self.persisted_selection(&persistable, &recency),
            workspaces,
            sidebar_width: Some(self.sidebar.width),
            sidebar_visible: Some(self.sidebar.visible),
            sidebar_mode: Some(self.sidebar.mode),
            focused_workspace_ids: (!focus_ids.is_empty()).then_some(focus_ids),
            focus_enabled: self.focus.enabled.then_some(true),
            session_recency: Some(recency),
            ..Snapshot::default()
        }
    }

    /// The selection to write. A remote session is not in the snapshot, so naming it would restore an empty window
    /// beside local rows: fall back to the most recent persisted session, then the first.
    fn persisted_selection(&self, persistable: &HashSet<Uuid>, recency: &[Uuid]) -> Option<Uuid> {
        let selected = self.selected_session_id?;
        if persistable.contains(&selected) {
            return Some(selected);
        }
        recency
            .first()
            .copied()
            .or_else(|| self.sessions().map(Session::id).find(|id| persistable.contains(id)))
    }

    /// Rebuilds the tree from `snapshot`, replacing the current state wholesale: fresh sessions keyed by the
    /// persisted ids (their terminals spawn on first display), duplicate workspace ids folded into the first, a
    /// repeated session id kept once, the sidebar width clamped, stale focus members pruned, a selection naming no
    /// session cleared, the recency order re-seeded with the selection in front, and a selection the restored focus
    /// filter hides moved into view. A reload is not a selection, so it emits no `session.selected`, and it asks for
    /// no save of its own: only the closing repair of a hidden selection writes.
    pub fn restore(&mut self, snapshot: &Snapshot, launch_restore: bool) {
        self.restoring_selection = true;
        self.fresh_workspace_id = None;
        for session in self
            .workspaces
            .iter_mut()
            .flat_map(|workspace| workspace.sessions.iter_mut())
        {
            session.teardown_surfaces();
        }
        self.workspaces = restored_workspaces(snapshot, launch_restore);
        self.sidebar.width = clamp_sidebar_width(snapshot.sidebar_width.unwrap_or(SIDEBAR_WIDTH_DEFAULT));
        self.sidebar.visible = snapshot.sidebar_visible.unwrap_or(true);
        self.sidebar.mode = snapshot.sidebar_mode.unwrap_or(SidebarMode::Tree);
        self.restore_focus(snapshot);
        let selected = snapshot.selected_session_id.filter(|id| self.session(*id).is_some());
        self.selected_session_id = selected;
        self.replace_sidebar_selection(selected);
        let restored: HashSet<Uuid> = self.sessions().map(Session::id).collect();
        let seed = snapshot
            .session_recency
            .iter()
            .flatten()
            .copied()
            .filter(|id| restored.contains(id));
        self.session_recency = RecencyStack::new(RECENCY_LIMIT, seed);
        self.record_recency();
        self.reselect_if_selection_hidden();
        self.restoring_selection = false;
    }

    /// The persisted focus set, pruned to the restored workspaces; enabled only with a member left.
    fn restore_focus(&mut self, snapshot: &Snapshot) {
        let present: HashSet<Uuid> = self.workspaces.iter().map(Workspace::id).collect();
        self.focus.ids = snapshot
            .focused_workspace_ids
            .iter()
            .flatten()
            .copied()
            .filter(|id| present.contains(id))
            .collect();
        self.focus.enabled = snapshot.focus_enabled.unwrap_or(false) && !self.focus.ids.is_empty();
    }
}

fn restored_workspaces(snapshot: &Snapshot, launch_restore: bool) -> Vec<Workspace> {
    let mut seen: HashSet<Uuid> = HashSet::new();
    let mut restored: Vec<Workspace> = Vec::new();
    for persisted in &snapshot.workspaces {
        let sessions: Vec<Session> = persisted
            .sessions
            .iter()
            .filter(|session| seen.insert(session.id))
            .map(|session| session_from_snapshot(session, launch_restore))
            .collect();
        if let Some(existing) = restored.iter_mut().find(|workspace| workspace.id() == persisted.id) {
            existing.sessions.extend(sessions);
            continue;
        }
        let mut workspace = Workspace::with_id(persisted.id, persisted.name.clone());
        workspace.sessions = sessions;
        workspace.is_expanded = !persisted.collapsed.unwrap_or(false);
        restored.push(workspace);
    }
    restored
}

/// One workspace as persisted: its local sessions, and the collapsed flag only when collapsed, so an all-expanded
/// tree writes what a legacy snapshot held.
#[must_use]
pub fn workspace_snapshot(workspace: &Workspace) -> WorkspaceSnapshot {
    WorkspaceSnapshot {
        id: workspace.id(),
        name: workspace.name.clone(),
        sessions: workspace
            .sessions
            .iter()
            .filter(|session| session.is_persistable())
            .map(session_snapshot)
            .collect(),
        collapsed: (!workspace.is_expanded).then_some(true),
    }
}

/// One session as persisted. The pending restore slots are never written; `isSplit` and `flagged` always are, the
/// split fields only while a split exists, the hold flags only when set.
#[must_use]
pub fn session_snapshot(session: &Session) -> SessionSnapshot {
    let exists = session.split.exists;
    SessionSnapshot {
        id: session.id(),
        pane_identity: Some(session.pane_identity),
        split_pane_identity: session.split_pane_identity.filter(|_| exists),
        custom_name: session.custom_name.clone(),
        cwd: session.effective_cwd().to_owned(),
        is_split: Some(session.split.shown),
        has_split: exists.then_some(true),
        split_axis: exists.then_some(session.split.axis),
        font_size: session.font_size,
        split_cwd: session.split_cwd.clone().or_else(|| session.initial_split_cwd.clone()),
        split_ratio: session.split.ratio,
        flagged: Some(session.flagged),
        foreground_command: session.restore.foreground.clone(),
        split_foreground_command: session.split_restore.foreground.clone(),
        initial_command: session.launch.command.clone(),
        command_wait: session.launch.wait.then_some(true),
        split_initial_command: session.split_launch.command.clone(),
        split_command_wait: session.split_launch.wait.then_some(true),
        background_watermark: session.background_watermark.clone(),
        pane_backgrounds: session.pane_backgrounds.persisted(),
        restore_command: session.restore.command.clone(),
        split_restore_command: session.split_restore.command.clone(),
        context: session.context.clone(),
    }
}

/// Rebuilds one session. A hidden split keeps its identity and restore state, so showing it reattaches the surviving
/// daemon; focus is not persisted and returns to the primary pane. Missing pane identities (a legacy snapshot) are
/// minted.
#[must_use]
pub fn session_from_snapshot(snapshot: &SessionSnapshot, launch_restore: bool) -> Session {
    let has_split = snapshot.has_any_split();
    let mut session = Session::new(SessionSeed {
        id: Some(snapshot.id),
        initial_cwd: snapshot.cwd.clone(),
        custom_name: snapshot.custom_name.clone(),
        pane_identity: Some(snapshot.pane_identity.unwrap_or_else(Uuid::new_v4)),
        split_pane_identity: has_split.then(|| snapshot.split_pane_identity.unwrap_or_else(Uuid::new_v4)),
        remote_host: None,
    });
    session.split.shown = snapshot.is_split.unwrap_or(false);
    session.split.exists = has_split;
    session.split.axis = if has_split {
        snapshot.split_axis.unwrap_or_default()
    } else {
        SplitAxis::LeftRight
    };
    session.split.ratio = snapshot.split_ratio.map(clamp_split_ratio);
    session.font_size = snapshot.font_size;
    session.initial_split_cwd.clone_from(&snapshot.split_cwd);
    session.flagged = snapshot.flagged.unwrap_or(false);
    session.context.clone_from(&snapshot.context);
    session.launch.command.clone_from(&snapshot.initial_command);
    session.launch.wait = snapshot.command_wait.unwrap_or(false);
    if has_split {
        session.split_launch.command.clone_from(&snapshot.split_initial_command);
        session.split_launch.wait = snapshot.split_command_wait.unwrap_or(false);
        session
            .split_restore
            .command
            .clone_from(&snapshot.split_restore_command);
    }
    session.was_restored = true;
    session.background_watermark.clone_from(&snapshot.background_watermark);
    let panes = snapshot.pane_backgrounds.as_ref();
    session.pane_backgrounds = PaneBackgrounds {
        left: panes.and_then(|panes| panes.left.clone()),
        right: panes.and_then(|panes| panes.right.clone()).filter(|_| has_split),
        scratch: None,
    };
    session.restore.command.clone_from(&snapshot.restore_command);
    if launch_restore {
        session
            .restore
            .pending_foreground
            .clone_from(&snapshot.foreground_command);
        session.restore.pending_command.clone_from(&snapshot.restore_command);
        if has_split {
            session
                .split_restore
                .pending_foreground
                .clone_from(&snapshot.split_foreground_command);
            session
                .split_restore
                .pending_command
                .clone_from(&session.split_restore.command);
        }
    }
    session
}
