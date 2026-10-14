"""The `zmx` group, `session.lead`/`session.restart` and the Live-sessions read-back.

The module's shared instance runs without Live, so its daemons are only claimed, never observed: the inventory,
prune, the hostless `zmx.tree` and every refusal are checked there. The Live tests share one instance launched with
`restoreMode: live` seeded; a pane in it runs the account's zsh inside a zmx daemon, so a line typed there only
prints a marker. Each test ends the sessions it opens, which ends their daemons; the harness's stop then finds no
process left under the instance's `ZMX_DIR`.

`zmx.reset` is refused here without `--force` only: a confirmed reset quits the app and relaunches it outside the
harness (see the catalog exemption)."""

import itertools
import os

import pytest
from agterm_conformance.match import assert_absent, assert_error, assert_ok, assert_subset, find_session, session_ids
from agterm_conformance.plugin import Ctl
from agterm_conformance.wait import wait_frame, wait_until
from agterm_conformance.zmx_client import leading_client

UNKNOWN = "zzz"
LIVE = pytest.mark.requires(
    "live-sessions", reason="needs panes in zmx daemons, which the references create only for a zsh login shell"
)
ATTRIBUTIONS = ("supervisor", "app", "orphaned", "unknown")
PRESENTATION_VERSION = 1
_markers = itertools.count(1)


def _rows(response, sid):
    """This session's `zmx.list` rows by pane."""
    rows = assert_ok(response)["zmx"]["entries"]
    return {row["pane"]: row for row in rows if row.get("sessionID") == sid}


def _entries(ctl, sid):
    return _rows(ctl.request("zmx.list"), sid)


def _initial_session(ctl):
    (sid,) = session_ids(ctl.tree())
    ctl.wait_mounted(sid, "left")
    return sid


# -- without Live


@pytest.mark.covers("zmx.list", "field:result.zmx")
def test_list_joins_each_pane_claim(ctl):
    sid = _initial_session(ctl)
    inventory = assert_ok(ctl.request("zmx.list"))["zmx"]
    assert_subset(
        inventory,
        {
            "inventoryComplete": True,
            "restore": {"configured": "none", "requestedAtLaunch": "none", "active": "none"},
            # The socket namespace is the state dir's hash, the one the harness checks for leftovers under.
            "endpoint": {"socketDirectory": ctl.instance.zmx_dir},
        },
    )
    (row,) = [row for row in inventory["entries"] if row.get("sessionID") == sid]
    # Outside Live a pane still claims its daemon name; no daemon runs, so none is observed.
    assert_subset(row, {"pane": "left", "state": "claimed", "observation": "absent", "windowState": "open"})
    assert row["daemon"].startswith("agterm-")
    assert_absent(row, "clients", "leaderPID")


@pytest.mark.covers("zmx.prune", "field:result.text", "field:result.affected")
def test_prune_finds_no_orphans(ctl):
    assert assert_ok(ctl.request("zmx.prune")) == {"text": "no orphan daemons", "affected": 0}


@pytest.mark.covers("zmx.tree", "field:result.remote")
def test_tree_without_a_host_lists_no_attachable_session_outside_live(ctl):
    remote = assert_ok(ctl.request("zmx.tree"))["remote"]
    assert remote["sessions"] == []
    assert remote["presentation"] >= PRESENTATION_VERSION
    assert remote["endpoint"]["socketDirectory"] == ctl.instance.zmx_dir


@pytest.mark.covers("zmx.kill")
@pytest.mark.parametrize(
    ("target", "args", "error"),
    [
        pytest.param(None, {"pane": "left", "force": True}, "zmx.kill requires an explicit --target", id="no-target"),
        pytest.param("SID", {"force": True}, "zmx.kill requires --pane left|right", id="no-pane"),
        pytest.param("SID", {"pane": "scratch", "force": True}, "zmx.kill requires --pane left|right", id="scratch"),
        pytest.param("SID", {"pane": "left"}, "zmx.kill requires --force", id="no-force"),
        pytest.param(
            "active",
            {"pane": "left", "force": True},
            "zmx.kill needs a session id; 'active' is not accepted",
            id="active",
        ),
        pytest.param(
            UNKNOWN, {"pane": "left", "force": True}, f"no left pane daemon for session {UNKNOWN}", id="unknown"
        ),
        pytest.param("SID", {"pane": "right", "force": True}, "no right pane daemon for session SID", id="no-split"),
    ],
)
def test_kill_rejects(ctl, target, args, error):
    sid = _initial_session(ctl)
    target = sid if target == "SID" else target
    assert_error(ctl.request("zmx.kill", target, **args), error.replace("SID", sid))


@pytest.mark.covers("zmx.kill")
def test_kill_refuses_a_daemon_that_is_not_running(ctl):
    sid = _initial_session(ctl)
    daemon = _entries(ctl, sid)["left"]["daemon"]
    assert_error(ctl.request("zmx.kill", sid, pane="left", force=True), f"{daemon} is not running")
    assert find_session(ctl.tree(), sid)["realized"] is True


@pytest.mark.covers("zmx.screen")
@pytest.mark.parametrize(
    ("args", "error"),
    [
        pytest.param({}, "zmx.screen requires a daemon name", id="no-name"),
        pytest.param({"name": "two words"}, "zmx.screen requires a daemon name", id="not-plain"),
        pytest.param({"name": "agterm-x", "all": True, "lines": 3}, "use either --all or --lines, not both", id="both"),
        pytest.param({"name": "agterm-x", "lines": 0}, "--lines must be greater than 0", id="zero-lines"),
        pytest.param({"name": "agterm-x"}, "could not read the zmx screen of agterm-x", id="no-daemon"),
    ],
)
def test_screen_rejects(ctl, args, error):
    assert_error(ctl.request("zmx.screen", **args), error)


@pytest.mark.covers("zmx.reset")
def test_reset_requires_force(ctl):
    assert_error(ctl.request("zmx.reset"), "zmx.reset requires --force")
    assert_absent(ctl.tree()["tree"], "liveReset")


@pytest.mark.covers("session.lead")
@pytest.mark.parametrize(
    ("args", "error"),
    [
        pytest.param({}, "pane has no lead to take", id="not-live"),
        pytest.param({"pane": "right"}, "session has no split pane", id="no-split"),
        pytest.param({"pane": "scratch"}, "the scratch terminal has no lead", id="scratch"),
    ],
)
def test_lead_rejects(ctl, args, error):
    assert_error(ctl.request("session.lead", _initial_session(ctl), **args), error)


@pytest.mark.covers("session.restart")
@pytest.mark.parametrize(
    ("args", "error"),
    [
        pytest.param({}, "session.restart requires --pane-id or --pane", id="no-pane"),
        pytest.param(
            {"pane": "left"},
            "session.restart needs Live sessions mode; this pane has no live shell to replace",
            id="not-live",
        ),
        pytest.param({"pane": "right"}, "session has no split pane", id="no-split"),
        pytest.param({"pane": "scratch"}, "session.restart does not address the scratch pane", id="scratch"),
        pytest.param({"paneID": UNKNOWN}, f"unknown pane id: {UNKNOWN}", id="unknown-pane-id"),
        pytest.param(
            {"pane": "left", "command": "  "},
            "session.restart takes a non-empty command, or none to replay",
            id="blank-command",
        ),
    ],
)
def test_restart_rejects(ctl, args, error):
    assert_error(ctl.request("session.restart", _initial_session(ctl), **args), error)


@pytest.mark.covers("zmx.present")
def test_present_refuses_without_a_session(ctl):
    # Upstream answers `zmx.present requires a session`; the Linux fork resolves the active session instead and
    # refuses it here as not live-backed.
    with ctl.stream("zmx.present") as stream:
        assert_error(stream.reply)


@pytest.mark.covers("zmx.present", "session.overlay.job.run")
@pytest.mark.parametrize(
    ("cmd", "target", "error"),
    [
        pytest.param(
            "zmx.present", "SID", "session is not live-backed, so nothing can be attached to it", id="present-not-live"
        ),
        pytest.param("session.overlay.job.run", None, "session.overlay.job.run requires a job id", id="job-no-id"),
        pytest.param("session.overlay.job.run", UNKNOWN, "invalid job id", id="job-invalid"),
        pytest.param(
            "session.overlay.job.run", "00000000-0000-0000-0000-000000000000", "job not claimable", id="job-unknown"
        ),
    ],
)
def test_stream_commands_refuse_before_streaming(ctl, cmd, target, error):
    target = _initial_session(ctl) if target == "SID" else target
    with ctl.stream(cmd, target) as stream:
        assert_error(stream.reply, error)


# -- Live


@pytest.fixture(scope="module")
def live(adapter):
    instance = adapter.launch(settings={"restoreMode": "live"})
    try:
        ctl = Ctl(instance)
        # An adapter that advertises Live must get it: a fallback to plain shells fails here rather than skipping.
        assert assert_ok(ctl.request("restore.mode"))["restore"]["active"] == "live"
        yield ctl
    finally:
        adapter.stop(instance)


def _live_session(live, split=False):
    sid = assert_ok(live.request("session.new"))["id"]
    live.wait_mounted(sid, "left")
    if split:
        assert_ok(live.request("session.split", sid, mode="on"))
        live.wait_mounted(sid, "right")

    def backed(tree):
        node = find_session(tree, sid)
        assert node.get("backedByZmx") is True, f"backedByZmx {node.get('backedByZmx')!r}"
        return True

    live.wait_tree(backed)
    panes = ("left", "right") if split else ("left",)

    def running(response):
        rows = _rows(response, sid)
        assert all(rows.get(pane, {}).get("observation") == "running" for pane in panes), rows
        return True

    live.wait_response("zmx.list", None, running)
    return sid


def _gone(live, daemon):
    live.wait_response(
        "zmx.list", None, lambda response: all(row["daemon"] != daemon for row in assert_ok(response)["zmx"]["entries"])
    )


@LIVE
@pytest.mark.covers(
    "zmx.list",
    "field:result.zmx",
    "field:tree.session.backedByZmx",
    "field:tree.session.liveAttribution",
    "field:tree.session.splitLiveAttribution",
)
def test_live_panes_run_in_claimed_daemons(live):
    sid = _live_session(live, split=True)
    node = find_session(live.tree(), sid)
    assert [surface["backedByZmx"] for surface in node["surfaces"]] == [True, True]
    assert node["liveAttribution"] in ATTRIBUTIONS and node["splitLiveAttribution"] in ATTRIBUTIONS
    # One reply with both clients attached: upstream's count drops to 0 for a moment while a pane's client attaches.
    attached = live.wait_response(
        "zmx.list", None, lambda r: all(_rows(r, sid).get(p, {}).get("clients", 0) >= 1 for p in ("left", "right"))
    )
    rows = _rows(attached, sid)
    for pane in ("left", "right"):
        assert_subset(rows[pane], {"state": "claimed", "observation": "running"})
        assert rows[pane]["leaderPID"] > 0
    assert rows["left"]["daemon"] != rows["right"]["daemon"]
    assert_ok(live.request("session.close", sid))
    for pane in ("left", "right"):
        _gone(live, rows[pane]["daemon"])


@LIVE
@pytest.mark.covers("zmx.screen", "field:result.text")
def test_screen_reads_the_daemon_grid(live):
    sid = _live_session(live)
    marker = f"ZMARK{next(_markers)}"
    # The command line carries the marker split in two, so only the printed line matches it.
    assert_ok(live.request("session.type", sid, text=f"printf '%s%s\\n' {marker[:5]} {marker[5:]}\n"))
    live.wait_text(sid, rf"(?m)^{marker}$")
    daemon = _entries(live, sid)["left"]["daemon"]
    screen = assert_ok(live.request("zmx.screen", name=daemon))
    assert f"\n{marker}\n" in f"\n{screen['text']}\n"
    assert_ok(live.request("session.close", sid))
    _gone(live, daemon)


@LIVE
@pytest.mark.covers("session.lead")
def test_lead_on_a_pane_that_already_leads(live):
    sid = _live_session(live)
    _wait_leads(live, sid)
    assert assert_ok(live.request("session.lead", sid)) == {"id": sid}
    assert_ok(live.request("session.close", sid))


@LIVE
@pytest.mark.covers("session.restart", "field:result.restart", "field:result.pane")
def test_restart_replaces_the_pane_shell(live):
    sid = _live_session(live)
    before = _entries(live, sid)["left"]
    pane_id = next(s for s in find_session(live.tree(), sid)["surfaces"] if s["kind"] == "left")["paneID"]
    result = assert_ok(live.request("session.restart", sid, pane="left", command="cat"))
    assert_subset(result, {"id": sid, "pane": "left", "restart": {"paneID": pane_id, "oldPid": before["leaderPID"]}})
    new_pid = result["restart"]["newPid"]
    assert new_pid != before["leaderPID"]
    assert result["text"].startswith(f"restarted left pane: shell {before['leaderPID']} -> {new_pid}")
    # The daemon keeps its name: the pane identity it is derived from survives the restart.
    live.wait_response("zmx.list", None, lambda response: _rows(response, sid)["left"]["leaderPID"] == new_pid)
    # Closed only once the pane runs the new program: a close racing the restart has left upstream's daemon
    # orphaned rather than ended.
    live.wait_tree(lambda tree: find_session(tree, sid).get("foreground") == ["cat"])
    assert_ok(live.request("session.close", sid))
    _gone(live, before["daemon"])


def _pane_id(live, sid):
    return next(s for s in find_session(live.tree(), sid)["surfaces"] if s["kind"] == "left")["paneID"]


@LIVE
@pytest.mark.covers("session.restart", "field:result.restart")
def test_restart_without_a_command_replays_the_foreground_program(live, tmp_path):
    sid = _live_session(live)
    pane_id = _pane_id(live, sid)
    daemon = _entries(live, sid)["left"]["daemon"]
    marker = tmp_path / "replay runs.txt"
    work = tmp_path / "work dir"
    work.mkdir()
    # A `sh -c` job of the pane's shell, not its root: the trailing `:` keeps sh from exec'ing the sleep.
    script = f"pwd -P >> '{marker}'; sleep 600; :"
    assert_ok(live.request("session.restart", sid, paneID=pane_id, command=f"cd '{work}' && sh -c \"{script}\""))
    ran = f"{os.path.realpath(work)}\n"
    wait_until(lambda: marker.exists() and marker.read_text() == ran, timeout=20)

    receipt = assert_ok(live.request("session.restart", sid, paneID=pane_id))["restart"]
    assert receipt["replayedArgv"] == ["sh", "-c", script]
    assert receipt["paneID"] == pane_id
    # The replay runs the program again where it was running, its quoting intact.
    wait_until(lambda: marker.read_text() == ran + ran, timeout=20)
    assert _pane_id(live, sid) == pane_id
    assert_ok(live.request("session.close", sid))
    _gone(live, daemon)


@LIVE
@pytest.mark.covers("session.restart")
def test_restart_replay_refuses_a_shell_at_its_prompt(live):
    sid = _live_session(live)
    marker = f"ZMARK{next(_markers)}"
    # Once the marker prints, the pane's own shell is back at its prompt.
    assert_ok(live.request("session.type", sid, text=f"printf '%s%s\\n' {marker[:5]} {marker[5:]}\n"))
    live.wait_text(sid, rf"(?m)^{marker}$")
    before = _entries(live, sid)["left"]
    response = live.request("session.restart", sid, paneID=_pane_id(live, sid))
    assert_error(response)
    assert response.error.endswith("holds the pane; pass --command to name the line. nothing was changed"), (
        response.error
    )
    assert _entries(live, sid)["left"]["leaderPID"] == before["leaderPID"]
    assert_ok(live.request("session.close", sid))
    _gone(live, before["daemon"])


@LIVE
@pytest.mark.covers("zmx.kill", "field:result.pane")
def test_kill_ends_the_daemon_and_closes_its_pane(live):
    sid = _live_session(live, split=True)
    daemon = _entries(live, sid)["right"]["daemon"]
    result = assert_ok(live.request("zmx.kill", sid, pane="right", force=True))
    assert result == {"id": sid, "text": f"killed {daemon}", "pane": "right"}
    live.wait_tree(lambda tree: "hasSplit" not in find_session(tree, sid))
    _gone(live, daemon)
    assert_ok(live.request("session.close", sid))


# -- presentation streams


def _hello(mode):
    kinds = ["status", "hud", "notify", "context", "layout"]
    return {
        "kind": "hello",
        "gen": 0,
        "rev": 0,
        "hello": {"version": PRESENTATION_VERSION, "kinds": kinds, "mode": mode},
    }


def _subscribed(live, sid, mode):
    stream = live.stream("zmx.present", sid)
    assert stream.reply.ok, stream.reply.raw
    assert stream.reply.result == {"id": sid}
    stream.send(_hello(mode))
    hello = wait_frame(stream.read, "hello", stream.send)
    assert hello["hello"]["mode"] == mode and hello["hello"]["version"] == PRESENTATION_VERSION
    wait_frame(stream.read, "snapshot", stream.send)
    return stream, hello["gen"]


def _wait_leads(live, sid):
    """Until the left pane's zmx has reported its role: before that `session.lead` has no lead to take."""

    def leads(tree):
        left = next(s for s in find_session(tree, sid)["surfaces"] if s["kind"] == "left")
        assert left.get("lead") == "leader", f"lead {left.get('lead')!r}"
        return True

    live.wait_tree(leads)


def _follows(tree, sid, viewer):
    left = next(s for s in find_session(tree, sid)["surfaces"] if s["kind"] == "left")
    assert left.get("lead") == "follower", f"lead {left.get('lead')!r}; viewer's zmx: {viewer.output()[-500:]!r}"
    return True


def _presenters(live, sid, expected):
    def counted(tree):
        assert find_session(tree, sid).get("presenters") == expected
        return True

    counted.__qualname__ = f"presenters of {sid} reading {expected}"
    live.wait_tree(counted)


@LIVE
@pytest.mark.covers("zmx.present", "field:tree.session.presenters", streams=("zmx.present",))
def test_present_mirrors_a_live_session(live):
    sid = _live_session(live)
    stream, _ = _subscribed(live, sid, "mirror")
    with stream:
        _presenters(live, sid, {"mirrors": 1})
        assert_ok(live.request("session.status", sid, status="active"))
        assert wait_frame(stream.read, "status", stream.send)["status"]["status"] == "active"
    _presenters(live, sid, None)
    assert_ok(live.request("session.close", sid))


@LIVE
@pytest.mark.covers(
    "session.overlay.job.run",
    "field:tree.session.presenters",
    "field:tree.session.remoteOverlays",
    streams=("session.overlay.job.run",),
)
def test_job_run_serves_an_overlay_a_presenter_shows(live):
    sid = _live_session(live)
    endpoint = assert_ok(live.request("zmx.tree"))["remote"]["endpoint"]
    # Both references hand an overlay to the presenter only while the pane's own client follows, and open it
    # locally otherwise; a viewer's zmx client claiming the lead makes it follow.
    with leading_client(live.instance, endpoint, _entries(live, sid)["left"]["daemon"]) as viewer:
        live.wait_tree(lambda tree: _follows(tree, sid, viewer))
        presenter, gen = _subscribed(live, sid, "presenter")
        with presenter:
            presenter.send({"kind": "presenter.acquire", "gen": gen, "rev": 1})
            wait_frame(presenter.read, "presenter.granted", presenter.send)
            _presenters(live, sid, {"mirrors": 0, "presenter": True})
            # Handed to the presenter to show, and run by a helper that claims the job.
            assert assert_ok(live.request("session.overlay.open", sid, command="true"))["id"] == sid
            job = wait_frame(presenter.read, "overlay.request", presenter.send)["overlay"]["job"]
            live.wait_tree(lambda tree: find_session(tree, sid).get("remoteOverlays") == [{}])
            assert find_session(live.tree(), sid)["overlay"] is False
            with live.stream("session.overlay.job.run", job) as helper:
                assert helper.reply.ok and helper.reply.result == {"id": job}
                context = helper.read()
                assert context["kind"] == "context" and context["context"]["command"] == "true"
                helper.send({"kind": "started"})
                helper.send({"kind": "exited", "code": 0})
                live.wait_tree(lambda tree: "remoteOverlays" not in find_session(tree, sid))
            # A job is claimable once.
            with live.stream("session.overlay.job.run", job) as again:
                assert_error(again.reply, "job not claimable")
    _presenters(live, sid, None)
    assert_ok(live.request("session.close", sid))
