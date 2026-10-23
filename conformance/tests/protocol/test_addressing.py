"""Target resolution and the request envelope."""

import pytest
from agterm_conformance.match import (
    active_session_id,
    active_workspace_id,
    assert_error,
    assert_ok,
    window_ids,
    workspace_ids,
)

RESOLUTION = "behavior:protocol.target-resolution"
# A target no UUID can start with.
UNKNOWN = "zzz"


def _resolved_id(response):
    return assert_ok(response)["id"]


@pytest.mark.covers(RESOLUTION, "field:result.id")
@pytest.mark.parametrize(
    "spelling",
    [
        pytest.param(lambda sid: sid, id="exact"),
        pytest.param(str.lower, id="lowercase"),
        pytest.param(lambda sid: sid[:8].lower(), id="unique-prefix"),
    ],
)
def test_session_target_by_id(ctl, spelling):
    sid = active_session_id(ctl.tree())
    assert _resolved_id(ctl.request("session.select", spelling(sid))) == sid


@pytest.mark.covers(RESOLUTION, "field:result.id")
def test_active_and_omitted_targets_name_the_selected_session(ctl):
    sid = active_session_id(ctl.tree())
    assert _resolved_id(ctl.request("session.select", "active")) == sid
    assert _resolved_id(ctl.request("session.select")) == sid


@pytest.mark.covers(RESOLUTION, "field:result.id")
def test_active_and_omitted_targets_name_the_current_workspace(ctl):
    wid = active_workspace_id(ctl.tree())
    assert _resolved_id(ctl.request("workspace.select", "active")) == wid
    assert _resolved_id(ctl.request("workspace.select")) == wid


@pytest.mark.covers(RESOLUTION)
@pytest.mark.parametrize(
    ("cmd", "target", "error"),
    [
        ("session.select", UNKNOWN, f"no such session: {UNKNOWN}"),
        ("session.select", "", "no such session: "),
        ("workspace.select", UNKNOWN, f"no such workspace: {UNKNOWN}"),
    ],
)
def test_unresolved_target(ctl, cmd, target, error):
    assert_error(ctl.request(cmd, target), error)


@pytest.mark.covers(RESOLUTION)
def test_ambiguous_prefix_lists_the_matches(ctl):
    # Sixteen hex digits, so seventeen workspaces always hold two sharing a first character.
    for _ in range(17):
        by_first: dict[str, list[str]] = {}
        for wid in workspace_ids(ctl.tree()):
            by_first.setdefault(wid[0].lower(), []).append(wid)
        shared = [(first, ids) for first, ids in by_first.items() if len(ids) > 1]
        if shared:
            break
        assert_ok(ctl.request("workspace.new"))
    else:
        pytest.fail(f"no two workspace ids share a first character: {workspace_ids(ctl.tree())}")
    prefix, hits = shared[0]
    listed = ", ".join(wid[:8] for wid in hits)
    assert_error(ctl.request("workspace.select", prefix), f"ambiguous workspace prefix '{prefix}' → {listed}")


@pytest.mark.covers("tree", RESOLUTION)
def test_tree_window_argument(ctl):
    (wid,) = window_ids(ctl.request("window.list"))
    expected = workspace_ids(ctl.tree())
    for window in (wid, wid.lower(), wid[:8].lower(), "active"):
        assert workspace_ids(ctl.request("tree", window=window)) == expected, window
    assert_error(ctl.request("tree", window=UNKNOWN), f"no such window: {UNKNOWN}")


@pytest.mark.covers("behavior:protocol.malformed-request")
@pytest.mark.parametrize(
    "line",
    [
        pytest.param(b'{"cmd":"no.such.command"}', id="unknown-command"),
        pytest.param(b"not json", id="not-json"),
        pytest.param(b'{"target":"active"}', id="no-cmd"),
        pytest.param(b"[]", id="not-an-object"),
        # The wrong type is the only defect: a coercing decoder would answer ok:true.
        pytest.param(b'{"cmd":"version","target":5}', id="target-type"),
        pytest.param(b'{"cmd":"events.read","args":{"limit":"5"}}', id="arg-type"),
    ],
)
def test_undecodable_request_is_refused(ctl, line):
    # Upstream answers `invalid request: <DecodingError context>`; only the prefix is shared, the context is the
    # decoder's own.
    response = ctl.instance.client().send_line(line)
    assert response.ok is False, response.raw
    assert isinstance(response.error, str) and response.error.startswith("invalid request: "), response.raw
    assert_ok(ctl.request("version"))
