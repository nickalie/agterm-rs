//! `sidebar` and `sidebar.*`: each command's effect and upstream's refusals (`ControlDispatcherSidebarTests`,
//! `ControlServerSidebarWidthTests`, `ControlServerFlaggedLayoutTests`, `ControlServer+AppCommands.swift`).

use agterm_proto::command::SidebarCmd;
use agterm_proto::{Args, Command, Response};
use serde_json::json;
use uuid::Uuid;

use super::{Fixture, UNKNOWN, args, core_over, failure, send, wire};
use crate::model::{SIDEBAR_WIDTH_MAX, SIDEBAR_WIDTH_MIN, SidebarMode, WindowLibrary};
use crate::persist::AppSettings;
use crate::persist::settings::FlaggedViewLayout;

impl Fixture {
    fn sidebar(&mut self, cmd: SidebarCmd, args: Args) -> Response {
        self.send(Command::Sidebar(cmd), None, args)
    }

    fn mode_arg(mode: Option<&str>) -> Args {
        args(|a| a.mode = mode.map(str::to_owned))
    }

    fn layout(&self) -> FlaggedViewLayout {
        self.core.settings().effective_flagged_view_layout()
    }

    /// Each workspace's persisted expansion, in tree order.
    fn expanded(&self) -> Vec<bool> {
        self.store().workspaces().iter().map(|w| w.is_expanded).collect()
    }
}

fn ok_with(result: &serde_json::Value) -> serde_json::Value {
    json!({"ok": true, "result": result})
}

fn value(response: &Response) -> serde_json::Value {
    serde_json::to_value(response).unwrap()
}

// MARK: - visibility

#[test]
fn visibility_shows_hides_and_toggles_the_frontmost_sidebar() {
    let mut fixture = Fixture::new(0);
    for (mode, visible) in [
        (Some("hide"), false),
        (Some("hide"), false),
        (Some("toggle"), true),
        (None, false),
        (Some("show"), true),
    ] {
        let response = fixture.sidebar(SidebarCmd::Visibility, Fixture::mode_arg(mode));
        assert_eq!(response, Response::success(), "{mode:?}");
        assert_eq!(fixture.store().sidebar().visible, visible, "{mode:?}");
    }
}

#[test]
fn visibility_and_mode_refuse_an_unknown_mode_without_a_change() {
    let mut fixture = Fixture::new(0);
    for cmd in [SidebarCmd::Visibility, SidebarCmd::Mode] {
        let response = fixture.sidebar(cmd, Fixture::mode_arg(Some("sideways")));
        assert_eq!(response, failure("invalid sidebar mode: sideways"));
    }
    // `on`/`off` belong to the other toggles, not to `sidebar`
    let response = fixture.sidebar(SidebarCmd::Visibility, Fixture::mode_arg(Some("on")));
    assert_eq!(response, failure("invalid sidebar mode: on"));
    assert!(fixture.store().sidebar().visible);
    assert_eq!(fixture.store().sidebar().mode, SidebarMode::Tree);
}

#[test]
fn visibility_and_mode_need_an_open_window() {
    let mut library = WindowLibrary::new("/home/u");
    let window = library.new_window(None);
    library.close_window(window);
    let mut core = core_over(library, AppSettings::default());
    for cmd in [SidebarCmd::Visibility, SidebarCmd::Mode] {
        let response = send(&mut core, Command::Sidebar(cmd), None, Args::default());
        assert_eq!(response, failure("no open window"));
    }
}

// MARK: - mode

#[test]
fn mode_switches_between_the_tree_and_the_flagged_view() {
    let mut fixture = Fixture::new(0);
    for (mode, shown) in [
        (Some("flagged"), SidebarMode::Flagged),
        (Some("flagged"), SidebarMode::Flagged),
        (Some("toggle"), SidebarMode::Tree),
        (None, SidebarMode::Flagged),
        (Some("tree"), SidebarMode::Tree),
    ] {
        assert_eq!(
            fixture.sidebar(SidebarCmd::Mode, Fixture::mode_arg(mode)),
            Response::success()
        );
        assert_eq!(fixture.store().sidebar().mode, shown, "{mode:?}");
    }
}

// MARK: - flagged layout

#[test]
fn flagged_layout_sets_toggles_and_echoes_the_layout() {
    let mut fixture = Fixture::new(0);
    let steps = [
        (Some("plain"), "plain"),
        (Some("toggle"), "tree"),
        (Some("toggle"), "flat"),
        (Some("flat"), "flat"),
        (None, "tree"),
        (Some("tree"), "tree"),
        (Some("plain"), "plain"),
    ];
    for (mode, layout) in steps {
        let response = fixture.sidebar(SidebarCmd::FlaggedLayout, Fixture::mode_arg(mode));
        assert_eq!(value(&response), ok_with(&json!({"text": layout})), "{mode:?}");
        assert_eq!(fixture.layout().raw(), layout, "{mode:?}");
    }
}

#[test]
fn flagged_layout_refuses_an_unknown_layout_and_needs_no_window() {
    let mut fixture = Fixture::new(0);
    let response = fixture.sidebar(SidebarCmd::FlaggedLayout, Fixture::mode_arg(Some("grid")));
    assert_eq!(response, failure("invalid flagged layout: grid"));
    assert_eq!(fixture.core.settings().flagged_view_layout, None);

    let mut library = WindowLibrary::new("/home/u");
    let window = library.new_window(None);
    library.close_window(window);
    let mut core = core_over(library, AppSettings::default());
    let response = send(
        &mut core,
        Command::Sidebar(SidebarCmd::FlaggedLayout),
        None,
        Fixture::mode_arg(Some("tree")),
    );
    assert_eq!(value(&response), ok_with(&json!({"text": "tree"})));
}

#[test]
fn flagged_layout_is_written_to_the_settings_file_once_it_changes() {
    let dir = tempfile::tempdir().unwrap();
    let mut library = WindowLibrary::new("/home/u");
    library.new_window(None);
    let mut core = crate::Core::new(
        crate::CoreConfig {
            app: agterm_proto::AppIdentity::from_recorded("0.1.0", None),
            capabilities: Vec::new(),
        },
        crate::CoreState {
            library,
            settings: AppSettings::default(),
            persistence: Some(crate::persist::LibraryPersistence::new(dir.path())),
        },
    );
    let path = crate::persist::SettingsStore::new(dir.path()).path();
    send(
        &mut core,
        Command::Sidebar(SidebarCmd::FlaggedLayout),
        None,
        Fixture::mode_arg(Some("flat")),
    );
    assert!(!path.exists(), "the effective layout was already flat");
    send(
        &mut core,
        Command::Sidebar(SidebarCmd::FlaggedLayout),
        None,
        Fixture::mode_arg(Some("tree")),
    );
    let saved = crate::persist::SettingsStore::new(dir.path()).load();
    assert_eq!(saved.flagged_view_layout.as_deref(), Some("tree"));
}

// MARK: - width

#[test]
fn width_is_clamped_stored_and_echoed() {
    let mut fixture = Fixture::new(0);
    for (asked, stored) in [
        (250.0, 250.0),
        (300.5, 300.5),
        (99999.0, SIDEBAR_WIDTH_MAX),
        (5.0, SIDEBAR_WIDTH_MIN),
    ] {
        let response = fixture.sidebar(SidebarCmd::Width, args(|a| a.sidebar_width = Some(asked)));
        assert_eq!(value(&response), ok_with(&json!({"sidebarWidth": stored})), "{asked}");
        assert!((fixture.store().sidebar().width - stored).abs() < f64::EPSILON);
    }
}

#[test]
fn width_requires_finite_points() {
    let mut fixture = Fixture::new(0);
    let before = fixture.store().sidebar().width;
    for points in [None, Some(f64::NAN), Some(f64::INFINITY)] {
        let response = fixture.sidebar(SidebarCmd::Width, args(|a| a.sidebar_width = points));
        assert_eq!(response, failure("sidebar.width requires a width in points"));
    }
    assert!((fixture.store().sidebar().width - before).abs() < f64::EPSILON);
}

#[test]
fn width_expand_and_collapse_take_an_open_window_or_refuse_it() {
    let mut fixture = Fixture::new(0);
    let other = fixture.core.library_mut().new_window(None);
    let response = fixture.sidebar(
        SidebarCmd::Width,
        args(|a| {
            a.sidebar_width = Some(300.0);
            a.window = Some(wire(other));
        }),
    );
    assert_eq!(value(&response), ok_with(&json!({"sidebarWidth": 300.0})));
    let width_of = |fixture: &Fixture, id: Uuid| fixture.core.library().store(id).unwrap().sidebar().width;
    assert!((width_of(&fixture, other) - 300.0).abs() < f64::EPSILON);
    assert!((width_of(&fixture, fixture.window) - 300.0).abs() > 1.0);

    for (cmd, sidebar_width) in [
        (SidebarCmd::Width, Some(250.0)),
        (SidebarCmd::Expand, None),
        (SidebarCmd::Collapse, None),
    ] {
        let response = fixture.sidebar(
            cmd,
            args(|a| {
                a.sidebar_width = sidebar_width;
                a.window = Some(UNKNOWN.to_owned());
            }),
        );
        assert_eq!(response, failure(&format!("no such window: {UNKNOWN}")), "{cmd:?}");
    }
}

// MARK: - expand and collapse

#[test]
fn collapse_keeps_the_current_workspace_open_and_expand_opens_all() {
    let mut fixture = Fixture::new(0);
    fixture.add_workspace("two", 1);
    fixture.add_workspace("three", 0);
    let current = fixture.store().current_workspace_id().unwrap();
    for _ in 0..2 {
        assert_eq!(
            fixture.sidebar(SidebarCmd::Collapse, Args::default()),
            Response::success()
        );
        for workspace in fixture.store().workspaces() {
            assert_eq!(workspace.is_expanded, workspace.id() == current);
        }
    }
    let requests = fixture.store_mut().unwrap().take_sidebar_expansion_requests();
    assert_eq!(requests.len(), 3);
    assert!(requests.iter().all(|(id, open)| *open == (*id == current)));

    for _ in 0..2 {
        assert_eq!(
            fixture.sidebar(SidebarCmd::Expand, Args::default()),
            Response::success()
        );
        assert_eq!(fixture.expanded(), [true, true, true]);
    }
    let requests = fixture.store_mut().unwrap().take_sidebar_expansion_requests();
    assert!(requests.len() == 3 && requests.values().all(|open| *open));
}

#[test]
fn collapse_and_expand_need_workspace_rows() {
    let mut fixture = Fixture::new(0);
    let (_, sessions) = fixture.add_workspace("two", 1);
    fixture.store_mut().unwrap().set_flag(true, sessions[0]);
    fixture.store_mut().unwrap().set_sidebar_mode(SidebarMode::Flagged);
    fixture.sidebar(SidebarCmd::FlaggedLayout, Fixture::mode_arg(Some("flat")));
    assert_eq!(
        fixture.sidebar(SidebarCmd::Collapse, Args::default()),
        Response::success()
    );
    assert_eq!(fixture.expanded(), [true, true]);
    assert!(
        fixture
            .store_mut()
            .unwrap()
            .take_sidebar_expansion_requests()
            .is_empty()
    );

    // the flagged tree has workspace rows again, so it collapses like the ordinary tree
    fixture.sidebar(SidebarCmd::FlaggedLayout, Fixture::mode_arg(Some("tree")));
    fixture.sidebar(SidebarCmd::Collapse, Args::default());
    assert!(fixture.expanded().contains(&false));
    let store = fixture.store();
    assert!(store.renders_workspace_rows(FlaggedViewLayout::Tree));
    assert!(!store.renders_workspace_rows(FlaggedViewLayout::Plain));
}
