"""`events.read`: the cursor contract and the core lifecycle events."""

import pytest
from agterm_conformance.match import (
    ANY,
    active_session_id,
    active_workspace_id,
    assert_error,
    assert_ok,
    assert_subset,
    find_session,
    window_ids,
)

# The ring keeps this many events; a cursor further behind has expired.
RING_CAPACITY = 4096
# Closing the selected session also selects another, so a page asks for these two only.
CREATE_CLOSE = ["session.created", "session.closed"]


def _batch(response):
    return assert_ok(response)["events"]


def _read(ctl, cursor, **args):
    return ctl.request("events.read", run=cursor.run, after=str(cursor.after), **args)


@pytest.mark.covers("events.read", "field:result.events")
def test_read_without_a_cursor_subscribes_from_now(ctl):
    first = _batch(ctl.request("events.read"))
    assert_subset(first, {"items": []})
    assert isinstance(first["run"], str) and first["run"]
    assert isinstance(first["next"], int) and not isinstance(first["next"], bool)
    sid = assert_ok(ctl.request("session.new"))["id"]
    later = _batch(ctl.request("events.read"))
    assert_subset(later, {"run": first["run"], "items": []})
    assert later["next"] > first["next"]
    assert_ok(ctl.request("session.close", sid))


@pytest.mark.covers("events.read", "field:result.events")
def test_cursor_pages_through_the_ring(ctl):
    cursor = ctl.events_baseline()
    sid = assert_ok(ctl.request("session.new"))["id"]
    assert_ok(ctl.request("session.close", sid))
    whole = _batch(_read(ctl, cursor, kinds=CREATE_CLOSE))
    assert [event["kind"] for event in whole["items"]] == CREATE_CLOSE
    seqs = [event["seq"] for event in whole["items"]]
    assert cursor.after < seqs[0] < seqs[1] <= whole["next"]

    page = _batch(_read(ctl, cursor, kinds=CREATE_CLOSE, limit=1))
    assert [event["seq"] for event in page["items"]] == seqs[:1]
    assert page["next"] == seqs[0], "a full page stops its cursor at its last event"
    rest = _batch(_read(ctl, cursor.past(page["items"][0]), kinds=CREATE_CLOSE))
    assert [event["seq"] for event in rest["items"]] == seqs[1:]


@pytest.mark.covers("events.read", "field:result.events")
@pytest.mark.parametrize(
    "kinds",
    [
        pytest.param(["session.created", "session.closed"], id="list"),
        pytest.param(["session.created,session.closed"], id="comma-joined"),
        pytest.param([" session.created , session.closed"], id="padded"),
    ],
)
def test_kinds_filter(ctl, kinds):
    cursor = ctl.events_baseline()
    sid = assert_ok(ctl.request("session.new"))["id"]
    assert_ok(ctl.request("session.close", sid))
    batch = _batch(_read(ctl, cursor, kinds=kinds))
    assert [(event["kind"], event["session"]) for event in batch["items"]] == [
        ("session.created", sid),
        ("session.closed", sid),
    ]


@pytest.mark.covers("events.read", "field:result.events")
def test_next_advances_past_filtered_out_events(ctl):
    cursor = ctl.events_baseline()
    sid = assert_ok(ctl.request("session.new"))["id"]
    created = ctl.wait_event(cursor, "session.created", {"session": sid})
    batch = _batch(_read(ctl, cursor, kinds=["session.closed"]))
    assert batch["items"] == []
    assert batch["next"] >= created["seq"]
    assert_ok(ctl.request("session.close", sid))


@pytest.mark.covers("events.read", "field:result.events")
def test_run_id_is_case_insensitive(ctl):
    cursor = ctl.events_baseline()
    batch = _batch(ctl.request("events.read", run=cursor.run.lower(), after=str(cursor.after)))
    assert batch["run"] == cursor.run


@pytest.mark.covers("events.read", "field:result.events")
@pytest.mark.parametrize(
    ("run", "after", "error"),
    [
        pytest.param(None, "999999999", "event cursor is ahead of the current sequence", id="ahead"),
        pytest.param("00000000-0000-4000-8000-000000000000", "0", "event run changed", id="other-run"),
    ],
)
def test_cursor_failure_carries_the_anchor(ctl, run, after, error):
    anchor = ctl.events_baseline()
    response = ctl.request("events.read", run=run or anchor.run, after=after)
    assert_error(response, error)
    failed = response.result["events"]
    assert_subset(failed, {"run": anchor.run, "items": []})
    assert failed["next"] >= anchor.after


@pytest.mark.covers("events.read")
def test_cursor_expires_past_the_ring_capacity(app, ctl):
    sid = active_session_id(ctl.tree())
    for index in range(RING_CAPACITY + 1):
        assert_ok(ctl.request("session.status", sid, status="active" if index % 2 == 0 else "idle"))
    anchor = ctl.events_baseline()
    run, head = anchor.run, anchor.after
    oldest = _batch(ctl.request("events.read", run=run, after=str(head - RING_CAPACITY), limit=1))
    assert [event["seq"] for event in oldest["items"]] == [head - RING_CAPACITY + 1]
    response = ctl.request("events.read", run=run, after=str(head - RING_CAPACITY - 1))
    assert_error(response, "event cursor expired")
    assert_subset(response.result["events"], {"run": run, "next": head, "items": []})


@pytest.mark.covers("events.read")
@pytest.mark.parametrize(
    ("args", "error"),
    [
        pytest.param({"run": "RUN"}, "events.read requires --run and --after together", id="run-alone"),
        pytest.param({"after": "0"}, "events.read requires --run and --after together", id="after-alone"),
        pytest.param({"run": "x", "after": "0"}, "invalid event run id", id="bad-run"),
        pytest.param({"run": "RUN", "after": "abc"}, "invalid event cursor", id="bad-after"),
        pytest.param({"run": "RUN", "after": "-1"}, "invalid event cursor", id="negative-after"),
        pytest.param({"limit": 0}, "event limit must be between 1 and 1000", id="limit-0"),
        pytest.param({"limit": 1001}, "event limit must be between 1 and 1000", id="limit-1001"),
        pytest.param({"kinds": ["bogus"]}, "invalid event kind: bogus", id="unknown-kind"),
        pytest.param({"kinds": ["session.created,"]}, "invalid event kind: ", id="empty-kind"),
    ],
)
def test_invalid_read_request(ctl, args, error):
    run = ctl.events_baseline().run
    args = {key: run if value == "RUN" else value for key, value in args.items()}
    assert_error(ctl.request("events.read", **args), error)


@pytest.mark.covers(
    "event:session.created",
    "event:session.selected",
    "event:session.closed",
    "event:tree.changed",
)
def test_session_lifecycle_events(ctl):
    tree = ctl.tree()
    (window,) = window_ids(ctl.request("window.list"))
    workspace = active_workspace_id(tree)
    first = active_session_id(tree)
    cursor = ctl.events_baseline()

    sid = assert_ok(ctl.request("session.new"))["id"]
    name = find_session(ctl.tree(), sid)["name"]
    where = {"window": window, "workspace": workspace, "session": sid}
    created = ctl.wait_event(cursor, "session.created", where)
    assert_subset(created, {"payload": {"name": name}, "ts": ANY})
    assert isinstance(created["ts"], (int, float))
    selected = ctl.wait_event(cursor, "session.selected", where)
    assert_subset(selected, {"payload": {"name": name, "previous": first}})
    assert created["seq"] < selected["seq"]
    ctl.wait_event(cursor, "tree.changed", {"window": window})

    cursor = ctl.events_baseline()
    assert_ok(ctl.request("session.select", first))
    ctl.wait_event(cursor, "session.selected", {"session": first, "payload": {"previous": sid}})
    assert_ok(ctl.request("session.close", sid))
    ctl.wait_event(cursor, "session.closed", {**where, "payload": {"name": name}})
