"""The Settings window, tab by tab: every control's id and role, the state seeded settings put it in, and a toggle
that live-applies. One instance for the module, relaunched once so the seed can name directories in its own root."""

import json

import pytest
from agterm_conformance.drivers.base import ElementNotFound
from agterm_conformance.match import assert_ok, session_ids
from agterm_conformance.plugin import Ctl
from agterm_conformance.wait import wait_until

STATUSES = ("active", "blocked", "completed")
INTERFACE_ELEMENTS = (
    "sidebarToggle", "workspaceName", "sessionName", "windowName", "remoteHost", "sessionContext", "recentSessions",
    "scratch", "split", "dashboard", "quickTerminal", "customCommands", "newWorkspace", "newSession", "flaggedView",
    "focusFilter", "workspaceAddSession",
)  # fmt: skip


@pytest.fixture(scope="module")
def seeded(adapter):
    instance = adapter.launch()
    relaunching = False
    try:
        config = instance.state_dir / "config"
        settings = {
            # Shows the custom-directory row and the Use Default button.
            "newSessionDirectory": "custom",
            "newSessionCustomDirectory": str(instance.root),
            "configDirectory": str(config),
            # Shows the second theme picker.
            "followSystemAppearance": True,
            "theme": "Alabaster",
            "darkTheme": "Dracula",
            "notificationsEnabled": False,
            "attentionButtonEnabled": True,
        }
        (instance.state_dir / "settings.json").write_text(json.dumps(settings))
        relaunching = True
        instance = adapter.relaunch(instance)
        relaunching = False
        ctl = Ctl(instance)
        (sid,) = session_ids(ctl.tree())
        ctl.wait_mounted(sid, "left")
        yield ctl
    finally:
        # A failed relaunch has stopped what it started and keeps the root for its log.
        if not relaunching:
            adapter.stop(instance)


def _open(ui, tab):
    try:
        found = ui.tab(tab, timeout=1)
    except ElementNotFound:
        ui.press("cmd+,")
        found = ui.tab(tab)
    ui.click(found)


def _check(ui, expected):
    """`expected` maps an id to its role, or to None where the role is the platform's own (a stepper, a color well)."""
    for ui_id, role in expected.items():
        ui.find(ui_id, **({} if role is None else {"role": role}))


@pytest.mark.covers(
    "ui:settings-scroll-speed",
    "ui:settings-right-click-paste",
    "ui:settings-link-open-mode",
    "ui:settings-workspace-row-click-expands",
    "ui:settings-new-session-directory",
    "ui:settings-new-session-custom-dir",
    "ui:settings-new-session-choose",
    "ui:settings-new-session-placement",
    "ui:settings-restore-mode",
    "ui:settings-restore-restart-hint",
    "ui:settings-confirm-close-session",
    "ui:settings-close-grace-undo",
    "ui:settings-flagged-view-layout",
    "ui:settings-inherit-global-ghostty",
)
def test_general_tab(seeded, ui_for):
    ui = ui_for(seeded)
    _open(ui, "General")
    _check(
        ui,
        {
            "settings-scroll-speed": "slider",
            "settings-right-click-paste": "checkbox",
            "settings-link-open-mode": "popup",
            "settings-workspace-row-click-expands": "checkbox",
            "settings-new-session-directory": "popup",
            "settings-new-session-choose": "button",
            "settings-new-session-placement": "popup",
            "settings-restore-mode": "popup",
            "settings-restore-restart-hint": "text",
            "settings-confirm-close-session": "checkbox",
            "settings-close-grace-undo": "checkbox",
            "settings-flagged-view-layout": "popup",
            "settings-inherit-global-ghostty": "checkbox",
        },
    )
    assert ui.text(ui.find("settings-new-session-custom-dir", role="text")) == str(seeded.instance.root)


@pytest.mark.covers(
    "ui:settings-font-family",
    "ui:settings-font-size",
    "ui:settings-cursor-style",
    "ui:settings-cursor-blink",
    "ui:settings-theme",
    "ui:settings-follow-appearance",
    "ui:settings-theme-dark",
    "ui:settings-toolbar-mode",
    "ui:settings-bg-opacity",
    "ui:settings-bg-blur",
    "ui:settings-sidebar-shift",
    "ui:settings-sidebar-font-size",
    "ui:settings-interface-font-size",
    "ui:settings-inactive-pane-mute",
)
def test_appearance_tab(seeded, ui_for):
    ui = ui_for(seeded)
    _open(ui, "Appearance")
    _check(
        ui,
        {
            "settings-font-family": "popup",
            "settings-font-size": None,
            "settings-cursor-style": "popup",
            "settings-cursor-blink": "popup",
            "settings-theme": "popup",
            "settings-theme-dark": "popup",
            "settings-toolbar-mode": "popup",
            "settings-bg-opacity": "slider",
            "settings-sidebar-shift": "slider",
            "settings-sidebar-font-size": None,
            "settings-interface-font-size": None,
            "settings-inactive-pane-mute": "slider",
        },
    )
    ui.find("settings-follow-appearance", role="checkbox", value=1)
    # Blur needs opacity below 100%, which the seed leaves at its default.
    ui.find("settings-bg-blur", role="slider", enabled=False)


@pytest.mark.covers(
    "ui:settings-interface-{element}", "ui:settings-auto-hide-inactive-sidebars", "ui:settings-quick-terminal-size"
)
def test_interface_tab_toggles_title_bar_buttons(seeded, ui_for):
    ui = ui_for(seeded)
    _open(ui, "Interface")
    for element in INTERFACE_ELEMENTS:
        ui.find(ui.ui_ids.expand("settings-interface-{element}", element=element), role="checkbox")
    ui.find("settings-auto-hide-inactive-sidebars", role="checkbox", value=0)
    ui.find("settings-quick-terminal-size", role="popup")
    split = ui.ui_ids.expand("settings-interface-{element}", element="split")
    ui.find("split-toggle")
    ui.click(ui.find(split, value=1))
    try:
        ui.absent("split-toggle")
    finally:
        # The module's instance outlives a failed attempt, and a rerun needs the button back.
        ui.click(ui.find(split, value=0))
    ui.find("split-toggle")


@pytest.mark.covers(
    "ui:settings-notifications",
    "ui:settings-notification-badges",
    "ui:settings-dock-bounce",
    "ui:settings-notification-sound",
    "ui:settings-attention-button",
)
def test_notifications_tab_reads_the_seeded_toggles(seeded, ui_for):
    ui = ui_for(seeded)
    _open(ui, "Notifications")
    ui.find("settings-notifications", role="checkbox", value=0)
    ui.find("settings-notification-badges", role="checkbox", value=1)
    ui.find("settings-attention-button", role="checkbox", value=1)
    _check(ui, {"settings-dock-bounce": "popup", "settings-notification-sound": "popup"})


@pytest.mark.covers(
    "ui:settings-status-{status}",
    "ui:settings-status-shape-{status}",
    "ui:settings-status-blocked-sound",
    "ui:settings-status-clear",
    "ui:settings-auto-follow",
    "ui:settings-auto-follow-stay-active",
    "ui:settings-status-reset",
)
def test_agent_status_tab(seeded, ui_for):
    ui = ui_for(seeded)
    _open(ui, "Agent Status")
    for status in STATUSES:
        ui.find(ui.ui_ids.expand("settings-status-{status}", status=status))
        ui.find(ui.ui_ids.expand("settings-status-shape-{status}", status=status), role="popup")
    _check(
        ui,
        {
            "settings-status-blocked-sound": "popup",
            "settings-status-clear": "popup",
            "settings-auto-follow": "popup",
            "settings-status-reset": "button",
        },
    )
    # Only applies while auto-follow is on, which it is not by default.
    ui.find("settings-auto-follow-stay-active", role="checkbox", enabled=False)


@pytest.mark.covers(
    "ui:settings-keymap-directory",
    "ui:settings-keymap-choose",
    "ui:settings-keymap-default",
    "ui:settings-keymap-diagnostics",
    "ui:settings-keymap-reload",
    "ui:settings-hooks-diagnostics",
    "ui:settings-hooks-reload",
)
def test_key_mapping_tab_reports_a_keymap_error_after_reload(seeded, ui_for):
    ui = ui_for(seeded)
    _open(ui, "Key Mapping")
    assert ui.text(ui.find("settings-keymap-directory", role="text")) == str(seeded.instance.state_dir / "config")
    _check(
        ui,
        {
            "settings-keymap-choose": "button",
            "settings-keymap-default": "button",
            "settings-hooks-diagnostics": None,
            "settings-hooks-reload": "button",
        },
    )
    ui.find("settings-keymap-diagnostics", value="No issues.")
    keymap = seeded.instance.state_dir / "config" / "keymap.conf"
    keymap.write_text("not a keymap line\n")
    try:
        ui.click(ui.find("settings-keymap-reload", role="button"))
        wait_until(lambda: "line 1" in ui.text("settings-keymap-diagnostics"))
    finally:
        keymap.write_text("")
        assert_ok(seeded.request("keymap.reload"))
