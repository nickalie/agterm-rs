//! The window store's own rules, ported from upstream's `AppStoreTests`, `AppStoreNameSanitizeTests`,
//! `AppStoreSidebarTests` and `AppStoreCurrentWorkspaceTests`: workspace and session create, select, close and
//! remove, recency, renames, font sizes, the sidebar state and multi-selection, batch moves, and the current
//! workspace. Upstream's on-disk save checks read the store's outbox here.

use std::time::SystemTime;

use ngterm_host::fakes::FakeSurface;
use uuid::Uuid;

use super::support::{add, add_background, ids};
use crate::model::{
    AgentIndicator, AgentStatus, AppStore, NewSession, SIDEBAR_WIDTH_MAX, SaveRequest, Session, SessionNavigation,
    SidebarMode, StatusPane, Workspace, WorkspaceOptions,
};

/// The session with `id`, which the test knows exists.
fn get(store: &AppStore, id: Uuid) -> &Session {
    store.session(id).expect("the session exists")
}

/// The ids of every workspace, in order.
fn workspace_ids(store: &AppStore) -> Vec<Uuid> {
    store.workspaces().iter().map(Workspace::id).collect()
}

/// The names of every workspace, in order.
fn workspace_names(store: &AppStore) -> Vec<String> {
    store
        .workspaces()
        .iter()
        .map(|workspace| workspace.name.clone())
        .collect()
}

/// The ids of the visible workspaces, in order.
fn visible_ids(store: &AppStore) -> Vec<Uuid> {
    store
        .visible_workspaces()
        .iter()
        .map(|workspace| workspace.id())
        .collect()
}

/// Upstream's `addWorkspace(name:, revealNewWorkspace: false)`.
fn add_hidden_workspace(store: &mut AppStore, name: &str) -> Uuid {
    store.add_workspace_with(
        name,
        WorkspaceOptions {
            collapsed: false,
            reveal: false,
        },
    )
}

/// Adds a selected session with extra creation arguments.
fn add_with(store: &mut AppStore, workspace: Uuid, spec: NewSession) -> Uuid {
    store.add_session(workspace, spec).expect("the workspace exists")
}

fn indicator(status: AgentStatus) -> AgentIndicator {
    AgentIndicator {
        status,
        ..AgentIndicator::default()
    }
}

fn auto_reset(status: AgentStatus) -> AgentIndicator {
    AgentIndicator {
        status,
        auto_reset: true,
        ..AgentIndicator::default()
    }
}

/// A store with one workspace holding sessions at `cwds`, each selected as it was added.
fn work_with(cwds: &[&str]) -> (AppStore, Uuid, Vec<Uuid>) {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let sessions = cwds.iter().map(|cwd| add(&mut store, ws, cwd)).collect();
    (store, ws, sessions)
}

// MARK: - Create

#[test]
fn empty_store_has_no_selection_or_active_session() {
    let store = AppStore::new();
    assert!(store.workspaces().is_empty());
    assert_eq!(store.selected_session_id(), None);
    assert!(store.active_session().is_none());
}

#[test]
fn sidebar_visible_defaults_true_and_toggles() {
    let mut store = AppStore::new();
    assert!(store.sidebar().visible);
    store.toggle_sidebar_visible();
    assert!(!store.sidebar().visible);
    store.set_sidebar_visible(true);
    assert!(store.sidebar().visible);
    store.set_sidebar_visible(true);
    assert!(store.sidebar().visible);
}

#[test]
fn default_workspace_name_counts_up() {
    let mut store = AppStore::new();
    assert_eq!(store.default_workspace_name(), "workspace 1");
    store.add_workspace("work");
    assert_eq!(store.default_workspace_name(), "workspace 2");
}

#[test]
fn add_workspace_appends() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    assert_eq!(workspace_ids(&store), [work, personal]);
    assert_eq!(workspace_names(&store), ["work", "personal"]);
}

#[test]
fn add_session_appends_and_selects() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let session = add(&mut store, ws, "/tmp");
    assert_eq!(ids(&store, ws), [session]);
    assert_eq!(get(&store, session).initial_cwd(), "/tmp");
    assert_eq!(store.selected_session_id(), Some(session));
    assert_eq!(store.active_session().map(Session::id), Some(session));
}

#[test]
fn add_session_without_select_leaves_selection_untouched() {
    let (mut store, ws, sessions) = work_with(&["/a"]);
    let first = sessions[0];
    assert_eq!(store.selected_session_id(), Some(first));
    let background = add_background(&mut store, ws, "/b");
    assert_eq!(ids(&store, ws), [first, background]);
    assert_eq!(store.selected_session_id(), Some(first));
    assert_eq!(store.active_session().map(Session::id), Some(first));
    assert_eq!(
        store.session_recency().items(),
        [first],
        "the background add was not recorded"
    );
}

#[test]
fn add_session_carries_initial_command() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let with_command = NewSession {
        command: Some("ssh host".to_owned()),
        ..NewSession::at_cwd("/tmp")
    };
    let with_command = add_with(&mut store, ws, with_command);
    assert_eq!(get(&store, with_command).launch.command.as_deref(), Some("ssh host"));
    let plain = add(&mut store, ws, "/tmp");
    assert_eq!(get(&store, plain).launch.command, None);
}

#[test]
fn add_session_carries_command_wait() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let spec = NewSession {
        command: Some("make test".to_owned()),
        wait: true,
        ..NewSession::at_cwd("/tmp")
    };
    let held = add_with(&mut store, ws, spec.clone());
    assert!(get(&store, held).launch.wait);
    // the default is false: a command session closes when its command exits
    let plain = add_with(&mut store, ws, NewSession { wait: false, ..spec });
    assert!(!get(&store, plain).launch.wait);
}

#[test]
fn add_session_seeds_custom_name() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let named = |name: &str| NewSession {
        name: Some(name.to_owned()),
        ..NewSession::at_cwd("/tmp")
    };
    let myhost = add_with(&mut store, ws, named("myhost"));
    assert_eq!(get(&store, myhost).custom_name.as_deref(), Some("myhost"));
    assert_eq!(get(&store, myhost).display_name(), "myhost");
    // a blank name clears to none, leaving the auto basename (as rename_session does)
    let blank = add_with(&mut store, ws, named("  "));
    assert_eq!(get(&store, blank).custom_name, None);
    let plain = add(&mut store, ws, "/tmp");
    assert_eq!(get(&store, plain).custom_name, None);
}

#[test]
fn add_session_to_unknown_workspace_returns_nil() {
    let mut store = AppStore::new();
    assert_eq!(store.add_session(Uuid::new_v4(), NewSession::at_cwd("/tmp")), None);
    assert_eq!(store.selected_session_id(), None);
}

#[test]
fn workspace_named_finds_exact_trimmed_match() {
    let mut store = AppStore::new();
    let servers = store.add_workspace("servers");
    store.add_workspace("other");
    let named = |name: &str| store.workspace_named(name).map(Workspace::id);
    assert_eq!(named("servers"), Some(servers));
    assert_eq!(named("  servers  "), Some(servers), "the input is trimmed");
    assert_eq!(named("Servers"), None, "case-sensitive");
    assert_eq!(named("missing"), None);
    assert_eq!(named("   "), None, "blank");
}

#[test]
fn ensure_workspace_reuses_existing_else_creates() {
    let mut store = AppStore::new();
    let existing = store.add_workspace("servers");
    let before = store.workspaces().len();
    assert_eq!(store.ensure_workspace("servers", true), Some(existing));
    assert_eq!(store.workspaces().len(), before);
    let created = store
        .ensure_workspace("  fresh  ", true)
        .expect("a non-blank name creates");
    assert_eq!(
        store.workspace(created).map(|workspace| workspace.name.as_str()),
        Some("fresh")
    );
    assert_eq!(store.workspaces().len(), before + 1);
    assert_eq!(store.ensure_workspace("fresh", true), Some(created));
    assert_eq!(store.workspaces().len(), before + 1);
    assert_eq!(store.ensure_workspace("   ", true), None);
    assert_eq!(store.workspaces().len(), before + 1);
}

// MARK: - Select, unseen, indicators

#[test]
fn select_session_updates_active() {
    let (mut store, _, sessions) = work_with(&["/a", "/b"]);
    let (a, b) = (sessions[0], sessions[1]);
    store.select_session(Some(a));
    assert_eq!(store.active_session().map(Session::id), Some(a));
    store.select_session(Some(b));
    assert_eq!(store.active_session().map(Session::id), Some(b));
}

#[test]
fn select_unknown_session_is_ignored() {
    let (mut store, _, sessions) = work_with(&["/a"]);
    store.select_session(Some(Uuid::new_v4()));
    assert_eq!(store.selected_session_id(), Some(sessions[0]));
}

#[test]
fn select_session_clears_only_its_unseen_badge() {
    let (mut store, _, sessions) = work_with(&["/a", "/b"]);
    let (a, b) = (sessions[0], sessions[1]);
    store.session_mut(a).unwrap().unseen_count = 3;
    store.session_mut(b).unwrap().unseen_count = 2;
    store.select_session(Some(a));
    assert_eq!(get(&store, a).unseen_count, 0);
    assert_eq!(get(&store, b).unseen_count, 2);
}

#[test]
fn clear_unseen_resets_count_and_ignores_unknown_id() {
    let (mut store, _, sessions) = work_with(&["/a"]);
    let a = sessions[0];
    store.session_mut(a).unwrap().unseen_count = 5;
    store.clear_unseen(a);
    assert_eq!(get(&store, a).unseen_count, 0);
    store.clear_unseen(Uuid::new_v4());
}

#[test]
fn clear_unseen_does_not_change_selection() {
    // the focus-free invariant behind session.seen: marking a session seen clears its badge and nothing else
    let (mut store, _, sessions) = work_with(&["/a", "/b"]);
    let (a, b) = (sessions[0], sessions[1]);
    store.session_mut(a).unwrap().unseen_count = 3;
    store.select_session(Some(b));
    store.clear_unseen(a);
    assert_eq!(store.selected_session_id(), Some(b));
    assert_eq!(get(&store, a).unseen_count, 0);
}

#[test]
fn set_agent_indicator_sets_field_on_right_session() {
    let (mut store, _, sessions) = work_with(&["/a", "/b"]);
    let (a, b) = (sessions[0], sessions[1]);
    let blinking = AgentIndicator {
        blink: true,
        ..indicator(AgentStatus::Active)
    };
    store.set_agent_indicator(blinking.clone(), a);
    assert_eq!(get(&store, a).agent_indicator, blinking);
    assert_eq!(get(&store, b).agent_indicator, AgentIndicator::default());
}

#[test]
fn set_agent_indicator_unknown_session_is_noop() {
    let (mut store, _, sessions) = work_with(&["/a"]);
    store.set_agent_indicator(indicator(AgentStatus::Blocked), Uuid::new_v4());
    assert_eq!(get(&store, sessions[0]).agent_indicator, AgentIndicator::default());
}

#[test]
fn set_agent_indicator_stamps_status_changed_at_on_non_idle() {
    let (mut store, _, sessions) = work_with(&["/a"]);
    let a = sessions[0];
    assert_eq!(get(&store, a).status_changed_at, None);
    let before = SystemTime::now();
    store.set_agent_indicator(indicator(AgentStatus::Active), a);
    let stamp = get(&store, a).status_changed_at.expect("stamped");
    assert!(stamp >= before);
}

#[test]
fn set_agent_indicator_stamps_and_refreshes_status_changed_at_on_idle() {
    let (mut store, _, sessions) = work_with(&["/a"]);
    let a = sessions[0];
    store.set_agent_indicator(indicator(AgentStatus::Blocked), a);
    assert!(get(&store, a).status_changed_at.is_some());
    for _ in 0..2 {
        store.session_mut(a).unwrap().status_changed_at = Some(SystemTime::UNIX_EPOCH);
        let before = SystemTime::now();
        store.set_agent_indicator(AgentIndicator::default(), a);
        let stamp = get(&store, a).status_changed_at.expect("stamped");
        assert!(stamp >= before);
        assert!(stamp <= SystemTime::now());
    }
}

#[test]
fn set_agent_indicator_reasserting_non_idle_updates_status_changed_at() {
    let (mut store, _, sessions) = work_with(&["/a"]);
    let a = sessions[0];
    store.session_mut(a).unwrap().status_changed_at = Some(SystemTime::UNIX_EPOCH);
    let before = SystemTime::now();
    store.set_agent_indicator(indicator(AgentStatus::Active), a);
    let stamp = get(&store, a).status_changed_at.expect("stamped");
    assert!(
        stamp >= before,
        "re-asserting moves the stamp to now, not just off the epoch"
    );
}

#[test]
fn select_session_keeps_non_auto_reset_indicator() {
    let (mut store, _, sessions) = work_with(&["/a", "/b"]);
    let (a, b) = (sessions[0], sessions[1]);
    store.set_agent_indicator(indicator(AgentStatus::Active), a);
    store.select_session(Some(a));
    assert_eq!(get(&store, a).agent_indicator, indicator(AgentStatus::Active));
    store.select_session(Some(b));
    assert_eq!(get(&store, a).agent_indicator, indicator(AgentStatus::Active));
}

#[test]
fn select_session_resets_auto_reset_indicator() {
    let (mut store, _, sessions) = work_with(&["/a"]);
    let a = sessions[0];
    store.session_mut(a).unwrap().split.exists = true;
    let routed = AgentIndicator {
        status_pane: Some(StatusPane::Right),
        ..auto_reset(AgentStatus::Completed)
    };
    store.set_agent_indicator(routed.clone(), a);
    let captured = store.select_session(Some(a));
    assert_eq!(
        captured,
        Some(routed),
        "visiting clears it but returns the pre-clear routing value"
    );
    assert_eq!(get(&store, a).agent_indicator, AgentIndicator::default());
}

#[test]
fn select_session_does_not_reset_unrelated_auto_reset_indicator() {
    let (mut store, _, sessions) = work_with(&["/a", "/b"]);
    let (a, b) = (sessions[0], sessions[1]);
    store.set_agent_indicator(auto_reset(AgentStatus::Completed), a);
    store.select_session(Some(b));
    assert_eq!(get(&store, a).agent_indicator, auto_reset(AgentStatus::Completed));
}

#[test]
fn switching_away_clears_auto_reset_indicator_on_the_left_session() {
    let (mut store, _, sessions) = work_with(&["/a", "/b"]);
    let (a, b) = (sessions[0], sessions[1]);
    store.select_session(Some(a));
    // the agent finishes while a is selected, so no visit fires and its completed flash lingers
    store.set_agent_indicator(auto_reset(AgentStatus::Completed), a);
    assert_eq!(get(&store, a).agent_indicator, auto_reset(AgentStatus::Completed));
    store.select_session(Some(b));
    assert_eq!(get(&store, a).agent_indicator, AgentIndicator::default());
    assert_eq!(get(&store, b).agent_indicator, AgentIndicator::default());
}

#[test]
fn workspace_for_session_derives_owner() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    let in_work = add(&mut store, work, "/a");
    let in_personal = add(&mut store, personal, "/b");
    let owner = |id| store.workspace_for_session(id).map(Workspace::id);
    assert_eq!(owner(in_work), Some(work));
    assert_eq!(owner(in_personal), Some(personal));
    assert_eq!(owner(Uuid::new_v4()), None);
}

// MARK: - Close and remove

#[test]
fn close_non_active_session_keeps_selection() {
    let (mut store, ws, sessions) = work_with(&["/a", "/b"]);
    let (a, b) = (sessions[0], sessions[1]);
    store.select_session(Some(b));
    store.close_session(a);
    assert_eq!(ids(&store, ws), [b]);
    assert_eq!(store.selected_session_id(), Some(b));
}

#[test]
fn close_active_session_reselects_next() {
    let (mut store, _, sessions) = work_with(&["/a", "/b"]);
    let (a, b) = (sessions[0], sessions[1]);
    store.select_session(Some(a));
    store.close_session(a);
    assert_eq!(store.selected_session_id(), Some(b));
    assert_eq!(store.active_session().map(Session::id), Some(b));
}

#[test]
fn close_active_last_session_reselects_previous() {
    let (mut store, _, sessions) = work_with(&["/a", "/b"]);
    let (a, b) = (sessions[0], sessions[1]);
    store.select_session(Some(b));
    store.close_session(b);
    assert_eq!(store.selected_session_id(), Some(a));
}

#[test]
fn close_active_session_falls_back_to_other_workspace() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    let other = add(&mut store, personal, "/other");
    let only = add(&mut store, work, "/only");
    store.select_session(Some(only));
    store.close_session(only);
    assert!(store.workspaces()[0].sessions.is_empty());
    assert_eq!(store.selected_session_id(), Some(other));
}

#[test]
fn close_last_session_clears_selection() {
    let (mut store, _, sessions) = work_with(&["/only"]);
    store.close_session(sessions[0]);
    assert_eq!(store.selected_session_id(), None);
    assert!(store.active_session().is_none());
}

#[test]
fn close_session_tears_down_surface() {
    let (mut store, _, sessions) = work_with(&["/a"]);
    let surface = FakeSurface::new("left");
    let probe = surface.probe();
    store
        .session_mut(sessions[0])
        .unwrap()
        .surface
        .replace(Box::new(surface));
    store.close_session(sessions[0]);
    assert_eq!(probe.teardowns(), 1);
}

#[test]
fn close_session_tears_down_split_surface() {
    let (mut store, _, sessions) = work_with(&["/a"]);
    let split = FakeSurface::new("right");
    let probe = split.probe();
    store
        .session_mut(sessions[0])
        .unwrap()
        .split_surface
        .replace(Box::new(split));
    store.close_session(sessions[0]);
    assert_eq!(probe.teardowns(), 1);
}

#[test]
fn close_unknown_session_is_ignored() {
    let (mut store, ws, sessions) = work_with(&["/a"]);
    let a = sessions[0];
    store.select_session(Some(a));
    assert!(store.close_session(Uuid::new_v4()).is_none());
    assert_eq!(ids(&store, ws), [a]);
    assert_eq!(store.selected_session_id(), Some(a));
}

#[test]
fn remove_workspace_tears_down_sessions_and_prunes_recency() {
    let mut store = AppStore::new();
    let keep = store.add_workspace("keep");
    let doomed = store.add_workspace("doomed");
    let session = add(&mut store, doomed, "/a");
    let (surface, split) = (FakeSurface::new("left"), FakeSurface::new("right"));
    let (surface_probe, split_probe) = (surface.probe(), split.probe());
    let slots = store.session_mut(session).unwrap();
    slots.surface.replace(Box::new(surface));
    slots.split_surface.replace(Box::new(split));
    store.remove_workspace(doomed);
    assert_eq!(workspace_ids(&store), [keep]);
    assert_eq!(surface_probe.teardowns(), 1);
    assert_eq!(split_probe.teardowns(), 1);
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
    assert_eq!(workspace_ids(&store), [only]);
}

// MARK: - Recency

#[test]
fn selection_updates_recency_most_recent_first() {
    let (mut store, _, sessions) = work_with(&["/a", "/b"]);
    let (a, b) = (sessions[0], sessions[1]);
    assert_eq!(store.session_recency().items(), [b, a], "b was selected last");
    store.select_session(Some(a));
    assert_eq!(store.session_recency().items(), [a, b]);
}

#[test]
fn close_session_removes_from_recency() {
    let (mut store, _, sessions) = work_with(&["/a", "/b"]);
    let (a, b) = (sessions[0], sessions[1]);
    store.close_session(b);
    assert!(!store.session_recency().items().contains(&b));
    assert_eq!(store.session_recency().items(), [a]);
}

#[test]
fn recent_sessions_returns_most_recent_first_respecting_limit() {
    let (mut store, _, sessions) = work_with(&["/a", "/b", "/c"]);
    let (a, b, c) = (sessions[0], sessions[1], sessions[2]);
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
    store.close_session(b);
    assert_eq!(store.recent_sessions(9), [a], "a closed session drops out");
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
    for id in [d, a, c, b] {
        store.select_session(Some(id));
    }

    assert_eq!(store.navigable_recent_sessions(2), [c, a]);

    store.set_focused_workspace(personal);
    assert_eq!(store.selected_session_id(), Some(c));
    assert_eq!(store.navigable_recent_sessions(9), [d]);
}

// MARK: - Font sizes and saves

#[test]
fn set_font_size_records_value() {
    let (mut store, _, sessions) = work_with(&["/a"]);
    let session = sessions[0];
    assert_eq!(get(&store, session).font_size, None);
    store.set_font_size(session, 16.0);
    assert_eq!(get(&store, session).font_size, Some(16.0));
    store.set_font_size(session, 16.0);
    assert_eq!(get(&store, session).font_size, Some(16.0));
    store.set_font_size(session, 13.0);
    assert_eq!(get(&store, session).font_size, Some(13.0));
}

#[test]
fn reset_session_font_sizes_clears_all_overrides() {
    let (mut store, _, sessions) = work_with(&["/a", "/b"]);
    let (a, b) = (sessions[0], sessions[1]);
    store.set_font_size(a, 16.0);
    store.set_font_size(b, 18.0);
    store.reset_session_font_sizes();
    assert_eq!(get(&store, a).font_size, None);
    assert_eq!(get(&store, b).font_size, None);
}

#[test]
fn set_font_size_unknown_session_is_ignored() {
    let (mut store, _, sessions) = work_with(&["/a"]);
    store.set_font_size(Uuid::new_v4(), 20.0);
    assert_eq!(get(&store, sessions[0]).font_size, None);
}

#[test]
fn select_session_defers_write_until_save_flushes() {
    let (mut store, _, sessions) = work_with(&["/a", "/b"]);
    assert_eq!(
        store.outbox().save,
        SaveRequest::Now,
        "adding a session is a structural, immediate save"
    );
    store.take_outbox();
    store.select_session(Some(sessions[0]));
    assert_eq!(
        store.outbox().save,
        SaveRequest::Debounced,
        "a selection must not hit disk at once"
    );
}

#[test]
fn set_font_size_defers_write_until_save_flushes() {
    let (mut store, _, sessions) = work_with(&["/a"]);
    store.take_outbox();
    store.set_font_size(sessions[0], 18.0);
    assert_eq!(
        store.outbox().save,
        SaveRequest::Debounced,
        "a font change must not hit disk at once"
    );
}

// MARK: - Rename

#[test]
fn rename_unknown_workspace_is_ignored() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    store.rename_workspace(Uuid::new_v4(), "renamed");
    assert_eq!(workspace_names(&store), ["work"]);
    assert_eq!(workspace_ids(&store), [ws]);
}

#[test]
fn rename_session_sets_custom_name() {
    let (mut store, _, sessions) = work_with(&["/a"]);
    store.rename_session(sessions[0], "build");
    assert_eq!(get(&store, sessions[0]).custom_name.as_deref(), Some("build"));
    assert_eq!(get(&store, sessions[0]).display_name(), "build");
}

#[test]
fn rename_session_with_blank_clears_custom_name() {
    let (mut store, _, sessions) = work_with(&["/Users/user/foo"]);
    store.rename_session(sessions[0], "build");
    store.rename_session(sessions[0], "   ");
    assert_eq!(get(&store, sessions[0]).custom_name, None);
    assert_eq!(get(&store, sessions[0]).display_name(), "foo");
}

#[test]
fn rename_session_trims_whitespace() {
    let (mut store, _, sessions) = work_with(&["/a"]);
    store.rename_session(sessions[0], "  build  ");
    assert_eq!(get(&store, sessions[0]).custom_name.as_deref(), Some("build"));
}

#[test]
fn rename_workspace_sets_name() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    store.rename_workspace(ws, "personal");
    assert_eq!(store.workspaces()[0].name, "personal");
}

#[test]
fn rename_workspace_with_blank_is_ignored() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    store.rename_workspace(ws, "   ");
    assert_eq!(store.workspaces()[0].name, "work");
}

// MARK: - Sidebar multi-selection (the transient selection model)

#[test]
fn context_targets_use_full_selection_only_when_clicked_row_is_selected() {
    let (mut store, _, sessions) = work_with(&["/a", "/b", "/c"]);
    let (a, b, c) = (sessions[0], sessions[1], sessions[2]);
    store.select_session(Some(a));
    store.set_sidebar_selection(vec![b, a]);

    assert_eq!(store.sidebar_selection_ids(), [a, b]);
    assert_eq!(store.sidebar_selection_targets(Some(a)), [a, b]);
    assert_eq!(store.sidebar_selection_targets(Some(c)), [c]);
    assert_eq!(store.sidebar_selection_targets(None), [a, b]);
}

#[test]
fn selecting_session_resets_transient_sidebar_selection() {
    let (mut store, _, sessions) = work_with(&["/a", "/b"]);
    let (a, b) = (sessions[0], sessions[1]);

    store.set_sidebar_selection(vec![a, b]);
    assert_eq!(store.sidebar_selection_ids(), [a, b]);

    store.select_session(Some(b));
    assert_eq!(store.selected_session_id(), Some(b));
    assert_eq!(store.sidebar_selection_ids(), [b]);
}

#[test]
fn selecting_session_can_preserve_transient_sidebar_selection() {
    let (mut store, _, sessions) = work_with(&["/a", "/b", "/c"]);
    let (a, b, c) = (sessions[0], sessions[1], sessions[2]);

    store.select_session_in_sidebar(Some(c), vec![a, c]);

    assert_eq!(store.selected_session_id(), Some(c));
    assert_eq!(store.sidebar_selection_ids(), [a, c]);
    assert_eq!(store.sidebar_selection_targets(Some(a)), [a, c]);
    assert_eq!(store.sidebar_selection_targets(Some(b)), [b]);
}

#[test]
fn sidebar_selection_falls_back_to_active_when_stored_selection_is_stale() {
    let (mut store, ws, sessions) = work_with(&["/a", "/b"]);
    let a = sessions[0];
    store.set_sidebar_selection(sessions.clone());

    let c = add(&mut store, ws, "/c");

    assert_eq!(store.selected_session_id(), Some(c));
    assert!(store.sidebar_selection_ids().is_empty());
    assert_eq!(store.sidebar_selection_targets(None), [c]);
    assert_eq!(store.sidebar_selection_targets(Some(a)), [a]);
}

#[test]
fn sidebar_targets_drop_rows_hidden_by_mode_or_focus() {
    let mut store = AppStore::new();
    let ws1 = store.add_workspace("one");
    let ws2 = store.add_workspace("two");
    let a = add(&mut store, ws1, "/a");
    let b = add(&mut store, ws1, "/b");
    let c = add(&mut store, ws2, "/c");
    store.set_flag(true, a);

    store.select_session(Some(a));
    store.set_sidebar_selection(vec![a, b, c]);
    store.set_sidebar_mode(SidebarMode::Flagged);

    assert_eq!(store.sidebar_selection_ids(), [a]);
    assert_eq!(store.sidebar_selection_targets(Some(a)), [a]);

    store.set_sidebar_mode(SidebarMode::Tree);
    store.select_session(Some(a));
    store.set_sidebar_selection(vec![a, c]);
    store.set_focused_workspace(ws1);

    assert_eq!(store.sidebar_selection_ids(), [a]);
    assert_eq!(store.sidebar_selection_targets(Some(a)), [a]);
}

// the prune-guard tests below hide selected rows, then RE-SHOW them and assert again: `sidebar_selection_ids`
// filters on read, so only the second step catches a missing prune.

#[test]
fn mode_change_prunes_rows_hidden_in_flagged_mode() {
    let (mut store, _, sessions) = work_with(&["/a", "/b"]);
    let b = sessions[1];
    store.set_flag(true, b);
    store.set_sidebar_selection(sessions.clone());

    store.set_sidebar_mode(SidebarMode::Flagged);

    assert_eq!(store.sidebar_selection_ids(), [b]);
    store.set_sidebar_mode(SidebarMode::Tree);
    assert_eq!(
        store.sidebar_selection_ids(),
        [b],
        "rows hidden by the mode switch must not re-enter the selection when visible again"
    );
}

/// Two flagged sessions, the sidebar in flagged mode, both rows selected.
fn flagged_pair() -> (AppStore, Uuid, Uuid) {
    let (mut store, _, sessions) = work_with(&["/a", "/b"]);
    store.set_flags(true, &sessions);
    store.set_sidebar_mode(SidebarMode::Flagged);
    store.set_sidebar_selection(sessions.clone());
    (store, sessions[0], sessions[1])
}

#[test]
fn single_flag_change_prunes_row_hidden_in_flagged_mode() {
    let (mut store, a, b) = flagged_pair();

    store.set_flag(false, a);

    assert_eq!(store.sidebar_selection_ids(), [b]);
    store.set_flag(true, a);
    assert_eq!(
        store.sidebar_selection_ids(),
        [b],
        "an unflagged row must not re-enter the selection when re-flagged"
    );
}

#[test]
fn batch_flag_change_prunes_rows_hidden_in_flagged_mode() {
    let (mut store, a, b) = flagged_pair();

    store.set_flags(false, &[a, b]);

    assert!(store.sidebar_selection_ids().is_empty());
    store.set_flags(true, &[a, b]);
    assert!(store.sidebar_selection_ids().is_empty());
}

#[test]
fn clear_flags_prunes_rows_hidden_in_flagged_mode() {
    let (mut store, a, b) = flagged_pair();

    store.clear_flags();

    assert!(store.sidebar_selection_ids().is_empty());
    store.set_flags(true, &[a, b]);
    assert!(
        store.sidebar_selection_ids().is_empty(),
        "cleared rows must not re-enter the selection when visible again"
    );
}

#[test]
fn batch_flag_sets_every_selected_session_in_one_command() {
    let (mut store, _, sessions) = work_with(&["/a", "/b"]);

    store.set_flags(true, &sessions);

    assert!(get(&store, sessions[0]).flagged);
    assert!(get(&store, sessions[1]).flagged);
}

// MARK: - Batch moves

#[test]
fn batch_move_appends_cross_workspace_sessions_and_leaves_target_sessions_in_place() {
    let mut store = AppStore::new();
    let ws1 = store.add_workspace("one");
    let ws2 = store.add_workspace("two");
    let a = add(&mut store, ws1, "/a");
    let b = add(&mut store, ws1, "/b");
    let c = add(&mut store, ws2, "/c");

    let affected = store.move_sessions(&[c, b, a], ws2, None);

    assert_eq!(affected, 2);
    assert!(ids(&store, ws1).is_empty());
    assert_eq!(ids(&store, ws2), [c, a, b]);
}

#[test]
fn one_element_batch_move_within_workspace_matches_singular_append() {
    let (mut store, ws, sessions) = work_with(&["/a", "/b", "/c"]);
    let (a, b, c) = (sessions[0], sessions[1], sessions[2]);

    let affected = store.move_sessions(&[a], ws, None);

    assert_eq!(affected, 1);
    assert_eq!(ids(&store, ws), [b, c, a]);
}

#[test]
fn multi_element_batch_move_already_in_target_reports_zero_and_keeps_order() {
    let (mut store, ws, sessions) = work_with(&["/a", "/b"]);

    let affected = store.move_sessions(&sessions, ws, None);

    assert_eq!(affected, 0);
    assert_eq!(ids(&store, ws), sessions);
}

#[test]
fn batch_move_inserts_cross_workspace_sessions_at_drop_index() {
    let mut store = AppStore::new();
    let ws1 = store.add_workspace("one");
    let ws2 = store.add_workspace("two");
    let moving = [add(&mut store, ws1, "/a"), add(&mut store, ws1, "/b")];
    let staying = [
        add(&mut store, ws2, "/c"),
        add(&mut store, ws2, "/d"),
        add(&mut store, ws2, "/e"),
    ];

    store.move_sessions(&moving, ws2, Some(1));

    assert!(ids(&store, ws1).is_empty());
    assert_eq!(
        ids(&store, ws2),
        [staying[0], moving[0], moving[1], staying[1], staying[2]]
    );
}

#[test]
fn batch_move_reorders_same_workspace_at_drop_index() {
    let (mut store, ws, sessions) = work_with(&["/a", "/b", "/c", "/d", "/e"]);
    store.move_sessions(&sessions[..2], ws, Some(2));

    let order: Vec<Uuid> = [2, 3, 0, 1, 4].iter().map(|&index| sessions[index]).collect();
    assert_eq!(ids(&store, ws), order, "c, d, a, b, e");
}

#[test]
fn batch_move_mixed_selection_adjusts_target_insertion_after_removal() {
    let mut store = AppStore::new();
    let ws1 = store.add_workspace("one");
    let ws2 = store.add_workspace("two");
    let a = add(&mut store, ws1, "/a");
    let b = add(&mut store, ws2, "/b");
    let c = add(&mut store, ws2, "/c");
    let d = add(&mut store, ws2, "/d");

    store.move_sessions(&[a, b], ws2, Some(1));

    assert!(ids(&store, ws1).is_empty());
    assert_eq!(ids(&store, ws2), [c, a, b, d]);
}

// MARK: - Name sanitizing (AppStoreNameSanitizeTests)

#[test]
fn add_session_strips_interior_control_characters_from_name_and_cwd() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let spec = NewSession {
        name: Some("demo\ntouch /tmp/pwned".to_owned()),
        ..NewSession::at_cwd("/work\ntouch /tmp/pwned")
    };
    let session = add_with(&mut store, ws, spec);
    let session = get(&store, session);
    assert_eq!(session.custom_name.as_deref(), Some("demotouch /tmp/pwned"));
    assert_eq!(session.initial_cwd(), "/worktouch /tmp/pwned");
    assert_eq!(session.effective_cwd(), "/worktouch /tmp/pwned");
}

#[test]
fn rename_session_strips_interior_control_characters() {
    let (mut store, _, sessions) = work_with(&["/a"]);
    store.rename_session(sessions[0], "  demo\ntouch /tmp/pwned  ");
    assert_eq!(
        get(&store, sessions[0]).custom_name.as_deref(),
        Some("demotouch /tmp/pwned")
    );
}

#[test]
fn add_workspace_strips_interior_control_characters() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("prod\ntouch /tmp/pwned");
    assert_eq!(
        store.workspace(ws).map(|workspace| workspace.name.as_str()),
        Some("prodtouch /tmp/pwned")
    );
}

#[test]
fn rename_workspace_strips_interior_control_characters() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    store.rename_workspace(ws, "prod\ntouch /tmp/pwned");
    assert_eq!(store.workspaces()[0].name, "prodtouch /tmp/pwned");
}

#[test]
fn workspace_named_sanitizes_the_needle() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("prod\ntouch /tmp/pwned");
    assert_eq!(
        store.workspace_named("prod\ntouch /tmp/pwned").map(Workspace::id),
        Some(ws)
    );
}

#[test]
fn ensure_workspace_with_control_character_name_stays_idempotent() {
    let mut store = AppStore::new();
    let first = store.ensure_workspace("prod\ntouch /tmp/pwned", true).expect("created");
    let second = store.ensure_workspace("prod\ntouch /tmp/pwned", true).expect("reused");
    assert_eq!(second, first);
    let matching = store
        .workspaces()
        .iter()
        .filter(|workspace| workspace.name == "prodtouch /tmp/pwned")
        .count();
    assert_eq!(matching, 1);
    // the blank gate runs after sanitizing, so a control-only name is none, not a new empty workspace
    assert_eq!(store.ensure_workspace("\u{07}", true), None);
}

#[test]
fn workspace_name_treats_a_blank_as_absent() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    assert_eq!(store.workspace_name(ws).as_deref(), Some("work"));
    // rename_workspace rejects a blank; a rebuild from a snapshot has no such gate
    store.workspaces[0].name = "   ".to_owned();
    assert_eq!(store.workspace_name(ws), None);
}

// MARK: - Sidebar width (AppStoreSidebarTests)

#[test]
fn set_sidebar_width_clamps_to_drag_bounds() {
    for (requested, applied) in [(300.0, 300.0), (120.0, 160.0), (900.0, 560.0)] {
        let mut store = AppStore::new();

        store.set_sidebar_width(requested);

        assert!(
            (store.sidebar().width - applied).abs() < f64::EPSILON,
            "{requested} → {applied}"
        );
    }
}

#[test]
fn set_sidebar_width_keeps_fractional_points() {
    let mut store = AppStore::new();

    store.set_sidebar_width(271.34);

    assert!((store.sidebar().width - 271.34).abs() < f64::EPSILON);
}

#[test]
fn set_sidebar_width_writes_through_to_disk() {
    let mut store = AppStore::new();

    store.set_sidebar_width(271.3);

    assert_eq!(store.outbox().save, SaveRequest::Now);
}

#[test]
fn set_sidebar_width_to_the_current_clamped_value_does_not_write() {
    let mut store = AppStore::new();
    store.set_sidebar_width(900.0);
    assert_eq!(
        store.outbox().save,
        SaveRequest::Now,
        "the first set clamps to the max and saves"
    );
    store.take_outbox();

    store.set_sidebar_width(1200.0);

    assert_eq!(store.outbox().save, SaveRequest::None, "a no-op setter must not write");
    assert!((store.sidebar().width - SIDEBAR_WIDTH_MAX).abs() < f64::EPSILON);
}

// MARK: - Current workspace (AppStoreCurrentWorkspaceTests)

#[test]
fn current_workspace_follows_selection_then_falls_back_to_last() {
    let mut store = AppStore::new();
    assert_eq!(store.current_workspace_id(), None);
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    assert_eq!(store.current_workspace_id(), Some(personal));
    let session = add(&mut store, work, "/a");
    store.select_session(Some(session));
    assert_eq!(store.current_workspace_id(), Some(work));
    store.select_session(None);
    assert_eq!(store.current_workspace_id(), Some(personal));
}

/// A store with `work` holding one selected session, as most current-workspace cases start.
fn selected_work() -> (AppStore, Uuid, Uuid) {
    let (mut store, work, sessions) = work_with(&["/a"]);
    store.select_session(Some(sessions[0]));
    (store, work, sessions[0])
}

// pins discussion #325: a foreground create used to leave the previous workspace current, so Rename Workspace
// edited the wrong row and the next new session landed in the old workspace
#[test]
fn foreground_create_becomes_current_over_the_selected_sessions_workspace() {
    let (mut store, work, _) = selected_work();
    assert_eq!(store.current_workspace_id(), Some(work));

    let fresh = store.add_workspace("fresh");
    assert_eq!(store.current_workspace_id(), Some(fresh));
}

#[test]
fn selecting_a_session_drops_the_fresh_workspace_as_current() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let first = add(&mut store, work, "/a");
    let other = store.add_workspace("other");
    add(&mut store, other, "/b");
    store.add_workspace("fresh");

    store.select_session(Some(first));
    assert_eq!(store.current_workspace_id(), Some(work));
}

// `navigate_session` with one visible session and `overlay open --follow` both reselect the active session
// without moving the user, so a same-value selection must leave the target alone
#[test]
fn reselecting_the_active_session_keeps_the_fresh_workspace_current() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let session = add(&mut store, work, "/a");
    let fresh = store.add_workspace("fresh");

    store.select_session(Some(session));
    assert_eq!(store.current_workspace_id(), Some(fresh));
    store.navigate_session(SessionNavigation::Next);
    assert_eq!(store.selected_session_id(), Some(session));
    assert_eq!(store.current_workspace_id(), Some(fresh));
}

#[test]
fn selecting_a_session_on_creation_drops_the_fresh_workspace_as_current() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    store.add_workspace("fresh");

    add(&mut store, work, "/a");
    assert_eq!(store.current_workspace_id(), Some(work));
}

#[test]
fn background_create_does_not_become_current() {
    let (mut store, work, _) = selected_work();

    add_hidden_workspace(&mut store, "background");
    assert_eq!(store.current_workspace_id(), Some(work));
}

#[test]
fn background_session_add_keeps_the_fresh_workspace_current() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let fresh = store.add_workspace("fresh");

    add_background(&mut store, work, "/a");
    assert_eq!(store.current_workspace_id(), Some(fresh));
}

#[test]
fn removing_the_fresh_workspace_falls_back_to_selection() {
    let (mut store, work, _) = selected_work();
    let fresh = store.add_workspace("fresh");

    store.remove_workspace(fresh);
    assert_eq!(store.current_workspace_id(), Some(work));
}

#[test]
fn closing_the_selected_session_drops_the_fresh_workspace_as_current() {
    let (mut store, work, sessions) = work_with(&["/a", "/b"]);
    let first = sessions[0];
    store.select_session(Some(first));
    let fresh = store.add_workspace("fresh");
    assert_eq!(store.current_workspace_id(), Some(fresh));

    store.close_session(first);
    assert_eq!(store.current_workspace_id(), Some(work));
}

#[test]
fn closing_the_only_session_drops_the_fresh_workspace_as_current() {
    let (mut store, _, session) = selected_work();
    let fresh = store.add_workspace("fresh");
    let last = add_hidden_workspace(&mut store, "last");
    assert_eq!(store.current_workspace_id(), Some(fresh));

    store.close_session(session);
    assert_eq!(store.selected_session_id(), None);
    assert_eq!(store.current_workspace_id(), Some(last));
}

// `workspace select` on the workspace already holding the selection lands on a same-value `select_session`, which
// by itself leaves the fresh preference alive
#[test]
fn selecting_a_workspace_drops_the_fresh_workspace_as_current() {
    let (mut store, work, session) = selected_work();
    store.add_workspace("fresh");

    assert!(store.select_workspace(work).is_some());
    assert_eq!(store.selected_session_id(), Some(session));
    assert_eq!(store.current_workspace_id(), Some(work));
}

// an empty workspace has no session to select, so targeting is the only thing `workspace select` can do there;
// reporting success while the target stayed elsewhere was the defect
#[test]
fn selecting_an_empty_workspace_still_makes_it_current() {
    let (mut store, _, session) = selected_work();
    store.add_workspace("fresh");
    let empty = add_hidden_workspace(&mut store, "empty");

    assert!(store.select_workspace(empty).is_some());
    assert_eq!(store.selected_session_id(), Some(session));
    assert_eq!(store.current_workspace_id(), Some(empty));
}

#[test]
fn selecting_a_filtered_out_empty_workspace_reveals_it() {
    let (mut store, work, _) = selected_work();
    let empty = add_hidden_workspace(&mut store, "empty");
    let other = add_hidden_workspace(&mut store, "other");
    store.set_focused_workspace(work);
    assert_eq!(visible_ids(&store), [work]);

    assert!(store.select_workspace(empty).is_some());
    assert_eq!(store.current_workspace_id(), Some(empty));
    assert_eq!(visible_ids(&store), [work, empty], "the target must be on screen");
    assert!(store.focus().enabled(), "the filter must widen, not switch off");
    assert!(
        !store.focus().ids().contains(&other),
        "an unrelated workspace stays filtered out"
    );
}

#[test]
fn removing_another_workspace_keeps_the_fresh_target() {
    let (mut store, _, _) = selected_work();
    let spare = add_hidden_workspace(&mut store, "spare");
    let fresh = store.add_workspace("fresh");

    store.remove_workspace(spare);
    assert_eq!(
        store.current_workspace_id(),
        Some(fresh),
        "only the target's own removal hands targeting back"
    );
}

#[test]
fn selecting_an_unknown_workspace_is_a_no_op() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");

    assert!(store.select_workspace(Uuid::new_v4()).is_none());
    assert_eq!(store.current_workspace_id(), Some(work));
}

// the sidebar builds its row cache from `visible_workspaces`, so a filtered-out target would leave Rename Workspace
// enabled and doing nothing, and un-filtering must not bring it back
#[test]
fn filtering_out_the_fresh_workspace_drops_it_for_good() {
    let (mut store, work, _) = selected_work();
    let fresh = store.add_workspace("fresh");
    assert_eq!(store.current_workspace_id(), Some(fresh));

    store.set_focused_workspace(work);
    assert_eq!(visible_ids(&store), [work]);
    assert_eq!(store.current_workspace_id(), Some(work));

    store.clear_focus();
    assert_eq!(
        store.current_workspace_id(),
        Some(work),
        "un-filtering must not revive a target already handed back"
    );
}
