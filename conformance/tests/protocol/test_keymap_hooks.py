"""`keymap.conf` and `hooks.conf`: editing the instance's own files, reloading them, and their `keymap.list` and
`hooks.list` read-back, plus `keymap.run`. The module shares one instance and every test leaves both files absent.

Both files live in `<state>/config`, never the account's. Every custom command and hook here only writes into the
instance root, by absolute path: a command runs in the session's directory, which on the Linux fork is the
account's real home, and a hook in the app's."""

import json
import os
import shlex

import pytest
from agterm_conformance.match import ANY, assert_absent, assert_error, assert_ok, assert_subset, window_ids
from agterm_conformance.wait import wait_until

UNKNOWN = "zzz"


def _quoted(path):
    return shlex.quote(os.fspath(path))


def _new(ctl, **args):
    sid = assert_ok(ctl.request("session.new", **args))["id"]
    ctl.wait_mounted(sid, "left")
    return sid


def _written(path):
    """The text of `path` once it exists: commands write a temp file and rename it, so it is never half written."""

    def written():
        assert path.exists(), f"{path.name} not written yet"
        return path.read_text()

    written.__qualname__ = f"{path.name} written"
    return wait_until(written)


# -- keymap


@pytest.fixture
def keymap(ctl):
    """Write the instance's `keymap.conf`; the file is removed and reloaded afterwards."""
    path = ctl.instance.state_dir / "config" / "keymap.conf"
    yield path
    path.unlink(missing_ok=True)
    assert assert_ok(ctl.request("keymap.reload")) == {"count": 0}


def _listed(ctl):
    return assert_ok(ctl.request("keymap.list"))["keymap"]


def _action(listing, name):
    (action,) = [action for action in listing["actions"] if action["action"] == name]
    return action


def _marker_command(name, marker):
    """A `command` line recording the session, pane and window the command ran for."""
    tmp = _quoted(f"{marker}.tmp")
    fields = '"$AGT_SESSION_ID" "$AGT_PANE" "$AGT_WINDOW_ID"'
    return f"command \"{name}\" printf '%s|%s|%s' {fields} > {tmp} && mv {tmp} {_quoted(marker)}\n"


@pytest.mark.covers("keymap.list", "keymap.reload", "field:result.keymap", "field:result.count")
def test_list_without_a_file_reports_the_builtins_alone(ctl, adapter, keymap):
    assert assert_ok(ctl.request("keymap.reload")) == {"count": 0}
    listing = _listed(ctl)
    assert listing["path"] == os.fspath(keymap)
    assert listing["commands"] == [] and listing["diagnostics"] == []
    names = [action["action"] for action in listing["actions"]]
    assert {"new_session", "close_session", "toggle_split"} <= set(names)
    assert len(names) == len(set(names))
    for action in listing["actions"]:
        assert_absent(action, "overridden", "repeats")
    if adapter.has("keymap-menu"):
        assert listing["menu"] and all({"menu", "title", "chord"} <= set(item) for item in listing["menu"])
    else:
        # The GTK fork has no menu bar to report.
        assert_absent(listing, "menu")


@pytest.mark.covers("keymap.reload", "keymap.list", "field:result.keymap", "field:result.count")
def test_reload_applies_the_edited_file(ctl, keymap):
    keymap.write_text(
        "# conformance keymap\n"
        "map ctrl+shift+f12|ctrl+shift+f10 new_session\n"
        'command "plain" true\n'
        'command "keyed" ctrl+shift+f11 --error-hud --error-position top-left --error-pane right true\n'
        "bogus line here\n"
    )
    assert assert_ok(ctl.request("keymap.reload")) == {"count": 1}
    listing = _listed(ctl)
    assert _action(listing, "new_session") == {
        "action": "new_session",
        "chord": "ctrl+shift+f12",
        "alternates": ["ctrl+shift+f10"],
        "overridden": True,
    }
    assert listing["commands"] == [
        {"name": "plain", "errorHud": False, "errorPosition": "center", "repeats": False},
        {
            "name": "keyed",
            "shortcut": "ctrl+shift+f11",
            "errorHud": True,
            "errorPosition": "top-left",
            "errorPane": "right",
            "repeats": False,
        },
    ]
    assert listing["diagnostics"] == [{"line": 5, "message": "unknown verb 'bogus'"}]

    keymap.write_text('command "plain" true\n')
    assert assert_ok(ctl.request("keymap.reload")) == {"count": 0}
    listing = _listed(ctl)
    assert_absent(_action(listing, "new_session"), "overridden", "alternates")
    assert [command["name"] for command in listing["commands"]] == ["plain"]
    assert listing["diagnostics"] == []


@pytest.mark.covers("keymap.run", "field:result.id", "behavior:protocol.target-resolution")
def test_run_starts_the_command_for_the_addressed_session(ctl, keymap):
    first, second = ctl.instance.root / "run-first", ctl.instance.root / "run-active"
    keymap.write_text(_marker_command("mark-first", first) + _marker_command("mark-active", second))
    assert assert_ok(ctl.request("keymap.reload")) == {"count": 0}
    window = window_ids(ctl.request("window.list"))[0]
    target = _new(ctl)
    active = _new(ctl)
    try:
        assert assert_ok(ctl.request("keymap.run", target, name="mark-first")) == {"id": target}
        assert _written(first) == f"{target}|left|{window}"
        assert assert_ok(ctl.request("keymap.run", name="mark-active")) == {"id": active}
        assert _written(second) == f"{active}|left|{window}"
    finally:
        for sid in (target, active):
            ctl.request("session.close", sid)


@pytest.mark.covers("keymap.run", "keymap.reload")
def test_reload_reaches_every_window(ctl, keymap):
    marker = ctl.instance.root / "run-second-window"
    wid = assert_ok(ctl.request("window.new", name="second"))["id"]
    try:
        sid = _new(ctl, window=wid)
        keymap.write_text(_marker_command("mark", marker))
        assert assert_ok(ctl.request("keymap.reload")) == {"count": 0}
        assert assert_ok(ctl.request("keymap.run", sid, name="mark", window=wid)) == {"id": sid}
        assert _written(marker) == f"{sid}|left|{wid}"
    finally:
        assert_ok(ctl.request("window.delete", wid))


@pytest.mark.covers("keymap.run")
@pytest.mark.parametrize(
    ("target", "args", "error"),
    [
        (None, {}, "keymap.run requires a command name"),
        (None, {"name": ""}, "keymap.run requires a command name"),
        (None, {"name": "nope"}, "no custom command named nope"),
        (None, {"name": " plain"}, "no custom command named  plain"),
        (UNKNOWN, {"name": "plain"}, f"no such session: {UNKNOWN}"),
    ],
)
def test_run_refusals(ctl, keymap, target, args, error):
    keymap.write_text('command "plain" true\n')
    assert assert_ok(ctl.request("keymap.reload")) == {"count": 0}
    assert_error(ctl.request("keymap.run", target, **args), error)


# -- hooks


@pytest.fixture
def hooks(ctl):
    """Write the instance's `hooks.conf`; the file is removed and reloaded afterwards."""
    path = ctl.instance.state_dir / "config" / "hooks.conf"
    yield path
    path.unlink(missing_ok=True)
    assert assert_ok(ctl.request("hooks.reload")) == {"count": 0}


def _hooks(ctl):
    return assert_ok(ctl.request("hooks.list"))["hooks"]


@pytest.mark.covers("hooks.list", "hooks.reload", "field:result.hooks", "field:result.count")
def test_list_without_a_file_has_no_hooks(ctl, hooks):
    assert assert_ok(ctl.request("hooks.reload")) == {"count": 0}
    assert _hooks(ctl) == {"path": os.fspath(hooks), "diagnostics": [], "hooks": []}


@pytest.mark.covers("hooks.reload", "hooks.list", "field:result.hooks", "field:result.count")
def test_reload_reports_each_bad_line_and_keeps_the_good_ones(ctl, hooks):
    good = "echo created > /dev/null"
    hooks.write_text(
        "# conformance hooks\n"
        f"on session.created {good}\n"
        "on session.renamed true\n"
        "off status true\n"
        "on notify\n"
        f"on session.created   {good}  \n"
    )
    assert assert_ok(ctl.request("hooks.reload")) == {"count": 4}
    assert _hooks(ctl) == {
        "path": os.fspath(hooks),
        "diagnostics": [
            {"line": 3, "message": "unknown event kind 'session.renamed'"},
            {"line": 4, "message": "unknown verb 'off'"},
            {"line": 5, "message": "hook for 'notify' has no shell line"},
            {"line": 6, "message": f"hook 'on session.created {good}' is already defined; hook skipped"},
        ],
        "hooks": [{"kind": "session.created", "command": good, "line": 2, "pending": 0, "dropped": 0}],
    }


@pytest.mark.covers("hooks.reload", "event:session.created", "event:session.closed")
def test_hook_gets_its_event_on_stdin_and_in_its_environment(ctl, hooks):
    out = ctl.instance.root / "hook-events"
    out.mkdir()
    lines = []
    for kind in ("session.created", "session.closed"):
        base = _quoted(out) + f"/{kind}-$AGT_SESSION_ID"
        fields = '"$AGT_EVENT_KIND" "$AGT_SESSION_ID" "$AGT_WINDOW_ID" "$AGT_SOCKET"'
        lines.append(
            f"on {kind} cat > {base}.json.tmp; printf '%s|%s|%s|%s' {fields} > {base}.env.tmp;"
            f" mv {base}.json.tmp {base}.json; mv {base}.env.tmp {base}.env\n"
        )
    hooks.write_text("".join(lines))
    assert assert_ok(ctl.request("hooks.reload")) == {"count": 0}
    window = window_ids(ctl.request("window.list"))[0]
    cursor = ctl.events_baseline()
    sid = assert_ok(ctl.request("session.new"))["id"]
    created = ctl.wait_event(cursor, "session.created", {"session": sid})
    assert json.loads(_written(out / f"session.created-{sid}.json")) == created
    socket = os.fspath(ctl.instance.socket)
    assert _written(out / f"session.created-{sid}.env") == f"session.created|{sid}|{window}|{socket}"
    assert not (out / f"session.closed-{sid}.env").exists()

    assert_ok(ctl.request("session.close", sid))
    closed = ctl.wait_event(cursor, "session.closed", {"session": sid})
    assert json.loads(_written(out / f"session.closed-{sid}.json")) == closed
    assert _written(out / f"session.closed-{sid}.env") == f"session.closed|{sid}|{window}|{socket}"


def _release(fifo):
    """Let one blocked hook run past its gate; False while nothing waits on it."""
    try:
        descriptor = os.open(fifo, os.O_WRONLY | os.O_NONBLOCK)
    except OSError:
        return False
    try:
        os.write(descriptor, b"go\n")
    finally:
        os.close(descriptor)
    return True


@pytest.mark.covers("hooks.list", "hooks.reload", "field:result.hooks")
def test_a_running_hook_queues_its_events_and_survives_its_removal(ctl, hooks):
    fifo = ctl.instance.root / "hook-gate"
    os.mkfifo(fifo)
    command = f"cat > /dev/null; read gate < {_quoted(fifo)}"
    hooks.write_text(f"on session.created {command}\n")
    assert assert_ok(ctl.request("hooks.reload")) == {"count": 0}
    sessions = []

    def row(**expected):
        def accept(response):
            (entry,) = assert_ok(response)["hooks"]["hooks"]
            for key, value in expected.items():
                assert entry.get(key) == value, entry
            return True

        accept.__qualname__ = f"the hook row reading {expected}"
        (entry,) = ctl.wait_response("hooks.list", None, accept).result["hooks"]["hooks"]
        return entry

    try:
        sessions.append(assert_ok(ctl.request("session.new"))["id"])
        running = wait_until(lambda: _hooks(ctl)["hooks"][0].get("runningPid"))
        entry = row(runningPid=running, pending=0)
        assert entry["elapsedSeconds"] >= 0
        assert_absent(entry, "retired", "lastFailure")

        sessions.append(assert_ok(ctl.request("session.new"))["id"])
        row(runningPid=running, pending=1)
        assert assert_ok(ctl.request("hooks.reload")) == {"count": 0}
        row(runningPid=running, pending=1)

        hooks.write_text("# no hooks\n")
        assert assert_ok(ctl.request("hooks.reload")) == {"count": 0}
        entry = row(runningPid=running, pending=0, retired=True)
        assert_subset(
            entry,
            {"kind": "session.created", "command": command, "line": 1, "elapsedSeconds": ANY, "dropped": 0},
        )
        assert_absent(entry, "lastFailure")
        wait_until(lambda: _release(fifo))
        ctl.wait_response("hooks.list", None, lambda response: assert_ok(response)["hooks"]["hooks"] == [])
    finally:
        # A gated child is detached from the app and would outlive it: retire the hook, then open the gate until
        # nothing runs.
        hooks.write_text("# no hooks\n")
        ctl.request("hooks.reload")
        wait_until(lambda: not _release(fifo) and not any("runningPid" in hook for hook in _hooks(ctl)["hooks"]))
        for sid in sessions:
            ctl.request("session.close", sid)


@pytest.mark.covers("hooks.reload", "hooks.list")
@pytest.mark.parametrize("cmd", ["hooks.reload", "hooks.list"])
@pytest.mark.parametrize(("target", "args"), [("active", {}), (UNKNOWN, {}), (None, {"window": UNKNOWN})])
def test_hooks_take_no_target_or_window(ctl, cmd, target, args):
    assert_error(ctl.request(cmd, target, **args), f"{cmd} takes no target or --window")
