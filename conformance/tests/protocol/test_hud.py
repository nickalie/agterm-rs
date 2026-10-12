"""The HUD: a passive message panel in the session-wide overlay slot. Its measured size depends on the font and the
window, so only the bounds the app clamps it to are compared, never a measured value. Every program shown beside it
waits in `cat` until its overlay is closed."""

import pytest
from agterm_conformance.match import assert_absent, assert_error, assert_ok, assert_subset, find_session

UNKNOWN = "zzz"
HOLD = "exec cat"
POSITIONS = (
    "top-left|top-center|top-right|center-left|center|center-right|bottom-left|bottom-center|bottom-right|top|bottom"
)


def _new(ctl):
    sid = assert_ok(ctl.request("session.new"))["id"]
    ctl.wait_mounted(sid, "left")
    return sid


def _split(ctl):
    sid = _new(ctl)
    assert_ok(ctl.request("session.split", sid, mode="on"))
    ctl.wait_mounted(sid, "right")
    return sid


def _node(ctl, sid):
    return find_session(ctl.tree(), sid)


def _hud(ctl, sid):
    return _node(ctl, sid)["hud"]


def _open(ctl, sid, message="working", **args):
    assert assert_ok(ctl.request("session.hud.open", sid, message=message, **args))["id"] == sid
    return _hud(ctl, sid)


def _update(ctl, sid, message, **args):
    assert assert_ok(ctl.request("session.hud.update", sid, message=message, **args))["id"] == sid
    return _hud(ctl, sid)


def _close(ctl, sid):
    assert assert_ok(ctl.request("session.hud.close", sid))["id"] == sid


def _pane_id(node, kind):
    (surface,) = [surface for surface in node["surfaces"] if surface["kind"] == kind]
    return surface["paneID"]


@pytest.mark.covers(
    "session.hud.open",
    "session.hud.close",
    "field:tree.session.hud",
    "field:tree.session.overlay",
    "field:tree.session.overlaySizePercent",
)
def test_hud_holds_the_slot_without_covering_the_session(ctl):
    sid = _new(ctl)
    hud = _open(ctl, sid, "gathering options")
    assert_subset(
        hud,
        {
            "message": "gathering options",
            "spinner": "none",
            "position": "center",
            "hideAfter": 0,
            "markdown": False,
            "sticky": False,
            "frame": True,
        },
    )
    assert 10 <= hud["sizePercent"] <= 80 and 1 <= hud["heightPercent"] <= 80
    assert_absent(hud, "detail", "backgroundColor", "textColor", "fontSize", "pane")
    node = _node(ctl, sid)
    assert node["overlay"] is False and "overlay" not in {surface["kind"] for surface in node["surfaces"]}
    assert_absent(node, "overlaySizePercent")
    assert_error(ctl.request("session.overlay.result", sid), "no overlay result: the slot holds a hud")
    assert_error(ctl.request("session.overlay.text", sid), "no overlay to read: the slot holds a hud")
    assert_error(ctl.request("session.overlay.copy", sid), "no overlay to read: the slot holds a hud")

    _close(ctl, sid)
    assert_absent(_node(ctl, sid), "hud")
    assert_error(ctl.request("session.hud.close", sid), "no hud")


@pytest.mark.covers("session.hud.open", "field:tree.session.hud")
def test_hud_reads_back_every_option(ctl):
    sid = _new(ctl)
    hud = _open(
        ctl,
        sid,
        "**Deploy**\n\n- build\n- ship",
        markdown=True,
        detail="step 2 of 3",
        spinner="braille",
        position="top",
        color="#102030",
        textColor="#405060",
        sizePercent=50,
        fontSize=14,
        hideAfter=600,
        frame=False,
    )
    assert hud == {
        "message": "**Deploy**\n\n- build\n- ship",
        "detail": "step 2 of 3",
        "spinner": "braille",
        "backgroundColor": "#102030",
        "textColor": "#405060",
        "sizePercent": 50,
        "heightPercent": hud["heightPercent"],
        "position": "top-center",
        "hideAfter": 600,
        "markdown": True,
        "fontSize": 14,
        "sticky": False,
        "frame": False,
    }
    # `none` is the read-back's spelling for a static panel, so it is accepted back.
    assert _open(ctl, sid, spinner="none")["spinner"] == "none"
    _close(ctl, sid)


@pytest.mark.covers("session.hud.open", "field:tree.session.hud")
@pytest.mark.parametrize(
    ("args", "width"),
    [
        pytest.param({"sizePercent": 100}, 80, id="max"),
        pytest.param({"sizePercent": 5}, 10, id="min"),
        pytest.param({"sizePercent": 100, "sticky": True, "position": "top-left"}, 100, id="sticky-edge"),
        pytest.param({"sizePercent": 100, "sticky": True}, 80, id="sticky-center"),
    ],
)
def test_hud_width_is_clamped(ctl, args, width):
    sid = _new(ctl)
    assert _open(ctl, sid, **args)["sizePercent"] == width
    _close(ctl, sid)


@pytest.mark.covers("session.hud.update", "field:tree.session.hud")
def test_update_replaces_the_whole_spec(ctl):
    sid = _new(ctl)
    assert_error(ctl.request("session.hud.update", sid, message="nothing up"), "no hud")
    _open(ctl, sid, "one", detail="first", spinner="dot", color="#102030", textColor="#405060", fontSize=14)
    hud = _update(ctl, sid, "two", textColor="#a0b0c0", color="#ffffff", position="bottom-right")
    # The backing color and the font are read once when the panel is created, so an update keeps the open's.
    assert_subset(
        hud,
        {
            "message": "two",
            "spinner": "none",
            "position": "bottom-right",
            "backgroundColor": "#102030",
            "textColor": "#a0b0c0",
            "fontSize": 14,
        },
    )
    assert_absent(hud, "detail")
    assert_absent(_update(ctl, sid, "three"), "textColor")
    assert_error(
        ctl.request("session.hud.update", sid, message="four", fontSize=20),
        "session.hud.update: --font-size is fixed at open; reopen the hud to change it",
    )
    assert _hud(ctl, sid)["message"] == "three"
    # A second open replaces the panel, background and font included.
    hud = _open(ctl, sid, "five", color="#000000")
    assert hud["backgroundColor"] == "#000000"
    assert_absent(hud, "fontSize")
    _close(ctl, sid)


@pytest.mark.covers("session.hud.open", "session.hud.update", "field:tree.session.hud")
def test_hud_hides_itself_after_its_interval(ctl):
    sid = _new(ctl)
    assert _open(ctl, sid, "brief", hideAfter=2)["hideAfter"] == 2
    ctl.wait_tree(lambda tree: "hud" not in find_session(tree, sid), timeout=15)
    assert_error(ctl.request("session.hud.close", sid), "no hud")

    # An update that names no interval cancels the one the open armed.
    _open(ctl, sid, "long", hideAfter=600)
    assert _update(ctl, sid, "still here")["hideAfter"] == 0
    _close(ctl, sid)


@pytest.mark.covers(
    "session.hud.open", "session.hud.close", "session.overlay.open", "session.overlay.close", "field:tree.session.hud"
)
def test_hud_and_program_share_one_slot(ctl):
    sid = _new(ctl)
    assert_ok(ctl.request("session.overlay.open", sid, command=HOLD))
    assert_error(ctl.request("session.hud.open", sid, message="over a program"), "overlay already open")
    assert_error(ctl.request("session.hud.close", sid), "no hud")
    node = _node(ctl, sid)
    assert node["overlay"] is True
    assert_absent(node, "hud")
    assert_ok(ctl.request("session.overlay.close", sid))

    # A program replaces a message; a close of the slot takes a message down too.
    _open(ctl, sid, "replaced")
    assert_ok(ctl.request("session.overlay.open", sid, command=HOLD))
    node = _node(ctl, sid)
    assert node["overlay"] is True
    assert_absent(node, "hud")
    assert_ok(ctl.request("session.overlay.close", sid))
    _open(ctl, sid, "closed with the slot")
    assert_ok(ctl.request("session.overlay.close", sid))
    assert_absent(_node(ctl, sid), "hud")


def _page(ctl):
    page = ctl.instance.root / "hud-page.html"
    page.write_text("<!doctype html><title>Slot Page</title><p>static</p>")
    return str(page)


@pytest.mark.covers("session.overlay.open", "field:tree.session.hud", "field:tree.session.htmlOverlays")
def test_page_replaces_a_hud(app, ctl):
    sid = _new(ctl)
    _open(ctl, sid, "replaced by a page")
    assert_ok(ctl.request("session.overlay.open", sid, html=_page(ctl)))
    node = _node(ctl, sid)
    assert node["overlay"] is True and len(node["htmlOverlays"]) == 1
    assert_absent(node, "hud")
    # Left open on an instance of its own: the installed Linux fork keeps the replaced HUD's surface and
    # segfaults when this page closes.


@pytest.mark.covers("session.hud.open", "field:tree.session.hud")
def test_hud_refused_over_a_page(ctl):
    sid = _new(ctl)
    assert_ok(ctl.request("session.overlay.open", sid, html=_page(ctl)))
    ctl.wait_tree(lambda tree: find_session(tree, sid)["htmlOverlays"][0]["state"] == "loaded")
    assert_error(ctl.request("session.hud.open", sid, message="over a page"), "overlay already open")
    assert_absent(_node(ctl, sid), "hud")
    assert_ok(ctl.request("session.overlay.close", sid))


@pytest.mark.covers("session.overlay.resize", "field:tree.session.hud")
def test_resize_sets_the_hud_width(ctl):
    sid = _new(ctl)
    _open(ctl, sid)
    for requested, width in [(30, 30), (100, 80), (1, 10)]:
        assert assert_ok(ctl.request("session.overlay.resize", sid, sizePercent=requested))["id"] == sid
        node = _node(ctl, sid)
        assert node["hud"]["sizePercent"] == width and node["overlay"] is False
        assert_absent(node, "overlaySizePercent")
    assert_error(
        ctl.request("session.overlay.resize", sid, full=True),
        "a hud is always floating: pass --size-percent, not --full",
    )
    assert _hud(ctl, sid)["sizePercent"] == 10
    _close(ctl, sid)


@pytest.mark.covers("session.hud.open", "session.hud.update", "field:tree.session.hud")
def test_hud_over_one_pane(ctl):
    sid = _split(ctl)
    assert _open(ctl, sid, pane="right")["pane"] == "right"
    # A live pane id wins over the role beside it.
    left = _pane_id(_node(ctl, sid), "left")
    assert _update(ctl, sid, "moved", paneID=left, pane="right")["pane"] == "left"
    assert_absent(_update(ctl, sid, "whole session"), "pane")
    assert_error(ctl.request("session.hud.update", sid, message="lost", paneID=UNKNOWN), f"unknown pane id: {UNKNOWN}")
    assert _open(ctl, sid, paneID=_pane_id(_node(ctl, sid), "right"))["pane"] == "right"
    _close(ctl, sid)


@pytest.mark.covers("session.hud.open", "session.hud.update")
def test_hud_pane_must_exist(ctl):
    sid = _new(ctl)
    assert_error(ctl.request("session.hud.open", sid, message="x", pane="right"), "pane not visible")
    assert_absent(_node(ctl, sid), "hud")
    _open(ctl, sid)
    assert_error(ctl.request("session.hud.update", sid, message="x", pane="right"), "session has no split")
    assert _hud(ctl, sid)["message"] == "working"
    _close(ctl, sid)


@pytest.mark.covers("session.hud.open", "session.hud.update")
@pytest.mark.parametrize("cmd", ["session.hud.open", "session.hud.update"])
@pytest.mark.parametrize(
    ("args", "error"),
    [
        pytest.param({}, "{cmd} requires a message", id="no-message"),
        pytest.param({"message": " \t "}, "{cmd} requires a message", id="blank"),
        pytest.param({"message": "a\nb"}, "hud text must not contain control characters", id="newline"),
        pytest.param(
            {"message": "a", "detail": "b\tc"}, "hud text must not contain control characters", id="detail-tab"
        ),
        pytest.param(
            {"message": "a\x1b[31m", "markdown": True},
            "hud text must not contain control characters",
            id="markdown-escape",
        ),
        pytest.param(
            {"message": "[ref]: /nowhere", "markdown": True}, "{cmd} requires a message", id="markdown-renders-nothing"
        ),
        pytest.param({"message": "x" * 257}, "hud message too long (max 256 characters)", id="long"),
        pytest.param(
            {"message": "x" * 4097, "markdown": True}, "hud message too long (max 4096 characters)", id="long-markdown"
        ),
        pytest.param(
            {"message": "a", "detail": "x" * 257}, "hud detail too long (max 256 characters)", id="long-detail"
        ),
        pytest.param({"message": "a", "color": "red"}, "invalid color: red (#rrggbb)", id="color"),
        pytest.param({"message": "a", "textColor": "#12345"}, "invalid text color: #12345 (#rrggbb)", id="text-color"),
        pytest.param({"message": "a", "sizePercent": 0}, "{cmd}: --size-percent must be 1...100", id="size-low"),
        pytest.param({"message": "a", "sizePercent": 101}, "{cmd}: --size-percent must be 1...100", id="size-high"),
        pytest.param({"message": "a", "hideAfter": -1}, "{cmd}: --hide-after must be 0...86400 seconds", id="hide-low"),
        pytest.param(
            {"message": "a", "hideAfter": 86401}, "{cmd}: --hide-after must be 0...86400 seconds", id="hide-high"
        ),
        pytest.param({"message": "a", "position": "middle"}, f"invalid position: middle ({POSITIONS})", id="position"),
        pytest.param(
            {"message": "a", "spinner": "wheel"},
            "invalid spinner: wheel (bar|braille|circle|blocks|dot|none)",
            id="spinner",
        ),
        pytest.param({"message": "a", "pane": "scratch"}, "--pane must be left or right", id="pane"),
    ],
)
def test_hud_refusals(ctl, cmd, args, error):
    sid = _new(ctl)
    if cmd == "session.hud.update":
        _open(ctl, sid, "kept")
    assert_error(ctl.request(cmd, sid, **args), error.format(cmd=cmd))
    if cmd == "session.hud.update":
        assert _hud(ctl, sid)["message"] == "kept"
        _close(ctl, sid)
    else:
        assert_absent(_node(ctl, sid), "hud")


@pytest.mark.covers("session.hud.open")
@pytest.mark.parametrize(
    ("args", "error"),
    [
        pytest.param({"fontSize": 5}, "session.hud.open: --font-size must be 6...72 points", id="font-low"),
        pytest.param({"fontSize": 73}, "session.hud.open: --font-size must be 6...72 points", id="font-high"),
    ],
)
def test_hud_font_size_range(ctl, args, error):
    sid = _new(ctl)
    assert_error(ctl.request("session.hud.open", sid, message="a", **args), error)
    assert_absent(_node(ctl, sid), "hud")


@pytest.mark.covers("session.hud.open", "session.hud.update", "session.hud.close")
@pytest.mark.parametrize(
    ("cmd", "args"),
    [("session.hud.open", {"message": "a"}), ("session.hud.update", {"message": "a"}), ("session.hud.close", {})],
)
def test_unknown_session_target(ctl, cmd, args):
    assert_error(ctl.request(cmd, UNKNOWN, **args), f"no such session: {UNKNOWN}")
