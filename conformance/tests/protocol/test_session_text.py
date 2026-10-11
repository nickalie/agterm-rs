"""`session.type` / `session.text` round-trips and `session.search`. A pane starts in a directory the suite does
not own, so every line typed here only prints a marker."""

import itertools
import threading
import time

import pytest
from agterm_conformance.match import assert_absent, assert_error, assert_ok, find_session

UNKNOWN = "zzz"
POSIX_SHELL = pytest.mark.requires("posix-shell", reason="a marker round-trip needs a known shell in the pane")
_markers = itertools.count(1)


def _realized(ctl):
    sid = assert_ok(ctl.request("session.new"))["id"]
    ctl.wait_mounted(sid, "left")
    return sid


def _split(ctl):
    sid = _realized(ctl)
    assert_ok(ctl.request("session.split", sid, mode="on"))
    ctl.wait_mounted(sid, "right")
    return sid


def _scratch_on(ctl, sid):
    assert_ok(ctl.request("session.scratch", sid, mode="on"))
    ctl.wait_mounted(sid, "scratch")


def _echo(ctl, sid, **args):
    """Type a line printing a fresh marker and wait for its output and the prompt after it, so a later read
    sees a settled screen; the echoed command line carries the marker split in two and never matches."""
    marker = f"MARK{next(_markers)}"
    result = assert_ok(ctl.request("session.type", sid, text=f"printf '%s%s\\n' {marker[:4]} {marker[4:]}\n", **args))
    pane = {key: value for key, value in args.items() if key in ("pane", "paneID")}
    ctl.wait_text(sid, rf"(?m)^{marker}\n[^\n]*\S", **pane)
    return marker, result


def _text(ctl, sid, **args):
    return assert_ok(ctl.request("session.text", sid, **args))


def _searched(ctl, sid, needle, count, timeout=10.0):
    """`session.search` reports the count libghostty has so far; repeat the search until it settles."""
    deadline = time.monotonic() + timeout
    while True:
        result = assert_ok(ctl.request("session.search", sid, text=needle))
        if result.get("count") == count:
            return result
        assert time.monotonic() < deadline, f"search for {needle!r} did not count {count}: {result}"
        threading.Event().wait(0.05)


def _pane_id(ctl, sid, kind):
    (surface,) = [surface for surface in find_session(ctl.tree(), sid)["surfaces"] if surface["kind"] == kind]
    return surface["paneID"]


@POSIX_SHELL
@pytest.mark.covers("session.type", "session.text", "field:result.text", "field:result.pane")
def test_type_then_read_the_marker_back(ctl):
    sid = _realized(ctl)
    marker, typed = _echo(ctl, sid)
    assert typed == {"id": sid, "pane": "left"}
    # Only `text` and `pane` are compared: the Linux fork also echoes the session id, upstream does not.
    result = _text(ctl, sid)
    assert result["pane"] == "left"
    assert f"\n{marker}\n" in result["text"]


@POSIX_SHELL
@pytest.mark.covers("session.text", "field:result.text")
def test_text_lines_returns_the_last_content_lines(ctl):
    sid = _realized(ctl)
    first, _ = _echo(ctl, sid)
    second, _ = _echo(ctl, sid)
    # The prompt after the last marker is the last content line; blank grid rows below it are trimmed.
    lines = _text(ctl, sid, lines=2)["text"].split("\n")
    assert lines[0] == second, lines
    assert len(lines) == 2, lines
    assert first in _text(ctl, sid, lines=4)["text"]


@POSIX_SHELL
@pytest.mark.covers("session.text")
def test_text_all_includes_the_scrollback(ctl):
    sid = _realized(ctl)
    markers = [_echo(ctl, sid)[0] for _ in range(3)]
    text = _text(ctl, sid, all=True)["text"]
    assert [text.index(f"\n{marker}\n") for marker in markers] == sorted(text.index(f"\n{m}\n") for m in markers)


@POSIX_SHELL
@pytest.mark.covers("session.type", "session.text", "field:result.pane")
def test_type_and_text_address_a_pane(ctl):
    sid = _split(ctl)
    right, typed = _echo(ctl, sid, pane="right")
    assert typed["pane"] == "right"
    left, typed = _echo(ctl, sid, pane="primary")
    assert typed["pane"] == "left"
    assert right not in _text(ctl, sid, pane="left")["text"]
    assert left not in _text(ctl, sid, pane="split")["text"]
    assert _text(ctl, sid, pane="bottom")["pane"] == "right"


@POSIX_SHELL
@pytest.mark.covers("session.type", "session.text", "field:result.pane")
def test_type_and_text_address_a_pane_by_token(ctl):
    sid = _split(ctl)
    token = _pane_id(ctl, sid, "right")
    marker, typed = _echo(ctl, sid, paneID=token)
    assert typed["pane"] == "right"
    result = _text(ctl, sid, paneID=token)
    assert result["pane"] == "right"
    assert marker in result["text"]
    # A live token wins over the role named beside it.
    assert _text(ctl, sid, paneID=token, pane="left")["pane"] == "right"


@POSIX_SHELL
@pytest.mark.covers("session.text", "session.scratch", "field:result.pane")
def test_text_defaults_to_the_pane_on_screen(ctl):
    sid = _split(ctl)
    assert_ok(ctl.request("session.focus", sid, pane="left"))
    assert _text(ctl, sid)["pane"] == "left"
    assert_ok(ctl.request("session.focus", sid, pane="right"))
    assert _text(ctl, sid)["pane"] == "right"
    _scratch_on(ctl, sid)
    marker, typed = _echo(ctl, sid, pane="scratch")
    assert typed["pane"] == "scratch"
    result = _text(ctl, sid)
    assert result["pane"] == "scratch"
    assert marker in result["text"]
    # A hidden scratch stays addressable.
    assert_ok(ctl.request("session.scratch", sid, mode="off"))
    assert _text(ctl, sid)["pane"] != "scratch"
    assert marker in _text(ctl, sid, pane="scratch")["text"]


@pytest.mark.covers("session.type")
@pytest.mark.parametrize(
    ("args", "error"),
    [
        pytest.param({}, "session.type requires text", id="no-text"),
        pytest.param({"text": "x", "pane": "middle"}, "invalid pane: middle", id="pane"),
        pytest.param({"text": "x", "paneID": UNKNOWN}, f"unknown pane id: {UNKNOWN}", id="pane-id"),
    ],
)
def test_type_rejects(ctl, args, error):
    sid = _realized(ctl)
    assert_error(ctl.request("session.type", sid, **args), error)


@pytest.mark.covers("session.type", "session.text")
@pytest.mark.parametrize("cmd", ["session.type", "session.text"])
@pytest.mark.parametrize(
    ("pane", "upstream"), [("right", "session has no split pane"), ("scratch", "session has no scratch terminal")]
)
def test_a_missing_pane_is_refused(ctl, cmd, pane, upstream):
    # The references disagree on the text: upstream names the missing pane, the Linux fork answers
    # `session not realized` for both.
    sid = _realized(ctl)
    args = {"text": "x"} if cmd == "session.type" else {}
    response = ctl.request(cmd, sid, pane=pane, **args)
    assert_error(response)
    assert response.error in (upstream, "session not realized"), response.raw
    node = find_session(ctl.tree(), sid)
    assert_absent(node, "hasSplit")
    assert node["scratch"] is False


@pytest.mark.covers("session.type")
def test_type_text_carrying_a_nul(adapter, ctl):
    sid = _realized(ctl)
    response = ctl.request("session.type", sid, text="a\u0000b")
    if adapter.has("type-rejects-nul"):
        assert_error(response, "text must not contain a NUL byte")
    else:
        # The Linux fork has no NUL guard and answers ok for the run libghostty types up to the NUL.
        assert_ok(response)
    assert_ok(ctl.request("session.close", sid))


@pytest.mark.covers("session.text")
@pytest.mark.parametrize(
    ("args", "error"),
    [
        pytest.param({"lines": 0}, "--lines must be greater than 0", id="zero-lines"),
        pytest.param({"lines": -1}, "--lines must be greater than 0", id="negative-lines"),
        pytest.param({"all": True, "lines": 3}, "use either --all or --lines, not both", id="all-and-lines"),
        pytest.param({"pane": "middle"}, "invalid pane: middle", id="pane"),
        pytest.param({"paneID": UNKNOWN}, f"unknown pane id: {UNKNOWN}", id="pane-id"),
    ],
)
def test_text_rejects(ctl, args, error):
    sid = _realized(ctl)
    assert_error(ctl.request("session.text", sid, **args), error)


@POSIX_SHELL
@pytest.mark.covers("session.search", "field:result.count")
def test_search_counts_the_matches(ctl):
    sid = _realized(ctl)
    assert_ok(ctl.request("session.select", sid))
    marker, _ = _echo(ctl, sid)
    _echo(ctl, sid)
    # The command line carries the marker split in two, so only the printed line matches it. The display
    # text differs by reference (upstream `1 of 1`, the Linux fork `0/1`), so only its presence is compared.
    result = _searched(ctl, sid, marker, 1)
    assert isinstance(result["text"], str) and result["text"]
    _searched(ctl, sid, "NOT-ON-SCREEN", 0)
    for to in ("next", "prev"):
        assert_ok(ctl.request("session.search", sid, to=to))
    assert assert_ok(ctl.request("session.search", sid, to="close"))["id"] == sid


@pytest.mark.covers("session.type", "session.text", "session.search")
@pytest.mark.parametrize(
    ("cmd", "args"),
    [("session.type", {"text": "x"}), ("session.text", {}), ("session.search", {"text": "x"})],
)
def test_unknown_session_target(ctl, cmd, args):
    assert_error(ctl.request(cmd, UNKNOWN, **args), f"no such session: {UNKNOWN}")
