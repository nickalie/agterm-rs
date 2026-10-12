//! The in-memory behavior of the window set, ported from upstream's `WindowLibraryTests.swift`.
//!
//! Upstream's `WindowLibrary(directory:)` over an empty state directory seeds one window, so
//! [`WindowLibrary::seeded`] stands in for it. The halves of a test that read back the index or a per-window file
//! (a relaunch, a deleted file) wait for persistence and are left out.

use uuid::Uuid;

use super::support::add;
use crate::model::naming::is_auto_window_name;
use crate::model::{AppStore, WindowInfo, WindowLibrary, WorkspaceNavigation};

const HOME: &str = "/home/tester";

fn library() -> WindowLibrary {
    WindowLibrary::seeded(HOME)
}

fn first_window(library: &WindowLibrary) -> Uuid {
    library.windows()[0].id
}

fn store(library: &WindowLibrary, id: Uuid) -> &AppStore {
    library.store(id).expect("the window is open")
}

fn store_mut(library: &mut WindowLibrary, id: Uuid) -> &mut AppStore {
    library.store_mut(id).expect("the window is open")
}

/// The first session of the window's first workspace: the one a new window is seeded with.
fn seeded_session(library: &WindowLibrary, id: Uuid) -> Uuid {
    store(library, id).workspaces()[0].sessions[0].id()
}

/// Adds a session at `/tmp` to the window's first workspace.
fn add_to_first_workspace(library: &mut WindowLibrary, id: Uuid) -> Uuid {
    let store = store_mut(library, id);
    let workspace = store.workspaces()[0].id();
    add(store, workspace, "/tmp")
}

fn sidebar_visible(library: &WindowLibrary, id: Uuid) -> bool {
    store(library, id).sidebar().visible
}

fn info(name: &str) -> WindowInfo {
    WindowInfo {
        id: Uuid::new_v4(),
        name: name.to_owned(),
    }
}

/// Upstream also pins the ask registry here; asks are not ported, so only the window keeping its store and session
/// through the refused removal and the terminating close remains.
#[test]
fn window_removal_no_ops_keep_the_session_ask() {
    for terminating in [false, true] {
        let mut library = library();
        let window = library.active_window_id().unwrap();
        let session = library.active_store().unwrap().active_session().unwrap().id();

        if terminating {
            library.is_terminating = true;
            library.close_window(window);
        } else {
            library.remove_window(window);
        }

        assert!(library.store(window).unwrap().session(session).is_some());
    }
}

// MARK: - Seeding

#[test]
fn fresh_library_seeds_one_window_with_default_tree() {
    let library = library();
    assert_eq!(library.windows().len(), 1);
    assert_eq!(library.windows()[0].name, "window 1");
    let store = store(&library, first_window(&library));
    assert_eq!(store.workspaces().len(), 1);
    assert_eq!(store.workspaces()[0].name, "workspace 1");
    assert_eq!(store.workspaces()[0].sessions.len(), 1);
    assert_eq!(library.open_ids(), [first_window(&library)]);
}

#[test]
fn window_name_for_id_returns_name_or_empty() {
    let mut library = library();
    assert_eq!(library.window_name(Some(first_window(&library))), "window 1");
    let work = library.new_window(Some("work"));
    assert_eq!(library.window_name(Some(work)), "work");
    assert_eq!(library.window_name(None), "");
    assert_eq!(library.window_name(Some(Uuid::new_v4())), "");
}

#[test]
fn custom_window_name_skips_auto_names_and_unknown_ids() {
    let mut library = library();
    assert_eq!(library.custom_window_name(first_window(&library)), None);
    let work = library.new_window(Some("work"));
    assert_eq!(library.custom_window_name(work), Some("work"));
    assert_eq!(library.custom_window_name(Uuid::new_v4()), None);
}

#[test]
fn all_open_sessions_flattens_every_session_across_windows() {
    let mut library = library();
    assert_eq!(library.all_open_sessions().len(), 1);
    let second = library.new_window(Some("work"));
    add_to_first_workspace(&mut library, second);
    // two windows: window 1 (1 session) + work (2 sessions) = 3.
    assert_eq!(library.all_open_sessions().len(), 3);
}

#[test]
fn total_unseen_count_sums_every_session_across_windows() {
    let mut library = library();
    assert_eq!(library.total_unseen_count(), 0);
    let first = first_window(&library);
    let first_session = seeded_session(&library, first);
    store_mut(&mut library, first)
        .session_mut(first_session)
        .unwrap()
        .unseen_count = 2;
    let second = library.new_window(Some("work"));
    let second_session = seeded_session(&library, second);
    store_mut(&mut library, second)
        .session_mut(second_session)
        .unwrap()
        .unseen_count = 3;
    let extra = add_to_first_workspace(&mut library, second);
    store_mut(&mut library, second).session_mut(extra).unwrap().unseen_count = 5;
    // 2 (window 1) + 3 + 5 (work) = 10 across both open windows.
    assert_eq!(library.total_unseen_count(), 10);
    library.close_window(second);
    assert_eq!(library.total_unseen_count(), 2);
}

#[test]
fn default_window_name_counts_up() {
    let mut library = library();
    assert_eq!(library.default_window_name(), "window 2");
    library.new_window(None);
    assert_eq!(library.default_window_name(), "window 3");
}

#[test]
fn window_info_distinguishes_auto_from_custom_names() {
    // auto "window N" names are omitted from the title bar
    assert!(!info("window 1").has_custom_name());
    assert!(!info("window 12").has_custom_name());
    assert!(is_auto_window_name("window 1"));
    assert!(info("work").has_custom_name());
    assert!(info("window").has_custom_name()); // no number
    assert!(info("window 0").has_custom_name()); // number must be >= 1
    assert!(info("Window 1").has_custom_name()); // case-sensitive vs the auto scheme
    assert!(info("my window 2").has_custom_name()); // extra words
}

// MARK: - Add / list / rename / delete

#[test]
fn new_window_appends_opens_and_seeds() {
    let mut library = library();
    let id = library.new_window(Some("work"));
    assert!(library.windows().iter().any(|info| info.name == "work"));
    assert!(library.is_open(id));
    let store = store(&library, id);
    assert_eq!(store.workspaces().len(), 1);
    assert_eq!(store.workspaces()[0].sessions.len(), 1);
}

#[test]
fn new_window_becomes_frontmost_and_active() {
    let mut library = library();
    let first = first_window(&library);
    let id = library.new_window(Some("work"));
    // a new window is the active one immediately — the palette / quick terminal key off this, so they target the
    // new window without waiting on its first focus.
    assert_ne!(id, first);
    assert_eq!(library.frontmost_window_id, Some(id));
    assert_eq!(library.active_window_id(), Some(id));
}

#[test]
fn apply_inactive_window_sidebar_hiding_shows_only_frontmost() {
    let mut library = library();
    let first = first_window(&library);
    let second = library.new_window(Some("work"));
    let third = library.new_window(Some("personal")); // now frontmost

    library.apply_inactive_window_sidebar_hiding();
    assert!(!sidebar_visible(&library, first));
    assert!(!sidebar_visible(&library, second));
    assert!(sidebar_visible(&library, third));

    library.frontmost_window_id = Some(first);
    library.apply_inactive_window_sidebar_hiding();
    assert!(sidebar_visible(&library, first));
    assert!(!sidebar_visible(&library, second));
    assert!(!sidebar_visible(&library, third));
}

#[test]
fn apply_inactive_window_sidebar_hiding_single_window_keeps_sidebar() {
    let mut library = library();
    let only = first_window(&library);
    store_mut(&mut library, only).set_sidebar_visible(false); // hide first so the assert proves the force-show
    library.apply_inactive_window_sidebar_hiding();
    assert!(sidebar_visible(&library, only));
}

#[test]
fn apply_inactive_window_sidebar_hiding_reshows_manually_hidden_frontmost() {
    let mut library = library();
    library.new_window(Some("work"));
    let front = library.active_window_id().unwrap();
    store_mut(&mut library, front).set_sidebar_visible(false);
    library.apply_inactive_window_sidebar_hiding();
    assert!(sidebar_visible(&library, front));
}

#[test]
fn new_window_blank_name_falls_back_to_default() {
    let mut library = library();
    let id = library.new_window(Some("   "));
    assert_eq!(library.window_name(Some(id)), "window 2");
}

#[test]
fn new_window_strips_interior_control_characters_from_name() {
    let mut library = library();
    let id = library.new_window(Some("prod\ntouch /tmp/pwned"));
    assert_eq!(library.window_name(Some(id)), "prodtouch /tmp/pwned");
}

#[test]
fn rename_window_updates_name_and_ignores_blank() {
    let mut library = library();
    let id = first_window(&library);
    library.rename_window(id, "personal");
    assert_eq!(library.windows()[0].name, "personal");
    library.rename_window(id, "  ");
    assert_eq!(library.windows()[0].name, "personal");
}

#[test]
fn rename_window_strips_interior_control_characters() {
    let mut library = library();
    let id = first_window(&library);
    library.rename_window(id, "prod\ntouch /tmp/pwned");
    assert_eq!(library.windows()[0].name, "prodtouch /tmp/pwned");
}

/// The per-window file half waits for persistence.
#[test]
fn remove_window_drops_entry_store_and_file() {
    let mut library = library();
    let extra = library.new_window(Some("extra"));
    library.remove_window(extra);
    assert!(!library.windows().iter().any(|info| info.id == extra));
    assert!(library.store(extra).is_none());
}

#[test]
fn remove_window_keeps_at_least_one() {
    let mut library = library();
    assert!(!library.can_remove_window());
    let only = first_window(&library);
    library.remove_window(only);
    assert_eq!(library.windows().len(), 1);
    assert_eq!(library.windows()[0].id, only);
}

#[test]
fn remove_window_clears_frontmost_when_it_matches() {
    let mut library = library();
    let extra = library.new_window(Some("extra"));
    library.frontmost_window_id = Some(extra);
    library.remove_window(extra);
    assert_eq!(library.frontmost_window_id, None);
}

// MARK: - Open-set / frontmost / close

#[test]
fn close_window_marks_closed_but_keeps_entry() {
    let mut library = library();
    let extra = library.new_window(Some("extra"));
    assert!(library.is_open(extra));
    library.close_window(extra);
    assert!(!library.is_open(extra));
    assert!(library.windows().iter().any(|info| info.id == extra));
    assert_eq!(library.open_ids(), [first_window(&library)]);
}

#[test]
fn window_step_wraps_both_ways_in_library_order() {
    let mut library = library();
    let first = first_window(&library);
    let second = library.new_window(Some("second"));
    let third = library.new_window(Some("third"));
    assert!(library.can_step_windows());

    library.frontmost_window_id = Some(first);
    assert_eq!(library.navigate_window(WorkspaceNavigation::Next), Some(second));
    assert_eq!(library.navigate_window(WorkspaceNavigation::Previous), Some(third));
    library.frontmost_window_id = Some(third);
    assert_eq!(library.navigate_window(WorkspaceNavigation::Next), Some(first));
    assert_eq!(library.navigate_window(WorkspaceNavigation::Previous), Some(second));
}

#[test]
fn window_step_skips_closed_entries() {
    let mut library = library();
    let first = first_window(&library);
    let middle = library.new_window(Some("middle"));
    let last = library.new_window(Some("last"));
    library.close_window(middle);

    library.frontmost_window_id = Some(first);
    assert_eq!(library.navigate_window(WorkspaceNavigation::Next), Some(last));
    assert_eq!(library.navigate_window(WorkspaceNavigation::Previous), Some(last));
}

#[test]
fn window_step_is_nil_with_one_open_window() {
    let mut library = library();
    let extra = library.new_window(Some("extra"));
    library.close_window(extra);
    assert!(!library.can_step_windows());
    assert_eq!(library.navigate_window(WorkspaceNavigation::Next), None);
    assert_eq!(library.navigate_window(WorkspaceNavigation::Previous), None);
}

#[test]
fn window_step_starts_from_the_resolved_active_window_when_frontmost_is_closed() {
    // `active_window_id` falls back to the first OPEN window, so a step after the frontmost closed leaves from that
    // survivor rather than returning nothing.
    let mut library = library();
    let first = first_window(&library);
    let second = library.new_window(Some("second"));
    let third = library.new_window(Some("third"));
    library.frontmost_window_id = Some(third);
    library.close_window(third);

    assert_eq!(library.active_window_id(), Some(first));
    assert_eq!(library.navigate_window(WorkspaceNavigation::Next), Some(second));
}

#[test]
fn close_unknown_window_is_no_op() {
    let mut library = library();
    let before = library.open_ids();
    library.close_window(Uuid::new_v4());
    assert_eq!(library.open_ids(), before);
}

#[test]
fn close_window_is_no_op_while_terminating() {
    let mut library = library();
    let extra = library.new_window(Some("extra"));
    library.is_terminating = true;
    // during quit the open set must survive for the next launch's reopen-all.
    library.close_window(extra);
    assert!(library.is_open(extra));
}

/// The relaunch half waits for persistence.
#[test]
fn closing_every_window_keeps_the_last_one_as_frontmost_for_the_next_launch() {
    let mut library = library();
    let first = first_window(&library);
    let last = library.new_window(Some("extra"));
    library.close_window(first);
    library.close_window(last);
    assert_eq!(library.frontmost_window_id, Some(last));
    assert_eq!(library.active_window_id(), None);
}

#[test]
fn closing_the_frontmost_with_another_window_open_moves_frontmost_to_it() {
    let mut library = library();
    let first = first_window(&library);
    let last = library.new_window(Some("extra"));
    library.close_window(last);
    assert_eq!(library.frontmost_window_id, Some(first));
}

/// Upstream seeds this from an index on disk (the oldest window closed, the deleted one frontmost); the same state
/// is built in memory here, and the relaunch half waits for persistence.
#[test]
fn all_closed_exit_pins_frontmost_even_when_remove_window_cleared_it() {
    let mut library = library();
    let oldest = first_window(&library);
    let deleted = library.new_window(Some("gone"));
    let exit_window = library.new_window(Some("work"));
    library.close_window(oldest);
    library.set_frontmost(Some(deleted));

    library.remove_window(deleted);
    assert_eq!(library.frontmost_window_id, None);

    library.close_window(exit_window);
    assert_eq!(library.frontmost_window_id, Some(exit_window));
}

// MARK: - Cross-window session lookup

#[test]
fn store_for_session_finds_owning_open_window() {
    let mut library = library();
    let a = first_window(&library);
    let b = library.new_window(Some("b"));
    let session_a = seeded_session(&library, a);
    let session_b = seeded_session(&library, b);
    assert!(std::ptr::eq(
        library.store_for_session(session_a).unwrap(),
        store(&library, a)
    ));
    assert!(std::ptr::eq(
        library.store_for_session(session_b).unwrap(),
        store(&library, b)
    ));
    assert_eq!(library.window_id_for_session(session_a), Some(a));
    assert_eq!(library.window_id_for_session(session_b), Some(b));
}

#[test]
fn session_lookup_miss_for_unknown_and_closed_windows() {
    let mut library = library();
    let extra = library.new_window(Some("extra"));
    let session = seeded_session(&library, extra);
    assert!(std::ptr::eq(
        library.store_for_session(session).unwrap(),
        store(&library, extra)
    ));
    assert!(library.store_for_session(Uuid::new_v4()).is_none());
    assert_eq!(library.window_id_for_session(Uuid::new_v4()), None);
    library.close_window(extra);
    assert!(library.store_for_session(session).is_none());
    assert_eq!(library.window_id_for_session(session), None);
}

// MARK: - Active store resolution

#[test]
fn active_store_follows_frontmost() {
    let mut library = library();
    let a = first_window(&library);
    let b = library.new_window(Some("b"));
    library.frontmost_window_id = Some(a);
    assert!(std::ptr::eq(library.active_store().unwrap(), store(&library, a)));
    library.frontmost_window_id = Some(b);
    assert!(std::ptr::eq(library.active_store().unwrap(), store(&library, b)));
}

#[test]
fn active_store_falls_back_to_first_open_when_no_frontmost() {
    let mut library = library();
    let first = first_window(&library);
    library.new_window(Some("extra"));
    library.frontmost_window_id = None;
    assert!(std::ptr::eq(library.active_store().unwrap(), store(&library, first)));
}

#[test]
fn active_store_skips_closed_frontmost() {
    let mut library = library();
    let a = first_window(&library);
    let b = library.new_window(Some("b"));
    library.frontmost_window_id = Some(b);
    library.close_window(b);
    assert!(std::ptr::eq(library.active_store().unwrap(), store(&library, a)));
}

#[test]
fn active_window_id_follows_frontmost_then_falls_back() {
    let mut library = library();
    let a = first_window(&library);
    let b = library.new_window(Some("b"));
    library.frontmost_window_id = Some(b);
    assert_eq!(library.active_window_id(), Some(b));
    library.close_window(b);
    assert_eq!(library.active_window_id(), Some(a));
    library.frontmost_window_id = None;
    assert_eq!(library.active_window_id(), Some(a));
}

// MARK: - Reset per-session font sizes across all windows

/// Upstream's closed window half rewrites its file on disk and waits for persistence; the open window clears live.
#[test]
fn reset_session_font_sizes_all_windows_clears_closed_and_open() {
    let mut library = library();
    let open = first_window(&library);
    let session = add_to_first_workspace(&mut library, open);
    store_mut(&mut library, open).set_font_size(session, 22.0);
    assert!(library.is_open(open));

    library.reset_session_font_sizes_all_windows();

    assert_eq!(store(&library, open).session(session).unwrap().font_size, None);
}

// MARK: - open_counts

#[test]
fn open_counts_sums_open_windows_and_sessions() {
    let mut library = library();
    // the seeded window already has one workspace + one session.
    let first = first_window(&library);
    add_to_first_workspace(&mut library, first);
    library.new_window(Some("work"));
    assert_eq!(library.open_counts(), (2, 3));
}

#[test]
fn open_counts_excludes_closed_windows() {
    let mut library = library();
    let extra = library.new_window(Some("extra"));
    assert_eq!(library.open_counts().0, 2);
    library.close_window(extra);
    assert_eq!(library.open_counts(), (1, 1));
}
