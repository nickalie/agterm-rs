//! The model ↔ snapshot boundary, ported from upstream's `SnapshotRoundTripTests`, `PersistenceTests` (the store-level
//! cases) and `AppStoreRestoreSeedTests`.

use uuid::Uuid;

use super::support::{add, add_background};
use crate::model::{
    AppStore, NewSession, SIDEBAR_WIDTH_DEFAULT, SIDEBAR_WIDTH_MAX, SIDEBAR_WIDTH_MIN, SaveRequest, Session,
    SidebarMode, SplitAxis,
};
use crate::persist::{SessionSnapshot, Snapshot, WorkspaceSnapshot};

fn store_with(cwd: &str) -> (AppStore, Uuid, Uuid) {
    let mut store = AppStore::new();
    let workspace = store.add_workspace("work");
    let session = add(&mut store, workspace, cwd);
    (store, workspace, session)
}

fn restored(snapshot: &Snapshot, launch_restore: bool) -> AppStore {
    let mut store = AppStore::new();
    store.restore(snapshot, launch_restore);
    store
}

fn one_workspace(sessions: Vec<SessionSnapshot>) -> Snapshot {
    Snapshot {
        workspaces: vec![WorkspaceSnapshot {
            id: Uuid::new_v4(),
            name: "work".to_owned(),
            sessions,
            collapsed: None,
        }],
        ..Snapshot::default()
    }
}

fn first(store: &AppStore) -> &Session {
    &store.workspaces()[0].sessions[0]
}

#[test]
fn a_snapshot_captures_the_tree_and_live_cwds() {
    let (mut store, workspace, session) = store_with("/start");
    store
        .session_mut(session)
        .unwrap()
        .set_current_cwd("/Users/user/dev/live");
    store.rename_session(session, "build");
    let other = add(&mut store, workspace, "/tmp");
    let snapshot = store.snapshot();
    assert_eq!(snapshot.selected_session_id, Some(other));
    let persisted = &snapshot.workspaces[0];
    assert_eq!((persisted.id, persisted.name.as_str()), (workspace, "work"));
    let ids: Vec<Uuid> = persisted.sessions.iter().map(|s| s.id).collect();
    assert_eq!(ids, [session, other]);
    assert_eq!(persisted.sessions[0].custom_name.as_deref(), Some("build"));
    assert_eq!(persisted.sessions[0].cwd, "/Users/user/dev/live");
    assert_eq!(persisted.sessions[1].cwd, "/tmp");
}

#[test]
fn restore_rebuilds_names_and_cwds_and_leaves_terminals_lazy() {
    let selected = Uuid::new_v4();
    let snapshot = Snapshot {
        selected_session_id: Some(selected),
        workspaces: vec![
            WorkspaceSnapshot {
                id: Uuid::new_v4(),
                name: "work".to_owned(),
                sessions: vec![
                    SessionSnapshot {
                        custom_name: Some("build".to_owned()),
                        ..SessionSnapshot::new(selected, "/Users/user/dev/foo")
                    },
                    SessionSnapshot::new(Uuid::new_v4(), "/var/log"),
                ],
                collapsed: None,
            },
            WorkspaceSnapshot {
                id: Uuid::new_v4(),
                name: "personal".to_owned(),
                sessions: vec![SessionSnapshot::new(Uuid::new_v4(), "/")],
                collapsed: None,
            },
        ],
        ..Snapshot::default()
    };
    let store = restored(&snapshot, false);
    assert_eq!(store.selected_session_id(), Some(selected));
    let names: Vec<&str> = store.workspaces().iter().map(|w| w.name.as_str()).collect();
    assert_eq!(names, ["work", "personal"]);
    let sessions = &store.workspaces()[0].sessions;
    assert_eq!(sessions[0].display_name(), "build");
    assert_eq!(sessions[0].initial_cwd(), "/Users/user/dev/foo");
    assert_eq!(sessions[1].display_name(), "log");
    assert_eq!(store.workspaces()[1].sessions[0].display_name(), "/");
    assert!(!sessions[0].surface.is_present());
    assert_eq!(
        (sessions[0].current_cwd.as_ref(), sessions[1].current_cwd.as_ref()),
        (None, None)
    );
    assert!(sessions.iter().all(|session| session.was_restored));
}

#[test]
fn restore_emits_nothing_and_writes_nothing() {
    let selected = Uuid::new_v4();
    let snapshot = Snapshot {
        selected_session_id: Some(selected),
        ..one_workspace(vec![SessionSnapshot::new(selected, "/a")])
    };
    let store = restored(&snapshot, false);
    assert!(store.outbox().is_empty(), "{:?}", store.outbox());
}

#[test]
fn a_round_trip_preserves_the_tree() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let personal = store.add_workspace("personal");
    add(&mut store, work, "/a");
    let b = add(&mut store, personal, "/b");
    store.rename_session(b, "server");
    store.select_session(Some(b));
    let snapshot = store.snapshot();
    assert_eq!(restored(&snapshot, false).snapshot(), snapshot);
}

#[test]
fn restore_clears_a_dangling_selection() {
    let snapshot = Snapshot {
        selected_session_id: Some(Uuid::new_v4()),
        ..one_workspace(vec![SessionSnapshot::new(Uuid::new_v4(), "/a")])
    };
    let store = restored(&snapshot, false);
    assert_eq!(store.selected_session_id(), None);
    assert!(store.active_session().is_none());
}

#[test]
fn restore_folds_duplicate_workspaces_and_keeps_a_repeated_session_once() {
    let workspace = Uuid::new_v4();
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let twin = |sessions| WorkspaceSnapshot {
        id: workspace,
        name: "work".to_owned(),
        sessions,
        collapsed: None,
    };
    let snapshot = Snapshot {
        workspaces: vec![
            twin(vec![SessionSnapshot::new(a, "/a")]),
            twin(vec![SessionSnapshot::new(a, "/dup"), SessionSnapshot::new(b, "/b")]),
        ],
        ..Snapshot::default()
    };
    let store = restored(&snapshot, false);
    assert_eq!(store.workspaces().len(), 1);
    let cwds: Vec<&str> = store.workspaces()[0]
        .sessions
        .iter()
        .map(Session::initial_cwd)
        .collect();
    assert_eq!(cwds, ["/a", "/b"]);
}

#[test]
fn remote_sessions_never_reach_the_snapshot() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let first = add(&mut store, work, "/a");
    let second = add(&mut store, work, "/b");
    let remote_spec = NewSession {
        remote_host: Some("buildbox".to_owned()),
        ..NewSession::at_cwd("/c")
    };
    let remote = store.add_session(work, remote_spec).unwrap();
    store.select_session(Some(first));
    store.select_session(Some(second));
    store.select_session(Some(remote));
    let snapshot = store.snapshot();
    let ids: Vec<Uuid> = snapshot.workspaces[0].sessions.iter().map(|s| s.id).collect();
    assert_eq!(ids, [first, second]);
    assert_eq!(
        snapshot.selected_session_id,
        Some(second),
        "the most recent local one stands in"
    );
    assert!(!snapshot.session_recency.unwrap().contains(&remote));
    assert!(
        restored(&store.snapshot(), false)
            .sessions()
            .all(|s| s.remote_host().is_none())
    );

    let mut only_remote = AppStore::new();
    let work = only_remote.add_workspace("remote only");
    let spec = NewSession {
        remote_host: Some("buildbox".to_owned()),
        ..NewSession::at_cwd("/a")
    };
    only_remote.add_session(work, spec);
    let snapshot = only_remote.snapshot();
    assert_eq!(snapshot.selected_session_id, None);
    assert!(
        snapshot.workspaces[0].sessions.is_empty(),
        "the local workspace survives empty"
    );
}

#[test]
fn pane_identities_round_trip_and_legacy_ones_are_minted() {
    let (mut store, _, session) = store_with("/a");
    let split = Uuid::new_v4();
    {
        let session = store.session_mut(session).unwrap();
        session.split.shown = true;
        session.split.exists = true;
        session.split_pane_identity = Some(split);
    }
    let primary = first(&store).pane_identity;
    let back = restored(&store.snapshot(), false);
    assert_eq!(
        (first(&back).pane_identity, first(&back).split_pane_identity),
        (primary, Some(split))
    );

    let legacy = SessionSnapshot {
        is_split: Some(true),
        ..SessionSnapshot::new(Uuid::new_v4(), "/tmp")
    };
    let minted = restored(&one_workspace(vec![legacy]), false);
    assert!(first(&minted).split_pane_identity.is_some());
    assert!(first(&minted).split.exists && first(&minted).split.shown);
    assert_eq!(first(&minted).split.axis, SplitAxis::LeftRight);
}

#[test]
fn split_state_round_trips_with_each_panes_cwd() {
    let (mut store, _, session) = store_with("/a");
    {
        let session = store.session_mut(session).unwrap();
        session.split.shown = true;
        session.split.exists = true;
        session.split.axis = SplitAxis::TopBottom;
        session.split.ratio = Some(0.63);
        session.set_current_cwd("/a/primary");
        session.split_cwd = Some("/var/log".to_owned());
    }
    let snapshot = store.snapshot();
    let persisted = &snapshot.workspaces[0].sessions[0];
    assert_eq!(persisted.cwd, "/a/primary");
    assert_eq!(persisted.split_cwd.as_deref(), Some("/var/log"));
    assert_eq!(persisted.split_ratio, Some(0.63));
    let back = restored(&snapshot, false);
    let session = first(&back);
    assert_eq!(session.initial_cwd(), "/a/primary");
    assert_eq!(session.initial_split_cwd.as_deref(), Some("/var/log"));
    assert_eq!(session.split.axis, SplitAxis::TopBottom);
    assert_eq!(session.split.ratio, Some(0.63));
}

#[test]
fn a_hidden_split_keeps_identity_and_axis_but_focus_returns_to_the_primary() {
    let (mut store, _, session) = store_with("/a");
    let split = Uuid::new_v4();
    {
        let session = store.session_mut(session).unwrap();
        session.split.exists = true;
        session.split.focused = true;
        session.split.axis = SplitAxis::TopBottom;
        session.split_pane_identity = Some(split);
    }
    let snapshot = store.snapshot();
    let persisted = &snapshot.workspaces[0].sessions[0];
    assert_eq!(persisted.has_split, Some(true));
    assert_eq!(persisted.is_split, Some(false));
    assert_eq!(persisted.split_pane_identity, Some(split));
    let back = restored(&snapshot, false);
    let session = first(&back);
    assert!(session.split.exists && !session.split.shown && !session.split.focused);
    assert_eq!(session.split_pane_identity, Some(split));
    assert_eq!(session.split.axis, SplitAxis::TopBottom);
}

#[test]
fn creation_commands_round_trip_and_an_unset_hold_writes_nothing() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let spec = NewSession {
        command: Some("make test".to_owned()),
        wait: true,
        ..NewSession::at_cwd("/a")
    };
    let held = store.add_session(work, spec).unwrap();
    let spec = NewSession {
        command: Some("make lint".to_owned()),
        ..NewSession::at_cwd("/b")
    };
    let plain = store.add_session(work, spec).unwrap();
    {
        let session = store.session_mut(held).unwrap();
        session.split.exists = true;
        session.split_launch.command = Some("ssh split-host".to_owned());
        session.split_launch.wait = true;
    }
    let snapshot = store.snapshot();
    let sessions = &snapshot.workspaces[0].sessions;
    assert_eq!(sessions[0].command_wait, Some(true));
    assert_eq!(sessions[1].command_wait, None, "false is omitted");
    let back = restored(&snapshot, false);
    let held = back.session(held).unwrap();
    assert_eq!(held.launch.command.as_deref(), Some("make test"));
    assert!(held.launch.wait);
    assert_eq!(held.split_launch.command.as_deref(), Some("ssh split-host"));
    assert!(held.split_launch.wait);
    assert!(!back.session(plain).unwrap().launch.wait);
}

#[test]
fn the_sidebar_round_trips_defaults_when_absent_and_clamps() {
    let (mut store, _, _) = store_with("/a");
    store.set_sidebar_width(312.0);
    store.set_sidebar_visible(false);
    store.set_sidebar_mode(SidebarMode::Flagged);
    let back = restored(&store.snapshot(), false);
    assert!((back.sidebar().width - 312.0).abs() < f64::EPSILON);
    assert!(!back.sidebar().visible);
    assert_eq!(back.sidebar().mode, SidebarMode::Flagged);

    let defaults = restored(&Snapshot::default(), false);
    assert!((defaults.sidebar().width - SIDEBAR_WIDTH_DEFAULT).abs() < f64::EPSILON);
    assert!(defaults.sidebar().visible);
    assert_eq!(defaults.sidebar().mode, SidebarMode::Tree);
    for (width, clamped) in [(2000.0, SIDEBAR_WIDTH_MAX), (10.0, SIDEBAR_WIDTH_MIN)] {
        let snapshot = Snapshot {
            sidebar_width: Some(width),
            ..Snapshot::default()
        };
        assert!((restored(&snapshot, false).sidebar().width - clamped).abs() < f64::EPSILON);
    }
}

#[test]
fn restore_clamps_an_out_of_range_split_ratio() {
    let session = SessionSnapshot {
        is_split: Some(true),
        split_ratio: Some(5.0),
        ..SessionSnapshot::new(Uuid::new_v4(), "/a")
    };
    let back = restored(&one_workspace(vec![session]), false);
    assert_eq!(first(&back).split.ratio, Some(0.95));
}

#[test]
fn flags_fonts_contexts_and_collapse_round_trip() {
    let mut store = AppStore::new();
    let work = store.add_workspace("work");
    let other = store.add_workspace("other");
    let session = add(&mut store, work, "/a");
    store.set_flag(true, session);
    store.set_font_size(session, 16.5);
    assert!(store.set_context(Some("PR #517: restore reap ordering".to_owned()), session));
    store.set_workspace_expanded(other, false);
    let snapshot = store.snapshot();
    assert_eq!(snapshot.workspaces[1].collapsed, Some(true));
    assert_eq!(snapshot.workspaces[0].collapsed, None);
    let back = restored(&snapshot, false);
    let restored_session = back.session(session).unwrap();
    assert!(restored_session.flagged);
    assert_eq!(restored_session.font_size, Some(16.5));
    assert_eq!(
        restored_session.context.as_deref(),
        Some("PR #517: restore reap ordering")
    );
    assert!(!back.workspace(other).unwrap().is_expanded);
    assert!(back.workspace(work).unwrap().is_expanded);
}

#[test]
fn the_focus_set_round_trips_in_tree_order() {
    let mut store = AppStore::new();
    let one = store.add_workspace("one");
    store.add_workspace("two");
    let three = store.add_workspace("three");
    store.set_focus_membership(three, true);
    store.set_focus_membership(one, true);
    store.set_focus_enabled(true);
    let snapshot = store.snapshot();
    assert_eq!(snapshot.focused_workspace_ids, Some(vec![one, three]));
    assert_eq!(snapshot.focus_enabled, Some(true));
    let back = restored(&snapshot, false);
    assert!(back.focus().enabled());
    let visible: Vec<Uuid> = back.visible_workspaces().iter().map(|w| w.id()).collect();
    assert_eq!(visible, [one, three]);

    store.set_focus_enabled(false);
    let snapshot = store.snapshot();
    assert_eq!(snapshot.focus_enabled, None, "off omits the key");
    let back = restored(&snapshot, false);
    assert!(!back.focus().enabled());
    assert_eq!(back.focus().ids().len(), 2);

    let unmarked = store_with("/a").0.snapshot();
    assert_eq!((unmarked.focused_workspace_ids, unmarked.focus_enabled), (None, None));
}

#[test]
fn restore_prunes_stale_focus_members() {
    let snapshot = Snapshot {
        focused_workspace_ids: Some(vec![Uuid::new_v4()]),
        focus_enabled: Some(true),
        ..one_workspace(vec![SessionSnapshot::new(Uuid::new_v4(), "/a")])
    };
    let back = restored(&snapshot, false);
    assert!(back.focus().ids().is_empty());
    assert!(!back.focus().enabled(), "an empty set cannot be enabled");
}

#[test]
fn recency_is_reseeded_without_stale_ids_and_the_selection_in_front() {
    let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let sessions = vec![
        SessionSnapshot::new(a, "/a"),
        SessionSnapshot::new(b, "/b"),
        SessionSnapshot::new(c, "/c"),
    ];
    let snapshot = Snapshot {
        selected_session_id: Some(c),
        session_recency: Some(vec![a, Uuid::new_v4(), b]),
        ..one_workspace(sessions)
    };
    assert_eq!(restored(&snapshot, false).session_recency().items(), [c, a, b]);
    let selection_only = Snapshot {
        session_recency: None,
        ..snapshot
    };
    assert_eq!(restored(&selection_only, false).session_recency().items(), [c]);
}

#[test]
fn a_selection_the_focus_filter_hides_moves_into_view_and_is_written() {
    let (hidden, shown) = (Uuid::new_v4(), Uuid::new_v4());
    let focused = Uuid::new_v4();
    let snapshot = Snapshot {
        selected_session_id: Some(hidden),
        focused_workspace_ids: Some(vec![focused]),
        focus_enabled: Some(true),
        workspaces: vec![
            WorkspaceSnapshot {
                id: Uuid::new_v4(),
                name: "elsewhere".to_owned(),
                sessions: vec![SessionSnapshot::new(hidden, "/a")],
                collapsed: None,
            },
            WorkspaceSnapshot {
                id: focused,
                name: "focused".to_owned(),
                sessions: vec![SessionSnapshot::new(shown, "/b")],
                collapsed: None,
            },
        ],
        ..Snapshot::default()
    };
    let store = restored(&snapshot, false);
    assert_eq!(store.selected_session_id(), Some(shown));
    assert!(store.outbox().events.is_empty(), "a reload is not a selection");
    assert_eq!(store.outbox().save, SaveRequest::Debounced);
}

/// The executable fields, ported from `AppStoreRestoreSeedTests`.
mod seeding {
    use super::*;

    fn armed(split_shown: bool, split_exists: bool) -> SessionSnapshot {
        SessionSnapshot {
            is_split: Some(split_shown),
            has_split: split_exists.then_some(true),
            foreground_command: Some(vec!["ssh".to_owned(), "gate".to_owned()]),
            split_foreground_command: Some(vec!["tail".to_owned(), "-f".to_owned(), "/var/log/x".to_owned()]),
            restore_command: Some("claude --resume abc".to_owned()),
            split_restore_command: Some("htop".to_owned()),
            ..SessionSnapshot::new(Uuid::new_v4(), "/a")
        }
    }

    #[test]
    fn a_bootstrap_restore_arms_both_panes_of_a_split() {
        let back = restored(&one_workspace(vec![armed(true, true)]), true);
        let session = first(&back);
        assert_eq!(session.restore.pending_command.as_deref(), Some("claude --resume abc"));
        assert_eq!(
            session.restore.pending_foreground.as_deref(),
            Some(&["ssh".to_owned(), "gate".to_owned()][..])
        );
        assert_eq!(session.split_restore.pending_command.as_deref(), Some("htop"));
        assert!(session.split_restore.pending_foreground.is_some());
        assert_eq!(
            session.restore.command.as_deref(),
            Some("claude --resume abc"),
            "the override stays sticky"
        );
        assert_eq!(
            session.restore.foreground, None,
            "the persisted capture is never re-armed"
        );
        let resaved = back.snapshot();
        assert_eq!(resaved.workspaces[0].sessions[0].foreground_command, None);
        assert_eq!(
            resaved.workspaces[0].sessions[0].restore_command.as_deref(),
            Some("claude --resume abc")
        );
    }

    #[test]
    fn a_session_without_a_split_drops_the_split_override_on_every_path() {
        for launch in [false, true] {
            let back = restored(&one_workspace(vec![armed(false, false)]), launch);
            let session = first(&back);
            assert_eq!(
                session.split_restore,
                crate::model::PaneRestore::default(),
                "launch {launch}"
            );
            assert_eq!(session.split_launch.command, None);
        }
    }

    #[test]
    fn a_rebuild_arms_nothing() {
        let back = restored(&one_workspace(vec![armed(true, true)]), false);
        let session = first(&back);
        assert_eq!(session.restore.pending_command, None);
        assert_eq!(session.restore.pending_foreground, None);
        assert_eq!(session.split_restore.pending_command, None);
        assert_eq!(session.split_restore.pending_foreground, None);
        assert_eq!(session.split_restore.command.as_deref(), Some("htop"));
    }

    #[test]
    fn the_empty_pinned_to_nothing_value_is_seeded_as_empty() {
        let session = SessionSnapshot {
            restore_command: Some(String::new()),
            ..SessionSnapshot::new(Uuid::new_v4(), "/a")
        };
        let back = restored(&one_workspace(vec![session]), true);
        assert_eq!(first(&back).restore.pending_command.as_deref(), Some(""));
        assert_eq!(
            back.snapshot().workspaces[0].sessions[0].restore_command.as_deref(),
            Some("")
        );
    }

    #[test]
    fn a_fresh_session_has_no_restore_state() {
        let (store, _, _) = store_with("/a");
        assert_eq!(first(&store).restore, crate::model::PaneRestore::default());
        assert!(!first(&store).was_restored);
    }
}

#[test]
fn backgrounds_round_trip_without_the_scratch_override() {
    use crate::model::{BackgroundWatermark, WatermarkKind};
    let color = |hex: &str| BackgroundWatermark {
        kind: WatermarkKind::Color,
        image_path: None,
        text: None,
        color_hex: Some(hex.to_owned()),
        opacity: None,
        fit: None,
        position: None,
        repeats: None,
    };
    let (mut store, workspace, session) = store_with("/a");
    let plain = add_background(&mut store, workspace, "/b");
    {
        let session = store.session_mut(session).unwrap();
        session.background_watermark = Some(color("#000000"));
        session.pane_backgrounds.left = Some(color("#111111"));
        session.pane_backgrounds.scratch = Some(color("#222222"));
    }
    store.session_mut(plain).unwrap().pane_backgrounds.scratch = Some(color("#333333"));
    let snapshot = store.snapshot();
    assert_eq!(
        snapshot.workspaces[0].sessions[1].pane_backgrounds, None,
        "a scratch-only override writes nothing"
    );
    let back = restored(&snapshot, false);
    let session = back.session(session).unwrap();
    assert_eq!(session.background_watermark, Some(color("#000000")));
    assert_eq!(session.pane_backgrounds.left, Some(color("#111111")));
    assert_eq!(session.pane_backgrounds.scratch, None);
}
