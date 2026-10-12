//! The sidebar focus filter, the visible sets derived from it and the sidebar mode, and the sidebar
//! multi-selection (upstream's `AppStore+Focus.swift`, `AppStore+SidebarSelection.swift`).
//!
//! One invariant runs through all of it: the active session stays inside the visible set. An explicit select
//! outside it suspends the filter (keeping the marked set); a narrowing that hides the active session moves the
//! selection back in.

use uuid::Uuid;

use super::session::Session;
use super::store::AppStore;
use super::workspace::{SidebarMode, Workspace};

/// One `workspace.focus` mode, applied to a workspace by [`AppStore::apply_focus_mode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FocusMode {
    /// Zoom the tree to just this workspace.
    On,
    /// Unmark it.
    Off,
    /// Mark it, leaving the filter's on/off state alone.
    Add,
    /// Clear the filter when this is the one workspace zoomed to, else zoom to it.
    Toggle,
}

impl AppStore {
    /// Suspends the filter, keeping the marked set, when the newly selected session lies outside it, so an explicit
    /// cross-set select reveals its target. A no-op with the filter off, nothing selected, a selection inside the set,
    /// or in flagged mode, which ignores the marked set.
    pub(crate) fn disable_focus_if_selection_outside_set(&mut self, session: Option<Uuid>) {
        if !self.focus.enabled || self.sidebar.mode == SidebarMode::Flagged {
            return;
        }
        let Some(session) = session else { return };
        if let Some(owner) = self.workspace_for_session(session).map(Workspace::id)
            && self.focus.ids.contains(&owner)
        {
            return;
        }
        self.focus.enabled = false;
    }

    /// Zooms the tree to `id` alone: replaces the marked set with it and enables the filter. A no-op for an id naming
    /// no workspace.
    pub fn set_focused_workspace(&mut self, id: Uuid) {
        if self.workspace(id).is_none() {
            return;
        }
        self.commit_focus([id].into(), true);
    }

    /// Empties the marked set and switches the filter off.
    pub fn clear_focus(&mut self) {
        self.commit_focus(std::collections::BTreeSet::new(), false);
    }

    /// Clears the filter when `id` is the one workspace zoomed to, else zooms to it.
    pub fn toggle_focused_workspace(&mut self, id: Uuid) {
        if self.is_sole_focus(id) {
            self.clear_focus();
        } else {
            self.set_focused_workspace(id);
        }
    }

    /// The one workspace the tree is zoomed to: the sole marked one while the filter applies.
    #[must_use]
    pub fn sole_focused_workspace_id(&self) -> Option<Uuid> {
        if !self.focus.enabled || self.focus.ids.len() != 1 {
            return None;
        }
        self.focus.ids.first().copied()
    }

    #[must_use]
    pub fn is_sole_focus(&self, id: Uuid) -> bool {
        self.sole_focused_workspace_id() == Some(id)
    }

    /// [`AppStore::is_sole_focus`] for the current workspace.
    #[must_use]
    pub fn is_current_workspace_sole_focus(&self) -> bool {
        self.current_workspace_id().is_some_and(|id| self.is_sole_focus(id))
    }

    /// Whether the current workspace is marked.
    #[must_use]
    pub fn is_current_workspace_focus_member(&self) -> bool {
        self.current_workspace_id()
            .is_some_and(|id| self.focus.ids.contains(&id))
    }

    /// Applies one `workspace.focus` mode to `id`; each is idempotent.
    pub fn apply_focus_mode(&mut self, mode: FocusMode, id: Uuid) {
        match mode {
            FocusMode::On => self.set_focused_workspace(id),
            FocusMode::Off => self.set_focus_membership(id, false),
            FocusMode::Add => self.set_focus_membership(id, true),
            FocusMode::Toggle => self.toggle_focused_workspace(id),
        }
    }

    /// Marks or unmarks one workspace. Marking only marks, so a working set is built row by row with the whole tree on
    /// screen; unmarking the last member switches the filter off. Marking an id naming no workspace is refused;
    /// unmarking is not, so a stale id stays removable.
    pub fn set_focus_membership(&mut self, id: Uuid, member: bool) {
        if member && self.workspace(id).is_none() {
            return;
        }
        let mut ids = self.focus.ids.clone();
        if member {
            ids.insert(id);
        } else {
            ids.remove(&id);
        }
        self.commit_focus(ids, self.focus.enabled);
    }

    /// Turns the filter on or off without touching the marked set; enabling an empty set is refused.
    pub fn set_focus_enabled(&mut self, on: bool) {
        self.commit_focus(self.focus.ids.clone(), on);
    }

    /// The one write point of the filter: an empty set forces it off, an unchanged write is a no-op, and a change
    /// re-checks the fresh workspace, the sidebar selection and the active session's visibility, then saves.
    fn commit_focus(&mut self, ids: std::collections::BTreeSet<Uuid>, enabled: bool) {
        let enabled = enabled && !ids.is_empty();
        if self.focus.ids == ids && self.focus.enabled == enabled {
            return;
        }
        self.focus.ids = ids;
        self.focus.enabled = enabled;
        self.forget_hidden_fresh_workspace();
        self.prune_sidebar_selection();
        self.reselect_if_selection_hidden();
        self.save();
    }

    /// Drops the fresh workspace when the filter has hidden it, so turning the filter off later cannot make it current
    /// again.
    fn forget_hidden_fresh_workspace(&mut self) {
        let Some(fresh) = self.fresh_workspace_id else { return };
        if !self
            .visible_workspaces()
            .iter()
            .any(|workspace| workspace.id() == fresh)
        {
            self.fresh_workspace_id = None;
        }
    }

    /// Drops a removed workspace from the marked set, switching the filter off as the set empties. Callers save.
    pub(crate) fn drop_focus_member(&mut self, id: Uuid) {
        self.focus.ids.remove(&id);
        if self.focus.ids.is_empty() {
            self.focus.enabled = false;
        }
    }

    /// Marks a freshly created workspace so an enabled filter shows it; with the filter off the set must not widen
    /// behind the user's back.
    pub(crate) fn reveal_new_focus_member(&mut self, id: Uuid) {
        if self.focus.enabled {
            self.focus.ids.insert(id);
        }
    }

    /// The workspaces the tree renders: the marked ones while the filter applies, else all. Should the invariant ever
    /// break and nothing be marked, the whole tree beats no rows.
    #[must_use]
    pub fn visible_workspaces(&self) -> Vec<&Workspace> {
        if self.focus.enabled {
            let visible: Vec<&Workspace> = self
                .workspaces
                .iter()
                .filter(|workspace| self.focus.ids.contains(&workspace.id()))
                .collect();
            if !visible.is_empty() {
                return visible;
            }
        }
        self.workspaces.iter().collect()
    }

    /// Whether a workspace step has anywhere to go: never in flagged mode, and a lone visible workspace is a dead end
    /// only while it already is current.
    #[must_use]
    pub fn can_step_workspaces(&self) -> bool {
        if self.sidebar.mode != SidebarMode::Tree {
            return false;
        }
        let ids: Vec<Uuid> = self
            .visible_workspaces()
            .iter()
            .map(|workspace| workspace.id())
            .collect();
        if ids.is_empty() {
            return false;
        }
        match self.current_workspace_id() {
            Some(current) if ids.contains(&current) => ids.len() > 1,
            _ => true,
        }
    }

    /// The session set navigation works over: the flagged sessions in flagged mode, else the visible workspaces'
    /// sessions.
    #[must_use]
    pub fn navigable_sessions(&self) -> Vec<&Session> {
        if self.sidebar.mode == SidebarMode::Flagged {
            return self.flagged_sessions();
        }
        self.visible_workspaces()
            .into_iter()
            .flat_map(|workspace| workspace.sessions.iter())
            .collect()
    }

    /// The ids of [`AppStore::navigable_sessions`].
    #[must_use]
    pub fn navigable_session_ids(&self) -> Vec<Uuid> {
        self.navigable_sessions().into_iter().map(Session::id).collect()
    }

    /// Moves the selection back inside the visible set when the active session is outside it: to the most recent
    /// visible session, else the first. A no-op with no selection or nothing visible.
    pub(crate) fn reselect_if_selection_hidden(&mut self) {
        let Some(selected) = self.selected_session_id else {
            return;
        };
        let visible = self.navigable_session_ids();
        let Some(&first) = visible.first() else { return };
        if visible.contains(&selected) {
            return;
        }
        let target = self.navigable_recent_sessions(1).first().copied().unwrap_or(first);
        self.select_session(Some(target));
    }

    /// The selection after the workspace holding it was removed: the most recent visible session, else the first
    /// visible one; a positional pick at `index` only when nothing is visible.
    pub(crate) fn workspace_removal_target(&self, index: usize) -> Option<Uuid> {
        let visible = self.navigable_session_ids();
        if let Some(&first) = visible.first() {
            return Some(self.navigable_recent_sessions(1).first().copied().unwrap_or(first));
        }
        let fallback = index.min(self.workspaces.len().checked_sub(1)?);
        self.workspaces[fallback]
            .sessions
            .first()
            .or_else(|| self.workspaces.iter().find_map(|workspace| workspace.sessions.first()))
            .map(Session::id)
    }

    // MARK: - Sidebar multi-selection

    /// Replaces the sidebar selection with ids from the visible projection.
    pub fn set_sidebar_selection(&mut self, ids: Vec<Uuid>) {
        self.sidebar_selection_raw = ids;
    }

    /// Replaces the sidebar selection with a single row.
    pub fn replace_sidebar_selection(&mut self, id: Option<Uuid>) {
        self.sidebar_selection_raw = id.into_iter().collect();
    }

    /// The selected ids still visible, in visible order; empty unless the active session is among them.
    #[must_use]
    pub fn sidebar_selection_ids(&self) -> Vec<Uuid> {
        let Some(selected) = self.selected_session_id else {
            return Vec::new();
        };
        let normalized: Vec<Uuid> = self
            .navigable_session_ids()
            .into_iter()
            .filter(|id| self.sidebar_selection_raw.contains(id))
            .collect();
        if normalized.contains(&selected) {
            normalized
        } else {
            Vec::new()
        }
    }

    /// What a session row's context menu acts on: the selection when the clicked row is in it, else the clicked row
    /// when visible, else the selection, else the visible active session.
    #[must_use]
    pub fn sidebar_selection_targets(&self, clicked: Option<Uuid>) -> Vec<Uuid> {
        let selection = self.sidebar_selection_ids();
        let visible = self.navigable_session_ids();
        if let Some(clicked) = clicked {
            if selection.contains(&clicked) {
                return selection;
            }
            if visible.contains(&clicked) {
                return vec![clicked];
            }
        }
        if !selection.is_empty() {
            return selection;
        }
        match self.selected_session_id {
            Some(selected) if visible.contains(&selected) => vec![selected],
            _ => Vec::new(),
        }
    }

    pub(crate) fn prune_sidebar_selection(&mut self) {
        self.sidebar_selection_raw = self.sidebar_selection_ids();
    }
}
