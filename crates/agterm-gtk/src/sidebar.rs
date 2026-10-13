//! A minimal sidebar after upstream's `WorkspaceSidebar.swift` (its outline coordinator) and
//! `WorkspaceSidebar+RowRendering.swift`: the workspaces the focus filter shows, each with its sessions while its row
//! is open, the selected session's row highlighted, unseen badges. A session-row click selects that session; a
//! workspace-row click opens or folds the workspace and persists that, as upstream's whole-row toggle does.
//!
//! The rows are a pure function of the model and the rows the outline shows open ([`rows`], [`Expansion`]), so they
//! are testable without a display. Context menus, drag and drop, rename, the flagged view and status glyphs come with
//! Task 22.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use agterm_core::model::{AppStore, Workspace};
use gtk::prelude::*;
use uuid::Uuid;

use crate::state::{self, AppState, Poster};

/// One sidebar row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarRow {
    pub id: Uuid,
    pub kind: RowKind,
    /// A workspace's name, a session's display name.
    pub name: String,
    /// The unseen-notification count the trailing badge shows; 0 hides it.
    pub badge: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// A workspace row, rendered open or folded.
    Workspace { expanded: bool },
    /// A session row, highlighted while it is the selected session.
    Session { selected: bool },
}

/// The workspace rows the outline renders open (upstream's coordinator `expandedWorkspaceIDs`): seeded from the
/// persisted `Workspace::is_expanded`, kept in step with the model on every refresh (gone workspaces drop out, any
/// the model marks open join, a user fold stays folded, a `workspace.collapse`/`expand` request opens or folds its
/// row), and opened for view only when the selection moves into a folded workspace (`syncSelection`'s reveal, which
/// never persists).
#[derive(Debug, Default)]
pub struct Expansion {
    ids: HashSet<Uuid>,
    /// The selection the last reveal ran for: only a selection change reveals.
    revealed: Option<Uuid>,
}

impl Expansion {
    /// Takes `store`'s pending per-row open and fold requests (upstream's `setWorkspaceExpandedNotified`): the row
    /// follows even where the persisted flag did not change, as after a reveal.
    pub fn apply_requests(&mut self, store: &mut AppStore) {
        for (id, expanded) in store.take_sidebar_expansion_requests() {
            self.set(id, expanded);
        }
    }

    /// Brings the open set in line with `store` (see the type docs) and returns it.
    pub fn sync(&mut self, store: &AppStore) -> &HashSet<Uuid> {
        let existing: HashSet<Uuid> = store.workspaces().iter().map(Workspace::id).collect();
        self.ids.retain(|id| existing.contains(id));
        self.ids.extend(
            store
                .workspaces()
                .iter()
                .filter(|workspace| workspace.is_expanded)
                .map(Workspace::id),
        );
        let selected = store.selected_session_id();
        if selected != self.revealed {
            if let Some(owner) = selected.and_then(|id| store.workspace_for_session(id)) {
                self.ids.insert(owner.id());
            }
            self.revealed = selected;
        }
        &self.ids
    }

    /// Records a user's open or fold of a workspace row.
    pub fn set(&mut self, id: Uuid, expanded: bool) {
        if expanded {
            self.ids.insert(id);
        } else {
            self.ids.remove(&id);
        }
    }
}

/// The rows of `store`'s tree: each workspace the focus filter shows, followed by its sessions while it renders open
/// (in `expanded`, or the filter's single marked workspace, which upstream force-opens). Badges show only when
/// `badges` is on (the notification badge setting).
#[must_use]
pub fn rows(store: &AppStore, expanded: &HashSet<Uuid>, badges: bool) -> Vec<SidebarRow> {
    let badge = |count: usize| if badges { count } else { 0 };
    let forced = store.sole_focused_workspace_id();
    let selected = store.selected_session_id();
    let mut rows = Vec::new();
    for workspace in store.visible_workspaces() {
        let open = expanded.contains(&workspace.id()) || forced == Some(workspace.id());
        rows.push(SidebarRow {
            id: workspace.id(),
            kind: RowKind::Workspace { expanded: open },
            name: workspace.name.clone(),
            badge: badge(workspace.unseen_count()),
        });
        if !open {
            continue;
        }
        rows.extend(workspace.sessions.iter().map(|session| SidebarRow {
            id: session.id(),
            kind: RowKind::Session {
                selected: selected == Some(session.id()),
            },
            name: session.display_name(),
            badge: badge(session.unseen_count),
        }));
    }
    rows
}

/// The sidebar widget: a navigation list of [`rows`], rebuilt when they change.
pub struct Sidebar {
    root: gtk::ScrolledWindow,
    list: gtk::ListBox,
    /// The rows the list shows, shared with its activation handler.
    rows: Rc<RefCell<Vec<SidebarRow>>>,
    expansion: Expansion,
}

impl Sidebar {
    #[must_use]
    pub fn new(poster: &Poster) -> Self {
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .activate_on_single_click(true)
            .build();
        list.add_css_class("navigation-sidebar");
        let root = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&list)
            .build();
        let rows = Rc::new(RefCell::new(Vec::<SidebarRow>::new()));
        let shown = Rc::clone(&rows);
        let posting = poster.clone();
        // posted, never in place: the list is rebuilt by the task, which must not happen inside its own handler
        list.connect_row_activated(move |_, row| {
            let Some(clicked) = usize::try_from(row.index())
                .ok()
                .and_then(|index| shown.borrow().get(index).cloned())
            else {
                return;
            };
            state::post(&posting, move |state: &mut AppState| state.sidebar_clicked(&clicked));
        });
        Self {
            root,
            list,
            rows,
            expansion: Expansion::default(),
        }
    }

    /// The widget to place in the window.
    #[must_use]
    pub fn widget(&self) -> &gtk::ScrolledWindow {
        &self.root
    }

    /// Records a user's open or fold of workspace `id` in the outline; the caller persists it.
    pub fn set_expanded(&mut self, id: Uuid, expanded: bool) {
        self.expansion.set(id, expanded);
    }

    /// Re-renders from `store`, mirroring the rows the outline shows open back into it
    /// (`AppStore::note_sidebar_expansion`, view state for Collapse/Expand Workspace).
    pub fn refresh(&mut self, store: &mut AppStore, badges: bool) {
        self.root.set_visible(store.sidebar().visible);
        self.expansion.apply_requests(store);
        let expanded = self.expansion.sync(store).clone();
        let rows = rows(store, &expanded, badges);
        store.note_sidebar_expansion(expanded);
        if *self.rows.borrow() != rows {
            self.rebuild(&rows);
            *self.rows.borrow_mut() = rows;
        }
        let selected = self
            .rows
            .borrow()
            .iter()
            .position(|row| row.kind == RowKind::Session { selected: true })
            .and_then(|index| i32::try_from(index).ok())
            .and_then(|index| self.list.row_at_index(index));
        match selected {
            Some(row) => self.list.select_row(Some(&row)),
            None => self.list.unselect_all(),
        }
    }

    fn rebuild(&self, rows: &[SidebarRow]) {
        self.list.remove_all();
        for row in rows {
            self.list.append(&row_widget(row));
        }
    }
}

/// One row: a disclosure arrow for a workspace, the row icon (upstream's 2x2 grid and terminal glyphs), the name,
/// truncated at the end, and the badge.
fn row_widget(row: &SidebarRow) -> gtk::ListBoxRow {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let (icon, accessible_id, selectable) = match row.kind {
        RowKind::Workspace { expanded } => {
            let arrow = if expanded {
                "pan-down-symbolic"
            } else {
                "pan-end-symbolic"
            };
            content.append(&gtk::Image::from_icon_name(arrow));
            ("view-grid-symbolic", "workspace-row", false)
        }
        RowKind::Session { .. } => {
            // under the workspace's name, past its disclosure arrow
            content.set_margin_start(22);
            ("utilities-terminal-symbolic", "session-row", true)
        }
    };
    let image = gtk::Image::from_icon_name(icon);
    image.add_css_class("dim-label");
    content.append(&image);
    let label = gtk::Label::builder()
        .label(&row.name)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .name(accessible_id)
        .build();
    if matches!(row.kind, RowKind::Workspace { .. }) {
        label.add_css_class("heading");
    }
    content.append(&label);
    if row.badge > 0 {
        let badge = gtk::Label::new(Some(&badge_text(row.badge)));
        badge.add_css_class("caption");
        badge.add_css_class("numeric");
        badge.add_css_class("accent");
        content.append(&badge);
    }
    gtk::ListBoxRow::builder()
        .child(&content)
        .selectable(selectable)
        .build()
}

/// The badge's count, capped as upstream's `BadgeView` caps it.
fn badge_text(count: usize) -> String {
    if count > 99 {
        "99+".to_owned()
    } else {
        count.to_string()
    }
}

#[cfg(test)]
mod tests {
    use agterm_core::model::{NewSession, WindowLibrary};

    use super::*;

    /// A store of two workspaces: "one" with two sessions (the second selected), "two" with one.
    fn store() -> (AppStore, [Uuid; 2], [Uuid; 3]) {
        let mut library = WindowLibrary::seeded("/home/u");
        let window = library.active_window_id().unwrap();
        let mut store = std::mem::take(library.store_mut(window).unwrap());
        let one = store.workspaces()[0].id();
        let first = store.sessions().next().unwrap().id();
        let second = store.add_session(one, NewSession::at_cwd("/srv")).unwrap();
        let two = store.add_workspace("two");
        let third = store.add_session(two, NewSession::at_cwd("/var")).unwrap();
        store.select_session(Some(second));
        (store, [one, two], [first, second, third])
    }

    fn kinds(rows: &[SidebarRow]) -> Vec<(Uuid, RowKind)> {
        rows.iter().map(|row| (row.id, row.kind)).collect()
    }

    #[test]
    fn open_workspaces_list_their_sessions_and_the_selected_one_is_highlighted() {
        let (mut store, [one, two], [first, second, third]) = store();
        let mut expansion = Expansion::default();
        let built = rows(&store, expansion.sync(&store), true);
        assert_eq!(
            kinds(&built),
            vec![
                (one, RowKind::Workspace { expanded: true }),
                (first, RowKind::Session { selected: false }),
                (second, RowKind::Session { selected: true }),
                (two, RowKind::Workspace { expanded: true }),
                (third, RowKind::Session { selected: false }),
            ]
        );
        assert_eq!(built[0].name, store.workspaces()[0].name);
        assert_eq!(built[2].name, "srv");
        assert_eq!(built[3].name, "two");

        store.select_session(Some(third));
        let built = rows(&store, expansion.sync(&store), true);
        assert_eq!(built[2].kind, RowKind::Session { selected: false });
        assert_eq!(built[4].kind, RowKind::Session { selected: true });
    }

    #[test]
    fn a_folded_workspace_hides_its_sessions_until_the_selection_moves_in() {
        let (mut store, [one, two], [first, _, third]) = store();
        store.set_workspace_expanded(two, false);
        let mut expansion = Expansion::default();
        let _ = expansion.sync(&store);
        // the reveal for the current selection ran; a user fold of "two" stays folded
        expansion.set(two, false);
        let built = rows(&store, expansion.sync(&store), true);
        assert_eq!(kinds(&built)[3..], [(two, RowKind::Workspace { expanded: false })]);

        // selecting into the folded workspace opens it for view, without persisting
        store.select_session(Some(third));
        let open = expansion.sync(&store).clone();
        assert!(open.contains(&two));
        assert!(!store.workspace(two).unwrap().is_expanded);
        let built = rows(&store, &open, true);
        assert_eq!(built.last().map(|row| row.id), Some(third));

        // a user fold (persisted, as the click persists it) holds while the selection stays
        expansion.set(one, false);
        store.set_workspace_expanded(one, false);
        store.select_session(Some(third));
        assert!(!expansion.sync(&store).contains(&one));
        store.select_session(Some(first));
        assert!(expansion.sync(&store).contains(&one), "a new selection reveals again");
    }

    #[test]
    fn a_collapse_or_expand_request_folds_or_opens_the_row_even_over_a_reveal() {
        let (mut store, [one, two], [first, _, third]) = store();
        store.set_workspace_expanded(two, false);
        store.select_session(Some(third));
        let mut expansion = Expansion::default();
        // the selection revealed the workspace persisted as collapsed
        assert!(expansion.sync(&store).contains(&two));
        // `workspace.collapse` on it changes no persisted flag, yet folds the row
        store.set_workspace_expanded_in_outline(two, false);
        expansion.apply_requests(&mut store);
        assert!(!expansion.sync(&store).contains(&two));
        store.set_workspace_expanded_in_outline(two, true);
        store.set_workspace_expanded_in_outline(one, false);
        expansion.apply_requests(&mut store);
        let open = expansion.sync(&store).clone();
        assert!(open.contains(&two) && !open.contains(&one));
        assert!(
            store.take_sidebar_expansion_requests().is_empty(),
            "requests are taken once"
        );
        // a new selection into the folded one reveals it again
        store.select_session(Some(first));
        assert!(expansion.sync(&store).contains(&one));
    }

    #[test]
    fn gone_workspaces_leave_and_new_ones_open() {
        let (mut store, [_, two], _) = store();
        let mut expansion = Expansion::default();
        let _ = expansion.sync(&store);
        store.remove_workspace(two);
        let three = store.add_workspace("three");
        let open = expansion.sync(&store);
        assert!(!open.contains(&two));
        assert!(open.contains(&three));
    }

    #[test]
    fn badges_count_unseen_notifications_when_enabled() {
        let (mut store, [one, _], [first, _, _]) = store();
        store.session_mut(first).unwrap().unseen_count = 3;
        let mut expansion = Expansion::default();
        let open = expansion.sync(&store).clone();
        let shown = rows(&store, &open, true);
        let badge_of = |rows: &[SidebarRow], id| rows.iter().find(|row| row.id == id).unwrap().badge;
        assert_eq!(badge_of(&shown, first), 3);
        assert_eq!(badge_of(&shown, one), 3, "the workspace rolls its sessions up");
        let hidden = rows(&store, &open, false);
        assert_eq!(badge_of(&hidden, first), 0);
        assert_eq!(badge_of(&hidden, one), 0);
        assert_eq!(badge_text(7), "7");
        assert_eq!(badge_text(100), "99+");
    }

    #[test]
    fn the_focus_filter_shows_its_marked_workspace_forced_open() {
        let (mut store, [one, two], [_, _, third]) = store();
        store.set_workspace_expanded(two, false);
        store.select_session(Some(third));
        store.select_workspace(two);
        store.set_focused_workspace(two);
        assert_eq!(store.sole_focused_workspace_id(), Some(two));
        let rows = rows(&store, &HashSet::new(), true);
        assert!(rows.iter().all(|row| row.id != one));
        assert_eq!(rows[0].kind, RowKind::Workspace { expanded: true });
        assert_eq!(rows.len(), 2);
    }
}
