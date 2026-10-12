import time
from collections.abc import Iterator

import pytest
from agterm_conformance import cli_format

SID = "0771818B-B246-46C8-9F9E-7279CA4DE6A4"
WID = "22FCC0D1-40EC-49AC-8CB8-D91BDD12B80B"


@pytest.mark.parametrize(
    ("result", "echo_id", "expected"),
    [
        (None, False, "ok"),
        ({"id": SID}, False, "ok"),
        ({"id": SID}, True, SID),
        ({"id": SID, "text": "saved, but not run"}, True, "saved, but not run"),
        ({"text": "picked", "count": 3, "exitCode": 1}, False, "picked"),
        ({"exitCode": 5, "affected": 2}, False, "exit 5"),
        ({"affected": 1}, False, "1 session"),
        ({"affected": 0}, False, "0 sessions"),
        ({"count": 0}, False, "ok"),
        ({"count": 2}, False, "2 diagnostic(s)"),
        ({"cursor": {"column": 8}, "id": "surface:x:left"}, True, "8"),
        ({"id": WID, "width": 900, "height": 640}, True, "900 640"),
        ({"ratio": 0.3}, False, "0.300"),
        ({"sidebarWidth": 300}, False, "300.0"),
        ({"sidebarWidth": 271.34}, False, "271.34"),
        ({"app": {"version": "0.36.0", "commit": "8a5f58d4"}}, False, "0.36.0 (8a5f58d4)"),
        ({"app": {"version": "0.36.0", "commit": ""}}, False, "0.36.0"),
        ({"app": {"version": "0.36.0"}}, False, "0.36.0"),
    ],
)
def test_human_takes_the_first_field_the_reference_formats(result, echo_id, expected):
    assert cli_format.human(result, echo_id) == expected


def _session(**fields):
    node = {"id": SID, "name": "one", "active": False, "split": False, "overlay": False, "scratch": False}
    node["cwd"] = "/home/user"
    node.update(fields)
    return node


def test_tree_tags_each_session_state():
    top = {
        "workspaces": [
            {
                "id": WID,
                "name": "work",
                "active": True,
                "sessions": [
                    _session(active=True, split=True, hasSplit=True, splitCwd="/srv", title="vim"),
                    _session(hasSplit=True, splitCwd="/home/user", realized=False, scratch=True),
                    _session(overlay=True, liveAttribution="supervisor", presenters={"mirrors": 2, "presenter": True}),
                    _session(presentation={"state": "connected", "mode": "presenter", "error": "slow"}),
                ],
            },
            {"id": "W2", "name": "empty", "active": False, "sessions": []},
        ]
    }
    assert cli_format.tree(top).splitlines() == [
        f"* work  [{WID}]",
        f"  * one (split)  [{SID}]  /home/user  split cwd: /srv  title: vim",
        f"    one (split hidden) (not realized) (scratch)  [{SID}]  /home/user",
        f"    one (overlay)  [{SID}]  /home/user  live attribution: supervisor  presented remotely  mirrored by: 2",
        f"    one  [{SID}]  /home/user  presentation: connected, presenter (slow)",
        "  empty  [W2]",
    ]


def test_windows_tag_open_and_active():
    nodes = [
        {"id": WID, "name": "window 1", "open": True, "active": True},
        {"id": "W2", "name": "parked", "open": False, "active": False},
    ]
    assert cli_format.windows(nodes) == f"{WID}  window 1 [open] [active]\nW2  parked"


def test_themes_mark_the_current_theme_or_both_synced_slots():
    assert cli_format.themes(["A", "B"], "B") == "  default ghostty\n  A\n* B"
    assert cli_format.themes(["A", "B"], None) == "* default ghostty\n  A\n  B"
    assert cli_format.themes(["A", "B"], None, sync=True, light="A", dark=None) == (
        "syncing with macOS appearance — light: A, dark: default ghostty\n  default ghostty\n* A\n  B"
    )


def test_keymap_lists_actions_commands_diagnostics_and_menu():
    listing = {
        "path": "/s/config/keymap.conf",
        "actions": [
            {"action": "new_window", "chord": "cmd+opt+n"},
            {
                "action": "previous_attention",
                "chord": "cmd+[",
                "alternates": ["f1"],
                "overridden": True,
                "repeats": True,
            },
            {"action": "rename", "alternates": []},
        ],
        "commands": [
            {"name": "plain", "errorHud": False, "errorPosition": "center", "repeats": False},
            {
                "name": "k",
                "shortcut": "f2",
                "errorHud": True,
                "errorPosition": "top",
                "errorPane": "right",
                "repeats": True,
            },
        ],
        "diagnostics": [{"line": 0, "message": "whole file"}, {"line": 3, "message": "unknown verb"}],
        "menu": [
            {"chord": "cmd+n", "menu": "File", "title": "New"},
            {"chord": "cmd+w", "menu": "File", "title": "Close", "enabled": False},
        ],
    }
    assert cli_format.keymap(listing).splitlines() == [
        "keymap: /s/config/keymap.conf",
        "",
        "actions:",
        "    new_window          cmd+opt+n",
        "  * previous_attention  cmd+[|f1  --repeat",
        "    rename              -",
        "",
        "commands:",
        "    plain  (palette only)",
        "    k  f2  --repeat  --error-hud --error-position top --error-pane right",
        "",
        "diagnostics:",
        "    whole file",
        "    line 3: unknown verb",
        "",
        "menu:",
        "    cmd+n  File ▸ New",
        "    cmd+w  File ▸ Close  (disabled)",
    ]


def test_hooks_list_rows_and_diagnostics():
    assert cli_format.hooks({"path": "/h.conf", "hooks": [], "diagnostics": []}) == "hooks: /h.conf\n  (no hooks)"
    listing = {
        "path": "/h.conf",
        "hooks": [
            {
                "line": 2,
                "kind": "status",
                "command": "x",
                "runningPid": 42,
                "elapsedSeconds": 3.9,
                "pending": 1,
                "dropped": 2,
                "lastFailure": "exit 1",
                "retired": True,
            },
            {"line": 4, "kind": "notify", "command": "y", "pending": 0, "dropped": 0},
        ],
        "diagnostics": [{"line": 5, "message": "unknown verb 'off'"}],
    }
    assert cli_format.hooks(listing).splitlines() == [
        "hooks: /h.conf",
        (
            "  line 2: on status x  running pid 42 for 3s  pending 1  dropped 2  last failure: exit 1"
            "  (retired, removed from the file)"
        ),
        "  line 4: on notify y",
        "",
        "diagnostics:",
        "    line 5: unknown verb 'off'",
    ]


RESTORE = {"configured": "none", "requestedAtLaunch": "none", "active": "none", "restartRequired": False}


def test_restore_status_lines():
    assert cli_format.restore_status(RESTORE) == (
        "configured: none (next launch)\nthis launch: requested none, active none"
    )
    changed = {**RESTORE, "configured": "live", "restartRequired": True, "unavailableReason": "shell is fish"}
    assert cli_format.restore_status(changed).splitlines()[2:] == [
        "restart agterm to apply the configured mode",
        "live unavailable: shell is fish",
    ]


def test_zmx_rows_carry_owner_and_state():
    entry = {
        "daemon": "agterm-5e1d",
        "state": "claimed",
        "windowState": "open",
        "observation": "absent",
        "sessionID": SID,
        "windowName": "second",
        "workspaceName": "workspace 1",
        "sessionName": "nick",
        "pane": "left",
        "windowID": WID,
    }
    orphan = {"daemon": "agterm-0000", "state": "orphan", "observation": "running", "clients": 1, "outdated": True}
    inventory = {"restore": RESTORE, "inventoryComplete": False, "entries": [entry, orphan]}
    assert cli_format.zmx(inventory).splitlines()[2:] == [
        "inventory incomplete: some pane is unaccounted for, so nothing can be pruned",
        "",
        "agterm-5e1d  claimed [open window]  absent  -  0771818B second / workspace 1 / nick (left) win 22FCC0D1",
        "agterm-0000  orphan  running outdated  1 client  -",
    ]
    assert cli_format.zmx({"restore": RESTORE, "inventoryComplete": True, "entries": []}).endswith("\nno daemons")


def test_remote_tree_rows():
    assert cli_format.remote_tree({"sessions": []}) == "no attachable sessions"
    assert cli_format.remote_tree({"sessions": [], "host": "box"}) == "no attachable sessions on box"
    session = {
        "id": SID,
        "name": "one",
        "windowName": "w",
        "workspaceName": "ws",
        "cwd": "/srv",
        "context": "deploys",
        "splitAxis": "vertical",
        "panes": [{"foreground": ["/usr/bin/vim", "x"]}, {}],
    }
    assert (
        cli_format.remote_tree({"sessions": [session]}) == f"  w/ws/one (split vertical)  [{SID}]  /srv  deploys  vim"
    )


@pytest.fixture
def utc() -> Iterator[None]:
    with pytest.MonkeyPatch.context() as patch:
        patch.setenv("TZ", "UTC")
        time.tzset()
        yield
    time.tzset()


@pytest.mark.usefixtures("utc")
@pytest.mark.parametrize(
    ("event", "expected"),
    [
        (
            {
                "kind": "status",
                "payload": {
                    "name": "one",
                    "status": "active",
                    "previous": "idle",
                    "pane": "left",
                    "blink": True,
                    "color": "#ff8800",
                    "shape": "star",
                },
            },
            "01:02:03 status one active previous=idle pane=left blink color=#ff8800 shape=star",
        ),
        ({"kind": "status", "payload": {"name": "one"}}, "01:02:03 status one idle"),
        ({"kind": "notify", "payload": {"name": "one", "body": "hi"}}, "01:02:03 notify one one: hi"),
        ({"kind": "notify", "payload": {"name": "one", "title": "T"}}, "01:02:03 notify one T: "),
        ({"kind": "session.selected", "payload": {"name": "one"}}, "01:02:03 session.selected one previous=-"),
        ({"kind": "pane.split", "payload": {"name": "one", "status": "on"}}, "01:02:03 pane.split one on"),
        ({"kind": "tree.changed", "window": WID, "payload": {}}, f"01:02:03 tree.changed {WID}"),
        ({"kind": "session.closed", "payload": {"name": ""}, "window": WID}, "01:02:03 session.closed "),
        ({"kind": "remote.opened", "payload": {}}, "01:02:03 remote.opened - host="),
    ],
)
def test_event_lines_carry_every_argument(event, expected):
    assert cli_format.event({"ts": 3723.9, **event}) == expected


def test_normalize_replaces_ids_and_paths():
    text = f"{SID}  /tmp/agt-x/home/a  /tmp/agt-x/b"
    paths = {"/tmp/agt-x": "<root>", "/tmp/agt-x/home": "<home>"}
    assert cli_format.normalize(text, paths) == "<id>  <home>/a  <root>/b"
