//! `workspace.*`: each command's effect and upstream's refusals (`ControlDispatcherWorkspaceTests`,
//! `ControlDispatcherTests`' workspace cases, `ControlServer+WorkspaceCommands.swift`).

use std::collections::BTreeSet;

use agterm_proto::command::WorkspaceCmd;
use agterm_proto::{Args, Command, Response};
use serde_json::json;
use uuid::Uuid;

use super::{Fixture, UNKNOWN, answered_id, args, core_over, failure, send, wire};
use crate::model::{WindowLibrary, Workspace};
use crate::persist::AppSettings;

impl Fixture {
    fn workspace(&mut self, cmd: WorkspaceCmd, target: Option<&str>, args: Args) -> Response {
        self.send(Command::Workspace(cmd), target, args)
    }

    fn workspace_ids(&self) -> Vec<Uuid> {
        self.store().workspaces().iter().map(Workspace::id).collect()
    }

    fn current_workspace(&self) -> Option<Uuid> {
        self.store().current_workspace_id()
    }

    /// The marked set and whether the filter applies.
    fn focus_state(&self) -> (BTreeSet<Uuid>, bool) {
        let focus = self.store().focus();
        (focus.ids.clone(), focus.enabled)
    }
}

// MARK: - new

#[test]
fn new_appends_an_empty_current_workspace_and_keeps_the_selection() {
    let mut fixture = Fixture::new(1);
    let selected = fixture.selected();
    let response = fixture.workspace(WorkspaceCmd::New, None, args(|a| a.name = Some("  alpha  ".into())));
    let id = answered_id(&response).unwrap();
    assert_eq!(
        serde_json::to_value(&response).unwrap(),
        json!({"ok": true, "result": {"id": wire(id)}})
    );
    assert_eq!(fixture.workspace_ids(), [fixture.workspace, id]);
    let workspace = fixture.store().workspace(id).unwrap();
    assert_eq!(workspace.name, "alpha");
    assert!(workspace.sessions.is_empty() && workspace.is_expanded);
    assert_eq!(fixture.current_workspace(), Some(id));
    assert_eq!(fixture.selected(), selected);
}

#[test]
fn new_without_a_name_takes_the_next_default_name() {
    let mut fixture = Fixture::new(0);
    for name in [None, Some("   ".into())] {
        let expected = format!("workspace {}", fixture.workspace_ids().len() + 1);
        let response = fixture.workspace(WorkspaceCmd::New, None, args(|a| a.name = name.clone()));
        let id = answered_id(&response).unwrap();
        assert_eq!(fixture.store().workspace(id).unwrap().name, expected, "{name:?}");
    }
}

#[test]
fn a_plain_create_joins_an_applied_filter_and_becomes_current() {
    let mut fixture = Fixture::new(0);
    let current = fixture.workspace;
    fixture.store_mut().unwrap().set_focused_workspace(current);
    let response = fixture.workspace(WorkspaceCmd::New, None, args(|a| a.collapsed = Some(false)));
    let plain = answered_id(&response).unwrap();
    assert_eq!(fixture.current_workspace(), Some(plain));
    assert_eq!(fixture.focus_state(), ([current, plain].into(), true));
}

/// `test_background_create_keeps_the_current_workspace[collapsed]`: upstream's contract, which the Swift/GTK fork
/// breaks (`conformance/docs/fork-bugs.md`).
#[test]
fn the_background_create_keeps_the_current_workspace() {
    let mut fixture = Fixture::new(0);
    let (current, sessions) = fixture.add_workspace("current", 1);
    fixture.store_mut().unwrap().select_session(Some(sessions[0]));
    fixture.store_mut().unwrap().set_focused_workspace(current);
    let response = fixture.workspace(WorkspaceCmd::New, None, args(|a| a.collapsed = Some(true)));
    let collapsed = answered_id(&response).unwrap();
    assert!(!fixture.store().workspace(collapsed).unwrap().is_expanded);
    assert_eq!(fixture.current_workspace(), Some(current));
    assert_eq!(fixture.selected(), Some(sessions[0]));
    assert_eq!(fixture.focus_state(), ([current].into(), true));
    // the next plain session still lands in the current workspace
    let next = fixture.send(
        Command::Session(agterm_proto::command::SessionCmd::New),
        None,
        Args::default(),
    );
    assert_eq!(
        fixture
            .store()
            .workspace_for_session(answered_id(&next).unwrap())
            .map(Workspace::id),
        Some(current)
    );
}

#[test]
fn new_lands_in_the_named_window_and_answers_on_the_empty_window_once_all_are_closed() {
    let mut library = WindowLibrary::new("/home/u");
    let first = library.new_window(None);
    let second = library.new_window(None);
    let mut core = core_over(library, AppSettings::default());
    let prefix = wire(first)[..8].to_owned();
    let response = send(
        &mut core,
        Command::Workspace(WorkspaceCmd::New),
        None,
        args(|a| a.window = Some(prefix)),
    );
    let id = answered_id(&response).unwrap();
    assert!(core.library().store(first).unwrap().workspace(id).is_some());
    assert_eq!(core.library().store(second).unwrap().workspaces().len(), 1);

    core.library_mut().close_window(first);
    core.library_mut().close_window(second);
    let response = send(&mut core, Command::Workspace(WorkspaceCmd::New), None, Args::default());
    assert!(answered_id(&response).is_some(), "{response:?}");
}

// MARK: - select and go

#[test]
fn select_moves_to_the_first_session_and_an_empty_workspace_keeps_the_selection() {
    let mut fixture = Fixture::new(0);
    let (full, sessions) = fixture.add_workspace("full", 2);
    let (empty, _) = fixture.add_workspace("empty", 0);
    let (_, elsewhere) = fixture.add_workspace("elsewhere", 1);
    fixture.store_mut().unwrap().select_session(Some(elsewhere[0]));

    let response = fixture.workspace(WorkspaceCmd::Select, Some(&wire(full)[..6]), Args::default());
    assert_eq!(answered_id(&response), Some(full));
    assert_eq!(fixture.selected(), Some(sessions[0]));

    let response = fixture.workspace(WorkspaceCmd::Select, Some(&wire(empty)), Args::default());
    assert_eq!(answered_id(&response), Some(empty));
    assert_eq!(fixture.current_workspace(), Some(empty));
    assert_eq!(fixture.selected(), Some(sessions[0]));
}

#[test]
fn go_steps_through_the_workspaces_and_wraps() {
    let mut fixture = Fixture::new(0);
    let (second, second_sessions) = fixture.add_workspace("second", 1);
    let (third, _) = fixture.add_workspace("third", 0);
    fixture.workspace(WorkspaceCmd::Select, Some(&wire(third)), Args::default());
    let first_session = fixture.sessions[0];
    for (to, expected, selected) in [
        ("next", fixture.workspace, first_session),
        ("next", second, second_sessions[0]),
        ("previous", fixture.workspace, first_session),
        // an empty destination becomes current with the selection left where it was
        ("prev", third, first_session),
    ] {
        let response = fixture.workspace(WorkspaceCmd::Go, None, args(|a| a.to = Some(to.into())));
        assert_eq!(answered_id(&response), Some(expected), "{to}");
        assert_eq!(fixture.current_workspace(), Some(expected), "{to}");
        assert_eq!(fixture.selected(), Some(selected), "{to}");
    }
}

#[test]
fn go_stays_inside_the_focus_filter() {
    let mut fixture = Fixture::new(0);
    let (first, first_sessions) = fixture.add_workspace("first", 1);
    fixture.add_workspace("middle", 1);
    let (last, last_sessions) = fixture.add_workspace("last", 1);
    for id in [first, last] {
        fixture.workspace(
            WorkspaceCmd::Focus,
            Some(&wire(id)),
            args(|a| a.mode = Some("add".into())),
        );
    }
    assert!(
        fixture
            .workspace(WorkspaceCmd::Filter, None, args(|a| a.mode = Some("on".into())))
            .ok
    );
    fixture.workspace(WorkspaceCmd::Select, Some(&wire(first)), Args::default());
    let step = |fixture: &mut Fixture| {
        answered_id(&fixture.workspace(WorkspaceCmd::Go, None, args(|a| a.to = Some("next".into()))))
    };
    assert_eq!(step(&mut fixture), Some(last));
    assert_eq!(fixture.selected(), Some(last_sessions[0]));
    assert_eq!(step(&mut fixture), Some(first));
    assert_eq!(fixture.selected(), Some(first_sessions[0]));
}

#[test]
fn go_refuses_a_missing_or_session_only_direction_and_a_lone_workspace() {
    let mut fixture = Fixture::new(0);
    for to in [None, Some("sideways".into()), Some("first".into())] {
        let response = fixture.workspace(WorkspaceCmd::Go, None, args(|a| a.to = to.clone()));
        assert_eq!(response, failure("workspace.go requires --to next|prev"), "{to:?}");
    }
    let response = fixture.workspace(WorkspaceCmd::Go, None, args(|a| a.to = Some("next".into())));
    assert_eq!(response, failure("no other workspace to navigate to"));
}

// MARK: - rename, delete, move

#[test]
fn rename_trims_and_requires_a_name() {
    let mut fixture = Fixture::new(0);
    let response = fixture.workspace(WorkspaceCmd::Rename, None, args(|a| a.name = Some("  after  ".into())));
    assert_eq!(answered_id(&response), Some(fixture.workspace));
    assert_eq!(fixture.store().workspaces()[0].name, "after");
    for name in [None, Some("   ".into())] {
        // the name is checked before the target resolves
        let response = fixture.workspace(WorkspaceCmd::Rename, Some(UNKNOWN), args(|a| a.name = name.clone()));
        assert_eq!(response, failure("workspace.rename requires a name"), "{name:?}");
    }
}

#[test]
fn delete_closes_the_workspace_sessions_and_keeps_the_last_workspace() {
    let mut fixture = Fixture::new(0);
    let (doomed, sessions) = fixture.add_workspace("doomed", 2);
    fixture.store_mut().unwrap().select_session(Some(sessions[1]));
    let response = fixture.workspace(WorkspaceCmd::Delete, Some(&wire(doomed)), Args::default());
    assert_eq!(answered_id(&response), Some(doomed));
    assert_eq!(fixture.workspace_ids(), [fixture.workspace]);
    assert!(sessions.iter().all(|id| fixture.store().session(*id).is_none()));
    assert_eq!(fixture.selected(), Some(fixture.sessions[0]));

    let response = fixture.workspace(WorkspaceCmd::Delete, None, Args::default());
    assert_eq!(response, failure("cannot delete last workspace"));
    assert_eq!(fixture.workspace_ids(), [fixture.workspace]);
}

#[test]
fn move_reorders_among_siblings_and_stops_at_the_ends() {
    let mut fixture = Fixture::new(0);
    let first = fixture.workspace;
    let (second, _) = fixture.add_workspace("second", 0);
    let (third, _) = fixture.add_workspace("third", 0);
    for (to, order) in [
        ("up", [first, third, second]),
        ("up", [third, first, second]),
        ("up", [third, first, second]),
        ("bottom", [first, second, third]),
        ("down", [first, second, third]),
        ("top", [third, first, second]),
    ] {
        let response = fixture.workspace(WorkspaceCmd::Move, Some(&wire(third)), args(|a| a.to = Some(to.into())));
        assert_eq!(answered_id(&response), Some(third), "{to}");
        assert_eq!(fixture.workspace_ids(), order, "{to}");
    }
}

#[test]
fn move_requires_a_known_direction() {
    let mut fixture = Fixture::new(0);
    assert_eq!(
        fixture.workspace(WorkspaceCmd::Move, Some(UNKNOWN), Args::default()),
        failure("workspace.move requires --to")
    );
    assert_eq!(
        fixture.workspace(WorkspaceCmd::Move, None, args(|a| a.to = Some("left".into()))),
        failure("workspace.move --to must be up|down|top|bottom")
    );
}

// MARK: - focus and filter

#[test]
fn focus_modes_mark_and_apply_the_filter() {
    let mut fixture = Fixture::new(0);
    let (first, first_sessions) = fixture.add_workspace("first", 1);
    let (second, _) = fixture.add_workspace("second", 1);
    let none = BTreeSet::new();
    for (mode, id, expected) in [
        (Some("on".into()), first, ([first].into(), true)),
        (Some("add".into()), second, ([first, second].into(), true)),
        (Some("off".into()), first, ([second].into(), true)),
        (Some("off".into()), second, (none.clone(), false)),
        (None, first, ([first].into(), true)),
        (Some("toggle".into()), first, (none.clone(), false)),
        (Some("add".into()), first, ([first].into(), false)),
    ] {
        let response = fixture.workspace(WorkspaceCmd::Focus, Some(&wire(id)), args(|a| a.mode = mode.clone()));
        assert_eq!(answered_id(&response), Some(id), "{mode:?}");
        assert_eq!(fixture.focus_state(), expected, "{mode:?}");
        if mode.as_deref() == Some("on") {
            // narrowing the view moves the selection into it
            assert_eq!(fixture.selected(), Some(first_sessions[0]));
        }
    }
}

#[test]
fn focus_refuses_an_unknown_mode_before_resolving_the_target() {
    let mut fixture = Fixture::new(0);
    let response = fixture.workspace(
        WorkspaceCmd::Focus,
        Some(UNKNOWN),
        args(|a| a.mode = Some("pin".into())),
    );
    assert_eq!(response, failure("invalid focus mode: pin (on|off|toggle|add)"));
    assert_eq!(fixture.focus_state(), (BTreeSet::new(), false));
}

#[test]
fn filter_switches_the_view_without_touching_the_marks() {
    let mut fixture = Fixture::new(0);
    let (marked, _) = fixture.add_workspace("marked", 1);
    let response = fixture.workspace(WorkspaceCmd::Filter, None, args(|a| a.mode = Some("on".into())));
    assert_eq!(serde_json::to_value(&response).unwrap(), json!({"ok": true}));
    assert_eq!(
        fixture.focus_state(),
        (BTreeSet::new(), false),
        "an empty set cannot be filtered to"
    );
    fixture.workspace(
        WorkspaceCmd::Focus,
        Some(&wire(marked)),
        args(|a| a.mode = Some("add".into())),
    );
    for (mode, enabled) in [
        (Some("on".into()), true),
        (Some("toggle".into()), false),
        (None, true),
        (Some("off".into()), false),
    ] {
        assert!(
            fixture
                .workspace(WorkspaceCmd::Filter, None, args(|a| a.mode = mode.clone()))
                .ok
        );
        assert_eq!(fixture.focus_state(), ([marked].into(), enabled), "{mode:?}");
    }
}

#[test]
fn filter_refuses_an_unknown_mode_and_needs_an_open_window() {
    let mut fixture = Fixture::new(0);
    assert_eq!(
        fixture.workspace(WorkspaceCmd::Filter, None, args(|a| a.mode = Some("show".into()))),
        failure("invalid workspace filter mode: show")
    );
    let window = fixture.window;
    fixture.core.library_mut().close_window(window);
    assert_eq!(
        fixture.workspace(WorkspaceCmd::Filter, None, Args::default()),
        failure("no open window")
    );
    let prefix = wire(window)[..8].to_owned();
    assert_eq!(
        fixture.workspace(WorkspaceCmd::Filter, None, args(|a| a.window = Some(prefix))),
        failure("window not open — window.select it first")
    );
}

// MARK: - collapse and expand

#[test]
fn collapse_and_expand_persist_and_ask_the_outline_to_follow() {
    let mut fixture = Fixture::new(0);
    let (id, _) = fixture.add_workspace("folding", 1);
    let _ = fixture.store_mut().unwrap().take_sidebar_expansion_requests();
    for (cmd, expanded) in [
        (WorkspaceCmd::Collapse, false),
        (WorkspaceCmd::Collapse, false),
        (WorkspaceCmd::Expand, true),
        (WorkspaceCmd::Expand, true),
    ] {
        let response = fixture.workspace(cmd, Some(&wire(id)), Args::default());
        assert_eq!(answered_id(&response), Some(id), "{cmd:?}");
        assert_eq!(fixture.store().workspace(id).unwrap().is_expanded, expanded, "{cmd:?}");
        // a repeat still asks: a reveal may have opened a row the persisted flag calls collapsed
        let requests = fixture.store_mut().unwrap().take_sidebar_expansion_requests();
        assert_eq!(requests, [(id, expanded)].into(), "{cmd:?}");
    }
}

// MARK: - targets

#[test]
fn an_unknown_workspace_target_is_refused_by_every_targeted_command() {
    let mut fixture = Fixture::new(0);
    for (cmd, edit) in [
        (WorkspaceCmd::Select, Args::default()),
        (WorkspaceCmd::Rename, args(|a| a.name = Some("renamed".into()))),
        (WorkspaceCmd::Delete, Args::default()),
        (WorkspaceCmd::Move, args(|a| a.to = Some("up".into()))),
        (WorkspaceCmd::Focus, args(|a| a.mode = Some("on".into()))),
        (WorkspaceCmd::Collapse, Args::default()),
        (WorkspaceCmd::Expand, Args::default()),
    ] {
        let response = fixture.workspace(cmd, Some(UNKNOWN), edit);
        assert_eq!(response, failure(&format!("no such workspace: {UNKNOWN}")), "{cmd:?}");
    }
}

#[test]
fn a_workspace_id_resolves_across_windows_and_active_is_the_frontmost_current_one() {
    let mut library = WindowLibrary::new("/home/u");
    let back = library.new_window(None);
    let front = library.new_window(None);
    let back_workspace = library.store(back).unwrap().workspaces()[0].id();
    let front_workspace = library.store(front).unwrap().workspaces()[0].id();
    let mut core = core_over(library, AppSettings::default());
    let rename = |core: &mut crate::Core, target: Option<&str>, name: &str| {
        send(
            core,
            Command::Workspace(WorkspaceCmd::Rename),
            target,
            args(|a| a.name = Some(name.into())),
        )
    };
    assert_eq!(
        answered_id(&rename(&mut core, Some(&wire(back_workspace)), "back")),
        Some(back_workspace)
    );
    assert_eq!(answered_id(&rename(&mut core, None, "front")), Some(front_workspace));
    assert_eq!(core.library().store(back).unwrap().workspaces()[0].name, "back");
    assert_eq!(core.library().store(front).unwrap().workspaces()[0].name, "front");
}
