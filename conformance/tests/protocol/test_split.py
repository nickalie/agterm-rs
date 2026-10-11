"""Split panes, the scratch terminal, terminal zoom and session backgrounds. Every test splits a session of
its own, so the module's shared instance never carries one test's panes into another."""

import pytest
from agterm_conformance.match import assert_absent, assert_error, assert_ok, assert_subset, find_session

UNKNOWN = "zzz"
# AppStore's split ratio bounds; a request outside them is clamped, never refused.
MIN_RATIO, MAX_RATIO = 0.05, 0.95


def _new(ctl):
    return assert_ok(ctl.request("session.new"))["id"]


def _node(ctl, sid):
    return find_session(ctl.tree(), sid)


def _surfaces(ctl, sid):
    return {surface["kind"]: surface for surface in _node(ctl, sid)["surfaces"]}


def _split(ctl, **args):
    sid = _new(ctl)
    assert_ok(ctl.request("session.split", sid, mode="on", **args))
    ctl.wait_tree(lambda tree: find_session(tree, sid).get("hasSplit"))
    return sid


@pytest.mark.covers(
    "session.split",
    "field:tree.session.split",
    "field:tree.session.hasSplit",
    "field:tree.session.splitAxis",
    "field:tree.session.splitFocused",
    "field:tree.session.splitCwd",
    "field:tree.session.surfaces",
    "event:pane.split",
)
def test_split_shows_hides_and_keeps_the_pane(ctl):
    sid = _new(ctl)
    cursor = ctl.events_baseline()

    assert assert_ok(ctl.request("session.split", sid, mode="on"))["id"] == sid
    tree = ctl.wait_tree(lambda tree: find_session(tree, sid).get("hasSplit"))
    node = find_session(tree, sid)
    assert_subset(node, {"split": True, "hasSplit": True, "splitAxis": "vertical", "splitFocused": True})
    assert isinstance(node["splitCwd"], str) and node["splitCwd"]
    assert {surface["kind"] for surface in node["surfaces"]} == {"left", "right"}
    ctl.wait_event(cursor, "pane.split", {"session": sid, "payload": {"status": "shown"}})

    # Off hides the pane and keeps its shell; `hasSplit` still reports it.
    cursor = ctl.events_baseline()
    assert_ok(ctl.request("session.split", sid, mode="off"))
    assert_subset(_node(ctl, sid), {"split": False, "hasSplit": True})
    ctl.wait_event(cursor, "pane.split", {"session": sid, "payload": {"status": "hidden"}})
    for mode, shown in [("toggle", True), (None, False), (None, True)]:
        assert_ok(ctl.request("session.split", sid, **({} if mode is None else {"mode": mode})))
        assert_subset(_node(ctl, sid), {"split": shown, "hasSplit": True})


@pytest.mark.covers("session.split", "field:tree.session.splitAxis")
def test_split_axis(ctl):
    sid = _split(ctl, axis="horizontal")
    assert _node(ctl, sid)["splitAxis"] == "horizontal"
    # Omitting the axis keeps the existing split's.
    assert_ok(ctl.request("session.split", sid, mode="on"))
    assert _node(ctl, sid)["splitAxis"] == "horizontal"
    assert_ok(ctl.request("session.split", sid, mode="on", axis="vertical"))
    assert _node(ctl, sid)["splitAxis"] == "vertical"


@pytest.mark.covers("session.split")
@pytest.mark.parametrize(
    ("args", "error"),
    [
        pytest.param({"mode": "show"}, "invalid split mode: show", id="mode"),
        pytest.param({"axis": "diagonal"}, "invalid split axis: diagonal (vertical|horizontal)", id="axis"),
    ],
)
def test_split_rejects(ctl, args, error):
    sid = _new(ctl)
    assert_error(ctl.request("session.split", sid, **args), error)
    assert_absent(_node(ctl, sid), "hasSplit")


@pytest.mark.covers("session.split.close", "event:pane.split")
def test_split_close_tears_the_pane_down(ctl):
    sid = _split(ctl)
    cursor = ctl.events_baseline()
    for _ in range(2):
        assert assert_ok(ctl.request("session.split.close", sid))["id"] == sid
        node = _node(ctl, sid)
        assert node["split"] is False
        assert_absent(node, "hasSplit", "splitAxis", "splitRatio", "splitFocused", "splitCwd")
        assert [surface["kind"] for surface in node["surfaces"]] == ["left"]
    ctl.wait_event(cursor, "pane.split", {"session": sid, "payload": {"status": "hidden"}})


@pytest.mark.covers("session.focus", "field:tree.session.splitFocused")
def test_focus_names_a_pane_by_role_or_position(ctl):
    sid = _split(ctl)
    for pane, split_focused in [
        ("left", False),
        ("right", True),
        ("primary", False),
        ("split", True),
        ("top", False),
        ("bottom", True),
        ("other", False),
        ("other", True),
    ]:
        assert assert_ok(ctl.request("session.focus", sid, pane=pane))["id"] == sid, pane
        node = _node(ctl, sid)
        assert node["splitFocused"] is split_focused, pane
        surfaces = {surface["kind"]: surface for surface in node["surfaces"]}
        assert surfaces["right"]["active"] is split_focused, pane
        assert surfaces["left"]["active"] is not split_focused, pane


@pytest.mark.covers("session.focus")
def test_focus_reaches_a_hidden_split(ctl):
    sid = _split(ctl)
    assert_ok(ctl.request("session.focus", sid, pane="left"))
    assert_ok(ctl.request("session.split", sid, mode="off"))
    assert_ok(ctl.request("session.focus", sid, pane="right"))
    assert_subset(_node(ctl, sid), {"split": False, "splitFocused": True})


@pytest.mark.covers("session.resize", "field:tree.session.splitRatio", "field:result.ratio")
def test_resize_sets_and_clamps_the_ratio(adapter, ctl):
    sid = _split(ctl)
    for ratio, stored in [(0.3, 0.3), (0.0, MIN_RATIO), (1.5, MAX_RATIO), (0.5, 0.5)]:
        result = assert_ok(ctl.request("session.resize", sid, ratio=ratio))
        assert result["id"] == sid, ratio
        if adapter.has("resize-echoes-ratio"):
            assert_subset(result, {"ratio": stored})
        else:
            # The Linux fork answers with the id alone; `splitRatio` is its only read-back.
            assert_absent(result, "ratio")
        assert_subset(_node(ctl, sid), {"splitRatio": stored})


@pytest.mark.covers("session.resize", "field:tree.session.splitRatio")
def test_resize_by_a_delta(ctl):
    sid = _split(ctl)
    assert_ok(ctl.request("session.resize", sid, ratio=0.5))
    for delta, stored in [(0.1, 0.6), (-0.25, 0.35), (-1.0, MIN_RATIO), (2.0, MAX_RATIO)]:
        assert_ok(ctl.request("session.resize", sid, ratioDelta=delta))
        assert_subset(_node(ctl, sid), {"splitRatio": stored})


@pytest.mark.covers("session.resize", "field:tree.session.splitRatio")
def test_resize_a_hidden_split_applies_when_shown(ctl):
    sid = _split(ctl)
    assert_ok(ctl.request("session.split", sid, mode="off"))
    assert_ok(ctl.request("session.resize", sid, ratio=0.4))
    assert_subset(_node(ctl, sid), {"split": False, "splitRatio": 0.4})
    assert_ok(ctl.request("session.split", sid, mode="on"))
    assert_subset(_node(ctl, sid), {"split": True, "splitRatio": 0.4})


@pytest.mark.covers("session.resize")
@pytest.mark.parametrize(
    ("args", "error"),
    [
        pytest.param({}, "session.resize requires --split-ratio, --grow-left, or --grow-right", id="nothing"),
        pytest.param(
            {"ratio": 0.5, "ratioDelta": 0.1},
            "session.resize: --split-ratio is mutually exclusive with --grow-left/--grow-right",
            id="both",
        ),
    ],
)
def test_resize_rejects(ctl, args, error):
    sid = _split(ctl)
    assert_ok(ctl.request("session.resize", sid, ratio=0.3))
    assert_error(ctl.request("session.resize", sid, **args), error)
    assert_subset(_node(ctl, sid), {"splitRatio": 0.3})


@pytest.mark.covers("session.swap", "field:tree.session.splitFocused")
def test_swap_exchanges_the_panes_and_focus_follows_the_terminal(ctl):
    sid = _split(ctl, axis="horizontal")
    assert_ok(ctl.request("session.resize", sid, ratio=0.3))
    assert_ok(ctl.request("session.focus", sid, pane="left"))
    before = _surfaces(ctl, sid)

    assert assert_ok(ctl.request("session.swap", sid))["id"] == sid
    node = _node(ctl, sid)
    after = {surface["kind"]: surface for surface in node["surfaces"]}
    assert after["left"]["paneID"] == before["right"]["paneID"]
    assert after["right"]["paneID"] == before["left"]["paneID"]
    # The terminal that had focus is now the split pane; axis and ratio stay with the layout.
    assert_subset(node, {"splitFocused": True, "splitAxis": "horizontal", "splitRatio": 0.3})
    assert_ok(ctl.request("session.swap", sid))
    assert {kind: surface["paneID"] for kind, surface in _surfaces(ctl, sid).items()} == {
        kind: surface["paneID"] for kind, surface in before.items()
    }


@pytest.mark.covers("session.swap")
def test_swap_reaches_a_hidden_split(ctl):
    sid = _split(ctl)
    before = _surfaces(ctl, sid)
    assert_ok(ctl.request("session.split", sid, mode="off"))
    assert_ok(ctl.request("session.swap", sid))
    assert _surfaces(ctl, sid)["left"]["paneID"] == before["right"]["paneID"]


@pytest.mark.covers("session.focus", "session.resize", "session.swap")
@pytest.mark.parametrize(
    ("cmd", "args", "error"),
    [
        ("session.focus", {"pane": "right"}, "session has no split"),
        ("session.resize", {"ratio": 0.3}, "session has no split"),
        ("session.swap", {}, "session has no split pane"),
    ],
)
def test_pane_commands_need_a_split(ctl, cmd, args, error):
    sid = _new(ctl)
    assert_error(ctl.request(cmd, sid, **args), error)
    assert_absent(_node(ctl, sid), "hasSplit")


@pytest.mark.covers("session.focus")
def test_focus_rejects_an_unknown_pane(ctl):
    sid = _split(ctl)
    assert_error(ctl.request("session.focus", sid, pane="middle"), "invalid pane: middle")


@pytest.mark.covers("session.scratch", "field:tree.session.scratch", "event:pane.scratch")
def test_scratch_shows_and_hides(ctl):
    sid = _new(ctl)
    cursor = ctl.events_baseline()
    assert assert_ok(ctl.request("session.scratch", sid, mode="on"))["id"] == sid
    assert _node(ctl, sid)["scratch"] is True
    ctl.wait_event(cursor, "pane.scratch", {"session": sid, "payload": {"status": "shown"}})
    for mode, shown in [("on", True), ("toggle", False), (None, True), ("off", False)]:
        assert_ok(ctl.request("session.scratch", sid, **({} if mode is None else {"mode": mode})))
        assert _node(ctl, sid)["scratch"] is shown, mode
    ctl.wait_event(cursor, "pane.scratch", {"session": sid, "payload": {"status": "hidden"}})


@pytest.mark.covers("session.scratch")
def test_scratch_rejects_an_unknown_mode(ctl):
    sid = _new(ctl)
    assert_error(ctl.request("session.scratch", sid, mode="show"), "invalid scratch mode: show")
    assert _node(ctl, sid)["scratch"] is False


@pytest.mark.covers("surface.zoom", "field:tree.zoomedSurface", "field:result.id")
def test_zoom_one_surface_and_back(ctl):
    sid = _split(ctl)
    right = f"surface:{sid}:right"
    assert assert_ok(ctl.request("surface.zoom", right, mode="show"))["id"] == right
    assert ctl.tree()["tree"]["zoomedSurface"] == right
    # Zoom leaves the panes alone.
    assert_subset(_node(ctl, sid), {"split": True, "hasSplit": True})
    assert_ok(ctl.request("surface.zoom", mode="hide"))
    assert_absent(ctl.tree()["tree"], "zoomedSurface")
    left = f"surface:{sid}:left"
    for mode, zoomed in [("toggle", True), ("toggle", False), (None, True), ("hide", False), ("hide", False)]:
        assert_ok(ctl.request("surface.zoom", left, **({} if mode is None else {"mode": mode})))
        tree = ctl.tree()["tree"]
        if zoomed:
            assert tree["zoomedSurface"] == left, mode
        else:
            assert_absent(tree, "zoomedSurface")


@pytest.mark.covers("surface.zoom")
@pytest.mark.parametrize("target", [UNKNOWN, f"surface:{UNKNOWN}:left"])
def test_zoom_rejects_an_unknown_surface(ctl, target):
    # The references disagree on the text: upstream `invalid surface: zzz` and `no such surface: surface:zzz:left`,
    # the Linux fork `invalid surface target: <target>` for both. Both name the target.
    response = ctl.request("surface.zoom", target, mode="show")
    assert_error(response)
    assert response.error.endswith(f": {target}"), response.raw
    assert_absent(ctl.tree()["tree"], "zoomedSurface")


@pytest.mark.covers("surface.zoom")
def test_zoom_rejects_an_unknown_mode(ctl):
    # The references disagree on the text: upstream `invalid surface zoom mode: big`, the Linux fork's own
    # dispatcher `invalid surface.zoom mode: big`. Both name the rejected mode.
    response = ctl.request("surface.zoom", mode="big")
    assert_error(response)
    assert response.error.endswith(" mode: big"), response.raw
    assert_absent(ctl.tree()["tree"], "zoomedSurface")


@pytest.mark.covers("session.background", "field:tree.session.background", "field:tree.session.paneBackgrounds")
def test_background_default_and_pane_override(ctl):
    sid = _new(ctl)
    assert assert_ok(ctl.request("session.background", sid, mode="color", color="#112233"))["id"] == sid
    assert _node(ctl, sid)["background"] == {"kind": "color", "colorHex": "#112233"}
    assert_ok(ctl.request("session.background", sid, mode="text", text="NOTES", opacity=0.5))
    assert_subset(_node(ctl, sid)["background"], {"kind": "text", "text": "NOTES", "opacity": 0.5})

    assert_ok(ctl.request("session.background", sid, mode="color", color="#445566", pane="left"))
    node = _node(ctl, sid)
    assert node["paneBackgrounds"] == {"left": {"kind": "color", "colorHex": "#445566"}}
    assert node["background"]["kind"] == "text"
    assert_ok(ctl.request("session.background", sid, mode="clear", pane="left"))
    assert_absent(_node(ctl, sid), "paneBackgrounds")
    assert_ok(ctl.request("session.background", sid, mode="clear"))
    assert_absent(_node(ctl, sid), "background", "paneBackgrounds")


@pytest.mark.covers("session.background")
@pytest.mark.parametrize(
    ("args", "error"),
    [
        pytest.param({"mode": "paint"}, "invalid background mode: paint (image|text|color|clear)", id="mode"),
        pytest.param({"mode": "color"}, "session.background color requires a color", id="no-color"),
        pytest.param({"mode": "color", "color": "blue"}, "invalid color: blue (#rrggbb)", id="color"),
        pytest.param({"mode": "text", "text": "x", "opacity": 2.0}, "invalid opacity: 2.0 (0.0-1.0)", id="opacity"),
        pytest.param(
            {"mode": "text", "text": "x", "fit": "squash"}, "invalid fit: squash (contain|cover|stretch|none)", id="fit"
        ),
        pytest.param({"mode": "text", "text": "x", "position": "middle"}, "invalid position: middle", id="position"),
        pytest.param({"mode": "color", "color": "#445566", "pane": "right"}, "session has no split pane", id="right"),
    ],
)
def test_background_rejects(ctl, args, error):
    sid = _new(ctl)
    assert_error(ctl.request("session.background", sid, **args), error)
    assert_absent(_node(ctl, sid), "background", "paneBackgrounds")


@pytest.mark.covers(
    "session.split",
    "session.split.close",
    "session.swap",
    "session.scratch",
    "session.focus",
    "session.resize",
    "session.background",
)
@pytest.mark.parametrize(
    ("cmd", "args"),
    [
        ("session.split", {"mode": "on"}),
        ("session.split.close", {}),
        ("session.swap", {}),
        ("session.scratch", {"mode": "on"}),
        ("session.focus", {"pane": "left"}),
        ("session.resize", {"ratio": 0.5}),
        ("session.background", {"mode": "clear"}),
    ],
)
def test_unknown_session_target(ctl, cmd, args):
    assert_error(ctl.request(cmd, UNKNOWN, **args), f"no such session: {UNKNOWN}")
