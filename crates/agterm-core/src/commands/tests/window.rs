//! `window.list`.

use agterm_proto::command::WindowCmd;
use agterm_proto::{Args, Command};
use serde_json::{Value, json};

use super::{core_over, send, wire};
use crate::WindowState;
use crate::model::WindowLibrary;
use crate::persist::AppSettings;

fn windows(core: &mut crate::Core) -> Value {
    let response = send(core, Command::Window(WindowCmd::List), None, Args::default());
    assert!(response.ok, "{response:?}");
    response.result.unwrap_or_default()
}

/// The suite's `test_fresh_window_node`, with the fields the frontend reports.
#[test]
fn a_fresh_window_reports_its_live_state_once_the_frontend_does() {
    let mut library = WindowLibrary::new("/home/u");
    let window = library.new_window(None);
    let mut core = core_over(library, AppSettings::default());
    let id = wire(window);
    assert_eq!(
        windows(&mut core),
        json!({"windows": [{"id": id, "name": "window 1", "open": true, "active": true, "sidebarVisible": true}]})
    );
    core.set_window_state(
        window,
        Some(WindowState {
            minimized: true,
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
    let window = library.new_window(None);
    library.close_window(window);
    let mut core = core_over(library, AppSettings::default());
    assert_eq!(
        windows(&mut core),
        json!({"windows": [{"id": wire(window), "name": "window 1", "open": false, "active": false}]})
    );
    let mut empty = core_over(WindowLibrary::new("/home/u"), AppSettings::default());
    assert_eq!(windows(&mut empty), json!({"windows": []}));
}

#[test]
fn other_window_commands_are_still_refused() {
    let mut core = core_over(WindowLibrary::seeded("/home/u"), AppSettings::default());
    let response = send(&mut core, Command::Window(WindowCmd::Rename), None, Args::default());
    assert_eq!(
        response.error.as_deref(),
        Some("window.rename is not supported by this host")
    );
}
