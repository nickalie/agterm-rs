//! Ported from upstream's `TitlebarCompositionTests` and `InterfaceElementTests` (the divider rule), plus the button
//! states, popover rows and actions per selection state.

use std::fmt::Write as _;

use agterm_proto::AppIdentity;

use super::*;
use crate::model::{NewSession, StatusPane, WindowLibrary};
use crate::persist::AppSettings;
use crate::{CoreConfig, CoreState};

fn parts() -> TitlebarParts {
    TitlebarParts {
        workspace_name: None,
        session_name: Some("api".into()),
        window_name: None,
        context: None,
        detail: "~/src/api".into(),
        remote_host: None,
    }
}

#[test]
fn normal_shows_the_identity_over_the_detail_and_the_context_replaces_it() {
    let composed = compose(&parts(), ToolbarMode::Normal);
    assert_eq!(composed.title, "api");
    assert_eq!(composed.subtitle, "~/src/api");
    assert!(composed.tail.is_empty());
    let with_context = TitlebarParts {
        context: Some("deploying".into()),
        ..parts()
    };
    assert_eq!(compose(&with_context, ToolbarMode::Normal).subtitle, "deploying");
}

#[test]
fn compact_puts_the_context_last_on_one_row() {
    let with_context = TitlebarParts {
        context: Some("deploying".into()),
        ..parts()
    };
    let composed = compose(&with_context, ToolbarMode::Compact);
    assert_eq!(composed.title, "api");
    assert_eq!(composed.tail, " · deploying");
    assert!(composed.subtitle.is_empty());
    let alone = TitlebarParts {
        session_name: None,
        context: Some("deploying".into()),
        ..parts()
    };
    assert_eq!(
        compose(&alone, ToolbarMode::Compact).tail,
        "deploying",
        "no separator without a part before it"
    );
    let hosted = TitlebarParts {
        remote_host: Some("box".into()),
        ..alone
    };
    assert_eq!(compose(&hosted, ToolbarMode::Compact).tail, " · deploying");
    assert_eq!(compose(&parts(), ToolbarMode::Compact).tail, "", "no context, no tail");
}

#[test]
fn hidden_composes_nothing() {
    assert_eq!(compose(&parts(), ToolbarMode::Hidden), TitlebarComposition::default());
}

#[test]
fn the_identity_joins_workspace_session_and_window_and_caps_the_workspace() {
    let full = TitlebarParts {
        workspace_name: Some("backend".into()),
        window_name: Some("work".into()),
        remote_host: Some("box".into()),
        ..parts()
    };
    let composed = compose(&full, ToolbarMode::Normal);
    assert_eq!(composed.title, "backend — api — work");
    assert_eq!(composed.host.as_deref(), Some("box"));
    let long = TitlebarParts {
        workspace_name: Some("a-very-long-workspace-name-indeed".into()),
        ..parts()
    };
    assert_eq!(
        compose(&long, ToolbarMode::Normal).title,
        "a-very-long-workspace-na… — api"
    );
    let blank = TitlebarParts {
        workspace_name: Some("   ".into()),
        ..parts()
    };
    assert_eq!(compose(&blank, ToolbarMode::Normal).title, "api");
}

#[test]
fn a_divider_sits_only_between_two_groups_of_two() {
    assert_eq!(
        group_dividers(2, 2, 3),
        GroupDividers {
            after_jump: true,
            after_panes: true
        }
    );
    assert_eq!(
        group_dividers(1, 2, 3),
        GroupDividers {
            after_jump: false,
            after_panes: true
        }
    );
    assert_eq!(group_dividers(2, 1, 2), GroupDividers::default());
    assert_eq!(
        group_dividers(2, 0, 2),
        GroupDividers {
            after_jump: false,
            after_panes: true
        }
    );
    assert_eq!(group_dividers(1, 0, 3), GroupDividers::default());
    assert_eq!(group_dividers(0, 2, 1), GroupDividers::default());
}

fn core_with(settings: AppSettings) -> (Core, Uuid) {
    let library = WindowLibrary::seeded("/home/u");
    let window = library.active_window_id().unwrap_or_default();
    let core = Core::new(
        CoreConfig {
            app: AppIdentity::from_recorded("0.1.0", None),
            capabilities: Vec::new(),
        },
        CoreState {
            library,
            settings,
            persistence: None,
        },
    );
    (core, window)
}

fn add_session(core: &mut Core, window: Uuid) -> Uuid {
    let store = core.library_mut().store_mut(window).unwrap();
    let workspace = store.current_workspace_id().unwrap();
    store.add_session(workspace, NewSession::at_cwd("/tmp")).unwrap()
}

fn set_status(core: &mut Core, session: Uuid, status: AgentStatus) {
    let store = core.library_mut().store_for_session_mut(session).unwrap();
    store.set_agent_indicator(
        AgentIndicator {
            status,
            ..AgentIndicator::default()
        },
        session,
    );
}

#[test]
fn the_default_bar_shows_every_button_but_the_opt_in_ones() {
    let (core, window) = core_with(AppSettings::default());
    let bar = core.titlebar(window);
    assert_eq!(bar.mode, ToolbarMode::Compact, "no setting reads as compact");
    assert!(bar.sidebar_toggle);
    assert!(bar.attention.is_none(), "the bell is opt-in");
    assert!(bar.custom_commands.is_none(), "hidden by default");
    assert_eq!(bar.recent, Some(Button { enabled: false }), "nothing else to switch to");
    assert_eq!(
        bar.scratch,
        Some(ScratchButton {
            active: false,
            enabled: true
        })
    );
    let split = bar.split.unwrap();
    assert_eq!(
        (split.glyph, split.hint, split.action),
        (SplitGlyph::None, "Split right", BuiltinAction::ToggleSplit)
    );
    assert_eq!(bar.dashboard, Some(Button { enabled: true }));
    assert_eq!(bar.quick_terminal, Some(Button { enabled: true }));
    assert_eq!(
        bar.dividers,
        GroupDividers {
            after_jump: false,
            after_panes: true
        }
    );
    assert_eq!(bar.label.title, "u");
}

#[test]
fn hidden_elements_drop_their_buttons_and_parts() {
    let settings = AppSettings {
        hidden_interface_elements: Some(vec!["scratch".into(), "sessionName".into(), "dashboard".into()]),
        shown_interface_elements: Some(vec!["workspaceName".into()]),
        attention_button_enabled: Some(true),
        toolbar_mode: Some("normal".into()),
        ..AppSettings::default()
    };
    let (core, window) = core_with(settings);
    let bar = core.titlebar(window);
    assert!(bar.scratch.is_none() && bar.dashboard.is_none());
    assert_eq!(
        bar.attention,
        Some(AttentionButton {
            bell: AttentionBell::None,
            enabled: false
        })
    );
    assert_eq!(bar.label.title, "workspace 1", "the workspace name only");
    assert_eq!(bar.label.subtitle, "/home/u");
    // jump 2, panes 1, window 1: no divider
    assert_eq!(bar.dividers, GroupDividers::default());
}

#[test]
fn the_split_glyph_follows_the_split_state() {
    let (mut core, window) = core_with(AppSettings::default());
    let session = core.library().store(window).unwrap().active_session().unwrap().id();
    let mut set = |shown: bool, focused: bool, axis: SplitAxis| {
        let store = core.library_mut().store_mut(window).unwrap();
        let state = store.session_mut(session).unwrap();
        state.split.exists = true;
        state.split.shown = shown;
        state.split.focused = focused;
        state.split.axis = axis;
        core.titlebar(window).split.unwrap()
    };
    let both = set(true, false, SplitAxis::LeftRight);
    assert_eq!((both.glyph, both.hint), (SplitGlyph::Both, "Hide split"));
    let stacked = set(true, true, SplitAxis::TopBottom);
    assert_eq!(
        (stacked.glyph, stacked.action),
        (SplitGlyph::BothHorizontal, BuiltinAction::ToggleHorizontalSplit)
    );
    let right = set(false, true, SplitAxis::LeftRight);
    assert_eq!((right.glyph, right.hint), (SplitGlyph::Right, "Show split"));
    assert_eq!(set(false, false, SplitAxis::LeftRight).glyph, SplitGlyph::Left);
    assert_eq!(set(false, true, SplitAxis::TopBottom).glyph, SplitGlyph::Bottom);
    assert_eq!(set(false, false, SplitAxis::TopBottom).glyph, SplitGlyph::Top);
    assert_eq!(SplitGlyph::BothHorizontal.name(), "both-horizontal");
}

#[test]
fn no_session_disables_the_session_buttons_and_titles_agterm() {
    let (mut core, window) = core_with(AppSettings::default());
    core.library_mut().store_mut(window).unwrap().select_session(None);
    let bar = core.titlebar(window);
    assert_eq!(bar.label.title, "Agterm");
    assert!(!bar.scratch.unwrap().enabled);
    assert!(!bar.split.unwrap().enabled);
    assert_eq!(bar.dashboard, Some(Button { enabled: false }));
    assert_eq!(bar.quick_terminal, Some(Button { enabled: true }));
}

#[test]
fn recent_rows_list_the_other_sessions_most_recent_first() {
    let (mut core, window) = core_with(AppSettings::default());
    let first = core.library().store(window).unwrap().active_session().unwrap().id();
    let second = add_session(&mut core, window);
    let third = add_session(&mut core, window);
    for id in [first, second, third] {
        core.library_mut().store_mut(window).unwrap().select_session(Some(id));
    }
    let rows = core.recent_session_rows(window);
    assert_eq!(rows.iter().map(|row| row.session).collect::<Vec<_>>(), [second, first]);
    assert!(
        rows.iter().all(|row| row.subtitle.starts_with("workspace 1 · ")),
        "{rows:?}"
    );
    assert_eq!(core.titlebar(window).recent, Some(Button { enabled: true }));
    core.titlebar_action(window, TitlebarAction::SelectRecent(first));
    assert_eq!(core.library().store(window).unwrap().selected_session_id(), Some(first));
}

#[test]
fn the_bell_and_attention_rows_span_every_window() {
    let settings = AppSettings {
        attention_button_enabled: Some(true),
        ..AppSettings::default()
    };
    let (mut core, window) = core_with(settings);
    let local = add_session(&mut core, window);
    let other = core.library_mut().new_window(Some("other"));
    let workspace = core.library_mut().store_mut(other).unwrap().add_workspace("ops");
    let far = core
        .library_mut()
        .store_mut(other)
        .unwrap()
        .add_session(workspace, NewSession::at_cwd("/srv"))
        .unwrap();
    set_status(&mut core, local, AgentStatus::Active);
    assert_eq!(core.titlebar(window).attention.unwrap().bell, AttentionBell::Attention);
    set_status(&mut core, far, AgentStatus::Blocked);
    let bell = core.titlebar(window).attention.unwrap();
    assert_eq!((bell.bell, bell.enabled), (AttentionBell::Blocked, true));
    let rows = core.attention_rows();
    assert_eq!(
        rows.iter().map(|row| row.session).collect::<Vec<_>>(),
        [far, local],
        "blocked first"
    );
    assert!(rows[0].subtitle.starts_with("other · ops · "), "{}", rows[0].subtitle);
    assert_eq!(rows[0].window, other);
    assert!(rows.iter().all(|row| row.enabled && row.indicator.is_some()));
}

#[test]
fn choosing_an_attention_row_in_another_window_raises_it_and_selects_the_session() {
    let (mut core, window) = core_with(AppSettings::default());
    let other = core.library_mut().new_window(Some("other"));
    core.report_frontmost(window);
    let far = add_session(&mut core, other);
    {
        let store = core.library_mut().store_mut(other).unwrap();
        let session = store.sessions().map(Session::id).find(|id| *id != far).unwrap();
        store.select_session(Some(session));
    }
    set_status(&mut core, far, AgentStatus::Blocked);
    core.library_mut()
        .store_mut(other)
        .unwrap()
        .session_mut(far)
        .unwrap()
        .agent_indicator
        .status_pane = Some(StatusPane::Left);
    let _ = core.take_window_requests();
    core.titlebar_action(
        window,
        TitlebarAction::SelectAttention {
            window: other,
            session: far,
        },
    );
    assert_eq!(core.library().active_window_id(), Some(other));
    assert_eq!(core.library().store(other).unwrap().selected_session_id(), Some(far));
    assert!(core.take_window_requests().contains(&WindowRequest::Raise(other)));
}

#[test]
fn a_covered_window_refuses_its_attention_rows_and_disables_its_buttons() {
    let settings = AppSettings {
        attention_button_enabled: Some(true),
        ..AppSettings::default()
    };
    let (mut core, window) = core_with(settings);
    let session = core.library().store(window).unwrap().active_session().unwrap().id();
    set_status(&mut core, session, AgentStatus::Blocked);
    core.toggle_dashboard(window);
    assert!(core.dashboard(window).is_some());
    assert!(!core.ui_actions_enabled(window));
    assert!(!core.attention_rows()[0].enabled);
    assert!(!core.select_attention(window, session));
}

/// A core whose keymap holds `count` custom commands `c0`..`c{count-1}` in that order, persisted under `state`.
fn core_with_commands(state: &std::path::Path, count: usize) -> (Core, Uuid) {
    let config = state.join("config");
    std::fs::create_dir_all(&config).unwrap();
    let keymap = (0..count).fold(String::new(), |mut keymap, index| {
        let _ = writeln!(keymap, "command \"c{index}\" ctrl+shift+f{} true", index + 1);
        keymap
    });
    std::fs::write(config.join("keymap.conf"), keymap).unwrap();
    let mut persistence = crate::persist::LibraryPersistence::new(state);
    let library = persistence.bootstrap("/home/u");
    let window = library.active_window_id().unwrap();
    let mut core = Core::new(
        CoreConfig {
            app: AppIdentity::from_recorded("0.1.0", None),
            capabilities: Vec::new(),
        },
        CoreState {
            library,
            settings: AppSettings {
                config_directory: Some(config.to_string_lossy().into_owned()),
                ..AppSettings::default()
            },
            persistence: Some(persistence),
        },
    );
    assert_eq!(core.reload_keymap(), 0, "the keymap parses");
    (core, window)
}

fn names(commands: &[crate::keymap::CustomCommand]) -> Vec<&str> {
    commands.iter().map(|command| command.name.as_str()).collect()
}

#[test]
fn a_short_keymap_lists_every_command_in_order_without_a_leading_section() {
    let state = tempfile::tempdir().unwrap();
    let (core, _) = core_with_commands(state.path(), 3);
    let rows = core.custom_command_rows();
    assert!(rows.leading.is_empty());
    assert_eq!(names(&rows.rest), ["c0", "c1", "c2"]);
}

#[test]
fn past_the_limit_the_most_run_commands_lead_both_groups_in_keymap_order() {
    let state = tempfile::tempdir().unwrap();
    let (core, _) = core_with_commands(state.path(), 7);
    let rows = core.custom_command_rows();
    assert!(rows.leading.is_empty(), "nothing run yet");
    assert_eq!(names(&rows.rest), ["c0", "c1", "c2", "c3", "c4", "c5", "c6"]);
    let usage = crate::keymap::usage::UsageStore::new(state.path());
    let commands = core.keymap().commands().to_vec();
    for (index, runs) in [(6, 3), (2, 1), (4, 2)] {
        for _ in 0..runs {
            usage.record(&commands[index]);
        }
    }
    let rows = core.custom_command_rows();
    assert_eq!(
        names(&rows.leading),
        ["c2", "c4", "c6"],
        "keymap order, not count order"
    );
    assert_eq!(names(&rows.rest), ["c0", "c1", "c3", "c5"]);
}

#[test]
fn a_command_row_does_not_run_under_a_cover() {
    let state = tempfile::tempdir().unwrap();
    let (mut core, window) = core_with_commands(state.path(), 1);
    let id = core.keymap().commands()[0].id;
    // the in-memory core has no process host, so a command that runs reports its failure in a banner
    let _ = core.take_notifications();
    core.toggle_dashboard(window);
    assert!(core.dashboard(window).is_some());
    core.titlebar_action(window, TitlebarAction::RunCustomCommand(id));
    assert!(core.take_notifications().is_empty(), "nothing ran behind the dashboard");
    core.toggle_dashboard(window);
    assert!(core.dashboard(window).is_none());
    core.titlebar_action(window, TitlebarAction::RunCustomCommand(id));
    assert!(!core.take_notifications().is_empty(), "uncovered, the command runs");
}

#[test]
fn a_popover_holds_auto_follow_until_it_closes() {
    let (mut core, window) = core_with(AppSettings::default());
    let idle = core.library().store(window).unwrap().active_session().unwrap().id();
    let blocked = add_session(&mut core, window);
    core.library_mut().store_mut(window).unwrap().select_session(Some(idle));
    set_status(&mut core, blocked, AgentStatus::Blocked);
    core.titlebar_action(window, TitlebarAction::PopoverOpened);
    assert!(
        core.auto_follow_step(window).is_none(),
        "held while the popover is open"
    );
    core.titlebar_action(window, TitlebarAction::PopoverClosed);
    assert_eq!(
        core.auto_follow_step(window).map(|followed| followed.session),
        Some(blocked)
    );
}

#[test]
fn the_buttons_toggle_through_the_builtin_actions() {
    let (mut core, window) = core_with(AppSettings::default());
    let visible = |core: &Core| core.library().store(window).unwrap().sidebar().visible;
    let before = visible(&core);
    core.titlebar_action(window, TitlebarAction::ToggleSidebar);
    assert_ne!(visible(&core), before);
    core.titlebar_action(window, TitlebarAction::Dashboard);
    assert!(core.dashboard(window).is_some());
}

#[test]
fn an_attention_row_reveals_the_pane_in_its_own_window_when_another_holds_the_same_id() {
    use crate::model::WindowInfo;
    use crate::persist::{SessionSnapshot, Snapshot, WorkspaceSnapshot};
    let (a, b, session) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let snapshot = |name: &str| Snapshot {
        workspaces: vec![WorkspaceSnapshot {
            id: Uuid::new_v4(),
            name: name.to_owned(),
            sessions: vec![SessionSnapshot::new(session, "/project")],
            collapsed: None,
        }],
        ..Snapshot::default()
    };
    let mut library = WindowLibrary::new("/home/u");
    for (id, name) in [(a, "a"), (b, "b")] {
        library.insert_window(WindowInfo {
            id,
            name: name.to_owned(),
        });
        let mut store = AppStore::new();
        store.restore(&snapshot(name), false);
        library.open_window(id, store);
    }
    library.set_frontmost(Some(a));
    let mut core = Core::new(
        CoreConfig {
            app: AppIdentity::from_recorded("0.1.0", None),
            capabilities: Vec::new(),
        },
        CoreState {
            library,
            settings: AppSettings::default(),
            persistence: None,
        },
    );
    for window in [a, b] {
        let store = core.library_mut().store_mut(window).unwrap();
        store.select_session(None);
        store.session_mut(session).unwrap().scratch_active = true;
    }
    let store = core.library_mut().store_mut(b).unwrap();
    store.set_agent_indicator(
        AgentIndicator {
            status: AgentStatus::Blocked,
            status_pane: Some(StatusPane::Left),
            ..AgentIndicator::default()
        },
        session,
    );
    assert!(core.select_attention(b, session));
    let scratch = |core: &Core, window| {
        core.library()
            .store(window)
            .unwrap()
            .session(session)
            .unwrap()
            .scratch_active
    };
    assert!(!scratch(&core, b), "the blocked pane is revealed in the chosen window");
    assert!(scratch(&core, a), "the other window's copy is left alone");
    assert_eq!(core.library().store(b).unwrap().selected_session_id(), Some(session));
    assert_eq!(core.library().store(a).unwrap().selected_session_id(), None);
}
