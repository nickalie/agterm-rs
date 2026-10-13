//! `session.restart` (upstream's `dispatchSessionRestart` and `resolveRestartTarget`): every refusal up to the live
//! check, in upstream's order. Receipt and replay decisions are tested beside their types.

use agterm_host::fakes::FakeSurface;
use agterm_proto::command::SessionCmd;
use agterm_proto::{Args, Response};
use uuid::Uuid;

use super::{Fixture, UNKNOWN, args, failure, wire};
use crate::model::{NewSession, StatusPane};

const NOT_LIVE: &str = "session.restart needs Live sessions mode; this pane has no live shell to replace";
/// The main pane's token: a pane identity, as every session pane's is.
const L: &str = "0A0B0C0D-0000-4000-8000-00000000000A";

fn restart(fixture: &mut Fixture, id: Uuid, edit: impl FnOnce(&mut Args)) -> Response {
    fixture.session(SessionCmd::Restart, Some(&wire(id)), args(edit))
}

fn install(fixture: &mut Fixture, id: Uuid, pane: StatusPane, surface: FakeSurface) {
    let session = fixture.store_mut().and_then(|store| store.session_mut(id)).unwrap();
    session.pane_slot_mut(pane).replace(Box::new(surface));
}

/// A fixture whose first session's main pane holds terminal `L`.
fn fixture() -> (Fixture, Uuid) {
    let mut fixture = Fixture::new(0);
    let id = fixture.sessions[0];
    install(&mut fixture, id, StatusPane::Left, FakeSurface::new(L));
    (fixture, id)
}

fn pane(name: &str) -> impl FnOnce(&mut Args) {
    let name = name.to_owned();
    move |a: &mut Args| a.pane = Some(name)
}

#[test]
fn the_line_is_checked_before_the_pane() {
    let (mut fixture, id) = fixture();
    let cases: [(&str, String); 4] = [
        (
            " \t ",
            "session.restart takes a non-empty command, or none to replay".to_owned(),
        ),
        (
            "",
            "session.restart takes a non-empty command, or none to replay".to_owned(),
        ),
        ("a\tb", "command must not contain control characters".to_owned()),
        (&"x".repeat(4097), "command too long (max 4096 bytes)".to_owned()),
    ];
    for (command, error) in cases {
        let response = restart(&mut fixture, id, |a| {
            a.command = Some(command.to_owned());
            a.pane = Some("middle".to_owned());
        });
        assert_eq!(response, failure(&error), "{command:?}");
    }
    // a line break is not whitespace to the blank check, so it is refused as a control character
    let response = restart(&mut fixture, id, |a| {
        a.command = Some("\n".to_owned());
        a.pane = Some("left".to_owned());
    });
    assert_eq!(response, failure("command must not contain control characters"));
}

#[test]
fn a_pane_must_be_named_and_never_the_scratch() {
    let (mut fixture, id) = fixture();
    assert_eq!(
        restart(&mut fixture, id, pane("middle")),
        failure("--pane must be left, right, or scratch")
    );
    assert_eq!(
        restart(&mut fixture, id, pane("scratch")),
        failure("session.restart does not address the scratch pane")
    );
    assert_eq!(
        restart(&mut fixture, id, |a| a.pane_id = Some(String::new())),
        failure("session.restart requires --pane-id or --pane")
    );
    assert_eq!(
        restart(&mut fixture, id, |_| {}),
        failure("session.restart requires --pane-id or --pane")
    );
}

#[test]
fn the_target_resolves_after_the_arguments() {
    let (mut fixture, _) = fixture();
    let response = fixture.session(
        SessionCmd::Restart,
        Some(UNKNOWN),
        args(|a| a.pane = Some("left".into())),
    );
    assert_eq!(response, failure(&format!("no such session: {UNKNOWN}")));
}

#[test]
fn an_unknown_token_is_refused_even_beside_a_pane() {
    let (mut fixture, id) = fixture();
    let response = restart(&mut fixture, id, |a| {
        a.pane = Some("left".to_owned());
        a.pane_id = Some("nope".to_owned());
    });
    assert_eq!(response, failure("unknown pane id: nope"));
}

#[test]
fn a_missing_split_or_terminal_is_refused() {
    let (mut fixture, id) = fixture();
    assert_eq!(
        restart(&mut fixture, id, pane("right")),
        failure("session has no split pane")
    );
    let bare = fixture.add_session(fixture.workspace);
    assert_eq!(
        restart(&mut fixture, bare, pane("left")),
        failure("session not realized")
    );
}

#[test]
fn a_local_pane_without_a_live_shell_needs_live_sessions() {
    let (mut fixture, id) = fixture();
    assert_eq!(restart(&mut fixture, id, pane("primary")), failure(NOT_LIVE));
    let response = restart(&mut fixture, id, |a| {
        a.pane_id = Some(L.to_owned());
        a.command = Some("cld brief.md".to_owned());
    });
    assert_eq!(response, failure(NOT_LIVE));
    // a live token wins over the role beside it
    let response = restart(&mut fixture, id, |a| {
        a.pane_id = Some(L.to_owned());
        a.pane = Some("right".to_owned());
    });
    assert_eq!(response, failure(NOT_LIVE));
}

#[test]
fn a_live_pane_needs_this_instance_s_zmx() {
    let (mut fixture, id) = fixture();
    install(
        &mut fixture,
        id,
        StatusPane::Left,
        FakeSurface::new(L).with_live_session(true),
    );
    assert_eq!(restart(&mut fixture, id, pane("left")), failure(NOT_LIVE));
}

#[test]
fn a_pane_without_an_identity_is_not_realized() {
    let (mut fixture, id) = fixture();
    install(&mut fixture, id, StatusPane::Left, FakeSurface::new("L"));
    assert_eq!(restart(&mut fixture, id, pane("left")), failure("session not realized"));
}

#[test]
fn a_remote_session_is_refused_first() {
    let (mut fixture, _) = fixture();
    let workspace = fixture.workspace;
    let remote = fixture
        .store_mut()
        .and_then(|store| {
            store.add_session(
                workspace,
                NewSession {
                    remote_host: Some("box".to_owned()),
                    ..NewSession::at_cwd("/srv")
                },
            )
        })
        .unwrap();
    let response = restart(&mut fixture, remote, |a| a.pane_id = Some("nope".to_owned()));
    assert_eq!(
        response,
        failure("session.restart needs a local pane; this session runs on box")
    );
}
