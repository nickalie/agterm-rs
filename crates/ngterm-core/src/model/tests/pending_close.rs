//! Soft close, undo and finalization, ported from upstream's `AppStorePendingCloseTests.swift` and the pending-close
//! cases of `AppStoreTests.swift`, `AppStoreCloseReselectionTests.swift`, `AppStoreFocusTests.swift`,
//! `AppStoreOrganizationTests.swift`, `AppStoreCurrentWorkspaceTests.swift`, `AppStoreRestoreTests.swift` and
//! `AppStoreEventTests.swift`.
//!
//! Upstream checks that an undo brings back the same object with `===`; here the same object is the one still holding
//! the fake surface installed before the close, never torn down. Grace timers do not run here: the frontend arms them,
//! so a test finalizes the record itself. The ask, HUD and remote-row cases wait for those subsystems; the launch
//! pacer's have no counterpart, a lazy restore leaving it nothing to pace.

use std::collections::BTreeSet;

use ngterm_host::fakes::{FakeSurface, FakeSurfaceProbe};
use ngterm_proto::events::EventKind;
use uuid::Uuid;

use super::support::{add, add_background, ids};
use crate::model::{
    AppStore, NewSession, PENDING_CLOSE_GRACE, PendingCloseKind, PendingCloseTimer, SaveRequest, Session,
    WindowLibrary, Workspace, WorkspaceOptions,
};
use crate::persist::{SessionSnapshot, Snapshot, WorkspaceSnapshot};

/// Adds a selected session named `name` at `cwd`.
fn named(store: &mut AppStore, workspace: Uuid, cwd: &str, name: &str) -> Uuid {
    let spec = NewSession {
        name: Some(name.to_owned()),
        ..NewSession::at_cwd(cwd)
    };
    store.add_session(workspace, spec).expect("the workspace exists")
}

/// Puts a fake terminal in the session's primary slot and returns its probe.
fn with_surface(store: &mut AppStore, session: Uuid) -> FakeSurfaceProbe {
    let surface = FakeSurface::new(session.to_string());
    let probe = surface.probe();
    let slot = &mut store.session_mut(session).expect("the session exists").surface;
    assert!(slot.replace(Box::new(surface)).is_none());
    probe
}

/// Whether the session is in the tree with its surface still in place.
fn alive_in_tree(store: &AppStore, session: Uuid) -> bool {
    store
        .session(session)
        .is_some_and(|session| session.surface.is_present())
}

fn member_ids(store: &AppStore) -> Vec<Uuid> {
    store
        .pending_close_members()
        .iter()
        .map(|member| member.session.id())
        .collect()
}

fn workspace_ids(store: &AppStore) -> Vec<Uuid> {
    store.workspaces().iter().map(Workspace::id).collect()
}

fn marked(ids: &[Uuid]) -> BTreeSet<Uuid> {
    ids.iter().copied().collect()
}

fn latest(store: &AppStore) -> Uuid {
    store.pending_close_summary().expect("a pending close").id
}

/// A store of one "workspace 1" holding background sessions named `names`, as upstream's fixture builds it.
fn fixture(names: &[&str]) -> (AppStore, Uuid, Vec<Uuid>) {
    let mut store = AppStore::new();
    let ws = store.add_workspace("workspace 1");
    let sessions = names
        .iter()
        .map(|name| {
            let spec = NewSession {
                name: Some((*name).to_owned()),
                select: false,
                ..NewSession::at_cwd("/tmp")
            };
            store.add_session(ws, spec).expect("the workspace exists")
        })
        .collect();
    (store, ws, sessions)
}

// MARK: - AppStorePendingCloseTests

#[test]
fn a_soft_closed_session_stays_held_with_both_its_panes() {
    let (mut store, ws, sessions) = fixture(&["build"]);
    let split = Uuid::new_v4();
    let session = store.session_mut(sessions[0]).unwrap();
    session.split.exists = true;
    session.split_pane_identity = Some(split);

    assert!(store.soft_close_session(sessions[0]));
    assert!(store.workspace(ws).unwrap().sessions.is_empty());
    let members = store.pending_close_members();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].session.id(), sessions[0]);
    assert_eq!(members[0].workspace_id, ws);
    assert_eq!(members[0].workspace_name, "workspace 1");
    assert_eq!(members[0].session.split_pane_identity, Some(split));
}

#[test]
fn a_batch_soft_close_keeps_every_member() {
    let (mut store, _, sessions) = fixture(&["one", "two"]);
    assert!(store.soft_close_sessions(&sessions));
    assert_eq!(marked(&member_ids(&store)), marked(&sessions));
}

#[test]
fn finalizing_one_pending_session_keeps_its_batch_mates_and_other_records_undoable() {
    let (mut store, _, sessions) = fixture(&["one", "two", "three"]);
    let (first, second, third) = (sessions[0], sessions[1], sessions[2]);
    assert!(store.soft_close_sessions(&[first, second]));
    assert!(store.soft_close_session(third));

    assert!(store.finalize_pending_close_of_session(first));

    assert_eq!(marked(&member_ids(&store)), marked(&[second, third]));
    assert!(store.undo_pending_close(None));
    assert!(store.undo_pending_close(None));
    assert!(store.session(second).is_some() && store.session(third).is_some());
    assert!(store.session(first).is_none());
}

#[test]
fn undo_after_finalizing_an_earlier_batch_mate_restores_the_survivor_in_its_place() {
    let (mut store, ws, sessions) = fixture(&["one", "two", "three"]);
    let before: Vec<Uuid> = sessions[1..].to_vec();
    assert!(store.soft_close_sessions(&sessions[..2]));

    assert!(store.finalize_pending_close_of_session(sessions[0]));
    assert!(store.undo_pending_close(None));

    assert_eq!(ids(&store, ws), before);
}

#[test]
fn finalizing_the_only_pending_session_drops_its_record() {
    let (mut store, _, sessions) = fixture(&["one"]);
    assert!(store.soft_close_session(sessions[0]));

    assert!(store.finalize_pending_close_of_session(sessions[0]));

    assert!(store.pending_close_members().is_empty());
    assert!(!store.undo_pending_close(None));
    assert!(!store.finalize_pending_close_of_session(sessions[0]));
}

#[test]
fn finalizing_one_session_of_a_pending_workspace_keeps_the_rest() {
    let (mut store, ws, sessions) = fixture(&["one", "two"]);
    store.add_workspace("workspace 2");
    let probe = with_surface(&mut store, sessions[0]);
    assert!(store.soft_remove_workspace(ws));

    assert!(store.finalize_pending_close_of_session(sessions[0]));

    assert_eq!(probe.teardowns(), 1);
    assert_eq!(member_ids(&store), [sessions[1]]);
    assert!(store.undo_pending_close(None));
    assert!(store.session(sessions[1]).is_some());
    assert!(store.session(sessions[0]).is_none());
}

#[test]
fn a_soft_closed_workspace_contributes_every_session_it_held() {
    let (mut store, ws, sessions) = fixture(&["build"]);
    store.add_workspace("workspace 2");

    assert!(store.soft_remove_workspace(ws));
    let members = store.pending_close_members();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].session.id(), sessions[0]);
    assert_eq!(members[0].workspace_id, ws);
    assert_eq!(members[0].workspace_name, "workspace 1");
    assert!(store.pending_holds_workspace(ws));
}

#[test]
fn a_pending_session_is_found_only_until_undo_or_finalize() {
    for path in ["session", "batch", "workspace"] {
        let (mut store, ws, sessions) = fixture(&["one", "two"]);
        store.add_workspace("staying");
        let first = sessions[0];
        assert!(
            store.pending_close_session(first).is_none(),
            "a visible row is not pending"
        );

        match path {
            "session" => assert!(store.soft_close_session(first)),
            "batch" => assert!(store.soft_close_sessions(&sessions)),
            _ => assert!(store.soft_remove_workspace(ws)),
        }
        assert!(store.pending_close_session(first).is_some(), "{path}");
        assert!(store.pending_held_session_ids().contains(&first), "{path}");
        assert!(store.pending_close_session_mut(first).is_some(), "{path}");
        assert!(store.undo_pending_close(None));
        assert!(store.pending_close_session(first).is_none(), "{path}");

        assert!(store.soft_close_session(first));
        store.finalize_all_pending_closes();
        assert!(store.pending_close_session(first).is_none(), "{path}");
        assert!(store.pending_held_session_ids().is_empty(), "{path}");
    }
}

#[test]
fn finalizing_the_grace_drops_the_hold() {
    let (mut store, _, sessions) = fixture(&["build"]);
    assert!(store.soft_close_session(sessions[0]));
    store.finalize_all_pending_closes();
    assert!(store.pending_close_members().is_empty());
}

#[test]
fn undoing_the_close_returns_the_session_to_the_tree() {
    let (mut store, ws, sessions) = fixture(&["build"]);
    assert!(store.soft_close_session(sessions[0]));
    assert!(store.undo_pending_close(None));
    assert!(store.pending_close_members().is_empty());
    assert_eq!(ids(&store, ws), sessions);
}

// MARK: - AppStoreTests

#[test]
fn soft_close_hides_without_tearing_down_and_undo_restores_the_same_session() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let first = named(&mut store, ws, "/a", "alpha");
    let second = named(&mut store, ws, "/b", "beta");
    let probe = with_surface(&mut store, first);
    store.select_session(Some(first));

    assert!(store.soft_close_session(first));

    assert_eq!(ids(&store, ws), [second]);
    assert_eq!(store.selected_session_id(), Some(second));
    assert_eq!(probe.teardowns(), 0);
    let summary = store.pending_close_summary().unwrap();
    assert_eq!(summary.kind, PendingCloseKind::Session);
    assert_eq!(summary.title, "alpha");

    assert!(store.undo_pending_close(Some(summary.id)));

    assert_eq!(ids(&store, ws), [first, second]);
    assert_eq!(store.selected_session_id(), Some(first));
    assert!(alive_in_tree(&store, first));
    assert_eq!(probe.teardowns(), 0);
    assert_eq!(store.pending_close_summary(), None);
}

#[test]
fn a_one_target_batch_keeps_the_single_session_summary() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let first = named(&mut store, ws, "/a", "alpha");
    let second = named(&mut store, ws, "/b", "beta");

    assert!(store.soft_close_sessions(&[first]));

    let summary = store.pending_close_summary().unwrap();
    assert_eq!(
        (summary.kind, summary.title.as_str()),
        (PendingCloseKind::Session, "alpha")
    );
    assert!(store.undo_pending_close(Some(summary.id)));
    assert_eq!(ids(&store, ws), [first, second]);
    assert_eq!(store.selected_session_id(), Some(first));
}

#[test]
fn a_batch_soft_close_undoes_as_one_and_restores_every_session() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let first = named(&mut store, ws, "/a", "alpha");
    let second = named(&mut store, ws, "/b", "beta");
    let third = named(&mut store, ws, "/c", "gamma");
    let (first_probe, second_probe) = (with_surface(&mut store, first), with_surface(&mut store, second));
    store.select_session(Some(second));

    assert!(store.soft_close_sessions(&[first, second]));

    assert_eq!(ids(&store, ws), [third]);
    assert_eq!(store.selected_session_id(), Some(third));
    let summary = store.pending_close_summary().unwrap();
    assert_eq!(
        (summary.kind, summary.title.as_str()),
        (PendingCloseKind::Sessions, "2 sessions")
    );

    assert!(store.undo_pending_close(Some(summary.id)));

    assert_eq!(ids(&store, ws), [first, second, third]);
    assert_eq!(store.selected_session_id(), Some(second));
    assert!(alive_in_tree(&store, first) && alive_in_tree(&store, second));
    assert_eq!((first_probe.teardowns(), second_probe.teardowns()), (0, 0));
    assert_eq!(store.pending_close_summary(), None);
}

#[test]
fn a_batch_close_of_the_active_session_selects_the_nearest_survivor() {
    let mut store = AppStore::new();
    let one = store.add_workspace("one");
    let two = store.add_workspace("two");
    let distant = add(&mut store, one, "/a");
    let active = add(&mut store, two, "/b");
    let neighbor = add(&mut store, two, "/c");
    store.select_session(Some(active));

    assert!(store.soft_close_sessions(&[active, distant]));

    assert_eq!(store.selected_session_id(), Some(neighbor));
}

#[test]
fn a_batch_close_shifts_the_reselection_by_the_earlier_members_removed() {
    // the positional fallback runs only with an empty scoped recency, hence the restore
    let mut store = AppStore::new();
    let sessions: Vec<Uuid> = (0..4).map(|_| Uuid::new_v4()).collect();
    store.restore(
        &Snapshot {
            selected_session_id: Some(sessions[1]),
            workspaces: vec![WorkspaceSnapshot {
                id: Uuid::new_v4(),
                name: "work".to_owned(),
                sessions: sessions
                    .iter()
                    .enumerate()
                    .map(|(index, id)| SessionSnapshot::new(*id, format!("/{index}")))
                    .collect(),
                collapsed: None,
            }],
            ..Snapshot::default()
        },
        false,
    );

    assert!(store.soft_close_sessions(&sessions[..2]));

    assert_eq!(store.selected_session_id(), Some(sessions[2]));
}

#[test]
fn a_batch_close_emptying_the_active_workspace_falls_back_elsewhere() {
    let mut store = AppStore::new();
    let one = store.add_workspace("one");
    let two = store.add_workspace("two");
    let distant = add(&mut store, one, "/a");
    let active = add(&mut store, two, "/b");
    let sibling = add(&mut store, two, "/c");
    store.select_session(Some(active));

    assert!(store.soft_close_sessions(&[active, sibling]));

    assert_eq!(store.selected_session_id(), Some(distant));
}

#[test]
fn a_finalized_close_tears_every_session_down_and_cannot_be_undone() {
    for batch in [false, true] {
        let mut store = AppStore::new();
        let ws = store.add_workspace("work");
        let first = add(&mut store, ws, "/a");
        let second = add(&mut store, ws, "/b");
        let probes = [with_surface(&mut store, first), with_surface(&mut store, second)];

        if batch {
            assert!(store.soft_close_sessions(&[first, second]));
        } else {
            assert!(store.soft_close_session(first));
        }
        let summary = store.pending_close_summary().unwrap();
        store.finalize_pending_close(summary.id);

        assert_eq!(probes[0].teardowns(), 1);
        assert_eq!(probes[1].teardowns(), u32::from(batch));
        assert!(!store.undo_pending_close(Some(summary.id)));
        assert_eq!(store.pending_close_summary(), None);
        assert!(
            !store.session_recency().items().contains(&first),
            "a finalize leaves the recency"
        );
    }
}

#[test]
fn undoing_the_latest_close_promotes_the_previous_one() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let first = named(&mut store, ws, "/a", "alpha");
    let second = named(&mut store, ws, "/b", "beta");
    let third = named(&mut store, ws, "/c", "gamma");

    assert!(store.soft_close_session(first));
    assert_eq!(store.pending_close_summary().unwrap().title, "alpha");
    assert!(store.soft_close_session(second));
    assert_eq!(store.pending_close_summary().unwrap().title, "beta");

    assert!(store.undo_pending_close(None));
    assert_eq!(ids(&store, ws), [second, third]);
    assert_eq!(store.pending_close_summary().unwrap().title, "alpha");

    assert!(store.undo_pending_close(None));
    assert_eq!(ids(&store, ws), [first, second, third]);
    assert_eq!(store.pending_close_summary(), None);
}

#[test]
fn a_timer_firing_after_the_undo_is_a_no_op() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let session = add(&mut store, ws, "/a");
    let probe = with_surface(&mut store, session);

    assert!(store.soft_close_session(session));
    let record = latest(&store);
    assert!(store.undo_pending_close(Some(record)));
    store.finalize_pending_close(record);

    assert!(alive_in_tree(&store, session));
    assert_eq!(probe.teardowns(), 0);
    assert_eq!(store.pending_close_summary(), None);
}

#[test]
fn the_timer_finalizes_and_tears_down() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let session = add(&mut store, ws, "/a");
    let probe = with_surface(&mut store, session);

    assert!(store.soft_close_session(session));
    store.finalize_pending_close(latest(&store));

    assert!(store.session(session).is_none());
    assert_eq!(probe.teardowns(), 1);
    assert_eq!(store.pending_close_summary(), None);
}

#[test]
fn a_soft_removed_workspace_keeps_its_terminals_and_undo_restores_it() {
    let mut store = AppStore::new();
    let keep = store.add_workspace("keep");
    let doomed = store.add_workspace("doomed");
    let session = add(&mut store, doomed, "/a");
    let probe = with_surface(&mut store, session);
    add(&mut store, keep, "/b");

    assert!(store.soft_remove_workspace(doomed));

    assert_eq!(workspace_ids(&store), [keep]);
    assert_eq!(probe.teardowns(), 0);
    let summary = store.pending_close_summary().unwrap();
    assert_eq!(
        (summary.kind, summary.title.as_str()),
        (PendingCloseKind::Workspace, "doomed")
    );

    assert!(store.undo_pending_close(Some(summary.id)));

    assert_eq!(workspace_ids(&store), [keep, doomed]);
    assert!(alive_in_tree(&store, session));
    assert_eq!(store.selected_session_id(), Some(session));
    assert_eq!(probe.teardowns(), 0);
}

#[test]
fn the_last_workspace_cannot_be_soft_removed() {
    let mut store = AppStore::new();
    let only = store.add_workspace("only");
    assert!(!store.soft_remove_workspace(only));
    assert!(!store.soft_remove_workspace(Uuid::new_v4()));
    assert!(!store.soft_close_session(Uuid::new_v4()));
    assert!(!store.soft_close_sessions(&[Uuid::new_v4(), Uuid::new_v4()]));
    assert!(!store.soft_close_sessions(&[]));
    assert_eq!(workspace_ids(&store), [only]);
}

/// A "doomed" workspace of two sessions closed one session first and then as a whole, plus a "keep" workspace.
fn session_then_workspace_close() -> (AppStore, Uuid, [Uuid; 2], [Uuid; 2], [FakeSurfaceProbe; 2]) {
    let mut store = AppStore::new();
    let doomed = store.add_workspace("doomed");
    store.add_workspace("keep");
    let first = add(&mut store, doomed, "/a");
    let second = add(&mut store, doomed, "/b");
    let probes = [with_surface(&mut store, first), with_surface(&mut store, second)];
    assert!(store.soft_close_session(first));
    let session_close = latest(&store);
    assert!(store.soft_remove_workspace(doomed));
    let workspace_close = latest(&store);
    (store, doomed, [first, second], [session_close, workspace_close], probes)
}

#[test]
fn undoing_the_session_close_then_the_workspace_close_keeps_one_workspace() {
    let (mut store, doomed, [first, second], [session_close, workspace_close], probes) = session_then_workspace_close();

    // the session undo rebuilds the workspace as a shell; the workspace undo merges into it
    assert!(store.undo_pending_close(Some(session_close)));
    assert!(store.undo_pending_close(Some(workspace_close)));

    assert_eq!(workspace_ids(&store).iter().filter(|id| **id == doomed).count(), 1);
    assert_eq!(ids(&store, doomed), [first, second]);
    assert!(alive_in_tree(&store, first) && alive_in_tree(&store, second));
    assert_eq!(store.selected_session_id(), Some(second));
    assert_eq!((probes[0].teardowns(), probes[1].teardowns()), (0, 0));
}

#[test]
fn undoing_the_workspace_close_then_the_session_close_keeps_one_workspace() {
    let (mut store, doomed, [first, second], [session_close, workspace_close], _) = session_then_workspace_close();

    assert!(store.undo_pending_close(Some(workspace_close)));
    assert!(store.undo_pending_close(Some(session_close)));

    assert_eq!(workspace_ids(&store).iter().filter(|id| **id == doomed).count(), 1);
    assert_eq!(ids(&store, doomed), [first, second]);
}

#[test]
fn a_rebuilt_shell_takes_its_name_and_expansion_from_the_pending_workspace_close() {
    let mut store = AppStore::new();
    let doomed = store.add_workspace("old");
    store.add_workspace("keep");
    let first = add(&mut store, doomed, "/a");
    add(&mut store, doomed, "/b");

    assert!(store.soft_close_session(first));
    let session_close = latest(&store);
    store.rename_workspace(doomed, "renamed");
    store.set_workspace_expanded(doomed, false);
    assert!(store.soft_remove_workspace(doomed));
    let workspace_close = latest(&store);

    assert!(store.undo_pending_close(Some(session_close)));
    let shell = store.workspace(doomed).unwrap();
    assert_eq!((shell.name.as_str(), shell.is_expanded), ("renamed", false));

    assert!(store.undo_pending_close(Some(workspace_close)));
    let restored = store.workspace(doomed).unwrap();
    assert_eq!((restored.name.as_str(), restored.is_expanded), ("renamed", false));
}

#[test]
fn a_workspace_undo_keeps_the_edits_made_to_the_rebuilt_shell() {
    let (mut store, doomed, _, [session_close, workspace_close], _) = session_then_workspace_close();

    assert!(store.undo_pending_close(Some(session_close)));
    store.rename_workspace(doomed, "new");
    store.set_workspace_expanded(doomed, false);
    assert!(store.undo_pending_close(Some(workspace_close)));

    let restored = store.workspace(doomed).unwrap();
    assert_eq!((restored.name.as_str(), restored.is_expanded), ("new", false));
}

#[test]
fn a_workspace_undo_merges_disjoint_sessions_into_the_rebuilt_shell() {
    let mut store = AppStore::new();
    let doomed = store.add_workspace("doomed");
    store.add_workspace("keep");
    let first = add(&mut store, doomed, "/a");
    let second = add(&mut store, doomed, "/b");

    // close the second, so the workspace record carries only the first
    assert!(store.soft_close_session(second));
    let session_close = latest(&store);
    assert!(store.soft_remove_workspace(doomed));
    let workspace_close = latest(&store);

    assert!(store.undo_pending_close(Some(session_close)));
    assert!(store.undo_pending_close(Some(workspace_close)));

    // the shell keeps its slot, so the merged order trails the shell's session
    assert_eq!(ids(&store, doomed), [second, first]);
}

// MARK: - AppStoreCloseReselectionTests

#[test]
fn a_soft_close_of_the_active_session_picks_the_recent_survivor_like_a_hard_close() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let first = add(&mut store, ws, "/a");
    add(&mut store, ws, "/b");
    add(&mut store, ws, "/c");
    let closing = add(&mut store, ws, "/d");
    store.select_session(Some(first));
    store.select_session(Some(closing));

    assert!(store.soft_close_session(closing));
    assert_eq!(store.selected_session_id(), Some(first));
}

#[test]
fn a_soft_close_with_an_empty_scoped_recency_falls_back_to_the_positional_target() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let sessions: Vec<Uuid> = ["/0", "/1", "/2"]
        .iter()
        .map(|cwd| add_background(&mut store, ws, cwd))
        .collect();
    store.select_session(Some(sessions[1]));
    assert_eq!(store.session_recency().items(), [sessions[1]]);

    assert!(store.soft_close_session(sessions[1]));
    assert_eq!(store.selected_session_id(), Some(sessions[2]));
}

#[test]
fn a_batch_close_never_picks_a_member_of_the_closing_group() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let oldest = add(&mut store, ws, "/a");
    let survivor = add(&mut store, ws, "/b");
    let also_closing = add(&mut store, ws, "/c");
    let closing = add(&mut store, ws, "/d");
    for id in [oldest, survivor, also_closing, closing] {
        store.select_session(Some(id));
    }

    assert!(store.soft_close_sessions(&[also_closing, closing]));
    assert_eq!(store.selected_session_id(), Some(survivor));
    assert!(store.session_recency().items().contains(&closing), "kept for the undo");
    assert!(store.session_recency().items().contains(&also_closing));
}

#[test]
fn an_undo_selects_the_closed_session_again() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let other = add(&mut store, ws, "/a");
    let closing = add(&mut store, ws, "/b");
    store.select_session(Some(other));
    store.select_session(Some(closing));

    assert!(store.soft_close_session(closing));
    assert_eq!(store.selected_session_id(), Some(other));
    assert!(store.undo_pending_close(Some(latest(&store))));
    assert_eq!(store.selected_session_id(), Some(closing));
}

#[test]
fn the_grace_expiry_leaves_the_selection_alone() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let survivor = add(&mut store, ws, "/a");
    let closing = add(&mut store, ws, "/b");
    let probe = with_surface(&mut store, closing);
    store.select_session(Some(survivor));
    store.select_session(Some(closing));

    assert!(store.soft_close_session(closing));
    assert_eq!(store.selected_session_id(), Some(survivor));
    store.finalize_pending_close(latest(&store));

    assert_eq!(probe.teardowns(), 1);
    assert_eq!(store.selected_session_id(), Some(survivor));
    assert!(!store.session_recency().items().contains(&closing));
}

// MARK: - Focus set (AppStoreFocusTests, AppStoreOrganizationTests)

#[test]
fn undoing_a_soft_removed_member_marks_it_again() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let doomed = store.add_workspace("doomed");
    add(&mut store, work, "/a");
    store.set_focus_membership(work, true);
    store.set_focus_membership(doomed, true);
    store.set_focus_enabled(true);

    assert!(store.soft_remove_workspace(doomed));
    assert_eq!(store.focus().ids(), &marked(&[work]));
    assert!(store.focus().enabled());

    assert!(store.undo_pending_close(None));
    assert_eq!(
        store.focus().ids(),
        &marked(&[work, doomed]),
        "an undone member is marked again"
    );
    assert!(store.focus().enabled());
    let visible: Vec<Uuid> = store.visible_workspaces().iter().map(|ws| ws.id()).collect();
    assert_eq!(visible, [work, doomed]);
}

#[test]
fn undoing_the_last_member_marks_it_without_switching_the_filter_on() {
    let mut store = AppStore::new();
    let other = store.add_workspace("other");
    let doomed = store.add_workspace("doomed");
    let spare = store.add_workspace("spare");
    store.set_focus_membership(doomed, true);
    store.set_focus_enabled(true);

    assert!(store.soft_remove_workspace(doomed));
    assert!(store.focus().ids().is_empty() && !store.focus().enabled());

    assert!(store.undo_pending_close(None));
    assert_eq!(store.focus().ids(), &marked(&[doomed]));
    assert!(
        !store.focus().enabled(),
        "restoring a member never switches the filter on"
    );
    assert_eq!(workspace_ids(&store), [other, doomed, spare]);
    store.set_focus_enabled(true);
    let visible: Vec<Uuid> = store.visible_workspaces().iter().map(|ws| ws.id()).collect();
    assert_eq!(visible, [doomed]);
}

#[test]
fn switching_the_filter_off_during_the_grace_survives_the_undo() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let doomed = store.add_workspace("doomed");
    store.add_workspace("spare");
    store.set_focus_membership(work, true);
    store.set_focus_membership(doomed, true);
    store.set_focus_enabled(true);

    assert!(store.soft_remove_workspace(doomed));
    store.set_focus_enabled(false);

    assert!(store.undo_pending_close(None));
    assert_eq!(store.focus().ids(), &marked(&[work, doomed]));
    assert!(!store.focus().enabled(), "the undo keeps the toggle the user just made");
}

#[test]
fn re_closing_a_workspace_rebuilt_by_a_session_undo_keeps_its_membership() {
    let mut store = AppStore::new();
    let keep = store.add_workspace("keep");
    let work = store.add_workspace("work");
    add(&mut store, keep, "/a");
    let stray = add_background(&mut store, work, "/b");
    store.set_focus_membership(keep, true);
    store.set_focus_membership(work, true);
    store.set_focus_enabled(true);

    assert!(store.soft_close_session(stray));
    let session_close = latest(&store);
    assert!(store.soft_remove_workspace(work));
    assert_eq!(store.focus().ids(), &marked(&[keep]));

    assert!(store.undo_pending_close(Some(session_close)));
    assert!(store.workspace(work).is_some());
    assert!(!store.focus().ids().contains(&work) && !store.focus().enabled());

    assert!(store.soft_remove_workspace(work));
    let item = store
        .recent_closed_items()
        .into_iter()
        .find(|item| item.workspace.as_ref().is_some_and(|recent| recent.snapshot.id == work))
        .unwrap();
    assert_eq!(
        item.workspace.unwrap().focus_member,
        Some(true),
        "the overwriting entry keeps it"
    );

    assert!(store.undo_pending_close(None));
    assert_eq!(
        store.focus().ids(),
        &marked(&[keep, work]),
        "the absorbed record's membership survives"
    );
}

#[test]
fn undoing_a_non_member_does_not_mark_it() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let doomed = store.add_workspace("doomed");
    store.add_workspace("spare");
    store.set_focus_membership(work, true);
    store.set_focus_enabled(true);

    assert!(store.soft_remove_workspace(doomed));
    assert!(store.undo_pending_close(None));
    assert_eq!(store.focus().ids(), &marked(&[work]));
    assert!(store.focus().enabled());
}

#[test]
fn soft_removing_members_prunes_them_and_switches_off_with_the_last() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let doomed = store.add_workspace("doomed");
    store.add_workspace("spare");
    store.set_focus_membership(work, true);
    store.set_focus_membership(doomed, true);
    store.set_focus_enabled(true);
    assert!(store.soft_remove_workspace(doomed));
    assert_eq!(store.focus().ids(), &marked(&[work]));
    assert!(store.focus().enabled());
    assert!(store.soft_remove_workspace(work));
    assert!(store.focus().ids().is_empty() && !store.focus().enabled());
}

#[test]
fn soft_removing_a_marked_workspace_reselects_inside_the_marked_set() {
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

    assert!(store.soft_remove_workspace(doomed));

    assert_eq!(store.selected_session_id(), Some(survivor));
    assert_eq!(store.focus().ids(), &marked(&[also_marked]));
    assert!(store.focus().enabled());
}

// MARK: - Targeting (AppStoreCurrentWorkspaceTests)

#[test]
fn undoing_a_fresh_workspace_close_does_not_revive_it_as_current() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let session = add(&mut store, work, "/a");
    store.select_session(Some(session));
    let fresh = store.add_workspace_with("fresh", WorkspaceOptions::default());
    assert_eq!(store.current_workspace_id(), Some(fresh));

    assert!(store.soft_remove_workspace(fresh));
    assert!(store.undo_pending_close(None));
    assert_eq!(workspace_ids(&store), [work, fresh]);
    assert_eq!(store.current_workspace_id(), Some(work));
}

// MARK: - Launch restore slots (AppStoreRestoreTests)

fn arm(session: &mut Session) {
    session.restore.command = Some("claude --resume abc".to_owned());
    session.restore.pending_command = Some("claude --resume abc".to_owned());
    session.restore.pending_foreground = Some(vec!["vim".to_owned()]);
    session.split_restore.command = Some("tail -f /var/log/x".to_owned());
    session.split_restore.pending_command = Some("tail -f /var/log/x".to_owned());
}

fn disarmed(session: &Session) -> bool {
    session.restore.pending_command.is_none()
        && session.restore.pending_foreground.is_none()
        && session.split_restore.pending_command.is_none()
        && session.restore.command.as_deref() == Some("claude --resume abc")
        && session.split_restore.command.as_deref() == Some("tail -f /var/log/x")
}

#[test]
fn an_undone_close_comes_back_with_nothing_armed_but_its_overrides() {
    for path in ["session", "batch", "workspace"] {
        let mut store = AppStore::new();
        let doomed = store.add_workspace("doomed");
        store.add_workspace("keep");
        let first = add(&mut store, doomed, "/a");
        let second = add(&mut store, doomed, "/b");
        for id in [first, second] {
            arm(store.session_mut(id).unwrap());
        }

        match path {
            "session" => assert!(store.soft_close_session(first)),
            "batch" => assert!(store.soft_close_sessions(&[first, second])),
            _ => assert!(store.soft_remove_workspace(doomed)),
        }
        assert!(store.undo_pending_close(Some(latest(&store))));

        assert!(disarmed(store.session(first).unwrap()), "{path}");
        if path != "session" {
            assert!(disarmed(store.session(second).unwrap()), "{path}");
        }
    }
}

// MARK: - Events (AppStoreEventTests)

fn kinds(store: &mut AppStore, wanted: &[EventKind]) -> Vec<(EventKind, Option<Uuid>)> {
    store
        .take_outbox()
        .events
        .into_iter()
        .filter(|event| wanted.contains(&event.kind))
        .map(|event| (event.kind, event.session))
        .collect()
}

#[test]
fn soft_close_undo_and_finalize_emit_only_the_visible_membership_edges() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let session = named(&mut store, ws, "/tmp", "api");
    store.take_outbox();
    let edges = [EventKind::SessionCreated, EventKind::SessionClosed];

    assert!(store.soft_close_session(session));
    assert!(store.undo_pending_close(None));
    assert!(store.soft_close_session(session));
    store.finalize_all_pending_closes();

    assert_eq!(
        kinds(&mut store, &edges),
        [
            (EventKind::SessionClosed, Some(session)),
            (EventKind::SessionCreated, Some(session)),
            (EventKind::SessionClosed, Some(session)),
        ]
    );
}

#[test]
fn folding_a_pending_workspace_close_does_not_repeat_the_hidden_sessions_edges() {
    let mut store = AppStore::new();
    let doomed = store.add_workspace("doomed");
    store.add_workspace("keep");
    let first = named(&mut store, doomed, "/one", "one");
    let second = named(&mut store, doomed, "/two", "two");
    store.take_outbox();

    assert!(store.soft_close_session(first));
    let session_close = latest(&store);
    assert!(store.soft_remove_workspace(doomed));
    assert!(store.undo_pending_close(Some(session_close)));
    assert!(store.soft_remove_workspace(doomed));

    assert_eq!(
        kinds(&mut store, &[EventKind::SessionCreated, EventKind::SessionClosed]),
        [
            (EventKind::SessionClosed, Some(first)),
            (EventKind::SessionClosed, Some(second)),
            (EventKind::SessionCreated, Some(first)),
            (EventKind::SessionClosed, Some(first)),
        ]
    );
    assert_eq!(store.pending_close_members().len(), 2, "one record holds both");
    assert!(store.undo_pending_close(None));
    assert_eq!(ids(&store, doomed), [first, second]);
}

// MARK: - Side effects

#[test]
fn a_soft_close_writes_nothing_and_drops_the_armed_debounced_write() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let first = add(&mut store, ws, "/a");
    let second = add(&mut store, ws, "/b");
    store.take_outbox();
    store.select_session(Some(first));
    assert_eq!(store.outbox().save, SaveRequest::Debounced);

    assert!(store.soft_close_session(second));

    let outbox = store.take_outbox();
    assert_eq!(outbox.save, SaveRequest::None);
    assert!(outbox.cancel_debounced_save);
    assert_eq!(outbox.pending_closes.len(), 1);
    assert_eq!(outbox.pending_closes[0].1, PENDING_CLOSE_GRACE);
    assert_eq!(
        Some(outbox.pending_closes[0].0),
        store.pending_close_summary().map(|summary| summary.id)
    );

    store.finalize_pending_close(outbox.pending_closes[0].0);
    let outbox = store.take_outbox();
    assert_eq!(outbox.save, SaveRequest::Now, "a finalize writes");
    assert!(outbox.events.is_empty(), "a finalize announces nothing");

    assert!(store.soft_close_session(first));
    store.take_outbox();
    assert!(store.undo_pending_close(None));
    assert_eq!(store.take_outbox().save, SaveRequest::Now, "an undo writes");
}

#[test]
fn a_soft_close_reaches_the_frontend_as_a_grace_timer() {
    let mut library = WindowLibrary::seeded("/home/tester");
    let window = library.active_window_id().unwrap();
    let session = library.active_store().unwrap().active_session().unwrap().id();
    library.drain_effects();

    assert!(library.store_mut(window).unwrap().soft_close_session(session));
    let effects = library.drain_effects();

    let record = library.store(window).unwrap().pending_close_summary().unwrap().id;
    assert_eq!(
        effects.pending_closes,
        [PendingCloseTimer {
            window,
            record,
            grace: PENDING_CLOSE_GRACE
        }]
    );
    assert_eq!(effects.cancelled_saves, [window]);
    assert!(effects.window_saves.is_empty());
    assert!(
        effects
            .events
            .iter()
            .any(|event| event.event.kind == EventKind::SessionClosed)
    );
    assert!(library.drain_effects().pending_closes.is_empty(), "drained once");
}

#[test]
fn closing_a_window_finalizes_its_pending_closes() {
    let mut library = WindowLibrary::seeded("/home/tester");
    let first = library.active_window_id().unwrap();
    let second = library.new_window(None);
    let store = library.store_mut(second).unwrap();
    let session = store.active_session().unwrap().id();
    let probe = with_surface(store, session);
    assert!(store.soft_close_session(session));

    library.close_window(second);

    assert_eq!(probe.teardowns(), 1);
    assert!(library.is_open(first) && !library.is_open(second));
    assert_eq!(library.window_holding_session(session), None);
}

#[test]
fn the_library_finds_a_session_while_its_close_can_be_undone() {
    let mut library = WindowLibrary::seeded("/home/tester");
    let window = library.active_window_id().unwrap();
    let session = library.active_store().unwrap().active_session().unwrap().id();
    assert_eq!(library.window_holding_session(session), Some(window));

    assert!(library.store_mut(window).unwrap().soft_close_session(session));
    assert_eq!(library.window_id_for_session(session), None);
    assert_eq!(library.window_holding_session(session), Some(window));

    library.finalize_all_pending_closes();
    assert_eq!(library.window_holding_session(session), None);
}
