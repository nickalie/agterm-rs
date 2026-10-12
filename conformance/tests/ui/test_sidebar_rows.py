"""The sidebar: rows, row glyphs and the footer buttons, found by their ui-ids.yaml ids and checked against the tree.
Every test leaves the shared instance's flags, focus set and sidebar mode as it found them."""

import pytest
from agterm_conformance.match import assert_absent, assert_ok, find_session, session_ids, workspace_ids
from agterm_conformance.wait import wait_until


def _new(ctl, **args):
    sid = assert_ok(ctl.request("session.new", **args))["id"]
    ctl.wait_mounted(sid, "left")
    return sid


def _texts(ui, ui_id):
    return [ui.text(element) for element in ui.all(ui_id)]


@pytest.mark.covers("ui:session-row", "ui:workspace-row", "ui:session-icon", "ui:workspace-icon")
def test_rows_carry_the_names_the_tree_reads(ctl, ui):
    wid = assert_ok(ctl.request("workspace.new", name="ui-rows-ws"))["id"]
    sid = _new(ctl, workspace=wid)
    assert_ok(ctl.request("session.rename", sid, name="ui-rows-session"))
    wait_until(lambda: "ui-rows-session" in _texts(ui, "session-row") and "ui-rows-ws" in _texts(ui, "workspace-row"))
    assert ui.all("session-icon") and ui.all("workspace-icon")
    assert_ok(ctl.request("workspace.delete", wid))
    wait_until(lambda: "ui-rows-session" not in _texts(ui, "session-row"))


@pytest.mark.covers("ui:add-session")
def test_add_session_menu_opens_a_session(ctl, ui):
    before = set(session_ids(ctl.tree()))
    add = ui.find("add-session", role="menu-button")
    ui.click(add)
    ui.click(ui.menu_item("New Session"))
    tree = ctl.wait_tree(lambda tree: set(session_ids(tree)) - before)
    (created,) = set(session_ids(tree)) - before
    ctl.wait_mounted(created, "left")
    assert_ok(ctl.request("session.close", created))


@pytest.mark.covers("ui:workspace-add-session")
def test_workspace_add_session_opens_a_session_in_its_workspace(app, ctl, ui):
    (wid,) = workspace_ids(ctl.tree())
    (first,) = session_ids(ctl.tree())
    ctl.wait_mounted(first, "left")
    # Drawn only under the pointer.
    ui.hover(ui.find("workspace-row"))
    ui.click(ui.find("workspace-add-session", role="button"))
    tree = ctl.wait_tree(lambda tree: len(session_ids(tree, wid)) == 2)
    (created,) = set(session_ids(tree, wid)) - {first}
    ctl.wait_mounted(created, "left")


@pytest.mark.covers("ui:edit-field")
def test_rename_session_edits_the_row_in_place(ctl, ui):
    sid = _new(ctl)
    ui.menu("File", "Rename Session")
    ui.find("edit-field", role="text-field")
    ui.press("cmd+a", *"uirenamed", "return")
    ctl.wait_tree(lambda tree: find_session(tree, sid)["name"] == "uirenamed")
    ui.absent("edit-field")
    assert_ok(ctl.request("session.close", sid))


@pytest.mark.covers("ui:notify-badge")
def test_notify_badge_counts_a_background_session_notifications(ctl, ui):
    sid = _new(ctl, noSelect=True)
    for body in ("one", "two"):
        assert_ok(ctl.request("notify", sid, body=body))
    ctl.wait_tree(lambda tree: find_session(tree, sid).get("unseen") == 2)
    ui.find("notify-badge", role="text", value="2")
    assert_ok(ctl.request("session.close", sid))


@pytest.mark.covers("ui:agent-status")
def test_agent_status_glyph_reads_the_session_status(ctl, ui):
    sid = _new(ctl)
    assert_ok(ctl.request("session.status", sid, status="blocked"))
    ui.find("agent-status", role="text", value="blocked")
    assert_ok(ctl.request("session.status", sid, status="completed"))
    ui.find("agent-status", role="text", value="completed")
    assert_ok(ctl.request("session.close", sid))
    wait_until(lambda: "completed" not in _texts(ui, "agent-status"))


@pytest.mark.covers("ui:flagged-view-toggle")
def test_flagged_view_toggle_switches_the_sidebar_mode(ctl, ui):
    assert ctl.tree()["tree"]["sidebarMode"] == "tree"
    ui.find("flagged-view-toggle", role="button", enabled=False)
    sid = _new(ctl)
    assert_ok(ctl.request("session.flag", sid, mode="on"))
    try:
        ui.click(ui.find("flagged-view-toggle", enabled=True))
        ctl.wait_tree(lambda tree: tree["tree"]["sidebarMode"] == "flagged")
        ui.click("flagged-view-toggle")
        ctl.wait_tree(lambda tree: tree["tree"]["sidebarMode"] == "tree")
    finally:
        ctl.request("sidebar.mode", mode="tree")
        ctl.request("session.flag", mode="clear")
        ctl.request("session.close", sid)


@pytest.mark.covers("ui:focus-filter-toggle")
def test_focus_filter_toggle_applies_the_focus_set(ctl, ui):
    for wid in workspace_ids(ctl.tree()):
        ctl.request("workspace.focus", wid, mode="off")
    ui.find("focus-filter-toggle", role="button", enabled=False, value="off")
    wid = workspace_ids(ctl.tree())[0]
    assert_ok(ctl.request("workspace.focus", wid, mode="add"))
    try:
        ui.click(ui.find("focus-filter-toggle", enabled=True, value="off"))
        ctl.wait_tree(lambda tree: tree["tree"]["workspaceFilter"] is True)
        ui.find("focus-filter-toggle", value="on")
        ui.click("focus-filter-toggle")
        ctl.wait_tree(lambda tree: tree["tree"]["workspaceFilter"] is False)
    finally:
        ctl.request("workspace.focus", wid, mode="off")


@pytest.mark.covers("field:tree.idleMs")
def test_idle_ms_starts_with_a_gui_keystroke(app, ctl, ui):
    (sid,) = session_ids(ctl.tree())
    ctl.wait_mounted(sid, "left")
    assert_absent(ctl.tree()["tree"], "idleMs")
    # A letter left on the prompt line, never run.
    ui.press("q")
    tree = ctl.wait_tree(lambda tree: "idleMs" in tree["tree"])
    assert isinstance(tree["tree"]["idleMs"], int) and tree["tree"]["idleMs"] >= 0
    ctl.wait_text(sid, "q")
