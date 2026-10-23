"""Window commands with their `window.list` and window-scoped `tree` read-back. The module shares one instance:
every test opens the windows it needs and deletes them again, so each one starts with the first window alone."""

import re

import pytest
from agterm_conformance.match import assert_absent, assert_error, assert_ok, assert_subset, window_ids, workspaces
from agterm_conformance.wait import WaitTimeout

UNKNOWN = "zzz"
NOT_OPEN = "window not open — window.select it first"
# Present only while the window is open.
LIVE_FIELDS = ("sidebarVisible", "fullscreen", "zoomed", "minimized", "geometry", "autoFollowMs")


def _windows(ctl):
    return assert_ok(ctl.request("window.list"))["windows"]


def _window(ctl, wid):
    (node,) = [node for node in _windows(ctl) if node["id"] == wid]
    return node


def _first(ctl):
    return _windows(ctl)[0]["id"]


def _settled(ctl, wid, timeout=10.0, **expected):
    """Poll `window.list` until window `wid` reads `expected`: frame and state changes land after the reply."""

    def accept(response):
        (node,) = [node for node in assert_ok(response)["windows"] if node["id"] == wid]
        assert_subset(node, expected)
        return True

    accept.__qualname__ = f"window {wid} reading {expected}"
    response = ctl.wait_response("window.list", None, accept, timeout)
    (node,) = [node for node in response.result["windows"] if node["id"] == wid]
    return node


def _toggle_until(ctl, wid, toggle, **expected):
    """`toggle()` until `wid` reads `expected`, sending one that changed nothing in 5s again; an accepted one settles
    within about a second. macOS drops a fullscreen toggle now and then, with nothing in `window.list` to tell, and
    weston drops an unmaximize that arrives before the client has committed the maximize it just configured."""
    for attempt in range(3):
        toggle()
        try:
            return _settled(ctl, wid, timeout=5.0, **expected)
        except WaitTimeout:
            if attempt == 2:
                raise
    return None


def _delete(ctl, wid):
    if wid in window_ids(ctl.request("window.list")):
        assert assert_ok(ctl.request("window.delete", wid))["id"] == wid


@pytest.fixture
def second(ctl):
    wid = assert_ok(ctl.request("window.new", name="second"))["id"]
    yield wid
    _delete(ctl, wid)


def _workspace_names(ctl, window):
    return [node["name"] for node in workspaces(assert_ok(ctl.request("tree", window=window)))]


@pytest.mark.covers(
    "window.new",
    "window.list",
    "field:result.id",
    "field:result.windows",
    "field:window.id",
    "field:window.name",
    "field:window.open",
)
def test_new_opens_a_window_with_its_own_workspace(ctl, second):
    first = _first(ctl)
    assert window_ids(ctl.request("window.list")) == [first, second]
    assert_subset(_window(ctl, second), {"name": "second", "open": True})
    (own,) = workspaces(assert_ok(ctl.request("tree", window=second)))
    assert len(own["sessions"]) == 1
    assert own["id"] not in {node["id"] for node in workspaces(assert_ok(ctl.request("tree", window=first)))}

    unnamed = assert_ok(ctl.request("window.new"))["id"]
    try:
        assert re.fullmatch(r"window \d+", _window(ctl, unnamed)["name"])
    finally:
        _delete(ctl, unnamed)


@pytest.mark.covers("workspace.new", "session.new", "tree", "behavior:protocol.target-resolution")
def test_window_option_addresses_that_windows_store(ctl, second):
    first = _first(ctl)
    before = _workspace_names(ctl, first)
    assert_ok(ctl.request("workspace.new", name="in-second", window=second))
    assert _workspace_names(ctl, second)[-1] == "in-second"
    assert _workspace_names(ctl, first) == before

    def session_count(window):
        return sum(len(node["sessions"]) for node in workspaces(assert_ok(ctl.request("tree", window=window))))

    counts = session_count(first), session_count(second)
    assert_ok(ctl.request("session.new", window=first))
    assert (session_count(first), session_count(second)) == (counts[0] + 1, counts[1])


@pytest.mark.covers("window.select", "window.go", "field:window.active")
def test_select_and_go_move_the_active_window(ctl, second):
    first = _first(ctl)
    assert assert_ok(ctl.request("window.select", first))["id"] == first
    _settled(ctl, first, active=True)
    for direction, landed, other in (("next", second, first), ("prev", first, second)):
        assert assert_ok(ctl.request("window.go", to=direction))["id"] == landed
        _settled(ctl, landed, active=True)
        _settled(ctl, other, active=False)


@pytest.mark.covers("window.go")
def test_go_needs_another_open_window(ctl, second):
    assert_ok(ctl.request("window.close", second))
    _settled(ctl, second, open=False)
    assert_error(ctl.request("window.go", to="next"), "no other open window to navigate to")


@pytest.mark.covers("window.rename", "field:window.name")
def test_rename_trims_the_name(ctl, second):
    assert assert_ok(ctl.request("window.rename", second, name="  renamed  "))["id"] == second
    assert _window(ctl, second)["name"] == "renamed"


@pytest.mark.covers("window.resize", "field:result.width", "field:result.height", "field:window.geometry")
def test_resize_echoes_the_applied_size(ctl, adapter, second):
    result = assert_ok(ctl.request("window.resize", second, width=900, height=600))
    assert result == {"id": second, "width": 900, "height": 600}
    if adapter.has("window-geometry"):
        _settled(ctl, second, geometry={"width": 900, "height": 600})
    else:
        # Wayland gives a client no window position, so the build omits `geometry` rather than half of it.
        assert_absent(_window(ctl, second), "geometry")


@pytest.mark.covers("window.move", "field:window.geometry")
def test_move_places_the_window(ctl, adapter, second):
    response = ctl.request("window.move", second, x=40, y=60)
    if adapter.has("window-geometry"):
        assert assert_ok(response)["id"] == second
        _settled(ctl, second, geometry={"x": 40, "y": 60})
    else:
        assert_error(
            response, "window.move is not supported on this platform (the compositor controls window position)"
        )
        assert_absent(_window(ctl, second), "geometry")


@pytest.mark.covers("window.zoom", "field:window.zoomed")
def test_zoom_toggles(ctl, second):
    def toggle():
        assert assert_ok(ctl.request("window.zoom", second))["id"] == second

    for zoomed in (True, False):
        _toggle_until(ctl, second, toggle, zoomed=zoomed)


@pytest.mark.covers("window.fullscreen", "field:window.fullscreen")
def test_fullscreen_toggles(ctl, second):
    def toggle():
        assert assert_ok(ctl.request("window.fullscreen", second))["id"] == second

    for fullscreen in (True, False):
        _toggle_until(ctl, second, toggle, fullscreen=fullscreen)


@pytest.mark.covers("window.minimize", "field:window.minimized")
def test_minimize_on_and_off(ctl, adapter, second):
    for mode, minimized in (("on", True), ("off", False)):
        assert assert_ok(ctl.request("window.minimize", second, mode=mode))["id"] == second
        if adapter.has("window-minimize"):
            _settled(ctl, second, minimized=minimized)
        else:
            # Wayland's xdg-shell has no minimized state to report.
            assert _window(ctl, second)["open"] is True
    assert_error(ctl.request("window.minimize", second, mode="pin"), "invalid window minimize mode: pin")


@pytest.mark.covers("window.new", "window.minimize", "field:window.minimized")
def test_new_can_start_minimized(ctl, adapter):
    wid = assert_ok(ctl.request("window.new", name="parked", minimized=True))["id"]
    try:
        if adapter.has("window-minimize"):
            _settled(ctl, wid, open=True, minimized=True)
        else:
            assert _window(ctl, wid)["open"] is True
    finally:
        _delete(ctl, wid)


@pytest.mark.covers("window.close", "window.select", "field:window.open")
def test_close_keeps_the_window_and_select_reopens_it(ctl, adapter, second):
    assert_ok(ctl.request("workspace.new", name="kept", window=second))
    assert assert_ok(ctl.request("window.close", second))["id"] == second
    closed = _settled(ctl, second, open=False)
    assert_absent(closed, *LIVE_FIELDS)
    assert assert_ok(ctl.request("window.close", second))["id"] == second

    for cmd, args in [
        ("window.resize", {"width": 900, "height": 600}),
        ("window.zoom", {}),
        ("window.fullscreen", {}),
        ("window.minimize", {"mode": "on"}),
    ] + ([("window.move", {"x": 40, "y": 60})] if adapter.has("window-geometry") else []):
        assert_error(ctl.request(cmd, second, **args), NOT_OPEN)
    for cmd in ("tree", "workspace.new"):
        assert_error(ctl.request(cmd, window=second), NOT_OPEN)

    assert assert_ok(ctl.request("window.select", second))["id"] == second
    _settled(ctl, second, open=True)
    assert "kept" in _workspace_names(ctl, second)


@pytest.mark.covers("window.delete")
def test_delete_removes_the_window_but_never_the_last(ctl, second):
    first = _first(ctl)
    assert assert_ok(ctl.request("window.delete", second))["id"] == second
    assert window_ids(ctl.request("window.list")) == [first]
    assert_error(ctl.request("window.delete", first), "cannot delete last window")
    assert window_ids(ctl.request("window.list")) == [first]


@pytest.mark.covers(
    "window.select",
    "window.go",
    "window.close",
    "window.rename",
    "window.delete",
    "window.resize",
    "window.move",
    "window.zoom",
    "window.fullscreen",
    "window.minimize",
)
@pytest.mark.parametrize(
    ("cmd", "target", "args", "error"),
    [
        pytest.param("window.select", UNKNOWN, {}, f"no such window: {UNKNOWN}", id="select-unknown"),
        pytest.param("window.close", UNKNOWN, {}, f"no such window: {UNKNOWN}", id="close-unknown"),
        pytest.param("window.rename", UNKNOWN, {"name": "x"}, f"no such window: {UNKNOWN}", id="rename-unknown"),
        pytest.param("window.delete", UNKNOWN, {}, f"no such window: {UNKNOWN}", id="delete-unknown"),
        pytest.param(
            "window.resize", UNKNOWN, {"width": 900, "height": 600}, f"no such window: {UNKNOWN}", id="resize-unknown"
        ),
        pytest.param("window.zoom", UNKNOWN, {}, f"no such window: {UNKNOWN}", id="zoom-unknown"),
        pytest.param("window.fullscreen", UNKNOWN, {}, f"no such window: {UNKNOWN}", id="fullscreen-unknown"),
        pytest.param("window.minimize", UNKNOWN, {"mode": "on"}, f"no such window: {UNKNOWN}", id="minimize-unknown"),
        pytest.param("window.go", None, {}, "window.go requires --to next|prev", id="go-no-direction"),
        pytest.param("window.go", None, {"to": "up"}, "window.go requires --to next|prev", id="go-bad-direction"),
        pytest.param("window.rename", None, {}, "window.rename requires a name", id="rename-no-name"),
        pytest.param("window.rename", None, {"name": "  "}, "window.rename requires a name", id="rename-blank"),
        pytest.param(
            "window.resize", None, {"width": 900}, "window.resize requires positive width and height", id="resize-half"
        ),
        pytest.param(
            "window.resize",
            None,
            {"width": 0, "height": 600},
            "window.resize requires positive width and height",
            id="resize-zero",
        ),
        pytest.param(
            "window.resize",
            None,
            {"width": 900, "height": -1},
            "window.resize requires positive width and height",
            id="resize-negative",
        ),
        pytest.param("window.move", None, {"x": 40}, "window.move requires x and y", id="move-half"),
    ],
)
def test_refusals(ctl, cmd, target, args, error):
    def listing():
        return [(node["id"], node["name"], node["open"]) for node in _windows(ctl)]

    before = listing()
    assert_error(ctl.request(cmd, target, **args), error)
    assert listing() == before
