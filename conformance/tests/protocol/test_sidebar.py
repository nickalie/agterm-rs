"""Sidebar commands with their `tree` read-back: visibility (`sidebar`), view mode (`sidebar.mode`), the app-wide
flagged layout (`sidebar.flagged-layout`), workspace expansion (`sidebar.expand`/`.collapse`) and the divider
(`sidebar.width`). The module shares one instance; every test puts back the sidebar state it found."""

import pytest
from agterm_conformance.match import active_workspace_id, assert_absent, assert_error, assert_ok, window_ids, workspaces

UNKNOWN = "zzz"
WIDTH_MIN, WIDTH_MAX = 160, 560


def _top(ctl, **args):
    return assert_ok(ctl.request("tree", **args))["tree"]


def _collapsed(ctl, **args):
    return {node["id"]: node.get("collapsed", False) for node in workspaces(_top(ctl, **args))}


@pytest.fixture(autouse=True)
def sidebar_restored(ctl):
    """Put the sidebar back as the test found it, so a failed test does not leak its view into the next one."""
    tree = _top(ctl)
    yield
    ctl.request("sidebar", mode="show" if tree["sidebarVisible"] else "hide")
    ctl.request("sidebar.mode", mode=tree["sidebarMode"])
    ctl.request("sidebar.flagged-layout", mode=tree["sidebarFlaggedLayout"])
    ctl.request("sidebar.width", sidebarWidth=tree["sidebarWidth"])


@pytest.fixture
def second(ctl):
    wid = assert_ok(ctl.request("window.new", name="second"))["id"]
    yield wid
    if wid in window_ids(ctl.request("window.list")):
        ctl.request("window.delete", wid)


@pytest.mark.covers("sidebar", "field:tree.sidebarVisible", "field:window.sidebarVisible")
def test_sidebar_shows_hides_and_toggles(ctl):
    (wid,) = window_ids(ctl.request("window.list"))
    assert_ok(ctl.request("sidebar", mode="show"))
    for mode, visible in [("hide", False), ("hide", False), ("toggle", True), (None, False), ("show", True)]:
        assert assert_ok(ctl.request("sidebar", **({} if mode is None else {"mode": mode}))) == {}, mode
        assert _top(ctl)["sidebarVisible"] is visible, mode
        (node,) = assert_ok(ctl.request("window.list"))["windows"]
        assert (node["id"], node["sidebarVisible"]) == (wid, visible), mode


@pytest.mark.covers("sidebar.mode", "field:tree.sidebarMode")
def test_mode_switches_between_the_tree_and_the_flagged_view(ctl):
    assert_ok(ctl.request("sidebar.mode", mode="tree"))
    for mode, shown in [("flagged", "flagged"), ("flagged", "flagged"), ("toggle", "tree"), (None, "flagged")]:
        assert assert_ok(ctl.request("sidebar.mode", **({} if mode is None else {"mode": mode}))) == {}, mode
        assert _top(ctl)["sidebarMode"] == shown, mode
    assert_ok(ctl.request("sidebar.mode", mode="tree"))
    assert _top(ctl)["sidebarMode"] == "tree"


@pytest.mark.covers("sidebar.flagged-layout", "field:tree.sidebarFlaggedLayout", "field:result.text")
def test_flagged_layout_echoes_the_layout_it_set(ctl):
    # A toggle goes to `tree` from anything but `tree`, and back to `flat`.
    steps = [("plain", "plain"), ("toggle", "tree"), ("toggle", "flat"), ("flat", "flat"), (None, "tree")]
    steps += [("tree", "tree"), ("plain", "plain")]
    for mode, layout in steps:
        response = ctl.request("sidebar.flagged-layout", **({} if mode is None else {"mode": mode}))
        assert assert_ok(response) == {"text": layout}, mode
        assert _top(ctl)["sidebarFlaggedLayout"] == layout, mode


@pytest.mark.covers("sidebar.flagged-layout", "field:tree.sidebarFlaggedLayout")
def test_flagged_layout_is_app_wide(ctl, second):
    first = window_ids(ctl.request("window.list"))[0]
    for layout in ("plain", "tree"):
        assert_ok(ctl.request("sidebar.flagged-layout", mode=layout))
        assert [_top(ctl, window=wid)["sidebarFlaggedLayout"] for wid in (first, second)] == [layout, layout]


@pytest.mark.covers("sidebar", "sidebar.mode", "sidebar.flagged-layout")
@pytest.mark.parametrize(
    ("cmd", "field", "error"),
    [
        ("sidebar", "sidebarVisible", "invalid sidebar mode: sideways"),
        ("sidebar.mode", "sidebarMode", "invalid sidebar mode: sideways"),
        ("sidebar.flagged-layout", "sidebarFlaggedLayout", "invalid flagged layout: sideways"),
    ],
)
def test_unknown_mode_is_refused(ctl, cmd, field, error):
    before = _top(ctl)[field]
    assert_error(ctl.request(cmd, mode="sideways"), error)
    assert _top(ctl)[field] == before


def _settled_width(ctl, width, window=None):
    """Poll `tree` until the sidebar reads `width`: a view may store its own position after the reply."""

    def reads(response):
        assert assert_ok(response)["tree"]["sidebarWidth"] == width
        return True

    reads.__qualname__ = f"sidebarWidth reading {width}"
    ctl.wait_response("tree", None, reads, **({} if window is None else {"window": window}))


@pytest.mark.covers("sidebar.width", "field:result.sidebarWidth", "field:tree.sidebarWidth")
@pytest.mark.parametrize(("asked", "stored"), [(250, 250), (300.5, 300.5), (99999, WIDTH_MAX)])
def test_width_is_clamped_and_read_back(ctl, asked, stored):
    assert assert_ok(ctl.request("sidebar.width", sidebarWidth=asked)) == {"sidebarWidth": stored}
    _settled_width(ctl, stored)


@pytest.mark.covers("sidebar.width", "field:result.sidebarWidth", "field:tree.sidebarWidth")
def test_width_below_the_floor_is_clamped(ctl, adapter):
    start = 400
    assert_ok(ctl.request("sidebar.width", sidebarWidth=start))
    _settled_width(ctl, start)
    assert assert_ok(ctl.request("sidebar.width", sidebarWidth=5)) == {"sidebarWidth": WIDTH_MIN}
    if adapter.has("sidebar-min-width"):
        _settled_width(ctl, WIDTH_MIN)
        return
    # Without the capability the view may stop above the floor, at the sidebar's own minimum width, and store
    # that position after the reply.

    def narrowed(response):
        assert WIDTH_MIN <= assert_ok(response)["tree"]["sidebarWidth"] < start
        return True

    ctl.wait_response("tree", None, narrowed)


@pytest.mark.covers("sidebar.width", "field:tree.sidebarWidth", "behavior:protocol.target-resolution")
def test_width_window_option_sets_that_window_alone(ctl, second):
    first = window_ids(ctl.request("window.list"))[0]
    assert_ok(ctl.request("sidebar.width", sidebarWidth=260, window=first))
    assert assert_ok(ctl.request("sidebar.width", sidebarWidth=300, window=second)) == {"sidebarWidth": 300}
    _settled_width(ctl, 300, window=second)
    _settled_width(ctl, 260, window=first)


@pytest.mark.covers("sidebar.width")
def test_width_is_required(ctl):
    before = _top(ctl)["sidebarWidth"]
    assert_error(ctl.request("sidebar.width"), "sidebar.width requires a width in points")
    assert _top(ctl)["sidebarWidth"] == before


@pytest.mark.covers("sidebar.width", "sidebar.expand", "sidebar.collapse", "behavior:protocol.target-resolution")
@pytest.mark.parametrize(
    ("cmd", "args"),
    [("sidebar.width", {"sidebarWidth": 250}), ("sidebar.expand", {}), ("sidebar.collapse", {})],
)
def test_unknown_window_is_refused(ctl, cmd, args):
    assert_error(ctl.request(cmd, window=UNKNOWN, **args), f"no such window: {UNKNOWN}")


@pytest.mark.covers("sidebar.collapse", "sidebar.expand", "field:tree.workspace.collapsed")
def test_collapse_keeps_the_current_workspace_open_and_expand_opens_all(ctl):
    for _ in range(2):
        assert_ok(ctl.request("workspace.new"))
    current = active_workspace_id(_top(ctl))

    for _ in range(2):
        assert assert_ok(ctl.request("sidebar.collapse")) == {}
        collapsed = _collapsed(ctl)
        assert collapsed.pop(current) is False
        assert set(collapsed.values()) == {True}
    assert_absent(next(node for node in workspaces(_top(ctl)) if node["id"] == current), "collapsed")

    for _ in range(2):
        assert assert_ok(ctl.request("sidebar.expand")) == {}
        assert set(_collapsed(ctl).values()) == {False}


@pytest.mark.covers("sidebar.collapse", "sidebar.expand", "field:tree.workspace.collapsed")
def test_window_option_collapses_that_window_alone(ctl, second):
    first = window_ids(ctl.request("window.list"))[0]
    for wid in (first, second):
        for _ in range(2):
            assert_ok(ctl.request("workspace.new", window=wid))
    assert_ok(ctl.request("sidebar.expand", window=first))

    assert_ok(ctl.request("sidebar.collapse", window=second))
    assert sorted(_collapsed(ctl, window=second).values()) == [False, True, True]
    assert set(_collapsed(ctl, window=first).values()) == {False}

    assert_ok(ctl.request("sidebar.expand", window=second))
    assert set(_collapsed(ctl, window=second).values()) == {False}


@pytest.mark.covers("sidebar.collapse", "sidebar.mode", "sidebar.flagged-layout")
def test_collapse_is_a_no_op_under_the_flat_flagged_list(ctl):
    assert_ok(ctl.request("workspace.new"))
    assert_ok(ctl.request("sidebar.expand"))
    assert_ok(ctl.request("sidebar.mode", mode="flagged"))
    assert_ok(ctl.request("sidebar.flagged-layout", mode="flat"))
    assert assert_ok(ctl.request("sidebar.collapse")) == {}
    assert set(_collapsed(ctl).values()) == {False}

    # The flagged tree layout has workspace rows again, so it collapses like the ordinary tree.
    assert_ok(ctl.request("sidebar.flagged-layout", mode="tree"))
    assert_ok(ctl.request("sidebar.collapse"))
    assert True in _collapsed(ctl).values()
    assert_ok(ctl.request("sidebar.expand"))
