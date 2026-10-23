"""Session commands: lifecycle, naming, selection, order and the per-session state the sidebar shows.
The module shares one instance, so every test builds the sessions it needs and reads the rest of the tree
relative to what is already there."""

import pytest
from agterm_conformance.match import (
    ANY,
    active_session_id,
    active_workspace_id,
    assert_absent,
    assert_error,
    assert_ok,
    assert_subset,
    find_session,
    session_ids,
    window_ids,
    workspace_ids,
    workspaces,
)

UNKNOWN = "zzz"


def _window(ctl):
    (window,) = window_ids(ctl.request("window.list"))
    return window


def _new(ctl, **args):
    return assert_ok(ctl.request("session.new", **args))["id"]


def _new_workspace(ctl, sessions=0):
    wid = assert_ok(ctl.request("workspace.new"))["id"]
    return wid, [_new(ctl, workspace=wid) for _ in range(sessions)]


def _node(ctl, sid):
    return find_session(ctl.tree(), sid)


def _settled_baseline(ctl, cursor):
    """`tree.changed` is debounced per window: wait out the one the preparation since `cursor` scheduled."""
    ctl.wait_event(cursor, "tree.changed")
    return ctl.events_baseline()


def _reordered(ids, sid, to):
    ids = list(ids)
    index = ids.index(sid)
    ids.pop(index)
    destination = {"up": max(index - 1, 0), "down": min(index + 1, len(ids)), "top": 0, "bottom": len(ids)}[to]
    ids.insert(destination, sid)
    return ids


# -- lifecycle


@pytest.mark.covers(
    "session.new",
    "field:result.id",
    "field:tree.session.name",
    "event:session.created",
    "event:session.selected",
)
def test_new_appends_a_selected_session_to_the_current_workspace(ctl):
    before = ctl.tree()
    workspace = active_workspace_id(before)
    previous = active_session_id(before)
    cursor = ctl.events_baseline()

    sid = _new(ctl, name="  alpha  ")
    tree = ctl.tree()
    assert session_ids(tree, workspace) == [*session_ids(before, workspace), sid]
    assert_subset(find_session(tree, sid), {"name": "alpha", "active": True})
    assert active_session_id(tree) == sid
    where = {"window": _window(ctl), "workspace": workspace, "session": sid}
    ctl.wait_event(cursor, "session.created", {**where, "payload": {"name": "alpha"}})
    ctl.wait_event(cursor, "session.selected", {**where, "payload": {"previous": previous}})


@pytest.mark.covers("session.new", "field:tree.session.active")
def test_new_without_selecting_keeps_the_selection(ctl):
    selected = active_session_id(ctl.tree())
    sid = _new(ctl, noSelect=True)
    tree = ctl.tree()
    assert active_session_id(tree) == selected
    assert find_session(tree, sid)["active"] is False


@pytest.mark.covers("session.new")
def test_new_places_next_to_an_anchor(ctl):
    wid, (first, last) = _new_workspace(ctl, sessions=2)
    after = _new(ctl, after=first)
    before = _new(ctl, before=first)
    assert session_ids(ctl.tree(), wid) == [before, first, after, last]


@pytest.mark.covers("session.new", "field:tree.workspace.name")
def test_new_into_a_workspace_by_name(ctl):
    name = f"named {len(workspaces(ctl.tree()))}"
    assert_error(
        ctl.request("session.new", workspaceName=name),
        f'no workspace named "{name}" (pass --create-workspace to add it)',
    )
    created = _new(ctl, workspaceName=name, createWorkspace=True)
    tree = ctl.tree()
    (node,) = [node for node in workspaces(tree) if node["name"] == name]
    assert session_ids(tree, node["id"]) == [created]
    reused = _new(ctl, workspaceName=f"  {name}  ", createWorkspace=True)
    tree = ctl.tree()
    assert [node["name"] for node in workspaces(tree)].count(name) == 1
    assert session_ids(tree, node["id"]) == [created, reused]


@pytest.mark.covers("session.new")
def test_new_into_a_workspace_by_id(ctl):
    wid, _ = _new_workspace(ctl)
    sid = _new(ctl, workspace=wid)
    assert session_ids(ctl.tree(), wid) == [sid]


@pytest.mark.covers("session.new")
@pytest.mark.parametrize(
    ("args", "error"),
    [
        pytest.param({"after": "A", "before": "A"}, "use either --after or --before, not both", id="both-anchors"),
        pytest.param(
            {"after": "A", "workspace": "active"},
            "session.new takes --after/--before or a workspace, not both",
            id="anchor-and-workspace",
        ),
        pytest.param(
            {"before": "A", "workspaceName": "x"},
            "session.new takes --after/--before or a workspace, not both",
            id="anchor-and-workspace-name",
        ),
        pytest.param(
            {"workspace": "active", "workspaceName": "x"},
            "use either --workspace or --workspace-name, not both",
            id="workspace-and-name",
        ),
        pytest.param({"createWorkspace": True}, "--create-workspace requires --workspace-name", id="create-alone"),
        pytest.param({"wait": True}, "--wait requires --command", id="wait-alone"),
        pytest.param({"workspace": UNKNOWN}, f"no such workspace: {UNKNOWN}", id="unknown-workspace"),
        pytest.param({"after": UNKNOWN}, f"no such session: {UNKNOWN}", id="unknown-anchor"),
    ],
)
def test_new_rejects(ctl, args, error):
    anchor = active_session_id(ctl.tree())
    before = session_ids(ctl.tree())
    args = {key: anchor if value == "A" else value for key, value in args.items()}
    assert_error(ctl.request("session.new", **args), error)
    assert session_ids(ctl.tree()) == before


@pytest.mark.covers("session.duplicate", "field:tree.session.cwd")
def test_duplicate_inserts_a_plain_copy_after_the_source(ctl):
    wid, (source, last) = _new_workspace(ctl, sessions=2)
    assert_ok(ctl.request("session.rename", source, name="original"))
    assert_ok(ctl.request("session.flag", source, mode="on"))
    assert_ok(ctl.request("session.context", source, mode="set", text="what it is for"))

    copy = assert_ok(ctl.request("session.duplicate", source))["id"]
    tree = ctl.tree()
    assert session_ids(tree, wid) == [source, copy, last]
    node = find_session(tree, copy)
    assert node["cwd"] == find_session(tree, source)["cwd"]
    assert node["name"] != "original"
    assert node["flagged"] is False
    assert_absent(node, "context", "status", "restoreCommand", "background")
    assert_ok(ctl.request("session.flag", source, mode="off"))


@pytest.mark.covers("session.close", "event:session.closed", "event:session.selected")
def test_close_removes_the_session_and_reselects(ctl):
    wid, (keep, closing) = _new_workspace(ctl, sessions=2)
    assert_ok(ctl.request("session.select", closing))
    cursor = ctl.events_baseline()

    assert assert_ok(ctl.request("session.close", closing))["id"] == closing
    tree = ctl.tree()
    assert session_ids(tree, wid) == [keep]
    assert active_session_id(tree) == keep
    where = {"window": _window(ctl), "workspace": wid}
    ctl.wait_event(cursor, "session.closed", {**where, "session": closing})
    ctl.wait_event(cursor, "session.selected", {**where, "session": keep, "payload": {"previous": closing}})


@pytest.mark.covers("session.close", "field:result.affected")
def test_close_several_targets_at_once(ctl):
    wid, (first, keep, last) = _new_workspace(ctl, sessions=3)
    result = assert_ok(ctl.request("session.close", targets=[first, last, first]))
    assert result["affected"] == 2
    assert session_ids(ctl.tree(), wid) == [keep]


@pytest.mark.covers("session.close")
def test_close_several_targets_is_atomic(ctl):
    wid, sids = _new_workspace(ctl, sessions=2)
    assert_error(ctl.request("session.close", targets=[*sids, UNKNOWN]), f"no such session: {UNKNOWN}")
    assert session_ids(ctl.tree(), wid) == sids


@pytest.mark.covers("session.close")
def test_close_requires_a_target(ctl):
    assert_error(ctl.request("session.close", targets=[]), "session.close requires at least one --target")


# -- selection


@pytest.mark.covers("session.select", "field:tree.session.active", "field:tree.workspace.active")
def test_select_moves_the_selection_across_workspaces(ctl):
    wid, (sid,) = _new_workspace(ctl, sessions=1)
    _new_workspace(ctl, sessions=1)
    previous = active_session_id(ctl.tree())
    cursor = ctl.events_baseline()

    assert assert_ok(ctl.request("session.select", sid))["id"] == sid
    tree = ctl.tree()
    assert active_session_id(tree) == sid
    assert active_workspace_id(tree) == wid
    ctl.wait_event(cursor, "session.selected", {"session": sid, "payload": {"previous": previous}})


@pytest.mark.covers("session.go", "field:result.id")
def test_go_steps_through_sessions_and_wraps(ctl):
    _new_workspace(ctl, sessions=2)
    ids = session_ids(ctl.tree())
    assert_ok(ctl.request("session.select", ids[0]))

    current = ids[0]
    for direction in ["prev", "next", "next", "last", "next", "first", "prev"]:
        index = ids.index(current)
        current = {
            "next": ids[(index + 1) % len(ids)],
            "prev": ids[(index - 1) % len(ids)],
            "first": ids[0],
            "last": ids[-1],
        }[direction]
        assert assert_ok(ctl.request("session.go", to=direction))["id"] == current, direction
        assert active_session_id(ctl.tree()) == current, direction


@pytest.mark.covers("session.go")
@pytest.mark.parametrize("to", [None, "sideways"])
def test_go_requires_a_direction(ctl, to):
    args = {} if to is None else {"to": to}
    assert_error(
        ctl.request("session.go", **args),
        "session.go requires --to next|prev|first|last|next-attention|prev-attention",
    )


# -- naming


@pytest.mark.covers("session.rename", "field:tree.session.name", "event:tree.changed")
def test_rename_and_blank_restores_the_automatic_name(ctl):
    cursor = ctl.events_baseline()
    sid = _new(ctl)
    automatic = _node(ctl, sid)["name"]
    cursor = _settled_baseline(ctl, cursor)

    assert assert_ok(ctl.request("session.rename", sid, name="  custom  "))["id"] == sid
    assert _node(ctl, sid)["name"] == "custom"
    ctl.wait_event(cursor, "tree.changed", {"window": _window(ctl)})
    assert_ok(ctl.request("session.rename", sid, name="   "))
    assert _node(ctl, sid)["name"] == automatic


@pytest.mark.covers("session.rename")
def test_rename_requires_a_name(ctl):
    assert_error(ctl.request("session.rename", "active"), "session.rename requires a name")


# -- order


@pytest.mark.covers("session.move", "event:tree.changed")
def test_move_reorders_within_the_workspace(ctl):
    cursor = ctl.events_baseline()
    wid, ids = _new_workspace(ctl, sessions=3)
    cursor = _settled_baseline(ctl, cursor)
    sid = ids[-1]
    # Each end gets one step past it, which leaves the order alone.
    for to in ["up", "up", "up", "top", "down", "bottom", "down", "top"]:
        assert assert_ok(ctl.request("session.move", sid, to=to))["id"] == sid, to
        ids = _reordered(ids, sid, to)
        assert session_ids(ctl.tree(), wid) == ids, to
    ctl.wait_event(cursor, "tree.changed", {"window": _window(ctl)})


@pytest.mark.covers("session.move")
def test_move_to_another_workspace_appends(ctl):
    source, (moving, staying) = _new_workspace(ctl, sessions=2)
    destination, (resident,) = _new_workspace(ctl, sessions=1)
    assert assert_ok(ctl.request("session.move", moving, workspace=destination))["id"] == moving
    tree = ctl.tree()
    assert session_ids(tree, source) == [staying]
    assert session_ids(tree, destination) == [resident, moving]


@pytest.mark.covers("session.move")
def test_move_next_to_an_anchor(ctl):
    source, (moving, other) = _new_workspace(ctl, sessions=2)
    destination, (first, last) = _new_workspace(ctl, sessions=2)
    assert_ok(ctl.request("session.move", moving, before=last))
    assert session_ids(ctl.tree(), destination) == [first, moving, last]
    assert_ok(ctl.request("session.move", moving, after=other))
    tree = ctl.tree()
    assert session_ids(tree, source) == [other, moving]
    assert session_ids(tree, destination) == [first, last]


@pytest.mark.covers("session.move")
def test_move_after_an_anchor_below_it(ctl):
    wid, (moving, anchor, last) = _new_workspace(ctl, sessions=3)
    assert assert_ok(ctl.request("session.move", moving, after=anchor))["id"] == moving
    assert session_ids(ctl.tree(), wid) == [anchor, moving, last]


@pytest.mark.covers("session.move", "field:result.affected")
def test_move_several_targets(ctl):
    source, (first, keep, last) = _new_workspace(ctl, sessions=3)
    destination, (resident,) = _new_workspace(ctl, sessions=1)
    assert assert_ok(ctl.request("session.move", workspace=destination, targets=[last, first]))["affected"] == 2
    tree = ctl.tree()
    assert session_ids(tree, source) == [keep]
    # A batch keeps tree order, whatever order it was given in.
    assert session_ids(tree, destination) == [resident, first, last]
    assert assert_ok(ctl.request("session.move", targets=[first, last], before=keep))["affected"] == 2
    assert session_ids(ctl.tree(), source) == [first, last, keep]


@pytest.mark.covers("session.move")
@pytest.mark.parametrize(
    ("args", "error"),
    [
        pytest.param({}, "session.move requires --to or a workspace", id="nothing"),
        pytest.param({"to": "left"}, "session.move --to must be up|down|top|bottom", id="unknown-direction"),
        pytest.param(
            {"to": "up", "workspace": "W"}, "session.move takes either --to or a workspace, not both", id="to-and-ws"
        ),
        pytest.param({"after": "A", "before": "A"}, "use either --after or --before, not both", id="both-anchors"),
        pytest.param(
            {"after": "A", "to": "up"}, "session.move takes --after/--before or --to, not both", id="anchor-and-to"
        ),
        pytest.param(
            {"before": "A", "workspace": "W"},
            "session.move takes --after/--before or a workspace, not both",
            id="anchor-and-workspace",
        ),
        pytest.param(
            {"to": "up", "targets": ["S", "A"]},
            "session.move --target can be repeated only with a workspace or --after/--before",
            id="batch-reorder",
        ),
        pytest.param(
            {"workspace": "W", "targets": []}, "session.move requires at least one --target", id="empty-batch"
        ),
        pytest.param({"workspace": UNKNOWN}, f"no such workspace: {UNKNOWN}", id="unknown-workspace"),
        pytest.param({"after": UNKNOWN}, f"no such session: {UNKNOWN}", id="unknown-anchor"),
    ],
)
def test_move_rejects(ctl, args, error):
    wid, (sid, anchor) = _new_workspace(ctl, sessions=2)
    names = {"S": sid, "A": anchor, "W": wid}
    args = {
        key: [names.get(item, item) for item in value] if isinstance(value, list) else names.get(value, value)
        for key, value in args.items()
    }
    assert_error(ctl.request("session.move", sid, **args), error)
    assert session_ids(ctl.tree(), wid) == [sid, anchor]


# -- sidebar state


@pytest.mark.covers("session.flag", "field:tree.session.flagged")
def test_flag_modes(ctl):
    sid = _new(ctl)
    for mode, flagged in [(None, True), (None, False), ("on", True), ("on", True), ("toggle", False), ("off", False)]:
        args = {} if mode is None else {"mode": mode}
        assert assert_ok(ctl.request("session.flag", sid, **args))["id"] == sid, mode
        assert _node(ctl, sid)["flagged"] is flagged, mode


@pytest.mark.covers("session.flag", "field:tree.session.flagged")
def test_flag_clear_unflags_every_session(ctl):
    flagged = [_new(ctl) for _ in range(2)]
    for sid in flagged:
        assert_ok(ctl.request("session.flag", sid, mode="on"))
    assert_ok(ctl.request("session.flag", mode="clear"))
    assert not [node["id"] for node in workspaces(ctl.tree()) for node in node["sessions"] if node["flagged"]]


@pytest.mark.covers("session.flag")
def test_flag_rejects_an_unknown_mode(ctl):
    assert_error(ctl.request("session.flag", "active", mode="pin"), "invalid flag mode: pin")


@pytest.mark.covers("session.seen", "field:tree.session.unseen")
def test_seen_clears_the_unseen_count(ctl):
    sid = _new(ctl, noSelect=True)
    assert_ok(ctl.request("notify", sid, body="one"))
    assert_ok(ctl.request("notify", sid, body="two"))
    assert _node(ctl, sid)["unseen"] == 2
    assert assert_ok(ctl.request("session.seen", sid))["id"] == sid
    node = _node(ctl, sid)
    assert_absent(node, "unseen")
    assert node["active"] is False


@pytest.mark.covers("session.reveal")
def test_reveal_answers_with_the_session(ctl):
    sid = _new(ctl, noSelect=True)
    assert assert_ok(ctl.request("session.reveal", sid))["id"] == sid


@pytest.mark.covers("session.context", "field:tree.session.context", "event:tree.changed")
def test_context_set_and_clear(ctl):
    cursor = ctl.events_baseline()
    sid = _new(ctl)
    cursor = _settled_baseline(ctl, cursor)

    assert assert_ok(ctl.request("session.context", sid, mode="set", text="  what it is for  "))["id"] == sid
    assert _node(ctl, sid)["context"] == "what it is for"
    ctl.wait_event(cursor, "tree.changed", {"window": _window(ctl)})
    assert_ok(ctl.request("session.context", sid, mode="clear"))
    assert_absent(_node(ctl, sid), "context")


@pytest.mark.covers("session.context")
@pytest.mark.parametrize(
    ("args", "error"),
    [
        pytest.param({}, "invalid context mode:  (set|clear)", id="no-mode"),
        pytest.param({"mode": "add"}, "invalid context mode: add (set|clear)", id="unknown-mode"),
        pytest.param({"mode": "set"}, "session.context set requires text", id="set-without-text"),
        pytest.param({"mode": "clear", "text": "x"}, "session.context clear takes no text", id="clear-with-text"),
        pytest.param(
            {"mode": "set", "text": "   "}, "context must not be empty (use --clear to remove it)", id="blank"
        ),
        pytest.param(
            {"mode": "set", "text": "a\nb"},
            "context must not contain control characters or line breaks",
            id="line-break",
        ),
        pytest.param({"mode": "set", "text": "x" * 257}, "context must be at most 256 UTF-8 bytes", id="too-long"),
    ],
)
def test_context_rejects_and_keeps_the_previous_value(ctl, args, error):
    sid = _new(ctl)
    assert_ok(ctl.request("session.context", sid, mode="set", text="kept"))
    assert_error(ctl.request("session.context", sid, **args), error)
    assert _node(ctl, sid)["context"] == "kept"


# -- status


@pytest.mark.covers(
    "session.status",
    "field:tree.session.status",
    "field:tree.session.statusBlink",
    "field:tree.session.statusColor",
    "field:tree.session.statusPane",
    "field:tree.session.statusChangedAt",
    "event:status",
)
def test_status_sets_the_indicator_and_idle_clears_it(ctl):
    sid = _new(ctl)
    cursor = ctl.events_baseline()

    assert assert_ok(ctl.request("session.status", sid, status="blocked", blink=True, color="#ff8800"))["id"] == sid
    node = _node(ctl, sid)
    assert_subset(node, {"status": "blocked", "statusBlink": True, "statusColor": "#ff8800", "statusChangedAt": ANY})
    assert_absent(node, "statusPane")
    ctl.wait_event(cursor, "status", {"session": sid, "payload": {"status": "blocked"}})

    # A fresh indicator per call: overrides the next call leaves out are gone.
    assert_ok(ctl.request("session.status", sid, status="active"))
    node = _node(ctl, sid)
    assert node["status"] == "active"
    assert_absent(node, "statusBlink", "statusColor")
    changed = node["statusChangedAt"]

    assert_ok(ctl.request("session.status", sid, status="idle"))
    node = _node(ctl, sid)
    assert_absent(node, "status", "statusBlink", "statusColor")
    assert node["statusChangedAt"] >= changed
    ctl.wait_event(cursor, "status", {"session": sid, "payload": {"status": "idle"}})


@pytest.mark.covers("session.status", "field:tree.session.statusPane")
def test_status_records_the_pane(ctl):
    sid = _new(ctl)
    assert_ok(ctl.request("session.scratch", sid, mode="on"))
    assert_ok(ctl.request("session.status", sid, status="completed", pane="scratch"))
    assert_subset(_node(ctl, sid), {"status": "completed", "statusPane": "scratch"})
    assert_ok(ctl.request("session.status", sid, status="idle", pane="scratch"))
    assert_ok(ctl.request("session.scratch", sid, mode="off"))


@pytest.mark.covers("session.status", "field:tree.session.statusShape")
def test_status_shape(ctl):
    sid = _new(ctl)
    assert_ok(ctl.request("session.status", sid, status="active", shape="star"))
    rejected = ctl.request("session.status", sid, status="blocked", shape="hexagon")
    assert_error(rejected, "invalid shape: hexagon (circle|square|triangle|diamond|capsule|star)")
    assert_subset(_node(ctl, sid), {"status": "active", "statusShape": "star"})


@pytest.mark.covers("session.status")
@pytest.mark.parametrize(
    ("args", "error"),
    [
        pytest.param({}, "invalid status", id="missing"),
        pytest.param({"status": "busy"}, "invalid status", id="unknown"),
        pytest.param({"status": "active", "color": "orange"}, "invalid color (expected #rrggbb)", id="color"),
        pytest.param({"status": "active", "pane": "middle"}, "--pane must be left, right, or scratch", id="pane"),
    ],
)
def test_status_rejects(ctl, args, error):
    sid = _new(ctl)
    assert_error(ctl.request("session.status", sid, **args), error)
    assert_absent(_node(ctl, sid), "status")


# -- restore pins


@pytest.mark.covers(
    "session.restore",
    "field:tree.session.restoreCommand",
    "field:result.text",
)
def test_restore_pins_read_back_without_touching_the_shell(ctl):
    sid = _new(ctl)
    result = assert_ok(ctl.request("session.restore", sid, mode="set", command="echo 'restored' | cat"))
    assert result["id"] == sid
    # Outside `rerun` the pin is saved with a note naming the active restore mode.
    assert isinstance(result["text"], str) and result["text"]
    assert _node(ctl, sid)["restoreCommand"] == "echo 'restored' | cat"
    assert_ok(ctl.request("session.restore", sid, mode="none"))
    assert _node(ctl, sid)["restoreCommand"] == ""
    result = assert_ok(ctl.request("session.restore", sid, mode="clear"))
    assert_absent(result, "text")
    assert_absent(_node(ctl, sid), "restoreCommand")


@pytest.mark.covers("session.restore")
@pytest.mark.parametrize(
    ("args", "error"),
    [
        pytest.param({}, "invalid restore mode:  (set|none|clear)", id="no-mode"),
        pytest.param({"mode": "pin"}, "invalid restore mode: pin (set|none|clear)", id="unknown-mode"),
        pytest.param({"mode": "set"}, "session.restore set requires a command", id="set-without-command"),
        pytest.param(
            {"mode": "set", "command": "a\tb"}, "command must not contain control characters", id="control-character"
        ),
        pytest.param({"mode": "set", "command": "x" * 1025}, "command too long (max 1024 bytes)", id="too-long"),
        pytest.param({"mode": "none", "pane": "middle"}, "--pane must be left, right, or scratch", id="bad-pane"),
        pytest.param({"mode": "none", "pane": "right"}, "session has no split", id="right-without-split"),
        pytest.param({"mode": "none", "pane": "scratch"}, "the scratch terminal is never restored", id="scratch"),
        pytest.param({"mode": "none", "paneID": UNKNOWN}, f"unknown pane id: {UNKNOWN}", id="unknown-pane-id"),
    ],
)
def test_restore_rejects(ctl, args, error):
    sid = _new(ctl)
    assert_error(ctl.request("session.restore", sid, **args), error)
    assert_absent(_node(ctl, sid), "restoreCommand")


@pytest.mark.covers(
    "session.close",
    "session.select",
    "session.rename",
    "session.reveal",
    "session.duplicate",
    "session.flag",
    "session.seen",
    "session.status",
    "session.context",
    "session.restore",
)
@pytest.mark.parametrize(
    ("cmd", "args"),
    [
        ("session.close", {}),
        ("session.select", {}),
        ("session.rename", {"name": "renamed"}),
        ("session.reveal", {}),
        ("session.duplicate", {}),
        ("session.flag", {"mode": "on"}),
        ("session.seen", {}),
        ("session.status", {"status": "active"}),
        ("session.context", {"mode": "set", "text": "x"}),
        ("session.restore", {"mode": "none"}),
    ],
)
def test_unknown_session_target(ctl, cmd, args):
    before = workspace_ids(ctl.tree())
    assert_error(ctl.request(cmd, UNKNOWN, **args), f"no such session: {UNKNOWN}")
    assert workspace_ids(ctl.tree()) == before
