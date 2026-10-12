"""The answers no protocol command gives: an ask's button, a picked row, and the palettes and switcher a person opens.
Each test closes what it opened, since a pending ask or pick holds its window's modal slot."""

import pytest
from agterm_conformance.match import active_session_id, assert_ok, session_ids
from agterm_conformance.wait import wait_until

YES_NO = [{"id": "yes", "label": "Yes"}, {"id": "no", "label": "No"}]
ITEMS = [{"id": "alpha", "label": "Alpha", "subtitle": "first"}, {"id": "beta", "label": "Beta"}]


def _new(ctl, **args):
    sid = assert_ok(ctl.request("session.new", **args))["id"]
    ctl.wait_mounted(sid, "left")
    return sid


@pytest.mark.covers("ui:ask-dialog", "ui:ask-title", "ui:ask-message", "ui:ask-button-{choice}")
def test_ask_button_answers_the_ask(ctl, ui):
    sid = _new(ctl)
    ask = assert_ok(
        ctl.request("ask.open", sid, title="Ship it?", message="all checks passed", buttons=YES_NO, style="gui")
    )["id"]
    try:
        ui.find("ask-dialog")
        assert ui.text("ask-title") == "Ship it?"
        assert ui.text("ask-message") == "all checks passed"
        ui.find(ui.ui_ids.expand("ask-button-{choice}", choice="no"), name="No")
        ui.click(ui.find(ui.ui_ids.expand("ask-button-{choice}", choice="yes"), name="Yes"))
        answer = wait_until(lambda: assert_ok(ctl.request("ask.result", ask))["ask"].get("id"))
        assert answer == "yes"
        ui.absent("ask-dialog")
    finally:
        ctl.request("ask.cancel", ask)
        ctl.request("session.close", sid)


def _row_texts(ui, item):
    return [ui.text(element) for element in ui.all(ui.ui_ids.expand("palette-item-{item}", item=item))]


@pytest.mark.covers("ui:pick-palette", "ui:pick-scrim", "ui:palette-item-{item}")
def test_pick_palette_returns_the_chosen_row(ctl, ui):
    pick = assert_ok(ctl.request("pick.open", items=ITEMS))["id"]
    try:
        ui.find("pick-palette")
        ui.find("pick-scrim")
        # The row's id is also its texts': the subtitle reads under the row, never as `palette-subtitle`.
        wait_until(lambda: {"Alpha", "first"} <= set(_row_texts(ui, "alpha")) and "Beta" in _row_texts(ui, "beta"))
        ui.press("down", "return")
        picked = wait_until(lambda: assert_ok(ctl.request("pick.result", pick))["pick"].get("id"))
        assert picked == "beta"
        ui.absent("pick-palette")
    finally:
        ctl.request("pick.cancel", pick)


@pytest.mark.covers("ui:command-palette", "ui:palette-scrim")
def test_command_palette_opens_from_the_menu_and_escape_closes_it(launch, ui_for):
    ctl = launch()
    ui = ui_for(ctl)
    (sid,) = session_ids(ctl.tree())
    ctl.wait_mounted(sid, "left")
    ui.menu("Navigate", "Command Palette")
    ui.find("command-palette", role="text-field")
    ui.find("palette-scrim")
    ui.press("escape")
    ui.absent("command-palette")


@pytest.mark.covers("ui:session-switcher")
def test_ctrl_tab_opens_the_session_switcher_and_escape_keeps_the_session(app, ctl, ui):
    (first,) = session_ids(ctl.tree())
    ctl.wait_mounted(first, "left")
    second = _new(ctl)
    # No Ctrl release is posted, so the switcher stays up until Escape.
    ui.press("ctrl+tab")
    ui.find("session-switcher")
    ui.press("escape")
    ui.absent("session-switcher")
    assert active_session_id(ctl.tree()) == second
