"""`config.reload` of the agterm-scoped `<state>/config/ghostty.conf`, and the browser commands: `browser.links`
with its `linkOpenMode` read-back, and `browser.clear` against the saved store persistent URL pages share.

Config and browser-store tests each take a pristine instance: a reload re-applies the config to every mounted
surface (on the Linux fork that path stops answering once about seven sessions are mounted), and the saved
store must start out never created. Every page is served by this process on 127.0.0.1."""

from http.server import BaseHTTPRequestHandler

import pytest
from agterm_conformance.loopback import serve
from agterm_conformance.match import assert_absent, assert_error, assert_ok, find_session, session_ids

UNKNOWN = "zzz"
COOKIE = "agt=kept"


# -- config


def _font_size(ctl, sid, size):
    def reads(tree):
        assert find_session(tree, sid).get("fontSize") == size
        return True

    reads.__qualname__ = f"fontSize of {sid} reading {size}"
    ctl.wait_tree(reads)


def _reload(ctl, adapter, diagnostics):
    result = assert_ok(ctl.request("config.reload"))
    if adapter.has("config-reload-counts"):
        assert result == {"count": diagnostics}
    else:
        # The Linux fork reloads without counting libghostty's diagnostics.
        assert_absent(result, "count")


@pytest.mark.covers("config.reload", "field:result.count", "field:tree.session.fontSize")
def test_reload_applies_the_edited_config_to_open_and_new_sessions(app, ctl, adapter):
    (first,) = session_ids(ctl.tree())
    ctl.wait_mounted(first, "left")
    config = app.state_dir / "config" / "ghostty.conf"
    original = config.read_text()

    config.write_text(original + "font-size = 17\n")
    _reload(ctl, adapter, 0)
    _font_size(ctl, first, 17)
    second = assert_ok(ctl.request("session.new"))["id"]
    ctl.wait_mounted(second, "left")
    _font_size(ctl, second, 17)

    config.write_text(original + "font-size = 15\nno-such-key = 1\n")
    _reload(ctl, adapter, 1)
    _font_size(ctl, first, 15)

    config.write_text(original)
    _reload(ctl, adapter, 0)


# -- browser.links


@pytest.fixture
def link_mode(ctl):
    """The mode the instance started with, put back afterwards."""
    mode = ctl.tree()["tree"]["linkOpenMode"]
    yield mode
    ctl.request("browser.links", mode=mode)


def _set_links(ctl, mode):
    assert assert_ok(ctl.request("browser.links", mode=mode)) == {"text": mode}
    assert ctl.tree()["tree"]["linkOpenMode"] == mode


@pytest.mark.covers("browser.links", "field:result.text", "field:tree.linkOpenMode")
def test_links_sets_and_reads_the_link_open_mode(ctl, link_mode):
    assert link_mode in ("browser", "overlay")
    assert assert_ok(ctl.request("browser.links")) == {"text": link_mode}
    _set_links(ctl, "overlay")
    _set_links(ctl, "overlay")
    assert assert_ok(ctl.request("browser.links")) == {"text": "overlay"}
    _set_links(ctl, "browser")
    assert assert_ok(ctl.request("browser.links")) == {"text": "browser"}


@pytest.mark.covers("browser.links")
@pytest.mark.parametrize("mode", ["bogus", "Overlay", ""])
def test_links_refuses_an_unknown_mode(ctl, link_mode, mode):
    assert_error(ctl.request("browser.links", mode=mode), f"invalid link mode: {mode}")
    assert ctl.tree()["tree"]["linkOpenMode"] == link_mode


@pytest.mark.covers("browser.links", "browser.clear")
@pytest.mark.parametrize("cmd", ["browser.links", "browser.clear"])
@pytest.mark.parametrize(("target", "args"), [("active", {}), (UNKNOWN, {}), (None, {"window": UNKNOWN})])
def test_browser_commands_take_no_target_or_window(ctl, link_mode, cmd, target, args):
    assert_error(ctl.request(cmd, target, **args), f"{cmd} takes no target or --window")
    assert ctl.tree()["tree"]["linkOpenMode"] == link_mode


# -- browser.clear


class _Jar(BaseHTTPRequestHandler):
    """Sets a cookie on every response and records the `Cookie` header each request carried."""

    def do_GET(self):
        self.server.cookies.append(self.headers.get("Cookie"))
        body = b"<!doctype html><title>Jar</title><body><p>jar</p></body>"
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Set-Cookie", f"{COOKIE}; Max-Age=3600; Path=/")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


@pytest.fixture
def jar():
    """A page on 127.0.0.1 that sets a cookie, and the list of cookies its requests carried."""
    with serve(_Jar) as server:
        server.cookies = []
        yield f"http://127.0.0.1:{server.server_port}/jar.html", server.cookies


def _open(ctl, sid, url, cookies, **args):
    """Open `url` on `sid` and wait for it to load; return the `Cookie` header it was requested with."""
    before = len(cookies)
    assert_ok(ctl.request("session.overlay.open", sid, url=url, **args))

    def loaded(tree):
        (page,) = find_session(tree, sid)["htmlOverlays"]
        assert page["state"] == "loaded", page
        return True

    ctl.wait_tree(loaded)
    assert len(cookies) == before + 1, cookies
    return cookies[-1]


def _visit(ctl, sid, url, cookies, **args):
    """`_open`, then close the page again."""
    cookie = _open(ctl, sid, url, cookies, **args)
    assert_ok(ctl.request("session.overlay.close", sid))
    return cookie


@pytest.mark.covers("browser.clear")
def test_clear_without_a_saved_store_creates_none(app, ctl):
    assert assert_ok(ctl.request("browser.clear")) == {}
    assert not (app.state_dir / "browser-profile").exists()


@pytest.mark.covers("browser.clear", "session.overlay.open", "field:tree.session.htmlOverlays")
def test_clear_empties_the_store_persistent_pages_share(app, ctl, jar):
    url, cookies = jar
    (sid,) = session_ids(ctl.tree())
    ctl.wait_mounted(sid, "left")
    profile = app.state_dir / "browser-profile"

    assert _visit(ctl, sid, url, cookies, persistent=True) is None
    identity = profile.read_text()
    assert _visit(ctl, sid, url, cookies, persistent=True) == COOKIE
    assert _visit(ctl, sid, url, cookies) is None

    _open(ctl, sid, url, cookies, persistent=True)
    assert_error(ctl.request("browser.clear"), "browser.clear: 1 persistent page still open")
    assert_ok(ctl.request("session.overlay.close", sid))

    assert assert_ok(ctl.request("browser.clear")) == {}
    assert profile.read_text() == identity
    assert _visit(ctl, sid, url, cookies, persistent=True) is None
