//! `session.background` and `session.restore` (upstream's dispatcher tests for both, `PaneBackgroundsTests` and the
//! `setSessionBackground`/`setSessionRestore` app side), with upstream's full error strings.

use agterm_host::fakes::FakeSurface;
use agterm_proto::command::SessionCmd;
use agterm_proto::{AppIdentity, Args, Command, Response};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{Fixture, UNKNOWN, answered_id, args, failure, send, wire};
use crate::model::{
    AppStore, BackgroundWatermark, SaveRequest, Session, StatusPane, WatermarkFit, WatermarkKind, WatermarkPosition,
};
use crate::persist::{LibraryPersistence, RestoreMode};
use crate::restore::mode::RestoreLaunchDecision;
use crate::{Core, CoreConfig, CoreState};

fn session(fixture: &Fixture, id: Uuid) -> &Session {
    fixture.store().session(id).expect("the session is in the tree")
}

fn install(fixture: &mut Fixture, id: Uuid, pane: StatusPane, token: &str) {
    let session = fixture.store_mut().and_then(|store| store.session_mut(id)).unwrap();
    session.pane_slot_mut(pane).replace(Box::new(FakeSurface::new(token)));
}

fn split_on(fixture: &mut Fixture, id: Uuid) {
    let response = fixture.session(SessionCmd::Split, Some(&wire(id)), args(|a| a.mode = Some("on".into())));
    assert_eq!(answered_id(&response), Some(id), "{response:?}");
    install(fixture, id, StatusPane::Right, "R");
}

/// A fixture whose first session's main pane holds terminal `L`.
fn fixture() -> (Fixture, Uuid) {
    let mut fixture = Fixture::new(0);
    let id = fixture.sessions[0];
    install(&mut fixture, id, StatusPane::Left, "L");
    let _ = fixture.store_mut().map(crate::model::AppStore::take_outbox);
    (fixture, id)
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

fn saved(fixture: &mut Fixture) -> SaveRequest {
    fixture
        .store_mut()
        .map(crate::model::AppStore::take_outbox)
        .map_or(SaveRequest::None, |outbox| outbox.save)
}

// MARK: - background

fn background(fixture: &mut Fixture, id: Uuid, edit: impl FnOnce(&mut Args)) -> Response {
    fixture.session(SessionCmd::Background, Some(&wire(id)), args(edit))
}

fn color(hex: &str) -> BackgroundWatermark {
    BackgroundWatermark {
        kind: WatermarkKind::Color,
        image_path: None,
        text: None,
        color_hex: Some(hex.to_owned()),
        opacity: None,
        fit: None,
        position: None,
        repeats: None,
    }
}

#[test]
fn a_default_and_a_pane_override_set_and_clear_independently() {
    let (mut fixture, id) = fixture();
    let response = background(&mut fixture, id, |a| {
        a.mode = Some("color".into());
        a.color = Some("#112233".into());
    });
    assert_eq!(answered_id(&response), Some(id));
    assert_eq!(saved(&mut fixture), SaveRequest::Now);
    assert_eq!(
        node(&mut fixture, id)["background"],
        json!({"kind": "color", "colorHex": "#112233"})
    );

    background(&mut fixture, id, |a| {
        a.mode = Some("text".into());
        a.text = Some("NOTES".into());
        a.opacity = Some(0.5);
        a.fit = Some("cover".into());
        a.position = Some("bottom-right".into());
        a.repeats = Some(true);
    });
    let expected = BackgroundWatermark {
        kind: WatermarkKind::Text,
        text: Some("NOTES".into()),
        color_hex: None,
        opacity: Some(0.5),
        fit: Some(WatermarkFit::Cover),
        position: Some(WatermarkPosition::BottomRight),
        ..color("#000000")
    };
    // a text takes no repeat
    assert_eq!(session(&fixture, id).background_watermark, Some(expected.clone()));

    background(&mut fixture, id, |a| {
        a.mode = Some("color".into());
        a.color = Some("#445566".into());
        a.pane = Some("left".into());
    });
    let tree = node(&mut fixture, id);
    assert_eq!(
        tree["paneBackgrounds"],
        json!({"left": {"kind": "color", "colorHex": "#445566"}})
    );
    assert_eq!(tree["background"]["kind"], json!("text"));
    assert_eq!(
        session(&fixture, id).effective_background(StatusPane::Left),
        Some(&color("#445566"))
    );

    background(&mut fixture, id, |a| {
        a.mode = Some("clear".into());
        a.pane = Some("left".into());
    });
    assert!(node(&mut fixture, id).get("paneBackgrounds").is_none());
    assert_eq!(
        session(&fixture, id).effective_background(StatusPane::Left),
        Some(&expected)
    );
    let _ = saved(&mut fixture);

    // an unchanged write saves nothing
    background(&mut fixture, id, |a| a.mode = Some("clear".into()));
    background(&mut fixture, id, |_| {});
    assert!(node(&mut fixture, id).get("background").is_none());
    let _ = saved(&mut fixture);
    background(&mut fixture, id, |a| a.mode = Some("clear".into()));
    assert_eq!(saved(&mut fixture), SaveRequest::None);
}

#[test]
fn an_image_is_checked_on_disk_before_the_target() -> std::io::Result<()> {
    let (mut fixture, id) = fixture();
    let dir = tempfile::tempdir()?;
    let png = dir.path().join("bg.png");
    std::fs::write(&png, b"png")?;
    let path = png.to_string_lossy().into_owned();
    let response = background(&mut fixture, id, |a| {
        a.mode = Some("image".into());
        a.path = Some(path.clone());
        a.opacity = Some(0.5);
        a.repeats = Some(true);
    });
    assert_eq!(answered_id(&response), Some(id));
    assert_eq!(
        node(&mut fixture, id)["background"],
        json!({"kind": "image", "imagePath": path, "opacity": 0.5, "repeats": true})
    );

    let gif = dir.path().join("bg.gif").to_string_lossy().into_owned();
    let missing = dir.path().join("none.jpg").to_string_lossy().into_owned();
    let cases = [
        (gif.clone(), format!("unsupported image (PNG or JPEG only): {gif}")),
        (missing.clone(), format!("no such image file: {missing}")),
    ];
    for (path, error) in cases {
        let response = fixture.session(
            SessionCmd::Background,
            Some(UNKNOWN),
            args(|a| {
                a.mode = Some("image".into());
                a.path = Some(path);
            }),
        );
        assert_eq!(response, failure(&error));
    }
    Ok(())
}

/// The `session.background` refusals with the arguments that reach them, in upstream's order of checks.
fn background_refusals() -> Vec<(Args, String)> {
    let mut cases = mode_refusals();
    cases.extend(pane_refusals());
    cases
}

/// The fit, position, opacity and mode checks.
fn mode_refusals() -> Vec<(Args, String)> {
    vec![
        (
            args(|a| a.mode = Some("paint".into())),
            "invalid background mode: paint (image|text|color|clear)".into(),
        ),
        (
            args(|a| a.mode = Some("color".into())),
            "session.background color requires a color".into(),
        ),
        (
            args(|a| {
                a.mode = Some("color".into());
                a.color = Some("blue".into());
            }),
            "invalid color: blue (#rrggbb)".into(),
        ),
        (
            args(|a| {
                a.mode = Some("text".into());
                a.text = Some("x".into());
                a.opacity = Some(2.0);
            }),
            "invalid opacity: 2.0 (0.0-1.0)".into(),
        ),
        (
            args(|a| {
                a.mode = Some("paint".into());
                a.fit = Some("squash".into());
                a.position = Some("middle".into());
            }),
            "invalid fit: squash (contain|cover|stretch|none)".into(),
        ),
        (
            args(|a| {
                a.mode = Some("paint".into());
                a.position = Some("middle".into());
                a.opacity = Some(-1.0);
            }),
            "invalid position: middle".into(),
        ),
        (
            args(|a| a.mode = Some("text".into())),
            "session.background text requires text".into(),
        ),
        (
            args(|a| {
                a.mode = Some("text".into());
                a.text = Some("x".repeat(257));
            }),
            "session.background text too long (max 256 characters)".into(),
        ),
        (
            args(|a| {
                a.mode = Some("text".into());
                a.text = Some("x".into());
                a.color = Some("#12345".into());
            }),
            "invalid color: #12345 (#rrggbb)".into(),
        ),
        (
            args(|a| a.mode = Some("image".into())),
            "session.background image requires a path".into(),
        ),
        (
            args(|a| {
                a.mode = Some("image".into());
                a.path = Some("x.png\nclipboard-read = allow".into());
            }),
            "image path must not contain control characters".into(),
        ),
    ]
}

/// The pane selector, then the pane's existence.
fn pane_refusals() -> Vec<(Args, String)> {
    vec![
        (
            args(|a| {
                a.mode = Some("clear".into());
                a.pane = Some("middle".into());
            }),
            "--pane must be left, right, or scratch".into(),
        ),
        (
            args(|a| {
                a.mode = Some("color".into());
                a.color = Some("#445566".into());
                a.pane = Some("right".into());
            }),
            "session has no split pane".into(),
        ),
        (
            args(|a| {
                a.mode = Some("color".into());
                a.color = Some("#445566".into());
                a.pane = Some("scratch".into());
            }),
            "session has no scratch terminal".into(),
        ),
    ]
}

#[test]
fn background_arguments_are_refused_in_upstreams_order() {
    let (mut fixture, id) = fixture();
    for (request, error) in background_refusals() {
        let response = fixture.session(SessionCmd::Background, Some(&wire(id)), request);
        assert_eq!(response, failure(&error));
    }
    let tree = node(&mut fixture, id);
    assert!(tree.get("background").is_none() && tree.get("paneBackgrounds").is_none());
    let response = fixture.session(
        SessionCmd::Background,
        Some(UNKNOWN),
        args(|a| a.mode = Some("clear".into())),
    );
    assert_eq!(response, failure(&format!("no such session: {UNKNOWN}")));
}

#[test]
fn split_and_scratch_overrides_follow_their_panes() {
    let (mut fixture, id) = fixture();
    split_on(&mut fixture, id);
    install(&mut fixture, id, StatusPane::Scratch, "S");
    for (pane, hex) in [("right", "#000001"), ("scratch", "#000002"), ("bottom", "#000003")] {
        let response = background(&mut fixture, id, |a| {
            a.mode = Some("color".into());
            a.color = Some(hex.into());
            a.pane = Some(pane.into());
        });
        assert_eq!(answered_id(&response), Some(id), "{pane}");
    }
    let session = session(&fixture, id);
    assert_eq!(session.pane_backgrounds.right, Some(color("#000003")));
    assert_eq!(session.pane_backgrounds.scratch, Some(color("#000002")));
    assert_eq!(session.wash_color_hex(StatusPane::Scratch), Some("#000002"));
    assert_eq!(session.wash_color_hex(StatusPane::Left), None);
    assert_eq!(
        session.background_file_key(StatusPane::Right),
        session.split_pane_identity.map(crate::pane_env::uuid_string)
    );
    assert_eq!(
        session.background_file_key(StatusPane::Scratch).as_deref(),
        Some("scratch")
    );
}

// MARK: - restore

fn restore(fixture: &mut Fixture, id: Uuid, edit: impl FnOnce(&mut Args)) -> Response {
    fixture.session(SessionCmd::Restore, Some(&wire(id)), args(edit))
}

fn result(response: &Response) -> Value {
    assert!(response.ok, "{response:?}");
    response.result.clone().unwrap_or_default()
}

#[test]
fn a_pin_reads_back_and_outside_rerun_carries_a_note() {
    let (mut fixture, id) = fixture();
    let response = restore(&mut fixture, id, |a| {
        a.mode = Some("set".into());
        a.command = Some("echo 'restored' | cat".into());
    });
    assert_eq!(
        result(&response),
        json!({
            "id": wire(id),
            "text": "saved for rerun mode; active restore mode is none",
            "pane": "left",
        })
    );
    assert_eq!(saved(&mut fixture), SaveRequest::Now);
    assert_eq!(node(&mut fixture, id)["restoreCommand"], json!("echo 'restored' | cat"));
    assert_eq!(session(&fixture, id).restore.pending_command, None);

    let response = restore(&mut fixture, id, |a| a.mode = Some("none".into()));
    assert!(result(&response)["text"].is_string());
    assert_eq!(node(&mut fixture, id)["restoreCommand"], json!(""));

    let response = restore(&mut fixture, id, |a| a.mode = Some("clear".into()));
    assert_eq!(result(&response), json!({"id": wire(id), "pane": "left"}));
    assert!(node(&mut fixture, id).get("restoreCommand").is_none());
}

#[test]
fn rerun_mode_takes_a_pin_without_a_note_and_live_falls_back_to_fresh_shells() {
    // the note follows the mode this launch runs in, not what the settings hold now
    let launched = |requested, reason: Option<&str>| {
        let mut fixture = Fixture::new(0);
        fixture
            .core
            .set_restore_launch(RestoreLaunchDecision::new(requested, reason.map(str::to_owned)));
        fixture
    };
    let mut fixture = launched(RestoreMode::Rerun, None);
    let id = fixture.sessions[0];
    let response = restore(&mut fixture, id, |a| {
        a.mode = Some("set".into());
        a.command = Some("htop".into());
    });
    assert_eq!(result(&response), json!({"id": wire(id), "pane": "left"}));

    let mut fixture = launched(RestoreMode::Live, Some("the live-session executable is unavailable"));
    let id = fixture.sessions[0];
    let response = restore(&mut fixture, id, |a| a.mode = Some("none".into()));
    assert_eq!(
        result(&response)["text"],
        json!("saved for rerun mode; active restore mode is none")
    );
}

#[test]
fn the_split_pin_is_its_own_and_a_token_finds_the_current_pane() {
    let (mut fixture, id) = fixture();
    split_on(&mut fixture, id);
    let response = restore(&mut fixture, id, |a| {
        a.mode = Some("set".into());
        a.pane = Some("right".into());
        a.command = Some("echo 'split' | cat".into());
    });
    assert_eq!(result(&response)["pane"], json!("right"));
    let tree = node(&mut fixture, id);
    assert_eq!(tree["splitRestoreCommand"], json!("echo 'split' | cat"));
    assert!(tree.get("restoreCommand").is_none());

    // a live token wins over the role beside it; an unknown one falls back to that role
    let response = restore(&mut fixture, id, |a| {
        a.mode = Some("none".into());
        a.pane = Some("left".into());
        a.pane_id = Some("R".into());
    });
    assert_eq!(result(&response)["pane"], json!("right"));
    assert_eq!(node(&mut fixture, id)["splitRestoreCommand"], json!(""));
    let response = restore(&mut fixture, id, |a| {
        a.mode = Some("none".into());
        a.pane = Some("left".into());
        a.pane_id = Some("gone".into());
    });
    assert_eq!(result(&response)["pane"], json!("left"));
    assert_eq!(node(&mut fixture, id)["restoreCommand"], json!(""));
}

#[test]
fn restore_arguments_are_refused_in_upstreams_order() {
    let (mut fixture, id) = fixture();
    install(&mut fixture, id, StatusPane::Scratch, "S");
    let cases: Vec<(Args, String)> = vec![
        (Args::default(), "invalid restore mode:  (set|none|clear)".into()),
        (
            args(|a| a.mode = Some("pin".into())),
            "invalid restore mode: pin (set|none|clear)".into(),
        ),
        (
            args(|a| a.mode = Some("set".into())),
            "session.restore set requires a command".into(),
        ),
        (
            args(|a| {
                a.mode = Some("set".into());
                a.command = Some("a\tb".into());
            }),
            "command must not contain control characters".into(),
        ),
        (
            args(|a| {
                a.mode = Some("set".into());
                a.command = Some("x".repeat(1025));
                a.pane = Some("middle".into());
            }),
            "command too long (max 1024 bytes)".into(),
        ),
        (
            args(|a| {
                a.mode = Some("none".into());
                a.pane = Some("middle".into());
            }),
            "--pane must be left, right, or scratch".into(),
        ),
        (
            args(|a| {
                a.mode = Some("none".into());
                a.pane = Some("right".into());
            }),
            "session has no split".into(),
        ),
        (
            args(|a| {
                a.mode = Some("none".into());
                a.pane = Some("scratch".into());
            }),
            "the scratch terminal is never restored".into(),
        ),
        (
            args(|a| {
                a.mode = Some("none".into());
                a.pane_id = Some("S".into());
            }),
            "the scratch terminal is never restored".into(),
        ),
        (
            args(|a| {
                a.mode = Some("none".into());
                a.pane_id = Some(UNKNOWN.into());
            }),
            format!("unknown pane id: {UNKNOWN}"),
        ),
    ];
    for (request, error) in cases {
        let response = fixture.session(SessionCmd::Restore, Some(&wire(id)), request);
        assert_eq!(response, failure(&error));
    }
    assert!(node(&mut fixture, id).get("restoreCommand").is_none());
    let response = fixture.session(
        SessionCmd::Restore,
        Some(UNKNOWN),
        args(|a| a.mode = Some("none".into())),
    );
    assert_eq!(response, failure(&format!("no such session: {UNKNOWN}")));
}

fn restore_in(core: &mut Core, id: Uuid, edit: impl FnOnce(&mut Args)) -> Response {
    send(core, Command::Session(SessionCmd::Restore), Some(&wire(id)), args(edit))
}

/// Upstream's `setRestoreCommand` and its `saveChecked`: a pin is acknowledged only once the window's file holds it,
/// and one whose write fails is refused with the previous pin put back.
#[test]
fn a_pin_is_written_at_once_and_one_that_cannot_be_written_is_refused_and_put_back() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let mut persistence = LibraryPersistence::new(dir.path());
    let library = persistence.bootstrap("/home/u");
    let mut core = Core::new(
        CoreConfig {
            app: AppIdentity::from_recorded("0.1.0", None),
            capabilities: Vec::new(),
        },
        CoreState {
            library,
            persistence: Some(persistence),
            ..CoreState::default()
        },
    );
    let _ = core.commit();
    let window = core.library().active_window_id().expect("a window");
    let id = core
        .library()
        .store(window)
        .and_then(AppStore::selected_session_id)
        .expect("a selected session");
    let pinned = |core: &Core| {
        core.library()
            .store(window)
            .and_then(|store| store.session(id))
            .and_then(|session| session.restore.command.clone())
    };

    let response = restore_in(&mut core, id, |a| {
        a.mode = Some("set".into());
        a.command = Some("echo one".into());
    });
    assert!(response.ok, "{response:?}");
    // on disk before any commit
    let file = dir.path().join(format!("windows/{}.json", wire(window)));
    let text = std::fs::read_to_string(&file).expect("the window file");
    assert!(text.contains("echo one"), "{text}");

    // a directory where the file goes makes every write of it fail
    std::fs::remove_file(&file).expect("the file goes");
    std::fs::create_dir_all(file.join("blocker")).expect("a blocking directory");
    let response = restore_in(&mut core, id, |a| a.mode = Some("clear".into()));
    assert_eq!(
        response,
        failure("failed to save the restore override, the previous value is still in effect")
    );
    assert_eq!(pinned(&core).as_deref(), Some("echo one"));
    // an unchanged pin writes nothing, so it is acknowledged even now
    let response = restore_in(&mut core, id, |a| {
        a.mode = Some("set".into());
        a.command = Some("echo one".into());
    });
    assert!(response.ok, "{response:?}");
}
