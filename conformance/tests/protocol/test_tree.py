"""The `tree` and `window.list` projections of a fresh instance. Every test here only reads, so the module's
shared instance stays fresh for all of them."""

import pytest
from agterm_conformance.match import assert_absent, assert_ok, assert_subset, find_session, sessions, workspaces


@pytest.mark.covers(
    "tree",
    "field:result.tree",
    "field:tree.workspaces",
    "field:tree.sidebarVisible",
    "field:tree.sidebarMode",
    "field:tree.sidebarFlaggedLayout",
    "field:tree.linkOpenMode",
    "field:tree.workspaceFilter",
    "field:tree.quickVisible",
)
def test_fresh_tree_top_level(ctl):
    tree = ctl.tree()["tree"]
    assert_subset(
        tree,
        {
            "sidebarVisible": True,
            "sidebarMode": "tree",
            "sidebarFlaggedLayout": "flat",
            "linkOpenMode": "browser",
            "workspaceFilter": False,
            "quickVisible": False,
        },
    )
    assert len(tree["workspaces"]) == 1


@pytest.mark.covers("tree")
def test_fresh_tree_omits_unset_top_level_fields(ctl):
    assert_absent(
        ctl.tree()["tree"],
        "idleMs",
        "autoFollowMs",
        "zoomedSurface",
        "dashboardMembers",
        "dashboardHighlighted",
        "dashboardFontSize",
        "dashboardFontMode",
        "pickPending",
        "askPending",
        "liveReset",
        "indexUnsaved",
    )


@pytest.mark.covers(
    "tree",
    "field:tree.workspace.id",
    "field:tree.workspace.name",
    "field:tree.workspace.active",
    "field:tree.workspace.sessions",
)
def test_fresh_workspace_node(ctl):
    (workspace,) = workspaces(ctl.tree())
    assert_subset(workspace, {"name": "workspace 1", "active": True})
    assert isinstance(workspace["id"], str) and workspace["id"]
    assert len(workspace["sessions"]) == 1
    assert_absent(workspace, "focused", "collapsed")


@pytest.mark.covers(
    "tree",
    "field:tree.session.id",
    "field:tree.session.name",
    "field:tree.session.cwd",
    "field:tree.session.active",
    "field:tree.session.split",
    "field:tree.session.overlay",
    "field:tree.session.scratch",
    "field:tree.session.flagged",
    "field:tree.session.realized",
    "field:tree.session.surfaces",
)
def test_fresh_session_node(ctl):
    (sid,) = [node["id"] for node in sessions(ctl.tree())]
    # The surface is created by the window's first layout, which may trail the socket coming up.
    session = find_session(ctl.wait_tree(lambda tree: find_session(tree, sid)["realized"]), sid)
    assert_subset(
        session,
        {
            "active": True,
            "split": False,
            "overlay": False,
            "scratch": False,
            "flagged": False,
            "realized": True,
            "surfaces": [{"kind": "left", "active": True, "visible": True}],
        },
    )
    for key in ("id", "name", "cwd"):
        assert isinstance(session[key], str) and session[key], f"{key}: {session[key]!r}"
    # `title`, `foreground` and the live-session fields depend on the shell and the restore mode, not on
    # the instance being fresh.
    assert_absent(
        session,
        "hasSplit",
        "splitAxis",
        "splitRatio",
        "splitFocused",
        "splitCwd",
        "splitForeground",
        "splitForegroundShell",
        "splitLiveAttribution",
        "splitCommandWait",
        "splitRestoreCommand",
        "splitFontSize",
        "overlaySizePercent",
        "paneOverlays",
        "hud",
        "ask",
        "commandWait",
        "restoreCommand",
        "status",
        "statusPane",
        "statusBlink",
        "statusColor",
        "statusShape",
        "statusChangedAt",
        "background",
        "paneBackgrounds",
        "unseen",
        "scratchFontSize",
        "context",
        "remoteHost",
        "presentation",
        "presenters",
        "remoteOverlays",
        "htmlOverlays",
    )


@pytest.mark.covers(
    "window.list",
    "field:result.windows",
    "field:window.id",
    "field:window.name",
    "field:window.open",
    "field:window.active",
    "field:window.sidebarVisible",
    "field:window.fullscreen",
    "field:window.zoomed",
    "field:window.minimized",
)
def test_fresh_window_node(ctl):
    (window,) = assert_ok(ctl.request("window.list"))["windows"]
    assert_subset(
        window,
        {
            "name": "window 1",
            "open": True,
            "active": True,
            "sidebarVisible": True,
            "fullscreen": False,
            "zoomed": False,
            "minimized": False,
        },
    )
    assert isinstance(window["id"], str) and window["id"]
    assert_absent(window, "autoFollowMs")
