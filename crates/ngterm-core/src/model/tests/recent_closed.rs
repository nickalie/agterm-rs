//! Open Recent: what a close records and how an entry reopens, ported from upstream's `RecentClosedTests.swift`, the
//! Open Recent cases of `WindowLibraryTests.swift`, `AppStoreTests.swift`, `AppStoreFocusTests.swift` and
//! `AppStoreCurrentWorkspaceTests.swift`. The file format and its store are tested in `persist::recent_closed` and
//! `tests/persistence.rs`.
//!
//! Upstream reads entries back from the shared `RecentClosedStore`; here they come from the in-memory list a store
//! shares with its library ([`AppStore::recent_closed_items`], [`WindowLibrary::recent_closed_items`]).

use std::collections::BTreeSet;
use std::time::SystemTime;

use ngterm_host::fakes::{FakeSurface, FakeSurfaceProbe};
use uuid::Uuid;

use super::support::{add, add_background, ids};
use crate::model::snapshot::{session_snapshot, workspace_snapshot};
use crate::model::{
    AppStore, NewSession, RecentClosedChange, RecentClosedRestore, WindowInfo, WindowLibrary, Workspace,
    WorkspaceOptions,
};
use crate::persist::{
    RecentClosedItem, RecentClosedKind, RecentClosedSession, RecentClosedWorkspace, SessionSnapshot, Snapshot,
    WorkspaceSnapshot,
};

const HOME: &str = "/home/tester";

fn named(store: &mut AppStore, workspace: Uuid, cwd: &str, name: &str) -> Uuid {
    let spec = NewSession {
        name: Some(name.to_owned()),
        ..NewSession::at_cwd(cwd)
    };
    store.add_session(workspace, spec).expect("the workspace exists")
}

fn remote(store: &mut AppStore, workspace: Uuid, cwd: &str) -> Uuid {
    let spec = NewSession {
        remote_host: Some("buildbox".to_owned()),
        ..NewSession::at_cwd(cwd)
    };
    store.add_session(workspace, spec).expect("the workspace exists")
}

fn with_surface(store: &mut AppStore, session: Uuid) -> FakeSurfaceProbe {
    let surface = FakeSurface::new(session.to_string());
    let probe = surface.probe();
    let slot = &mut store.session_mut(session).expect("the session exists").surface;
    assert!(slot.replace(Box::new(surface)).is_none());
    probe
}

fn marked(ids: &[Uuid]) -> BTreeSet<Uuid> {
    ids.iter().copied().collect()
}

fn workspace_ids(store: &AppStore) -> Vec<Uuid> {
    store.workspaces().iter().map(Workspace::id).collect()
}

fn visible_ids(store: &AppStore) -> Vec<Uuid> {
    store
        .visible_workspaces()
        .iter()
        .map(|workspace| workspace.id())
        .collect()
}

fn latest(store: &AppStore) -> Uuid {
    store.pending_close_summary().expect("a pending close").id
}

/// The newest entry of workspace `id`.
fn workspace_entry(items: &[RecentClosedItem], id: Uuid) -> RecentClosedItem {
    items
        .iter()
        .find(|item| item.workspace.as_ref().is_some_and(|recent| recent.snapshot.id == id))
        .cloned()
        .expect("an entry for the workspace")
}

/// The newest entry of session `id`.
fn session_entry(items: &[RecentClosedItem], id: Uuid) -> RecentClosedItem {
    items
        .iter()
        .find(|item| item.session.as_ref().is_some_and(|recent| recent.snapshot.id == id))
        .cloned()
        .expect("an entry for the session")
}

fn titles(items: &[RecentClosedItem]) -> Vec<String> {
    items.iter().map(|item| item.title.clone()).collect()
}

/// How many objects every open window holds under `session`.
fn copies(library: &WindowLibrary, session: Uuid) -> usize {
    library
        .all_open_sessions()
        .iter()
        .filter(|candidate| candidate.id() == session)
        .count()
}

fn session_item(workspace: Uuid, name: &str, snapshot: SessionSnapshot) -> RecentClosedItem {
    RecentClosedItem {
        id: Uuid::new_v4(),
        kind: RecentClosedKind::Session,
        title: name.to_owned(),
        subtitle: Some(name.to_owned()),
        closed_at: SystemTime::now(),
        session: Some(RecentClosedSession {
            workspace_id: workspace,
            workspace_name: name.to_owned(),
            workspace_index: 0,
            session_index: 0,
            snapshot,
        }),
        workspace: None,
    }
}

fn workspace_item(snapshot: WorkspaceSnapshot, selected: Option<Uuid>) -> RecentClosedItem {
    RecentClosedItem {
        id: Uuid::new_v4(),
        kind: RecentClosedKind::Workspace,
        title: snapshot.name.clone(),
        subtitle: None,
        closed_at: SystemTime::now(),
        session: None,
        workspace: Some(RecentClosedWorkspace {
            snapshot,
            selected_session_id: selected,
            focus_member: None,
        }),
    }
}

// MARK: - Recording (RecentClosedTests)

#[test]
fn closing_a_remote_session_records_nothing_to_reopen() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let session = remote(&mut store, ws, "/a");
    store.close_session(session);
    assert!(store.recent_closed_items().is_empty());
    assert!(store.take_outbox().recent_closed.is_empty());
}

#[test]
fn closing_a_workspace_records_and_counts_only_its_local_sessions() {
    let mut store = AppStore::new();
    store.add_workspace("keep");
    let ws = store.add_workspace("mixed");
    let local = add(&mut store, ws, "/a");
    remote(&mut store, ws, "/b");

    store.remove_workspace(ws);

    let item = workspace_entry(&store.recent_closed_items(), ws);
    let recent = item.workspace.as_ref().unwrap();
    assert_eq!(
        recent
            .snapshot
            .sessions
            .iter()
            .map(|session| session.id)
            .collect::<Vec<_>>(),
        [local]
    );
    assert_eq!(
        item.subtitle.as_deref(),
        Some("1 session"),
        "the count describes what a reopen restores"
    );
    assert_eq!((item.kind, item.title.as_str()), (RecentClosedKind::Workspace, "mixed"));
}

#[test]
fn a_workspace_selection_pointing_at_a_remote_session_is_not_recorded() {
    let mut store = AppStore::new();
    store.add_workspace("keep");
    let ws = store.add_workspace("mixed");
    add(&mut store, ws, "/a");
    let remote = remote(&mut store, ws, "/b");
    store.select_session(Some(remote));

    store.remove_workspace(ws);

    let item = workspace_entry(&store.recent_closed_items(), ws);
    assert_eq!(item.workspace.unwrap().selected_session_id, None);
}

#[test]
fn a_closed_session_records_its_place_and_goes_out_through_the_outbox() {
    let mut store = AppStore::new();
    store.add_workspace("first");
    let ws = store.add_workspace("project");
    add(&mut store, ws, "/first");
    let session = named(&mut store, ws, "/project", "api");
    store.take_outbox();

    store.close_session(session);

    let item = session_entry(&store.recent_closed_items(), session);
    assert_eq!((item.kind, item.title.as_str()), (RecentClosedKind::Session, "api"));
    assert_eq!(item.subtitle.as_deref(), Some("project"));
    let recent = item.session.as_ref().unwrap();
    assert_eq!((recent.workspace_id, recent.workspace_name.as_str()), (ws, "project"));
    assert_eq!((recent.workspace_index, recent.session_index), (1, 1));
    assert_eq!(recent.snapshot.cwd, "/project");
    assert_eq!(
        store.take_outbox().recent_closed,
        [RecentClosedChange::Record(Box::new(item))]
    );
}

#[test]
fn reopening_a_closed_session_restores_the_override_without_arming_it() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let session = add(&mut store, ws, "/a");
    store.session_mut(session).unwrap().restore.command = Some("claude --resume abc".to_owned());
    let probe = with_surface(&mut store, session);
    store.close_session(session);
    assert_eq!(probe.teardowns(), 1);

    let item = session_entry(&store.recent_closed_items(), session);
    assert!(store.restore_recent_closed(&item));

    let reopened = store.session(session).unwrap();
    assert!(
        !reopened.surface.is_present(),
        "a fresh object rebuilt from the snapshot"
    );
    assert!(reopened.was_restored);
    assert_eq!(reopened.restore.command.as_deref(), Some("claude --resume abc"));
    assert_eq!(reopened.restore.pending_command, None);
    assert_eq!(reopened.split_restore.pending_command, None);
}

// MARK: - Reopening into a store (AppStoreTests)

#[test]
fn reopening_a_session_then_undoing_its_workspace_close_keeps_one_workspace() {
    let mut store = AppStore::new();
    let doomed = store.add_workspace("doomed");
    store.add_workspace("keep");
    let first = add(&mut store, doomed, "/a");
    let second = add(&mut store, doomed, "/b");
    let second_probe = with_surface(&mut store, second);
    let first_snapshot = session_snapshot(store.session(first).unwrap());

    assert!(store.soft_close_session(first));
    store.finalize_pending_close(latest(&store));
    assert!(store.soft_remove_workspace(doomed));
    let workspace_close = latest(&store);

    // the finalized session reopens on its own, rebuilding the workspace as a shell
    let item = session_item(doomed, "doomed", first_snapshot);
    assert!(store.restore_recent_closed(&item));
    assert!(store.undo_pending_close(Some(workspace_close)));

    assert_eq!(workspace_ids(&store).iter().filter(|id| **id == doomed).count(), 1);
    assert_eq!(marked(&ids(&store, doomed)), marked(&[first, second]));
    assert_eq!(ids(&store, doomed).len(), 2);
    assert_eq!(store.selected_session_id(), Some(second));
    assert!(
        store.session(second).unwrap().surface.is_present(),
        "the merged-in one is the live object"
    );
    assert_eq!(second_probe.teardowns(), 0);
    assert!(!store.session(first).unwrap().surface.is_present());
}

#[test]
fn reopening_a_workspace_merges_its_missing_sessions_into_the_rebuilt_shell() {
    let mut store = AppStore::new();
    let doomed = store.add_workspace("doomed");
    store.add_workspace("keep");
    let first = add(&mut store, doomed, "/a");
    let second = add(&mut store, doomed, "/b");
    let first_snapshot = session_snapshot(store.session(first).unwrap());
    // taken while it holds both, so it overlaps the shell the session reopen rebuilds
    let snapshot = workspace_snapshot(store.workspace(doomed).unwrap());

    assert!(store.soft_close_session(first));
    store.finalize_pending_close(latest(&store));
    assert!(store.soft_remove_workspace(doomed));
    store.finalize_pending_close(latest(&store));

    assert!(store.restore_recent_closed(&session_item(doomed, "doomed", first_snapshot)));
    store.take_outbox();
    assert!(store.restore_recent_closed(&workspace_item(snapshot, Some(second))));

    // a merge without the live-session filter would append a second copy of `first`
    assert_eq!(ids(&store, doomed).len(), 2);
    assert_eq!(marked(&ids(&store, doomed)), marked(&[first, second]));
    assert_eq!(store.selected_session_id(), Some(second));
    assert_eq!(
        store.outbox().save,
        crate::model::SaveRequest::Now,
        "the merge writes at once, ahead of the debounced selection save"
    );
}

#[test]
fn reopening_a_workspace_rebuilds_the_members_a_pending_session_close_does_not_hold() {
    let mut store = AppStore::new();
    let doomed = store.add_workspace("doomed");
    store.add_workspace("keep");
    let first = add(&mut store, doomed, "/a");
    let second = add(&mut store, doomed, "/b");
    let first_snapshot = session_snapshot(store.session(first).unwrap());
    let snapshot = workspace_snapshot(store.workspace(doomed).unwrap());

    assert!(store.soft_remove_workspace(doomed));
    store.finalize_pending_close(latest(&store));
    // reopen `first` alone, then close it again: a pending session close holds it while the workspace entry
    // still lists both
    assert!(store.restore_recent_closed(&session_item(doomed, "doomed", first_snapshot)));
    assert!(store.soft_close_session(first));

    assert!(store.restore_recent_closed(&workspace_item(snapshot, Some(second))));

    assert_eq!(ids(&store, doomed).len(), 2);
    assert_eq!(marked(&ids(&store, doomed)), marked(&[first, second]));
    assert!(
        store.pending_close_members().is_empty(),
        "the session close was undone, not rebuilt beside"
    );
}

#[test]
fn an_entry_without_its_payload_restores_nothing() {
    let mut store = AppStore::new();
    store.add_workspace("work");
    let mut item = session_item(Uuid::new_v4(), "x", SessionSnapshot::new(Uuid::new_v4(), "/x"));
    item.session = None;
    assert!(!store.restore_recent_closed(&item));
    item.kind = RecentClosedKind::Workspace;
    assert_eq!(
        store.restore_recent_closed_with(&item, &std::collections::HashSet::new()),
        RecentClosedRestore {
            restored: false,
            complete: false
        }
    );
}

// MARK: - Focus set (AppStoreFocusTests)

/// A "work" member with a session and a "doomed" member closed and finalized, the filter on.
fn closed_member(doomed_session: bool) -> (AppStore, Uuid, Uuid, Option<Uuid>) {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let doomed = store.add_workspace("doomed");
    add(&mut store, work, "/a");
    let stray = doomed_session.then(|| add_background(&mut store, doomed, "/b"));
    store.set_focus_membership(work, true);
    store.set_focus_membership(doomed, true);
    store.set_focus_enabled(true);
    assert!(store.soft_remove_workspace(doomed));
    store.finalize_pending_close(latest(&store));
    (store, work, doomed, stray)
}

#[test]
fn reopening_a_closed_member_marks_it_again() {
    let (mut store, work, doomed, _) = closed_member(false);
    assert_eq!(store.focus().ids(), &marked(&[work]));
    assert!(store.focus().enabled());

    let item = workspace_entry(&store.recent_closed_items(), doomed);
    assert!(store.restore_recent_closed(&item));
    assert_eq!(store.focus().ids(), &marked(&[work, doomed]));
    assert!(store.focus().enabled());
    assert_eq!(visible_ids(&store), [work, doomed]);
}

#[test]
fn reopening_a_closed_member_with_sessions_keeps_the_filter_on() {
    let (mut store, work, doomed, stray) = closed_member(true);

    let item = workspace_entry(&store.recent_closed_items(), doomed);
    assert!(store.restore_recent_closed(&item));
    assert_eq!(store.focus().ids(), &marked(&[work, doomed]));
    assert!(store.focus().enabled());
    assert_eq!(visible_ids(&store), [work, doomed]);
    assert_eq!(store.selected_session_id(), stray);
}

#[test]
fn reopening_the_last_member_marks_it_without_switching_the_filter_on() {
    let mut store = AppStore::new();
    let other = store.add_workspace("other");
    let doomed = store.add_workspace("doomed");
    let spare = store.add_workspace("spare");
    store.set_focus_membership(doomed, true);
    store.set_focus_enabled(true);
    assert!(store.soft_remove_workspace(doomed));
    store.finalize_pending_close(latest(&store));
    assert!(store.focus().ids().is_empty() && !store.focus().enabled());

    let item = workspace_entry(&store.recent_closed_items(), doomed);
    assert!(store.restore_recent_closed(&item));
    assert_eq!(store.focus().ids(), &marked(&[doomed]));
    assert!(
        !store.focus().enabled(),
        "marking on reopen never switches the filter on"
    );
    assert_eq!(visible_ids(&store), [other, spare, doomed]);
}

#[test]
fn reopening_a_member_with_the_filter_off_leaves_it_off() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let doomed = store.add_workspace("doomed");
    let spare = store.add_workspace("spare");
    store.set_focus_membership(work, true);
    store.set_focus_membership(doomed, true);
    store.set_focus_enabled(true);
    assert!(store.soft_remove_workspace(doomed));
    store.finalize_pending_close(latest(&store));
    store.set_focus_enabled(false);

    let item = workspace_entry(&store.recent_closed_items(), doomed);
    assert!(store.restore_recent_closed(&item));
    assert_eq!(store.focus().ids(), &marked(&[work, doomed]));
    assert!(!store.focus().enabled());
    assert_eq!(visible_ids(&store), [work, spare, doomed]);
}

#[test]
fn an_entry_from_a_filtering_window_cannot_switch_another_windows_filter_on() {
    let mut window_a = AppStore::new();
    let shared = window_a.add_workspace("shared");
    window_a.add_workspace("keeper");
    window_a.set_focused_workspace(shared);
    assert!(window_a.focus().enabled());
    window_a.remove_workspace(shared);
    let item = workspace_entry(&window_a.recent_closed_items(), shared);

    let mut window_b = AppStore::new();
    let curated = window_b.add_workspace("curated");
    let extra = window_b.add_workspace("extra");
    window_b.set_focus_membership(curated, true);

    assert!(window_b.restore_recent_closed(&item));
    assert!(
        !window_b.focus().enabled(),
        "an entry from a filtering window must not apply B's filter"
    );
    assert_eq!(window_b.focus().ids(), &marked(&[curated, shared]));
    assert_eq!(visible_ids(&window_b), [curated, extra, shared]);
}

#[test]
fn reopening_a_closed_non_member_does_not_mark_it() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let doomed = store.add_workspace("doomed");
    store.add_workspace("spare");
    store.set_focus_membership(work, true);
    store.set_focus_enabled(true);
    assert!(store.soft_remove_workspace(doomed));
    store.finalize_pending_close(latest(&store));

    let item = workspace_entry(&store.recent_closed_items(), doomed);
    assert!(store.restore_recent_closed(&item));
    assert_eq!(store.focus().ids(), &marked(&[work]));
    assert!(store.focus().enabled());
    assert_eq!(visible_ids(&store), [work]);
}

#[test]
fn hard_removing_a_member_records_its_membership_for_the_reopen() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let doomed = store.add_workspace("doomed");
    add(&mut store, work, "/a");
    store.set_focus_membership(work, true);
    store.set_focus_membership(doomed, true);
    store.set_focus_enabled(true);

    store.remove_workspace(doomed);
    let item = workspace_entry(&store.recent_closed_items(), doomed);
    assert_eq!(item.workspace.as_ref().unwrap().focus_member, Some(true));

    assert!(store.restore_recent_closed(&item));
    assert_eq!(store.focus().ids(), &marked(&[work, doomed]));
    assert!(store.focus().enabled());
}

// MARK: - Targeting (AppStoreCurrentWorkspaceTests)

#[test]
fn reopening_a_closed_session_drops_the_fresh_workspace_as_current() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let first = add(&mut store, work, "/a");
    add(&mut store, work, "/b");
    store.select_session(Some(first));
    store.close_session(first);
    let fresh = store.add_workspace("fresh");
    assert_eq!(store.current_workspace_id(), Some(fresh));

    let item = store.recent_closed_items()[0].clone();
    assert!(store.restore_recent_closed(&item));
    assert_eq!(store.current_workspace_id(), Some(work));
}

#[test]
fn reopening_an_empty_closed_workspace_keeps_the_selection_and_the_target() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let session = add(&mut store, work, "/a");
    let second = add(&mut store, work, "/b");
    store.select_session_in_sidebar(Some(session), vec![session, second]);
    let empty = store.add_workspace("empty");
    assert!(store.soft_remove_workspace(empty));
    store.finalize_pending_close(latest(&store));

    let item = store.recent_closed_items()[0].clone();
    assert!(store.restore_recent_closed(&item));
    assert_eq!(workspace_ids(&store), [work, empty]);
    assert_eq!(store.selected_session_id(), Some(session));
    assert_eq!(
        store.sidebar_selection_ids(),
        [session, second],
        "a multi-row selection survives"
    );
    assert_eq!(store.current_workspace_id(), Some(work));
}

#[test]
fn reopening_a_hard_removed_fresh_workspace_does_not_revive_the_target() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let session = add(&mut store, work, "/a");
    store.select_session(Some(session));
    let fresh = store.add_workspace_with("fresh", WorkspaceOptions::default());
    assert_eq!(store.current_workspace_id(), Some(fresh));

    store.remove_workspace(fresh);
    let item = store.recent_closed_items()[0].clone();
    assert!(store.restore_recent_closed(&item));
    assert_eq!(workspace_ids(&store), [work, fresh]);
    assert_eq!(store.selected_session_id(), Some(session));
    assert_eq!(store.current_workspace_id(), Some(work));
}

// MARK: - The library (WindowLibraryTests)

fn library() -> (WindowLibrary, Uuid) {
    let library = WindowLibrary::seeded(HOME);
    let window = library.active_window_id().unwrap();
    (library, window)
}

fn store_mut(library: &mut WindowLibrary, window: Uuid) -> &mut AppStore {
    library.store_mut(window).expect("the window is open")
}

#[test]
fn closing_a_session_records_an_entry_the_library_reopens() {
    let (mut library, window) = library();
    let store = store_mut(&mut library, window);
    let project = store.add_workspace("project");
    let first = named(store, project, "/first", "first");
    let session = named(store, project, "/project", "api");
    let last = named(store, project, "/last", "last");
    store.add_workspace("other");

    store.close_session(session);

    let recent = library.recent_closed_items()[0].clone();
    assert_eq!((recent.kind, recent.title.as_str()), (RecentClosedKind::Session, "api"));
    assert_eq!(recent.subtitle.as_deref(), Some("project"));

    assert!(library.reopen_recent_closed(recent.id, None));
    let store = library.store(window).unwrap();
    let restored = store.session(session).unwrap();
    assert_eq!(restored.initial_cwd(), "/project");
    assert_eq!(restored.custom_name.as_deref(), Some("api"));
    assert_eq!(ids(store, project), [first, session, last]);
    assert_eq!(store.selected_session_id(), Some(session));
    assert!(library.recent_closed_items().is_empty());
}

#[test]
fn reopening_a_session_recreates_its_missing_workspace() {
    let (mut library, window) = library();
    let store = store_mut(&mut library, window);
    let project = store.add_workspace("project");
    let session = named(store, project, "/project", "api");
    store.close_session(session);
    let recent = library.recent_closed_items()[0].clone();
    store_mut(&mut library, window).remove_workspace(project);

    assert!(library.reopen_recent_closed(recent.id, None));
    let store = library.store(window).unwrap();
    assert_eq!(store.workspace(project).unwrap().name, "project");
    assert_eq!(ids(store, project), [session]);
    assert_eq!(store.selected_session_id(), Some(session));
}

#[test]
fn reopening_into_another_window_restores_where_the_session_is_still_pending() {
    let (mut library, source) = library();
    let store = store_mut(&mut library, source);
    let project = store.add_workspace("project");
    let session = named(store, project, "/project", "api");
    let probe = with_surface(store, session);
    let destination = library.new_window(None);

    assert!(store_mut(&mut library, source).soft_close_session(session));
    let recent = library.recent_closed_items()[0].clone();
    assert_eq!(
        library.reopen_recent_closed_reporting_window(recent.id, Some(destination)),
        Some(source)
    );

    assert!(library.store(destination).unwrap().session(session).is_none());
    assert!(
        library
            .store(source)
            .unwrap()
            .session(session)
            .unwrap()
            .surface
            .is_present()
    );
    assert_eq!(probe.teardowns(), 0);
    assert_eq!(copies(&library, session), 1);
    assert_eq!(library.window_id_for_session(session), Some(source));
}

/// Opens window `id` named `name` with the tree of `snapshot`.
fn open(library: &mut WindowLibrary, id: Uuid, name: &str, snapshot: &Snapshot) {
    library.insert_window(WindowInfo {
        id,
        name: name.to_owned(),
    });
    let mut store = AppStore::new();
    store.restore(snapshot, false);
    library.open_window(id, store);
}

fn one_workspace(id: Uuid, name: &str, sessions: Vec<SessionSnapshot>) -> Snapshot {
    Snapshot {
        workspaces: vec![WorkspaceSnapshot {
            id,
            name: name.to_owned(),
            sessions,
            collapsed: None,
        }],
        ..Snapshot::default()
    }
}

/// A workspace entry whose members are spread over two windows, as an older build could save: the first window
/// holds x, the second y, and z survives only in the entry. `shell` gives the first window the entry's own workspace
/// id, so the merge takes its existing-shell arm; `external_pending` parks y in a pending close.
fn reopen_split_workspace(shell: bool, external_pending: bool) {
    let (first, second, workspace) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let [x, y, z] = ["/x", "/y", "/z"].map(|cwd| SessionSnapshot::new(Uuid::new_v4(), cwd));
    let mut library = WindowLibrary::new(HOME);
    let first_workspace = if shell { workspace } else { Uuid::new_v4() };
    open(
        &mut library,
        first,
        "a",
        &one_workspace(first_workspace, "A", vec![x.clone()]),
    );
    open(
        &mut library,
        second,
        "b",
        &one_workspace(Uuid::new_v4(), "B", vec![y.clone()]),
    );
    library.set_frontmost(Some(first));
    let snapshot = WorkspaceSnapshot {
        id: workspace,
        name: "original".to_owned(),
        sessions: vec![x.clone(), y.clone(), z.clone()],
        collapsed: None,
    };
    let item = workspace_item(snapshot, Some(z.id));
    library.set_recent_closed_items(vec![item.clone()]);
    let x_probe = with_surface(store_mut(&mut library, first), x.id);
    let y_probe = with_surface(store_mut(&mut library, second), y.id);
    if external_pending {
        assert!(store_mut(&mut library, second).soft_close_session(y.id));
    }

    assert_eq!(
        library.reopen_recent_closed_reporting_window(item.id, Some(first)),
        Some(first)
    );

    let store = library.store(first).unwrap();
    assert!(
        store.session(x.id).unwrap().surface.is_present(),
        "x stays the original"
    );
    assert!(store.session(y.id).is_none(), "y belongs to the other window");
    assert!(
        store.session(z.id).is_some(),
        "z exists only in the entry and comes back"
    );
    assert!(!library.recent_closed_items().iter().any(|entry| entry.id == item.id));
    if external_pending {
        assert!(store_mut(&mut library, second).undo_pending_close(None));
    }
    assert!(
        library
            .store(second)
            .unwrap()
            .session(y.id)
            .unwrap()
            .surface
            .is_present()
    );
    assert_eq!((x_probe.teardowns(), y_probe.teardowns()), (0, 0));
    for id in [x.id, y.id, z.id] {
        assert_eq!(copies(&library, id), 1, "shell {shell}, pending {external_pending}");
    }
}

#[test]
fn reopening_a_split_workspace_rebuilds_only_the_member_nobody_holds() {
    for (shell, external_pending) in [(false, false), (false, true), (true, false), (true, true)] {
        reopen_split_workspace(shell, external_pending);
    }
}

#[test]
fn reopening_a_workspace_pending_its_close_restores_the_original_objects() {
    let (mut library, source) = library();
    let store = store_mut(&mut library, source);
    let workspace = store.add_workspace("project");
    let session = named(store, workspace, "/project", "api");
    let probe = with_surface(store, session);
    let destination = library.new_window(None);

    assert!(store_mut(&mut library, source).soft_remove_workspace(workspace));
    let recent = library.recent_closed_items()[0].clone();
    assert_eq!(recent.kind, RecentClosedKind::Workspace);
    assert_eq!(
        library.reopen_recent_closed_reporting_window(recent.id, Some(destination)),
        Some(source)
    );

    assert!(library.store(destination).unwrap().workspace(workspace).is_none());
    let source_store = library.store(source).unwrap();
    assert!(source_store.session(session).unwrap().surface.is_present());
    assert!(
        !source_store.pending_holds_workspace(workspace),
        "the pending close is consumed"
    );
    assert_eq!(probe.teardowns(), 0);
    assert_eq!(copies(&library, session), 1);
}

#[test]
fn reopening_an_empty_workspace_pending_its_close_still_finds_its_owner() {
    let (mut library, source) = library();
    let workspace = store_mut(&mut library, source).add_workspace("empty");
    let destination = library.new_window(None);

    assert!(store_mut(&mut library, source).soft_remove_workspace(workspace));
    let recent = library.recent_closed_items()[0].clone();
    assert_eq!(
        library.reopen_recent_closed_reporting_window(recent.id, Some(destination)),
        Some(source)
    );
    assert!(library.store(source).unwrap().workspace(workspace).is_some());
    assert!(library.store(destination).unwrap().workspace(workspace).is_none());
}

#[test]
fn reopening_a_session_persisted_in_two_windows_does_not_rebuild_the_closed_copy() {
    let (a, b, workspace, session) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let shared = SessionSnapshot {
        custom_name: Some("api".to_owned()),
        ..SessionSnapshot::new(session, "/project")
    };
    let mut library = WindowLibrary::new(HOME);
    open(
        &mut library,
        a,
        "a",
        &one_workspace(workspace, "project", vec![shared.clone()]),
    );
    open(
        &mut library,
        b,
        "b",
        &one_workspace(Uuid::new_v4(), "elsewhere", vec![shared]),
    );
    library.set_frontmost(Some(a));
    assert_eq!(copies(&library, session), 2);

    store_mut(&mut library, a).close_session(session);
    assert_eq!(copies(&library, session), 1);

    let recent = library.recent_closed_items()[0].clone();
    assert_eq!(library.reopen_recent_closed_reporting_window(recent.id, None), Some(b));
    assert_eq!(copies(&library, session), 1);
    assert!(library.store(a).unwrap().session(session).is_none());
    assert!(library.store(b).unwrap().session(session).is_some());
}

#[test]
fn reopening_follows_the_workspace_owner_rather_than_the_requested_window() {
    let (mut library, source) = library();
    let store = store_mut(&mut library, source);
    let workspace = store.add_workspace("project");
    let x = named(store, workspace, "/x", "x");
    named(store, workspace, "/y", "y");
    let destination = library.new_window(None);

    store_mut(&mut library, source).close_session(x);
    let recent = library.recent_closed_items()[0].clone();
    assert_eq!(
        library.reopen_recent_closed_reporting_window(recent.id, Some(destination)),
        Some(source)
    );

    assert!(library.store(destination).unwrap().workspace(workspace).is_none());
    assert!(library.store(source).unwrap().session(x).is_some());
    assert_eq!(copies(&library, x), 1);
}

#[test]
fn reopening_a_workspace_nobody_holds_goes_to_the_requested_window() {
    let (mut library, source) = library();
    let store = store_mut(&mut library, source);
    let workspace = store.add_workspace("project");
    let x = named(store, workspace, "/x", "x");
    let y = named(store, workspace, "/y", "y");
    let destination = library.new_window(None);

    store_mut(&mut library, source).remove_workspace(workspace);
    let recent = library.recent_closed_items()[0].clone();
    assert_eq!(recent.kind, RecentClosedKind::Workspace);
    assert_eq!(
        library.reopen_recent_closed_reporting_window(recent.id, Some(destination)),
        Some(destination)
    );

    assert_eq!(ids(library.store(destination).unwrap(), workspace), [x, y]);
    assert!(library.recent_closed_items().is_empty());
}

#[test]
fn reopening_into_another_window_restores_where_the_session_is_still_live() {
    let (mut library, source) = library();
    let store = store_mut(&mut library, source);
    let workspace = store.add_workspace("project");
    let session = named(store, workspace, "/project", "api");
    let destination = library.new_window(None);

    store_mut(&mut library, source).close_session(session);
    let recent = library.recent_closed_items()[0].clone();
    assert!(store_mut(&mut library, source).restore_recent_closed(&recent));
    assert!(library.reopen_recent_closed(recent.id, Some(destination)));

    assert!(library.store(destination).unwrap().session(session).is_none());
    assert_eq!(copies(&library, session), 1);
}

#[test]
fn reopening_a_stale_session_entry_selects_the_existing_session() {
    let (mut library, window) = library();
    let store = store_mut(&mut library, window);
    let workspace = store.add_workspace("project");
    let session = named(store, workspace, "/project", "api");
    store.close_session(session);
    let recent = library.recent_closed_items()[0].clone();

    let store = store_mut(&mut library, window);
    assert!(store.restore_recent_closed(&recent));
    assert!(store.restore_recent_closed(&recent));

    assert_eq!(
        store.sessions().filter(|candidate| candidate.id() == session).count(),
        1
    );
    assert_eq!(store.selected_session_id(), Some(session));
}

#[test]
fn closing_a_workspace_records_an_entry_the_library_reopens() {
    let (mut library, window) = library();
    let store = store_mut(&mut library, window);
    let workspace = store.add_workspace("project");
    let session = named(store, workspace, "/project", "api");

    store.remove_workspace(workspace);

    let recent = library.recent_closed_items()[0].clone();
    assert_eq!(
        (recent.kind, recent.title.as_str()),
        (RecentClosedKind::Workspace, "project")
    );
    assert_eq!(recent.subtitle.as_deref(), Some("1 session"));
    assert!(library.store(window).unwrap().workspace(workspace).is_none());

    assert!(library.reopen_recent_closed(recent.id, None));
    let store = library.store(window).unwrap();
    assert_eq!(store.workspace(workspace).unwrap().name, "project");
    assert_eq!(ids(store, workspace), [session]);
    assert_eq!(store.selected_session_id(), Some(session));
    assert!(library.recent_closed_items().is_empty());
}

#[test]
fn reopening_a_stale_workspace_entry_selects_the_existing_workspace() {
    let (mut library, window) = library();
    let store = store_mut(&mut library, window);
    let workspace = store.add_workspace("project");
    let session = named(store, workspace, "/project", "api");
    store.remove_workspace(workspace);
    let recent = library.recent_closed_items()[0].clone();

    let store = store_mut(&mut library, window);
    assert!(store.restore_recent_closed(&recent));
    assert!(store.restore_recent_closed(&recent));

    assert_eq!(workspace_ids(store).iter().filter(|id| **id == workspace).count(), 1);
    assert_eq!(
        store.sessions().filter(|candidate| candidate.id() == session).count(),
        1
    );
    assert_eq!(store.selected_session_id(), Some(session));
}

#[test]
fn reopening_during_the_grace_restores_the_pending_session_itself() {
    let (mut library, window) = library();
    let store = store_mut(&mut library, window);
    let workspace = store.workspaces()[0].id();
    let session = named(store, workspace, "/grace", "grace");
    let probe = with_surface(store, session);

    assert!(store.soft_close_session(session));
    let recent = library.recent_closed_items()[0].clone();
    assert!(library.reopen_recent_closed(recent.id, None));

    assert!(
        library
            .store(window)
            .unwrap()
            .session(session)
            .unwrap()
            .surface
            .is_present()
    );
    assert_eq!(probe.teardowns(), 0);
    assert!(library.recent_closed_items().is_empty());
}

#[test]
fn a_soft_close_records_its_entry_at_once_and_an_undo_removes_it() {
    let (mut library, window) = library();
    let store = store_mut(&mut library, window);
    let workspace = store.workspaces()[0].id();
    let undone = named(store, workspace, "/undo", "undo");
    assert!(store.soft_close_session(undone));
    assert_eq!(titles(&library.recent_closed_items()), ["undo"]);
    assert!(store_mut(&mut library, window).undo_pending_close(None));
    assert!(library.recent_closed_items().is_empty());

    let store = store_mut(&mut library, window);
    let finalized = named(store, workspace, "/final", "final");
    assert!(store.soft_close_session(finalized));
    store.finalize_all_pending_closes();

    let recent = library.recent_closed_items()[0].clone();
    assert_eq!(
        (recent.kind, recent.title.as_str()),
        (RecentClosedKind::Session, "final")
    );
}

#[test]
fn a_grouped_undo_restores_every_session_and_removes_their_entries() {
    let (mut library, window) = library();
    let store = store_mut(&mut library, window);
    let workspace = store.workspaces()[0].id();
    let first = named(store, workspace, "/one", "one");
    let second = named(store, workspace, "/two", "two");

    assert!(store.soft_close_sessions(&[first, second]));
    let mut recorded = titles(&library.recent_closed_items());
    recorded.sort();
    assert_eq!(recorded, ["one", "two"]);

    assert!(store_mut(&mut library, window).undo_pending_close(None));
    let store = library.store(window).unwrap();
    assert!(store.session(first).is_some() && store.session(second).is_some());
    assert!(library.recent_closed_items().is_empty());
}

#[test]
fn reopening_one_member_of_a_grouped_close_selects_it() {
    let (mut library, window) = library();
    let store = store_mut(&mut library, window);
    let workspace = store.workspaces()[0].id();
    let first = named(store, workspace, "/one", "one");
    let second = named(store, workspace, "/two", "two");
    store.select_session(Some(first));
    assert!(store.soft_close_sessions(&[first, second]));
    let chosen = session_entry(&library.recent_closed_items(), second);

    assert!(library.reopen_recent_closed(chosen.id, None));

    let store = library.store(window).unwrap();
    assert!(store.session(first).is_some() && store.session(second).is_some());
    assert_eq!(store.selected_session_id(), Some(second));
    assert!(library.recent_closed_items().is_empty());
}

#[test]
fn the_latest_entry_reopens_and_clearing_empties_the_list() {
    let (mut library, window) = library();
    let store = store_mut(&mut library, window);
    let workspace = store.workspaces()[0].id();
    let older = named(store, workspace, "/old", "old");
    let newer = named(store, workspace, "/new", "new");
    store.close_session(older);
    store.close_session(newer);
    library.drain_effects();

    assert_eq!(library.reopen_latest_recent_closed_reporting_window(None), Some(window));
    assert!(library.store(window).unwrap().session(newer).is_some());
    assert_eq!(titles(&library.recent_closed_items()), ["old"]);

    library.clear_recent_closed_items();
    assert!(library.recent_closed_items().is_empty());
    assert!(!library.reopen_latest_recent_closed(None));
    let changes = library.drain_effects().recent_closed;
    assert!(matches!(changes.last(), Some(RecentClosedChange::Clear)), "{changes:?}");
}

#[test]
fn the_library_drains_every_change_in_the_order_it_happened() {
    let (mut library, window) = library();
    let session = library.active_store().unwrap().active_session().unwrap().id();
    let second = library.new_window(None);
    library.drain_effects();

    store_mut(&mut library, window).close_session(session);
    let recent = library.recent_closed_items()[0].clone();
    assert!(library.reopen_recent_closed(recent.id, Some(second)));
    let other = library.store(second).unwrap().active_session().unwrap().id();
    store_mut(&mut library, second).close_session(other);

    let changes = library.drain_effects().recent_closed;
    assert_eq!(changes.len(), 3, "{changes:?}");
    assert!(matches!(&changes[0], RecentClosedChange::Record(item) if item.id == recent.id));
    assert_eq!(changes[1], RecentClosedChange::Remove(recent.id));
    assert!(matches!(
        &changes[2],
        RecentClosedChange::Record(item) if item.session.as_ref().is_some_and(|recent| recent.snapshot.id == other)
    ));
    assert_eq!(
        library.store(window).unwrap().recent_closed_items(),
        library.recent_closed_items(),
        "one shared list"
    );
}
