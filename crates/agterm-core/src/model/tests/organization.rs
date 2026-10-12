//! The tree's organization rules, ported from upstream's `AppStoreOrganizationTests` (move, reorder, insert-at,
//! placement, expansion, flags and the focus filter around them) and the store-level parts of
//! `AppStoreEventTests` (session lifecycle and selection events, status events, and when `tree.changed` fires).
//!
//! Upstream's on-disk save checks read the store's outbox here. Upstream passes signed indices to show a negative one
//! clamps to the head; a `usize` cannot be negative, so those cases pass `0`, the lowest index the type admits.
//! Upstream debounces `tree.changed` in the library; the store emits it straight into its outbox, so these tests assert
//! its presence or absence rather than debounced counts.

use std::collections::{BTreeSet, HashSet};

use agterm_host::Surface;
use agterm_host::fakes::FakeSurface;
use agterm_proto::events::EventKind;
use uuid::Uuid;

use super::support::{add, add_background, ids};
use crate::model::{
    AgentIndicator, AgentStatus, AppStore, EventPayload, ModelEvent, NewSession, NewSessionPlacement, ReorderDirection,
    SaveRequest, Session, SidebarMode, StatusPane, Workspace, WorkspaceOptions,
};

// MARK: - Helpers

/// The ids of every workspace, in order.
fn workspace_ids(store: &AppStore) -> Vec<Uuid> {
    store.workspaces().iter().map(Workspace::id).collect()
}

/// The session ids of the workspace at `index`, in order.
fn ids_at(store: &AppStore, index: usize) -> Vec<Uuid> {
    store.workspaces()[index].sessions.iter().map(Session::id).collect()
}

/// The marked workspaces and whether the focus filter applies.
fn focus_state(store: &AppStore) -> (BTreeSet<Uuid>, bool) {
    (store.focus().ids().clone(), store.focus().enabled())
}

/// The ids of the flagged sessions, in tree order.
fn flagged_ids(store: &AppStore) -> Vec<Uuid> {
    store.flagged_sessions().into_iter().map(Session::id).collect()
}

/// The id of the workspace owning `session`.
fn owner(store: &AppStore, session: Uuid) -> Option<Uuid> {
    store.workspace_for_session(session).map(Workspace::id)
}

/// The id of the workspace owning the selected session.
fn selected_owner(store: &AppStore) -> Option<Uuid> {
    owner(store, store.selected_session_id().expect("a session is selected"))
}

/// Adds a session with `spec`, which the test knows succeeds.
fn add_with(store: &mut AppStore, workspace: Uuid, spec: NewSession) -> Uuid {
    store.add_session(workspace, spec).expect("the workspace exists")
}

/// Adds a selected session at `cwd`, inserted at `at`.
fn add_at(store: &mut AppStore, workspace: Uuid, cwd: &str, at: usize) -> Uuid {
    add_with(
        store,
        workspace,
        NewSession {
            at: Some(at),
            ..NewSession::at_cwd(cwd)
        },
    )
}

/// Adds a selected session at `cwd` with the custom name `name`.
fn add_named(store: &mut AppStore, workspace: Uuid, cwd: &str, name: &str, select: bool) -> Uuid {
    add_with(
        store,
        workspace,
        NewSession {
            name: Some(name.to_owned()),
            select,
            ..NewSession::at_cwd(cwd)
        },
    )
}

/// One workspace holding `a`, `b`, `c` with the middle one selected (upstream's `makeReorderTree`).
fn reorder_tree() -> (AppStore, Vec<Uuid>) {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let sessions = vec![
        add(&mut store, ws, "/a"),
        add(&mut store, ws, "/b"),
        add(&mut store, ws, "/c"),
    ];
    store.select_session(Some(sessions[1]));
    (store, sessions)
}

/// Three empty workspaces `w0`, `w1`, `w2` (upstream's `makeWorkspaceReorderTree`).
fn workspace_reorder_tree() -> (AppStore, Vec<Uuid>) {
    let mut store = AppStore::new();
    let workspaces = vec![
        store.add_workspace("w0"),
        store.add_workspace("w1"),
        store.add_workspace("w2"),
    ];
    (store, workspaces)
}

/// The kinds of `events`, in order.
fn kinds(events: &[ModelEvent]) -> Vec<EventKind> {
    events.iter().map(|event| event.kind).collect()
}

/// The pending events of `kind`, in order.
fn events_of(store: &AppStore, kind: EventKind) -> Vec<ModelEvent> {
    store
        .outbox()
        .events
        .iter()
        .filter(|event| event.kind == kind)
        .cloned()
        .collect()
}

/// The pending lifecycle and selection events (upstream's `selections`).
fn selections(store: &AppStore) -> Vec<ModelEvent> {
    let wanted = [
        EventKind::SessionCreated,
        EventKind::SessionClosed,
        EventKind::SessionSelected,
    ];
    store
        .outbox()
        .events
        .iter()
        .filter(|event| wanted.contains(&event.kind))
        .cloned()
        .collect()
}

/// The `status` wire names of the pending status events.
fn statuses(store: &AppStore) -> Vec<String> {
    events_of(store, EventKind::Status)
        .into_iter()
        .filter_map(|event| event.payload.status)
        .collect()
}

/// Whether a `tree.changed` is pending.
fn tree_changed(store: &AppStore) -> bool {
    !events_of(store, EventKind::TreeChanged).is_empty()
}

/// A store holding one workspace with one selected session, its outbox drained: upstream's library-created window.
fn seeded() -> (AppStore, Uuid, Uuid) {
    let mut store = AppStore::new();
    let ws = store.add_workspace("main");
    let session = add(&mut store, ws, "/tmp");
    store.take_outbox();
    (store, ws, session)
}

// MARK: - Move

#[test]
fn move_session_does_not_tear_down_surface() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    let session = add(&mut store, work, "/a");
    let surface = FakeSurface::new("spy");
    let probe = surface.probe();
    store.session_mut(session).unwrap().surface.replace(Box::new(surface));
    store.move_session(session, personal, None);
    assert_eq!(probe.teardowns(), 0);
    let moved = &store.workspaces()[1].sessions[0];
    assert_eq!(moved.surface.get().map(Surface::pane_token), Some("spy"));
}

#[test]
fn move_session_clamps_negative_index() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    let moved = add(&mut store, work, "/moved");
    let x = add(&mut store, personal, "/x");
    store.move_session(moved, personal, Some(0));
    assert_eq!(ids_at(&store, 1), [moved, x]);
}

#[test]
fn move_session_appends_to_target_workspace() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    let a = add(&mut store, work, "/a");
    let b = add(&mut store, personal, "/b");
    store.move_session(a, personal, None);
    assert!(store.workspaces()[0].sessions.is_empty());
    assert_eq!(ids_at(&store, 1), [b, a]);
}

#[test]
fn move_session_inserts_at_index() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    let moved = add(&mut store, work, "/moved");
    let x = add(&mut store, personal, "/x");
    let y = add(&mut store, personal, "/y");
    store.move_session(moved, personal, Some(1));
    assert_eq!(ids_at(&store, 1), [x, moved, y]);
}

#[test]
fn move_session_clamps_out_of_range_index() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    let moved = add(&mut store, work, "/moved");
    let x = add(&mut store, personal, "/x");
    store.move_session(moved, personal, Some(99));
    assert_eq!(ids_at(&store, 1), [x, moved]);
}

/// Upstream checks reference identity; here the moved value keeps its id, pane identity and fields.
#[test]
fn move_session_preserves_same_instance() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    let session = add(&mut store, work, "/a");
    store.session_mut(session).unwrap().custom_name = Some("build".to_owned());
    let pane = store.session(session).unwrap().pane_identity;
    store.move_session(session, personal, None);
    let moved = &store.workspaces()[1].sessions[0];
    assert_eq!(moved.id(), session);
    assert_eq!(moved.pane_identity, pane);
    assert_eq!(moved.custom_name.as_deref(), Some("build"));
}

// MARK: - Insert at

#[test]
fn add_session_appends_when_index_nil() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let a = add(&mut store, work, "/a");
    let b = add(&mut store, work, "/b");
    assert_eq!(ids(&store, work), [a, b]);
}

#[test]
fn add_session_inserts_at_head() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let a = add(&mut store, work, "/a");
    let head = add_at(&mut store, work, "/head", 0);
    assert_eq!(ids(&store, work), [head, a]);
}

#[test]
fn add_session_inserts_at_middle() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let a = add(&mut store, work, "/a");
    let b = add(&mut store, work, "/b");
    let mid = add_at(&mut store, work, "/mid", 1);
    assert_eq!(ids(&store, work), [a, mid, b]);
}

#[test]
fn add_session_inserts_at_tail() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let a = add(&mut store, work, "/a");
    let tail = add_at(&mut store, work, "/tail", 1);
    assert_eq!(ids(&store, work), [a, tail]);
}

#[test]
fn add_session_clamps_negative_index_to_head() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let a = add(&mut store, work, "/a");
    let clamped = add_at(&mut store, work, "/clamped", 0);
    assert_eq!(ids(&store, work), [clamped, a]);
}

#[test]
fn add_session_clamps_out_of_range_index_to_tail() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let a = add(&mut store, work, "/a");
    let clamped = add_at(&mut store, work, "/clamped", 99);
    assert_eq!(ids(&store, work), [a, clamped]);
}

#[test]
fn add_session_unknown_workspace_returns_nil_with_index() {
    let mut store = AppStore::new();
    let spec = NewSession {
        at: Some(0),
        ..NewSession::at_cwd("/a")
    };
    assert_eq!(store.add_session(Uuid::new_v4(), spec), None);
}

// MARK: - Flags

#[test]
fn set_flag_toggles_and_persists() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let a = add(&mut store, ws, "/a");
    assert!(!store.session(a).unwrap().flagged);
    store.take_outbox();
    store.set_flag(true, a);
    assert!(store.session(a).unwrap().flagged);
    assert_eq!(store.take_outbox().save, SaveRequest::Now);
    store.set_flag(false, a);
    assert!(!store.session(a).unwrap().flagged);
    assert_eq!(store.take_outbox().save, SaveRequest::Now);
}

#[test]
fn set_flag_unknown_id_is_no_op() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let a = add(&mut store, ws, "/a");
    store.set_flag(true, Uuid::new_v4());
    assert!(!store.session(a).unwrap().flagged);
}

#[test]
fn clear_flags_empties_the_set() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    let a = add(&mut store, work, "/a");
    let b = add(&mut store, personal, "/b");
    store.set_flag(true, a);
    store.set_flag(true, b);
    assert_eq!(store.flagged_sessions().len(), 2);
    store.clear_flags();
    assert!(store.flagged_sessions().is_empty());
    assert!(!store.session(a).unwrap().flagged);
    assert!(!store.session(b).unwrap().flagged);
}

#[test]
fn flagged_sessions_returns_matches_in_tree_order() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    let a = add(&mut store, work, "/a");
    add(&mut store, work, "/b"); // unflagged, skipped
    let c = add(&mut store, personal, "/c");
    store.set_flag(true, c);
    store.set_flag(true, a);
    // workspace-then-session order, regardless of flag-setting order
    assert_eq!(flagged_ids(&store), [a, c]);
}

#[test]
fn flagged_session_moved_to_other_workspace_resorts() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    let a = add(&mut store, work, "/a");
    let b = add(&mut store, personal, "/b");
    store.set_flag(true, a);
    store.set_flag(true, b);
    assert_eq!(flagged_ids(&store), [a, b]);
    store.move_session(a, personal, None);
    assert!(store.session(a).unwrap().flagged);
    assert_eq!(flagged_ids(&store), [b, a]);
}

#[test]
fn flagged_sessions_ignore_focus() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    let a = add(&mut store, work, "/a");
    let b = add(&mut store, personal, "/b");
    store.set_flag(true, a);
    store.set_flag(true, b);
    store.set_focused_workspace(work); // focus is orthogonal: it must NOT shrink the flagged set
    assert_eq!(flagged_ids(&store), [a, b]);
}

// MARK: - Workspaces and expansion

#[test]
fn ensure_workspace_reveal_new_workspace_false_preserves_focus_on_create() {
    let mut store = AppStore::new();
    let ws1 = store.add_workspace("one");
    store.set_focused_workspace(ws1);
    assert_eq!(focus_state(&store), (BTreeSet::from([ws1]), true));
    let two = store.add_workspace("two");
    assert_eq!(focus_state(&store), (BTreeSet::from([ws1, two]), true));
    // reveal false backs `session.new --no-select --create-workspace`
    store.set_focused_workspace(ws1);
    let created = store.ensure_workspace("bg", false);
    assert!(created.is_some());
    assert_eq!(focus_state(&store), (BTreeSet::from([ws1]), true));
    let reused = store.ensure_workspace("bg", true);
    assert_eq!(reused, created);
    assert_eq!(focus_state(&store), (BTreeSet::from([ws1]), true));
}

#[test]
fn new_workspace_starts_expanded() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    assert!(store.workspace(work).unwrap().is_expanded);
    assert!(store.workspaces()[0].is_expanded);
}

#[test]
fn new_workspace_collapsed_starts_collapsed() {
    let mut store = AppStore::new();
    let options = WorkspaceOptions {
        collapsed: true,
        ..WorkspaceOptions::default()
    };
    let work = store.add_workspace_with("work", options);
    assert!(!store.workspace(work).unwrap().is_expanded);
    assert!(!store.workspaces()[0].is_expanded);
}

/// Upstream reads the written snapshot's `collapsed` flags; the snapshot is not ported yet, so the write request
/// stands in for the write.
#[test]
fn set_workspaces_expanded_toggles_and_persists() {
    let mut store = AppStore::new();
    let a = store.add_workspace("a");
    store.add_workspace("b");
    store.take_outbox();
    store.set_workspaces_expanded(&HashSet::from([a]));
    assert!(store.workspaces()[0].is_expanded);
    assert!(!store.workspaces()[1].is_expanded);
    assert_eq!(store.outbox().save, SaveRequest::Now);
}

#[test]
fn set_workspaces_expanded_unchanged_does_not_write() {
    let mut store = AppStore::new();
    store.add_workspace("a");
    store.set_workspaces_expanded(&HashSet::new()); // a collapsed, saved
    assert_eq!(store.take_outbox().save, SaveRequest::Now);
    store.set_workspaces_expanded(&HashSet::new()); // same state, no save
    assert_eq!(store.outbox().save, SaveRequest::None);
}

#[test]
fn set_workspace_expanded_toggles_single_workspace_and_persists() {
    let mut store = AppStore::new();
    let a = store.add_workspace("a");
    store.add_workspace("b");
    store.take_outbox();
    store.set_workspace_expanded(a, false);
    assert!(!store.workspaces()[0].is_expanded);
    assert!(store.workspaces()[1].is_expanded); // b untouched: per-workspace, not whole-tree
    assert_eq!(store.outbox().save, SaveRequest::Now);
}

#[test]
fn set_workspace_expanded_unknown_or_unchanged_is_no_op() {
    let mut store = AppStore::new();
    let a = store.add_workspace("a");
    store.take_outbox();
    store.set_workspace_expanded(Uuid::new_v4(), false); // unknown id
    assert!(store.workspaces()[0].is_expanded);
    store.set_workspace_expanded(a, true); // already expanded
    assert!(store.workspaces()[0].is_expanded);
    assert_eq!(store.outbox().save, SaveRequest::None);
}

// MARK: - Focus filter

#[test]
fn select_session_while_unfocused_is_no_op_on_focus() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let a = add(&mut store, work, "/a");
    store.select_session(Some(a));
    assert_eq!(focus_state(&store), (BTreeSet::new(), false));
}

#[test]
fn close_focused_session_revealing_other_workspace_disables_the_filter() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    let only = add(&mut store, work, "/only");
    add(&mut store, personal, "/other");
    store.select_session(Some(only));
    store.set_focused_workspace(work);
    store.close_session(only); // reselects the personal session, outside the now-empty focused work
    assert!(store.active_session().is_some());
    assert_eq!(selected_owner(&store), Some(personal));
    assert_eq!(focus_state(&store), (BTreeSet::from([work]), false));
}

#[test]
fn removing_the_only_marked_workspace_reselects_outside_and_disables_the_filter() {
    let mut store = AppStore::new();
    let a = store.add_workspace("a");
    let b = store.add_workspace("b");
    let active_in_a = add(&mut store, a, "/a");
    add_background(&mut store, b, "/b");
    store.select_session(Some(active_in_a));
    store.set_focused_workspace(a);
    assert_eq!(store.selected_session_id(), Some(active_in_a));

    store.remove_workspace(a);
    assert_eq!(selected_owner(&store), Some(b));
    assert_eq!(focus_state(&store), (BTreeSet::new(), false));
}

#[test]
fn removing_a_marked_workspace_stays_inside_the_marked_set() {
    let mut store = AppStore::new();
    let doomed = store.add_workspace("doomed");
    let unmarked = store.add_workspace("unmarked");
    let also_marked = store.add_workspace("also-marked");
    let active = add(&mut store, doomed, "/a");
    add_background(&mut store, unmarked, "/stray");
    let survivor = add_background(&mut store, also_marked, "/keep");
    store.select_session(Some(active));
    store.set_focus_membership(doomed, true);
    store.set_focus_membership(also_marked, true);
    store.set_focus_enabled(true);

    store.remove_workspace(doomed);

    assert_eq!(store.selected_session_id(), Some(survivor));
    assert_eq!(focus_state(&store), (BTreeSet::from([also_marked]), true));
}

#[test]
fn removing_the_active_workspace_in_flagged_mode_stays_inside_the_flagged_set() {
    let mut store = AppStore::new();
    let doomed = store.add_workspace("doomed");
    let unflagged = store.add_workspace("unflagged");
    let elsewhere = store.add_workspace("elsewhere");
    let active = add(&mut store, doomed, "/a");
    add_background(&mut store, unflagged, "/stray");
    let survivor = add_background(&mut store, elsewhere, "/keep");
    store.set_flag(true, active);
    store.set_flag(true, survivor);
    store.set_sidebar_mode(SidebarMode::Flagged);
    store.select_session(Some(active));

    store.remove_workspace(doomed);

    assert_eq!(store.selected_session_id(), Some(survivor));
    assert_eq!(flagged_ids(&store), [survivor]);
}

#[test]
fn removing_the_last_flagged_workspace_falls_back_to_an_unflagged_survivor() {
    let mut store = AppStore::new();
    let doomed = store.add_workspace("doomed");
    let elsewhere = store.add_workspace("elsewhere");
    let active = add(&mut store, doomed, "/a");
    let unflagged = add_background(&mut store, elsewhere, "/keep");
    store.set_flag(true, active);
    store.set_sidebar_mode(SidebarMode::Flagged);
    store.select_session(Some(active));

    store.remove_workspace(doomed);

    assert!(store.navigable_sessions().is_empty());
    assert_eq!(store.selected_session_id(), Some(unflagged));
}

#[test]
fn removing_a_marked_workspace_prefers_the_most_recent_visible_survivor() {
    let mut store = AppStore::new();
    let doomed = store.add_workspace("doomed");
    let also_marked = store.add_workspace("also-marked");
    let active = add(&mut store, doomed, "/a");
    let first = add_background(&mut store, also_marked, "/first");
    let recent = add_background(&mut store, also_marked, "/recent");
    store.select_session(Some(recent));
    store.select_session(Some(active));
    store.set_focus_membership(doomed, true);
    store.set_focus_membership(also_marked, true);
    store.set_focus_enabled(true);

    store.remove_workspace(doomed);

    assert_eq!(store.selected_session_id(), Some(recent));
    assert_ne!(first, recent);
}

#[test]
fn add_session_to_other_workspace_while_focused_disables_the_filter() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let other = store.add_workspace("other");
    add(&mut store, work, "/w");
    store.set_focused_workspace(work);
    let created = add(&mut store, other, "/o"); // a control add into another workspace
    assert_eq!(store.selected_session_id(), Some(created));
    assert_eq!(focus_state(&store), (BTreeSet::from([work]), false));
}

#[test]
fn add_session_inside_focused_workspace_keeps_focus() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    add(&mut store, work, "/a");
    store.set_focused_workspace(work);
    let created = add(&mut store, work, "/b"); // the GUI new-session path lands here
    assert_eq!(store.selected_session_id(), Some(created));
    assert_eq!(focus_state(&store), (BTreeSet::from([work]), true));
}

#[test]
fn select_nil_while_focused_keeps_focus() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let a = add(&mut store, work, "/a");
    store.select_session(Some(a));
    store.set_focused_workspace(work);
    store.select_session(None); // deselect reveals nothing, so focus is retained
    assert_eq!(focus_state(&store), (BTreeSet::from([work]), true));
}

#[test]
fn move_active_session_out_of_focused_workspace_disables_the_filter() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let other = store.add_workspace("other");
    let a = add(&mut store, work, "/a");
    store.select_session(Some(a));
    store.set_focused_workspace(work);
    store.move_session(a, other, None); // the active session leaves the focused workspace
    assert_eq!(store.selected_session_id(), Some(a));
    assert_eq!(focus_state(&store), (BTreeSet::from([work]), false));
}

#[test]
fn move_non_active_session_out_of_focused_workspace_keeps_focus() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let other = store.add_workspace("other");
    let a = add(&mut store, work, "/a");
    let b = add(&mut store, work, "/b");
    store.select_session(Some(a));
    store.set_focused_workspace(work);
    store.move_session(b, other, None); // a non-active session leaves; focus must stand
    assert_eq!(store.selected_session_id(), Some(a));
    assert_eq!(focus_state(&store), (BTreeSet::from([work]), true));
}

#[test]
fn add_workspace_while_focused_joins_the_set_and_reveals_new() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    store.add_workspace("hidden");
    add(&mut store, work, "/a");
    store.set_focused_workspace(work);
    let fresh = store.add_workspace("fresh"); // a new (empty) workspace must become visible
    assert_eq!(focus_state(&store), (BTreeSet::from([work, fresh]), true)); // the filter survives
    let visible: Vec<Uuid> = store.visible_workspaces().into_iter().map(Workspace::id).collect();
    assert_eq!(visible, [work, fresh]); // `hidden` stays filtered out
}

/// Upstream deletes the snapshot file and checks the no-op setters do not recreate it; here they request no write.
#[test]
fn set_same_values_are_no_op_writes_and_stable() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let a = add(&mut store, ws, "/a");
    store.set_flag(true, a);
    store.set_sidebar_mode(SidebarMode::Flagged);
    store.set_focused_workspace(ws);
    store.take_outbox();
    store.set_flag(true, a); // unchanged
    store.set_sidebar_mode(SidebarMode::Flagged); // unchanged
    store.set_focused_workspace(ws); // unchanged
    assert_eq!(store.outbox().save, SaveRequest::None); // no write happened
    assert!(store.session(a).unwrap().flagged);
    assert_eq!(store.sidebar().mode, SidebarMode::Flagged);
    assert_eq!(focus_state(&store), (BTreeSet::from([ws]), true)); // stable across the no-op setters
}

// MARK: - Move and selection

#[test]
fn move_active_session_keeps_it_selected() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    let a = add(&mut store, work, "/a");
    store.select_session(Some(a));
    store.move_session(a, personal, None);
    assert_eq!(store.selected_session_id(), Some(a));
    assert_eq!(store.active_session().map(Session::id), Some(a));
    assert_eq!(owner(&store, a), Some(personal));
}

#[test]
fn move_non_active_session_leaves_selection_untouched() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    let a = add(&mut store, work, "/a");
    let b = add(&mut store, work, "/b");
    store.select_session(Some(b));
    store.move_session(a, personal, None);
    assert_eq!(store.selected_session_id(), Some(b));
}

#[test]
fn move_last_session_leaves_source_empty() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    let only = add(&mut store, work, "/only");
    store.select_session(Some(only));
    store.move_session(only, personal, None);
    assert!(store.workspaces()[0].sessions.is_empty());
    assert_eq!(ids_at(&store, 1), [only]);
    assert_eq!(store.selected_session_id(), Some(only));
}

#[test]
fn move_session_within_same_workspace_reorders() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let a = add(&mut store, ws, "/a");
    let b = add(&mut store, ws, "/b");
    let c = add(&mut store, ws, "/c");
    store.move_session(a, ws, Some(2));
    assert_eq!(ids(&store, ws), [b, c, a]);
}

#[test]
fn move_session_within_same_workspace_to_current_slot_is_no_op() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let a = add(&mut store, ws, "/a");
    let b = add(&mut store, ws, "/b");
    store.move_session(a, ws, Some(0));
    assert_eq!(ids(&store, ws), [a, b]);
}

#[test]
fn move_unknown_session_is_ignored() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    let a = add(&mut store, work, "/a");
    store.move_session(Uuid::new_v4(), personal, None);
    assert_eq!(ids(&store, work), [a]);
    assert!(ids(&store, personal).is_empty());
}

#[test]
fn move_to_unknown_workspace_is_ignored() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let a = add(&mut store, work, "/a");
    store.move_session(a, Uuid::new_v4(), None);
    assert_eq!(ids(&store, work), [a]);
}

// MARK: - Session reorder

#[test]
fn reorder_session_up() {
    let (mut store, s) = reorder_tree();
    store.reorder_session(s[1], ReorderDirection::Up);
    assert_eq!(ids_at(&store, 0), [s[1], s[0], s[2]]);
    assert_eq!(store.selected_session_id(), Some(s[1]));
}

#[test]
fn reorder_session_down() {
    let (mut store, s) = reorder_tree();
    store.reorder_session(s[1], ReorderDirection::Down);
    assert_eq!(ids_at(&store, 0), [s[0], s[2], s[1]]);
    assert_eq!(store.selected_session_id(), Some(s[1]));
}

#[test]
fn reorder_session_top() {
    let (mut store, s) = reorder_tree();
    store.reorder_session(s[2], ReorderDirection::Top);
    assert_eq!(ids_at(&store, 0), [s[2], s[0], s[1]]);
}

#[test]
fn reorder_session_bottom() {
    let (mut store, s) = reorder_tree();
    store.reorder_session(s[0], ReorderDirection::Bottom);
    assert_eq!(ids_at(&store, 0), [s[1], s[2], s[0]]);
}

#[test]
fn reorder_session_up_at_top_is_no_op() {
    let (mut store, s) = reorder_tree();
    store.reorder_session(s[0], ReorderDirection::Up);
    assert_eq!(ids_at(&store, 0), s);
    store.reorder_session(s[0], ReorderDirection::Top);
    assert_eq!(ids_at(&store, 0), s);
}

#[test]
fn reorder_session_down_at_bottom_is_no_op() {
    let (mut store, s) = reorder_tree();
    store.reorder_session(s[2], ReorderDirection::Down);
    assert_eq!(ids_at(&store, 0), s);
    store.reorder_session(s[2], ReorderDirection::Bottom);
    assert_eq!(ids_at(&store, 0), s);
}

#[test]
fn reorder_unknown_session_is_ignored() {
    let (mut store, s) = reorder_tree();
    store.reorder_session(Uuid::new_v4(), ReorderDirection::Up);
    assert_eq!(ids_at(&store, 0), s);
}

// MARK: - Session location

#[test]
fn session_location_reports_workspace_index_and_count() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    let a = add(&mut store, work, "/a");
    let b = add(&mut store, work, "/b");
    let c = add(&mut store, personal, "/c");

    let loc_a = store.session_location(a).unwrap();
    assert_eq!((loc_a.workspace, loc_a.index, loc_a.count), (work, 0, 2));
    let loc_b = store.session_location(b).unwrap();
    assert_eq!((loc_b.workspace, loc_b.index, loc_b.count), (work, 1, 2));
    let loc_c = store.session_location(c).unwrap();
    assert_eq!((loc_c.workspace, loc_c.index, loc_c.count), (personal, 0, 1));
}

#[test]
fn session_location_of_unknown_session_is_nil() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    add(&mut store, work, "/a");
    assert_eq!(store.session_location(Uuid::new_v4()), None);
}

// MARK: - Workspace reorder

#[test]
fn move_workspace_reorders_within_bounds() {
    let (mut store, w) = workspace_reorder_tree();
    store.move_workspace(w[0], 2);
    assert_eq!(workspace_ids(&store), [w[1], w[2], w[0]]);
}

#[test]
fn move_workspace_clamps_index_at_both_ends() {
    let (mut store, w) = workspace_reorder_tree();
    store.move_workspace(w[1], 99);
    assert_eq!(workspace_ids(&store), [w[0], w[2], w[1]]);
    store.move_workspace(w[1], 0);
    assert_eq!(workspace_ids(&store), [w[1], w[0], w[2]]);
}

#[test]
fn move_unknown_workspace_is_ignored() {
    let (mut store, w) = workspace_reorder_tree();
    store.move_workspace(Uuid::new_v4(), 0);
    assert_eq!(workspace_ids(&store), w);
}

#[test]
fn reorder_workspace_up() {
    let (mut store, w) = workspace_reorder_tree();
    store.reorder_workspace(w[1], ReorderDirection::Up);
    assert_eq!(workspace_ids(&store), [w[1], w[0], w[2]]);
}

#[test]
fn reorder_workspace_down() {
    let (mut store, w) = workspace_reorder_tree();
    store.reorder_workspace(w[1], ReorderDirection::Down);
    assert_eq!(workspace_ids(&store), [w[0], w[2], w[1]]);
}

#[test]
fn reorder_workspace_top() {
    let (mut store, w) = workspace_reorder_tree();
    store.reorder_workspace(w[2], ReorderDirection::Top);
    assert_eq!(workspace_ids(&store), [w[2], w[0], w[1]]);
}

#[test]
fn reorder_workspace_bottom() {
    let (mut store, w) = workspace_reorder_tree();
    store.reorder_workspace(w[0], ReorderDirection::Bottom);
    assert_eq!(workspace_ids(&store), [w[1], w[2], w[0]]);
}

#[test]
fn reorder_workspace_at_ends_is_no_op() {
    let (mut store, w) = workspace_reorder_tree();
    store.reorder_workspace(w[0], ReorderDirection::Up);
    store.reorder_workspace(w[0], ReorderDirection::Top);
    store.reorder_workspace(w[2], ReorderDirection::Down);
    store.reorder_workspace(w[2], ReorderDirection::Bottom);
    assert_eq!(workspace_ids(&store), w);
}

#[test]
fn reorder_workspace_keeps_selected_session() {
    let (mut store, w) = workspace_reorder_tree();
    let session = add(&mut store, w[0], "/a");
    store.select_session(Some(session));
    store.reorder_workspace(w[0], ReorderDirection::Bottom);
    assert_eq!(workspace_ids(&store), [w[1], w[2], w[0]]);
    assert_eq!(store.selected_session_id(), Some(session));
}

#[test]
fn move_workspace_keeps_selected_session() {
    let (mut store, w) = workspace_reorder_tree();
    let session = add(&mut store, w[1], "/a");
    store.select_session(Some(session));
    store.move_workspace(w[1], 0);
    assert_eq!(workspace_ids(&store), [w[1], w[0], w[2]]);
    assert_eq!(store.selected_session_id(), Some(session));
}

// MARK: - New-session placement

#[test]
fn after_current_inserts_behind_the_selected_session() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let one = add(&mut store, ws, "/1");
    add(&mut store, ws, "/2");
    store.select_session(Some(one));
    assert_eq!(
        store.new_session_insertion_index(ws, NewSessionPlacement::AfterCurrent),
        Some(1)
    );
}

#[test]
fn end_always_appends() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let one = add(&mut store, ws, "/1");
    add(&mut store, ws, "/2");
    store.select_session(Some(one));
    assert_eq!(store.new_session_insertion_index(ws, NewSessionPlacement::End), None);
}

#[test]
fn after_current_appends_into_a_workspace_without_the_selection() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let other = store.add_workspace("other");
    let selected = add(&mut store, work, "/1");
    add(&mut store, other, "/x");
    store.select_session(Some(selected));
    assert_eq!(
        store.new_session_insertion_index(other, NewSessionPlacement::AfterCurrent),
        None
    );
}

#[test]
fn after_current_appends_with_no_selection() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    add(&mut store, ws, "/1");
    store.select_session(None);
    assert_eq!(
        store.new_session_insertion_index(ws, NewSessionPlacement::AfterCurrent),
        None
    );
}

#[test]
fn chained_after_current_inserts_keep_creation_order() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let one = add(&mut store, ws, "/1");
    let last = add(&mut store, ws, "/9");
    store.select_session(Some(one));

    for cwd in ["/a", "/b"] {
        let at = store.new_session_insertion_index(ws, NewSessionPlacement::AfterCurrent);
        add_with(
            &mut store,
            ws,
            NewSession {
                at,
                ..NewSession::at_cwd(cwd)
            },
        );
    }

    let sessions = &store.workspace(ws).unwrap().sessions;
    let cwds: Vec<&str> = sessions.iter().map(Session::initial_cwd).collect();
    assert_eq!(cwds, ["/1", "/a", "/b", "/9"]);
    assert_eq!(sessions.last().map(Session::id), Some(last));
}

// MARK: - Status events (AppStoreEventTests)

/// Upstream also checks the payload's `blink`, `color` and `shape`; the model's payload carries none of them and its
/// indicator has no shape, so only status, pane and name are compared here.
#[test]
fn normalized_status_changes_emit_complete_payloads_and_idle_edge() {
    let (mut store, _, session) = seeded();
    let active = AgentIndicator {
        status: AgentStatus::Active,
        status_pane: Some(StatusPane::Right),
        ..AgentIndicator::default()
    };
    let blocked = AgentIndicator {
        status: AgentStatus::Blocked,
        blink: true,
        color: Some("#aabbcc".to_owned()),
        status_pane: Some(StatusPane::Scratch),
        ..AgentIndicator::default()
    };
    let completed = AgentIndicator {
        status: AgentStatus::Completed,
        ..AgentIndicator::default()
    };
    store.set_agent_indicator(active, session);
    store.set_agent_indicator(blocked, session);
    store.set_agent_indicator(completed, session);
    store.set_agent_indicator(AgentIndicator::default(), session);

    assert_eq!(statuses(&store), ["active", "blocked", "completed", "idle"]);
    let events = events_of(&store, EventKind::Status);
    // a `right` tag on a splitless session folds to `left`
    assert_eq!(events[0].payload.pane.as_deref(), Some("left"));
    assert_eq!(events[1].payload.pane.as_deref(), Some("scratch"));
    let name = store.session(session).unwrap().display_name();
    assert!(
        events
            .iter()
            .all(|event| event.payload.name.as_deref() == Some(name.as_str()))
    );
}

#[test]
fn same_normalized_status_and_unknown_session_do_not_emit() {
    let (mut store, _, session) = seeded();
    let indicator = AgentIndicator {
        status: AgentStatus::Blocked,
        blink: true,
        status_pane: Some(StatusPane::Right),
        ..AgentIndicator::default()
    };

    store.set_agent_indicator(indicator.clone(), session);
    store.set_agent_indicator(indicator.clone(), session);
    store.set_agent_indicator(indicator, Uuid::new_v4());

    assert_eq!(events_of(&store, EventKind::Status).len(), 1);
    assert!(store.session(session).unwrap().status_changed_at.is_some());
}

#[test]
fn auto_reset_visit_routes_through_status_setter_and_emits_idle() {
    let (mut store, _, session) = seeded();
    let indicator = AgentIndicator {
        status: AgentStatus::Completed,
        auto_reset: true,
        ..AgentIndicator::default()
    };
    store.set_agent_indicator(indicator, session);
    store.select_session(Some(session));
    assert_eq!(statuses(&store), ["completed", "idle"]);
}

// MARK: - Lifecycle and selection events (AppStoreEventTests)

#[test]
fn workspace_removal_emits_closed_sessions_in_tree_order() {
    let (mut store, _, _) = seeded();
    let workspace = store.add_workspace("batch");
    let first = add_named(&mut store, workspace, "/one", "one", true);
    let second = add_named(&mut store, workspace, "/two", "two", true);
    store.take_outbox();

    store.remove_workspace(workspace);

    let closed = events_of(&store, EventKind::SessionClosed);
    let sessions: Vec<Option<Uuid>> = closed.iter().map(|event| event.session).collect();
    assert_eq!(sessions, [Some(first), Some(second)]);
    let names: Vec<Option<&str>> = closed.iter().map(|event| event.payload.name.as_deref()).collect();
    assert_eq!(names, [Some("one"), Some("two")]);
}

#[test]
fn selecting_another_session_emits_it_with_the_one_that_lost_the_selection() {
    let (mut store, workspace, first) = seeded();
    let second = add_named(&mut store, workspace, "/tmp", "api", false);
    store.take_outbox();

    store.select_session(Some(second));
    store.select_session(Some(second));
    store.select_session(None);

    let events = selections(&store);
    assert_eq!(kinds(&events), [EventKind::SessionSelected, EventKind::SessionSelected]);
    assert_eq!(events[0].session, Some(second));
    assert_eq!(events[0].workspace, Some(workspace));
    let selected = EventPayload {
        name: Some("api".to_owned()),
        previous: Some(first),
        ..EventPayload::default()
    };
    assert_eq!(events[0].payload, selected);
    assert_eq!(events[1].session, None);
    let cleared = EventPayload {
        previous: Some(second),
        ..EventPayload::default()
    };
    assert_eq!(events[1].payload, cleared);
}

#[test]
fn a_selected_add_is_created_before_it_is_selected_and_a_background_add_selects_nothing() {
    let (mut store, workspace, first) = seeded();

    let added = add(&mut store, workspace, "/tmp");
    add_background(&mut store, workspace, "/tmp");

    let events = selections(&store);
    assert_eq!(
        kinds(&events),
        [
            EventKind::SessionCreated,
            EventKind::SessionSelected,
            EventKind::SessionCreated
        ]
    );
    assert_eq!(events[1].session, Some(added));
    assert_eq!(events[1].payload.previous, Some(first));
}

#[test]
fn closing_the_selected_session_emits_the_reselection() {
    let (mut store, workspace, first) = seeded();
    let added = add(&mut store, workspace, "/tmp");
    store.take_outbox();

    store.close_session(added);

    let events = selections(&store);
    assert_eq!(kinds(&events), [EventKind::SessionClosed, EventKind::SessionSelected]);
    assert_eq!(events[1].session, Some(first));
    assert_eq!(events[1].payload.previous, Some(added));
}

// MARK: - tree.changed (AppStoreEventTests)

/// Upstream counts the debounced events after each flush; here each step's pending `tree.changed` is present or not.
#[test]
fn setting_session_context_emits_tree_changed_only_when_it_actually_changes() {
    let (mut store, _, session) = seeded();

    assert!(store.set_context(Some("PR #517".to_owned()), session));
    assert!(tree_changed(&store));
    store.take_outbox();

    assert!(!store.set_context(Some("PR #517".to_owned()), session));
    assert!(!tree_changed(&store));

    assert!(store.set_context(None, session));
    assert!(tree_changed(&store));
    store.take_outbox();

    assert!(!store.set_context(None, session));
    assert!(!tree_changed(&store));
    assert_eq!(store.session(session).unwrap().context, None);
}

/// Upstream's per-window coalescing and the empty ring before the flush belong to the library's debounce; at the
/// store level each window's structural change leaves its own `tree.changed` pending.
#[test]
fn tree_changes_coalesce_per_window_and_stay_independent() {
    let (mut first, _, _) = seeded();
    let (mut second, second_workspace, _) = seeded();

    let workspace = first.add_workspace("one");
    first.rename_workspace(workspace, "renamed");
    add(&mut second, second_workspace, "/tmp");

    assert!(tree_changed(&first));
    assert!(tree_changed(&second));
}

#[test]
fn status_and_selection_do_not_schedule_structural_invalidation() {
    let (mut store, _, session) = seeded();
    let active = AgentIndicator {
        status: AgentStatus::Active,
        ..AgentIndicator::default()
    };

    store.set_agent_indicator(active, session);
    store.select_session(Some(session));

    assert!(!tree_changed(&store));
}
