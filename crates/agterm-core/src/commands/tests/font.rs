//! `font.inc`, `font.dec`, `font.reset` over fake terminals (upstream's `ControlDispatcherTests` font cases and
//! `ControlServer+SurfaceIO.font`), and the cell-size reports that persist the main pane's size.

use agterm_host::fakes::{FakeSurface, FakeSurfaceProbe};
use agterm_proto::command::FontCmd;
use agterm_proto::{Command, Response};
use uuid::Uuid;

use super::{Fixture, UNKNOWN, answered_id, args, failure, wire};

/// One window with two sessions, the second selected; the first one's main pane holds a fake terminal.
fn fixture() -> (Fixture, Uuid, FakeSurfaceProbe) {
    let mut fixture = Fixture::new(1);
    let id = fixture.sessions[0];
    let probe = install_main(&mut fixture, id, "L");
    (fixture, id, probe)
}

fn install_main(fixture: &mut Fixture, id: Uuid, token: &str) -> FakeSurfaceProbe {
    let surface = FakeSurface::new(token);
    let probe = surface.probe();
    let session = fixture.store_mut().and_then(|store| store.session_mut(id)).unwrap();
    session.surface.replace(Box::new(surface));
    probe
}

fn install_split(fixture: &mut Fixture, id: Uuid) -> FakeSurfaceProbe {
    let surface = FakeSurface::new("R");
    let probe = surface.probe();
    let store = fixture.store_mut().unwrap();
    store.set_split_visibility(id, true, None);
    store.session_mut(id).unwrap().split_surface.replace(Box::new(surface));
    probe
}

fn install_scratch(fixture: &mut Fixture, id: Uuid) -> FakeSurfaceProbe {
    let surface = FakeSurface::new("S");
    let probe = surface.probe();
    let session = fixture.store_mut().and_then(|store| store.session_mut(id)).unwrap();
    session.scratch_surface.replace(Box::new(surface));
    probe
}

fn font(fixture: &mut Fixture, cmd: FontCmd, target: Option<&str>, pane: Option<&str>) -> Response {
    fixture.send(Command::Font(cmd), target, args(|a| a.pane = pane.map(str::to_owned)))
}

#[test]
fn each_command_runs_its_binding_action_on_the_main_pane_and_names_the_session() {
    let (mut fixture, id, probe) = fixture();
    let target = wire(id);
    for cmd in [FontCmd::Inc, FontCmd::Dec, FontCmd::Reset] {
        assert_eq!(answered_id(&font(&mut fixture, cmd, Some(&target), None)), Some(id));
    }
    for pane in ["left", "primary", "top"] {
        assert_eq!(
            answered_id(&font(&mut fixture, FontCmd::Inc, Some(&target), Some(pane))),
            Some(id)
        );
    }
    assert_eq!(
        probe.actions(),
        [
            "increase_font_size:1",
            "decrease_font_size:1",
            "reset_font_size",
            "increase_font_size:1",
            "increase_font_size:1",
            "increase_font_size:1"
        ]
    );
}

#[test]
fn without_a_target_the_selected_session_alone_steps() {
    let (mut fixture, first, first_probe) = fixture();
    let selected = fixture.sessions[1];
    let selected_probe = install_main(&mut fixture, selected, "L2");
    assert_eq!(
        answered_id(&font(&mut fixture, FontCmd::Inc, None, None)),
        Some(selected)
    );
    assert_eq!(selected_probe.actions(), ["increase_font_size:1"]);
    assert!(first_probe.actions().is_empty(), "{first}");
}

#[test]
fn the_split_and_the_scratch_step_alone() {
    let (mut fixture, id, main) = fixture();
    let split = install_split(&mut fixture, id);
    let scratch = install_scratch(&mut fixture, id);
    let target = wire(id);
    for pane in ["right", "split", "bottom"] {
        assert!(font(&mut fixture, FontCmd::Inc, Some(&target), Some(pane)).ok);
    }
    assert!(font(&mut fixture, FontCmd::Dec, Some(&target), Some("scratch")).ok);
    assert_eq!(split.actions(), ["increase_font_size:1"; 3]);
    assert_eq!(scratch.actions(), ["decrease_font_size:1"]);
    assert!(main.actions().is_empty());
}

#[test]
fn a_promoted_split_survivor_is_the_main_pane() {
    let mut fixture = Fixture::new(0);
    let id = fixture.sessions[0];
    let split = install_split(&mut fixture, id);
    assert!(font(&mut fixture, FontCmd::Reset, Some(&wire(id)), Some("left")).ok);
    assert_eq!(split.actions(), ["reset_font_size"]);
}

#[test]
fn refusals_name_the_missing_pane_the_bad_pane_or_the_target() {
    let (mut fixture, id, probe) = fixture();
    let target = wire(id);
    for cmd in [FontCmd::Inc, FontCmd::Dec, FontCmd::Reset] {
        for (target, pane, error) in [
            (target.as_str(), Some("right"), "session has no split pane".to_owned()),
            (
                target.as_str(),
                Some("scratch"),
                "session has no scratch terminal".to_owned(),
            ),
            (target.as_str(), Some("middle"), "invalid pane: middle".to_owned()),
            (UNKNOWN, None, format!("no such session: {UNKNOWN}")),
        ] {
            assert_eq!(
                font(&mut fixture, cmd, Some(target), pane),
                failure(&error),
                "{cmd:?} {pane:?}"
            );
        }
    }
    assert!(probe.actions().is_empty());
}

#[test]
fn an_unrealized_or_missing_terminal_is_not_realized() {
    let (mut fixture, id, probe) = fixture();
    probe.set_realized(false);
    assert_eq!(
        font(&mut fixture, FontCmd::Inc, Some(&wire(id)), None),
        failure("session not realized")
    );
    let bare = fixture.sessions[1];
    assert_eq!(
        font(&mut fixture, FontCmd::Inc, Some(&wire(bare)), None),
        failure("session not realized")
    );
}

#[test]
fn only_the_main_role_persists_its_reported_size() {
    let (mut fixture, id, _) = fixture();
    install_split(&mut fixture, id);
    install_scratch(&mut fixture, id);
    let size = |fixture: &Fixture| fixture.store().session(id).unwrap().font_size;
    assert!(!fixture.core.report_font_size(id, "R", 15.0));
    assert!(!fixture.core.report_font_size(id, "S", 15.0));
    assert!(!fixture.core.report_font_size(id, "gone", 15.0));
    assert_eq!(size(&fixture), None);
    assert!(fixture.core.report_font_size(id, "L", 15.0));
    assert_eq!(size(&fixture), Some(15.0));
    // the same size again changes nothing, nor does a nonsense one
    assert!(!fixture.core.report_font_size(id, "L", 15.0));
    assert!(!fixture.core.report_font_size(id, "L", f64::NAN));
    assert!(!fixture.core.report_font_size(Uuid::new_v4(), "L", 12.0));
    assert_eq!(size(&fixture), Some(15.0));
}
