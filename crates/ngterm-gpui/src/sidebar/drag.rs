//! Sidebar drag and drop (upstream's `WorkspaceSidebar+DragDrop.swift`): what a drag hovering over the rows proposes,
//! as pure functions over the rows, their vertical spans and the window snapshot. The index arithmetic itself is the
//! host-free [`ngterm_core::sidebar_drop`], shared with `session.move --after/--before`.
//!
//! - A session drag lands on a workspace row (appended), on the upper half of a session row (before it) or on its
//!   lower half (just after it); a selected row drags the whole selection, in visible order.
//! - A workspace drag ignores session rows: the slot is the count of workspace rows whose midpoint sits above the
//!   cursor, mapped onto the full workspace list through the focus filter's visible indices.
//! - A file-manager folder drop opens a session per directory in the hovered row's workspace, else the sole focused
//!   one, else the current one; the tree only.
//!
//! Ported from the GTK build's `sidebar/drag.rs`; the payload is a plain value GPUI's drag carries.

use std::collections::HashMap;

use ngterm_core::model::SessionLocation;
use ngterm_core::sidebar_drop::{
    self, DropIndex, SessionDropTarget, SessionSource, resolve_sessions, resolve_workspace, workspace_insert_index,
};
use uuid::Uuid;

use super::rows::{RowKind, SidebarRow};

/// What a sidebar row drag carries; local to the app, so no other drop target (a terminal) takes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DragPayload {
    /// A workspace drag (one id) or a session drag (the dragged block, in visible order).
    pub workspace: bool,
    pub ids: Vec<Uuid>,
}

/// A row's vertical extent in the list's coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RowSpan {
    pub top: f64,
    pub height: f64,
}

impl RowSpan {
    fn contains(self, y: f64) -> bool {
        y >= self.top && y < self.top + self.height
    }

    fn mid(self) -> f64 {
        self.top + self.height / 2.0
    }
}

/// How a proposal is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Highlight {
    /// The row is the drop target.
    OnRow(usize),
    /// A line above the row.
    Above(usize),
    /// A line below the row.
    Below(usize),
}

/// What dropping here would do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Proposal {
    Sessions {
        sessions: Vec<Uuid>,
        workspace: Uuid,
        destination: usize,
    },
    Workspace {
        workspace: Uuid,
        destination: usize,
    },
    Directories {
        workspace: Uuid,
    },
}

/// What the drop math reads of the window.
#[derive(Debug, Clone, Default)]
pub struct DropContext {
    pub locations: HashMap<Uuid, SessionLocation>,
    /// Every workspace in tree order, with its session count.
    pub workspaces: Vec<(Uuid, usize)>,
    pub tree: bool,
    pub sole_focus: Option<Uuid>,
    pub current_workspace: Option<Uuid>,
}

impl DropContext {
    fn session_count(&self, workspace: Uuid) -> usize {
        self.workspaces
            .iter()
            .find(|(id, _)| *id == workspace)
            .map_or(0, |(_, count)| *count)
    }
}

/// The row under `y`, if any.
#[must_use]
pub fn row_at(spans: &[RowSpan], y: f64) -> Option<usize> {
    spans.iter().position(|span| span.contains(y))
}

/// A session drag of `dragged` hovering at `y`: the move it would perform and how to draw it; `None` over empty space
/// or for a drop that changes nothing.
#[must_use]
pub fn propose_sessions(
    rows: &[SidebarRow],
    spans: &[RowSpan],
    y: f64,
    dragged: &[Uuid],
    context: &DropContext,
) -> Option<(Proposal, Highlight)> {
    let index = row_at(spans, y)?;
    let row = rows.get(index)?;
    let (target, child_index, highlight) = match row.kind {
        RowKind::Workspace { .. } => (
            SessionDropTarget::WorkspaceRow {
                id: row.id,
                session_count: context.session_count(row.id),
            },
            DropIndex::OnItem,
            Highlight::OnRow(index),
        ),
        RowKind::Session { .. } => {
            let location = context.locations.get(&row.id)?;
            let target = SessionDropTarget::SessionRow {
                workspace: location.workspace,
                session_index: location.index,
                session_count: location.count,
            };
            if y < spans[index].mid() {
                (target, DropIndex::Before(location.index), Highlight::Above(index))
            } else {
                (target, DropIndex::OnItem, Highlight::Below(index))
            }
        }
    };
    let sources: Vec<SessionSource> = dragged
        .iter()
        .filter_map(|id| context.locations.get(id))
        .map(|location| SessionSource {
            workspace: location.workspace,
            index: location.index,
        })
        .collect();
    if sources.len() != dragged.len() {
        return None;
    }
    let resolution = resolve_sessions(&sources, target, child_index)?;
    Some((
        Proposal::Sessions {
            sessions: dragged.to_vec(),
            workspace: resolution.workspace,
            destination: resolution.destination,
        },
        highlight,
    ))
}

/// A workspace drag of `dragged` hovering at `y` (upstream's `resolveWorkspaceMove`): session rows are ignored, the
/// slot counts the rendered workspace rows whose midpoint is above the cursor.
#[must_use]
pub fn propose_workspace(
    rows: &[SidebarRow],
    spans: &[RowSpan],
    y: f64,
    dragged: Uuid,
    context: &DropContext,
) -> Option<(Proposal, Highlight)> {
    let order: Vec<Uuid> = context.workspaces.iter().map(|(id, _)| *id).collect();
    let source = order.iter().position(|id| *id == dragged)?;
    // (row index, full index) of each rendered workspace row
    let rendered: Vec<(usize, usize)> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.is_workspace())
        .filter_map(|(index, row)| Some((index, order.iter().position(|id| *id == row.id)?)))
        .collect();
    let slot = rendered
        .iter()
        .filter(|(index, _)| spans.get(*index).is_some_and(|span| y > span.mid()))
        .count();
    let visible: Vec<usize> = rendered.iter().map(|(_, full)| *full).collect();
    let insert = workspace_insert_index(&visible, slot);
    let resolution = resolve_workspace(source, order.len(), DropIndex::Before(insert))?;
    let highlight = match rendered.get(slot) {
        Some((index, _)) => Highlight::Above(*index),
        None => Highlight::Below(rows.len().checked_sub(1)?),
    };
    Some((
        Proposal::Workspace {
            workspace: dragged,
            destination: resolution.destination,
        },
        highlight,
    ))
}

/// A folder drop hovering at `y`: the workspace that would receive the sessions, highlighted when its row shows.
#[must_use]
pub fn propose_directories(
    rows: &[SidebarRow],
    spans: &[RowSpan],
    y: f64,
    context: &DropContext,
) -> Option<(Proposal, Option<Highlight>)> {
    let row_workspace = row_at(spans, y)
        .and_then(|index| rows.get(index))
        .map(|row| match row.kind {
            RowKind::Workspace { .. } => row.id,
            RowKind::Session { workspace, .. } => workspace,
        });
    let mode = if context.tree {
        ngterm_core::model::SidebarMode::Tree
    } else {
        ngterm_core::model::SidebarMode::Flagged
    };
    let workspace =
        sidebar_drop::resolve_directory_workspace(mode, row_workspace, context.sole_focus, context.current_workspace)?;
    let highlight = rows
        .iter()
        .position(|row| row.is_workspace() && row.id == workspace)
        .map(Highlight::OnRow);
    Some((Proposal::Directories { workspace }, highlight))
}

/// The real directories among a folder drop's paths, at most the import cap: `Err` when there are more.
///
/// # Errors
///
/// More directories than [`sidebar_drop::MAXIMUM_DIRECTORY_IMPORT_COUNT`].
pub fn directories(paths: impl IntoIterator<Item = std::path::PathBuf>) -> Result<Vec<String>, ()> {
    let mut found = Vec::new();
    for path in paths {
        if !path.is_dir() {
            continue;
        }
        if found.len() == sidebar_drop::MAXIMUM_DIRECTORY_IMPORT_COUNT {
            return Err(());
        }
        found.push(path.to_string_lossy().into_owned());
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::row_icon::Glyph;

    fn workspace_row(id: Uuid) -> SidebarRow {
        SidebarRow {
            id,
            kind: RowKind::Workspace { expanded: true },
            label: String::new(),
            badge: 0,
            icon: Glyph::Workspace { focus_member: false },
            status: None,
            icon_tooltip: None,
        }
    }

    fn session_row(id: Uuid, workspace: Uuid) -> SidebarRow {
        SidebarRow {
            id,
            kind: RowKind::Session {
                workspace,
                nested: true,
                selected: false,
            },
            label: String::new(),
            badge: 0,
            icon: Glyph::Session {
                split: None,
                flagged: false,
            },
            status: None,
            icon_tooltip: None,
        }
    }

    /// Workspace one (a, b), workspace two (c), every row 20 high.
    fn tree() -> (Vec<SidebarRow>, Vec<RowSpan>, DropContext, [Uuid; 2], [Uuid; 3]) {
        let [one, two] = [Uuid::new_v4(), Uuid::new_v4()];
        let [a, b, c] = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
        let rows = vec![
            workspace_row(one),
            session_row(a, one),
            session_row(b, one),
            workspace_row(two),
            session_row(c, two),
        ];
        let spans = (0..rows.len())
            .map(|index| RowSpan {
                top: f64::from(u8::try_from(index).unwrap()) * 20.0,
                height: 20.0,
            })
            .collect();
        let location = |workspace, index, count| SessionLocation {
            workspace,
            index,
            count,
        };
        let context = DropContext {
            locations: HashMap::from([
                (a, location(one, 0, 2)),
                (b, location(one, 1, 2)),
                (c, location(two, 0, 1)),
            ]),
            workspaces: vec![(one, 2), (two, 1)],
            tree: true,
            sole_focus: None,
            current_workspace: Some(one),
        };
        (rows, spans, context, [one, two], [a, b, c])
    }

    #[test]
    fn a_session_lands_before_or_after_a_row_or_appended_to_a_workspace() {
        let (rows, spans, context, [one, two], [a, b, c]) = tree();
        // the upper half of b's row: before b, which is where a already is
        assert_eq!(propose_sessions(&rows, &spans, 42.0, &[a], &context), None);
        // the lower half of b's row: after b
        assert_eq!(
            propose_sessions(&rows, &spans, 55.0, &[a], &context),
            Some((
                Proposal::Sessions {
                    sessions: vec![a],
                    workspace: one,
                    destination: 1
                },
                Highlight::Below(2)
            ))
        );
        assert_eq!(
            propose_sessions(&rows, &spans, 65.0, &[a, b], &context),
            Some((
                Proposal::Sessions {
                    sessions: vec![a, b],
                    workspace: two,
                    destination: 1
                },
                Highlight::OnRow(3)
            ))
        );
        assert_eq!(
            propose_sessions(&rows, &spans, 85.0, &[c], &context),
            None,
            "after itself"
        );
        assert_eq!(
            propose_sessions(&rows, &spans, 500.0, &[c], &context),
            None,
            "empty space"
        );
    }

    #[test]
    fn a_workspace_lands_by_workspace_row_midpoints() {
        let (rows, spans, context, [one, two], _) = tree();
        assert_eq!(
            propose_workspace(&rows, &spans, 5.0, two, &context),
            Some((
                Proposal::Workspace {
                    workspace: two,
                    destination: 0
                },
                Highlight::Above(0)
            ))
        );
        // over one's sessions: still after one, which is where two is
        assert_eq!(propose_workspace(&rows, &spans, 45.0, two, &context), None);
        assert_eq!(
            propose_workspace(&rows, &spans, 95.0, one, &context),
            Some((
                Proposal::Workspace {
                    workspace: one,
                    destination: 1
                },
                Highlight::Below(4)
            ))
        );
    }

    #[test]
    fn folders_land_in_the_hovered_rows_workspace_in_the_tree_only() {
        let (rows, spans, mut context, [one, two], _) = tree();
        assert_eq!(
            propose_directories(&rows, &spans, 85.0, &context),
            Some((Proposal::Directories { workspace: two }, Some(Highlight::OnRow(3))))
        );
        assert_eq!(
            propose_directories(&rows, &spans, 500.0, &context),
            Some((Proposal::Directories { workspace: one }, Some(Highlight::OnRow(0)))),
            "empty space takes the current workspace"
        );
        context.tree = false;
        assert_eq!(propose_directories(&rows, &spans, 85.0, &context), None);
    }

    #[test]
    fn only_directories_are_imported_up_to_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(
            directories([dir.path().to_path_buf(), file]),
            Ok(vec![dir.path().to_string_lossy().into_owned()])
        );
        let many = (0..=sidebar_drop::MAXIMUM_DIRECTORY_IMPORT_COUNT).map(|_| dir.path().to_path_buf());
        assert_eq!(directories(many), Err(()));
    }
}
