//! Who takes the selection after the active session goes, ported from upstream's
//! `AppStoreCloseReselectionTests.swift`. The recency and `removeWorkspace` cases of `AppStoreTests.swift` live in
//! `store.rs`, the removal-reselection cases of `AppStoreOrganizationTests.swift` in `organization.rs`.
//!
//! Upstream builds its "empty scoped recency" trees through `restore(from:)`, which seeds the recency with the
//! selected session alone. Restore is not ported yet, so those cases build the same tree with background adds (which
//! leave recency alone) followed by one select: the recency is then exactly `[selected]`, as each test asserts.

use std::collections::BTreeSet;

use uuid::Uuid;

use super::support::{add, add_background};
use crate::model::{AppStore, NewSession, SidebarMode};

/// A single "work" workspace holding background sessions at `cwds`, nothing selected and an empty recency.
fn background_tree(cwds: &[&str]) -> (AppStore, Uuid, Vec<Uuid>) {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let sessions = cwds.iter().map(|cwd| add_background(&mut store, ws, cwd)).collect();
    (store, ws, sessions)
}

fn marked(ids: &[Uuid]) -> BTreeSet<Uuid> {
    ids.iter().copied().collect()
}

fn flagged_ids(store: &AppStore) -> Vec<Uuid> {
    store.flagged_sessions().iter().map(|session| session.id()).collect()
}

// MARK: - AppStoreCloseReselectionTests

#[test]
fn close_active_session_inserted_after_current_returns_to_the_session_it_came_from() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let one = add(&mut store, ws, "/1");
    add(&mut store, ws, "/2");
    add(&mut store, ws, "/3");
    store.select_session(Some(one));
    let spec = NewSession {
        at: Some(1),
        ..NewSession::at_cwd("/4")
    };
    let four = store.add_session(ws, spec).unwrap();
    assert_eq!(store.selected_session_id(), Some(four));

    store.close_session(four);
    assert_eq!(store.selected_session_id(), Some(one));
}

#[test]
fn close_active_session_appended_at_the_end_returns_to_the_session_it_came_from() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let one = add(&mut store, ws, "/1");
    add(&mut store, ws, "/2");
    add(&mut store, ws, "/3");
    store.select_session(Some(one));
    let four = add(&mut store, ws, "/4");

    store.close_session(four);
    assert_eq!(store.selected_session_id(), Some(one));
}

#[test]
fn close_active_session_prefers_the_recent_survivor_over_the_positional_neighbor() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let first = add(&mut store, ws, "/a");
    let second = add(&mut store, ws, "/b");
    add(&mut store, ws, "/c");
    let fourth = add(&mut store, ws, "/d");
    store.select_session(Some(first));
    store.select_session(Some(second));
    store.select_session(Some(fourth));

    store.close_session(fourth);
    assert_eq!(store.selected_session_id(), Some(second));
}

#[test]
fn close_active_session_ignores_a_more_recent_session_in_another_workspace() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    let in_work = add(&mut store, work, "/a");
    let closing = add(&mut store, work, "/b");
    let elsewhere = add(&mut store, personal, "/x");
    store.select_session(Some(in_work));
    store.select_session(Some(elsewhere));
    store.select_session(Some(closing));

    store.close_session(closing);
    assert_eq!(store.selected_session_id(), Some(in_work));
    assert!(store.focus().ids().is_empty() && !store.focus().enabled());
}

#[test]
fn close_active_session_emptying_its_workspace_picks_the_recent_survivor_elsewhere() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let scratch = store.add_workspace("scratch");
    add(&mut store, work, "/a");
    let came_from = add(&mut store, work, "/b");
    let only_in_scratch = add(&mut store, scratch, "/x");
    store.select_session(Some(came_from));
    store.select_session(Some(only_in_scratch));

    store.close_session(only_in_scratch);
    assert!(store.workspaces()[1].sessions.is_empty());
    assert_eq!(store.selected_session_id(), Some(came_from));
}

#[test]
fn close_active_session_with_a_focus_filter_stays_inside_the_focused_workspace() {
    let mut store = AppStore::new();
    let personal = store.add_workspace("personal");
    let work = store.add_workspace("work");
    add(&mut store, personal, "/x");
    let first = add(&mut store, work, "/a");
    add(&mut store, work, "/b");
    let closing = add(&mut store, work, "/c");
    store.select_session(Some(first));
    store.select_session(Some(closing));
    store.set_focused_workspace(work);

    store.close_session(closing);
    assert_eq!(store.selected_session_id(), Some(first));
    assert!(*store.focus().ids() == marked(&[work]) && store.focus().enabled());
}

#[test]
fn close_the_focused_workspaces_last_session_picks_the_recent_survivor_elsewhere() {
    let mut store = AppStore::new();
    let personal = store.add_workspace("personal");
    let work = store.add_workspace("work");
    add(&mut store, personal, "/first");
    let came_from = add(&mut store, personal, "/mru");
    let only_in_work = add(&mut store, work, "/only");
    store.select_session(Some(came_from));
    store.select_session(Some(only_in_work));
    store.set_focused_workspace(work);

    store.close_session(only_in_work);
    assert!(store.workspaces()[1].sessions.is_empty());
    assert_eq!(store.selected_session_id(), Some(came_from));
    assert!(*store.focus().ids() == marked(&[work]) && !store.focus().enabled());
}

#[test]
fn close_under_a_focus_filter_stays_inside_the_marked_set() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    let survivor = add(&mut store, work, "/a");
    let closing = add(&mut store, work, "/c");
    let elsewhere = add(&mut store, personal, "/x");
    store.select_session(Some(survivor));
    store.select_session(Some(elsewhere));
    store.select_session(Some(closing));
    store.set_focused_workspace(work);
    assert_eq!(store.selected_session_id(), Some(closing));

    store.close_session(closing);
    assert_eq!(store.selected_session_id(), Some(survivor));
    assert!(*store.focus().ids() == marked(&[work]) && store.focus().enabled());
}

#[test]
fn close_in_a_marked_workspace_crosses_to_the_other_member_not_an_unmarked_survivor() {
    // `unmarked` is deliberately more recent than `other_member`: an unscoped MRU takes it.
    let mut store = AppStore::new();
    let first = store.add_workspace("first");
    let second = store.add_workspace("second");
    let outside = store.add_workspace("outside");
    let closing = add(&mut store, first, "/only");
    let other_member = add_background(&mut store, second, "/member");
    let unmarked = add_background(&mut store, outside, "/stray");
    store.select_session(Some(other_member));
    store.select_session(Some(unmarked));
    store.select_session(Some(closing));
    store.set_focus_membership(first, true);
    store.set_focus_membership(second, true);
    store.set_focus_enabled(true);

    store.close_session(closing);
    assert!(store.workspaces()[0].sessions.is_empty());
    assert_eq!(store.selected_session_id(), Some(other_member));
    assert!(*store.focus().ids() == marked(&[first, second]) && store.focus().enabled());
}

#[test]
fn close_under_a_focus_filter_with_no_recency_keeps_the_positional_pick_inside_the_set() {
    // the unmarked workspace sits between the emptied one and the other marked one, so the picks differ.
    let mut store = AppStore::new();
    let ws_closing = store.add_workspace("closing");
    let ws_unmarked = store.add_workspace("unmarked");
    let ws_other_member = store.add_workspace("member");
    let closing = add_background(&mut store, ws_closing, "/closing");
    add_background(&mut store, ws_unmarked, "/outside");
    let in_set = add_background(&mut store, ws_other_member, "/inset");
    store.select_session(Some(closing));
    store.set_focus_membership(ws_closing, true);
    store.set_focus_membership(ws_other_member, true);
    store.set_focus_enabled(true);
    assert_eq!(store.session_recency().items(), [closing]);

    store.close_session(closing);
    assert_eq!(store.selected_session_id(), Some(in_set));
    assert!(*store.focus().ids() == marked(&[ws_closing, ws_other_member]) && store.focus().enabled());
}

#[test]
fn close_active_session_in_flagged_mode_stays_within_the_flagged_set() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let flagged = add(&mut store, ws, "/a");
    let unflagged = add(&mut store, ws, "/b");
    let closing = add(&mut store, ws, "/c");
    store.set_flag(true, flagged);
    store.set_flag(true, closing);
    store.set_sidebar_mode(SidebarMode::Flagged);
    store.select_session(Some(flagged));
    store.select_session(Some(unflagged));
    store.select_session(Some(closing));

    store.close_session(closing);
    assert_eq!(store.selected_session_id(), Some(flagged));
}

#[test]
fn close_active_session_in_flagged_mode_crosses_workspaces_rather_than_leaving_the_flagged_set() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    let unflagged = add(&mut store, work, "/a");
    let closing = add(&mut store, work, "/b");
    let flagged_elsewhere = add(&mut store, personal, "/x");
    store.set_flag(true, closing);
    store.set_flag(true, flagged_elsewhere);
    store.set_sidebar_mode(SidebarMode::Flagged);
    store.select_session(Some(flagged_elsewhere));
    store.select_session(Some(unflagged));
    store.select_session(Some(closing));

    store.close_session(closing);
    assert_eq!(store.selected_session_id(), Some(flagged_elsewhere));
    assert!(flagged_ids(&store).contains(&store.selected_session_id().unwrap()));
}

#[test]
fn close_active_session_in_flagged_mode_with_an_empty_scoped_recency_stays_within_the_flagged_set() {
    let (mut store, _, ids) = background_tree(&["/a", "/b", "/c"]);
    store.set_flag(true, ids[0]);
    store.set_flag(true, ids[1]);
    store.select_session(Some(ids[1]));
    store.set_sidebar_mode(SidebarMode::Flagged);
    assert_eq!(store.session_recency().items(), [ids[1]]);

    store.close_session(ids[1]);
    assert_eq!(store.selected_session_id(), Some(ids[0]));
    assert!(flagged_ids(&store).contains(&store.selected_session_id().unwrap()));
}

#[test]
fn close_active_session_in_flagged_mode_with_an_empty_scoped_recency_picks_the_nearest_flagged_survivor() {
    let (mut store, _, ids) = background_tree(&["/0", "/1", "/2"]);
    for &id in &ids {
        store.set_flag(true, id);
    }
    store.select_session(Some(ids[1]));
    store.set_sidebar_mode(SidebarMode::Flagged);
    assert_eq!(store.session_recency().items(), [ids[1]]);

    store.close_session(ids[1]);
    assert_eq!(store.selected_session_id(), Some(ids[2]));
}

#[test]
fn close_the_workspaces_last_flagged_session_with_an_empty_recency_picks_the_adjacent_flagged_row() {
    let mut store = AppStore::new();
    let first = store.add_workspace("work");
    let last = store.add_workspace("personal");
    let ids = [
        add_background(&mut store, first, "/a"),
        add_background(&mut store, first, "/b"),
        add_background(&mut store, last, "/c"),
        add_background(&mut store, last, "/d"),
    ];
    for id in [ids[0], ids[1], ids[3]] {
        store.set_flag(true, id);
    }
    store.select_session(Some(ids[3]));
    store.set_sidebar_mode(SidebarMode::Flagged);
    assert_eq!(store.session_recency().items(), [ids[3]]);

    store.close_session(ids[3]);
    assert_eq!(store.selected_session_id(), Some(ids[1]));
}

#[test]
fn close_the_last_flagged_session_widens_to_the_whole_tree() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let recent_survivor = add(&mut store, ws, "/a");
    let closing = add(&mut store, ws, "/b");
    add(&mut store, ws, "/c");
    store.set_flag(true, closing);
    store.set_sidebar_mode(SidebarMode::Flagged);
    store.select_session(Some(recent_survivor));
    store.select_session(Some(closing));

    store.close_session(closing);
    assert!(store.flagged_sessions().is_empty());
    assert_eq!(store.selected_session_id(), Some(recent_survivor));
}

#[test]
fn close_active_session_with_an_empty_scoped_recency_falls_back_to_the_positional_target() {
    let (mut store, _, ids) = background_tree(&["/0", "/1", "/2"]);
    store.select_session(Some(ids[1]));
    assert_eq!(store.session_recency().items(), [ids[1]]);

    store.close_session(ids[1]);
    assert_eq!(store.selected_session_id(), Some(ids[2]));
}

/// Upstream runs this through `softCloseSession`, which picks the next selection with the same
/// `closeReselectionTarget` as the hard close; soft close is not ported, so the hard close stands in.
#[test]
fn soft_close_active_session_picks_the_recent_survivor_like_the_hard_close() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let first = add(&mut store, ws, "/a");
    add(&mut store, ws, "/b");
    add(&mut store, ws, "/c");
    let closing = add(&mut store, ws, "/d");
    store.select_session(Some(first));
    store.select_session(Some(closing));

    assert!(store.close_session(closing).is_some());
    assert_eq!(store.selected_session_id(), Some(first));
}

/// Upstream runs this through `softCloseSession`; see the test above.
#[test]
fn soft_close_active_session_with_an_empty_scoped_recency_falls_back_to_the_positional_target() {
    let (mut store, _, ids) = background_tree(&["/0", "/1", "/2"]);
    store.select_session(Some(ids[1]));

    assert!(store.close_session(ids[1]).is_some());
    assert_eq!(store.selected_session_id(), Some(ids[2]));
}

#[test]
fn close_active_session_never_clears_the_selection_while_sessions_survive() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    add(&mut store, work, "/a");
    add(&mut store, work, "/b");
    add(&mut store, personal, "/x");

    for remaining in [3, 2] {
        let active = store.selected_session_id().unwrap();
        store.close_session(active);
        assert!(
            store.selected_session_id().is_some(),
            "{} sessions survive, selection must not go nil",
            remaining - 1
        );
    }
    let last = store.selected_session_id().unwrap();
    store.close_session(last);
    assert_eq!(store.selected_session_id(), None);
}
