//! A workspace: a user-named group of sessions, a sidebar header row (upstream's `Workspace.swift`), plus the
//! sidebar's view mode (`SidebarMode.swift`) and the one-step reorder rule (`Reorder.swift`).

use uuid::Uuid;

use super::session::Session;

/// A user-named group of sessions (e.g. "work", "personal") with a stable id.
#[derive(Debug)]
pub struct Workspace {
    id: Uuid,
    pub name: String,
    pub sessions: Vec<Session>,
    /// Whether the sidebar row is expanded. A fresh workspace opens; persisted per workspace.
    pub is_expanded: bool,
}

impl Workspace {
    /// A workspace with a fresh id. Stores the name as given: callers sanitize.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self::with_id(Uuid::new_v4(), name)
    }

    /// A workspace with a known id (a restore).
    #[must_use]
    pub fn with_id(id: Uuid, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            sessions: Vec::new(),
            is_expanded: true,
        }
    }

    #[must_use]
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// The unseen-notification count across this workspace's sessions, for a collapsed row's badge.
    #[must_use]
    pub fn unseen_count(&self) -> usize {
        self.sessions.iter().map(|session| session.unseen_count).sum()
    }

    /// The session with `id`, if this workspace holds it.
    #[must_use]
    pub fn session(&self, id: Uuid) -> Option<&Session> {
        self.sessions.iter().find(|session| session.id() == id)
    }
}

/// Which view the sidebar renders: the workspace tree, or the flagged working set across all workspaces. Per-window,
/// persisted; an absent value reads as the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum SidebarMode {
    #[default]
    Tree,
    Flagged,
}

impl SidebarMode {
    /// The wire and snapshot spelling.
    #[must_use]
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Tree => "tree",
            Self::Flagged => "flagged",
        }
    }

    /// Parses the wire spelling; `None` for anything else.
    #[must_use]
    pub fn from_wire(name: &str) -> Option<Self> {
        match name {
            "tree" => Some(Self::Tree),
            "flagged" => Some(Self::Flagged),
            _ => None,
        }
    }
}

/// A relative one-step reorder within a list (`--to up|down|top|bottom`), without wraparound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReorderDirection {
    Up,
    Down,
    Top,
    Bottom,
}

impl ReorderDirection {
    /// Parses the wire spelling; `None` for anything else.
    #[must_use]
    pub fn from_wire(name: &str) -> Option<Self> {
        match name {
            "up" => Some(Self::Up),
            "down" => Some(Self::Down),
            "top" => Some(Self::Top),
            "bottom" => Some(Self::Bottom),
            _ => None,
        }
    }

    /// The post-removal insert index of a move from `current` in a list of `count` elements, or `None` when the move
    /// would change nothing (already at that end).
    #[must_use]
    pub fn destination_index(self, current: usize, count: usize) -> Option<usize> {
        let last = count.checked_sub(1)?;
        match self {
            Self::Up => current.checked_sub(1),
            Self::Down => (current < last).then_some(current + 1),
            Self::Top => (current > 0).then_some(0),
            Self::Bottom => (current < last).then_some(last),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::session::SessionSeed;

    #[test]
    fn a_new_workspace_is_expanded_and_empty() {
        let workspace = Workspace::new("work");
        assert!(workspace.is_expanded);
        assert!(workspace.sessions.is_empty());
        assert_eq!(workspace.unseen_count(), 0);
    }

    #[test]
    fn unseen_count_sums_the_sessions() {
        let mut workspace = Workspace::new("w");
        for count in [2, 0, 3] {
            let mut session = Session::new(SessionSeed::default());
            session.unseen_count = count;
            workspace.sessions.push(session);
        }
        assert_eq!(workspace.unseen_count(), 5);
    }

    #[test]
    fn reorder_destinations() {
        use ReorderDirection::{Bottom, Down, Top, Up};
        assert_eq!(Up.destination_index(2, 4), Some(1));
        assert_eq!(Up.destination_index(0, 4), None);
        assert_eq!(Down.destination_index(2, 4), Some(3));
        assert_eq!(Down.destination_index(3, 4), None);
        assert_eq!(Top.destination_index(2, 4), Some(0));
        assert_eq!(Top.destination_index(0, 4), None);
        assert_eq!(Bottom.destination_index(1, 4), Some(3));
        assert_eq!(Bottom.destination_index(3, 4), None);
        assert_eq!(Down.destination_index(0, 1), None);
        assert_eq!(Bottom.destination_index(0, 0), None);
    }

    #[test]
    fn wire_names() {
        assert_eq!(SidebarMode::from_wire("flagged"), Some(SidebarMode::Flagged));
        assert_eq!(
            SidebarMode::from_wire(SidebarMode::Tree.wire_name()),
            Some(SidebarMode::Tree)
        );
        assert_eq!(SidebarMode::from_wire("list"), None);
        assert_eq!(ReorderDirection::from_wire("top"), Some(ReorderDirection::Top));
        assert_eq!(ReorderDirection::from_wire("first"), None);
    }
}
