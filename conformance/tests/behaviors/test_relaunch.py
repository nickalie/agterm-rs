"""What a quit and a relaunch on the same state directory bring back. Each test starts its own instance through the
`launch` fixture and relaunches it with `launch.relaunch`, which SIGTERMs the launched pid and starts the same binary
on the same root.

A pane's shell starts in the account's real home on the Linux fork, so every line typed here only prints."""

import re
import uuid

import pytest
from agterm_conformance.match import assert_absent, assert_ok, find_session, workspaces

LIVE = pytest.mark.requires(
    "live-sessions", reason="needs panes in zmx daemons, which the references create only for a zsh login shell"
)


def _new(ctl, **args):
    sid = assert_ok(ctl.request("session.new", **args))["id"]
    ctl.wait_mounted(sid, "left")
    return sid


def _layout(tree):
    """What a relaunch must keep: the workspace/session model, without the live process state."""
    kept = ("id", "name", "flagged", "context", "split", "hasSplit", "splitAxis", "restoreCommand", "active")
    return [
        {
            "id": workspace["id"],
            "name": workspace["name"],
            "collapsed": workspace.get("collapsed", False),
            "sessions": [{key: session.get(key) for key in kept} for session in workspace["sessions"]],
        }
        for workspace in workspaces(tree)
    ]


def _marker():
    return f"RL{uuid.uuid4().hex[:8].upper()}"


@pytest.mark.covers("behavior:restore.relaunch-keeps-layout")
def test_relaunch_keeps_workspaces_sessions_and_their_metadata(launch):
    ctl = launch()
    window = assert_ok(ctl.request("window.list"))["windows"][0]
    assert_ok(ctl.request("window.rename", window["id"], name="kept window"))
    alpha = assert_ok(ctl.request("workspace.new", name="alpha"))["id"]
    # Each split is made while its session is selected: the Linux fork mounts a pane only once it is on screen.
    first = _new(ctl, workspace=alpha, name="first")
    assert_ok(ctl.request("session.flag", first, mode="on"))
    assert_ok(ctl.request("session.context", first, mode="set", text="what it is for"))
    assert_ok(ctl.request("session.split", first, mode="on", axis="horizontal"))
    ctl.wait_mounted(first, "right")
    second = _new(ctl, workspace=alpha, name="second")
    assert_ok(ctl.request("session.split", second, mode="on"))
    ctl.wait_mounted(second, "right")
    assert_ok(ctl.request("session.split", second, mode="off"))
    assert_ok(ctl.request("session.restore", second, mode="none"))
    # Before a structural change: upstream saves a selection 0.3s later, and SIGTERM ends it with no final save.
    assert_ok(ctl.request("session.select", first))
    folded = assert_ok(ctl.request("workspace.new", name="folded", collapsed=True))["id"]
    before = ctl.tree()
    assert folded in {node["id"] for node in workspaces(before)}
    assert find_session(before, first)["active"] is True

    ctl = launch.relaunch(ctl)
    after = ctl.wait_tree(lambda tree: find_session(tree, first)["realized"] is True)
    assert _layout(after) == _layout(before)
    assert find_session(after, first)["active"] is True
    (restored,) = assert_ok(ctl.request("window.list"))["windows"]
    assert (restored["id"], restored["name"]) == (window["id"], "kept window")


@pytest.mark.covers("behavior:restore.relaunch-keeps-layout")
def test_relaunch_forgets_what_is_not_persisted(launch):
    ctl = launch()
    sid = _new(ctl)
    assert_ok(ctl.request("session.select", sid))
    assert_ok(ctl.request("session.status", sid, status="blocked"))
    assert_ok(ctl.request("session.scratch", sid, mode="on"))
    ctl.wait_mounted(sid, "scratch")
    # After the select, which clears unseen.
    assert_ok(ctl.request("notify", sid, body="relaunch"))
    ctl.wait_tree(lambda tree: find_session(tree, sid).get("unseen", 0) >= 1)

    ctl = launch.relaunch(ctl)
    node = find_session(ctl.wait_tree(lambda tree: find_session(tree, sid)["realized"] is True), sid)
    assert node["scratch"] is False
    assert_absent(node, "status", "statusChangedAt", "unseen")


@pytest.mark.covers("behavior:restore.rerun-types-pinned-command")
@pytest.mark.requires("posix-shell", reason="the pinned line runs in /bin/sh")
def test_rerun_types_the_pinned_line_at_the_next_launch(launch):
    ctl = launch(settings={"restoreMode": "rerun"})
    sid = _new(ctl)
    marker = _marker()
    # The pin prints the marker split in two, so only its output matches.
    assert_ok(ctl.request("session.restore", sid, mode="set", command=f"printf '%s%s\\n' {marker[:4]} {marker[4:]}"))
    ctl = launch.relaunch(ctl)
    ctl.wait_mounted(sid, "left")
    # Not anchored at the line's start: the line is typed before the shell's first prompt, and a /bin/sh without a
    # line editor (dash) does not echo it again after that prompt, so the output follows the prompt on its line.
    ctl.wait_text(sid, rf"(?m){marker}$")
    # The pin persists for the launches after this one too.
    assert find_session(ctl.tree(), sid)["restoreCommand"].endswith(marker[4:])


@pytest.mark.covers("behavior:restore.fresh-shells-ignore-pins")
@pytest.mark.requires("posix-shell", reason="the pinned line and the probe run in /bin/sh")
def test_fresh_shells_do_not_run_the_pin(launch):
    ctl = launch()
    assert assert_ok(ctl.request("restore.mode"))["restore"]["configured"] == "none"
    sid = _new(ctl)
    pinned, probe = _marker(), _marker()
    assert_ok(ctl.request("session.restore", sid, mode="set", command=f"printf '%s%s\\n' {pinned[:4]} {pinned[4:]}"))
    ctl = launch.relaunch(ctl)
    ctl.wait_mounted(sid, "left")
    # A line typed after the launch shows up; had the pin run, its output would sit above it.
    assert_ok(ctl.request("session.type", sid, text=f"printf '%s%s\\n' {probe[:4]} {probe[4:]}\n"))
    ctl.wait_text(sid, rf"(?m)^{probe}$")
    assert not re.search(rf"(?m)^{pinned}$", assert_ok(ctl.request("session.text", sid, all=True))["text"])


@pytest.mark.covers("behavior:restore.rerun-replays-captured-command")
@pytest.mark.requires("posix-shell", reason="the captured program is started from /bin/sh")
def test_rerun_replays_the_captured_foreground_program(launch):
    ctl = launch(settings={"restoreMode": "rerun"})
    sid = _new(ctl)
    # A job of its own: upstream captures the foreground group's leader, and an exec'd program would share the
    # shell's group, led by macOS's setuid `login`. The shell passes the pane's hangup on to it at the quit.
    assert_ok(ctl.request("session.type", sid, text="sleep 600\n"))
    ctl.wait_tree(lambda tree: find_session(tree, sid).get("foreground") == ["sleep", "600"])
    assert assert_ok(ctl.request("restore.capture"))["count"] >= 1
    ctl = launch.relaunch(ctl)
    ctl.wait_mounted(sid, "left")
    ctl.wait_tree(lambda tree: find_session(tree, sid).get("foreground") == ["sleep", "600"])


@LIVE
@pytest.mark.covers("behavior:restore.live-session-survives-relaunch")
def test_live_session_survives_the_relaunch(launch):
    ctl = launch(settings={"restoreMode": "live"})
    sid = _new(ctl)
    ctl.wait_tree(lambda tree: find_session(tree, sid).get("backedByZmx") is True)
    marker = _marker()
    assert_ok(ctl.request("session.type", sid, text=f"printf '%s%s\\n' {marker[:4]} {marker[4:]}\n"))
    ctl.wait_text(sid, rf"(?m)^{marker}$")
    rows = assert_ok(ctl.request("zmx.list"))["zmx"]["entries"]
    (before,) = [row for row in rows if row.get("sessionID") == sid and row["pane"] == "left"]

    ctl = launch.relaunch(ctl)
    ctl.wait_tree(lambda tree: find_session(tree, sid).get("backedByZmx") is True)

    def same_daemon(response):
        (row,) = [row for row in assert_ok(response)["zmx"]["entries"] if row.get("sessionID") == sid]
        assert (row["daemon"], row["observation"]) == (before["daemon"], "running"), row
        return row

    after = ctl.wait_response("zmx.list", None, same_daemon)
    (row,) = [row for row in after.result["zmx"]["entries"] if row.get("sessionID") == sid]
    # The same shell, not a new one: its leader pid and its screen came through the quit.
    assert row["leaderPID"] == before["leaderPID"]
    ctl.wait_text(sid, rf"(?m)^{marker}$", all=True)
