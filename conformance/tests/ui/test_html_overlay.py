"""An HTML page in a session's overlay slot: its web view, the strip that names it, the navigation buttons and the
failure cover. Buttons that leave the app (browser, Finder, copy to the clipboard) are found, never pressed."""

import socket
from http.server import BaseHTTPRequestHandler

import pytest
from agterm_conformance.loopback import serve
from agterm_conformance.match import assert_ok, find_session
from agterm_conformance.wait import wait_until


def _new(ctl):
    sid = assert_ok(ctl.request("session.new"))["id"]
    ctl.wait_mounted(sid, "left")
    return sid


def _state(ctl, sid, state, title=None):
    def reached(tree):
        (page,) = find_session(tree, sid)["htmlOverlays"]
        assert page["state"] == state and (title is None or page.get("title") == title), page
        return True

    ctl.wait_tree(reached)


def _closed(ctl, sid):
    ctl.wait_tree(lambda tree: "htmlOverlays" not in find_session(tree, sid))


class _Page(BaseHTTPRequestHandler):
    def do_GET(self):
        body = b"<!doctype html><title>Served UI Page</title><body><p>served</p></body>"
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


@pytest.mark.covers(
    "ui:htmlOverlay.page",
    "ui:htmlOverlay.identity",
    "ui:htmlOverlay.title",
    "ui:htmlOverlay.back",
    "ui:htmlOverlay.forward",
    "ui:htmlOverlay.reload",
    "ui:htmlOverlay.browser",
    "ui:htmlOverlay.finder",
    "ui:htmlOverlay.close",
)
def test_file_page_strip_names_the_page_and_closes_it(ctl, ui):
    sid = _new(ctl)
    page = ctl.instance.root / "ui-page.html"
    page.write_text("<!doctype html><title>UI Page</title><body><p>page</p></body>")
    assert_ok(ctl.request("session.overlay.open", sid, html=str(page), navigation=True))
    try:
        _state(ctl, sid, "loaded", "UI Page")
        ui.find("htmlOverlay.page")
        assert "ui-page.html" in ui.text("htmlOverlay.identity")
        wait_until(lambda: ui.text("htmlOverlay.title") == "UI Page")
        # A first page has no history either way.
        ui.find("htmlOverlay.back", role="button", enabled=False)
        ui.find("htmlOverlay.forward", role="button", enabled=False)
        ui.find("htmlOverlay.browser", role="button", enabled=True)
        ui.find("htmlOverlay.finder", role="button", enabled=True)
        ui.absent("htmlOverlay.copy")
        ui.click(ui.find("htmlOverlay.reload", role="button", enabled=True))
        _state(ctl, sid, "loaded", "UI Page")
        ui.click(ui.find("htmlOverlay.close", role="button"))
        _closed(ctl, sid)
        ui.absent("htmlOverlay.page")
    finally:
        ctl.request("session.overlay.close", sid)
        ctl.request("session.close", sid)


@pytest.mark.covers("ui:htmlOverlay.copy")
def test_url_page_offers_to_copy_its_link(ctl, ui):
    sid = _new(ctl)
    with serve(_Page) as server:
        url = f"http://127.0.0.1:{server.server_port}/page.html"
        assert_ok(ctl.request("session.overlay.open", sid, url=url, navigation=True))
        try:
            _state(ctl, sid, "loaded", "Served UI Page")
            ui.find("htmlOverlay.copy", role="button", enabled=True)
            ui.absent("htmlOverlay.finder")
        finally:
            ctl.request("session.overlay.close", sid)
            ctl.request("session.close", sid)


def _closed_port():
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


@pytest.mark.covers("ui:htmlOverlay.error")
def test_a_page_that_fails_to_load_is_covered(ctl, ui):
    sid = _new(ctl)
    assert_ok(ctl.request("session.overlay.open", sid, url=f"http://127.0.0.1:{_closed_port()}/gone.html"))
    try:
        _state(ctl, sid, "failed")
        ui.find("htmlOverlay.error")
    finally:
        ctl.request("session.overlay.close", sid)
        ctl.request("session.close", sid)
