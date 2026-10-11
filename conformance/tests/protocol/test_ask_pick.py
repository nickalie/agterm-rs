"""Questions (`ask.*`) and the native picker (`pick.*`). No protocol command answers either one, so the outcomes
reached here are `pending` and `cancelled`; a button, a hotkey, a picked row and a custom answer need the GUI (the ui
layer). Every test cancels what it opened, because a pending GUI ask or pick holds its window's modal slot."""

import contextlib

import pytest
from agterm_conformance.match import assert_absent, assert_error, assert_ok, find_session

UNKNOWN = "zzz"
YES_NO = [{"id": "yes", "label": "Yes", "hotkey": "Y"}, {"id": "no", "label": "No"}]
ITEMS = [{"id": "a", "label": "Alpha", "subtitle": "first"}, {"id": "b", "label": "Beta"}]


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


def _top(ctl):
    return ctl.tree()["tree"]


def _ask(ctl, target=None, **args):
    args.setdefault("buttons", YES_NO)
    return assert_ok(ctl.request("ask.open", target, title="Proceed?", **args))


def _ask_result(ctl, ask_id):
    return assert_ok(ctl.request("ask.result", ask_id))["ask"]


def _pick_result(ctl, pick_id):
    return assert_ok(ctl.request("pick.result", pick_id))["pick"]


@contextlib.contextmanager
def _cancelled(ctl, cmd, opened_id):
    """Cancel what the block opened even when an assertion inside it fails."""
    try:
        yield opened_id
    finally:
        ctl.request(cmd, opened_id)


def _pane_id(node, kind):
    (surface,) = [surface for surface in node["surfaces"] if surface["kind"] == kind]
    return surface["paneID"]


# -- terminal asks


@pytest.mark.covers("ask.open", "ask.result", "ask.cancel", "field:tree.session.ask", "field:result.ask")
def test_terminal_ask_waits_on_its_session(ctl):
    sid = _new(ctl)
    opened = _ask(ctl, sid, message="Deploy to staging.", defaultButton="yes", align="center", width=50)
    ask_id = opened["id"]
    with _cancelled(ctl, "ask.cancel", ask_id):
        assert_absent(opened, "pane")
        assert _node(ctl, sid)["ask"] == {"id": ask_id}
        assert_absent(_top(ctl), "askPending")
        assert _ask_result(ctl, ask_id) == {"result": "pending"}
        assert_error(ctl.request("ask.open", sid, title="Again?", buttons=YES_NO), "ask already pending")

        assert_ok(ctl.request("ask.cancel", ask_id))
        assert _ask_result(ctl, ask_id) == {"result": "cancelled"}
        assert_absent(_node(ctl, sid), "ask")
        # A finished ask cancels as a no-op and keeps its result.
        assert_ok(ctl.request("ask.cancel", ask_id))
        assert _ask_result(ctl, ask_id) == {"result": "cancelled"}


@pytest.mark.covers("ask.open", "field:tree.session.ask")
def test_terminal_ask_without_a_target_lands_on_the_selected_session(ctl):
    sid = _new(ctl)
    assert_ok(ctl.request("session.select", sid))
    with _cancelled(ctl, "ask.cancel", _ask(ctl)["id"]) as ask_id:
        assert _node(ctl, sid)["ask"] == {"id": ask_id}


@pytest.mark.covers("ask.open", "field:tree.session.ask")
def test_terminal_ask_waits_hidden_on_an_unselected_session(ctl):
    hidden, shown = _new(ctl), _new(ctl)
    with _cancelled(ctl, "ask.cancel", _ask(ctl, hidden)["id"]) as ask_id:
        assert _node(ctl, shown)["active"] is True
        assert _node(ctl, hidden)["ask"] == {"id": ask_id}


@pytest.mark.covers("ask.open", "field:tree.session.ask", "field:result.pane")
def test_terminal_ask_on_one_pane(ctl):
    sid = _split(ctl)
    opened = _ask(ctl, sid, pane="right")
    with _cancelled(ctl, "ask.cancel", opened["id"]) as ask_id:
        assert opened["pane"] == "right"
        assert _node(ctl, sid)["ask"] == {"id": ask_id, "pane": "right"}
    # A live pane id wins over the role beside it.
    left = _pane_id(_node(ctl, sid), "left")
    opened = _ask(ctl, sid, pane="right", paneID=left)
    with _cancelled(ctl, "ask.cancel", opened["id"]):
        assert opened["pane"] == "left"


@pytest.mark.covers("ask.open")
def test_terminal_ask_pane_must_be_laid_out(ctl):
    sid = _new(ctl)
    assert_error(ctl.request("ask.open", sid, title="Proceed?", buttons=YES_NO, pane="right"), "pane not visible")
    assert_error(
        ctl.request("ask.open", sid, title="Proceed?", buttons=YES_NO, paneID=UNKNOWN), f"unknown pane id: {UNKNOWN}"
    )
    assert_absent(_node(ctl, sid), "ask")


@pytest.mark.covers("ask.open", "ask.result")
def test_closing_the_session_cancels_its_ask(ctl):
    sid = _new(ctl)
    ask_id = _ask(ctl, sid)["id"]
    assert_ok(ctl.request("session.close", sid))
    assert _ask_result(ctl, ask_id) == {"result": "cancelled"}


# -- GUI asks and the modal slot


@pytest.mark.covers("ask.open", "ask.cancel", "field:tree.askPending", "field:tree.pickPending")
def test_gui_ask_holds_the_window_slot(ctl):
    sid = _new(ctl)
    with _cancelled(ctl, "ask.cancel", _ask(ctl, style="gui", destructiveButton="no")["id"]) as ask_id:
        assert _top(ctl)["askPending"] == ask_id
        assert_absent(_node(ctl, sid), "ask")
        assert _ask_result(ctl, ask_id) == {"result": "pending"}
        assert_error(ctl.request("ask.open", title="Second?", style="gui", buttons=YES_NO), "ask already pending")
        # The upstream app names the ask holding the slot, the Linux fork answers `pick already pending`.
        refused = ctl.request("pick.open", items=ITEMS)
        assert_error(refused)
        assert refused.error.endswith("already pending")
        assert_absent(_top(ctl), "pickPending")
        # A terminal ask has a slot of its own on its session.
        with _cancelled(ctl, "ask.cancel", _ask(ctl, sid)["id"]) as terminal_id:
            assert _node(ctl, sid)["ask"] == {"id": terminal_id}

        assert_ok(ctl.request("ask.cancel", ask_id))
        assert_absent(_top(ctl), "askPending")
        assert _ask_result(ctl, ask_id) == {"result": "cancelled"}


@pytest.mark.covers("ask.open")
def test_gui_ask_target_must_be_on_screen(ctl):
    shown = _new(ctl)
    hidden = _new(ctl)
    assert_ok(ctl.request("session.select", shown))
    assert_error(ctl.request("ask.open", hidden, title="Proceed?", buttons=YES_NO, style="gui"), "session not visible")
    assert_error(
        ctl.request("ask.open", title="Proceed?", buttons=YES_NO, style="gui", pane="left"), "--pane requires a session"
    )
    opened = _ask(ctl, shown, style="gui", pane="left")
    with _cancelled(ctl, "ask.cancel", opened["id"]) as ask_id:
        assert opened["pane"] == "left"
        assert _top(ctl)["askPending"] == ask_id
        assert_absent(_node(ctl, shown), "ask")


@pytest.mark.covers("ask.open", "field:tree.askPending")
def test_gui_ask_and_control_driven_covers(ctl, adapter):
    sid = _new(ctl)
    assert_ok(ctl.request("session.select", sid))
    with _cancelled(ctl, "ask.cancel", _ask(ctl, style="gui")["id"]) as ask_id:
        response = ctl.request("session.search", sid, text="needle")
        if adapter.has("modal-blocks-covers"):
            assert_error(response, "ask pending")
        else:
            # The Linux fork has no modal gate on control commands: the search runs under the dialog.
            assert assert_ok(response)["id"] == sid
            assert_ok(ctl.request("session.search", sid, to="close"))
        assert _top(ctl)["askPending"] == ask_id


# -- pick


@pytest.mark.covers("pick.open", "pick.result", "pick.cancel", "field:tree.pickPending", "field:result.pick")
def test_pick_waits_in_the_window_slot(ctl):
    opened = assert_ok(ctl.request("pick.open", items=ITEMS, prompt="Branch", query="", selection="b"))
    with _cancelled(ctl, "pick.cancel", opened["id"]) as pick_id:
        assert _top(ctl)["pickPending"] == pick_id
        assert _pick_result(ctl, pick_id) == {"result": "pending"}
        assert_error(ctl.request("pick.open", items=ITEMS), "pick already pending")
        assert_error(ctl.request("ask.open", title="Proceed?", buttons=YES_NO, style="gui"), "pick already pending")
        assert_absent(_top(ctl), "askPending")

        assert_ok(ctl.request("pick.cancel", pick_id))
        assert_absent(_top(ctl), "pickPending")
        assert _pick_result(ctl, pick_id) == {"result": "cancelled"}
        assert_ok(ctl.request("pick.cancel", pick_id))
        assert _pick_result(ctl, pick_id) == {"result": "cancelled"}


@pytest.mark.covers("pick.open", "field:tree.pickPending")
def test_pick_with_no_items_is_a_text_prompt(ctl):
    with _cancelled(ctl, "pick.cancel", assert_ok(ctl.request("pick.open", items=[], allowCustom=True))["id"]) as pid:
        assert _top(ctl)["pickPending"] == pid


# -- refusals


@pytest.mark.covers("ask.open")
@pytest.mark.parametrize(
    ("args", "error"),
    [
        pytest.param({"buttons": YES_NO}, "ask.open requires a title", id="no-title"),
        pytest.param({"title": "  ", "buttons": YES_NO}, "ask.open requires a title", id="blank-title"),
        pytest.param({"title": "Q"}, "ask.open requires buttons", id="no-buttons"),
        pytest.param({"title": "Q", "buttons": []}, "ask.open requires at least one button", id="empty-buttons"),
        pytest.param(
            {"title": "Q", "buttons": [{"id": str(n), "label": str(n)} for n in range(7)]},
            "too many buttons (max 6)",
            id="seven-buttons",
        ),
        pytest.param(
            {"title": "Q", "buttons": [{"id": "a", "label": ""}]}, "ask button label must not be empty", id="label"
        ),
        pytest.param(
            {"title": "Q", "buttons": [{"id": "a", "label": "A"}, {"id": "a", "label": "B"}]},
            "ask button ids must be unique",
            id="ids",
        ),
        pytest.param(
            {"title": "Q\n", "buttons": YES_NO}, "ask text must not contain control characters", id="title-control"
        ),
        pytest.param(
            {"title": "Q", "message": "a\tb", "buttons": YES_NO},
            "ask text must not contain control characters",
            id="message-control",
        ),
        pytest.param(
            {"title": "Q", "defaultButton": "maybe", "buttons": YES_NO}, "unknown default button: maybe", id="default"
        ),
        pytest.param(
            {"title": "Q", "destructiveButton": "maybe", "buttons": YES_NO},
            "unknown destructive button: maybe",
            id="destructive",
        ),
        pytest.param(
            {"title": "Q", "defaultButton": "no", "destructiveButton": "no", "buttons": YES_NO},
            "default button must not be destructive",
            id="default-destructive",
        ),
        pytest.param({"title": "Q", "style": "web", "buttons": YES_NO}, "unknown style", id="style"),
        pytest.param({"title": "Q", "align": "top", "buttons": YES_NO}, "unknown align", id="align"),
        pytest.param({"title": "Q", "width": 9, "buttons": YES_NO}, "width must be 10 to 100", id="width-low"),
        pytest.param({"title": "Q", "width": 101, "buttons": YES_NO}, "width must be 10 to 100", id="width-high"),
        pytest.param(
            {"title": "Q", "buttons": [{"id": "a", "label": "A", "hotkey": "1"}]},
            "ask button hotkey must be one ASCII letter",
            id="hotkey-digit",
        ),
        pytest.param(
            {"title": "Q", "buttons": [{"id": "a", "label": "A", "hotkey": "ab"}]},
            "ask button hotkey must be one ASCII letter",
            id="hotkey-long",
        ),
        pytest.param(
            {
                "title": "Q",
                "buttons": [{"id": "a", "label": "A", "hotkey": "x"}, {"id": "b", "label": "B", "hotkey": "X"}],
            },
            "ask button hotkeys must be unique",
            id="hotkey-twice",
        ),
        pytest.param({"title": "Q", "pane": "scratch", "buttons": YES_NO}, "--pane must be left or right", id="pane"),
    ],
)
def test_ask_refusals(ctl, args, error):
    sid = _new(ctl)
    assert_error(ctl.request("ask.open", sid, **args), error)
    assert_absent(_node(ctl, sid), "ask")
    assert_absent(_top(ctl), "askPending")


@pytest.mark.covers("pick.open")
@pytest.mark.parametrize(
    ("args", "error"),
    [
        pytest.param({}, "pick.open requires items", id="no-items"),
        pytest.param({"items": []}, "pick.open requires at least one item", id="empty"),
        pytest.param(
            {"items": [{"id": str(n), "label": str(n)} for n in range(1001)]}, "too many items (max 1000)", id="many"
        ),
        pytest.param({"items": [{"id": "a", "label": ""}]}, "pick item label must not be empty", id="label"),
        pytest.param(
            {"items": [{"id": "a", "label": "A"}, {"id": "a", "label": "B"}]}, "pick item ids must be unique", id="ids"
        ),
        pytest.param(
            {"items": [{"id": "a", "label": "A\x07"}]}, "item text must not contain control characters", id="label-bell"
        ),
        pytest.param(
            {"items": [{"id": "a", "label": "A", "subtitle": "x\ny"}]},
            "item text must not contain control characters",
            id="subtitle-newline",
        ),
        pytest.param({"items": ITEMS, "selection": "c"}, "pick select must name an item id", id="selection"),
        pytest.param(
            {"items": [], "allowCustom": True, "selection": "a"}, "pick select must name an item id", id="prompt-select"
        ),
    ],
)
def test_pick_refusals(ctl, args, error):
    assert_error(ctl.request("pick.open", **args), error)
    assert_absent(_top(ctl), "pickPending")


@pytest.mark.covers("ask.result", "ask.cancel", "pick.result", "pick.cancel")
@pytest.mark.parametrize("cmd", ["ask.result", "ask.cancel", "pick.result", "pick.cancel"])
def test_result_and_cancel_need_a_known_id(ctl, cmd):
    noun = cmd.split(".")[0]
    assert_error(ctl.request(cmd), f"{cmd} requires {'an ask' if noun == 'ask' else 'a pick'} id")
    assert_error(ctl.request(cmd, UNKNOWN), f"unknown {noun}: {UNKNOWN}")
