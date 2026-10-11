"""Workspace commands: effect, `tree` read-back and events. The module shares one instance, so every test
builds the workspaces it needs and reads the rest of the tree relative to what is already there."""

import pytest
from agterm_conformance.match import (
    active_session_id,
    active_workspace_id,
    assert_absent,
    assert_error,
    assert_ok,
    assert_subset,
    session_ids,
    window_ids,
    workspace_ids,
    workspaces,
)

UNKNOWN = "zzz"


def _new_workspace(ctl, sessions=0, **args):
    wid = assert_ok(ctl.request("workspace.new", **args))["id"]
    sids = [assert_ok(ctl.request("session.new", workspace=wid))["id"] for _ in range(sessions)]
    return wid, sids


def _node(source, wid):
    (node,) = [node for node in workspaces(source) if node["id"] == wid]
    return node


def _window(ctl):
    (window,) = window_ids(ctl.request("window.list"))
    return window


def _settled_baseline(ctl, cursor):
    """`tree.changed` is debounced per window: wait out the one the preparation since `cursor` scheduled, so
    a tree.changed after the returned baseline can only come from the action under test."""
    ctl.wait_event(cursor, "tree.changed")
    return ctl.events_baseline()


def _clear_focus(ctl):
    for node in workspaces(ctl.tree()):
        if node.get("focused"):
            assert_ok(ctl.request("workspace.focus", node["id"], mode="off"))
    assert ctl.tree()["tree"]["workspaceFilter"] is False


def _focus_state(ctl):
    tree = ctl.tree()
    return {node["id"] for node in workspaces(tree) if node.get("focused")}, tree["tree"]["workspaceFilter"]


def _reordered(ids, wid, to):
    ids = list(ids)
    index = ids.index(wid)
    ids.pop(index)
    destination = {"up": max(index - 1, 0), "down": min(index + 1, len(ids)), "top": 0, "bottom": len(ids)}[to]
    ids.insert(destination, wid)
    return ids


@pytest.mark.covers(
    "workspace.new",
    "field:result.id",
    "field:tree.workspace.name",
    "field:tree.workspace.active",
    "field:tree.workspace.sessions",
    "event:tree.changed",
)
def test_new_appends_an_empty_current_workspace(ctl):
    before = ctl.tree()
    selected = active_session_id(before)
    cursor = ctl.events_baseline()

    wid = assert_ok(ctl.request("workspace.new", name="alpha"))["id"]
    tree = ctl.tree()
    assert workspace_ids(tree) == [*workspace_ids(before), wid]
    node = _node(tree, wid)
    assert_subset(node, {"name": "alpha", "active": True, "sessions": []})
    assert_absent(node, "collapsed", "focused")
    assert active_workspace_id(tree) == wid
    assert active_session_id(tree) == selected
    ctl.wait_event(cursor, "tree.changed", {"window": _window(ctl)})


@pytest.mark.covers("workspace.new", "field:tree.workspace.name")
@pytest.mark.parametrize(
    "name",
    [
        pytest.param(None, id="omitted"),
        pytest.param("   ", id="blank"),
        pytest.param("  beta  ", id="padded"),
    ],
)
def test_new_workspace_name(ctl, name):
    default = f"workspace {len(workspaces(ctl.tree())) + 1}"
    wid, _ = _new_workspace(ctl, **({} if name is None else {"name": name}))
    assert _node(ctl.tree(), wid)["name"] == ((name or "").strip() or default)


@pytest.mark.covers("workspace.new", "field:tree.workspace.collapsed")
def test_new_collapsed_workspace(ctl):
    # Whether a collapsed create also becomes the current workspace differs by reference (macOS keeps the
    # current one, the Linux fork moves to it), so only `collapsed` is compared.
    collapsed = assert_ok(ctl.request("workspace.new", collapsed=True))["id"]
    plain = assert_ok(ctl.request("workspace.new", collapsed=False))["id"]
    tree = ctl.tree()
    assert_subset(_node(tree, collapsed), {"collapsed": True, "sessions": []})
    assert_absent(_node(tree, plain), "collapsed")


@pytest.mark.covers("workspace.select", "field:tree.workspace.active", "event:session.selected")
def test_select_moves_to_the_first_session(ctl):
    wid, (first, _) = _new_workspace(ctl, sessions=2)
    _, (elsewhere,) = _new_workspace(ctl, sessions=1)
    cursor = ctl.events_baseline()

    assert assert_ok(ctl.request("workspace.select", wid))["id"] == wid
    tree = ctl.tree()
    assert active_workspace_id(tree) == wid
    assert active_session_id(tree) == first
    ctl.wait_event(
        cursor,
        "session.selected",
        {"window": _window(ctl), "workspace": wid, "session": first, "payload": {"previous": elsewhere}},
    )


@pytest.mark.covers("workspace.select", "field:tree.workspace.active")
def test_select_empty_workspace_keeps_the_selection(ctl):
    empty, _ = _new_workspace(ctl)
    _, (selected,) = _new_workspace(ctl, sessions=1)

    assert assert_ok(ctl.request("workspace.select", empty))["id"] == empty
    tree = ctl.tree()
    assert active_workspace_id(tree) == empty
    assert active_session_id(tree) == selected


@pytest.mark.covers("workspace.go", "field:tree.workspace.active")
def test_go_steps_through_workspaces_and_wraps(ctl):
    _clear_focus(ctl)
    _new_workspace(ctl, sessions=1)
    _new_workspace(ctl, sessions=1)
    ids = workspace_ids(ctl.tree())
    assert_ok(ctl.request("workspace.select", ids[-1]))

    current = ids[-1]
    for direction, step in [("next", 1), ("next", 1), ("prev", -1), ("previous", -1), ("prev", -1)]:
        expected = ids[(ids.index(current) + step) % len(ids)]
        selected = active_session_id(ctl.tree())
        assert assert_ok(ctl.request("workspace.go", to=direction))["id"] == expected, direction
        tree = ctl.tree()
        assert active_workspace_id(tree) == expected
        node_sessions = session_ids(tree, expected)
        assert active_session_id(tree) == (node_sessions[0] if node_sessions else selected)
        current = expected


@pytest.mark.covers("workspace.go", "workspace.focus", "workspace.filter")
def test_go_stays_inside_the_focus_filter(ctl):
    _clear_focus(ctl)
    first, (first_session,) = _new_workspace(ctl, sessions=1)
    _new_workspace(ctl, sessions=1)
    last, (last_session,) = _new_workspace(ctl, sessions=1)
    assert_ok(ctl.request("workspace.focus", first, mode="add"))
    assert_ok(ctl.request("workspace.focus", last, mode="add"))
    assert_ok(ctl.request("workspace.filter", mode="on"))
    assert_ok(ctl.request("workspace.select", first))

    assert assert_ok(ctl.request("workspace.go", to="next"))["id"] == last
    assert active_session_id(ctl.tree()) == last_session
    assert assert_ok(ctl.request("workspace.go", to="next"))["id"] == first
    assert active_session_id(ctl.tree()) == first_session
    _clear_focus(ctl)


@pytest.mark.covers("workspace.go")
@pytest.mark.parametrize("to", [None, "sideways", "first"])
def test_go_requires_a_direction(ctl, to):
    args = {} if to is None else {"to": to}
    assert_error(ctl.request("workspace.go", **args), "workspace.go requires --to next|prev")


@pytest.mark.covers("workspace.go", "workspace.delete")
def test_a_lone_workspace_has_nowhere_to_go_and_cannot_be_deleted(app, ctl):
    (wid,) = workspace_ids(ctl.tree())
    assert_error(ctl.request("workspace.go", to="next"), "no other workspace to navigate to")
    assert_error(ctl.request("workspace.delete", wid), "cannot delete last workspace")
    assert workspace_ids(ctl.tree()) == [wid]


@pytest.mark.covers("workspace.rename", "field:tree.workspace.name", "event:tree.changed")
def test_rename(ctl):
    cursor = ctl.events_baseline()
    wid, _ = _new_workspace(ctl, name="before")
    cursor = _settled_baseline(ctl, cursor)

    assert assert_ok(ctl.request("workspace.rename", wid, name="  after  "))["id"] == wid
    assert _node(ctl.tree(), wid)["name"] == "after"
    ctl.wait_event(cursor, "tree.changed", {"window": _window(ctl)})


@pytest.mark.covers("workspace.rename")
@pytest.mark.parametrize("name", [None, "   "])
def test_rename_requires_a_name(ctl, name):
    args = {} if name is None else {"name": name}
    assert_error(ctl.request("workspace.rename", "active", **args), "workspace.rename requires a name")


@pytest.mark.covers("workspace.delete", "event:session.closed", "event:session.selected", "event:tree.changed")
def test_delete_closes_the_workspace_sessions(ctl):
    window = _window(ctl)
    cursor = ctl.events_baseline()
    wid, (first, last) = _new_workspace(ctl, sessions=2)
    cursor = _settled_baseline(ctl, cursor)

    assert assert_ok(ctl.request("workspace.delete", wid))["id"] == wid
    tree = ctl.tree()
    assert wid not in workspace_ids(tree)
    assert not {first, last} & set(session_ids(tree))
    selected = active_session_id(tree)
    for sid in (first, last):
        ctl.wait_event(cursor, "session.closed", {"window": window, "workspace": wid, "session": sid})
    ctl.wait_event(cursor, "session.selected", {"session": selected, "payload": {"previous": last}})
    ctl.wait_event(cursor, "tree.changed", {"window": window})


@pytest.mark.covers("workspace.delete", "event:tree.changed")
def test_delete_empty_workspace(ctl):
    cursor = ctl.events_baseline()
    wid, _ = _new_workspace(ctl)
    cursor = _settled_baseline(ctl, cursor)

    assert assert_ok(ctl.request("workspace.delete", wid))["id"] == wid
    assert wid not in workspace_ids(ctl.tree())
    ctl.wait_event(cursor, "tree.changed", {"window": _window(ctl)})


@pytest.mark.covers("workspace.move", "event:tree.changed")
def test_move_reorders_among_siblings(ctl):
    cursor = ctl.events_baseline()
    _new_workspace(ctl)
    _new_workspace(ctl)
    wid, _ = _new_workspace(ctl)
    cursor = _settled_baseline(ctl, cursor)

    ids = workspace_ids(ctl.tree())
    # Each end gets one step past it, which leaves the order alone.
    for to in ["up", "up", "top", "up", "down", "bottom", "down", "top"]:
        assert assert_ok(ctl.request("workspace.move", wid, to=to))["id"] == wid, to
        ids = _reordered(ids, wid, to)
        assert workspace_ids(ctl.tree()) == ids, to
    ctl.wait_event(cursor, "tree.changed", {"window": _window(ctl)})


@pytest.mark.covers("workspace.move")
@pytest.mark.parametrize(
    ("args", "error"),
    [
        pytest.param({}, "workspace.move requires --to", id="missing"),
        pytest.param({"to": "left"}, "workspace.move --to must be up|down|top|bottom", id="unknown"),
    ],
)
def test_move_requires_a_direction(ctl, args, error):
    assert_error(ctl.request("workspace.move", "active", **args), error)


@pytest.mark.covers("workspace.focus", "field:tree.workspace.focused", "field:tree.workspaceFilter")
def test_focus_modes(ctl):
    _clear_focus(ctl)
    first, (first_session,) = _new_workspace(ctl, sessions=1)
    second, _ = _new_workspace(ctl, sessions=1)

    steps = [
        # Narrowing the view moves the selection into it.
        ("on", first, ({first}, True)),
        ("add", second, ({first, second}, True)),
        ("off", first, ({second}, True)),
        ("off", second, (set(), False)),
        (None, first, ({first}, True)),
        ("toggle", first, (set(), False)),
        ("add", first, ({first}, False)),
    ]
    for mode, wid, expected in steps:
        args = {} if mode is None else {"mode": mode}
        assert assert_ok(ctl.request("workspace.focus", wid, **args))["id"] == wid, mode
        assert _focus_state(ctl) == expected, mode
        if mode == "on":
            assert active_session_id(ctl.tree()) == first_session
    assert_absent(_node(ctl.tree(), second), "focused")
    _clear_focus(ctl)


@pytest.mark.covers("workspace.focus")
def test_focus_rejects_an_unknown_mode(ctl):
    # The references disagree on the text: upstream `invalid focus mode: pin (on|off|toggle|add)`, the Linux
    # fork's own dispatcher `invalid workspace.focus mode: pin`. Both name the rejected mode.
    before = ctl.tree()
    response = ctl.request("workspace.focus", "active", mode="pin")
    assert_error(response)
    assert "pin" in response.error, response.raw
    assert ctl.tree()["tree"]["workspaceFilter"] == before["tree"]["workspaceFilter"]


@pytest.mark.covers("workspace.filter", "field:tree.workspaceFilter", "field:tree.workspace.focused")
def test_filter_switches_the_view_without_touching_the_marks(ctl):
    _clear_focus(ctl)
    wid, _ = _new_workspace(ctl, sessions=1)

    assert_ok(ctl.request("workspace.filter", mode="on"))
    assert _focus_state(ctl) == (set(), False), "an empty set cannot be filtered to"
    assert_ok(ctl.request("workspace.focus", wid, mode="add"))
    for mode, enabled in [("on", True), ("toggle", False), (None, True), ("off", False)]:
        assert_ok(ctl.request("workspace.filter", **({} if mode is None else {"mode": mode})))
        assert _focus_state(ctl) == ({wid}, enabled), mode
    _clear_focus(ctl)


@pytest.mark.covers("workspace.filter")
def test_filter_rejects_an_unknown_mode(ctl):
    assert_error(ctl.request("workspace.filter", mode="show"), "invalid workspace filter mode: show")


@pytest.mark.covers("workspace.collapse", "workspace.expand", "field:tree.workspace.collapsed")
def test_collapse_and_expand(ctl):
    wid, _ = _new_workspace(ctl, sessions=1)
    for cmd, collapsed in [
        ("workspace.collapse", True),
        ("workspace.collapse", True),
        ("workspace.expand", False),
        ("workspace.expand", False),
    ]:
        assert assert_ok(ctl.request(cmd, wid))["id"] == wid, cmd
        node = _node(ctl.tree(), wid)
        if collapsed:
            assert node["collapsed"] is True, cmd
        else:
            assert_absent(node, "collapsed")


@pytest.mark.covers(
    "workspace.select",
    "workspace.rename",
    "workspace.delete",
    "workspace.move",
    "workspace.focus",
    "workspace.collapse",
    "workspace.expand",
)
@pytest.mark.parametrize(
    ("cmd", "args"),
    [
        ("workspace.select", {}),
        ("workspace.rename", {"name": "renamed"}),
        ("workspace.delete", {}),
        ("workspace.move", {"to": "up"}),
        ("workspace.focus", {"mode": "on"}),
        ("workspace.collapse", {}),
        ("workspace.expand", {}),
    ],
)
def test_unknown_workspace_target(ctl, cmd, args):
    assert_error(ctl.request(cmd, UNKNOWN, **args), f"no such workspace: {UNKNOWN}")
