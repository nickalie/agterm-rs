//! `session.status`, `session.flag`, `session.context` and `session.seen` (upstream's
//! `ControlDispatcherSessionMetadataTests` and the app-side rules of `ControlServer+SessionActions.swift`).

use ngterm_host::fakes::FakeSurface;
use ngterm_proto::command::SessionCmd;
use ngterm_proto::{Args, EventKind, EventPayload, Response};
use serde_json::json;
use uuid::Uuid;

use super::{Fixture, UNKNOWN, answered_id, args, failure, wire};
use crate::model::{AgentStatus, StatusPane, StatusShape};
use crate::persist::AppSettings;

const SHAPES: &str = "circle|square|triangle|diamond|capsule|star";

fn status_args(status: &str, edit: impl FnOnce(&mut Args)) -> Args {
    args(|a| {
        a.status = Some(status.into());
        edit(a);
    })
}

/// `session.status` on `id`.
fn set_status(fixture: &mut Fixture, id: Uuid, status: &str, edit: impl FnOnce(&mut Args)) -> Response {
    fixture.session(SessionCmd::Status, Some(&wire(id)), status_args(status, edit))
}

/// The `status` events the window emitted so far, payloads only.
fn status_events(fixture: &Fixture) -> Vec<EventPayload> {
    fixture
        .store()
        .outbox()
        .events
        .iter()
        .filter(|event| event.kind == EventKind::Status)
        .map(|event| event.payload.clone())
        .collect()
}

fn status_of(fixture: &Fixture, id: Uuid) -> AgentStatus {
    fixture.store().session(id).unwrap().agent_indicator.status
}

// MARK: - session.status

#[test]
fn status_sets_a_fresh_indicator_and_emits_its_payload() {
    let mut fixture = Fixture::new(0);
    let id = fixture.sessions[0];
    let response = set_status(&mut fixture, id, "blocked", |a| {
        a.blink = Some(true);
        a.auto_reset = Some(true);
        a.color = Some("#ff8800".into());
        a.shape = Some("triangle".into());
    });
    assert_eq!(
        serde_json::to_value(&response).unwrap(),
        json!({"ok": true, "result": {"id": wire(id)}})
    );
    let session = fixture.store().session(id).unwrap();
    let indicator = &session.agent_indicator;
    assert_eq!(indicator.status, AgentStatus::Blocked);
    assert!(indicator.blink && indicator.auto_reset);
    assert_eq!(indicator.color.as_deref(), Some("#ff8800"));
    assert_eq!(indicator.shape, Some(StatusShape::Triangle));
    assert_eq!(indicator.status_pane, None);
    assert!(session.status_changed_at.is_some());
    let name = session.display_name();
    assert_eq!(
        status_events(&fixture),
        [EventPayload {
            name: Some(name),
            status: Some("blocked".into()),
            blink: Some(true),
            color: Some("#ff8800".into()),
            shape: Some("triangle".into()),
            previous: Some("idle".into()),
            ..EventPayload::default()
        }]
    );

    // the next call without the overrides discards them
    assert!(set_status(&mut fixture, id, "active", |_| {}).ok);
    let indicator = &fixture.store().session(id).unwrap().agent_indicator;
    assert_eq!(
        (indicator.blink, indicator.auto_reset, &indicator.color, indicator.shape),
        (false, false, &None, None)
    );
    let events = status_events(&fixture);
    assert_eq!(
        (
            events[1].status.as_deref(),
            events[1].previous.as_deref(),
            events[1].blink
        ),
        (Some("active"), Some("blocked"), Some(false))
    );
}

#[test]
fn a_repeated_status_restamps_without_an_event() {
    let mut fixture = Fixture::new(0);
    let id = fixture.sessions[0];
    assert!(set_status(&mut fixture, id, "active", |_| {}).ok);
    fixture.store_mut().unwrap().session_mut(id).unwrap().status_changed_at = Some(std::time::UNIX_EPOCH);
    assert!(set_status(&mut fixture, id, "active", |_| {}).ok);
    assert!(fixture.store().session(id).unwrap().status_changed_at > Some(std::time::UNIX_EPOCH));
    assert_eq!(status_events(&fixture).len(), 1);
}

#[test]
fn status_reads_back_in_the_tree_shape_included() {
    let mut fixture = Fixture::new(0);
    let id = fixture.sessions[0];
    assert!(set_status(&mut fixture, id, "active", |a| a.shape = Some("star".into())).ok);
    assert_eq!(
        set_status(&mut fixture, id, "blocked", |a| a.shape = Some("hexagon".into())),
        failure(&format!("invalid shape: hexagon ({SHAPES})"))
    );
    let tree = serde_json::to_value(fixture.core.tree(None).unwrap()).unwrap();
    let node = &tree["workspaces"][0]["sessions"][0];
    assert_eq!(
        (&node["status"], &node["statusShape"]),
        (&json!("active"), &json!("star"))
    );
}

#[test]
fn status_accepts_every_shape_and_a_shape_on_idle() {
    let mut fixture = Fixture::new(0);
    let id = fixture.sessions[0];
    for shape in StatusShape::ALL {
        assert!(
            set_status(&mut fixture, id, "blocked", |a| a.shape =
                Some(shape.wire_name().into()))
            .ok
        );
        assert_eq!(fixture.store().session(id).unwrap().agent_indicator.shape, Some(shape));
    }
    assert!(set_status(&mut fixture, id, "idle", |a| a.shape = Some("star".into())).ok);
    assert_eq!(status_of(&fixture, id), AgentStatus::Idle);
}

#[test]
fn status_refusals_follow_upstreams_order_and_leave_the_status() {
    let mut fixture = Fixture::new(0);
    let id = fixture.sessions[0];
    let shape_error = format!("invalid shape: hexagon ({SHAPES})");
    let cases: [(Args, &str); 7] = [
        (Args::default(), "invalid status"),
        (status_args("busy", |_| {}), "invalid status"),
        (
            status_args("blocked", |a| a.color = Some("nope".into())),
            "invalid color (expected #rrggbb)",
        ),
        (
            status_args("blocked", |a| {
                a.color = Some("nope".into());
                a.shape = Some("hexagon".into());
                a.pane = Some("middle".into());
            }),
            "invalid color (expected #rrggbb)",
        ),
        (
            status_args("blocked", |a| {
                a.shape = Some("hexagon".into());
                a.pane = Some("middle".into());
            }),
            &shape_error,
        ),
        (
            status_args("blocked", |a| a.pane = Some("middle".into())),
            "--pane must be left, right, or scratch",
        ),
        (
            status_args("blocked", |a| a.sound = Some("Gong".into())),
            "unknown sound: Gong (use 'default', 'beep', or one of: Basso, Blow, Bottle, Frog, Funk, Hero, Morse, \
             Ping, Pop, Purr, Sosumi, Submarine, Tink, Glass)",
        ),
    ];
    for (request, error) in cases {
        assert_eq!(
            fixture.session(SessionCmd::Status, Some(&wire(id)), request),
            failure(error)
        );
    }
    assert_eq!(status_of(&fixture, id), AgentStatus::Idle);
    assert!(status_events(&fixture).is_empty());
    assert!(fixture.core.take_status_sounds().is_empty());
}

#[test]
fn a_hex_color_without_the_hash_is_accepted_as_upstream_does() {
    let mut fixture = Fixture::new(0);
    let id = fixture.sessions[0];
    assert!(set_status(&mut fixture, id, "active", |a| a.color = Some("FF8800".into())).ok);
    assert_eq!(
        fixture.store().session(id).unwrap().agent_indicator.color.as_deref(),
        Some("FF8800")
    );
}

#[test]
fn an_unknown_sound_wins_over_an_unknown_target() {
    let mut fixture = Fixture::new(0);
    let response = fixture.session(
        SessionCmd::Status,
        Some(UNKNOWN),
        status_args("active", |a| a.sound = Some("Gong".into())),
    );
    assert!(response.error.unwrap().starts_with("unknown sound: Gong "));
    assert_eq!(
        fixture.session(SessionCmd::Status, Some(UNKNOWN), status_args("active", |_| {})),
        failure(&format!("no such session: {UNKNOWN}"))
    );
}

#[test]
fn status_records_the_pane_and_a_live_token_wins_over_the_role() {
    let mut fixture = Fixture::new(0);
    let id = fixture.sessions[0];
    assert!(set_status(&mut fixture, id, "completed", |a| a.pane = Some("scratch".into())).ok);
    let indicator = fixture.store().session(id).unwrap().agent_indicator.clone();
    assert_eq!(indicator.status_pane, Some(StatusPane::Scratch));
    let session = fixture.store_mut().unwrap().session_mut(id).unwrap();
    session.split.exists = true;
    session.surface.replace(Box::new(FakeSurface::new("main-token")));
    session.split_surface.replace(Box::new(FakeSurface::new("split-token")));
    assert!(
        set_status(&mut fixture, id, "completed", |a| {
            a.pane = Some("left".into());
            a.pane_id = Some("split-token".into());
        })
        .ok
    );
    assert_eq!(
        fixture.store().session(id).unwrap().agent_indicator.status_pane,
        Some(StatusPane::Right)
    );
    // an unknown token falls back to the role named beside it, and to none without one
    assert!(
        set_status(&mut fixture, id, "completed", |a| {
            a.pane = Some("top".into());
            a.pane_id = Some("gone".into());
        })
        .ok
    );
    assert_eq!(
        fixture.store().session(id).unwrap().agent_indicator.status_pane,
        Some(StatusPane::Left)
    );
    assert!(set_status(&mut fixture, id, "completed", |a| a.pane_id = Some("gone".into())).ok);
    assert_eq!(fixture.store().session(id).unwrap().agent_indicator.status_pane, None);
}

#[test]
fn another_panes_block_refuses_the_write_whole() {
    let mut fixture = Fixture::with_settings(
        0,
        AppSettings {
            blocked_status_sound_name: Some("Funk".into()),
            ..AppSettings::default()
        },
    );
    let id = fixture.sessions[0];
    fixture.store_mut().unwrap().session_mut(id).unwrap().split.exists = true;
    assert!(set_status(&mut fixture, id, "blocked", |a| a.pane = Some("left".into())).ok);
    assert_eq!(fixture.core.take_status_sounds(), ["Funk"]);
    let stamp = fixture.store().session(id).unwrap().status_changed_at;
    for status in ["active", "completed", "idle"] {
        let response = set_status(&mut fixture, id, status, |a| {
            a.pane = Some("right".into());
            a.sound = Some("Ping".into());
        });
        assert_eq!(
            response,
            failure("blocked status owned by pane left (write from that pane to change it)")
        );
    }
    let session = fixture.store().session(id).unwrap();
    assert_eq!(session.agent_indicator.status, AgentStatus::Blocked);
    assert_eq!(session.status_changed_at, stamp, "a refused write is not restamped");
    assert_eq!(status_events(&fixture).len(), 1, "a refused write emits nothing");
    assert!(fixture.core.take_status_sounds().is_empty(), "nor plays");

    // the other pane's own block replaces it; an untagged write is the main pane's
    assert!(set_status(&mut fixture, id, "blocked", |a| a.pane = Some("right".into())).ok);
    assert_eq!(
        set_status(&mut fixture, id, "idle", |_| {}),
        failure("blocked status owned by pane right (write from that pane to change it)")
    );
    assert!(set_status(&mut fixture, id, "idle", |a| a.pane = Some("right".into())).ok);
    assert_eq!(status_of(&fixture, id), AgentStatus::Idle);
}

#[test]
fn sounds_queue_the_per_call_one_else_the_blocked_default_on_a_transition_into_blocked() {
    let mut fixture = Fixture::with_settings(
        0,
        AppSettings {
            blocked_status_sound_name: Some("Funk".into()),
            ..AppSettings::default()
        },
    );
    let id = fixture.sessions[0];
    assert!(set_status(&mut fixture, id, "active", |_| {}).ok);
    assert!(
        fixture.core.take_status_sounds().is_empty(),
        "the default is for blocked only"
    );
    assert!(set_status(&mut fixture, id, "active", |a| a.sound = Some("beep".into())).ok);
    assert_eq!(fixture.core.take_status_sounds(), ["beep"]);
    assert!(set_status(&mut fixture, id, "blocked", |a| a.sound = Some(String::new())).ok);
    assert_eq!(
        fixture.core.take_status_sounds(),
        ["Funk"],
        "an empty sound counts as none"
    );
    assert!(set_status(&mut fixture, id, "blocked", |_| {}).ok);
    assert!(
        fixture.core.take_status_sounds().is_empty(),
        "a re-asserted block is no transition"
    );
    assert!(set_status(&mut fixture, id, "blocked", |a| a.sound = Some("Glass".into())).ok);
    assert_eq!(fixture.core.take_status_sounds(), ["Glass"]);
}

#[test]
fn without_a_configured_default_blocked_is_silent() {
    let mut fixture = Fixture::new(0);
    let id = fixture.sessions[0];
    assert!(set_status(&mut fixture, id, "blocked", |_| {}).ok);
    assert!(fixture.core.take_status_sounds().is_empty());
}

// MARK: - session.flag

#[test]
fn flag_modes_are_idempotent_and_toggle_by_default() {
    let mut fixture = Fixture::new(0);
    let id = fixture.sessions[0];
    for (mode, flagged) in [
        (None, true),
        (None, false),
        (Some("on"), true),
        (Some("on"), true),
        (Some("toggle"), false),
        (Some("off"), false),
    ] {
        let response = fixture.session(
            SessionCmd::Flag,
            Some(&wire(id)),
            args(|a| a.mode = mode.map(Into::into)),
        );
        assert_eq!(answered_id(&response), Some(id), "{mode:?}");
        assert_eq!(fixture.store().session(id).unwrap().flagged, flagged, "{mode:?}");
    }
}

#[test]
fn flag_clear_unflags_every_session_of_the_window() {
    let mut fixture = Fixture::new(2);
    for id in fixture.sessions.clone() {
        assert!(
            fixture
                .session(SessionCmd::Flag, Some(&wire(id)), args(|a| a.mode = Some("on".into())))
                .ok
        );
    }
    let response = fixture.session(SessionCmd::Flag, Some(UNKNOWN), args(|a| a.mode = Some("clear".into())));
    assert_eq!(
        serde_json::to_value(&response).unwrap(),
        json!({"ok": true}),
        "a bare ok, as upstream's ControlResponse(ok: true)"
    );
    assert!(fixture.store().flagged_sessions().is_empty());
}

#[test]
fn flag_resolves_the_target_before_refusing_the_mode() {
    let mut fixture = Fixture::new(0);
    let pin = || args(|a| a.mode = Some("pin".into()));
    assert_eq!(
        fixture.session(SessionCmd::Flag, Some("active"), pin()),
        failure("invalid flag mode: pin")
    );
    assert_eq!(
        fixture.session(SessionCmd::Flag, Some(UNKNOWN), pin()),
        failure(&format!("no such session: {UNKNOWN}"))
    );
}

// MARK: - session.context

#[test]
fn context_sets_trimmed_and_clears() {
    let mut fixture = Fixture::new(0);
    let id = fixture.sessions[0];
    let set = fixture.session(
        SessionCmd::Context,
        Some(&wire(id)),
        args(|a| {
            a.mode = Some("set".into());
            a.text = Some("  PR #517  ".into());
        }),
    );
    assert_eq!(answered_id(&set), Some(id));
    assert_eq!(fixture.store().session(id).unwrap().context.as_deref(), Some("PR #517"));
    let cleared = fixture.session(SessionCmd::Context, None, args(|a| a.mode = Some("clear".into())));
    assert_eq!(answered_id(&cleared), Some(id), "the active session by default");
    assert_eq!(fixture.store().session(id).unwrap().context, None);
}

#[test]
fn context_refusals_keep_the_previous_value() {
    let mut fixture = Fixture::new(0);
    let id = fixture.sessions[0];
    let context = |mode: Option<&str>, text: Option<&str>| {
        args(|a| {
            a.mode = mode.map(Into::into);
            a.text = text.map(Into::into);
        })
    };
    assert!(
        fixture
            .session(SessionCmd::Context, None, context(Some("set"), Some("kept")))
            .ok
    );
    let oversized = "x".repeat(257);
    let cases = [
        (context(None, None), "invalid context mode:  (set|clear)"),
        (context(None, Some("PR #517")), "invalid context mode:  (set|clear)"),
        (
            context(Some("toggle"), Some("PR")),
            "invalid context mode: toggle (set|clear)",
        ),
        (context(Some("set"), None), "session.context set requires text"),
        (
            context(Some("clear"), Some("PR")),
            "session.context clear takes no text",
        ),
        (
            context(Some("set"), Some("   ")),
            "context must not be empty (use --clear to remove it)",
        ),
        (
            context(Some("set"), Some("PR\n517")),
            "context must not contain control characters or line breaks",
        ),
        (
            context(Some("set"), Some(&oversized)),
            "context must be at most 256 UTF-8 bytes",
        ),
    ];
    for (request, error) in cases {
        assert_eq!(fixture.session(SessionCmd::Context, None, request), failure(error));
    }
    assert_eq!(fixture.store().session(id).unwrap().context.as_deref(), Some("kept"));
    // the value is checked before the target resolves
    assert_eq!(
        fixture.session(SessionCmd::Context, Some(UNKNOWN), context(Some("set"), Some(" "))),
        failure("context must not be empty (use --clear to remove it)")
    );
}

// MARK: - session.seen

#[test]
fn seen_clears_the_badge_without_selecting() {
    let mut fixture = Fixture::new(1);
    let (first, selected) = (fixture.sessions[0], fixture.sessions[1]);
    fixture.store_mut().unwrap().session_mut(first).unwrap().unseen_count = 2;
    let response = fixture.session(SessionCmd::Seen, Some(&wire(first)), Args::default());
    assert_eq!(answered_id(&response), Some(first));
    assert_eq!(fixture.store().session(first).unwrap().unseen_count, 0);
    assert_eq!(fixture.selected(), Some(selected));
}

#[test]
fn every_metadata_command_refuses_an_unknown_target() {
    let mut fixture = Fixture::new(0);
    let cases = [
        (SessionCmd::Flag, args(|a| a.mode = Some("on".into()))),
        (SessionCmd::Seen, Args::default()),
        (SessionCmd::Status, status_args("active", |_| {})),
        (
            SessionCmd::Context,
            args(|a| {
                a.mode = Some("set".into());
                a.text = Some("x".into());
            }),
        ),
    ];
    for (cmd, request) in cases {
        assert_eq!(
            fixture.session(cmd, Some(UNKNOWN), request),
            failure(&format!("no such session: {UNKNOWN}")),
            "{cmd:?}"
        );
    }
}
