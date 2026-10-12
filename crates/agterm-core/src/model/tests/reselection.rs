//! Who takes the selection after the active session or its workspace goes, and the recency order behind it, ported
//! from upstream's `AppStoreCloseReselectionTests.swift`, the recency and `removeWorkspace` cases of
//! `AppStoreTests.swift` and the removal-reselection cases of `AppStoreOrganizationTests.swift`.
//!
//! Upstream builds its "empty scoped recency" trees through `restore(from:)`, which seeds the recency with the
//! selected session alone. Restore is not ported yet, so those cases build the same tree with background adds (which
//! leave recency alone) followed by one select: the recency is then exactly `[selected]`, as each test asserts.

use std::collections::BTreeSet;

use uuid::Uuid;

use super::support::{add, add_background, ids};
use crate::model::{AppStore, NewSession, Session, SidebarMode, Workspace};

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

// MARK: - Recency order (AppStoreTests)

#[test]
fn add_session_without_select_leaves_selection_untouched() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let first = add(&mut store, ws, "/a");
    assert_eq!(store.selected_session_id(), Some(first));
    // the control `session.new --no-select` path: appended, but selection and recency untouched
    let background = add_background(&mut store, ws, "/b");
    assert_eq!(ids(&store, ws), [first, background]);
    assert_eq!(store.selected_session_id(), Some(first));
    assert_eq!(store.active_session().map(Session::id), Some(first));
    assert_eq!(store.session_recency().items(), [first]); // the background add was not recorded
}

#[test]
fn selection_updates_recency_most_recent_first() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let a = add(&mut store, ws, "/a");
    let b = add(&mut store, ws, "/b");
    assert_eq!(store.session_recency().items(), [b, a]); // b selected last
    store.select_session(Some(a));
    assert_eq!(store.session_recency().items(), [a, b]);
    assert_eq!(store.session_recency().items()[1], b);
}

#[test]
fn close_session_removes_from_recency() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let a = add(&mut store, ws, "/a");
    let b = add(&mut store, ws, "/b");
    store.close_session(b);
    assert!(!store.session_recency().items().contains(&b));
    assert_eq!(store.session_recency().items(), [a]);
}

#[test]
fn recent_sessions_returns_most_recent_first_respecting_limit() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let a = add(&mut store, ws, "/a");
    let b = add(&mut store, ws, "/b");
    let c = add(&mut store, ws, "/c");
    // drive the recency order explicitly (select_session pushes MRU front): most-recent is the last select.
    store.select_session(Some(a));
    store.select_session(Some(c));
    store.select_session(Some(b));
    assert_eq!(store.recent_sessions(9), [b, c, a]);
    assert_eq!(store.recent_sessions(2), [b, c]);
}

#[test]
fn recent_sessions_spans_workspaces_and_skips_closed() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    let a = add(&mut store, work, "/a");
    let b = add(&mut store, personal, "/b");
    store.select_session(Some(a));
    store.select_session(Some(b));
    assert_eq!(store.recent_sessions(9), [b, a]);
    store.close_session(b); // closed sessions are pruned from recency, so they drop out
    assert_eq!(store.recent_sessions(9), [a]);
}

#[test]
fn navigable_recent_sessions_excludes_current_and_uses_visible_scope() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    let a = add(&mut store, work, "/a");
    let b = add(&mut store, work, "/b");
    let c = add(&mut store, personal, "/c");
    let d = add_background(&mut store, personal, "/d");
    store.select_session(Some(d));
    store.select_session(Some(a));
    store.select_session(Some(c));
    store.select_session(Some(b));

    assert_eq!(store.navigable_recent_sessions(2), [c, a]);

    store.set_focused_workspace(personal);
    assert_eq!(store.selected_session_id(), Some(c));
    assert_eq!(store.navigable_recent_sessions(9), [d]);
}

// MARK: - Workspace removal (AppStoreTests)

#[test]
fn remove_workspace_tears_down_sessions_and_prunes_recency() {
    // the surface teardown half needs spy surfaces, which the model has no seam for yet.
    let mut store = AppStore::new();
    let keep = store.add_workspace("keep");
    let doomed = store.add_workspace("doomed");
    let session = add(&mut store, doomed, "/a");
    store.remove_workspace(doomed);
    let remaining: Vec<Uuid> = store.workspaces().iter().map(Workspace::id).collect();
    assert_eq!(remaining, [keep]);
    assert!(!store.session_recency().items().contains(&session));
}

#[test]
fn remove_workspace_reselects_when_active_inside() {
    let mut store = AppStore::new();
    let keep = store.add_workspace("keep");
    let kept = add(&mut store, keep, "/k");
    let doomed = store.add_workspace("doomed");
    let active = add(&mut store, doomed, "/a");
    store.select_session(Some(active));
    store.remove_workspace(doomed);
    assert_eq!(store.selected_session_id(), Some(kept));
}

#[test]
fn remove_workspace_leaves_selection_when_active_elsewhere() {
    let mut store = AppStore::new();
    let keep = store.add_workspace("keep");
    let active = add(&mut store, keep, "/k");
    let doomed = store.add_workspace("doomed");
    add(&mut store, doomed, "/d");
    store.select_session(Some(active));
    store.remove_workspace(doomed);
    assert_eq!(store.selected_session_id(), Some(active));
}

#[test]
fn remove_workspace_keeps_at_least_one() {
    let mut store = AppStore::new();
    let only = store.add_workspace("only");
    assert!(!store.can_remove_workspace());
    assert!(store.remove_workspace(only).is_none());
    let remaining: Vec<Uuid> = store.workspaces().iter().map(Workspace::id).collect();
    assert_eq!(remaining, [only]);
}

// MARK: - Reselection under the focus filter and flagged mode (AppStoreOrganizationTests)

#[test]
fn close_focused_session_revealing_other_workspace_disables_the_filter() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    let only = add(&mut store, work, "/only");
    add(&mut store, personal, "/other");
    store.select_session(Some(only));
    store.set_focused_workspace(work);
    store.close_session(only); // reselects the personal session — outside the now-empty focused work
    assert!(store.active_session().is_some());
    let owner = store
        .workspace_for_session(store.selected_session_id().unwrap())
        .unwrap()
        .id();
    assert_eq!(owner, personal);
    assert!(*store.focus().ids() == marked(&[work]) && !store.focus().enabled());
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
    let owner = store
        .workspace_for_session(store.selected_session_id().unwrap())
        .unwrap()
        .id();
    assert_eq!(owner, b);
    assert!(store.focus().ids().is_empty() && !store.focus().enabled());
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
    assert!(*store.focus().ids() == marked(&[also_marked]) && store.focus().enabled());
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
