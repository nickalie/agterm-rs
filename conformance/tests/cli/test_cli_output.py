"""`agtermctl` against one shared instance: every subcommand that talks to the socket, its exit status, its `--json`
line and its exact human output, rendered from the protocol result by `agterm_conformance.cli_format`.

The `cli` fixture always passes the instance's own `--socket`. What a pane runs here only prints into the pane;
every file a test writes lives in the instance root."""

import itertools
import json
import os
import re
import uuid

import pytest
from agterm_conformance import cli_format
from agterm_conformance.match import (
    assert_absent,
    assert_ok,
    assert_subset,
    find_session,
    window_ids,
    workspaces,
)
from agterm_conformance.plugin import Ctl
from agterm_conformance.wait import WaitTimeout, wait_frame, wait_until
from agterm_conformance.zmx_client import leading_client

UNKNOWN = "zzz"
_names = itertools.count(1)
UUID_LINE = re.compile(rf"{cli_format.UUID.pattern}\n")


def _name(prefix):
    return f"{prefix}-{next(_names)}"


# -- what every CLI result looks like


def _ok(result, stdout=None):
    """Exit 0 with nothing on stderr; `stdout` is the exact output when given."""
    assert result.returncode == 0, (result.argv, result.returncode, result.stdout, result.stderr)
    assert result.stderr == "", result.stderr
    if stdout is not None:
        assert result.stdout == stdout, result.argv
    return result.stdout


def _json(result):
    """`--json`: the server's own response line, `{"ok": true, "result": ...}`."""
    _ok(result)
    assert result.stdout.endswith("\n") and result.stdout.count("\n") == 1, result.stdout
    payload = json.loads(result.stdout)
    assert payload["ok"] is True and "error" not in payload, payload
    return payload.get("result", {})


def _refused(result, error):
    """A server refusal: exit 1, `error: <text>` on stderr, nothing on stdout."""
    assert (result.returncode, result.stdout, result.stderr) == (1, "", f"error: {error}\n"), result.argv


def _refused_json(result, error):
    """A server refusal under `--json`: exit 1 and the server's `{"ok": false, "error": ...}` line on stdout."""
    assert (result.returncode, result.stderr) == (1, ""), (result.argv, result.stderr)
    assert json.loads(result.stdout) == {"ok": False, "error": error}


def _usage(result, message=None):
    """Refused before anything is sent: exit 64, `Error: ...` and the usage line on stderr."""
    assert (result.returncode, result.stdout) == (64, ""), (result.argv, result.returncode, result.stdout)
    assert result.stderr.startswith("Error: "), result.stderr
    assert "\nUsage: agtermctl " in result.stderr, result.stderr
    if message is not None:
        assert result.stderr.startswith(f"Error: {message}\n"), result.stderr


def _rendered(result, response_result, echo_id=False):
    """The human form of `response_result`, one line per `print`."""
    return _ok(result, cli_format.human(response_result, echo_id) + "\n")


def _created(result):
    """A create command prints the new id alone."""
    stdout = _ok(result)
    assert UUID_LINE.fullmatch(stdout), stdout
    return stdout.strip()


def _new(ctl, **args):
    sid = assert_ok(ctl.request("session.new", **args))["id"]
    ctl.wait_mounted(sid, "left")
    return sid


def _node(ctl, sid):
    return find_session(ctl.tree(), sid)


def _workspace(ctl, wid):
    (node,) = [node for node in workspaces(ctl.tree()) if node["id"] == wid]
    return node


# -- version, tree and events


@pytest.mark.covers("cli:version")
def test_version(cli, ctl):
    app = assert_ok(ctl.request("version"))["app"]
    assert _json(cli("version", "--json")) == {"app": app}
    stdout = _ok(cli("version"))
    first, client = stdout.splitlines()
    assert stdout == f"{first}\n{client}\n" and first == cli_format.human({"app": app})
    # The CLI's own resolved executable, after the app's version: the kernel's answer, not argv[0].
    path = client.removeprefix("client: ")
    assert client.startswith("client: ") and os.path.isabs(path) and os.access(path, os.X_OK), client


@pytest.fixture
def tree_workspace(ctl):
    """A workspace holding a session in each state `agtermctl tree` tags, removed afterwards."""
    wid = assert_ok(ctl.request("workspace.new", name=_name("cli-tree")))["id"]
    plain = _new(ctl, workspace=wid, name="plain")
    shown = _new(ctl, workspace=wid, name="shown")
    assert_ok(ctl.request("session.split", shown, mode="on"))
    ctl.wait_mounted(shown, "right")
    hidden = _new(ctl, workspace=wid, name="hidden")
    assert_ok(ctl.request("session.split", hidden, mode="on"))
    ctl.wait_mounted(hidden, "right")
    assert_ok(ctl.request("session.split", hidden, mode="off"))
    scratch = _new(ctl, workspace=wid, name="scratch")
    assert_ok(ctl.request("session.scratch", scratch, mode="on"))
    ctl.wait_mounted(scratch, "scratch")
    covered = _new(ctl, workspace=wid, name="covered")
    assert_ok(ctl.request("session.overlay.open", covered, command="exec cat"))
    ctl.wait_tree(lambda tree: find_session(tree, covered)["overlay"] is True)
    assert_ok(ctl.request("session.select", plain))
    yield wid
    assert_ok(ctl.request("session.overlay.close", covered))
    assert_ok(ctl.request("workspace.delete", wid))


@pytest.mark.covers("cli:tree")
def test_tree_lists_workspaces_and_tagged_sessions(cli, ctl, tree_workspace):
    top = _json(cli("tree", "--json"))["tree"]
    human = _ok(cli("tree"))
    assert human == cli_format.tree(top) + "\n"
    tags = {line.split("  [")[0].strip("* ") for line in human.splitlines() if line.startswith("  ")}
    assert {"plain", "shown (split)", "hidden (split hidden)", "scratch (scratch)", "covered (overlay)"} <= tags
    assert [node["id"] for node in top["workspaces"]] == [node["id"] for node in workspaces(ctl.tree())]

    window = window_ids(ctl.request("window.list"))[0]
    assert _ok(cli("tree", "--window", window)) == human
    _refused(cli("tree", "--window", UNKNOWN), f"no such window: {UNKNOWN}")
    _refused_json(cli("tree", "--json", "--window", UNKNOWN), f"no such window: {UNKNOWN}")


def _events(ctl, cursor, last):
    """Every event after `cursor` up to the one with seq `last`, as the protocol reports them."""
    items, after = [], cursor.after
    while after < last:
        batch = assert_ok(ctl.request("events.read", run=cursor.run, after=str(after)))["events"]
        items += batch["items"]
        after = batch["next"]
    return [item for item in items if item["seq"] <= last]


@pytest.mark.covers(
    "cli:events",
    "event:session.created",
    "event:session.selected",
    "event:status",
    "event:pane.split",
    "event:pane.scratch",
    "event:notify",
    "event:session.closed",
)
def test_events_print_every_event_with_its_arguments(cli, ctl):
    cursor = ctl.events_baseline()
    with cli.spawn("events", "--run", cursor.run, "--after", str(cursor.after)) as stream:
        name = _name("events")
        sid = _new(ctl, name=name)
        assert_ok(ctl.request("session.status", sid, status="active", blink=True, color="#ff8800", pane="left"))
        assert_ok(ctl.request("session.split", sid, mode="on"))
        assert_ok(ctl.request("session.scratch", sid, mode="on"))
        assert_ok(ctl.request("notify", sid, body="cli body", title="cli title"))
        assert_ok(ctl.request("session.close", sid))
        last = ctl.wait_event(cursor, "session.closed", {"session": sid})["seq"]
        expected = _events(ctl, cursor, last)
        printed = [stream.line() for _ in expected]
    assert printed == [cli_format.event(item) for item in expected]
    lines = "\n".join(printed) + "\n"
    shown = (f"pane.split {name} shown\n", f"pane.scratch {name} shown\n", "cli title: cli body\n")
    for argument in ("previous=idle", "pane=left", "blink", "color=#ff8800", *shown):
        assert argument in lines, argument


@pytest.mark.covers("cli:events", "event:status")
def test_events_json_prints_each_event_object_of_the_asked_kinds(cli, ctl):
    sid = _new(ctl)
    cursor = ctl.events_baseline()
    args = ("events", "--json", "--kind", "status", "--run", cursor.run, "--after", str(cursor.after), "--limit", "1")
    with cli.spawn(*args) as stream:
        for status in ("active", "blocked", "idle"):
            assert_ok(ctl.request("session.status", sid, status=status))
        events = []
        for status in ("active", "blocked", "idle"):
            events.append(ctl.wait_event(cursor, "status", {"session": sid, "payload": {"status": status}}))
            cursor = cursor.past(events[-1])
        assert [json.loads(stream.line()) for _ in events] == events
    assert_ok(ctl.request("session.close", sid))


@pytest.mark.covers("cli:events")
@pytest.mark.parametrize(
    ("args", "message"),
    [
        (("--limit", "0"), "event limit must be between 1 and 1000"),
        (("--run", "6F1C2A8E-0000-4000-8000-000000000000"), "events.read requires --run and --after together"),
        (("--run", "not-a-run", "--after", "1"), "invalid event run id"),
        (("--run", "6F1C2A8E-0000-4000-8000-000000000000", "--after", "x"), "invalid event cursor"),
        (("--kind", "session.renamed"), None),
    ],
)
def test_events_refuses_a_bad_cursor_or_kind_before_reading(cli, args, message):
    _usage(cli("events", *args), message)


# -- workspaces


@pytest.mark.covers(
    "cli:workspace new",
    "cli:workspace rename",
    "cli:workspace select",
    "cli:workspace collapse",
    "cli:workspace expand",
    "cli:workspace delete",
)
def test_workspace_lifecycle(cli, ctl):
    name = _name("cli-ws")
    wid = _created(cli("workspace", "new", name))
    assert _workspace(ctl, wid)["name"] == name
    folded = _json(cli("workspace", "new", _name("cli-folded"), "--collapsed", "--json"))["id"]
    assert _workspace(ctl, folded)["collapsed"] is True

    _ok(cli("workspace", "rename", "renamed by cli", "--target", wid), "ok\n")
    assert _workspace(ctl, wid)["name"] == "renamed by cli"
    _ok(cli("workspace", "select", "--target", folded), "ok\n")
    _ok(cli("workspace", "expand", "--target", folded), "ok\n")
    assert_absent(_workspace(ctl, folded), "collapsed")
    assert _json(cli("workspace", "collapse", "--target", folded, "--json")) == {"id": folded}
    assert _workspace(ctl, folded)["collapsed"] is True

    for target in (wid, folded):
        _ok(cli("workspace", "delete", "--target", target), "ok\n")
    assert {wid, folded}.isdisjoint(node["id"] for node in workspaces(ctl.tree()))
    _refused(cli("workspace", "rename", "x", "--target", UNKNOWN), f"no such workspace: {UNKNOWN}")
    _refused_json(cli("workspace", "delete", "--target", UNKNOWN, "--json"), f"no such workspace: {UNKNOWN}")


@pytest.mark.covers("cli:workspace go", "cli:workspace move")
def test_workspace_go_and_move(cli, ctl):
    first = assert_ok(ctl.request("workspace.new", name=_name("cli-go")))["id"]
    second = assert_ok(ctl.request("workspace.new", name=_name("cli-go")))["id"]
    try:
        _ok(cli("workspace", "select", "--target", first), "ok\n")
        assert _json(cli("workspace", "go", "--to", "next", "--json")) == {"id": second}
        _ok(cli("workspace", "go", "--to", "prev"), "ok\n")
        assert next(node for node in workspaces(ctl.tree()) if node["active"])["id"] == first

        _ok(cli("workspace", "move", "--to", "top", "--target", second), "ok\n")
        assert workspaces(ctl.tree())[0]["id"] == second
        assert _json(cli("workspace", "move", "--to", "bottom", "--target", second, "--json")) == {"id": second}
        assert workspaces(ctl.tree())[-1]["id"] == second
        _usage(cli("workspace", "go"))
    finally:
        for wid in (first, second):
            ctl.request("workspace.delete", wid)


@pytest.mark.covers("cli:workspace focus", "cli:workspace filter")
def test_workspace_focus_and_filter(cli, ctl):
    wid = assert_ok(ctl.request("workspace.new", name=_name("cli-focus")))["id"]
    try:
        _ok(cli("workspace", "focus", "on", "--target", wid), "ok\n")
        top = ctl.tree()["tree"]
        assert _workspace(ctl, wid)["focused"] is True and top["workspaceFilter"] is True
        _ok(cli("workspace", "filter", "off"), "ok\n")
        assert ctl.tree()["tree"]["workspaceFilter"] is False and _workspace(ctl, wid)["focused"] is True
        _json(cli("workspace", "filter", "on", "--json"))
        assert ctl.tree()["tree"]["workspaceFilter"] is True
        _ok(cli("workspace", "focus", "off", "--target", wid), "ok\n")
        assert_absent(_workspace(ctl, wid), "focused")
        assert ctl.tree()["tree"]["workspaceFilter"] is False
        # An empty focus set cannot be filtered to: the call answers ok and leaves the filter off.
        _ok(cli("workspace", "filter", "on"), "ok\n")
        assert ctl.tree()["tree"]["workspaceFilter"] is False
        _usage(cli("workspace", "filter", "show"))
    finally:
        ctl.request("workspace.focus", wid, mode="off")
        ctl.request("workspace.delete", wid)


# -- sessions


@pytest.mark.covers("cli:session new", "cli:session duplicate", "cli:session close", "cli:session select")
def test_session_create_select_and_close(cli, ctl):
    name = _name("cli-new")
    sid = _created(cli("session", "new", "--name", name))
    ctl.wait_mounted(sid, "left")
    assert _node(ctl, sid)["name"] == name and _node(ctl, sid)["active"] is True
    workspace = _name("cli-made")
    background = _json(
        cli("session", "new", "--no-select", "--workspace-name", workspace, "--create-workspace", "--json")
    )
    assert _node(ctl, background["id"])["active"] is False and _node(ctl, sid)["active"] is True
    made = next(node for node in workspaces(ctl.tree()) if node["name"] == workspace)
    assert [session["id"] for session in made["sessions"]] == [background["id"]]

    copy = _created(cli("session", "duplicate", "--target", sid))
    ctl.wait_mounted(copy, "left")
    assert _node(ctl, copy)["name"] != name
    _ok(cli("session", "select", "--target", background["id"]), "ok\n")
    assert _node(ctl, background["id"])["active"] is True

    _ok(cli("session", "close", "--target", copy), "ok\n")
    # A batch close answers with the count it closed.
    _ok(cli("session", "close", "--target", sid, "--target", background["id"]), "2 sessions\n")
    assert {sid, copy, background["id"]}.isdisjoint(
        s["id"] for node in workspaces(ctl.tree()) for s in node["sessions"]
    )
    assert_ok(ctl.request("workspace.delete", made["id"]))
    _refused(cli("session", "close", "--target", UNKNOWN), f"no such session: {UNKNOWN}")
    _refused_json(cli("session", "close", "--target", UNKNOWN, "--json"), f"no such session: {UNKNOWN}")
    _usage(cli("session", "new", "--workspace", "active", "--workspace-name", "x"))


@pytest.mark.covers(
    "cli:session rename", "cli:session reveal", "cli:session flag", "cli:session context", "cli:session seen"
)
def test_session_metadata(cli, ctl):
    sid = _new(ctl)
    _ok(cli("session", "rename", "named by cli", "--target", sid), "ok\n")
    _ok(cli("session", "reveal", "--target", sid), "ok\n")
    assert _node(ctl, sid)["name"] == "named by cli"
    _ok(cli("session", "flag", "on", "--target", sid), "ok\n")
    assert _node(ctl, sid)["flagged"] is True
    assert _json(cli("session", "flag", "off", "--target", sid, "--json")) == {"id": sid}
    assert _node(ctl, sid)["flagged"] is False
    _ok(cli("session", "context", "what it is for", "--target", sid), "ok\n")
    assert _node(ctl, sid)["context"] == "what it is for"
    _ok(cli("session", "context", "--clear", "--target", sid), "ok\n")
    assert_absent(_node(ctl, sid), "context")
    assert_ok(ctl.request("notify", sid, body="seen by cli"))
    ctl.wait_tree(lambda tree: find_session(tree, sid).get("unseen", 0) > 0)
    _ok(cli("session", "seen", "--target", sid), "ok\n")
    assert_absent(_node(ctl, sid), "unseen")
    assert_ok(ctl.request("session.select", sid))
    _refused(cli("session", "rename", "x", "--target", UNKNOWN), f"no such session: {UNKNOWN}")
    assert_ok(ctl.request("session.close", sid))


@pytest.mark.covers("cli:session status")
def test_session_status_carries_its_overrides(cli, ctl, adapter):
    sid = _new(ctl)
    _ok(cli("session", "status", "active", "--blink", "--color", "#ff8800", "--pane", "left", "--target", sid), "ok\n")
    assert_subset(_node(ctl, sid), {"status": "active", "statusBlink": True, "statusColor": "#ff8800"})
    assert _json(cli("session", "status", "idle", "--target", sid, "--json")) == {"id": sid}
    assert _node(ctl, sid).get("status", "idle") == "idle"
    _refused(cli("session", "status", "busy", "--target", sid), "invalid status")
    assert_ok(ctl.request("session.close", sid))


@pytest.mark.covers("cli:session move", "cli:session go")
def test_session_move_and_go(cli, ctl):
    wid = assert_ok(ctl.request("workspace.new", name=_name("cli-move")))["id"]
    other = assert_ok(ctl.request("workspace.new", name=_name("cli-move")))["id"]
    try:
        first, second = _new(ctl, workspace=wid), _new(ctl, workspace=wid)
        _ok(cli("session", "move", "--to", "top", "--target", second), "ok\n")
        assert [s["id"] for s in _workspace(ctl, wid)["sessions"]] == [second, first]
        _ok(cli("session", "move", other, "--target", first), "ok\n")
        assert [s["id"] for s in _workspace(ctl, other)["sessions"]][-1] == first
        _usage(cli("session", "move", "--to", "top", "--after", second, "--target", first))

        assert_ok(ctl.request("session.select", second))
        landed = _json(cli("session", "go", "--to", "next", "--json"))["id"]
        assert _node(ctl, landed)["active"] is True and landed != second
        _ok(cli("session", "go", "--to", "prev"), "ok\n")
        assert _node(ctl, second)["active"] is True
    finally:
        for target in (wid, other):
            ctl.request("workspace.delete", target)


@pytest.mark.covers(
    "cli:session split",
    "cli:session split close",
    "cli:session swap",
    "cli:session focus",
    "cli:session resize",
    "cli:session scratch",
)
def test_session_panes(cli, ctl, adapter):
    sid = _new(ctl)
    _ok(cli("session", "split", "on", "--axis", "horizontal", "--target", sid), "ok\n")
    ctl.wait_mounted(sid, "left", "right")
    assert_subset(_node(ctl, sid), {"split": True, "hasSplit": True, "splitAxis": "horizontal"})
    left = next(s["paneID"] for s in _node(ctl, sid)["surfaces"] if s["kind"] == "left")
    _ok(cli("session", "swap", "--target", sid), "ok\n")
    ctl.wait_tree(
        lambda tree: next(s for s in find_session(tree, sid)["surfaces"] if s["kind"] == "right")["paneID"] == left
    )
    _ok(cli("session", "focus", "left", "--target", sid), "ok\n")
    assert _node(ctl, sid)["splitFocused"] is False

    resized = cli("session", "resize", "--split-ratio", "0.3", "--target", sid)
    if adapter.has("resize-echoes-ratio"):
        _ok(resized, "0.300\n")
    else:
        # The Linux fork answers without the applied ratio, so its CLI prints the bare acknowledgement.
        _ok(resized, "ok\n")
    assert _node(ctl, sid)["splitRatio"] == pytest.approx(0.3, abs=1e-6)
    _usage(cli("session", "resize", "--target", sid))

    _ok(cli("session", "split", "--target", sid), "ok\n")  # the default subcommand toggles
    assert_subset(_node(ctl, sid), {"split": False, "hasSplit": True})
    _ok(cli("session", "split", "close", "--target", sid), "ok\n")
    ctl.wait_tree(lambda tree: "hasSplit" not in find_session(tree, sid))

    _ok(cli("session", "scratch", "on", "--target", sid), "ok\n")
    ctl.wait_mounted(sid, "scratch")
    assert _node(ctl, sid)["scratch"] is True
    _ok(cli("session", "scratch", "off", "--target", sid), "ok\n")
    assert _node(ctl, sid)["scratch"] is False
    _refused(cli("session", "swap", "--target", sid), "session has no split pane")
    assert_ok(ctl.request("session.close", sid))


@pytest.mark.covers(
    "cli:session background image",
    "cli:session background text",
    "cli:session background color",
    "cli:session background clear",
)
def test_session_background(cli, ctl):
    sid = _new(ctl)
    image = ctl.instance.root / "cli-background.png"
    image.write_bytes(PNG)
    _ok(cli("session", "background", "image", os.fspath(image), "--opacity", "0.5", "--target", sid), "ok\n")
    assert_subset(_node(ctl, sid)["background"], {"kind": "image", "opacity": 0.5})
    _ok(cli("session", "background", "text", "WATERMARK", "--color", "#336699", "--target", sid), "ok\n")
    assert_subset(_node(ctl, sid)["background"], {"kind": "text", "text": "WATERMARK"})
    _ok(cli("session", "background", "color", "#102030", "--target", sid), "ok\n")
    assert _node(ctl, sid)["background"] == {"kind": "color", "colorHex": "#102030"}
    assert _json(cli("session", "background", "clear", "--target", sid, "--json")) == {"id": sid}
    assert_absent(_node(ctl, sid), "background")
    _usage(cli("session", "background", "color", "blue", "--target", sid))
    assert_ok(ctl.request("session.close", sid))


# A 1x1 PNG.
PNG = bytes.fromhex(
    "89504e470d0a1a0a0000000d4948445200000001000000010806000000"
    "1f15c4890000000d49444154789c6360f8cfc0f01f0005000201e221bc330000000049454e44ae426082"
)


@pytest.mark.covers("cli:session restore")
def test_session_restore_prints_the_note_or_ok(cli, ctl):
    sid = _new(ctl)
    result = _json(cli("session", "restore", "echo pinned", "--target", sid, "--json"))
    assert result["id"] == sid
    _rendered(cli("session", "restore", "echo pinned", "--target", sid), result)
    assert _node(ctl, sid)["restoreCommand"] == "echo pinned"
    _ok(cli("session", "restore", "--clear", "--target", sid), "ok\n")
    assert_absent(_node(ctl, sid), "restoreCommand")
    _usage(cli("session", "restore", "--none", "--clear", "--target", sid))
    assert_ok(ctl.request("session.close", sid))


@pytest.mark.covers(
    "cli:session type", "cli:session text", "cli:session search", "cli:session select-all", "cli:session copy"
)
@pytest.mark.requires("posix-shell", reason="the typed line runs in /bin/sh")
def test_session_type_text_search_and_copy(cli, ctl):
    sid = _new(ctl)
    marker = f"CLI{uuid.uuid4().hex[:8].upper()}"
    # The command line carries the marker split in two, so only the printed line matches it.
    _ok(
        cli("session", "type", "--stdin", "--target", sid, input=f"printf '%s%s\\n' {marker[:4]} {marker[4:]}\n"),
        "ok\n",
    )
    ctl.wait_text(sid, rf"(?m)^{marker}\n.+")
    text = _json(cli("session", "text", "--lines", "2", "--target", sid, "--json"))
    assert text["text"].startswith(f"{marker}\n")
    _rendered(cli("session", "text", "--lines", "2", "--target", sid), {"text": text["text"]})

    search = ctl.wait_response("session.search", sid, lambda r: assert_ok(r).get("count", 0) >= 1, text=marker)
    _rendered(cli("session", "search", marker, "--target", sid), search.result)
    _ok(cli("session", "search", "--close", "--target", sid), "ok\n")

    _ok(cli("session", "select-all", "--target", sid), "ok\n")
    copied = _json(cli("session", "copy", "--target", sid, "--json"))["text"]
    assert marker in copied
    _rendered(cli("session", "copy", "--target", sid), {"text": copied})
    _usage(cli("session", "text", "--all", "--lines", "2", "--target", sid))
    _ok(cli("session", "type", "--target", sid, "--", "--dash"), "ok\n")  # after --, a leading dash is text
    ctl.wait_text(sid, r"--dash")
    assert_ok(ctl.request("session.close", sid))


@pytest.mark.covers("cli:session paste", "cli:session restart", "cli:session lead")
def test_session_commands_refused_outside_their_state(cli, ctl):
    sid = _new(ctl)
    _usage(cli("session", "paste", "--pane", "middle", "--target", sid), "--pane must be left, right, or scratch")
    _refused(cli("session", "paste", "--target", UNKNOWN), f"no such session: {UNKNOWN}")
    _refused(cli("session", "lead", "--target", sid), "pane has no lead to take")
    _refused(
        cli("session", "restart", "--pane", "left", "--target", sid),
        "session.restart needs Live sessions mode; this pane has no live shell to replace",
    )
    present = cli("zmx", "present", sid)
    message = "session is not live-backed, so nothing can be attached to it"
    assert (present.returncode, present.stdout, present.stderr) == (1, "", f"Error: {message}\n")
    assert_ok(ctl.request("session.close", sid))


# -- overlays and HUDs


def _overlay_up(ctl, sid):
    ctl.wait_tree(lambda tree: find_session(tree, sid)["overlay"] is True)


@pytest.mark.covers(
    "cli:session overlay open",
    "cli:session overlay resize",
    "cli:session overlay text",
    "cli:session overlay close",
    "cli:session overlay result",
    "cli:session overlay copy",
)
def test_overlay_program(cli, ctl):
    sid = _new(ctl)
    marker = f"OVL{uuid.uuid4().hex[:8].upper()}"
    _ok(cli("session", "overlay", "open", "--size-percent", "60", "--target", sid, f"echo {marker}; exec cat"), "ok\n")
    _overlay_up(ctl, sid)
    assert _node(ctl, sid)["overlaySizePercent"] == 60
    _refused(cli("session", "overlay", "result", "--target", sid), "overlay still running")
    _ok(cli("session", "overlay", "resize", "--full", "--target", sid), "ok\n")
    assert_absent(_node(ctl, sid), "overlaySizePercent")
    _usage(cli("session", "overlay", "resize", "--target", sid))

    shown = ctl.wait_response("session.overlay.text", sid, lambda r: r.ok and marker in r.result["text"])
    _rendered(cli("session", "overlay", "text", "--target", sid), {"text": shown.result["text"]})
    _refused(cli("session", "overlay", "copy", "--target", sid), "no selection")
    _ok(cli("session", "overlay", "close", "--target", sid), "ok\n")
    assert _node(ctl, sid)["overlay"] is False
    _refused(cli("session", "overlay", "close", "--target", UNKNOWN), f"no such session: {UNKNOWN}")

    # --block waits for the program and exits with its status, printing nothing of its own.
    blocked = cli("session", "overlay", "open", "--block", "--target", sid, "exit 5")
    assert (blocked.returncode, blocked.stdout, blocked.stderr) == (5, "", "")
    result = _json(cli("session", "overlay", "result", "--target", sid, "--json"))
    assert result == {"id": sid, "exitCode": 5}
    _rendered(cli("session", "overlay", "result", "--target", sid), result)
    _usage(cli("session", "overlay", "open", "--block", "--wait", "--target", sid, "true"))
    assert_ok(ctl.request("session.close", sid))


def _page(ctl, name):
    path = ctl.instance.root / f"{name}.html"
    path.write_text(f"<!doctype html><title>{name}</title><body><p>{name}</p></body>")
    return path


@pytest.mark.covers(
    "cli:session overlay open",
    "cli:session overlay reload",
    "cli:session overlay navigate",
    "cli:session overlay submit",
    "cli:session overlay result",
)
def test_overlay_page_blocks_until_submitted(cli, ctl):
    sid = _new(ctl)
    page = _page(ctl, _name("cli-page"))
    with cli.spawn("session", "overlay", "open", "--html", os.fspath(page), "--block", "--target", sid) as waiting:
        ctl.wait_tree(lambda tree: find_session(tree, sid).get("htmlOverlays", [{}])[0].get("state") == "loaded")
        page_id = _node(ctl, sid)["htmlOverlays"][0]["id"]
        _ok(cli("session", "overlay", "reload", "--target", sid), "ok\n")
        _ok(cli("session", "overlay", "reload", "--current", "--target", sid), "ok\n")
        _refused(cli("session", "overlay", "navigate", "back", "--target", sid), "no page to go back to")
        _usage(cli("session", "overlay", "navigate", "sideways", "--target", sid))
        _ok(cli("session", "overlay", "submit", "--value", "picked", "--target", sid), "ok\n")
        finished = waiting.finish()
    outcome = {"pageID": page_id, "outcome": "submitted", "value": "picked"}
    assert (finished.returncode, json.loads(finished.stdout), finished.stderr) == (0, outcome, "")
    # A one-shot read of the page's outcome prints the same object.
    read = cli("session", "overlay", "result", "--page", page_id)
    assert (read.returncode, json.loads(read.stdout)) == (0, outcome)
    _refused(cli("session", "overlay", "submit", "--value", "late", "--target", sid), "no overlay")
    assert_ok(ctl.request("session.close", sid))


@pytest.mark.covers("cli:session overlay open", "cli:session overlay close")
def test_overlay_page_dismissed_exits_2(cli, ctl):
    sid = _new(ctl)
    page = _page(ctl, _name("cli-page"))
    with cli.spawn("session", "overlay", "open", "--html", os.fspath(page), "--block", "--target", sid) as waiting:
        ctl.wait_tree(lambda tree: find_session(tree, sid).get("htmlOverlays", [{}])[0].get("state") == "loaded")
        page_id = _node(ctl, sid)["htmlOverlays"][0]["id"]
        _ok(cli("session", "overlay", "close", "--target", sid), "ok\n")
        finished = waiting.finish()
    assert (finished.returncode, json.loads(finished.stdout)) == (2, {"pageID": page_id, "outcome": "dismissed"})
    assert_ok(ctl.request("session.close", sid))


@pytest.mark.covers("cli:session hud", "cli:session hud update", "cli:session hud close")
def test_hud(cli, ctl):
    sid = _new(ctl)
    _ok(cli("session", "hud", "first", "--detail", "waiting", "--position", "top", "--target", sid), "ok\n")
    assert_subset(_node(ctl, sid)["hud"], {"message": "first", "detail": "waiting", "position": "top-center"})
    _ok(cli("session", "hud", "update", "second", "--target", sid), "ok\n")
    assert _node(ctl, sid)["hud"]["message"] == "second"
    assert _json(cli("session", "hud", "close", "--target", sid, "--json")) == {"id": sid}
    assert_absent(_node(ctl, sid), "hud")
    _refused(cli("session", "hud", "update", "third", "--target", sid), "no hud")
    _usage(cli("session", "hud", "open", "--spinner-style", "wheel", "--target", sid, "x"))
    assert_ok(ctl.request("session.close", sid))


# -- windows, surfaces and the dashboard


def _window(ctl, wid):
    (node,) = [node for node in assert_ok(ctl.request("window.list"))["windows"] if node["id"] == wid]
    return node


def _window_reads(ctl, wid, timeout=10.0, **expected):
    def accept(response):
        (node,) = [node for node in assert_ok(response)["windows"] if node["id"] == wid]
        assert_subset(node, expected)
        return True

    ctl.wait_response("window.list", None, accept, timeout)


@pytest.mark.covers(
    "cli:window new",
    "cli:window list",
    "cli:window rename",
    "cli:window resize",
    "cli:window zoom",
    "cli:window fullscreen",
    "cli:window minimize",
    "cli:window move",
    "cli:window close",
    "cli:window select",
    "cli:window go",
    "cli:window delete",
)
def test_window_commands(cli, ctl, adapter):
    wid = _created(cli("window", "new", "cli-second"))
    try:
        listing = _json(cli("window", "list", "--json"))
        assert [node["id"] for node in listing["windows"]][-1] == wid
        _rendered(cli("window", "list"), assert_ok(ctl.request("window.list")))
        _ok(cli("window", "rename", wid, "renamed"), "ok\n")
        assert _window(ctl, wid)["name"] == "renamed"
        _ok(cli("window", "resize", wid, "--width", "900", "--height", "640"), "900 640\n")
        _ok(cli("window", "zoom", wid), "ok\n")
        _window_reads(ctl, wid, zoomed=True)
        _ok(cli("window", "zoom", wid), "ok\n")
        _window_reads(ctl, wid, zoomed=False)
        for fullscreen in (True, False):
            # A dropped macOS toggle is sent again, as in protocol/test_window.py's _toggle_until.
            for attempt in range(3):
                _ok(cli("window", "fullscreen", wid), "ok\n")
                try:
                    _window_reads(ctl, wid, timeout=5.0, fullscreen=fullscreen)
                    break
                except WaitTimeout:
                    if attempt == 2:
                        raise
        _ok(cli("window", "minimize", wid, "off"), "ok\n")
        moved = cli("window", "move", wid, "--x", "40", "--y", "60")
        if adapter.has("window-geometry"):
            _ok(moved, "ok\n")
            _window_reads(ctl, wid, geometry={"x": 40, "y": 60})
        else:
            _refused(moved, "window.move is not supported on this platform (the compositor controls window position)")
        _ok(cli("window", "select", wid), "ok\n")
        _ok(cli("window", "go", "--to", "next"), "ok\n")
        _ok(cli("window", "close", wid), "ok\n")
        _window_reads(ctl, wid, open=False)
        _rendered(cli("window", "list"), assert_ok(ctl.request("window.list")))
        _usage(cli("window", "resize", wid, "--width", "900"))
        _refused(cli("window", "rename", UNKNOWN, "x"), f"no such window: {UNKNOWN}")
    finally:
        if wid in window_ids(ctl.request("window.list")):
            _ok(cli("window", "delete", wid), "ok\n")
    assert wid not in window_ids(ctl.request("window.list"))
    _refused(cli("window", "delete", UNKNOWN), f"no such window: {UNKNOWN}")


@pytest.mark.covers("cli:surface zoom", "cli:surface cursor", "cli:dashboard")
def test_surface_and_dashboard(cli, ctl):
    sid = _new(ctl)
    surface = f"surface:{sid}:left"
    _ok(cli("surface", "zoom", "show", "--target", surface), "ok\n")
    assert ctl.tree()["tree"]["zoomedSurface"] == surface
    _ok(cli("surface", "zoom", "hide", "--target", surface), "ok\n")
    assert_absent(ctl.tree()["tree"], "zoomedSurface")
    cursor = _json(cli("surface", "cursor", "--target", surface, "--json"))
    assert cursor["id"] == surface and isinstance(cursor["cursor"]["column"], int)
    _rendered(cli("surface", "cursor", "--target", surface), {"cursor": cursor["cursor"]})

    _ok(cli("dashboard", sid), "ok\n")
    ctl.wait_tree(lambda tree: tree["tree"].get("dashboardMembers") == [f"{sid}:left"])
    _ok(cli("dashboard", "--close"), "ok\n")
    assert_absent(ctl.tree()["tree"], "dashboardMembers")
    _usage(cli("dashboard", "--close", "--mru"))
    assert_ok(ctl.request("session.close", sid))


# -- pick and ask


@pytest.mark.covers("cli:pick", "cli:pick result", "cli:pick cancel")
def test_pick_without_blocking(cli, ctl):
    opened = cli("pick", "--no-block", "--prompt", "choose", input="a=Alpha\nb=Beta\n")
    pick_id = json.loads(_ok(opened))["id"]
    assert opened.stdout == json.dumps({"id": pick_id}, separators=(",", ":")) + "\n"
    assert ctl.tree()["tree"]["pickPending"] == pick_id
    pending = cli("pick", "result", pick_id)
    assert (pending.returncode, json.loads(pending.stdout), pending.stderr) == (1, {"result": "pending"}, "")
    _ok(cli("pick", "cancel", pick_id), "ok\n")
    cancelled = cli("pick", "result", pick_id)
    assert (cancelled.returncode, json.loads(cancelled.stdout)) == (2, {"result": "cancelled"})
    assert_absent(ctl.tree()["tree"], "pickPending")
    _refused(cli("pick", "result", UNKNOWN), f"unknown pick: {UNKNOWN}")
    _refused(cli("pick", "cancel", UNKNOWN), f"unknown pick: {UNKNOWN}")


@pytest.mark.covers("cli:pick", "cli:pick cancel")
def test_pick_blocks_until_cancelled(cli, ctl):
    with cli.spawn("pick", input='[{"id":"a","label":"Alpha"},{"id":"b","label":"Beta"}]') as waiting:
        pick_id = ctl.wait_tree(lambda tree: tree["tree"].get("pickPending"))["tree"]["pickPending"]
        assert_ok(ctl.request("pick.cancel", pick_id))
        finished = waiting.finish()
    assert (finished.returncode, json.loads(finished.stdout), finished.stderr) == (2, {"result": "cancelled"}, "")


@pytest.mark.covers("cli:ask", "cli:ask result", "cli:ask cancel")
def test_ask_without_blocking(cli, ctl):
    sid = _new(ctl)
    opened = cli("ask", "--no-block", "--button", "yes=Yes", "--button", "no", "--target", sid, "Proceed?")
    ask_id = json.loads(_ok(opened))["id"]
    assert _node(ctl, sid)["ask"]["id"] == ask_id
    pending = cli("ask", "result", ask_id)
    assert (pending.returncode, json.loads(pending.stdout)) == (1, {"result": "pending"})
    _ok(cli("ask", "cancel", ask_id), "ok\n")
    cancelled = cli("ask", "result", ask_id)
    assert (cancelled.returncode, json.loads(cancelled.stdout)) == (2, {"result": "cancelled"})
    _usage(cli("ask", "--no-block", "Proceed?"), "ask requires at least one --button")
    _usage(cli("ask", "--no-block", "--button", "y", "--width", "5", "Proceed?"), "width must be 10 to 100")
    _refused(cli("ask", "result", UNKNOWN), f"unknown ask: {UNKNOWN}")
    assert_ok(ctl.request("session.close", sid))


@pytest.mark.covers("cli:ask", "cli:ask cancel")
def test_ask_blocks_until_cancelled(cli, ctl):
    sid = _new(ctl)
    with cli.spawn("ask", "--button", "ok", "--target", sid, "Wait here") as waiting:
        ask_id = ctl.wait_tree(lambda tree: find_session(tree, sid).get("ask"))
        ask_id = find_session(ask_id, sid)["ask"]["id"]
        _ok(cli("ask", "cancel", ask_id), "ok\n")
        finished = waiting.finish()
    assert (finished.returncode, json.loads(finished.stdout), finished.stderr) == (2, {"result": "cancelled"}, "")
    assert_ok(ctl.request("session.close", sid))


# -- the quick terminal, the sidebar, notify, fonts and themes


@pytest.mark.covers("cli:quick", "cli:quick type", "cli:quick text")
def test_quick_terminal(cli, ctl, adapter):
    _ok(cli("quick", "show"), "ok\n")
    ctl.wait_tree(lambda tree: tree["tree"]["quickVisible"] is True)
    if adapter.has("posix-shell"):
        marker = f"QCK{uuid.uuid4().hex[:8].upper()}"
        _ok(cli("quick", "type", "--stdin", input=f"printf '%s%s\\n' {marker[:3]} {marker[3:]}\n"), "ok\n")
        shown = ctl.wait_response("quick.text", None, lambda r: r.ok and f"\n{marker}\n" in f"\n{r.result['text']}")
        _rendered(cli("quick", "text"), {"text": shown.result["text"]})
    _ok(cli("quick", "hide"), "ok\n")
    assert ctl.tree()["tree"]["quickVisible"] is False
    _ok(cli("quick"), "ok\n")  # the default subcommand toggles
    ctl.wait_tree(lambda tree: tree["tree"]["quickVisible"] is True)
    _ok(cli("quick", "visibility", "hide"), "ok\n")
    assert ctl.tree()["tree"]["quickVisible"] is False
    _usage(cli("quick", "text", "--all", "--lines", "1"))


@pytest.mark.covers(
    "cli:sidebar",
    "cli:sidebar mode",
    "cli:sidebar flagged-layout",
    "cli:sidebar expand",
    "cli:sidebar collapse",
    "cli:sidebar width",
)
def test_sidebar(cli, ctl):
    top = ctl.tree()["tree"]
    try:
        _ok(cli("sidebar", "hide"), "ok\n")
        assert ctl.tree()["tree"]["sidebarVisible"] is False
        _ok(cli("sidebar"), "ok\n")  # the default subcommand toggles
        assert ctl.tree()["tree"]["sidebarVisible"] is True
        _ok(cli("sidebar", "mode", "flagged"), "ok\n")
        assert ctl.tree()["tree"]["sidebarMode"] == "flagged"
        _ok(cli("sidebar", "mode", "tree"), "ok\n")
        result = _json(cli("sidebar", "flagged-layout", "tree", "--json"))
        assert result == {"text": "tree"} and ctl.tree()["tree"]["sidebarFlaggedLayout"] == "tree"
        _rendered(cli("sidebar", "flagged-layout", "toggle"), {"text": "flat"})
        _ok(cli("sidebar", "expand"), "ok\n")
        _ok(cli("sidebar", "collapse"), "ok\n")
        _ok(cli("sidebar", "width", "300"), "300.0\n")
        assert _json(cli("sidebar", "width", "301.5", "--json")) == {"sidebarWidth": 301.5}
        _rendered(cli("sidebar", "width", "301.5"), {"sidebarWidth": 301.5})
        ctl.wait_tree(lambda tree: tree["tree"]["sidebarWidth"] == pytest.approx(301.5))
        _usage(cli("sidebar", "width", "wide"))
        _usage(cli("sidebar", "mode", "grid"))
    finally:
        assert_ok(ctl.request("sidebar.flagged-layout", mode=top["sidebarFlaggedLayout"]))
        assert_ok(ctl.request("sidebar.width", sidebarWidth=top["sidebarWidth"]))


@pytest.mark.covers("cli:notify")
def test_notify_raises_the_unseen_count(cli, ctl):
    sid = _new(ctl)
    assert_ok(ctl.request("session.select", _new(ctl)))
    cursor = ctl.events_baseline()
    _ok(cli("notify", "cli body", "--title", "cli title", "--target", sid), "ok\n")
    ctl.wait_event(cursor, "notify", {"session": sid, "payload": {"title": "cli title", "body": "cli body"}})
    assert _node(ctl, sid)["unseen"] >= 1
    # Selecting the session withdraws the desktop banner the notification posted.
    assert_ok(ctl.request("session.select", sid))
    _usage(cli("notify", "--target", sid))
    assert_ok(ctl.request("session.close", sid))


@pytest.mark.covers("cli:font inc", "cli:font dec", "cli:font reset")
def test_font(cli, ctl):
    sid = _new(ctl)
    size = _node(ctl, sid)["fontSize"]
    _ok(cli("font", "inc", "--target", sid), "ok\n")
    assert _node(ctl, sid)["fontSize"] > size
    assert _json(cli("font", "dec", "--target", sid, "--json")) == {"id": sid}
    assert _node(ctl, sid)["fontSize"] == size
    _ok(cli("font", "inc", "--target", sid), "ok\n")
    _ok(cli("font", "reset", "--target", sid), "ok\n")
    assert _node(ctl, sid)["fontSize"] == size
    _usage(cli("font", "inc", "--pane", "middle", "--target", sid))
    assert_ok(ctl.request("session.close", sid))


@pytest.mark.covers("cli:theme list", "cli:theme set")
def test_theme(cli, ctl):
    before = assert_ok(ctl.request("theme.list"))
    listing = _json(cli("theme", "list", "--json"))
    assert listing == before
    _rendered(cli("theme", "list"), listing)
    name = next(theme for theme in listing["themes"] if theme not in (listing.get("theme"), listing.get("light")))
    try:
        result = _json(cli("theme", "set", name, "--json"))
        _rendered(cli("theme", "set", name), result)
        after = assert_ok(ctl.request("theme.list"))
        assert name in (after.get("theme"), after.get("light"))
        _rendered(cli("theme", "list"), after)
        _refused(cli("theme", "set", "No Such Theme"), "unknown theme: No Such Theme")
    finally:
        restore = before.get("light") or before.get("theme")
        assert_ok(ctl.request("theme.set", **({"name": restore} if restore else {})))


# -- keymap, hooks, browser, config


@pytest.fixture
def config_files(ctl):
    """The instance's `keymap.conf` and `hooks.conf`, removed and reloaded afterwards."""
    folder = ctl.instance.state_dir / "config"
    yield folder / "keymap.conf", folder / "hooks.conf"
    for name, cmd in (("keymap.conf", "keymap.reload"), ("hooks.conf", "hooks.reload")):
        (folder / name).unlink(missing_ok=True)
        assert_ok(ctl.request(cmd))


@pytest.mark.covers("cli:keymap reload", "cli:keymap list", "cli:keymap run")
def test_keymap(cli, ctl, config_files):
    keymap, _ = config_files
    marker = ctl.instance.root / "cli-keymap-ran"
    keymap.write_text(
        f'command "mark" ctrl+shift+f11 --error-hud printf \'%s\' "$AGT_SESSION_ID" > {marker}\n'
        "map ctrl+shift+f12 new_session\nbogus line\n"
    )
    _ok(cli("keymap", "reload"), "1 diagnostic(s)\n")
    listing = _json(cli("keymap", "list", "--json"))
    assert [command["name"] for command in listing["keymap"]["commands"]] == ["mark"]
    _rendered(cli("keymap", "list"), listing)
    keymap.write_text(f'command "mark" printf \'%s\' "$AGT_SESSION_ID" > {marker}\n')
    assert _json(cli("keymap", "reload", "--json")) == assert_ok(ctl.request("keymap.reload"))
    _rendered(cli("keymap", "reload"), assert_ok(ctl.request("keymap.reload")))

    sid = _new(ctl)
    _ok(cli("keymap", "run", "mark", "--target", sid), "ok\n")
    assert wait_until(lambda: marker.exists() and marker.read_text()) == sid
    _refused(cli("keymap", "run", "nope"), "no custom command named nope")
    _usage(cli("keymap", "run"))
    assert_ok(ctl.request("session.close", sid))


@pytest.mark.covers("cli:hooks reload", "cli:hooks list")
def test_hooks(cli, ctl, config_files):
    _, hooks = config_files
    hooks.write_text("on session.created true\non session.renamed true\n")
    _ok(cli("hooks", "reload"), "1 diagnostic(s)\n")
    listing = _json(cli("hooks", "list", "--json"))
    assert [hook["command"] for hook in listing["hooks"]["hooks"]] == ["true"]
    _rendered(cli("hooks", "list"), listing)
    hooks.unlink()
    _ok(cli("hooks", "reload"), "ok\n")
    _rendered(cli("hooks", "list"), assert_ok(ctl.request("hooks.list")))
    _usage(cli("hooks", "list", "--window", "active"))


@pytest.mark.covers("cli:browser links", "cli:browser clear", "cli:config reload")
def test_browser_and_config(cli, ctl):
    mode = assert_ok(ctl.request("browser.links"))["text"]
    try:
        _rendered(cli("browser", "links"), {"text": mode})
        assert _json(cli("browser", "links", "overlay", "--json")) == {"text": "overlay"}
        assert ctl.tree()["tree"]["linkOpenMode"] == "overlay"
        _ok(cli("browser", "links", "browser"), "browser\n")
        _usage(cli("browser", "links", "tab"))
    finally:
        assert_ok(ctl.request("browser.links", mode=mode))
    _ok(cli("browser", "clear"), "ok\n")
    reloaded = _json(cli("config", "reload", "--json"))
    _rendered(cli("config", "reload"), reloaded)


# -- restore and the zmx group


@pytest.mark.covers("cli:restore mode", "cli:restore capture", "cli:restore clear")
def test_restore(cli, ctl):
    status = _json(cli("restore", "mode", "--json"))
    _rendered(cli("restore", "mode"), status)
    configured = status["restore"]["configured"]
    try:
        changed = _json(cli("restore", "mode", "rerun", "--json"))
        assert changed["restore"]["configured"] == "rerun"
        _rendered(cli("restore", "mode"), assert_ok(ctl.request("restore.mode")))
        captured = _json(cli("restore", "capture", "--json"))
        _rendered(cli("restore", "capture"), assert_ok(ctl.request("restore.capture")))
        assert isinstance(captured["count"], int) and captured["text"]
        _ok(cli("restore", "clear"), "ok\n")
        _usage(cli("restore", "mode", "sometimes"))
    finally:
        assert_ok(ctl.request("restore.mode", mode=configured))
    if configured != "rerun":
        _refused(
            cli("restore", "capture"), f"restore.capture requires rerun mode; configured restore mode is {configured}"
        )


@pytest.mark.covers("cli:zmx list", "cli:zmx prune", "cli:zmx tree", "cli:zmx screen", "cli:zmx kill")
def test_zmx_reads_outside_live(cli, ctl):
    inventory = _json(cli("zmx", "list", "--json"))
    _rendered(cli("zmx", "list"), assert_ok(ctl.request("zmx.list")))
    assert inventory["zmx"]["restore"] == assert_ok(ctl.request("restore.mode"))["restore"]
    prune = _json(cli("zmx", "prune", "--json"))
    _rendered(cli("zmx", "prune"), prune)
    remote = _json(cli("zmx", "tree", "--json"))
    assert remote["remote"]["sessions"] == []
    _ok(cli("zmx", "tree"), "no attachable sessions\n")
    _refused(cli("zmx", "screen", "nope"), "could not read the zmx screen of nope")
    _usage(
        cli("zmx", "kill", "--pane", "left", "--target", UNKNOWN), "--force is required to destroy a running process"
    )
    _refused(
        cli("zmx", "kill", "--pane", "left", "--force", "--target", UNKNOWN),
        f"no left pane daemon for session {UNKNOWN}",
    )


@pytest.mark.covers("cli:zmx reset", "cli:zmx attach", "cli:zmx present", "cli:session overlay run-job")
def test_zmx_commands_refused_before_acting(cli, ctl):
    _usage(cli("zmx", "reset"), "--force is required to end the processes in the affected live sessions")
    _refused(cli("zmx", "attach", "bad host", UNKNOWN), "invalid host")
    present = cli("zmx", "present", UNKNOWN)
    assert (present.returncode, present.stdout, present.stderr) == (1, "", f"Error: no such session: {UNKNOWN}\n")
    _refused(cli("session", "overlay", "run-job", UNKNOWN), "invalid job id")


# -- Live sessions

LIVE = pytest.mark.requires(
    "live-sessions", reason="needs panes in zmx daemons, which the references create only for a zsh login shell"
)
PRESENTATION_HELLO = {
    "kind": "hello",
    "gen": 0,
    "rev": 0,
    "hello": {"version": 1, "kinds": ["status", "hud", "notify", "context", "layout"], "mode": "mirror"},
}


@pytest.fixture(scope="module")
def live(adapter):
    instance = adapter.launch(settings={"restoreMode": "live"})
    try:
        ctl = Ctl(instance)
        assert assert_ok(ctl.request("restore.mode"))["restore"]["active"] == "live"
        yield ctl
    finally:
        adapter.stop(instance)


def _rows(response, sid):
    return {row["pane"]: row for row in assert_ok(response)["zmx"]["entries"] if row.get("sessionID") == sid}


def _live_rows(ctl, sid):
    return _rows(ctl.request("zmx.list"), sid)


def _live_session(ctl):
    sid = _new(ctl)
    ctl.wait_tree(lambda tree: find_session(tree, sid).get("backedByZmx") is True)
    ctl.wait_response("zmx.list", None, lambda r: _rows(r, sid).get("left", {}).get("observation") == "running")
    return sid


@LIVE
@pytest.mark.covers("cli:zmx list", "cli:zmx screen", "cli:session lead", "cli:session restart", "cli:zmx kill")
def test_live_pane_commands(cli, live):
    cli = cli.on(live.instance)
    sid = _live_session(live)
    # Upstream's count drops to 0 for a moment while a pane's client attaches: render a settled list.
    live.wait_response(
        "zmx.list", None, lambda r: all(row.get("clients", 0) >= 1 for row in assert_ok(r)["zmx"]["entries"])
    )
    _rendered(cli("zmx", "list"), assert_ok(live.request("zmx.list")))
    daemon = _live_rows(live, sid)["left"]["daemon"]
    marker = f"LIVE{uuid.uuid4().hex[:8].upper()}"
    assert_ok(live.request("session.type", sid, text=f"printf '%s%s\\n' {marker[:4]} {marker[4:]}\n"))
    live.wait_text(sid, rf"(?m)^{marker}$")
    screen = _json(cli("zmx", "screen", daemon, "--json"))
    assert f"\n{marker}\n" in f"\n{screen['text']}\n"
    _rendered(cli("zmx", "screen", daemon), assert_ok(live.request("zmx.screen", name=daemon)))

    # Once the pane's zmx has reported its role: before that there is no lead to take.
    live.wait_tree(
        lambda tree: next(s for s in find_session(tree, sid)["surfaces"] if s["kind"] == "left").get("lead") == "leader"
    )
    _ok(cli("session", "lead", "--target", sid), "ok\n")
    restarted = _json(cli("session", "restart", "--pane", "left", "--command", "cat", "--target", sid, "--json"))
    assert restarted["restart"]["newPid"] != restarted["restart"]["oldPid"]
    _ok(cli("zmx", "kill", "--pane", "left", "--force", "--target", sid), f"killed {daemon}\n")
    live.wait_response(
        "zmx.list", None, lambda r: all(row["daemon"] != daemon for row in assert_ok(r)["zmx"]["entries"])
    )


@LIVE
@pytest.mark.covers("cli:zmx present")
def test_live_present_bridges_the_stream(cli, live):
    cli = cli.on(live.instance)
    sid = _live_session(live)
    with cli.spawn("zmx", "present", sid, interactive=True) as bridge:

        def read(seconds):
            return json.loads(bridge.line(seconds))

        def send(frame):
            bridge.send(json.dumps(frame) + "\n")

        send(PRESENTATION_HELLO)
        assert read(10.0)["kind"] == "hello"
        wait_frame(read, "snapshot", send)
        live.wait_tree(lambda tree: find_session(tree, sid).get("presenters") == {"mirrors": 1})
        assert_ok(live.request("session.status", sid, status="active"))
        assert wait_frame(read, "status", send)["status"]["status"] == "active"
    live.wait_tree(lambda tree: "presenters" not in find_session(tree, sid))
    assert_ok(live.request("session.close", sid))


@LIVE
@pytest.mark.covers("cli:session overlay run-job")
def test_live_run_job_runs_the_handed_over_program(cli, live):
    cli = cli.on(live.instance)
    sid = _live_session(live)
    endpoint = assert_ok(live.request("zmx.tree"))["remote"]["endpoint"]
    with leading_client(live.instance, endpoint, _live_rows(live, sid)["left"]["daemon"]):
        live.wait_tree(
            lambda tree: (
                next(s for s in find_session(tree, sid)["surfaces"] if s["kind"] == "left").get("lead") == "follower"
            )
        )
        with live.stream("zmx.present", sid) as presenter:
            presenter.send({**PRESENTATION_HELLO, "hello": {**PRESENTATION_HELLO["hello"], "mode": "presenter"}})
            gen = wait_frame(presenter.read, "hello", presenter.send)["gen"]
            presenter.send({"kind": "presenter.acquire", "gen": gen, "rev": 1})
            wait_frame(presenter.read, "presenter.granted", presenter.send)
            assert_ok(live.request("session.overlay.open", sid, command="exit 3"))
            job = wait_frame(presenter.read, "overlay.request", presenter.send)["overlay"]["job"]
            ran = cli("session", "overlay", "run-job", job)
            assert ran.returncode == 3, (ran.stdout, ran.stderr)
            live.wait_tree(lambda tree: "remoteOverlays" not in find_session(tree, sid))
            assert assert_ok(live.request("session.overlay.result", sid))["exitCode"] == 3
            _refused(cli("session", "overlay", "run-job", job), "job not claimable")
    assert_ok(live.request("session.close", sid))
