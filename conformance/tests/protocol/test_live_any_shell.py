"""Live sessions in the host's own login shell: a Rust-only extension (capability `live-any-shell`).

Upstream wraps a pane in zmx only for a zsh login shell; the Rust build also wraps bash and fish, starting each with
Ghostty's shell integration. The reference skips these tests as unsupported; the Rust CI runs them
under zsh, bash and fish login shells with `--must-pass live-any-shell`.

The host picks the shell (the password database's), so every line typed here is valid in zsh, bash and fish alike."""

import uuid

import pytest
from agterm_conformance.match import assert_ok, find_session

LIVE_ANY_SHELL = pytest.mark.requires(
    "live-any-shell",
    reason="a Rust-only extension: upstream wraps a pane in zmx only for a zsh login shell",
)


def _marker():
    return f"LA{uuid.uuid4().hex[:8].upper()}"


def _daemon_row(ctl, sid):
    rows = assert_ok(ctl.request(ctl.vocabulary.command("zmx.list")))[ctl.vocabulary.result_key]["entries"]
    (row,) = [row for row in rows if row.get("sessionID") == sid and row["pane"] == "left"]
    return row


@LIVE_ANY_SHELL
@pytest.mark.covers("behavior:restore.live-session-survives-relaunch", "field:tree.session.cwd")
def test_a_live_pane_in_the_login_shell_reports_its_cwd_and_survives_the_relaunch(launch):
    ctl = launch(settings={"restoreMode": "live"})
    sid = assert_ok(ctl.request("session.new"))["id"]
    ctl.wait_mounted(sid, "left")
    ctl.wait_tree(lambda tree: find_session(tree, sid).get(ctl.vocabulary.backed_key) is True)
    # The shell integration reports the directory through OSC 7, which the pane's cwd follows.
    directory = ctl.instance.root / "live-cwd"
    directory.mkdir()
    assert_ok(ctl.request("session.type", sid, text=f"cd '{directory}'\n"))
    ctl.wait_tree(lambda tree: find_session(tree, sid).get("cwd") == str(directory))
    marker = _marker()
    assert_ok(ctl.request("session.type", sid, text=f"printf '%s%s\\n' {marker[:4]} {marker[4:]}\n"))
    ctl.wait_text(sid, rf"(?m)^{marker}$")
    before = _daemon_row(ctl, sid)

    ctl = launch.relaunch(ctl)
    ctl.wait_mounted(sid, "left")
    ctl.wait_tree(lambda tree: find_session(tree, sid).get(ctl.vocabulary.backed_key) is True)

    def same_daemon(response):
        rows = assert_ok(response)[ctl.vocabulary.result_key]["entries"]
        (row,) = [row for row in rows if row.get("sessionID") == sid]
        assert (row["daemon"], row["observation"]) == (before["daemon"], "running"), row
        return row

    ctl.wait_response(ctl.vocabulary.command("zmx.list"), None, same_daemon)
    # The same shell, still in its directory: its leader pid, its screen and its cwd came through the quit.
    assert _daemon_row(ctl, sid)["leaderPID"] == before["leaderPID"]
    ctl.wait_text(sid, rf"(?m)^{marker}$", all=True)
    ctl.wait_tree(lambda tree: find_session(tree, sid).get("cwd") == str(directory))
