"""The title bar's buttons and what they open: split, scratch, quick terminal, sidebar, terminal zoom, the dashboard
grid, search, and the recent-sessions, attention and custom-commands popovers. Each click is read back through the
tree; a test that opens something closes it."""

import pytest
from agterm_conformance.match import active_session_id, assert_ok, find_session, session_ids
from agterm_conformance.wait import WaitTimeout, wait_until


def _new(ctl, **args):
    sid = assert_ok(ctl.request("session.new", **args))["id"]
    ctl.wait_mounted(sid, "left")
    return sid


def _top(ctl):
    return ctl.tree()["tree"]


def _sidebar_visible(ctl):
    (window,) = assert_ok(ctl.request("window.list"))["windows"]
    return window["sidebarVisible"]


@pytest.mark.covers("ui:sidebar-toggle-button")
def test_sidebar_toggle_button_hides_and_shows_the_sidebar(ctl, ui):
    assert _sidebar_visible(ctl) is True
    try:
        ui.click(ui.find("sidebar-toggle-button", role="button"))
        wait_until(lambda: _sidebar_visible(ctl) is False)
        ui.click("sidebar-toggle-button")
        wait_until(lambda: _sidebar_visible(ctl) is True)
    finally:
        ctl.request("sidebar", mode="show")


@pytest.mark.covers("ui:split-toggle")
def test_split_toggle_reads_back_the_split(ctl, ui):
    sid = _new(ctl)
    ui.find("split-toggle", role="button", value="none")
    ui.click("split-toggle")
    ctl.wait_tree(lambda tree: find_session(tree, sid).get("split") is True)
    ctl.wait_mounted(sid, "right")
    ui.find("split-toggle", value="both")
    ui.click("split-toggle")
    tree = ctl.wait_tree(lambda tree: find_session(tree, sid).get("split") is False)
    shown = "right" if find_session(tree, sid)["splitFocused"] else "left"
    ui.find("split-toggle", value=shown)
    assert_ok(ctl.request("session.close", sid))


@pytest.mark.covers("ui:scratch-toggle")
def test_scratch_toggle_shows_and_hides_the_scratch_terminal(ctl, ui):
    sid = _new(ctl)
    ui.click(ui.find("scratch-toggle", role="button", enabled=True))
    ctl.wait_tree(lambda tree: find_session(tree, sid).get("scratch") is True)
    ctl.wait_mounted(sid, "scratch")
    ui.click("scratch-toggle")
    ctl.wait_tree(lambda tree: find_session(tree, sid).get("scratch") is False)
    assert_ok(ctl.request("session.close", sid))


@pytest.mark.covers("ui:quick-terminal-toggle", "ui:quick-terminal")
def test_quick_terminal_toggle_shows_and_hides_the_panel(ctl, ui):
    assert _top(ctl)["quickVisible"] is False
    ui.absent("quick-terminal")
    try:
        ui.click(ui.find("quick-terminal-toggle", role="button", enabled=True))
        ctl.wait_tree(lambda tree: tree["tree"]["quickVisible"] is True)
        ui.find("quick-terminal")
        ui.click("quick-terminal-toggle")
        ctl.wait_tree(lambda tree: tree["tree"]["quickVisible"] is False)
        ui.absent("quick-terminal")
    finally:
        ctl.request("quick", mode="hide")


@pytest.mark.covers("ui:terminal-zoom", "ui:terminal-zoom-exit")
def test_terminal_zoom_exit_button_ends_the_zoom(ctl, ui):
    sid = _new(ctl)
    assert_ok(ctl.request("surface.zoom", "active", mode="show"))
    try:
        assert _top(ctl)["zoomedSurface"] == f"surface:{sid}:left"
        ui.find("terminal-zoom")
        ui.click(ui.find("terminal-zoom-exit", role="button", name="Exit Terminal Zoom"))
        ctl.wait_tree(lambda tree: "zoomedSurface" not in tree["tree"])
        ui.absent("terminal-zoom")
    finally:
        ctl.request("surface.zoom", "active", mode="hide")
        ctl.request("session.close", sid)


@pytest.mark.covers(
    "ui:dashboard-toggle-button", "ui:dashboard", "ui:dashboard-cell", "ui:dashboard-highlighted", "ui:dashboard-exit"
)
def test_dashboard_button_opens_the_grid_and_exit_closes_it(ctl, ui):
    sids = [_new(ctl), _new(ctl)]
    try:
        ui.click(ui.find("dashboard-toggle-button", role="button", enabled=True))
        tree = ctl.wait_tree(lambda tree: tree["tree"].get("dashboardMembers"))
        members = tree["tree"]["dashboardMembers"]
        ui.find("dashboard")
        wait_until(lambda: len(ui.all("dashboard-cell")) == len(members))
        assert len(ui.all("dashboard-highlighted")) == 1
        ui.absent("dashboard-toggle-button")
        ui.click(ui.find("dashboard-exit", role="button", name="Exit Dashboard"))
        ctl.wait_tree(lambda tree: "dashboardMembers" not in tree["tree"])
        ui.absent("dashboard")
    finally:
        ctl.request("dashboard", close=True)
        for sid in sids:
            ctl.request("session.close", sid)


@pytest.mark.covers(
    "ui:dashboard-overlay-cover", "ui:dashboard-overlay-cover-source", "ui:dashboard-overlay-cover-title"
)
def test_dashboard_cell_under_a_page_names_the_page(ctl, ui):
    sid = _new(ctl)
    assert_ok(ctl.request("session.split", sid, mode="on"))
    ctl.wait_mounted(sid, "right")
    page = ctl.instance.root / "cover.html"
    page.write_text("<!doctype html><title>Cover Title</title><body>page</body>")
    try:
        assert_ok(ctl.request("dashboard", targets=[sid]))
        wait_until(lambda: len(ui.all("dashboard-cell")) == 2)
        ui.absent("dashboard-overlay-cover")
        assert_ok(ctl.request("session.overlay.open", sid, html=str(page), pane="right"))
        wait_until(lambda: len(ui.all("dashboard-overlay-cover")) == 1)
        assert ui.text("dashboard-overlay-cover") == "HTML overlay"
        wait_until(lambda: ui.text("dashboard-overlay-cover-source") == "cover.html")
        wait_until(lambda: ui.text("dashboard-overlay-cover-title") == "Cover Title")
        assert_ok(ctl.request("session.overlay.close", sid, pane="right"))
        ui.absent("dashboard-overlay-cover")
    finally:
        ctl.request("dashboard", close=True)
        ctl.request("session.close", sid)


@pytest.mark.covers("ui:recent-sessions-button", "ui:recent-session-row")
def test_recent_sessions_popover_switches_to_the_previous_session(app, ctl, ui):
    (first,) = session_ids(ctl.tree())
    ctl.wait_mounted(first, "left")
    ui.find("recent-sessions-button", role="button", enabled=False)
    second = _new(ctl)
    assert active_session_id(ctl.tree()) == second
    ui.click(ui.find("recent-sessions-button", enabled=True))
    wait_until(lambda: len(ui.all("recent-session-row")) == 1)
    ui.click("recent-session-row")
    ctl.wait_tree(lambda tree: active_session_id(tree) == first)
    ui.absent("recent-session-row")


@pytest.mark.covers("ui:attention-button", "ui:attention-session-row")
def test_attention_button_lists_the_blocked_session(launch, ui_for):
    ctl = launch(settings={"attentionButtonEnabled": True})
    ui = ui_for(ctl)
    (first,) = session_ids(ctl.tree())
    ctl.wait_mounted(first, "left")
    ui.find("attention-button", role="button", enabled=False, value="none")
    blocked = _new(ctl, noSelect=True)
    assert_ok(ctl.request("session.status", blocked, status="blocked"))
    ui.click(ui.find("attention-button", enabled=True, value="blocked"))
    wait_until(lambda: len(ui.all("attention-session-row")) == 1)
    ui.click("attention-session-row")
    ctl.wait_tree(lambda tree: active_session_id(tree) == blocked)
    ui.absent("attention-session-row")


def _custom_commands(ctl, count):
    lines = "".join(f'command "ui-cmd-{n}" true\n' for n in range(1, count + 1))
    (ctl.instance.state_dir / "config" / "keymap.conf").write_text(lines)
    assert_ok(ctl.request("keymap.reload"))


@pytest.mark.covers("ui:custom-commands-button", "ui:custom-command-row", "ui:custom-command-top-row")
def test_custom_commands_popover_lists_the_keymap_commands(launch, ui_for):
    # Hidden by default; past five commands the most-run ones lead the list.
    ctl = launch(settings={"shownInterfaceElements": ["customCommands"]})
    ui = ui_for(ctl)
    (sid,) = session_ids(ctl.tree())
    ctl.wait_mounted(sid, "left")
    ui.find("custom-commands-button", role="button", enabled=False)
    _custom_commands(ctl, 6)
    ui.click(ui.find("custom-commands-button", enabled=True))
    wait_until(lambda: len(ui.all("custom-command-row")) == 6)
    assert ui.all("custom-command-top-row") == []
    ui.click(ui.all("custom-command-row")[-1])
    ui.absent("custom-command-row")
    ui.click("custom-commands-button")
    wait_until(lambda: len(ui.all("custom-command-top-row")) == 1 and len(ui.all("custom-command-row")) == 5)
    ui.click("custom-commands-button")
    ui.absent("custom-command-row")


@pytest.mark.covers("ui:search-field", "ui:search-counter")
def test_find_bar_counts_the_matches(ctl, ui):
    sid = _new(ctl)
    marker = "zqxuisearch"
    assert_ok(ctl.request("session.type", sid, text=f"printf '%s%s\\n%s%s\\n' zqx uisearch zqx uisearch\n"))
    ctl.wait_text(sid, rf"(?m)^{marker}\n{marker}$")
    ui.menu("View", "Find…")
    field = ui.find("search-field", role="text-field")

    def counted():
        # Typed, as the counter fills only once a typed needle searches; retyped, since a needle typed before the
        # field took focus or before the output rendered never counts.
        ui.set_text(field, "")
        ui.press(*marker)
        try:
            return wait_until(lambda: any("2" in ui.text(counter) for counter in ui.all("search-counter")), 3)
        except WaitTimeout:
            return None

    wait_until(counted, 20)
    ui.menu("View", "Find…")
    ui.absent("search-field")
    assert_ok(ctl.request("session.close", sid))
