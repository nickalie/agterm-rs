"""`notify`: the unseen badge on the session (`unseen`) and the `notify` event. Each post also reaches the desktop
over the real session bus, so the module posts as few as it can and selects the notified session afterwards,
which withdraws its banners."""

import pytest
from agterm_conformance.match import assert_absent, assert_error, assert_ok, find_session

UNKNOWN = "zzz"


def _node(ctl, sid):
    return find_session(ctl.tree(), sid)


@pytest.fixture
def background(ctl):
    """A session that is not selected, so a notification counts as unseen."""
    sid = assert_ok(ctl.request("session.new", noSelect=True))["id"]
    yield sid
    ctl.request("session.select", sid)


@pytest.mark.covers("notify", "event:notify", "field:tree.session.unseen", "field:result.id")
def test_notify_raises_the_badge_and_emits_the_event(ctl, background):
    tree = ctl.tree()
    node = find_session(tree, background)
    (workspace,) = [ws["id"] for ws in tree["tree"]["workspaces"] if node in ws["sessions"]]
    (window,) = [w["id"] for w in assert_ok(ctl.request("window.list"))["windows"]]
    cursor = ctl.events_baseline()

    # Upstream adds a note in `result.text` when banners are off in settings; only the id is shared.
    assert assert_ok(ctl.request("notify", background, title="Build", body="tests passed"))["id"] == background
    assert _node(ctl, background)["unseen"] == 1
    first = ctl.wait_event(cursor, "notify", {"session": background})
    where = {"session": background, "workspace": workspace, "window": window}
    assert {key: first[key] for key in where} == where
    assert first["payload"] == {"name": node["name"], "title": "Build", "body": "tests passed"}

    # An omitted title falls back to the session's name.
    assert_ok(ctl.request("notify", background, body="second"))
    assert _node(ctl, background)["unseen"] == 2
    second = ctl.wait_event(cursor.past(first), "notify", {"session": background})
    assert second["payload"] == {"name": node["name"], "title": node["name"], "body": "second"}

    assert_ok(ctl.request("session.seen", background))
    assert_absent(_node(ctl, background), "unseen")


@pytest.mark.covers("notify")
@pytest.mark.parametrize(
    ("target", "args", "error"),
    [
        ("background", {}, "notify requires a body"),
        ("background", {"body": "", "title": "t"}, "notify requires a body"),
        (UNKNOWN, {"body": "b"}, f"no such session: {UNKNOWN}"),
    ],
)
def test_notify_refusals_post_nothing(ctl, background, target, args, error):
    cursor = ctl.events_baseline()
    assert_error(ctl.request("notify", background if target == "background" else target, **args), error)
    assert_absent(_node(ctl, background), "unseen")
    response = ctl.request("events.read", run=cursor.run, after=str(cursor.after), kinds=["notify"])
    assert assert_ok(response)["events"]["items"] == []
