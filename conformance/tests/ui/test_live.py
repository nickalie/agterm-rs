"""A Live pane another zmx client leads: its own window covers it until it takes the lead back."""

import pytest
from agterm_conformance.drivers.base import ElementNotFound
from agterm_conformance.match import assert_ok, find_session
from agterm_conformance.plugin import Ctl
from agterm_conformance.zmx_client import leading_client

LIVE = pytest.mark.requires(
    "live-sessions", reason="needs panes in zmx daemons, which the references create only for a zsh login shell"
)


@pytest.fixture(scope="module")
def live(adapter):
    instance = adapter.launch(settings={"restoreMode": "live"})
    try:
        ctl = Ctl(instance)
        assert assert_ok(ctl.request("restore.mode"))["restore"]["active"] == "live"
        yield ctl
    finally:
        adapter.stop(instance)


def _lead(tree, sid):
    return next(s for s in find_session(tree, sid)["surfaces"] if s["kind"] == "left").get("lead")


@LIVE
@pytest.mark.covers("ui:pane-lead-cover-{pane}")
def test_a_pane_another_client_leads_is_covered(live, ui_for):
    ui = ui_for(live)
    cover = ui.ui_ids.expand("pane-lead-cover-{pane}", pane="left")
    sid = assert_ok(live.request("session.new"))["id"]
    live.wait_mounted(sid, "left")
    live.wait_tree(lambda tree: find_session(tree, sid).get("backedByZmx") is True and _lead(tree, sid) == "leader")
    ui.absent(cover)
    endpoint = assert_ok(live.request("zmx.tree"))["remote"]["endpoint"]
    rows = assert_ok(live.request("zmx.list"))["zmx"]["entries"]
    (daemon,) = [row["daemon"] for row in rows if row.get("sessionID") == sid and row["pane"] == "left"]
    with leading_client(live.instance, endpoint, daemon) as viewer:
        live.wait_tree(lambda tree: _lead(tree, sid) == "follower", timeout=15)
        try:
            ui.find(cover)
        except ElementNotFound as error:
            pytest.fail(f"{error}\nleading client output: {viewer.output()[-300:]!r}")
        assert_ok(live.request("session.lead", sid))
        live.wait_tree(lambda tree: _lead(tree, sid) == "leader")
        ui.absent(cover)
    assert_ok(live.request("session.close", sid))
