"""The dashboard (`dashboard`, read back as the tree's `dashboard*` fields) and the quick terminal (`quick`,
`quick.type`, `quick.text`, read back as `quickVisible`). A quick terminal starts in a directory the suite does
not own, so every line typed into one only prints a marker."""

import itertools
import re

import pytest
from agterm_conformance.match import assert_absent, assert_error, assert_ok

UNKNOWN = "zzz"
DASHBOARD_FIELDS = ("dashboardMembers", "dashboardHighlighted", "dashboardFontSize", "dashboardFontMode")
POSIX_SHELL = pytest.mark.requires("posix-shell", reason="a marker round-trip needs a known shell in the pane")
_markers = itertools.count(1)


def _new(ctl):
    sid = assert_ok(ctl.request("session.new"))["id"]
    ctl.wait_mounted(sid, "left")
    return sid


def _split(ctl):
    sid = _new(ctl)
    assert_ok(ctl.request("session.split", sid, mode="on"))
    ctl.wait_mounted(sid, "right")
    return sid


def _top(ctl):
    return ctl.tree()["tree"]


def _open(ctl, **args):
    return assert_ok(ctl.request("dashboard", **args))


def _closed(ctl):
    assert_ok(ctl.request("dashboard", close=True))
    assert_absent(_top(ctl), *DASHBOARD_FIELDS)


# -- dashboard


@pytest.fixture
def no_dashboard(ctl):
    """A failed dashboard test would otherwise leave the shared instance's dashboard or zoom up for the next."""
    yield
    ctl.request("dashboard", close=True)
    ctl.request("surface.zoom", "active", mode="hide")


@pytest.mark.covers(
    "dashboard",
    "field:tree.dashboardMembers",
    "field:tree.dashboardHighlighted",
    "field:tree.dashboardFontMode",
)
@pytest.mark.usefixtures("no_dashboard")
def test_dashboard_shows_each_pane_of_the_given_sessions(ctl):
    single, split = _new(ctl), _split(ctl)
    assert "text" not in _open(ctl, targets=[single, split])
    tree = _top(ctl)
    members = [f"{single}:left", f"{split}:left", f"{split}:right"]
    assert tree["dashboardMembers"] == members
    assert tree["dashboardHighlighted"] == members[0]
    assert tree["dashboardFontMode"] == "untouched"
    assert_absent(tree, "dashboardFontSize")
    _closed(ctl)
    assert_ok(ctl.request("dashboard", close=True))


@pytest.mark.covers("dashboard", "field:tree.dashboardMembers")
@pytest.mark.usefixtures("no_dashboard")
def test_dashboard_pane_refs_pick_one_pane(ctl):
    single, split = _new(ctl), _split(ctl)
    _open(ctl, targets=[f"{split}:BOTTOM", f"{single}:primary", f"{split}:left", f"{single}:left"])
    assert _top(ctl)["dashboardMembers"] == [f"{split}:right", f"{single}:left", f"{split}:left"]
    _closed(ctl)


@pytest.mark.covers("dashboard", "field:tree.dashboardFontSize", "field:tree.dashboardFontMode")
@pytest.mark.usefixtures("no_dashboard")
def test_dashboard_font_modes(ctl):
    first, second = _new(ctl), _new(ctl)
    _open(ctl, targets=[first, second], fontSize=14)
    tree = _top(ctl)
    assert (tree["dashboardFontMode"], tree["dashboardFontSize"]) == ("fixed", 14)

    _open(ctl, targets=[first, second], autoSize=True)
    tree = _top(ctl)
    assert tree["dashboardFontMode"] == "auto"
    assert isinstance(tree["dashboardFontSize"], int | float) and tree["dashboardFontSize"] > 0

    _open(ctl, targets=[first, second])
    tree = _top(ctl)
    assert tree["dashboardFontMode"] == "untouched"
    assert_absent(tree, "dashboardFontSize")
    _closed(ctl)


@pytest.mark.covers("dashboard", "field:result.text")
@pytest.mark.usefixtures("no_dashboard")
def test_dashboard_names_what_it_left_out(ctl):
    single = _new(ctl)
    result = _open(ctl, targets=[single, UNKNOWN, f"{single}:right"])
    assert result["text"] == f"unresolved: {UNKNOWN}, {single}:right"
    assert _top(ctl)["dashboardMembers"] == [f"{single}:left"]

    ten = [_new(ctl) for _ in range(10)]
    assert _open(ctl, targets=ten)["text"] == "dropped 1 pane(s) beyond the 9-cell limit"
    assert _top(ctl)["dashboardMembers"] == [f"{sid}:left" for sid in ten[:9]]
    _closed(ctl)


@pytest.mark.covers("dashboard", "field:tree.dashboardMembers", "field:tree.dashboardHighlighted")
def test_dashboard_mru_takes_the_recent_sessions(app, ctl):
    (oldest,) = [node["id"] for node in ctl.tree()["tree"]["workspaces"][0]["sessions"]]
    split = _split(ctl)
    newest = _new(ctl)
    _open(ctl, mru=True)
    tree = _top(ctl)
    assert tree["dashboardMembers"] == [f"{newest}:left", f"{split}:left", f"{split}:right", f"{oldest}:left"]
    assert tree["dashboardHighlighted"] == f"{newest}:left"
    _closed(ctl)


@pytest.mark.covers("dashboard", "surface.zoom", "field:tree.zoomedSurface", "field:tree.dashboardMembers")
@pytest.mark.usefixtures("no_dashboard")
def test_dashboard_and_terminal_zoom_replace_each_other(ctl):
    sid = _new(ctl)
    assert_ok(ctl.request("surface.zoom", "active", mode="show"))
    assert _top(ctl)["zoomedSurface"] == f"surface:{sid}:left"

    _open(ctl, targets=[sid])
    tree = _top(ctl)
    assert tree["dashboardMembers"] == [f"{sid}:left"]
    assert_absent(tree, "zoomedSurface")

    assert_ok(ctl.request("surface.zoom", "active", mode="show"))
    tree = _top(ctl)
    assert tree["zoomedSurface"] == f"surface:{sid}:left"
    assert_absent(tree, *DASHBOARD_FIELDS)
    assert_ok(ctl.request("surface.zoom", "active", mode="hide"))
    assert_absent(_top(ctl), "zoomedSurface")


@pytest.mark.covers("dashboard")
@pytest.mark.parametrize(
    ("args", "error"),
    [
        pytest.param({}, "dashboard requires at least one session id", id="nothing"),
        pytest.param(
            {"targets": ["T"], "fontSize": 12, "autoSize": True},
            "dashboard: --font-size is mutually exclusive with --auto-size",
            id="both-font-modes",
        ),
        pytest.param({"targets": ["T"], "fontSize": 0}, "dashboard --font-size must be a positive number", id="zero"),
        pytest.param(
            {"targets": ["T"], "fontSize": -3}, "dashboard --font-size must be a positive number", id="negative"
        ),
        pytest.param(
            {"targets": ["T"], "close": True},
            "dashboard --close takes no ids, --mru, or font options",
            id="close-with-ids",
        ),
        pytest.param(
            {"close": True, "mru": True}, "dashboard --close takes no ids, --mru, or font options", id="close-with-mru"
        ),
        pytest.param(
            {"close": True, "fontSize": 12},
            "dashboard --close takes no ids, --mru, or font options",
            id="close-with-font-size",
        ),
        pytest.param(
            {"close": True, "autoSize": True},
            "dashboard --close takes no ids, --mru, or font options",
            id="close-with-auto-size",
        ),
        pytest.param(
            {"targets": ["T"], "mru": True},
            "dashboard --mru cannot be combined with explicit session ids",
            id="mru-with-ids",
        ),
        pytest.param(
            {"targets": [f"{UNKNOWN}:scratch"]},
            f"dashboard: invalid session id '{UNKNOWN}:scratch' — use <id>, <id>:left, or <id>:right",
            id="bad-pane-ref",
        ),
        pytest.param({"targets": [UNKNOWN]}, "no dashboard sessions resolved", id="none-resolved"),
        pytest.param({"targets": ["T"], "window": UNKNOWN}, f"no such window: {UNKNOWN}", id="unknown-window"),
    ],
)
@pytest.mark.usefixtures("no_dashboard")
def test_dashboard_refusals(ctl, args, error):
    sid = _new(ctl)
    if "targets" in args:
        args = {**args, "targets": [sid if target == "T" else target for target in args["targets"]]}
    assert_error(ctl.request("dashboard", **args), error)
    assert_absent(_top(ctl), *DASHBOARD_FIELDS)


# -- quick terminal


def _quick_echo(ctl):
    """Print a fresh marker in the quick terminal and wait for it and the prompt after it, so a later read sees a
    settled screen; the typed line holds the marker split in two and never matches."""
    marker = f"QMARK{next(_markers)}"
    assert_ok(ctl.request("quick.type", text=f"printf '%s%s\\n' {marker[:5]} {marker[5:]}\n"))
    pattern = re.compile(rf"(?m)^{marker}\n[^\n]*\S")

    def accept(response):
        assert response.ok, response.error
        return pattern.search(response.result["text"])

    accept.__qualname__ = f"quick text matching {pattern.pattern!r}"
    ctl.wait_response("quick.text", None, accept)
    return marker


@pytest.mark.covers("quick.type", "quick.text", "surface.zoom", "field:tree.quickVisible")
def test_quick_commands_need_a_shown_terminal(app, ctl):
    assert ctl.tree()["tree"]["quickVisible"] is False
    assert_error(ctl.request("quick.type", text="x"), "quick terminal not open")
    assert_error(ctl.request("quick.text"), "quick terminal not open")
    assert_error(ctl.request("surface.zoom", "quick", mode="show"), "surface not available: quick")


@pytest.mark.covers("quick", "field:tree.quickVisible")
def test_quick_show_hide_toggle(ctl):
    for mode, visible in (
        ("show", True),
        ("show", True),
        ("hide", False),
        ("hide", False),
        (None, True),
        (None, False),
    ):
        response = ctl.request("quick", mode=mode) if mode else ctl.request("quick")
        assert assert_ok(response) == {}
        assert _top(ctl)["quickVisible"] is visible, mode
    assert_error(ctl.request("quick", mode="maybe"), "invalid quick mode: maybe")
    assert _top(ctl)["quickVisible"] is False


@POSIX_SHELL
@pytest.mark.covers("quick.type", "quick.text", "field:result.text")
def test_quick_type_then_read_the_marker_back(ctl):
    assert_ok(ctl.request("quick", mode="show"))
    marker = _quick_echo(ctl)
    tail = assert_ok(ctl.request("quick.text", lines=2))["text"]
    assert tail.splitlines()[0] == marker, tail
    assert marker in assert_ok(ctl.request("quick.text", all=True))["text"]

    # A shown-then-hidden quick terminal stays addressable.
    assert_ok(ctl.request("quick", mode="hide"))
    hidden = _quick_echo(ctl)
    assert hidden in assert_ok(ctl.request("quick.text"))["text"]
    assert _top(ctl)["quickVisible"] is False


@pytest.mark.covers("quick", "surface.zoom", "field:tree.zoomedSurface")
def test_quick_zoom_ends_with_the_panel(ctl):
    assert_ok(ctl.request("quick", mode="show"))
    assert assert_ok(ctl.request("surface.zoom", "quick", mode="show"))["id"] == "quick"
    assert _top(ctl)["zoomedSurface"] == "quick"
    assert_ok(ctl.request("quick", mode="hide"))
    assert_absent(_top(ctl), "zoomedSurface")
    assert_error(ctl.request("surface.zoom", "quick", mode="show"), "surface not available: quick")


@pytest.mark.covers("quick.type", "quick.text")
def test_quick_input_validation(app, ctl):
    assert_error(ctl.request("quick.type"), "quick.type requires text")
    assert_ok(ctl.request("quick", mode="show"))
    ctl.wait_response("quick.text", None, lambda response: response.ok)
    refusals = [
        (("quick.type", {"text": "a\u0000b"}), "text must not contain a NUL byte"),
        (("quick.text", {"all": True, "lines": 2}), "use either --all or --lines, not both"),
        (("quick.text", {"lines": 0}), "--lines must be greater than 0"),
        (("quick.text", {"lines": -1}), "--lines must be greater than 0"),
    ]
    for (cmd, args), error in refusals:
        assert_error(ctl.request(cmd, **args), error)
