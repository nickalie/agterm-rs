//! The ring after upstream's `ControlEventRingTests`, the request validation after `ControlEventDispatcherTests`, and
//! the per-window `tree.changed` debounce after `AppStoreEventTests`.

use std::collections::HashSet;

use agterm_proto::{Args, EventKind, EventPayload};
use uuid::Uuid;

use super::*;
use crate::model::{ModelEvent, WindowEvent};

const RUN: Uuid = Uuid::from_u128(0xAAAA_AAAA_BBBB_CCCC_DDDD_EEEE_EEEE_EEEE);
const TS: f64 = 1_783_969_641.38;

fn ring(capacity: usize) -> EventRing {
    EventRing::new(capacity, RUN, Box::new(|| TS))
}

fn draft(kind: EventKind, name: &str) -> EventDraft {
    EventDraft {
        kind,
        window: Some("window".to_owned()),
        workspace: Some("workspace".to_owned()),
        session: Some("session".to_owned()),
        payload: EventPayload {
            name: Some(name.to_owned()),
            ..EventPayload::default()
        },
    }
}

fn after(after: u64) -> ReadOptions {
    ReadOptions {
        cursor: Some(EventCursor { run: RUN, after }),
        ..ReadOptions::default()
    }
}

fn of_kinds(options: &ReadOptions, kinds: &[EventKind], limit: usize) -> ReadOptions {
    ReadOptions {
        kinds: Some(kinds.iter().copied().collect()),
        limit,
        cursor: options.cursor,
    }
}

fn seqs(batch: &EventBatch) -> Vec<u64> {
    batch.items.iter().map(|event| event.seq).collect()
}

fn names(batch: &EventBatch) -> Vec<&str> {
    batch
        .items
        .iter()
        .map(|event| event.payload.name.as_deref().unwrap_or_default())
        .collect()
}

#[test]
fn append_assigns_monotonic_sequences_and_the_timestamp() {
    let mut subject = ring(DEFAULT_CAPACITY);
    let first = subject.append(draft(EventKind::Status, "api-fix"));
    let second = subject.append(draft(EventKind::Notify, "api-fix"));
    assert_eq!((first.seq, second.seq), (1, 2));
    assert!((first.ts - TS).abs() < f64::EPSILON);
    assert_eq!(first.kind, EventKind::Status);
    assert_eq!(first.window.as_deref(), Some("window"));
    assert_eq!(first.payload.name.as_deref(), Some("api-fix"));
    assert_eq!(subject.current_sequence(), 2);
}

#[test]
fn a_read_without_a_cursor_anchors_at_the_tail_without_history() {
    let mut subject = ring(DEFAULT_CAPACITY);
    subject.append(draft(EventKind::Status, "a"));
    subject.append(draft(EventKind::Notify, "b"));
    let batch = subject.read(&ReadOptions::default()).unwrap();
    assert_eq!(batch, subject.batch(2, Vec::new()));
    assert_eq!(
        batch.run, "AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE",
        "the run id is upper case"
    );
}

#[test]
fn overflow_evicts_the_oldest_and_keeps_the_exact_boundary_readable() {
    let mut subject = ring(2);
    subject.append(draft(EventKind::Status, "one"));
    subject.append(draft(EventKind::Notify, "two"));
    subject.append(draft(EventKind::SessionCreated, "three"));

    let boundary = subject.read(&after(1)).unwrap();
    assert_eq!(seqs(&boundary), [2, 3]);
    assert_eq!(names(&boundary), ["two", "three"]);
    assert_eq!(boundary.next, 3);

    let expired = subject.read(&after(0)).unwrap_err();
    assert_eq!(expired.error, EventReadError::CursorExpired);
    assert_eq!(expired.anchor, subject.batch(3, Vec::new()));
}

#[test]
fn a_full_ring_serves_exactly_its_capacity() {
    let capacity = 8;
    let mut subject = ring(capacity);
    for _ in 0..=capacity {
        subject.append(draft(EventKind::Status, "s"));
    }
    let head = subject.current_sequence();
    let oldest = subject
        .read(&of_kinds(&after(head - 8), &[EventKind::Status], 1))
        .unwrap();
    assert_eq!(seqs(&oldest), [head - 7]);
    assert_eq!(
        subject.read(&after(head - 9)).unwrap_err().error,
        EventReadError::CursorExpired
    );
}

#[test]
fn reads_are_non_destructive_and_independent() {
    let mut subject = ring(DEFAULT_CAPACITY);
    subject.append(draft(EventKind::Status, "s"));
    let first = subject.read(&after(0)).unwrap();
    let second = subject.read(&after(0)).unwrap();
    assert_eq!(first, second);
    assert_eq!(seqs(&first), [1]);
}

#[test]
fn a_filter_advances_past_non_matching_events() {
    let mut subject = ring(DEFAULT_CAPACITY);
    subject.append(draft(EventKind::Notify, "n"));
    subject.append(draft(EventKind::Status, "s"));
    subject.append(draft(EventKind::SessionClosed, "c"));
    let batch = subject
        .read(&of_kinds(&after(0), &[EventKind::Status], DEFAULT_LIMIT))
        .unwrap();
    assert_eq!(seqs(&batch), [2]);
    assert_eq!(batch.next, 3);

    let empty = subject
        .read(&of_kinds(&after(0), &[EventKind::PaneSplit], DEFAULT_LIMIT))
        .unwrap();
    assert_eq!(
        empty,
        subject.batch(3, Vec::new()),
        "an empty filtered page advances to the tail"
    );
}

#[test]
fn a_full_page_stops_at_its_last_match_without_dropping_later_ones() {
    let mut subject = ring(DEFAULT_CAPACITY);
    subject.append(draft(EventKind::Notify, "skip"));
    subject.append(draft(EventKind::Status, "first"));
    subject.append(draft(EventKind::Notify, "skip-two"));
    subject.append(draft(EventKind::Status, "second"));

    let first = subject.read(&of_kinds(&after(0), &[EventKind::Status], 1)).unwrap();
    assert_eq!((first.next, names(&first)), (2, vec!["first"]));
    let second = subject
        .read(&of_kinds(&after(first.next), &[EventKind::Status], 1))
        .unwrap();
    assert_eq!((second.next, names(&second)), (4, vec!["second"]));
}

#[test]
fn a_cursor_of_another_run_or_ahead_fails_with_the_anchor() {
    let mut subject = ring(DEFAULT_CAPACITY);
    subject.append(draft(EventKind::Status, "s"));
    let other = ReadOptions {
        cursor: Some(EventCursor {
            run: Uuid::from_u128(0x1111_1111_2222_3333_4444_5555_5555_5555),
            after: 0,
        }),
        ..ReadOptions::default()
    };
    let changed = subject.read(&other).unwrap_err();
    assert_eq!(changed.error, EventReadError::RunChanged);
    assert_eq!(changed.anchor, subject.batch(1, Vec::new()));

    let ahead = subject.read(&after(2)).unwrap_err();
    assert_eq!(ahead.error, EventReadError::CursorAhead);
    assert_eq!(ahead.anchor, subject.batch(1, Vec::new()));
    assert_eq!(subject.read(&after(1)).unwrap(), subject.batch(1, Vec::new()));
}

#[test]
fn read_errors_carry_upstreams_text() {
    assert_eq!(EventReadError::RunChanged.to_string(), "event run changed");
    assert_eq!(EventReadError::CursorExpired.to_string(), "event cursor expired");
    assert_eq!(
        EventReadError::CursorAhead.to_string(),
        "event cursor is ahead of the current sequence"
    );
}

// MARK: - Request validation (upstream's `ControlEventDispatcherTests`)

fn args(run: Option<&str>, after: Option<&str>, kinds: Option<&[&str]>, limit: Option<i64>) -> Args {
    Args {
        run: run.map(str::to_owned),
        after: after.map(str::to_owned),
        kinds: kinds.map(|kinds| kinds.iter().map(|kind| (*kind).to_owned()).collect()),
        limit,
        ..Args::default()
    }
}

const RUN_TEXT: &str = "CBB5E3D0-7A9B-4C96-9EA2-18B14380DDB1";

#[test]
fn a_bare_read_takes_the_defaults() {
    assert_eq!(ReadOptions::parse(None).unwrap(), ReadOptions::default());
    assert_eq!(
        ReadOptions::parse(Some(&Args::default())).unwrap(),
        ReadOptions::default()
    );
    assert_eq!(ReadOptions::default().limit, 100);
}

#[test]
fn the_cursor_kinds_and_the_maximum_limit_parse() {
    let parsed = ReadOptions::parse(Some(&args(
        Some(&RUN_TEXT.to_lowercase()),
        Some("42"),
        Some(&["status, notify", "tree.changed", "status"]),
        Some(1_000),
    )))
    .unwrap();
    let expected = ReadOptions {
        cursor: Some(EventCursor {
            run: Uuid::from_u128(0xCBB5_E3D0_7A9B_4C96_9EA2_18B1_4380_DDB1),
            after: 42,
        }),
        kinds: Some(HashSet::from([
            EventKind::Status,
            EventKind::Notify,
            EventKind::TreeChanged,
        ])),
        limit: 1_000,
    };
    assert_eq!(parsed, expected);
}

#[test]
fn refused_requests_carry_upstreams_text() {
    let cases: [(Args, &str); 11] = [
        (
            args(Some(RUN_TEXT), None, None, None),
            "events.read requires --run and --after together",
        ),
        (
            args(None, Some("1"), None, None),
            "events.read requires --run and --after together",
        ),
        (args(Some("not-a-uuid"), Some("1"), None, None), "invalid event run id"),
        (args(Some(RUN_TEXT), Some("-1"), None, None), "invalid event cursor"),
        (args(Some(RUN_TEXT), Some("abc"), None, None), "invalid event cursor"),
        (
            args(None, None, None, Some(0)),
            "event limit must be between 1 and 1000",
        ),
        (
            args(None, None, None, Some(1_001)),
            "event limit must be between 1 and 1000",
        ),
        (
            args(None, None, Some(&["status", "future.kind"]), None),
            "invalid event kind: future.kind",
        ),
        (args(None, None, Some(&[""]), None), "invalid event kind: "),
        (args(None, None, Some(&[","]), None), "invalid event kind: "),
        (args(None, None, Some(&["status,"]), None), "invalid event kind: "),
    ];
    for (args, error) in cases {
        let refused = ReadOptions::parse(Some(&args)).unwrap_err();
        assert_eq!(ControlError::from(refused).to_string(), error, "{args:?}");
    }
}

#[test]
fn the_run_check_comes_before_the_cursor_and_the_limit_before_the_kinds() {
    let refused = ReadOptions::parse(Some(&args(Some("x"), Some("y"), None, None))).unwrap_err();
    assert_eq!(refused, EventRequestError::InvalidRun);
    let refused = ReadOptions::parse(Some(&args(None, None, Some(&["bogus"]), Some(0)))).unwrap_err();
    assert_eq!(refused, EventRequestError::InvalidLimit);
}

// MARK: - The sink and the tree.changed debounce

fn window_event(window: Uuid, kind: EventKind, session: Option<Uuid>) -> WindowEvent {
    WindowEvent {
        window,
        event: ModelEvent {
            session,
            ..ModelEvent::bare(kind)
        },
    }
}

fn events() -> ControlEvents {
    ControlEvents::new(ring(DEFAULT_CAPACITY))
}

fn kinds_after(events: &ControlEvents, cursor: u64) -> Vec<(EventKind, Option<String>)> {
    events
        .read(&after(cursor))
        .unwrap()
        .items
        .into_iter()
        .map(|event| (event.kind, event.window))
        .collect()
}

#[test]
fn the_sink_tags_identities_upper_case_and_sequences_in_emission_order() {
    let mut subject = events();
    let (window, session) = (Uuid::new_v4(), Uuid::new_v4());
    let timers = subject.record(vec![
        window_event(window, EventKind::SessionCreated, Some(session)),
        window_event(window, EventKind::SessionSelected, Some(session)),
    ]);
    assert!(timers.is_empty());
    let batch = subject.read(&after(0)).unwrap();
    assert_eq!(seqs(&batch), [1, 2]);
    let created = &batch.items[0];
    assert_eq!(created.kind, EventKind::SessionCreated);
    assert_eq!(created.window, Some(uuid_string(window)));
    assert_eq!(created.session, Some(uuid_string(session)));
    assert_eq!(created.workspace, None);
}

#[test]
fn tree_changed_waits_for_its_timer_and_coalesces_per_window() {
    let mut subject = events();
    let (first, second) = (Uuid::new_v4(), Uuid::new_v4());
    let timers = subject.record(vec![
        window_event(first, EventKind::TreeChanged, None),
        window_event(first, EventKind::SessionCreated, None),
        window_event(second, EventKind::TreeChanged, None),
        window_event(first, EventKind::TreeChanged, None),
    ]);
    assert_eq!(
        timers.iter().map(|timer| timer.window).collect::<Vec<_>>(),
        [first, second],
        "one timer per window"
    );
    assert_eq!(
        kinds_after(&subject, 0),
        [(EventKind::SessionCreated, Some(uuid_string(first)))]
    );

    for timer in timers {
        assert!(subject.fire_tree_changed(timer).is_some());
    }
    assert_eq!(
        kinds_after(&subject, 1),
        [
            (EventKind::TreeChanged, Some(uuid_string(first))),
            (EventKind::TreeChanged, Some(uuid_string(second))),
        ]
    );
    let tree_changed = subject.read(&after(1)).unwrap().items.remove(0);
    assert_eq!((tree_changed.workspace, tree_changed.session), (None, None));
    assert_eq!(tree_changed.payload, EventPayload::default());
}

#[test]
fn a_later_change_rearms_the_debounce_and_the_stale_timer_appends_nothing() {
    let mut subject = events();
    let window = Uuid::new_v4();
    let stale = subject.record(vec![window_event(window, EventKind::TreeChanged, None)]);
    let live = subject.record(vec![window_event(window, EventKind::TreeChanged, None)]);
    assert!(subject.fire_tree_changed(stale[0]).is_none());
    assert_eq!(subject.ring().current_sequence(), 0);
    let fired = subject.fire_tree_changed(live[0]).unwrap();
    assert_eq!(fired.seq, 1);
    assert!(subject.fire_tree_changed(live[0]).is_none(), "a timer fires once");
}

#[test]
fn a_flush_lands_every_pending_tree_changed_in_armed_order() {
    let mut subject = events();
    let (first, second) = (Uuid::new_v4(), Uuid::new_v4());
    let timers = subject.record(vec![
        window_event(second, EventKind::TreeChanged, None),
        window_event(first, EventKind::TreeChanged, None),
    ]);
    subject.flush_tree_changed();
    assert_eq!(
        kinds_after(&subject, 0),
        [
            (EventKind::TreeChanged, Some(uuid_string(second))),
            (EventKind::TreeChanged, Some(uuid_string(first))),
        ]
    );
    for timer in timers {
        assert!(subject.fire_tree_changed(timer).is_none(), "flushed timers are stale");
    }
}
