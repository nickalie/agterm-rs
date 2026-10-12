//! Session and workspace navigation, the attention list, and the sidebar focus filter (upstream's
//! `AppStoreNavigationTests.swift` and `AppStoreFocusTests.swift`).

use std::collections::{BTreeSet, HashSet};
use std::time::{Duration, SystemTime};

use uuid::Uuid;

use super::support::{add, add_background};
use crate::model::{
    AgentIndicator, AgentStatus, AppStore, FocusMode, SaveRequest, Session, SessionNavigation, SidebarMode,
    WorkspaceNavigation, WorkspaceOptions,
};

/// Two workspaces (work: a, b; personal: c, d), so the flattened order is [a, b, c, d].
fn nav_tree() -> (AppStore, Vec<Uuid>) {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    let a = add(&mut store, work, "/a");
    let b = add(&mut store, work, "/b");
    let c = add(&mut store, personal, "/c");
    let d = add(&mut store, personal, "/d");
    (store, vec![a, b, c, d])
}

/// Three workspaces (work: a, b; personal: c, d; archive: e, f), enough to mark a non-contiguous pair.
fn three_workspace_nav_tree() -> (AppStore, Vec<Uuid>, Vec<Uuid>) {
    let mut store = AppStore::new();
    let workspaces: Vec<Uuid> = ["work", "personal", "archive"]
        .into_iter()
        .map(|name| store.add_workspace(name))
        .collect();
    let mut sessions = Vec::new();
    for (workspace, cwds) in workspaces.iter().zip([["/a", "/b"], ["/c", "/d"], ["/e", "/f"]]) {
        for cwd in cwds {
            sessions.push(add(&mut store, *workspace, cwd));
        }
    }
    (store, workspaces, sessions)
}

/// Three workspaces (work: a, b; personal: c; scratch: d), so a step's landing session is unambiguous.
fn workspace_nav_tree() -> (AppStore, Vec<Uuid>, Vec<Uuid>) {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    let scratch = store.add_workspace("scratch");
    let a = add(&mut store, work, "/a");
    let b = add(&mut store, work, "/b");
    let c = add(&mut store, personal, "/c");
    let d = add(&mut store, scratch, "/d");
    (store, vec![work, personal, scratch], vec![a, b, c, d])
}

fn set(ids: &[Uuid]) -> BTreeSet<Uuid> {
    ids.iter().copied().collect()
}

/// The marked set and whether the filter applies.
fn focus_state(store: &AppStore) -> (BTreeSet<Uuid>, bool) {
    (store.focus().ids().clone(), store.focus().enabled())
}

fn visible(store: &AppStore) -> Vec<Uuid> {
    store
        .visible_workspaces()
        .iter()
        .map(|workspace| workspace.id())
        .collect()
}

fn attention(store: &AppStore) -> Vec<Uuid> {
    store.attention_sessions().into_iter().map(Session::id).collect()
}

fn indicator(status: AgentStatus) -> AgentIndicator {
    AgentIndicator {
        status,
        ..AgentIndicator::default()
    }
}

/// Writes the indicator straight onto the session, bypassing the setter, as upstream's tests do.
fn put_status(store: &mut AppStore, id: Uuid, status: AgentStatus) {
    store.session_mut(id).expect("the session exists").agent_indicator = indicator(status);
}

fn owner(store: &AppStore, session: Uuid) -> Uuid {
    store.workspace_for_session(session).expect("the session exists").id()
}

fn step(store: &mut AppStore, direction: WorkspaceNavigation) -> Uuid {
    store.navigate_workspace(direction).expect("a step lands").workspace
}

// MARK: - AppStoreNavigationTests: session steps

#[test]
fn navigate_session_stays_within_focused_workspace() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    let a = add(&mut store, work, "/a");
    add(&mut store, personal, "/b");
    store.select_session(Some(a));
    store.set_focused_workspace(work);
    store.navigate_session(SessionNavigation::Next);
    assert_eq!(store.selected_session_id(), Some(a));
    assert_eq!(focus_state(&store), (set(&[work]), true));
}

#[test]
fn navigate_next_steps_forward_crossing_workspaces() {
    let (mut store, ids) = nav_tree();
    store.select_session(Some(ids[0]));
    for expected in &ids[1..] {
        store.navigate_session(SessionNavigation::Next);
        assert_eq!(store.selected_session_id(), Some(*expected));
    }
}

#[test]
fn navigate_previous_steps_backward_crossing_workspaces() {
    let (mut store, ids) = nav_tree();
    store.select_session(Some(ids[3]));
    for expected in [ids[2], ids[1], ids[0]] {
        store.navigate_session(SessionNavigation::Previous);
        assert_eq!(store.selected_session_id(), Some(expected));
    }
}

#[test]
fn navigate_next_at_last_wraps_to_first() {
    let (mut store, ids) = nav_tree();
    store.select_session(Some(ids[3]));
    store.navigate_session(SessionNavigation::Next);
    assert_eq!(store.selected_session_id(), Some(ids[0]));
}

#[test]
fn navigate_previous_at_first_wraps_to_last() {
    let (mut store, ids) = nav_tree();
    store.select_session(Some(ids[0]));
    store.navigate_session(SessionNavigation::Previous);
    assert_eq!(store.selected_session_id(), Some(ids[3]));
}

#[test]
fn navigate_first_and_last_jump_to_ends() {
    let (mut store, ids) = nav_tree();
    store.select_session(Some(ids[1]));
    store.navigate_session(SessionNavigation::Last);
    assert_eq!(store.selected_session_id(), Some(ids[3]));
    store.navigate_session(SessionNavigation::First);
    assert_eq!(store.selected_session_id(), Some(ids[0]));
}

#[test]
fn navigate_with_no_selection_lands_on_first() {
    let (mut store, ids) = nav_tree();
    store.select_session(None);
    store.navigate_session(SessionNavigation::Next);
    assert_eq!(store.selected_session_id(), Some(ids[0]));
    store.select_session(None);
    store.navigate_session(SessionNavigation::Previous);
    assert_eq!(store.selected_session_id(), Some(ids[0]));
}

#[test]
fn navigate_first_and_last_with_no_selection_land_on_ends() {
    let (mut store, ids) = nav_tree();
    store.select_session(None);
    store.navigate_session(SessionNavigation::Last);
    assert_eq!(store.selected_session_id(), Some(ids[3]));
    store.select_session(None);
    store.navigate_session(SessionNavigation::First);
    assert_eq!(store.selected_session_id(), Some(ids[0]));
}

#[test]
fn navigate_single_session_stays_selected() {
    let mut store = AppStore::new();
    let workspace = store.add_workspace("work");
    let only = add(&mut store, workspace, "/only");
    store.select_session(Some(only));
    for direction in [
        SessionNavigation::Next,
        SessionNavigation::Previous,
        SessionNavigation::First,
        SessionNavigation::Last,
    ] {
        store.navigate_session(direction);
        assert_eq!(store.selected_session_id(), Some(only));
    }
}

#[test]
fn navigate_empty_tree_is_no_op() {
    let mut store = AppStore::new();
    store.add_workspace("work");
    store.navigate_session(SessionNavigation::Next);
    assert_eq!(store.selected_session_id(), None);
    store.navigate_session(SessionNavigation::First);
    assert_eq!(store.selected_session_id(), None);
}

#[test]
fn navigate_routes_through_select_session() {
    let (mut store, ids) = nav_tree();
    store.session_mut(ids[1]).unwrap().unseen_count = 5;
    store.select_session(Some(ids[0]));
    store.navigate_session(SessionNavigation::Next);
    assert_eq!(store.selected_session_id(), Some(ids[1]));
    assert_eq!(store.session(ids[1]).unwrap().unseen_count, 0);
    assert_eq!(store.session_recency().items()[..2], [ids[1], ids[0]]);
}

#[test]
fn session_navigation_wire_mapping() {
    let cases = [
        ("next", Some(SessionNavigation::Next)),
        ("prev", Some(SessionNavigation::Previous)),
        ("previous", Some(SessionNavigation::Previous)),
        ("first", Some(SessionNavigation::First)),
        ("last", Some(SessionNavigation::Last)),
        ("next-attention", Some(SessionNavigation::NextAttention)),
        ("prev-attention", Some(SessionNavigation::PreviousAttention)),
        ("previous-attention", Some(SessionNavigation::PreviousAttention)),
        ("sideways", None),
    ];
    for (wire, expected) in cases {
        assert_eq!(SessionNavigation::from_wire(wire), expected, "{wire}");
    }
}

// MARK: - AppStoreNavigationTests: attention steps

#[test]
fn navigate_attention_steps_through_attention_sessions_only() {
    let (mut store, ids) = nav_tree();
    put_status(&mut store, ids[1], AgentStatus::Blocked);
    put_status(&mut store, ids[3], AgentStatus::Completed);
    store.select_session(Some(ids[0]));
    store.navigate_session(SessionNavigation::NextAttention);
    assert_eq!(store.selected_session_id(), Some(ids[1]));
    store.navigate_session(SessionNavigation::NextAttention);
    assert_eq!(store.selected_session_id(), Some(ids[3]));
}

#[test]
fn navigate_attention_wraps_around() {
    let (mut store, ids) = nav_tree();
    put_status(&mut store, ids[1], AgentStatus::Blocked);
    put_status(&mut store, ids[3], AgentStatus::Completed);
    store.select_session(Some(ids[3]));
    store.navigate_session(SessionNavigation::NextAttention);
    assert_eq!(store.selected_session_id(), Some(ids[1]));
    store.navigate_session(SessionNavigation::PreviousAttention);
    assert_eq!(store.selected_session_id(), Some(ids[3]));
}

#[test]
fn navigate_attention_skips_active_and_idle() {
    let (mut store, ids) = nav_tree();
    put_status(&mut store, ids[1], AgentStatus::Active);
    put_status(&mut store, ids[2], AgentStatus::Blocked);
    store.select_session(Some(ids[0]));
    store.navigate_session(SessionNavigation::NextAttention);
    assert_eq!(store.selected_session_id(), Some(ids[2]));
}

#[test]
fn navigate_attention_with_single_attention_session_stays_put() {
    let (mut store, ids) = nav_tree();
    put_status(&mut store, ids[2], AgentStatus::Completed);
    store.select_session(Some(ids[2]));
    store.navigate_session(SessionNavigation::NextAttention);
    assert_eq!(store.selected_session_id(), Some(ids[2]));
    store.navigate_session(SessionNavigation::PreviousAttention);
    assert_eq!(store.selected_session_id(), Some(ids[2]));
}

#[test]
fn navigate_attention_with_no_attention_sessions_is_no_op() {
    let (mut store, ids) = nav_tree();
    store.select_session(Some(ids[0]));
    store.navigate_session(SessionNavigation::NextAttention);
    assert_eq!(store.selected_session_id(), Some(ids[0]));
}

#[test]
fn navigate_attention_with_no_selection_lands_on_an_attention_end() {
    let (mut store, ids) = nav_tree();
    put_status(&mut store, ids[1], AgentStatus::Blocked);
    put_status(&mut store, ids[3], AgentStatus::Completed);
    store.select_session(None);
    store.navigate_session(SessionNavigation::NextAttention);
    assert_eq!(store.selected_session_id(), Some(ids[1]));
    store.select_session(None);
    store.navigate_session(SessionNavigation::PreviousAttention);
    assert_eq!(store.selected_session_id(), Some(ids[3]));
}

// MARK: - AppStoreNavigationTests: scoping to the filtered set

#[test]
fn navigable_sessions_reflects_flagged_focus_and_unfocused() {
    let (mut store, ids) = nav_tree();
    assert_eq!(store.navigable_session_ids(), ids);
    let work = owner(&store, ids[0]);
    store.set_focused_workspace(work);
    assert_eq!(store.navigable_session_ids(), [ids[0], ids[1]]);
    // marking an id that names no workspace is refused, so the focus survives intact
    store.set_focused_workspace(Uuid::new_v4());
    assert_eq!(store.navigable_session_ids(), [ids[0], ids[1]]);
    // an all-stale set is reachable only by writing the fields directly; the whole tree is then visible
    store.focus.ids = set(&[Uuid::new_v4()]);
    store.focus.enabled = true;
    assert_eq!(store.navigable_session_ids(), ids);
    store.clear_focus();
    store.set_flag(true, ids[1]);
    store.set_flag(true, ids[2]);
    store.set_sidebar_mode(SidebarMode::Flagged);
    assert_eq!(store.navigable_session_ids(), [ids[1], ids[2]]);
}

#[test]
fn navigate_scopes_to_focused_workspace() {
    let (mut store, ids) = nav_tree();
    let work = owner(&store, ids[0]);
    store.set_focused_workspace(work);
    store.select_session(Some(ids[0]));
    store.navigate_session(SessionNavigation::Next);
    assert_eq!(store.selected_session_id(), Some(ids[1]));
    store.navigate_session(SessionNavigation::Next);
    assert_eq!(store.selected_session_id(), Some(ids[0]));
    store.navigate_session(SessionNavigation::Last);
    assert_eq!(store.selected_session_id(), Some(ids[1]));
    store.navigate_session(SessionNavigation::First);
    assert_eq!(store.selected_session_id(), Some(ids[0]));
    assert_eq!(focus_state(&store), (set(&[work]), true));
}

#[test]
fn navigate_scopes_to_every_member_of_a_multi_workspace_set() {
    let (mut store, workspaces, ids) = three_workspace_nav_tree();
    store.set_focus_membership(workspaces[0], true);
    store.set_focus_membership(workspaces[2], true);
    store.set_focus_enabled(true);
    assert_eq!(store.navigable_session_ids(), [ids[0], ids[1], ids[4], ids[5]]);
    store.select_session(Some(ids[0]));
    let steps = [
        (SessionNavigation::Next, ids[1]),
        (SessionNavigation::Next, ids[4]),
        (SessionNavigation::Next, ids[5]),
        (SessionNavigation::Next, ids[0]),
        (SessionNavigation::Previous, ids[5]),
        (SessionNavigation::Previous, ids[4]),
        (SessionNavigation::Last, ids[5]),
        (SessionNavigation::First, ids[0]),
    ];
    for (direction, expected) in steps {
        store.navigate_session(direction);
        assert_eq!(store.selected_session_id(), Some(expected), "{direction:?}");
    }
    assert_eq!(focus_state(&store), (set(&[workspaces[0], workspaces[2]]), true));
}

#[test]
fn navigate_attention_scopes_to_every_member_of_a_multi_workspace_set() {
    let (mut store, workspaces, ids) = three_workspace_nav_tree();
    put_status(&mut store, ids[1], AgentStatus::Blocked);
    put_status(&mut store, ids[3], AgentStatus::Blocked);
    put_status(&mut store, ids[5], AgentStatus::Completed);
    store.set_focus_membership(workspaces[0], true);
    store.set_focus_membership(workspaces[2], true);
    store.set_focus_enabled(true);
    store.select_session(Some(ids[0]));
    for expected in [ids[1], ids[5], ids[1]] {
        store.navigate_session(SessionNavigation::NextAttention);
        assert_eq!(store.selected_session_id(), Some(expected));
    }
}

#[test]
fn navigate_scopes_to_flagged_set() {
    let (mut store, ids) = nav_tree();
    store.set_flag(true, ids[0]);
    store.set_flag(true, ids[2]);
    store.set_sidebar_mode(SidebarMode::Flagged);
    store.select_session(Some(ids[0]));
    store.navigate_session(SessionNavigation::Next);
    assert_eq!(store.selected_session_id(), Some(ids[2]));
    store.navigate_session(SessionNavigation::Next);
    assert_eq!(store.selected_session_id(), Some(ids[0]));
    store.navigate_session(SessionNavigation::First);
    assert_eq!(store.selected_session_id(), Some(ids[0]));
}

#[test]
fn navigate_attention_scopes_to_focused_workspace() {
    let (mut store, ids) = nav_tree();
    put_status(&mut store, ids[1], AgentStatus::Blocked);
    put_status(&mut store, ids[3], AgentStatus::Completed);
    let work = owner(&store, ids[0]);
    store.set_focused_workspace(work);
    store.select_session(Some(ids[0]));
    store.navigate_session(SessionNavigation::NextAttention);
    assert_eq!(store.selected_session_id(), Some(ids[1]));
    store.navigate_session(SessionNavigation::NextAttention);
    assert_eq!(store.selected_session_id(), Some(ids[1]));
}

#[test]
fn navigate_restores_full_set_when_focus_cleared() {
    let (mut store, ids) = nav_tree();
    let work = owner(&store, ids[0]);
    store.set_focused_workspace(work);
    store.select_session(Some(ids[1]));
    store.navigate_session(SessionNavigation::Next);
    assert_eq!(store.selected_session_id(), Some(ids[0]));
    store.clear_focus();
    store.select_session(Some(ids[1]));
    store.navigate_session(SessionNavigation::Next);
    assert_eq!(store.selected_session_id(), Some(ids[2]));
}

// MARK: - AppStoreNavigationTests: attention_sessions

#[test]
fn attention_sessions_filters_out_idle() {
    let mut store = AppStore::new();
    let workspace = store.add_workspace("work");
    add(&mut store, workspace, "/idle");
    let active = add(&mut store, workspace, "/active");
    store.set_agent_indicator(indicator(AgentStatus::Active), active);
    assert_eq!(attention(&store), [active]);
}

#[test]
fn attention_sessions_order_blocked_active_completed() {
    let mut store = AppStore::new();
    let workspace = store.add_workspace("work");
    let completed = add(&mut store, workspace, "/c");
    let active = add(&mut store, workspace, "/a");
    let blocked = add(&mut store, workspace, "/b");
    store.set_agent_indicator(indicator(AgentStatus::Completed), completed);
    store.set_agent_indicator(indicator(AgentStatus::Active), active);
    store.set_agent_indicator(indicator(AgentStatus::Blocked), blocked);
    assert_eq!(attention(&store), [blocked, active, completed]);
}

fn stamp(store: &mut AppStore, id: Uuid, seconds: Option<u64>) {
    store.session_mut(id).unwrap().status_changed_at = seconds.map(|s| SystemTime::UNIX_EPOCH + Duration::from_secs(s));
}

#[test]
fn attention_sessions_within_rank_order_newest_first_nil_last() {
    let mut store = AppStore::new();
    let workspace = store.add_workspace("work");
    let oldest = add(&mut store, workspace, "/old");
    let newest = add(&mut store, workspace, "/new");
    let unstamped = add(&mut store, workspace, "/none");
    for id in [oldest, newest, unstamped] {
        store.set_agent_indicator(indicator(AgentStatus::Blocked), id);
    }
    stamp(&mut store, oldest, Some(100));
    stamp(&mut store, newest, Some(200));
    stamp(&mut store, unstamped, None);
    assert_eq!(attention(&store), [newest, oldest, unstamped]);
}

#[test]
fn attention_sessions_tie_break_stable_for_equal_and_nil_stamps() {
    let mut store = AppStore::new();
    let workspace = store.add_workspace("work");
    let stamped_a = add(&mut store, workspace, "/sa");
    let stamped_b = add(&mut store, workspace, "/sb");
    let nil_a = add(&mut store, workspace, "/na");
    let nil_b = add(&mut store, workspace, "/nb");
    for id in [stamped_a, stamped_b, nil_a, nil_b] {
        store.set_agent_indicator(indicator(AgentStatus::Blocked), id);
    }
    stamp(&mut store, stamped_a, Some(500));
    stamp(&mut store, stamped_b, Some(500));
    stamp(&mut store, nil_a, None);
    stamp(&mut store, nil_b, None);
    assert_eq!(attention(&store), [stamped_a, stamped_b, nil_a, nil_b]);
}

#[test]
fn attention_sessions_span_all_workspaces_ignoring_focus_and_flagged() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let other = store.add_workspace("other");
    let here = add(&mut store, work, "/here");
    let away = add(&mut store, other, "/away");
    store.set_agent_indicator(indicator(AgentStatus::Active), here);
    store.set_agent_indicator(indicator(AgentStatus::Blocked), away);
    store.set_focused_workspace(work);
    assert_eq!(attention(&store), [away, here]);
    store.set_sidebar_mode(SidebarMode::Flagged);
    assert_eq!(attention(&store), [away, here]);
}

#[test]
fn attention_sessions_empty_when_all_idle() {
    let mut store = AppStore::new();
    let workspace = store.add_workspace("work");
    add(&mut store, workspace, "/a");
    add(&mut store, workspace, "/b");
    assert!(attention(&store).is_empty());
}

// MARK: - AppStoreNavigationTests: workspace steps

#[test]
fn navigate_workspace_next_steps_forward_and_wraps() {
    let (mut store, workspaces, sessions) = workspace_nav_tree();
    store.select_session(Some(sessions[1]));
    assert_eq!(step(&mut store, WorkspaceNavigation::Next), workspaces[1]);
    assert_eq!(
        store.selected_session_id(),
        Some(sessions[2]),
        "lands on the target's first session"
    );
    assert_eq!(step(&mut store, WorkspaceNavigation::Next), workspaces[2]);
    assert_eq!(step(&mut store, WorkspaceNavigation::Next), workspaces[0]);
    assert_eq!(store.selected_session_id(), Some(sessions[0]));
}

#[test]
fn navigate_workspace_previous_steps_backward_and_wraps() {
    let (mut store, workspaces, sessions) = workspace_nav_tree();
    store.select_session(Some(sessions[1]));
    assert_eq!(step(&mut store, WorkspaceNavigation::Previous), workspaces[2]);
    assert_eq!(store.selected_session_id(), Some(sessions[3]));
    assert_eq!(step(&mut store, WorkspaceNavigation::Previous), workspaces[1]);
    assert_eq!(store.selected_session_id(), Some(sessions[2]));
}

#[test]
fn navigate_workspace_stays_within_the_focus_filter() {
    let (mut store, workspaces, sessions) = workspace_nav_tree();
    store.select_session(Some(sessions[0]));
    store.set_focused_workspace(workspaces[0]);
    assert_eq!(store.navigate_workspace(WorkspaceNavigation::Next), None);
    assert_eq!(store.selected_session_id(), Some(sessions[0]));
}

#[test]
fn navigate_workspace_is_a_no_op_in_flagged_mode() {
    let (mut store, _, sessions) = workspace_nav_tree();
    store.select_session(Some(sessions[0]));
    store.set_flag(true, sessions[0]);
    store.set_sidebar_mode(SidebarMode::Flagged);
    assert!(!store.can_step_workspaces());
    assert_eq!(store.navigate_workspace(WorkspaceNavigation::Next), None);
    assert_eq!(store.selected_session_id(), Some(sessions[0]));
}

#[test]
fn navigate_workspace_enters_the_sole_visible_workspace_when_current_is_hidden() {
    let (mut store, workspaces, _) = workspace_nav_tree();
    store.set_focus_membership(workspaces[0], true);
    store.set_focus_enabled(true);
    store.select_session(None);
    assert_eq!(visible(&store), [workspaces[0]]);
    assert_eq!(
        store.current_workspace_id(),
        Some(workspaces[2]),
        "the fallback sits outside the visible set"
    );
    assert!(
        store.can_step_workspaces(),
        "the step must not go dead on a reachable workspace"
    );
    assert_eq!(step(&mut store, WorkspaceNavigation::Next), workspaces[0]);
    assert!(
        !store.can_step_workspaces(),
        "inside the sole workspace there is nowhere left to step"
    );
}

#[test]
fn navigate_workspace_is_a_no_op_with_one_workspace() {
    let mut store = AppStore::new();
    let only = store.add_workspace("only");
    let a = add(&mut store, only, "/a");
    let b = add(&mut store, only, "/b");
    store.select_session(Some(b));
    assert_eq!(store.navigate_workspace(WorkspaceNavigation::Next), None);
    assert_eq!(
        store.selected_session_id(),
        Some(b),
        "a lone workspace must not yank the selection"
    );
    assert_ne!(a, b);
}

#[test]
fn navigate_workspace_ignores_collapse_state() {
    let (mut store, workspaces, sessions) = workspace_nav_tree();
    store.select_session(Some(sessions[0]));
    store.set_workspace_expanded(workspaces[1], false);
    assert_eq!(step(&mut store, WorkspaceNavigation::Next), workspaces[1]);
    assert_eq!(store.selected_session_id(), Some(sessions[2]));
}

#[test]
fn current_workspace_collapse_follows_the_sidebar_row_not_the_persisted_flag() {
    let (mut store, workspaces, sessions) = workspace_nav_tree();
    store.select_session(Some(sessions[0]));
    store.set_workspace_expanded(workspaces[0], false);
    assert!(
        store.is_current_workspace_collapsed(),
        "persisted collapsed with no row open reads collapsed"
    );

    store.note_sidebar_expansion(HashSet::from([workspaces[0]]));
    assert!(
        !store.workspaces()[0].is_expanded,
        "the reveal must not touch the persisted flag"
    );
    assert!(!store.is_current_workspace_collapsed(), "an open row reads expanded");

    store.note_sidebar_expansion(HashSet::new());
    assert!(store.is_current_workspace_collapsed());
}

#[test]
fn current_workspace_collapse_falls_back_to_the_persisted_flag_with_no_sidebar() {
    let (mut store, workspaces, sessions) = workspace_nav_tree();
    store.select_session(Some(sessions[0]));
    store.note_sidebar_expansion(HashSet::from([workspaces[0]]));
    store.set_sidebar_visible(false);
    store.set_workspace_expanded(workspaces[0], false);
    assert!(
        store.is_current_workspace_collapsed(),
        "a stale live set must not outvote the persisted flag"
    );

    store.set_workspace_expanded(workspaces[0], true);
    assert!(!store.is_current_workspace_collapsed());
}

// upstream reads the active workspace back through `controlTree`; its source here is `current_workspace_id`
#[test]
fn tree_reports_the_empty_workspace_a_step_landed_on_as_active() {
    let (mut store, workspaces, sessions) = workspace_nav_tree();
    let options = WorkspaceOptions {
        collapsed: false,
        reveal: false,
    };
    let empty = store.add_workspace_with("empty", options);
    store.select_session(Some(sessions[0]));

    let step = store.select_workspace(empty).expect("the workspace exists");
    assert_eq!(step.workspace, empty);
    assert_eq!(step.indicator, None, "an empty workspace selects nothing");
    assert_eq!(
        store.selected_session_id(),
        Some(sessions[0]),
        "the selection stays where it was"
    );
    assert_eq!(store.current_workspace_id(), Some(empty));
    assert_ne!(store.current_workspace_id(), Some(workspaces[0]));
}

#[test]
fn current_workspace_collapse_is_false_with_no_current_workspace() {
    let store = AppStore::new();
    assert_eq!(store.current_workspace_id(), None);
    assert!(!store.is_current_workspace_collapsed());
}

#[test]
fn navigate_workspace_enters_the_visible_set_when_current_is_outside_it() {
    for (direction, entry) in [(WorkspaceNavigation::Next, 0), (WorkspaceNavigation::Previous, 1)] {
        let (mut store, workspaces, _) = workspace_nav_tree();
        store.set_focus_membership(workspaces[0], true);
        store.set_focus_membership(workspaces[1], true);
        store.set_focus_enabled(true);
        store.select_session(None);
        assert_eq!(visible(&store), [workspaces[0], workspaces[1]]);
        assert_eq!(
            store.current_workspace_id(),
            Some(workspaces[2]),
            "the fallback sits outside the set"
        );
        assert_eq!(
            step(&mut store, direction),
            workspaces[entry],
            "{direction:?} enters from its end"
        );
    }
}

// MARK: - AppStoreFocusTests: the focus mutators

#[test]
fn set_focused_workspace_sets_and_clears() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    assert_eq!(focus_state(&store), (set(&[]), false));
    store.set_focused_workspace(work);
    assert_eq!(focus_state(&store), (set(&[work]), true));
    store.clear_focus();
    assert_eq!(focus_state(&store), (set(&[]), false));
}

#[test]
fn clear_focus_empties_a_whole_multi_member_set() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    let side = store.add_workspace("side");
    store.set_focus_membership(work, true);
    store.set_focus_membership(personal, true);
    store.set_focus_enabled(true);

    store.clear_focus();

    assert_eq!(focus_state(&store), (set(&[]), false));
    assert_eq!(visible(&store), [work, personal, side]);
}

#[test]
fn marking_an_id_that_is_not_a_workspace_is_a_clean_no_op() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let phantom = Uuid::new_v4();

    store.set_focus_membership(phantom, true);
    assert!(store.focus().ids().is_empty());
    store.set_focus_enabled(true);
    assert!(!store.focus().enabled());

    store.set_focused_workspace(phantom);
    assert_eq!(focus_state(&store), (set(&[]), false));

    store.set_focus_membership(work, true);
    store.focus.ids.insert(phantom);
    store.set_focus_membership(phantom, false);
    assert_eq!(store.focus().ids(), &set(&[work]));
}

#[test]
fn is_sole_focus_is_true_only_for_the_one_marked_workspace_with_the_filter_on() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");

    store.set_focus_membership(work, true);
    assert!(!store.is_sole_focus(work));
    store.set_focus_enabled(true);
    assert!(store.is_sole_focus(work));
    assert!(!store.is_sole_focus(personal));
    store.set_focus_membership(personal, true);
    assert!(!store.is_sole_focus(work));
}

#[test]
fn toggle_focused_workspace_narrows_a_multi_member_set_then_clears() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    store.set_focus_membership(work, true);
    store.set_focus_membership(personal, true);
    store.set_focus_enabled(true);

    store.toggle_focused_workspace(personal);
    assert_eq!(focus_state(&store), (set(&[personal]), true));
    assert_eq!(visible(&store), [personal]);

    store.toggle_focused_workspace(personal);
    assert_eq!(focus_state(&store), (set(&[]), false));

    store.toggle_focused_workspace(work);
    assert_eq!(focus_state(&store), (set(&[work]), true));
    store.set_focus_enabled(false);
    store.toggle_focused_workspace(work);
    assert_eq!(focus_state(&store), (set(&[work]), true));
}

#[test]
fn current_workspace_focus_helpers_follow_the_selection() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    add(&mut store, work, "/a");
    assert_eq!(store.current_workspace_id(), Some(work));
    assert!(!store.is_current_workspace_sole_focus() && !store.is_current_workspace_focus_member());

    store.set_focus_membership(work, true);
    assert!(store.is_current_workspace_focus_member());
    assert!(!store.is_current_workspace_sole_focus());
    store.set_focus_enabled(true);
    assert!(store.is_current_workspace_sole_focus());

    add(&mut store, personal, "/b");
    assert_eq!(store.current_workspace_id(), Some(personal));
    assert!(!store.is_current_workspace_sole_focus() && !store.is_current_workspace_focus_member());
}

#[test]
fn set_focused_workspace_replaces_the_whole_set() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    store.set_focus_membership(work, true);
    store.set_focus_membership(personal, true);
    assert_eq!(store.focus().ids(), &set(&[work, personal]));
    store.set_focused_workspace(personal);
    assert_eq!(focus_state(&store), (set(&[personal]), true));
}

#[test]
fn set_focus_membership_adds_and_removes() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    store.set_focus_membership(work, true);
    assert_eq!(focus_state(&store), (set(&[work]), false));
    store.set_focus_membership(personal, true);
    assert_eq!(focus_state(&store), (set(&[work, personal]), false));
    store.set_focus_enabled(true);
    store.set_focus_membership(work, false);
    assert_eq!(focus_state(&store), (set(&[personal]), true));
}

#[test]
fn adding_to_the_set_never_turns_the_filter_on() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    store.set_focus_membership(work, true);
    assert_eq!(focus_state(&store), (set(&[work]), false));
    assert_eq!(visible(&store), [work, personal]);
    store.set_focus_enabled(true);
    store.set_focus_membership(personal, true);
    assert_eq!(focus_state(&store), (set(&[work, personal]), true));
}

#[test]
fn removing_the_last_member_disables_the_filter() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    store.set_focus_membership(work, true);
    store.set_focus_enabled(true);
    store.set_focus_membership(work, false);
    assert_eq!(focus_state(&store), (set(&[]), false));
}

#[test]
fn set_focus_membership_on_a_disabled_set_keeps_it_disabled_when_removing() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    store.set_focus_membership(work, true);
    store.set_focus_membership(personal, true);
    store.set_focus_enabled(true);
    store.set_focus_enabled(false);
    store.set_focus_membership(work, false);
    assert_eq!(focus_state(&store), (set(&[personal]), false));
}

#[test]
fn set_focus_enabled_round_trips_without_losing_the_set() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    store.set_focus_membership(work, true);
    store.set_focus_membership(personal, true);
    store.set_focus_enabled(true);
    store.set_focus_enabled(false);
    assert_eq!(focus_state(&store), (set(&[work, personal]), false));
    store.set_focus_enabled(true);
    assert_eq!(focus_state(&store), (set(&[work, personal]), true));
}

#[test]
fn set_focus_enabled_refuses_an_empty_set() {
    let mut store = AppStore::new();
    store.add_workspace("work");
    store.set_focus_enabled(true);
    assert!(!store.focus().enabled());
}

#[test]
fn focus_setters_are_no_op_writes_when_unchanged() {
    let mut store = AppStore::new();
    let workspace = store.add_workspace("work");
    store.set_focused_workspace(workspace);
    store.take_outbox();
    store.set_focused_workspace(workspace);
    store.set_focus_membership(workspace, true);
    store.set_focus_enabled(true);
    assert_eq!(store.outbox().save, SaveRequest::None);
    assert_eq!(focus_state(&store), (set(&[workspace]), true));
}

#[test]
fn set_focus_enabled_on_an_empty_set_is_a_no_op_write() {
    let mut store = AppStore::new();
    store.add_workspace("work");
    store.take_outbox();
    store.set_focus_enabled(true);
    assert_eq!(store.outbox().save, SaveRequest::None);
}

#[test]
fn workspace_focus_modes_drive_the_store() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");

    store.apply_focus_mode(FocusMode::Add, work);
    store.apply_focus_mode(FocusMode::Add, personal);
    assert_eq!(focus_state(&store), (set(&[work, personal]), false));

    store.apply_focus_mode(FocusMode::Toggle, work);
    assert_eq!(focus_state(&store), (set(&[work]), true));
    assert_eq!(visible(&store), [work]);

    store.apply_focus_mode(FocusMode::Toggle, work);
    assert_eq!(focus_state(&store), (set(&[]), false));

    store.apply_focus_mode(FocusMode::On, personal);
    assert_eq!(focus_state(&store), (set(&[personal]), true));
    store.apply_focus_mode(FocusMode::Off, personal);
    assert_eq!(focus_state(&store), (set(&[]), false));
}

// MARK: - AppStoreFocusTests: the visible workspaces

#[test]
fn visible_workspaces_returns_all_when_the_filter_is_off() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    assert_eq!(visible(&store), [work, personal]);
    store.set_focus_membership(work, true);
    store.set_focus_enabled(true);
    store.set_focus_enabled(false);
    assert_eq!(visible(&store), [work, personal]);
}

#[test]
fn visible_workspaces_returns_one_when_focused() {
    let mut store = AppStore::new();
    store.add_workspace("work");
    let personal = store.add_workspace("personal");
    store.set_focused_workspace(personal);
    assert_eq!(visible(&store), [personal]);
}

#[test]
fn visible_workspaces_returns_the_marked_subset_in_tree_order() {
    let mut store = AppStore::new();
    let one = store.add_workspace("one");
    store.add_workspace("two");
    let three = store.add_workspace("three");
    store.set_focus_membership(three, true);
    store.set_focus_membership(one, true);
    store.set_focus_enabled(true);
    assert_eq!(visible(&store), [one, three]);
}

#[test]
fn visible_workspaces_keeps_survivors_for_a_partially_stale_set() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    store.add_workspace("personal");
    store.focus.ids = set(&[work, Uuid::new_v4()]);
    store.focus.enabled = true;
    assert_eq!(visible(&store), [work]);
}

#[test]
fn visible_workspaces_falls_back_to_all_for_an_all_stale_set() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    store.focus.ids = set(&[Uuid::new_v4()]);
    store.focus.enabled = true;
    assert_eq!(visible(&store), [work, personal]);
}

#[test]
fn removing_a_member_prunes_it_and_keeps_the_rest_filtered() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let doomed = store.add_workspace("doomed");
    store.add_workspace("other");
    store.set_focus_membership(work, true);
    store.set_focus_membership(doomed, true);
    store.set_focus_enabled(true);
    store.remove_workspace(doomed);
    assert_eq!(focus_state(&store), (set(&[work]), true));
    assert_eq!(visible(&store), [work]);
}

#[test]
fn removing_the_last_member_workspace_disables_the_filter() {
    let mut store = AppStore::new();
    store.add_workspace("other");
    let doomed = store.add_workspace("doomed");
    store.set_focus_membership(doomed, true);
    store.set_focus_enabled(true);
    store.remove_workspace(doomed);
    assert_eq!(focus_state(&store), (set(&[]), false));
}

#[test]
fn removing_a_non_member_workspace_leaves_the_filter_untouched() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    let doomed = store.add_workspace("doomed");
    store.set_focus_membership(work, true);
    store.set_focus_membership(personal, true);
    store.set_focus_enabled(true);
    store.remove_workspace(doomed);
    assert_eq!(focus_state(&store), (set(&[work, personal]), true));
}

// MARK: - AppStoreFocusTests: the selection against the visible set

#[test]
fn selecting_outside_the_set_disables_the_filter_but_keeps_every_member() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    let outside = store.add_workspace("outside");
    add(&mut store, work, "/a");
    store.set_focus_membership(work, true);
    store.set_focus_membership(personal, true);
    store.set_focus_enabled(true);
    let stray = add_background(&mut store, outside, "/b");
    store.select_session(Some(stray));
    assert_eq!(focus_state(&store), (set(&[work, personal]), false));
    store.set_focus_enabled(true);
    assert_eq!(visible(&store), [work, personal]);
}

#[test]
fn applying_the_filter_moves_the_selection_into_the_visible_set() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    let in_set = add(&mut store, work, "/a");
    let outside = add(&mut store, personal, "/b");
    store.select_session(Some(in_set));
    store.select_session(Some(outside));
    store.set_focus_membership(work, true);
    assert_eq!(store.selected_session_id(), Some(outside));

    store.set_focus_enabled(true);
    assert_eq!(store.selected_session_id(), Some(in_set));
    assert_eq!(focus_state(&store), (set(&[work]), true));
}

#[test]
fn focusing_a_workspace_moves_the_selection_into_it() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    let target = add(&mut store, work, "/a");
    let outside = add(&mut store, personal, "/b");
    store.select_session(Some(target));
    store.select_session(Some(outside));

    store.set_focused_workspace(work);
    assert_eq!(store.selected_session_id(), Some(target));
    assert_eq!(focus_state(&store), (set(&[work]), true));
}

#[test]
fn switching_to_the_flagged_view_moves_the_selection_into_it() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let flagged = add(&mut store, work, "/a");
    let plain = add(&mut store, work, "/b");
    store.set_flag(true, flagged);
    store.select_session(Some(plain));

    store.set_sidebar_mode(SidebarMode::Flagged);
    assert_eq!(store.selected_session_id(), Some(flagged));
}

#[test]
fn switching_to_the_flagged_view_never_disables_the_workspace_filter() {
    let mut store = AppStore::new();
    let marked = store.add_workspace("marked");
    let other = store.add_workspace("other");
    let in_set = add(&mut store, marked, "/a");
    let flagged_outside = add_background(&mut store, other, "/b");
    store.set_flag(true, flagged_outside);
    store.select_session(Some(in_set));
    store.set_focused_workspace(marked);

    store.set_sidebar_mode(SidebarMode::Flagged);
    assert_eq!(store.selected_session_id(), Some(flagged_outside));
    assert_eq!(focus_state(&store), (set(&[marked]), true));

    store.set_sidebar_mode(SidebarMode::Tree);
    assert_eq!(store.selected_session_id(), Some(in_set));
    assert_eq!(focus_state(&store), (set(&[marked]), true));
}

#[test]
fn unflagging_the_active_session_in_flagged_mode_moves_the_selection() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let survivor = add(&mut store, work, "/a");
    let active = add(&mut store, work, "/b");
    store.set_flag(true, survivor);
    store.set_flag(true, active);
    store.select_session(Some(active));
    store.set_sidebar_mode(SidebarMode::Flagged);
    assert_eq!(store.selected_session_id(), Some(active));

    store.set_flag(false, active);
    assert_eq!(store.selected_session_id(), Some(survivor));
}

#[test]
fn batch_unflagging_the_active_session_in_flagged_mode_moves_the_selection() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let survivor = add(&mut store, work, "/a");
    let active = add(&mut store, work, "/b");
    let also_unflagged = add_background(&mut store, work, "/c");
    store.set_flags(true, &[survivor, active, also_unflagged]);
    store.select_session(Some(active));
    store.set_sidebar_mode(SidebarMode::Flagged);

    store.set_flags(false, &[active, also_unflagged]);
    let flagged: Vec<Uuid> = store.flagged_sessions().into_iter().map(Session::id).collect();
    assert_eq!(flagged, [survivor]);
    assert_eq!(store.selected_session_id(), Some(survivor));
}

#[test]
fn unmarking_the_active_sessions_workspace_moves_the_selection_to_a_surviving_member() {
    let mut store = AppStore::new();
    let staying = store.add_workspace("staying");
    let leaving = store.add_workspace("leaving");
    let survivor = add(&mut store, staying, "/a");
    let active = add(&mut store, leaving, "/b");
    store.set_focus_membership(staying, true);
    store.set_focus_membership(leaving, true);
    store.set_focus_enabled(true);
    store.select_session(Some(active));
    assert_eq!(store.selected_session_id(), Some(active));

    store.set_focus_membership(leaving, false);
    assert_eq!(store.selected_session_id(), Some(survivor));
    assert_eq!(focus_state(&store), (set(&[staying]), true));
}

#[test]
fn flagging_a_session_while_the_flagged_view_is_empty_moves_the_selection_into_it() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let active = add(&mut store, work, "/a");
    let other = add_background(&mut store, work, "/b");
    store.select_session(Some(active));
    store.set_sidebar_mode(SidebarMode::Flagged);
    assert_eq!(store.selected_session_id(), Some(active));

    store.set_flag(true, other);
    assert_eq!(store.selected_session_id(), Some(other));
}

#[test]
fn marking_a_populated_workspace_while_the_filter_shows_an_empty_one_moves_the_selection() {
    let mut store = AppStore::new();
    let home = store.add_workspace("home");
    let empty = store.add_workspace("empty");
    let populated = store.add_workspace("populated");
    let active = add(&mut store, home, "/a");
    let target = add_background(&mut store, populated, "/b");
    store.select_session(Some(target));
    store.select_session(Some(active));
    store.set_focused_workspace(empty);
    assert_eq!(store.selected_session_id(), Some(active));

    store.set_focus_membership(populated, true);
    assert_eq!(store.selected_session_id(), Some(target));
}

#[test]
fn a_background_insertion_into_an_empty_visible_set_does_not_repair_it() {
    let mut store = AppStore::new();
    let home = store.add_workspace("home");
    let empty = store.add_workspace("empty");
    let active = add(&mut store, home, "/a");
    store.select_session(Some(active));
    store.set_focused_workspace(empty);
    assert_eq!(store.selected_session_id(), Some(active));

    add_background(&mut store, empty, "/b");
    assert_eq!(store.navigable_sessions().len(), 1);
    assert_eq!(store.selected_session_id(), Some(active));
}

#[test]
fn clearing_every_flag_in_flagged_mode_leaves_the_selection_alone() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let active = add(&mut store, work, "/a");
    store.set_flag(true, active);
    store.select_session(Some(active));
    store.set_sidebar_mode(SidebarMode::Flagged);

    store.clear_flags();
    assert!(store.flagged_sessions().is_empty());
    assert_eq!(store.selected_session_id(), Some(active));
}

#[test]
fn unflagging_in_tree_mode_does_not_move_the_selection() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    add(&mut store, work, "/a");
    let active = add(&mut store, work, "/b");
    store.set_flag(true, active);
    store.select_session(Some(active));

    store.set_flag(false, active);
    assert_eq!(store.selected_session_id(), Some(active));
}

#[test]
fn the_narrowing_reselect_prefers_the_most_recent_visible_session() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    let first = add(&mut store, work, "/a");
    let recent = add_background(&mut store, work, "/b");
    let outside = add(&mut store, personal, "/c");
    store.select_session(Some(first));
    store.select_session(Some(recent));
    store.select_session(Some(outside));

    store.set_focused_workspace(work);
    assert_eq!(store.selected_session_id(), Some(recent));
}

#[test]
fn the_narrowing_reselect_falls_back_to_the_first_visible_session_without_recency() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    let first = add_background(&mut store, work, "/a");
    add_background(&mut store, work, "/b");
    let outside = add(&mut store, personal, "/c");
    store.select_session(Some(outside));

    store.set_focused_workspace(work);
    assert_eq!(store.selected_session_id(), Some(first));
}

#[test]
fn a_narrowing_with_no_visible_session_leaves_the_selection_alone() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let empty = store.add_workspace("empty");
    let active = add(&mut store, work, "/a");
    store.select_session(Some(active));

    store.set_focused_workspace(empty);
    assert_eq!(store.selected_session_id(), Some(active));
}

#[test]
fn selecting_inside_the_set_changes_nothing() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    add(&mut store, work, "/a");
    let in_set = add_background(&mut store, personal, "/b");
    store.set_focus_membership(work, true);
    store.set_focus_membership(personal, true);
    store.set_focus_enabled(true);
    store.select_session(Some(in_set));
    assert_eq!(focus_state(&store), (set(&[work, personal]), true));
}

// MARK: - AppStoreFocusTests: revealing new workspaces, the drop fallback, the sidebar selection

#[test]
fn add_workspace_joins_the_set_only_while_the_filter_is_on() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    store.set_focus_membership(work, true);
    store.set_focus_enabled(false);
    store.add_workspace("quiet");
    assert_eq!(focus_state(&store), (set(&[work]), false));
    store.set_focus_enabled(true);
    let fresh = store.add_workspace("fresh");
    assert_eq!(focus_state(&store), (set(&[work, fresh]), true));
    assert_eq!(visible(&store), [work, fresh]);
}

#[test]
fn add_workspace_without_reveal_does_not_widen_the_set() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    store.set_focus_membership(work, true);
    store.set_focus_enabled(true);
    let options = WorkspaceOptions {
        collapsed: false,
        reveal: false,
    };
    store.add_workspace_with("background", options);
    assert_eq!(focus_state(&store), (set(&[work]), true));
    assert_eq!(visible(&store), [work]);
}

#[test]
fn drop_fallback_is_nil_with_nothing_marked() {
    let mut store = AppStore::new();
    store.add_workspace("work");
    assert_eq!(store.sole_focused_workspace_id(), None);
}

#[test]
fn drop_fallback_is_the_sole_member_while_the_filter_is_on() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    store.add_workspace("personal");
    store.set_focused_workspace(work);
    assert_eq!(store.sole_focused_workspace_id(), Some(work));
}

#[test]
fn drop_fallback_is_nil_when_the_filter_is_off() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    store.add_workspace("personal");
    store.set_focus_membership(work, true);
    store.set_focus_enabled(false);
    assert_eq!(store.focus().ids(), &set(&[work]));
    assert_eq!(store.sole_focused_workspace_id(), None);
}

#[test]
fn drop_fallback_is_nil_with_two_members_marked() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    store.set_focus_membership(work, true);
    store.set_focus_membership(personal, true);
    store.set_focus_enabled(true);
    assert_eq!(store.sole_focused_workspace_id(), None);
}

#[test]
fn workspace_focus_prunes_rows_outside_focused_workspace() {
    let mut store = AppStore::new();
    let one = store.add_workspace("one");
    let two = store.add_workspace("two");
    let a = add(&mut store, one, "/a");
    let b = add(&mut store, two, "/b");
    store.set_sidebar_selection(vec![a, b]);

    store.set_focused_workspace(two);

    assert_eq!(store.sidebar_selection_ids(), [b]);
    store.clear_focus();
    assert_eq!(
        store.sidebar_selection_ids(),
        [b],
        "rows hidden by the focus filter must not re-enter the selection when unfocused"
    );
}
