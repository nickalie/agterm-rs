"""The restore policy (`restore.mode`, `restore.capture`, `restore.clear`) and the per-pane fields a capture and a
relaunch read: the foreground program, the hold-after-exit flag and the split's restore pin.

Mode changes persist for the NEXT launch only, so each test that sets one puts `none` back. A pane runs only `cat`
or a shell here: neither touches the directory it starts in."""

import pytest
from agterm_conformance.match import assert_absent, assert_error, assert_ok, assert_subset, find_session

POSIX_SHELL = pytest.mark.requires("posix-shell", reason="the foreground shell reads `sh` only for a /bin/sh pane")
STATUS = {"configured": "none", "requestedAtLaunch": "none", "active": "none", "restartRequired": False}


def _status(ctl, **args):
    return assert_ok(ctl.request("restore.mode", **args))["restore"]


@pytest.fixture
def mode_none(ctl):
    yield
    assert_ok(ctl.request("restore.mode", mode="none"))


def _new(ctl, **args):
    sid = assert_ok(ctl.request("session.new", **args))["id"]
    ctl.wait_mounted(sid, "left")
    return sid


def _node_when(ctl, sid, **fields):
    """The session node once every field reads its value; None means omitted."""

    def settled(tree):
        node = find_session(tree, sid)
        for name, value in fields.items():
            assert node.get(name) == value, f"{name} is {node.get(name)!r}, want {value!r}"
        return True

    settled.__qualname__ = f"{sid} reading {fields}"
    return find_session(ctl.wait_tree(settled), sid)


# -- restore.mode


@pytest.mark.covers("restore.mode", "field:result.restore")
def test_mode_reads_the_launch_policy(ctl):
    status = _status(ctl)
    assert status == STATUS
    assert_absent(status, "unavailableReason")


@pytest.mark.covers("restore.mode", "field:result.restore")
def test_mode_set_applies_to_the_next_launch(ctl, mode_none):
    for mode in ("rerun", "live"):
        # The running launch keeps what it started with: only `configured` moves, flagged as needing a restart.
        expected = {**STATUS, "configured": mode, "restartRequired": True}
        assert _status(ctl, mode=mode) == expected
        assert _status(ctl) == expected
    assert _status(ctl, mode="none") == STATUS


@pytest.mark.covers("restore.mode")
def test_mode_rejects_an_unknown_mode(ctl):
    assert_error(ctl.request("restore.mode", mode="bogus"), "invalid restore mode: bogus")
    assert _status(ctl) == STATUS


@pytest.mark.covers("restore.mode", "field:result.restore", "field:tree.session.backedByZmx")
def test_seeded_live_mode_is_requested_at_launch(launch, adapter):
    live = launch(settings={"restoreMode": "live"})
    status = _status(live)
    assert_subset(status, {"configured": "live", "requestedAtLaunch": "live", "restartRequired": False})
    (session,) = [s for w in live.tree()["tree"]["workspaces"] for s in w["sessions"]]
    live.wait_mounted(session["id"], "left")
    if adapter.has("live-sessions"):
        assert status["active"] == "live"
        assert_absent(status, "unavailableReason")
        _node_when(live, session["id"], backedByZmx=True)
    else:
        # A host that cannot run Live falls back to plain shells and says why.
        assert status["active"] == "none"
        assert isinstance(status["unavailableReason"], str) and status["unavailableReason"]
        assert _node_when(live, session["id"], backedByZmx=False)["surfaces"][0]["backedByZmx"] is False


@pytest.mark.covers("field:tree.session.backedByZmx")
def test_panes_are_not_zmx_backed_outside_live_mode(ctl):
    sid = _new(ctl)
    node = _node_when(ctl, sid, backedByZmx=False)
    assert [surface["backedByZmx"] for surface in node["surfaces"]] == [False]
    assert_absent(node, "liveAttribution")
    assert_ok(ctl.request("session.close", sid))


# -- restore.capture / restore.clear


@pytest.mark.covers("restore.capture")
def test_capture_needs_rerun_mode(ctl):
    assert_error(ctl.request("restore.capture"), "restore.capture requires rerun mode; configured restore mode is none")


@POSIX_SHELL
@pytest.mark.covers("restore.capture", "restore.clear", "field:result.count", "field:result.text")
def test_capture_counts_the_panes_running_a_program_and_clear_drops_them(ctl, adapter, mode_none):
    typed = [_new(ctl) for _ in range(2)]
    for sid in typed:
        assert_ok(ctl.request("session.type", sid, text="cat\n"))
    command = _new(ctl, command="cat")
    for sid in (*typed, command):
        _node_when(ctl, sid, foreground=["cat"])
    _status(ctl, mode="rerun")
    # Every other pane sits at its shell prompt, which is not a command to restore.
    count = 3 if adapter.has("capture-reads-command-panes") else 2
    assert assert_ok(ctl.request("restore.capture")) == {"count": count, "text": f"captured {count} panes"}
    assert assert_ok(ctl.request("restore.clear")) == {}
    for sid in (*typed, command):
        assert_ok(ctl.request("session.close", sid))
    assert assert_ok(ctl.request("restore.capture")) == {"count": 0, "text": "captured 0 panes"}


# -- what a capture reads


@POSIX_SHELL
@pytest.mark.covers("field:tree.session.foreground", "field:tree.session.foregroundShell")
def test_foreground_names_the_program_or_the_shell(ctl):
    shell = _new(ctl)
    assert_absent(_node_when(ctl, shell, foregroundShell="sh"), "foreground")
    program = _new(ctl, command="cat")
    assert_absent(_node_when(ctl, program, foreground=["cat"]), "foregroundShell")
    for sid in (shell, program):
        assert_ok(ctl.request("session.close", sid))


@POSIX_SHELL
@pytest.mark.covers(
    "session.new",
    "session.swap",
    "field:tree.session.commandWait",
    "field:tree.session.splitCommandWait",
    "field:tree.session.splitForeground",
    "field:tree.session.splitForegroundShell",
)
def test_a_held_command_moves_to_the_split_with_a_swap(ctl):
    sid = _new(ctl, command="cat", wait=True)
    node = _node_when(ctl, sid, commandWait=True, foreground=["cat"])
    assert_absent(node, "splitCommandWait", "splitForeground", "splitForegroundShell")
    assert_ok(ctl.request("session.split", sid, mode="on"))
    ctl.wait_mounted(sid, "right")
    _node_when(ctl, sid, splitForegroundShell="sh")
    assert_ok(ctl.request("session.swap", sid))
    node = _node_when(ctl, sid, commandWait=None, splitCommandWait=True, splitForeground=["cat"], foregroundShell="sh")
    assert_absent(node, "foreground", "splitForegroundShell")
    assert_ok(ctl.request("session.close", sid))


@pytest.mark.covers("session.restore", "field:tree.session.splitRestoreCommand")
def test_split_restore_pin_reads_back(ctl):
    sid = _new(ctl)
    assert_ok(ctl.request("session.split", sid, mode="on"))
    ctl.wait_mounted(sid, "right")
    assert_absent(find_session(ctl.tree(), sid), "splitRestoreCommand")
    assert_ok(ctl.request("session.restore", sid, mode="set", pane="right", command="echo 'split' | cat"))
    node = _node_when(ctl, sid, splitRestoreCommand="echo 'split' | cat")
    assert_absent(node, "restoreCommand")
    assert_ok(ctl.request("session.restore", sid, mode="none", pane="right"))
    _node_when(ctl, sid, splitRestoreCommand="")
    assert_ok(ctl.request("session.restore", sid, mode="clear", pane="right"))
    _node_when(ctl, sid, splitRestoreCommand=None)
    assert_ok(ctl.request("session.close", sid))
