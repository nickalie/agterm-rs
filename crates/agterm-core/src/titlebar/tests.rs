//! Ported from upstream's `TitlebarCompositionTests` and `InterfaceElementTests` (the divider rule), plus the button
//! states, popover rows and actions per selection state.

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

#[test]
fn custom_command_rows_keep_keymap_order_without_usage() {
    let (core, _) = core_with(AppSettings::default());
    assert_eq!(core.custom_command_rows(), CustomCommandRows::default());
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
