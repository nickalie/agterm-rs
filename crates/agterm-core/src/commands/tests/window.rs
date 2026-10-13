//! `window.*`: the commands' model effects, the requests they leave for the frontend, and their refusals (strings
//! from upstream's `ControlDispatcher.swift` and `ControlServer+WindowCommands.swift`).

use agterm_proto::command::{WindowCmd, WorkspaceCmd};
use agterm_proto::{AppIdentity, Args, Command, Response};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{UNKNOWN, answered_id, args, core_over, failure, send, wire};
use crate::model::WindowLibrary;
use crate::persist::{AppSettings, LibraryPersistence};
use crate::{Core, CoreConfig, CoreState, WindowRequest, WindowSize, WindowState};

const NOT_OPEN: &str = "window not open — window.select it first";

fn window(core: &mut Core, cmd: WindowCmd, target: Option<&str>, args: Args) -> Response {
    send(core, Command::Window(cmd), target, args)
}

fn windows(core: &mut Core) -> Value {
    let response = window(core, WindowCmd::List, None, Args::default());
    assert!(response.ok, "{response:?}");
    response.result.unwrap_or_default()
}

/// A core over `count` fresh windows in memory, the last one frontmost, and their ids.
fn with_windows(count: usize) -> (Core, Vec<Uuid>) {
    let mut library = WindowLibrary::new("/home/u");
    let ids = (0..count).map(|_| library.new_window(None)).collect();
    (core_over(library, AppSettings::default()), ids)
}

fn active(core: &Core) -> Option<Uuid> {
    core.library().active_window_id()
}

/// The suite's `test_fresh_window_node`, with the fields the frontend reports.
#[test]
fn a_fresh_window_reports_its_live_state_once_the_frontend_does() {
    let (mut core, ids) = with_windows(1);
    let id = wire(ids[0]);
    assert_eq!(
        windows(&mut core),
        json!({"windows": [{"id": id, "name": "window 1", "open": true, "active": true, "sidebarVisible": true}]})
    );
    core.set_window_state(
        ids[0],
        Some(WindowState {
            minimized: Some(true),
            ..WindowState::default()
        }),
    );
    let text = serde_json::to_string(&windows(&mut core)).unwrap();
    assert_eq!(
        text,
        format!(
            r#"{{"windows":[{{"id":"{id}","name":"window 1","open":true,"active":true,"sidebarVisible":true,"fullscreen":false,"zoomed":false,"minimized":true}}]}}"#
        )
    );
    core.set_window_state(ids[0], Some(WindowState::default()));
    assert_eq!(
        windows(&mut core)["windows"][0]["minimized"],
        false,
        "no read-back reads as not minimized"
    );
}

#[test]
fn every_window_is_listed_in_order_and_a_closed_one_has_no_live_fields() {
    let settings = AppSettings {
        auto_follow_attention: Some("s30".to_owned()),
        ..AppSettings::default()
    };
    let mut library = WindowLibrary::new("/home/u");
    let closed = library.new_window(Some("archive"));
    let open = library.new_window(None);
    library.store_mut(open).unwrap().set_sidebar_visible(false);
    let mut core = core_over(library, settings);
    core.set_window_state(closed, Some(WindowState::default()));
    core.library_mut().close_window(closed);
    assert_eq!(
        windows(&mut core),
        json!({"windows": [
            {"id": wire(closed), "name": "archive", "open": false, "active": false},
            {"id": wire(open), "name": "window 2", "open": true, "active": true, "autoFollowMs": 30000,
             "sidebarVisible": false},
        ]})
    );
}

#[test]
fn with_every_window_closed_none_is_active() {
    let mut library = WindowLibrary::new("/home/u");
    let window_id = library.new_window(None);
    library.close_window(window_id);
    let mut core = core_over(library, AppSettings::default());
    assert_eq!(
        windows(&mut core),
        json!({"windows": [{"id": wire(window_id), "name": "window 1", "open": false, "active": false}]})
    );
    let mut empty = core_over(WindowLibrary::new("/home/u"), AppSettings::default());
    assert_eq!(windows(&mut empty), json!({"windows": []}));
}

// MARK: - new

#[test]
fn new_opens_a_frontmost_window_with_its_own_workspace_and_session() {
    let (mut core, ids) = with_windows(1);
    let response = window(
        &mut core,
        WindowCmd::New,
        None,
        args(|a| a.name = Some("  second  ".to_owned())),
    );
    let second = answered_id(&response).unwrap();
    assert_eq!(response.result, Some(json!({"id": wire(second)})));
    assert_eq!(core.library().windows()[1].name, "second", "trimmed");
    assert_eq!(active(&core), Some(second));
    let store = core.library().store(second).unwrap();
    assert_eq!(store.workspaces().len(), 1);
    assert_eq!(store.sessions().count(), 1);
    let first_workspace = core.library().store(ids[0]).unwrap().workspaces()[0].id();
    assert_ne!(store.workspaces()[0].id(), first_workspace);
    assert!(
        core.take_window_requests().is_empty(),
        "a new window is presented on build"
    );

    let unnamed = answered_id(&window(&mut core, WindowCmd::New, None, Args::default())).unwrap();
    assert_eq!(core.library().windows()[2].name, "window 3");
    assert_eq!(active(&core), Some(unnamed));
}

#[test]
fn new_minimized_parks_it_and_leaves_frontmost_on_a_window_in_view() {
    let (mut core, ids) = with_windows(1);
    let response = window(&mut core, WindowCmd::New, None, args(|a| a.minimized = Some(true)));
    let parked = answered_id(&response).unwrap();
    assert!(core.library().is_open(parked));
    assert_eq!(
        core.take_window_requests(),
        [WindowRequest::Minimize {
            window: parked,
            minimized: true
        }]
    );
    assert_eq!(active(&core), Some(ids[0]));
}

#[test]
fn new_minimized_as_the_only_open_window_keeps_frontmost() {
    let mut library = WindowLibrary::new("/home/u");
    let closed = library.new_window(None);
    library.close_window(closed);
    let mut core = core_over(library, AppSettings::default());
    let parked = answered_id(&window(
        &mut core,
        WindowCmd::New,
        None,
        args(|a| a.minimized = Some(true)),
    ))
    .unwrap();
    assert_eq!(active(&core), Some(parked), "nothing else is in view");
}

// MARK: - select and go

#[test]
fn select_raises_an_open_window_and_makes_it_frontmost() {
    let (mut core, ids) = with_windows(2);
    let prefix = &wire(ids[0])[..8];
    let response = window(&mut core, WindowCmd::Select, Some(prefix), Args::default());
    assert_eq!(answered_id(&response), Some(ids[0]));
    assert_eq!(active(&core), Some(ids[0]));
    assert_eq!(core.take_window_requests(), [WindowRequest::Raise(ids[0])]);
    assert_eq!(windows(&mut core)["windows"][0]["active"], true);
}

#[test]
fn select_opens_a_closed_window() {
    let (mut core, ids) = with_windows(2);
    core.close_window(ids[1]);
    assert!(!core.library().is_open(ids[1]));
    let response = window(&mut core, WindowCmd::Select, Some(&wire(ids[1])), Args::default());
    assert_eq!(answered_id(&response), Some(ids[1]));
    assert!(core.library().is_open(ids[1]));
    assert_eq!(active(&core), Some(ids[1]));
}

#[test]
fn go_steps_the_open_windows_wrapping_and_skips_closed_ones() {
    let (mut core, ids) = with_windows(3);
    core.close_window(ids[1]);
    let _ = core.take_window_requests();
    let to = |direction: &str| args(|a| a.to = Some(direction.to_owned()));
    assert_eq!(active(&core), Some(ids[2]));
    assert_eq!(
        answered_id(&window(&mut core, WindowCmd::Go, None, to("next"))),
        Some(ids[0])
    );
    assert_eq!(active(&core), Some(ids[0]));
    assert_eq!(
        answered_id(&window(&mut core, WindowCmd::Go, None, to("prev"))),
        Some(ids[2])
    );
    assert_eq!(
        answered_id(&window(&mut core, WindowCmd::Go, None, to("previous"))),
        Some(ids[0])
    );
    assert_eq!(
        core.take_window_requests(),
        [
            WindowRequest::Raise(ids[0]),
            WindowRequest::Raise(ids[2]),
            WindowRequest::Raise(ids[0])
        ]
    );
}

#[test]
fn go_needs_a_direction_and_another_open_window() {
    let (mut core, ids) = with_windows(2);
    for bad in [None, Some("up")] {
        assert_eq!(
            window(&mut core, WindowCmd::Go, None, args(|a| a.to = bad.map(str::to_owned))),
            failure("window.go requires --to next|prev")
        );
    }
    core.close_window(ids[0]);
    assert_eq!(
        window(&mut core, WindowCmd::Go, None, args(|a| a.to = Some("next".to_owned()))),
        failure("no other open window to navigate to")
    );
}

// MARK: - close, rename, delete

#[test]
fn close_keeps_the_window_listed_and_answers_ok_when_already_closed() {
    let (mut core, ids) = with_windows(2);
    core.set_window_state(ids[1], Some(WindowState::default()));
    for _ in 0..2 {
        let response = window(&mut core, WindowCmd::Close, Some(&wire(ids[1])), Args::default());
        assert_eq!(answered_id(&response), Some(ids[1]));
    }
    assert!(!core.library().is_open(ids[1]));
    assert_eq!(core.window_state(ids[1]), None, "its live state is withdrawn");
    assert_eq!(core.library().windows().len(), 2);
    assert_eq!(active(&core), Some(ids[0]));
}

#[test]
fn rename_trims_the_name_and_needs_one() {
    let (mut core, ids) = with_windows(1);
    let response = window(
        &mut core,
        WindowCmd::Rename,
        None,
        args(|a| a.name = Some("  work  ".to_owned())),
    );
    assert_eq!(answered_id(&response), Some(ids[0]));
    assert_eq!(core.library().windows()[0].name, "work");
    for name in [None, Some("  ")] {
        assert_eq!(
            window(
                &mut core,
                WindowCmd::Rename,
                None,
                args(|a| a.name = name.map(str::to_owned))
            ),
            failure("window.rename requires a name")
        );
    }
}

#[test]
fn delete_removes_a_window_but_never_the_last() {
    let (mut core, ids) = with_windows(2);
    let response = window(&mut core, WindowCmd::Delete, Some(&wire(ids[1])), Args::default());
    assert_eq!(answered_id(&response), Some(ids[1]));
    assert_eq!(core.library().windows().len(), 1);
    assert_eq!(active(&core), Some(ids[0]));
    assert_eq!(
        window(&mut core, WindowCmd::Delete, None, Args::default()),
        failure("cannot delete last window")
    );
    assert_eq!(core.library().windows().len(), 1);
}

// MARK: - resize, move, zoom, fullscreen

#[test]
fn resize_echoes_the_size_clamped_between_the_minimum_and_the_display() {
    let (mut core, ids) = with_windows(1);
    let size = |width, height| args(|a| (a.width, a.height) = (Some(width), Some(height)));
    let response = window(&mut core, WindowCmd::Resize, None, size(900, 600));
    assert_eq!(
        response.result,
        Some(json!({"id": wire(ids[0]), "width": 900, "height": 600}))
    );
    core.set_window_state(
        ids[0],
        Some(WindowState {
            display: Some(WindowSize {
                width: 1920,
                height: 1080,
            }),
            ..WindowState::default()
        }),
    );
    let response = window(&mut core, WindowCmd::Resize, None, size(5000, 100));
    assert_eq!(
        response.result,
        Some(json!({"id": wire(ids[0]), "width": 1920, "height": 400}))
    );
    assert_eq!(
        core.take_window_requests(),
        [
            WindowRequest::Resize {
                window: ids[0],
                size: WindowSize {
                    width: 900,
                    height: 600
                }
            },
            WindowRequest::Resize {
                window: ids[0],
                size: WindowSize {
                    width: 1920,
                    height: 400
                }
            },
        ]
    );
}

#[test]
fn resize_needs_a_positive_width_and_height() {
    let (mut core, _) = with_windows(1);
    for (width, height) in [
        (Some(900), None),
        (Some(0), Some(600)),
        (Some(900), Some(-1)),
        (None, None),
    ] {
        assert_eq!(
            window(
                &mut core,
                WindowCmd::Resize,
                None,
                args(|a| (a.width, a.height) = (width, height))
            ),
            failure("window.resize requires positive width and height")
        );
    }
    assert!(core.take_window_requests().is_empty());
}

#[test]
fn move_needs_x_and_y_and_a_host_that_places_windows() {
    let (mut core, ids) = with_windows(1);
    assert_eq!(
        window(&mut core, WindowCmd::Move, None, args(|a| a.x = Some(40))),
        failure("window.move requires x and y")
    );
    let at = args(|a| (a.x, a.y) = (Some(40), Some(60)));
    assert_eq!(
        window(&mut core, WindowCmd::Move, Some(UNKNOWN), at.clone()),
        failure("no such window: zzz")
    );
    assert_eq!(
        window(&mut core, WindowCmd::Move, Some(&wire(ids[0])), at),
        failure("window.move is not supported on this platform (the compositor controls window position)")
    );
}

#[test]
fn move_on_a_host_with_window_geometry_asks_the_frontend() {
    let mut library = WindowLibrary::new("/home/u");
    let first = library.new_window(None);
    let second = library.new_window(None);
    let mut core = Core::new(
        CoreConfig {
            app: AppIdentity::from_recorded("0.1.0", None),
            capabilities: vec!["window-geometry".to_owned()],
        },
        CoreState {
            library,
            ..CoreState::default()
        },
    );
    core.close_window(first);
    let at = args(|a| (a.x, a.y, a.display) = (Some(40), Some(60), Some(1)));
    assert_eq!(
        window(&mut core, WindowCmd::Move, Some(&wire(first)), at.clone()),
        failure(NOT_OPEN)
    );
    assert_eq!(answered_id(&window(&mut core, WindowCmd::Move, None, at)), Some(second));
    assert_eq!(
        core.take_window_requests(),
        [WindowRequest::Move {
            window: second,
            x: 40,
            y: 60,
            display: Some(1)
        }]
    );
}

#[test]
fn zoom_and_fullscreen_toggle_an_open_window() {
    let (mut core, ids) = with_windows(1);
    assert_eq!(
        answered_id(&window(&mut core, WindowCmd::Zoom, None, Args::default())),
        Some(ids[0])
    );
    assert_eq!(
        answered_id(&window(&mut core, WindowCmd::Fullscreen, None, Args::default())),
        Some(ids[0])
    );
    assert_eq!(
        core.take_window_requests(),
        [
            WindowRequest::ToggleZoom(ids[0]),
            WindowRequest::ToggleFullscreen(ids[0])
        ]
    );
}

#[test]
fn a_closed_window_refuses_the_frame_commands_and_an_unknown_one_every_command() {
    let (mut core, ids) = with_windows(2);
    core.close_window(ids[1]);
    let closed = wire(ids[1]);
    let frame_commands = [
        (
            WindowCmd::Resize,
            args(|a| (a.width, a.height) = (Some(900), Some(600))),
        ),
        (WindowCmd::Zoom, Args::default()),
        (WindowCmd::Fullscreen, Args::default()),
        (WindowCmd::Minimize, args(|a| a.mode = Some("on".to_owned()))),
    ];
    for (cmd, args) in &frame_commands {
        assert_eq!(
            window(&mut core, *cmd, Some(&closed), args.clone()),
            failure(NOT_OPEN),
            "{cmd:?}"
        );
    }
    let unknown = [
        (WindowCmd::Select, Args::default()),
        (WindowCmd::Close, Args::default()),
        (WindowCmd::Rename, args(|a| a.name = Some("x".to_owned()))),
        (WindowCmd::Delete, Args::default()),
    ];
    for (cmd, args) in unknown.iter().chain(&frame_commands) {
        assert_eq!(
            window(&mut core, *cmd, Some(UNKNOWN), args.clone()),
            failure("no such window: zzz"),
            "{cmd:?}"
        );
    }
    assert!(core.take_window_requests().is_empty());
    for cmd in [Command::Tree, Command::Workspace(WorkspaceCmd::New)] {
        assert_eq!(
            send(&mut core, cmd, None, args(|a| a.window = Some(closed.clone()))),
            failure(NOT_OPEN)
        );
    }
}

// MARK: - minimize

#[test]
fn minimize_refuses_an_unknown_mode() {
    let (mut core, _) = with_windows(1);
    assert_eq!(
        window(
            &mut core,
            WindowCmd::Minimize,
            None,
            args(|a| a.mode = Some("pin".to_owned()))
        ),
        failure("invalid window minimize mode: pin")
    );
}

#[test]
fn minimize_without_a_read_back_asks_every_time_and_hands_frontmost_off() {
    let (mut core, ids) = with_windows(2);
    let mode = |mode: &str| args(|a| a.mode = Some(mode.to_owned()));
    let second = Some(wire(ids[1]));
    for mode_word in ["on", "toggle", "off"] {
        let response = window(&mut core, WindowCmd::Minimize, second.as_deref(), mode(mode_word));
        assert_eq!(answered_id(&response), Some(ids[1]));
    }
    assert_eq!(
        core.take_window_requests(),
        [
            WindowRequest::Minimize {
                window: ids[1],
                minimized: true
            },
            WindowRequest::Minimize {
                window: ids[1],
                minimized: true
            },
            WindowRequest::Minimize {
                window: ids[1],
                minimized: false
            },
        ]
    );
    assert_eq!(active(&core), Some(ids[0]), "the parked frontmost window handed off");
}

#[test]
fn minimize_with_a_read_back_is_idempotent_and_refuses_full_screen() {
    let (mut core, ids) = with_windows(2);
    let minimized = |minimized, fullscreen| {
        Some(WindowState {
            minimized: Some(minimized),
            fullscreen,
            ..WindowState::default()
        })
    };
    core.set_window_state(ids[1], minimized(true, false));
    let mode = |mode: &str| args(|a| a.mode = Some(mode.to_owned()));
    assert!(window(&mut core, WindowCmd::Minimize, None, mode("on")).ok);
    assert!(core.take_window_requests().is_empty(), "already minimized");
    assert_eq!(active(&core), Some(ids[0]), "frontmost still handed off");

    core.set_window_state(ids[0], minimized(false, true));
    assert_eq!(
        window(&mut core, WindowCmd::Minimize, None, mode("on")),
        failure("cannot minimize a full-screen window — window.fullscreen it first")
    );
    assert!(
        window(&mut core, WindowCmd::Minimize, None, mode("off")).ok,
        "nothing to restore"
    );
    assert!(core.take_window_requests().is_empty());
}

#[test]
fn minimize_without_a_read_back_restores_a_full_screen_window_as_a_no_op() {
    let (mut core, ids) = with_windows(1);
    core.set_window_state(
        ids[0],
        Some(WindowState {
            fullscreen: true,
            minimized: None,
            ..WindowState::default()
        }),
    );
    let mode = |mode: &str| args(|a| a.mode = Some(mode.to_owned()));
    let response = window(&mut core, WindowCmd::Minimize, None, mode("off"));
    assert_eq!(answered_id(&response), Some(ids[0]));
    assert!(
        core.take_window_requests().is_empty(),
        "a full-screen window is not minimized"
    );
    for refused in ["on", "toggle"] {
        assert_eq!(
            window(&mut core, WindowCmd::Minimize, None, mode(refused)),
            failure("cannot minimize a full-screen window — window.fullscreen it first"),
            "{refused}"
        );
    }
    assert!(core.take_window_requests().is_empty());
}

#[test]
fn a_hand_off_skips_windows_read_back_as_minimized() {
    let (mut core, ids) = with_windows(3);
    core.set_window_state(
        ids[0],
        Some(WindowState {
            minimized: Some(true),
            ..WindowState::default()
        }),
    );
    assert!(
        window(
            &mut core,
            WindowCmd::Minimize,
            None,
            args(|a| a.mode = Some("on".to_owned()))
        )
        .ok
    );
    assert_eq!(active(&core), Some(ids[1]));
}

// MARK: - frontmost

#[test]
fn reporting_a_window_frontmost_moves_active_and_reconciles_auto_hidden_sidebars() {
    let settings = AppSettings {
        auto_hide_sidebar_inactive_windows: Some(true),
        ..AppSettings::default()
    };
    let mut library = WindowLibrary::new("/home/u");
    let first = library.new_window(None);
    let second = library.new_window(None);
    let mut core = core_over(library, settings);
    core.report_frontmost(first);
    assert_eq!(active(&core), Some(first));
    let visible = |core: &Core, id| core.library().store(id).unwrap().sidebar().visible;
    assert!(visible(&core, first));
    assert!(!visible(&core, second));
    core.close_window(second);
    core.report_frontmost(second);
    assert_eq!(
        active(&core),
        Some(first),
        "a closed window is never reported frontmost"
    );
}

#[test]
fn select_reconciles_auto_hidden_sidebars_only_when_frontmost_changes() {
    let settings = AppSettings {
        auto_hide_sidebar_inactive_windows: Some(true),
        ..AppSettings::default()
    };
    let mut library = WindowLibrary::new("/home/u");
    let first = library.new_window(None);
    let second = library.new_window(None);
    let mut core = core_over(library, settings);
    assert!(window(&mut core, WindowCmd::Select, Some(&wire(first)), Args::default()).ok);
    let visible = |core: &Core, id| core.library().store(id).unwrap().sidebar().visible;
    assert!(visible(&core, first) && !visible(&core, second));
    core.library_mut().store_mut(second).unwrap().set_sidebar_visible(true);
    assert!(window(&mut core, WindowCmd::Select, Some(&wire(first)), Args::default()).ok);
    assert!(visible(&core, second), "an unchanged frontmost reconciles nothing");
}

// MARK: - persistence

/// A core over a bootstrapped state dir.
fn persisted(dir: &std::path::Path) -> Core {
    let mut persistence = LibraryPersistence::new(dir);
    let library = persistence.bootstrap("/home/u");
    Core::new(
        CoreConfig {
            app: AppIdentity::from_recorded("0.1.0", None),
            capabilities: Vec::new(),
        },
        CoreState {
            library,
            persistence: Some(persistence),
            ..CoreState::default()
        },
    )
}

/// The index's entries as (id, open) and its frontmost id.
fn index(dir: &std::path::Path) -> (Vec<(String, bool)>, Option<String>) {
    let text = std::fs::read_to_string(dir.join("windows.json")).unwrap();
    let index: Value = serde_json::from_str(&text).unwrap();
    let entries = index["windows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            (
                entry["id"].as_str().unwrap().to_owned(),
                entry["isOpen"].as_bool().unwrap(),
            )
        })
        .collect();
    (entries, index["frontmost"].as_str().map(str::to_owned))
}

#[test]
fn the_window_lifecycle_lands_in_the_index_and_the_window_files() {
    let dir = tempfile::Builder::new().prefix("agt").tempdir_in("/tmp").unwrap();
    let mut core = persisted(dir.path());
    let first = active(&core).unwrap();
    let second = answered_id(&window(
        &mut core,
        WindowCmd::New,
        None,
        args(|a| a.name = Some("second".to_owned())),
    ))
    .unwrap();
    let _ = core.commit();
    assert_eq!(
        index(dir.path()),
        (vec![(wire(first), true), (wire(second), true)], Some(wire(second)))
    );

    let kept = args(|a| {
        a.name = Some("kept".to_owned());
        a.window = Some(wire(second));
    });
    assert!(send(&mut core, Command::Workspace(WorkspaceCmd::New), None, kept).ok);
    let _ = core.commit();
    assert!(window(&mut core, WindowCmd::Close, Some(&wire(second)), Args::default()).ok);
    let _ = core.commit();
    assert_eq!(
        index(dir.path()),
        (vec![(wire(first), true), (wire(second), false)], Some(wire(first)))
    );
    let file = dir.path().join("windows").join(format!("{}.json", wire(second)));
    assert!(
        std::fs::read_to_string(&file).unwrap().contains("kept"),
        "saved on close"
    );

    assert!(window(&mut core, WindowCmd::Select, Some(&wire(second)), Args::default()).ok);
    let _ = core.commit();
    let names: Vec<String> = core
        .library()
        .store(second)
        .unwrap()
        .workspaces()
        .iter()
        .map(|workspace| workspace.name.clone())
        .collect();
    assert!(names.contains(&"kept".to_owned()), "reopened from its file: {names:?}");
    assert_eq!(
        index(dir.path()),
        (vec![(wire(first), true), (wire(second), true)], Some(wire(second)))
    );

    assert!(window(&mut core, WindowCmd::Delete, Some(&wire(second)), Args::default()).ok);
    let _ = core.commit();
    assert!(!file.exists(), "a deleted window's file goes");
    assert_eq!(index(dir.path()).0, vec![(wire(first), true)]);
}

#[test]
fn closing_the_last_open_window_pins_it_frontmost_in_the_index() {
    let dir = tempfile::Builder::new().prefix("agt").tempdir_in("/tmp").unwrap();
    let mut core = persisted(dir.path());
    let only = active(&core).unwrap();
    assert!(window(&mut core, WindowCmd::Close, None, Args::default()).ok);
    let _ = core.commit();
    assert_eq!(index(dir.path()), (vec![(wire(only), false)], Some(wire(only))));
}
