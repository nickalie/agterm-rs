"""`surface.cursor` over `surface.zoom`'s target vocabulary, and the selection commands: `session.selectall` read
back through `session.copy`, plus the refusals of `session.paste`, whose success path is exempt (catalog.yaml).
The module shares one instance; every test closes the sessions it opens.

Selections stay inside the instance: the harness starts it with `copy-on-select = false`, so selecting never
reaches the desktop's own selection, and `session.copy` returns the text instead of writing a clipboard."""

import pytest
from agterm_conformance.match import assert_error, assert_ok, find_session

UNKNOWN = "zzz"
# Clears the screen and prints a number the echoed command line does not contain, leaving the cursor after it.
PARKED = "printf '\\033[2J\\033[H%s' \"$((10000 + 2345))\"; read parked\n"


@pytest.fixture
def opened(ctl):
    sids = []
    yield sids
    for sid in sids:
        ctl.request("session.close", sid)


@pytest.fixture
def fresh(ctl, opened):
    sid = assert_ok(ctl.request("session.new"))["id"]
    opened.append(sid)
    ctl.wait_mounted(sid, "left")
    return sid


def _surface(sid, kind):
    return f"surface:{sid}:{kind}"


def _cursor(ctl, target, column, **args):
    """Poll `surface.cursor` until it reads `column`: the reply follows the pty, which trails the keystrokes."""

    def reads(response):
        assert assert_ok(response)["cursor"] == {"column": column}
        return True

    reads.__qualname__ = f"cursor of {target} at column {column}"
    return ctl.wait_response("surface.cursor", target, reads, **args).result


def _parked(ctl, sid, pane=None):
    """Park the pane's cursor at column 5 on a cleared screen, waiting in `read`."""
    args = {} if pane is None else {"pane": pane}
    assert_ok(ctl.request("session.type", sid, text=PARKED, **args))
    ctl.wait_text(sid, r"(?m)^12345$", **args)


def _release(ctl, sid, pane=None):
    args = {} if pane is None else {"pane": pane}
    assert_ok(ctl.request("session.type", sid, text="\n", **args))


def _pane_id(ctl, sid, kind):
    (surface,) = [node for node in find_session(ctl.tree(), sid)["surfaces"] if node["kind"] == kind]
    return surface["paneID"]


# -- surface.cursor


@pytest.mark.requires("posix-shell", reason="the parked line is /bin/sh input")
@pytest.mark.covers("surface.cursor", "field:result.cursor", "field:result.id")
def test_cursor_reports_the_column_and_follows_input(ctl, fresh):
    _parked(ctl, fresh)
    left = _surface(fresh, "left")
    assert _cursor(ctl, left, 5) == {"id": left, "cursor": {"column": 5}}
    assert_ok(ctl.request("session.type", fresh, text="ab"))
    _cursor(ctl, left, 7)
    _release(ctl, fresh)


@pytest.mark.requires("posix-shell", reason="the parked line is /bin/sh input")
@pytest.mark.covers("surface.cursor", "behavior:protocol.target-resolution")
def test_cursor_without_a_target_reads_the_active_surface(ctl, fresh):
    _parked(ctl, fresh)
    assert _cursor(ctl, None, 5)["id"] == _surface(fresh, "left")
    _release(ctl, fresh)


@pytest.mark.requires("posix-shell", reason="the parked line is /bin/sh input")
@pytest.mark.covers("surface.cursor", "field:tree.session.surfaces")
def test_cursor_reads_each_pane_by_surface_id_or_pane_id(ctl, fresh):
    assert_ok(ctl.request("session.split", fresh, mode="on"))
    ctl.wait_mounted(fresh, "right")
    _parked(ctl, fresh, pane="right")
    right = _surface(fresh, "right")
    _cursor(ctl, right, 5)
    assert _cursor(ctl, fresh, 5, paneID=_pane_id(ctl, fresh, "right"))["id"] == right

    # After the right pane is parked: a scratch coming up while it lays out can leave it a few cells wide.
    assert_ok(ctl.request("session.scratch", fresh, mode="on"))
    ctl.wait_mounted(fresh, "scratch")
    _parked(ctl, fresh, pane="scratch")
    scratch = _surface(fresh, "scratch")
    _cursor(ctl, scratch, 5)
    assert _cursor(ctl, fresh, 5, paneID=_pane_id(ctl, fresh, "scratch"))["id"] == scratch
    for pane in ("right", "scratch"):
        _release(ctl, fresh, pane=pane)


@pytest.mark.covers("surface.cursor", "session.overlay.open")
def test_cursor_reads_a_program_overlay_but_not_a_hud(ctl, fresh):
    command = "printf '\\033[2J\\033[H%s' abcdef; exec cat"
    assert_ok(ctl.request("session.overlay.open", fresh, command=command))
    overlay = _surface(fresh, "overlay")
    _cursor(ctl, overlay, 6)
    assert_ok(ctl.request("session.overlay.close", fresh))
    assert_error(ctl.request("surface.cursor", overlay), f"surface not available: {overlay}")

    assert_ok(ctl.request("session.hud.open", fresh, message="cursor"))
    assert_error(ctl.request("surface.cursor", overlay), f"surface not available: {overlay}")
    assert_ok(ctl.request("session.overlay.close", fresh))


@pytest.mark.covers("surface.cursor")
def test_cursor_reads_the_quick_terminal_only_while_shown(ctl):
    assert_error(ctl.request("surface.cursor", "quick"), "surface not available: quick")
    assert_ok(ctl.request("quick", mode="show"))
    try:
        result = ctl.wait_response("surface.cursor", "quick", lambda response: response.ok).result
        assert result["id"] == "quick" and result["cursor"]["column"] >= 0
    finally:
        assert_ok(ctl.request("quick", mode="hide"))
    ctl.wait_tree(lambda tree: tree["tree"]["quickVisible"] is False)
    assert_error(ctl.request("surface.cursor", "quick"), "surface not available: quick")


@pytest.mark.covers("surface.cursor")
@pytest.mark.parametrize(
    ("target", "args", "error"),
    [
        (UNKNOWN, {}, f"invalid surface: {UNKNOWN}"),
        ("session", {}, "invalid surface: {sid}"),
        ("bogus-kind", {}, "invalid surface: surface:{sid}:bogus"),
        ("right", {}, "surface not available: surface:{sid}:right"),
        ("scratch", {}, "surface not available: surface:{sid}:scratch"),
        ("session", {"paneID": "nope"}, "unknown pane id: nope"),
        ("left", {"paneID": "nope"}, "surface.cursor: --pane-id takes a session target"),
        ("quick", {"paneID": "nope"}, "surface.cursor: --pane-id takes a session target"),
    ],
)
def test_cursor_refusals(ctl, fresh, target, args, error):
    resolved = {
        "session": fresh,
        "bogus-kind": f"surface:{fresh}:bogus",
        "right": _surface(fresh, "right"),
        "scratch": _surface(fresh, "scratch"),
        "left": _surface(fresh, "left"),
    }.get(target, target)
    assert_error(ctl.request("surface.cursor", resolved, **args), error.format(sid=fresh))


# -- selection


@pytest.mark.requires("posix-shell", reason="the marker is printed by /bin/sh")
@pytest.mark.covers("session.selectall", "session.copy", "field:result.text")
def test_selectall_selects_the_buffer_for_copy(ctl, fresh):
    assert_error(ctl.request("session.copy", fresh), "no selection")
    assert_ok(ctl.request("session.type", fresh, text="printf '%s\\n' \"sel$((1000 + 234))\"\n"))
    ctl.wait_text(fresh, r"(?m)^sel1234$")
    assert assert_ok(ctl.request("session.selectall", fresh)) == {"id": fresh}
    text = assert_ok(ctl.request("session.copy", fresh))["text"]
    assert "\nsel1234\n" in f"\n{text}\n"


@pytest.mark.requires("posix-shell", reason="the marker is printed by /bin/sh")
@pytest.mark.covers("session.selectall", "session.copy", "behavior:protocol.target-resolution")
def test_selection_commands_without_a_target_use_the_active_session(ctl, fresh):
    assert_error(ctl.request("session.copy"), "no selection")
    assert_ok(ctl.request("session.type", fresh, text="printf '%s\\n' \"act$((1000 + 567))\"\n"))
    ctl.wait_text(fresh, r"(?m)^act1567$")
    assert assert_ok(ctl.request("session.selectall")) == {"id": fresh}
    text = assert_ok(ctl.request("session.copy"))["text"]
    assert "\nact1567\n" in f"\n{text}\n"


@pytest.mark.covers("session.selectall", "session.copy", "session.paste")
@pytest.mark.parametrize("cmd", ["session.selectall", "session.copy", "session.paste"])
def test_selection_commands_refuse_an_unknown_session(ctl, cmd):
    assert_error(ctl.request(cmd, UNKNOWN), f"no such session: {UNKNOWN}")


@pytest.mark.covers("session.paste")
@pytest.mark.parametrize(
    ("pane", "error"),
    [
        ("middle", "--pane must be left, right, or scratch"),
        ("right", "session has no split pane"),
        ("split", "session has no split pane"),
        ("scratch", "session has no scratch terminal"),
    ],
)
def test_paste_refuses_a_pane_it_cannot_reach(ctl, fresh, pane, error):
    assert_error(ctl.request("session.paste", fresh, pane=pane), error)
