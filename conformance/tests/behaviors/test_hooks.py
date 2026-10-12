"""When `hooks.conf` hooks fire: read at launch, after an edit only once `hooks.reload` runs, and once per matching
event in event order. Each test runs its own instance; every hook only writes into the instance root, by absolute
path."""

import json
import os
import shlex

import pytest
from agterm_conformance.match import assert_ok, session_ids
from agterm_conformance.wait import wait_until


def _hooks(ctl):
    return ctl.instance.state_dir / "config" / "hooks.conf"


def _recording(kind, marker, tag):
    """A hook line that writes `tag` and the session id into `marker` when `kind` fires."""
    tmp, done = shlex.quote(os.fspath(marker) + ".tmp"), shlex.quote(os.fspath(marker))
    return f"on {kind} cat > /dev/null; printf '%s %s' {tag} \"$AGT_SESSION_ID\" > {tmp} && mv {tmp} {done}\n"


def _written(marker):
    return wait_until(lambda: marker.exists() and marker.read_text())


@pytest.mark.covers("behavior:hooks.loaded-at-launch")
def test_hooks_file_is_read_at_launch(launch):
    ctl = launch()
    marker = ctl.instance.root / "created-marker"
    _hooks(ctl).write_text(_recording("session.created", marker, "launch"))
    ctl = launch.relaunch(ctl)
    assert [hook["kind"] for hook in assert_ok(ctl.request("hooks.list"))["hooks"]["hooks"]] == ["session.created"]
    sid = assert_ok(ctl.request("session.new"))["id"]
    assert _written(marker) == f"launch {sid}"


@pytest.mark.covers("behavior:hooks.edit-needs-reload")
def test_an_edit_takes_effect_only_on_reload(launch):
    ctl = launch()
    marker = ctl.instance.root / "created-marker"
    _hooks(ctl).write_text(_recording("session.created", marker, "v1"))
    assert_ok(ctl.request("hooks.reload"))
    _hooks(ctl).write_text(_recording("session.created", marker, "v2"))

    sid = assert_ok(ctl.request("session.new"))["id"]
    assert _written(marker) == f"v1 {sid}"
    marker.unlink()
    assert_ok(ctl.request("hooks.reload"))
    sid = assert_ok(ctl.request("session.new"))["id"]
    assert _written(marker) == f"v2 {sid}"


@pytest.mark.covers("behavior:hooks.once-per-event-in-order", "event:status")
def test_a_hook_runs_once_per_event_in_event_order(launch):
    ctl = launch()
    log = ctl.instance.root / "status-log"
    _hooks(ctl).write_text(f"on status cat >> {shlex.quote(os.fspath(log))}; echo >> {shlex.quote(os.fspath(log))}\n")
    assert_ok(ctl.request("hooks.reload"))
    (sid,) = session_ids(ctl.tree())
    cursor = ctl.events_baseline()
    statuses = ["active", "blocked", "completed", "idle"]
    for status in statuses:
        assert_ok(ctl.request("session.status", sid, status=status))
    events = []
    for status in statuses:
        events.append(ctl.wait_event(cursor, "status", {"session": sid, "payload": {"status": status}}))
        cursor = cursor.past(events[-1])

    def logged():
        # The JSON may arrive with its own newline; the hook adds one in case it does not.
        lines = [line for line in (log.read_text().splitlines() if log.exists() else []) if line]
        assert len(lines) >= len(statuses), lines
        return [json.loads(line) for line in lines]

    assert wait_until(logged) == events
