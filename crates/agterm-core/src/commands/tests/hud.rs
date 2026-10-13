//! `session.hud.*` and `notify` (upstream's `ControlDispatcherHudTests`, `AppStoreHudTests`, the notify cases of
//! `ControlDispatcherTests`): full error strings, the slot's state transitions, the body file, the auto-hide.

use std::time::Duration;

use agterm_host::fakes::FakeSurface;
use agterm_proto::command::SessionCmd;
use agterm_proto::{Args, Command, EventKind, Response};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{Fixture, UNKNOWN, answered_id, args, failure, wire};
use crate::hud::{HudPaneFrame, HudPaneFrames};
use crate::persist::AppSettings;

/// A fixture with two sessions and the painter installed.
fn hud_fixture() -> (Fixture, tempfile::TempDir) {
    let mut fixture = Fixture::new(1);
    let dir = tempfile::tempdir().unwrap();
    let helper = dir.path().join("hud.sh");
    std::fs::write(&helper, "#!/bin/sh\n").unwrap();
    fixture.core.set_hud_helper(helper);
    (fixture, dir)
}

fn hud(fixture: &mut Fixture, cmd: SessionCmd, id: Uuid, edit: impl FnOnce(&mut Args)) -> Response {
    fixture.session(cmd, Some(&wire(id)), args(edit))
}

fn open(fixture: &mut Fixture, id: Uuid, message: &str) -> Response {
    hud(fixture, SessionCmd::HudOpen, id, |a| {
        a.message = Some(message.to_owned());
    })
}

fn node(fixture: &mut Fixture, id: Uuid) -> Value {
    let response = fixture.send(Command::Tree, None, Args::default());
    let tree = response.result.expect("a tree")["tree"].clone();
    tree["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|workspace| workspace["sessions"].as_array().cloned().unwrap_or_default())
        .find(|node| node["id"] == json!(wire(id)))
        .expect("the session's node")
}

fn body_file(fixture: &Fixture, id: Uuid) -> std::path::PathBuf {
    fixture
        .store()
        .session(id)
        .unwrap()
        .overlay
        .hud
        .as_ref()
        .unwrap()
        .file
        .clone()
}

#[test]
fn open_holds_the_slot_reads_back_and_close_frees_it() {
    let (mut fixture, _dir) = hud_fixture();
    let id = fixture.sessions[0];
    assert_eq!(answered_id(&open(&mut fixture, id, "gathering options")), Some(id));
    let read = node(&mut fixture, id);
    assert_eq!(read["overlay"], json!(false));
    assert!(read.get("overlaySizePercent").is_none());
    let panel = &read["hud"];
    assert_eq!(panel["message"], "gathering options");
    assert_eq!(panel["spinner"], "none");
    assert_eq!(panel["position"], "center");
    assert_eq!(panel["hideAfter"], json!(0.0));
    assert_eq!(
        (
            panel["markdown"].clone(),
            panel["sticky"].clone(),
            panel["frame"].clone()
        ),
        (json!(false), json!(false), json!(true))
    );
    // unmeasured: the width takes the cap, the height the floor
    assert_eq!(panel["sizePercent"], 80);
    assert_eq!(panel["heightPercent"], 10);
    for absent in ["detail", "backgroundColor", "textColor", "fontSize", "pane"] {
        assert!(panel.get(absent).is_none(), "{absent}");
    }
    assert_body_and_close(&mut fixture, id);
}

/// The open's body names the message and this process; a close takes the body and the HUD, once.
fn assert_body_and_close(fixture: &mut Fixture, id: Uuid) {
    let file = body_file(fixture, id);
    let body = std::fs::read_to_string(&file).unwrap();
    assert!(body.contains("gathering options"), "{body}");
    assert!(body.lines().next().unwrap().contains(&std::process::id().to_string()));

    assert_eq!(answered_id(&hud(fixture, SessionCmd::HudClose, id, |_| {})), Some(id));
    assert!(node(fixture, id).get("hud").is_none());
    assert!(!file.exists(), "the body file goes with the hud");
    assert_eq!(hud(fixture, SessionCmd::HudClose, id, |_| {}), failure("no hud"));
}

#[test]
fn every_option_reads_back_and_none_is_accepted_back() {
    let (mut fixture, _dir) = hud_fixture();
    let id = fixture.sessions[0];
    let response = hud(&mut fixture, SessionCmd::HudOpen, id, |a| {
        a.message = Some("**Deploy**\n\n- build\n- ship".to_owned());
        a.markdown = Some(true);
        a.detail = Some("step 2 of 3".to_owned());
        a.spinner = Some("braille".to_owned());
        a.position = Some("top".to_owned());
        a.color = Some("#102030".to_owned());
        a.text_color = Some("#405060".to_owned());
        a.size_percent = Some(50);
        a.font_size = Some(14.0);
        a.hide_after = Some(600.0);
        a.frame = Some(false);
    });
    assert_eq!(answered_id(&response), Some(id));
    let hud_node = node(&mut fixture, id)["hud"].clone();
    assert_eq!(hud_node["spinner"], "braille");
    assert_eq!(hud_node["position"], "top-center", "the alias reads back canonical");
    assert_eq!(hud_node["sizePercent"], 50);
    assert_eq!(hud_node["fontSize"], json!(14.0));
    assert_eq!(hud_node["frame"], json!(false));
    assert_eq!(hud_node["backgroundColor"], "#102030");
    let response = hud(&mut fixture, SessionCmd::HudOpen, id, |a| {
        a.message = Some("again".to_owned());
        a.spinner = Some("none".to_owned());
    });
    assert!(response.ok);
    assert_eq!(node(&mut fixture, id)["hud"]["spinner"], "none");
    assert_eq!(
        fixture.store().session(id).unwrap().overlay.generation,
        2,
        "a reopen replaces the panel"
    );
}

#[test]
fn widths_clamp_to_the_specs_bound() {
    let (mut fixture, _dir) = hud_fixture();
    let id = fixture.sessions[0];
    for (requested, sticky, position, width) in [
        (100, false, None, 80),
        (5, false, None, 10),
        (100, true, Some("top-left"), 100),
        (100, true, None, 80),
    ] {
        let response = hud(&mut fixture, SessionCmd::HudOpen, id, |a| {
            a.message = Some("m".to_owned());
            a.size_percent = Some(requested);
            a.sticky = Some(sticky);
            a.position = position.map(str::to_owned);
        });
        assert!(response.ok);
        assert_eq!(
            node(&mut fixture, id)["hud"]["sizePercent"],
            width,
            "{requested} {sticky}"
        );
    }
}

#[test]
fn an_update_replaces_the_spec_but_keeps_the_creation_fields() {
    let (mut fixture, _dir) = hud_fixture();
    let id = fixture.sessions[0];
    assert_eq!(
        hud(&mut fixture, SessionCmd::HudUpdate, id, |a| a.message =
            Some("x".into())),
        failure("no hud")
    );
    let response = hud(&mut fixture, SessionCmd::HudOpen, id, |a| {
        a.message = Some("one".to_owned());
        a.detail = Some("first".to_owned());
        a.color = Some("#102030".to_owned());
        a.font_size = Some(14.0);
    });
    assert!(response.ok);
    let generation = fixture.store().session(id).unwrap().overlay.generation;
    let response = hud(&mut fixture, SessionCmd::HudUpdate, id, |a| {
        a.message = Some("two".to_owned());
        a.color = Some("#ffffff".to_owned());
        a.position = Some("bottom-right".to_owned());
    });
    assert!(response.ok);
    let read = node(&mut fixture, id)["hud"].clone();
    assert_eq!(read["message"], "two");
    assert_eq!(read["backgroundColor"], "#102030");
    assert_eq!(read["fontSize"], json!(14.0));
    assert_eq!(read["position"], "bottom-right");
    assert!(read.get("detail").is_none());
    assert_eq!(
        fixture.store().session(id).unwrap().overlay.generation,
        generation,
        "the surface stays"
    );
    assert!(
        std::fs::read_to_string(body_file(&fixture, id))
            .unwrap()
            .contains("two")
    );
    assert_eq!(
        hud(&mut fixture, SessionCmd::HudUpdate, id, |a| {
            a.message = Some("four".into());
            a.font_size = Some(20.0);
        }),
        failure("session.hud.update: --font-size is fixed at open; reopen the hud to change it")
    );
}

/// `message` with `edit` applied.
fn with(message: Option<&str>, edit: impl FnOnce(&mut Args)) -> Args {
    args(|a| {
        a.message = message.map(str::to_owned);
        edit(a);
    })
}

/// Each case refused with its error, the session left without a HUD.
fn assert_refused(fixture: &mut Fixture, id: Uuid, cases: Vec<(Args, String)>) {
    for (case, error) in cases {
        assert_eq!(
            fixture.session(SessionCmd::HudOpen, Some(&wire(id)), case),
            failure(&error)
        );
        assert!(node(fixture, id).get("hud").is_none(), "{error}");
    }
}

#[test]
fn text_refusals_name_upstreams_errors() {
    let (mut fixture, _dir) = hud_fixture();
    let id = fixture.sessions[0];
    let markdown = |a: &mut Args| a.markdown = Some(true);
    let cases = vec![
        (with(None, |_| {}), "session.hud.open requires a message".to_owned()),
        (
            with(Some(" \t "), |_| {}),
            "session.hud.open requires a message".to_owned(),
        ),
        (
            with(Some("a\nb"), |_| {}),
            "hud text must not contain control characters".to_owned(),
        ),
        (
            with(Some("a\u{1b}[31m"), markdown),
            "hud text must not contain control characters".to_owned(),
        ),
        (
            with(Some("[ref]: /nowhere"), markdown),
            "session.hud.open requires a message".to_owned(),
        ),
        (
            with(Some(&"x".repeat(257)), |_| {}),
            "hud message too long (max 256 characters)".to_owned(),
        ),
        (
            with(Some("a"), |a| a.detail = Some("x".repeat(257))),
            "hud detail too long (max 256 characters)".to_owned(),
        ),
    ];
    assert_refused(&mut fixture, id, cases);
}

#[test]
fn option_refusals_name_upstreams_errors() {
    let (mut fixture, _dir) = hud_fixture();
    let id = fixture.sessions[0];
    let positions = "top-left|top-center|top-right|center-left|center|center-right|bottom-left|bottom-center|\
                     bottom-right|top|bottom";
    let cases = vec![
        (
            with(Some("a"), |a| a.color = Some("red".into())),
            "invalid color: red (#rrggbb)".to_owned(),
        ),
        (
            with(Some("a"), |a| a.size_percent = Some(0)),
            "session.hud.open: --size-percent must be 1...100".to_owned(),
        ),
        (
            with(Some("a"), |a| a.hide_after = Some(86_401.0)),
            "session.hud.open: --hide-after must be 0...86400 seconds".to_owned(),
        ),
        (
            with(Some("a"), |a| a.font_size = Some(5.0)),
            "session.hud.open: --font-size must be 6...72 points".to_owned(),
        ),
        (
            with(Some("a"), |a| a.position = Some("middle".into())),
            format!("invalid position: middle ({positions})"),
        ),
        (
            with(Some("a"), |a| a.spinner = Some("wheel".into())),
            "invalid spinner: wheel (bar|braille|circle|blocks|dot|none)".to_owned(),
        ),
        (
            with(Some("a"), |a| a.pane = Some("scratch".into())),
            "--pane must be left or right".to_owned(),
        ),
        (
            with(Some("a"), |a| a.pane = Some("right".into())),
            "pane not visible".to_owned(),
        ),
    ];
    assert_refused(&mut fixture, id, cases);
    for cmd in [SessionCmd::HudOpen, SessionCmd::HudUpdate, SessionCmd::HudClose] {
        let response = fixture.session(cmd, Some(UNKNOWN), args(|a| a.message = Some("a".into())));
        assert_eq!(response, failure(&format!("no such session: {UNKNOWN}")));
    }
}

#[test]
fn a_missing_painter_refuses_the_open() {
    let mut fixture = Fixture::new(0);
    let id = fixture.sessions[0];
    assert_eq!(
        open(&mut fixture, id, "m"),
        failure("hud helper is not bundled in this build")
    );
}

#[test]
fn a_hud_scopes_to_a_pane_by_role_or_id() {
    let (mut fixture, _dir) = hud_fixture();
    let id = fixture.sessions[0];
    assert!(open(&mut fixture, id, "m").ok);
    assert_eq!(
        hud(&mut fixture, SessionCmd::HudUpdate, id, |a| {
            a.message = Some("x".into());
            a.pane = Some("right".into());
        }),
        failure("session has no split")
    );
    let response = fixture.session(SessionCmd::Split, Some(&wire(id)), args(|a| a.mode = Some("on".into())));
    assert!(response.ok);
    let session = fixture.store_mut().unwrap().session_mut(id).unwrap();
    session.surface.replace(Box::new(FakeSurface::new("L")));
    session.split_surface.replace(Box::new(FakeSurface::new("R")));
    let response = hud(&mut fixture, SessionCmd::HudOpen, id, |a| {
        a.message = Some("m".into());
        a.pane = Some("right".into());
    });
    assert!(response.ok);
    assert_eq!(node(&mut fixture, id)["hud"]["pane"], "right");
    let response = hud(&mut fixture, SessionCmd::HudUpdate, id, |a| {
        a.message = Some("moved".into());
        a.pane_id = Some("L".into());
        a.pane = Some("right".into());
    });
    assert!(response.ok);
    assert_eq!(
        node(&mut fixture, id)["hud"]["pane"],
        "left",
        "a live id wins over the role"
    );
    assert!(
        hud(&mut fixture, SessionCmd::HudUpdate, id, |a| a.message =
            Some("all".into()))
        .ok
    );
    assert!(node(&mut fixture, id)["hud"].get("pane").is_none());
    assert_eq!(
        hud(&mut fixture, SessionCmd::HudUpdate, id, |a| {
            a.message = Some("lost".into());
            a.pane_id = Some(UNKNOWN.into());
        }),
        failure(&format!("unknown pane id: {UNKNOWN}"))
    );
}

#[test]
fn the_auto_hide_closes_unless_superseded() {
    let (mut fixture, _dir) = hud_fixture();
    let id = fixture.sessions[0];
    assert!(
        hud(&mut fixture, SessionCmd::HudOpen, id, |a| {
            a.message = Some("brief".into());
            a.hide_after = Some(2.0);
        })
        .ok
    );
    let timers = fixture.core.take_hud_timers();
    assert_eq!(timers.len(), 1);
    assert_eq!(timers[0].delay, Duration::from_secs(2));
    assert!(fixture.core.fire_hud_hide(timers[0]));
    assert!(node(&mut fixture, id).get("hud").is_none());

    assert!(
        hud(&mut fixture, SessionCmd::HudOpen, id, |a| {
            a.message = Some("long".into());
            a.hide_after = Some(600.0);
        })
        .ok
    );
    let stale = fixture.core.take_hud_timers()[0];
    // an update naming no interval cancels the one the open armed
    assert!(
        hud(&mut fixture, SessionCmd::HudUpdate, id, |a| a.message =
            Some("still".into()))
        .ok
    );
    assert!(fixture.core.take_hud_timers().is_empty());
    assert!(!fixture.core.fire_hud_hide(stale));
    assert_eq!(node(&mut fixture, id)["hud"]["hideAfter"], json!(0.0));
}

#[test]
fn reported_geometry_measures_the_panel() {
    let (mut fixture, _dir) = hud_fixture();
    let id = fixture.sessions[0];
    let surface = FakeSurface::new("L");
    surface.probe().set_cell_size(Some((8.0, 16.0)));
    fixture
        .store_mut()
        .unwrap()
        .session_mut(id)
        .unwrap()
        .surface
        .replace(Box::new(surface));
    let frames = HudPaneFrames {
        detail: Some(HudPaneFrame {
            x: 0.0,
            y: 0.0,
            width: 1000.0,
            height: 600.0,
        }),
        ..HudPaneFrames::default()
    };
    assert!(!fixture.core.report_hud_frames(id, &frames), "no hud: only cached");
    assert!(open(&mut fixture, id, "working").ok);
    let read = node(&mut fixture, id)["hud"].clone();
    // "working" is 7 cells + 4 padding = 11 cells of 8pt + 16pt padding over 1000pt: 11% (rounded up)
    assert_eq!(read["sizePercent"], 11);
    // 3 rows of 16pt + 12pt padding over 600pt: 10%
    assert_eq!(read["heightPercent"], 10);
    let wider = HudPaneFrames {
        detail: Some(HudPaneFrame {
            width: 500.0,
            ..frames.detail.unwrap()
        }),
        ..frames
    };
    assert!(fixture.core.report_hud_frames(id, &wider));
    assert_eq!(node(&mut fixture, id)["hud"]["sizePercent"], 21);
}

#[test]
fn the_slot_closes_with_its_session_and_its_painters_exit() {
    let (mut fixture, _dir) = hud_fixture();
    let id = fixture.sessions[0];
    assert!(open(&mut fixture, id, "m").ok);
    let generation = fixture.store().session(id).unwrap().overlay.generation;
    let surface = FakeSurface::new("");
    let probe = surface.probe();
    fixture
        .store_mut()
        .unwrap()
        .session_mut(id)
        .unwrap()
        .overlay
        .surface
        .replace(Box::new(surface));
    assert!(
        !fixture.core.overlay_exited(id, generation + 1),
        "a replaced generation's exit closes nothing"
    );
    assert!(fixture.core.overlay_exited(id, generation));
    assert_eq!(probe.teardowns(), 1);
    assert!(node(&mut fixture, id).get("hud").is_none());

    assert!(open(&mut fixture, id, "m").ok);
    let file = body_file(&fixture, id);
    assert!(fixture.session(SessionCmd::Close, Some(&wire(id)), Args::default()).ok);
    assert!(!file.exists(), "a session close discards the body");
}

// MARK: - notify

fn notify(fixture: &mut Fixture, target: &str, edit: impl FnOnce(&mut Args)) -> Response {
    fixture.send(Command::Notify, Some(target), args(edit))
}

#[test]
fn notify_raises_the_badge_emits_the_event_and_queues_a_banner() {
    let mut fixture = Fixture::new(1);
    let id = fixture.sessions[0];
    let name = fixture.store().session(id).unwrap().display_name();
    let response = notify(&mut fixture, &wire(id), |a| {
        a.title = Some("Build".into());
        a.body = Some("tests passed".into());
    });
    assert_eq!(answered_id(&response), Some(id));
    assert!(response.result.unwrap().get("text").is_none());
    assert!(notify(&mut fixture, &wire(id), |a| a.body = Some("second".into())).ok);
    assert_eq!(fixture.store().session(id).unwrap().unseen_count, 2);
    let events: Vec<_> = fixture
        .store()
        .outbox()
        .events
        .iter()
        .filter(|event| event.kind == EventKind::Notify)
        .map(|event| (event.session, event.payload.title.clone(), event.payload.body.clone()))
        .collect();
    assert_eq!(
        events,
        [
            (Some(id), Some("Build".to_owned()), Some("tests passed".to_owned())),
            (Some(id), Some(name.clone()), Some("second".to_owned())),
        ]
    );
    let posts = fixture.core.take_notifications();
    assert_eq!(posts.len(), 2);
    assert_eq!(posts[1].title, name);
    assert_eq!(
        posts[0].identity(),
        format!("{}:{}:main", wire(fixture.window), wire(id))
    );
}

#[test]
fn notify_refusals_post_nothing() {
    let mut fixture = Fixture::new(1);
    let id = fixture.sessions[0];
    assert_eq!(
        notify(&mut fixture, &wire(id), |_| {}),
        failure("notify requires a body")
    );
    assert_eq!(
        notify(&mut fixture, &wire(id), |a| {
            a.body = Some(String::new());
            a.title = Some("t".into());
        }),
        failure("notify requires a body")
    );
    assert_eq!(
        notify(&mut fixture, UNKNOWN, |a| a.body = Some("b".into())),
        failure(&format!("no such session: {UNKNOWN}"))
    );
    assert_eq!(fixture.store().session(id).unwrap().unseen_count, 0);
    assert!(fixture.core.take_notifications().is_empty());
}

#[test]
fn banners_off_still_badge_and_note_it() {
    let mut fixture = Fixture::with_settings(
        1,
        AppSettings {
            notifications_enabled: Some(false),
            ..AppSettings::default()
        },
    );
    let id = fixture.sessions[0];
    let response = notify(&mut fixture, &wire(id), |a| a.body = Some("b".into()));
    assert_eq!(
        response.result.unwrap()["text"],
        crate::commands::notify::BANNERS_OFF_NOTE
    );
    assert_eq!(fixture.store().session(id).unwrap().unseen_count, 1);
    assert!(fixture.core.take_notifications().is_empty());
}

#[test]
fn a_terminal_notification_names_its_pane_and_a_click_reveals_it() {
    let mut fixture = Fixture::new(1);
    let (first, second) = (fixture.sessions[0], fixture.sessions[1]);
    let response = fixture.session(
        SessionCmd::Split,
        Some(&wire(first)),
        args(|a| a.mode = Some("on".into())),
    );
    assert!(response.ok);
    let session = fixture.store_mut().unwrap().session_mut(first).unwrap();
    session.surface.replace(Box::new(FakeSurface::new("L")));
    session.split_surface.replace(Box::new(FakeSurface::new("R")));
    fixture.store_mut().unwrap().select_session(Some(second));
    assert!(fixture.core.terminal_notification(first, "R", "", "done"));
    let post = fixture.core.take_notifications().remove(0);
    assert_eq!(post.pane, crate::commands::notify::PaneRole::Split);
    fixture.store_mut().unwrap().set_split_focus(first, false);
    assert!(fixture.core.reveal_notification(post.window, first, post.pane));
    let store = fixture.store();
    assert_eq!(store.selected_session_id(), Some(first));
    assert_eq!(store.session(first).unwrap().unseen_count, 0);
    assert!(
        store.session(first).unwrap().split.focused,
        "the firing pane takes the keyboard"
    );
    assert!(fixture.core.pane_focus_requested(first));
    assert!(!fixture.core.reveal_notification(post.window, Uuid::new_v4(), post.pane));
}

#[test]
fn focusing_a_pane_clears_the_badge() {
    let mut fixture = Fixture::new(1);
    let id = fixture.sessions[0];
    fixture
        .store_mut()
        .unwrap()
        .session_mut(id)
        .unwrap()
        .surface
        .replace(Box::new(FakeSurface::new("L")));
    assert!(notify(&mut fixture, &wire(id), |a| a.body = Some("b".into())).ok);
    assert!(fixture.core.pane_focused(id, "L"));
    assert_eq!(fixture.store().session(id).unwrap().unseen_count, 0);
}
