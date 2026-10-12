//! Moving the selection: relative steps through sessions and workspaces (upstream's `navigateSession`,
//! `navigateWorkspace`), the recency queries (`AppStore+Recency.swift`), where a new session lands
//! (`AppStore+NewSessionPlacement.swift`) and who takes the selection after a close
//! (`AppStore+CloseReselection.swift`, `reselectionTarget`).

use std::collections::HashSet;

use uuid::Uuid;

use super::session::{AgentIndicator, Session};
use super::store::{AppStore, Location, WorkspaceStep};
use super::workspace::SidebarMode;

/// A relative step through the visible session list. `Next`/`Previous` step one and wrap, `First`/`Last` jump to an
/// end, and the attention steps go (wrapping) through the sessions needing attention only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionNavigation {
    Next,
    Previous,
    First,
    Last,
    NextAttention,
    PreviousAttention,
}

impl SessionNavigation {
    /// Parses a control-channel direction; both `prev` (the CLI's spelling) and `previous` are accepted.
    #[must_use]
    pub fn from_wire(direction: &str) -> Option<Self> {
        match direction {
            "next" => Some(Self::Next),
            "prev" | "previous" => Some(Self::Previous),
            "first" => Some(Self::First),
            "last" => Some(Self::Last),
            "next-attention" => Some(Self::NextAttention),
            "prev-attention" | "previous-attention" => Some(Self::PreviousAttention),
            _ => None,
        }
    }
}

/// A relative step through the visible workspace list, wrapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkspaceNavigation {
    Next,
    Previous,
}

impl WorkspaceNavigation {
    /// Parses a control-channel direction; both spellings of previous are accepted.
    #[must_use]
    pub fn from_wire(direction: &str) -> Option<Self> {
        match direction {
            "next" => Some(Self::Next),
            "prev" | "previous" => Some(Self::Previous),
            _ => None,
        }
    }

    pub(crate) fn step(self) -> isize {
        match self {
            Self::Next => 1,
            Self::Previous => -1,
        }
    }
}

/// Where a new session goes in its workspace (the `newSessionPlacement` setting).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum NewSessionPlacement {
    /// Appended.
    #[default]
    End,
    /// Right after the selected session, when it lives in the same workspace.
    AfterCurrent,
}

impl NewSessionPlacement {
    /// The settings spelling.
    #[must_use]
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::End => "end",
            Self::AfterCurrent => "afterCurrent",
        }
    }

    /// Parses the settings spelling.
    #[must_use]
    pub fn from_wire(name: &str) -> Option<Self> {
        match name {
            "end" => Some(Self::End),
            "afterCurrent" => Some(Self::AfterCurrent),
            _ => None,
        }
    }
}

/// `index + step` wrapped into `0..count`; `count` is never zero here.
pub(crate) fn wrapped(index: usize, step: isize, count: usize) -> usize {
    let count = count.cast_signed();
    (index.cast_signed() + step).rem_euclid(count).cast_unsigned()
}

impl AppStore {
    /// Up to `limit` most recently used session ids across all workspaces, most recent first, skipping closed ones
    /// (`dashboard --mru`).
    #[must_use]
    pub fn recent_sessions(&self, limit: usize) -> Vec<Uuid> {
        let valid: HashSet<Uuid> = self.sessions().map(Session::id).collect();
        self.session_recency.top(limit, |id| valid.contains(id))
    }

    /// The recent-session candidates of the title-bar popover and the switcher: most recent first, within the visible
    /// navigation scope, without the current session.
    #[must_use]
    pub fn navigable_recent_sessions(&self, limit: usize) -> Vec<Uuid> {
        let mut valid: HashSet<Uuid> = self.navigable_session_ids().into_iter().collect();
        if let Some(active) = self.selected_session_id {
            valid.remove(&active);
        }
        self.session_recency.top(limit, |id| valid.contains(id))
    }

    /// The slot after the selection in `workspace` under [`NewSessionPlacement::AfterCurrent`], or `None` to append.
    #[must_use]
    pub fn new_session_insertion_index(&self, workspace: Uuid, placement: NewSessionPlacement) -> Option<usize> {
        if placement != NewSessionPlacement::AfterCurrent {
            return None;
        }
        let location = self.session_location(self.selected_session_id?)?;
        (location.workspace == workspace).then_some(location.index + 1)
    }

    /// Steps the selection through [`AppStore::navigable_sessions`] in sidebar order, wrapping within that set and
    /// never leaking across the filter. With no selection in the set, `Next`/`Previous` land on the first session.
    /// A no-op on an empty set, or an attention step with nowhere to go. Returns what
    /// [`AppStore::select_session`] returns.
    pub fn navigate_session(&mut self, direction: SessionNavigation) -> Option<AgentIndicator> {
        let ids = self.navigable_session_ids();
        let (&first, &last) = (ids.first()?, ids.last()?);
        let current = self
            .selected_session_id
            .and_then(|id| ids.iter().position(|&x| x == id));
        let target = match direction {
            SessionNavigation::First => first,
            SessionNavigation::Last => last,
            SessionNavigation::Next | SessionNavigation::Previous => {
                let step = if direction == SessionNavigation::Next { 1 } else { -1 };
                current.map_or(first, |index| ids[wrapped(index, step, ids.len())])
            }
            SessionNavigation::NextAttention | SessionNavigation::PreviousAttention => {
                self.attention_target(&ids, current, direction == SessionNavigation::NextAttention)?
            }
        };
        self.select_session(Some(target))
    }

    /// The next or previous session needing attention after `current`, wrapping and excluding the current one; with
    /// no current session the scan starts from the end opposite the direction.
    fn attention_target(&self, ids: &[Uuid], current: Option<usize>, forward: bool) -> Option<Uuid> {
        let count = ids.len().cast_signed();
        let step: isize = if forward { 1 } else { -1 };
        let start = current.map_or(if forward { -1 } else { count }, usize::cast_signed);
        (1..=count)
            .map(|k| (start + step * k).rem_euclid(count).cast_unsigned())
            .take_while(|&index| Some(index) != current)
            .map(|index| ids[index])
            .find(|&id| {
                self.session(id)
                    .is_some_and(|session| session.agent_indicator.status.needs_attention())
            })
    }

    /// Steps the current workspace one place through the visible workspaces, wrapping, and selects it as
    /// [`AppStore::select_workspace`] does. `None` with nowhere to step (see [`AppStore::can_step_workspaces`]).
    pub fn navigate_workspace(&mut self, direction: WorkspaceNavigation) -> Option<WorkspaceStep> {
        if !self.can_step_workspaces() {
            return None;
        }
        let ids: Vec<Uuid> = self
            .visible_workspaces()
            .iter()
            .map(|workspace| workspace.id())
            .collect();
        let current = self
            .current_workspace_id()
            .and_then(|id| ids.iter().position(|&x| x == id));
        let target = match (current, direction) {
            (Some(index), _) => ids[wrapped(index, direction.step(), ids.len())],
            // the current workspace is filtered out: enter the set at the end the step comes from
            (None, WorkspaceNavigation::Next) => *ids.first()?,
            (None, WorkspaceNavigation::Previous) => *ids.last()?,
        };
        self.select_workspace(target)
    }

    /// The next selection after closing the active session at `at` (already removed): the most recent surviving
    /// session in scope, else a positional pick. The scope is the closed session's workspace within the visible set,
    /// widening to everything visible and then the whole tree when empty. Without recency, a narrowed view (flagged
    /// mode or the filter) walks its widened scope in flattened sidebar order; otherwise the positional
    /// [`AppStore::reselection_target`] applies.
    pub(crate) fn close_reselection_target(&self, at: Location) -> Option<Uuid> {
        let visible: HashSet<Uuid> = self.navigable_session_ids().into_iter().collect();
        let everything: HashSet<Uuid> = self.sessions().map(Session::id).collect();
        let same_workspace: HashSet<Uuid> = self.workspaces[at.workspace]
            .sessions
            .iter()
            .map(Session::id)
            .filter(|id| visible.contains(id))
            .collect();
        let scope = if !same_workspace.is_empty() {
            same_workspace
        } else if !visible.is_empty() {
            visible
        } else {
            everything
        };
        if let Some(&recent) = self.session_recency.top(1, |id| scope.contains(id)).first() {
            return Some(recent);
        }
        let narrowed = self.sidebar.mode == SidebarMode::Flagged || self.focus.enabled;
        if narrowed && let Some(in_scope) = self.nearest_in_scope_target(at, &scope) {
            return Some(in_scope);
        }
        self.reselection_target(at)
    }

    /// The positional walk restricted to `scope` over the tree flattened in sidebar order: the in-scope session that
    /// shifted into the removed slot or after it, else the nearest one before it.
    fn nearest_in_scope_target(&self, at: Location, scope: &HashSet<Uuid>) -> Option<Uuid> {
        let before: usize = self.workspaces[..at.workspace]
            .iter()
            .map(|workspace| workspace.sessions.len())
            .sum();
        let removed_slot = before + at.session.min(self.workspaces[at.workspace].sessions.len());
        let flattened: Vec<Uuid> = self.sessions().map(Session::id).collect();
        flattened[removed_slot..]
            .iter()
            .find(|id| scope.contains(id))
            .or_else(|| flattened[..removed_slot].iter().rev().find(|id| scope.contains(id)))
            .copied()
    }

    /// The positional pick after removing the session at `at`: the one that shifted into its slot, else the previous
    /// one in that workspace, else the first session of any workspace.
    pub(crate) fn reselection_target(&self, at: Location) -> Option<Uuid> {
        let sessions = &self.workspaces[at.workspace].sessions;
        if let Some(shifted) = sessions.get(at.session) {
            return Some(shifted.id());
        }
        if at.session > 0
            && let Some(previous) = sessions.last()
        {
            return Some(previous.id());
        }
        self.workspaces
            .iter()
            .find_map(|workspace| workspace.sessions.first())
            .map(Session::id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapping_steps() {
        assert_eq!(wrapped(0, -1, 3), 2);
        assert_eq!(wrapped(2, 1, 3), 0);
        assert_eq!(wrapped(1, 1, 3), 2);
        assert_eq!(wrapped(0, 1, 1), 0);
    }

    #[test]
    fn direction_wire_names() {
        assert_eq!(SessionNavigation::from_wire("prev"), Some(SessionNavigation::Previous));
        assert_eq!(
            SessionNavigation::from_wire("previous"),
            Some(SessionNavigation::Previous)
        );
        assert_eq!(
            SessionNavigation::from_wire("previous-attention"),
            Some(SessionNavigation::PreviousAttention)
        );
        assert_eq!(SessionNavigation::from_wire("up"), None);
        assert_eq!(
            WorkspaceNavigation::from_wire("prev"),
            Some(WorkspaceNavigation::Previous)
        );
        assert_eq!(WorkspaceNavigation::from_wire("first"), None);
        assert_eq!(
            NewSessionPlacement::from_wire(NewSessionPlacement::AfterCurrent.wire_name()),
            Some(NewSessionPlacement::AfterCurrent)
        );
        assert_eq!(NewSessionPlacement::from_wire("start"), None);
    }
}
