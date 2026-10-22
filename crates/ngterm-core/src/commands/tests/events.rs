//! `events.read` through the dispatcher, fed by commits of real model mutations (upstream's
//! `ControlEventDispatcherTests` and the `AppStoreEventTests` that read the ring back).

use ngterm_proto::command::{EventsCmd, SessionCmd};
use ngterm_proto::{Args, Command, EventKind, Response};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{Fixture, args, failure, wire};
use crate::events::{DEFAULT_CAPACITY, EventRing};
use crate::model::WindowLibrary;
use crate::persist::AppSettings;

const RUN: Uuid = Uuid::from_u128(0xCBB5_E3D0_7A9B_4C96_9EA2_18B1_4380_DDB1);
const RUN_TEXT: &str = "CBB5E3D0-7A9B-4C96-9EA2-18B14380DDB1";

/// A fixture whose seeding is committed before its ring is replaced by an empty one with a known run and clock.
fn fixture(extra: usize) -> Fixture {
    let mut fixture = Fixture::new(extra);
    let _ = fixture.core.commit();
    let placeholder = super::core_over(WindowLibrary::default(), AppSettings::default());
    let core = std::mem::replace(&mut fixture.core, placeholder);
    fixture.core = core.with_event_ring(EventRing::new(DEFAULT_CAPACITY, RUN, Box::new(|| 10.5)));
    fixture
}

fn read(fixture: &mut Fixture, edit: impl FnOnce(&mut Args)) -> Response {
    fixture.send(Command::Events(EventsCmd::Read), None, args(edit))
}

fn from(after: u64) -> impl FnOnce(&mut Args) {
    move |args: &mut Args| {
        args.run = Some(RUN_TEXT.to_owned());
        args.after = Some(after.to_string());
    }
}

fn events_of(response: &Response) -> &Value {
    &response.result.as_ref().unwrap()["events"]
}

fn items(response: &Response) -> Vec<(String, Value, Value)> {
    assert!(response.ok, "{response:?}");
    events_of(response)["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| {
            (
                event["kind"].as_str().unwrap().to_owned(),
                event["session"].clone(),
                event["payload"].clone(),
            )
        })
        .collect()
}

#[test]
fn a_read_without_a_cursor_subscribes_from_now() {
    let mut fixture = fixture(0);
    let response = read(&mut fixture, |_| {});
    assert_eq!(
        serde_json::to_value(&response).unwrap(),
        json!({"ok": true, "result": {"events": {"run": RUN_TEXT, "next": 0, "items": []}}})
    );
    fixture.add_session(fixture.workspace);
    let _ = fixture.core.commit();
    let later = read(&mut fixture, |_| {});
    assert_eq!(
        events_of(&later)["next"],
        2,
        "created and selected, no history returned"
    );
    assert_eq!(events_of(&later)["items"], json!([]));
}

#[test]
fn a_commit_sequences_lifecycle_events_and_debounces_tree_changed() {
    let mut fixture = fixture(0);
    let first = fixture.sessions[0];
    let response = fixture.session(SessionCmd::New, None, Args::default());
    let added = super::answered_id(&response).unwrap();
    let timers = fixture.core.commit();
    assert_eq!(timers.tree_changed.len(), 1, "one debounce for the window");

    let page = read(&mut fixture, from(0));
    let name = fixture.store().session(added).unwrap().display_name();
    assert_eq!(
        items(&page),
        [
            ("session.created".to_owned(), json!(wire(added)), json!({"name": name})),
            (
                "session.selected".to_owned(),
                json!(wire(added)),
                json!({"name": name, "previous": wire(first)})
            ),
        ]
    );
    let created = &events_of(&page)["items"][0];
    assert_eq!(created["window"], json!(wire(fixture.window)));
    assert_eq!(created["workspace"], json!(wire(fixture.workspace)));
    assert_eq!(created["ts"], json!(10.5));

    for timer in timers.tree_changed {
        fixture.core.fire_tree_changed(timer);
    }
    let tree = read(&mut fixture, from(2));
    assert_eq!(
        serde_json::to_value(&events_of(&tree)["items"]).unwrap(),
        json!([{"seq": 3, "ts": 10.5, "kind": "tree.changed", "window": wire(fixture.window), "payload": {}}])
    );
}

#[test]
fn the_kinds_filter_and_the_page_limit_apply() {
    let mut fixture = fixture(0);
    let response = fixture.session(SessionCmd::New, None, Args::default());
    let added = super::answered_id(&response).unwrap();
    let _ = fixture.core.commit();
    fixture.session(SessionCmd::Close, Some(&wire(added)), Args::default());
    let _ = fixture.core.commit();

    let created_closed = |args: &mut Args| {
        from(0)(args);
        args.kinds = Some(vec![" session.created , session.closed".to_owned()]);
    };
    let whole = read(&mut fixture, created_closed);
    let kinds: Vec<String> = items(&whole).into_iter().map(|(kind, _, _)| kind).collect();
    assert_eq!(kinds, ["session.created", "session.closed"]);

    let page = read(&mut fixture, |args| {
        created_closed(args);
        args.limit = Some(1);
    });
    assert_eq!(events_of(&page)["next"], 1, "a full page stops at its last event");
    assert_eq!(items(&page).len(), 1);
    assert_eq!(EventKind::SessionCreated.wire_name(), items(&page)[0].0);
}

#[test]
fn a_cursor_failure_carries_the_anchor() {
    let mut fixture = fixture(0);
    fixture.add_session(fixture.workspace);
    let _ = fixture.core.commit();
    let ahead = read(&mut fixture, from(99));
    assert_eq!(
        serde_json::to_value(&ahead).unwrap(),
        json!({
            "ok": false,
            "result": {"events": {"run": RUN_TEXT, "next": 2, "items": []}},
            "error": "event cursor is ahead of the current sequence"
        })
    );
    let other_run = read(&mut fixture, |args| {
        args.run = Some("00000000-0000-4000-8000-000000000000".to_owned());
        args.after = Some("0".to_owned());
    });
    assert_eq!(other_run.error.as_deref(), Some("event run changed"));
    assert_eq!(events_of(&other_run)["run"], RUN_TEXT);
}

#[test]
fn a_lower_case_run_reads_and_the_answer_names_the_run_upper_case() {
    let mut fixture = fixture(0);
    let response = read(&mut fixture, |args| {
        args.run = Some(RUN_TEXT.to_lowercase());
        args.after = Some("0".to_owned());
    });
    assert!(response.ok);
    assert_eq!(events_of(&response)["run"], RUN_TEXT);
}

#[test]
fn refused_arguments_answer_upstreams_errors() {
    type Edit = fn(&mut Args);
    let mut fixture = fixture(0);
    let cases: [(Edit, &str); 4] = [
        (
            |args| args.run = Some(RUN_TEXT.to_owned()),
            "events.read requires --run and --after together",
        ),
        (
            |args| {
                args.run = Some(RUN_TEXT.to_owned());
                args.after = Some("-1".to_owned());
            },
            "invalid event cursor",
        ),
        (
            |args| args.limit = Some(1_001),
            "event limit must be between 1 and 1000",
        ),
        (
            |args| args.kinds = Some(vec!["session.created,".to_owned()]),
            "invalid event kind: ",
        ),
    ];
    for (edit, error) in cases {
        assert_eq!(read(&mut fixture, edit), failure(error));
    }
}
