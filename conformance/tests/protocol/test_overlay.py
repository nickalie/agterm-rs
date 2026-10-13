"""The session overlay slot holding a caller's program or an HTML page, and the pane-scoped overlays.

Every program shown here only prints a marker and then waits in `cat` until its overlay is closed, so nothing runs
outside the instance. Every page is a static file written into the instance's own root, or served from this
process on 127.0.0.1: no page loads anything from the network."""

import itertools
import uuid
from http.server import BaseHTTPRequestHandler

import pytest
from agterm_conformance.loopback import serve
from agterm_conformance.match import ANY, assert_absent, assert_error, assert_ok, assert_subset, find_session

UNKNOWN = "zzz"
HOLD = "exec cat"
_pages = itertools.count(1)


def _new(ctl):
    sid = assert_ok(ctl.request("session.new"))["id"]
    ctl.wait_mounted(sid, "left")
    return sid


def _split(ctl):
    sid = _new(ctl)
    assert_ok(ctl.request("session.split", sid, mode="on"))
    ctl.wait_mounted(sid, "right")
    return sid


def _node(ctl, sid):
    return find_session(ctl.tree(), sid)


def _kinds(node):
    return {surface["kind"] for surface in node["surfaces"]}


def _open(ctl, sid, command=HOLD, **args):
    assert assert_ok(ctl.request("session.overlay.open", sid, command=command, **args))["id"] == sid


def _closed(ctl, sid, **args):
    assert assert_ok(ctl.request("session.overlay.close", sid, **args))["id"] == sid


def _result(ctl, sid, code, **args):
    """The status lands at the surface's teardown, which upstream runs after the slot reads closed."""

    def accept(response):
        return response.ok and response.result == {"id": sid, "exitCode": code}

    accept.__qualname__ = f"exit status {code}"
    ctl.wait_response("session.overlay.result", sid, accept, **args)


def _showing(needle):
    def accept(response):
        # `overlay not realized` is the moment between the open and the overlay's terminal coming up.
        assert response.ok, response.error
        return needle in response.result["text"]

    accept.__qualname__ = f"text containing {needle!r}"
    return accept


def _overlay_text(ctl, sid, needle, **args):
    return ctl.wait_response("session.overlay.text", sid, _showing(needle), **args).result["text"]


def _page(ctl, body="<p>static</p>", directory=None):
    """A static page in the instance's own root; `directory` is a subdirectory to put it in."""
    number = next(_pages)
    folder = ctl.instance.root / directory if directory else ctl.instance.root
    folder.mkdir(exist_ok=True)
    path = folder / f"page-{number}.html"
    path.write_text(f"<!doctype html><title>Page {number}</title><body>{body}</body>")
    return path, f"Page {number}"


def _loaded(ctl, sid, title, index=0):
    def loaded(tree):
        page = find_session(tree, sid)["htmlOverlays"][index]
        assert page["state"] == "loaded" and page.get("title") == title, page
        return True

    return find_session(ctl.wait_tree(loaded), sid)


def _open_page(ctl, sid, **args):
    path, title = _page(ctl)
    result = assert_ok(ctl.request("session.overlay.open", sid, html=str(path), **args))
    assert result["id"] == sid
    return path, title, result["pageID"]


def _outcome(ctl, page_id):
    return assert_ok(ctl.request("session.overlay.result", page=page_id))["pageOutcome"]


# -- a program


@pytest.mark.covers(
    "session.overlay.open",
    "session.overlay.close",
    "session.overlay.result",
    "field:tree.session.overlay",
    "field:tree.session.overlaySizePercent",
    "field:tree.session.surfaces",
)
def test_program_overlay_covers_the_session(ctl):
    sid = _new(ctl)
    _open(ctl, sid)
    node = _node(ctl, sid)
    assert node["overlay"] is True
    assert_absent(node, "overlaySizePercent", "hud", "htmlOverlays", "paneOverlays")
    assert "overlay" in _kinds(node)
    assert_error(ctl.request("session.overlay.result", sid), "overlay still running")

    _closed(ctl, sid)
    node = _node(ctl, sid)
    assert node["overlay"] is False and "overlay" not in _kinds(node)
    assert_error(ctl.request("session.overlay.close", sid), "no overlay")


@pytest.mark.covers("session.overlay.result", "field:result.exitCode", "field:result.id")
def test_overlay_result_reports_the_exit_status(ctl):
    sid = _new(ctl)
    assert_error(ctl.request("session.overlay.result", sid), "no overlay result")
    _open(ctl, sid, "exit 7")
    ctl.wait_tree(lambda tree: find_session(tree, sid)["overlay"] is False)
    _result(ctl, sid, 7)

    # A new overlay replaces the previous status.
    _open(ctl, sid, "exit 3")
    ctl.wait_tree(lambda tree: find_session(tree, sid)["overlay"] is False)
    _result(ctl, sid, 3)


@pytest.mark.covers("session.overlay.text")
def test_overlay_text_reads_the_covering_program(ctl):
    sid = _new(ctl)
    assert_error(ctl.request("session.overlay.text", sid), "no overlay")
    _open(ctl, sid, "echo OVL-TEXT-1; echo OVL-TEXT-2; exec cat")
    text = _overlay_text(ctl, sid, "OVL-TEXT-2")
    assert "OVL-TEXT-1" in text
    assert _overlay_text(ctl, sid, "OVL-TEXT-1", all=True)
    last = assert_ok(ctl.request("session.overlay.text", sid, lines=1))["text"]
    assert "OVL-TEXT-2" in last and "OVL-TEXT-1" not in last
    # `session.text` reads the pane underneath, never the program drawn over it.
    assert "OVL-TEXT" not in assert_ok(ctl.request("session.text", sid))["text"]
    assert_error(ctl.request("session.overlay.text", sid, all=True, lines=2), "use either --all or --lines, not both")
    assert_error(ctl.request("session.overlay.text", sid, lines=0), "--lines must be greater than 0")
    _closed(ctl, sid)


@pytest.mark.covers("session.overlay.open", "session.overlay.resize", "field:tree.session.overlaySizePercent")
def test_overlay_size_and_resize(ctl):
    sid = _new(ctl)
    assert_error(ctl.request("session.overlay.resize", sid, sizePercent=50), "no overlay")
    _open(ctl, sid, sizePercent=40)
    assert _node(ctl, sid)["overlaySizePercent"] == 40
    for args, size in [({"sizePercent": 75}, 75), ({"full": True}, None), ({"sizePercent": 1}, 1)]:
        assert assert_ok(ctl.request("session.overlay.resize", sid, **args))["id"] == sid
        node = _node(ctl, sid)
        assert node["overlay"] is True
        if size is None:
            assert_absent(node, "overlaySizePercent")
        else:
            assert node["overlaySizePercent"] == size
    for args, error in [
        ({"full": True, "sizePercent": 50}, "session.overlay.resize: --full is mutually exclusive with --size-percent"),
        ({}, "session.overlay.resize requires --size-percent or --full"),
        ({"sizePercent": 0}, "session.overlay.resize: --size-percent must be 1...100"),
        ({"sizePercent": 101}, "session.overlay.resize: --size-percent must be 1...100"),
        (
            {"sizePercent": 50, "pane": "left"},
            "session.overlay.resize: --pane is not supported (pane overlays are always full)",
        ),
    ]:
        assert_error(ctl.request("session.overlay.resize", sid, **args), error)
    assert _node(ctl, sid)["overlaySizePercent"] == 1
    _closed(ctl, sid)


@pytest.mark.covers("session.overlay.open", "field:tree.session.overlaySizePercent")
@pytest.mark.parametrize(("requested", "clamped"), [(0, 1), (101, 100)])
def test_overlay_open_size_outside_its_range(ctl, adapter, requested, clamped):
    sid = _new(ctl)
    response = ctl.request("session.overlay.open", sid, command=HOLD, sizePercent=requested)
    if adapter.has("overlay-size-validated"):
        assert_error(response, "session.overlay.open: --size-percent must be 1...100")
        assert _node(ctl, sid)["overlay"] is False
    else:
        # The Linux fork's dispatcher skips the range check and the store clamps the percent instead.
        assert_ok(response)
        assert _node(ctl, sid)["overlaySizePercent"] == clamped
        _closed(ctl, sid)


@pytest.mark.covers("session.overlay.open", "field:tree.session.active")
def test_overlay_opens_in_the_background_unless_it_follows(ctl):
    first, second = _new(ctl), _new(ctl)
    assert_ok(ctl.request("session.select", first))
    _open(ctl, second)
    assert _node(ctl, first)["active"] is True and _node(ctl, second)["overlay"] is True
    _closed(ctl, second)
    _open(ctl, second, follow=True)
    assert _node(ctl, second)["active"] is True
    _closed(ctl, second)


@pytest.mark.covers("session.overlay.open")
def test_overlay_runs_in_its_cwd(ctl):
    sid = _new(ctl)
    root = ctl.instance.root
    _open(ctl, sid, "pwd; exec cat", cwd=str(root), color="#203040")
    text = _overlay_text(ctl, sid, root.name)
    assert str(root) in text or str(root.resolve()) in text
    _closed(ctl, sid)


# -- pane overlays


@pytest.mark.covers(
    "session.overlay.open",
    "session.overlay.close",
    "session.overlay.result",
    "session.overlay.text",
    "field:tree.session.paneOverlays",
    "field:tree.session.surfaces",
)
def test_pane_overlays_cover_one_pane_each(ctl):
    sid = _split(ctl)
    _open(ctl, sid, "echo PANE-RIGHT; exec cat", pane="right")
    node = _node(ctl, sid)
    assert node["paneOverlays"] == ["right"] and node["overlay"] is False
    assert "overlay-right" in _kinds(node) and "overlay-left" not in _kinds(node)
    assert_error(ctl.request("session.overlay.open", sid, command=HOLD, pane="right"), "pane overlay already open")
    _open(ctl, sid, pane="left")
    assert _node(ctl, sid)["paneOverlays"] == ["left", "right"]
    assert _overlay_text(ctl, sid, "PANE-RIGHT", pane="right")
    assert_error(ctl.request("session.overlay.result", sid, pane="right"), "overlay still running")
    # The session-wide slot stays free beside them.
    assert_error(ctl.request("session.overlay.text", sid), "no overlay")

    _closed(ctl, sid, pane="right")
    assert _node(ctl, sid)["paneOverlays"] == ["left"]
    _closed(ctl, sid, pane="left")
    assert_absent(_node(ctl, sid), "paneOverlays")
    assert_error(ctl.request("session.overlay.close", sid, pane="left"), "no overlay")

    # A fresh session: upstream records a closed overlay's status at its surface's teardown, which can land after
    # the next overlay in the same pane has exited and overwrite that one's.
    sid = _split(ctl)
    _open(ctl, sid, "exit 5", pane="right")
    ctl.wait_tree(lambda tree: "paneOverlays" not in find_session(tree, sid))
    _result(ctl, sid, 5, pane="right")


@pytest.mark.covers("session.overlay.open", "field:tree.session.paneOverlays")
def test_unsplit_session_takes_a_left_pane_overlay(ctl):
    sid = _new(ctl)
    _open(ctl, sid, pane="left")
    assert _node(ctl, sid)["paneOverlays"] == ["left"]
    _closed(ctl, sid, pane="left")


@pytest.mark.covers("session.overlay.open")
@pytest.mark.parametrize("hidden", [False, True], ids=["unsplit", "hidden-split"])
def test_pane_overlay_needs_a_rendered_pane(ctl, hidden):
    sid = _split(ctl) if hidden else _new(ctl)
    if hidden:
        # A hidden split still lays out its focused pane, so the right one is hidden only behind a focused left.
        assert_ok(ctl.request("session.focus", sid, pane="left"))
        assert_ok(ctl.request("session.split", sid, mode="off"))
    assert_error(ctl.request("session.overlay.open", sid, command=HOLD, pane="right"), "pane not visible")
    assert_absent(_node(ctl, sid), "paneOverlays")


# -- an html page


@pytest.mark.covers(
    "session.overlay.open",
    "session.overlay.result",
    "session.overlay.close",
    "field:result.pageID",
    "field:result.pageOutcome",
    "field:tree.session.htmlOverlays",
    "field:tree.session.overlay",
)
def test_html_page_occupies_the_slot(ctl):
    sid = _new(ctl)
    path, title, page_id = _open_page(ctl, sid)
    node = _loaded(ctl, sid, title)
    assert node["overlay"] is True and "overlay" not in _kinds(node)
    assert_absent(node, "overlaySizePercent", "hud")
    (page,) = node["htmlOverlays"]
    assert_subset(
        page,
        {
            "id": page_id,
            "file": str(path),
            "state": "loaded",
            "title": title,
            "javascript": False,
            "chromeless": False,
            "persistent": False,
            "browse": False,
            "canGoBack": False,
            "canGoForward": False,
        },
    )
    assert_absent(page, "pane", "url", "cwd", "navigation", "error")
    assert _outcome(ctl, page_id) == {"pageID": page_id, "outcome": "pending"}
    assert_error(ctl.request("session.overlay.result", sid), "no overlay result: the slot holds an html page")
    assert_error(ctl.request("session.overlay.text", sid), "no overlay to read: the slot holds an html page")
    assert_error(ctl.request("session.overlay.copy", sid), "no overlay to read: the slot holds an html page")

    _closed(ctl, sid)
    node = _node(ctl, sid)
    assert node["overlay"] is False
    assert_absent(node, "htmlOverlays")
    assert _outcome(ctl, page_id) == {"pageID": page_id, "outcome": "dismissed"}


@pytest.mark.covers("session.overlay.submit", "field:result.pageOutcome")
@pytest.mark.parametrize("value", ["chosen", ""], ids=["value", "empty"])
def test_submit_answers_the_page_and_closes_it(ctl, value):
    sid = _new(ctl)
    _, title, page_id = _open_page(ctl, sid)
    _loaded(ctl, sid, title)
    assert assert_ok(ctl.request("session.overlay.submit", sid, value=value))["id"] == sid
    node = _node(ctl, sid)
    assert node["overlay"] is False
    assert_absent(node, "htmlOverlays")
    assert _outcome(ctl, page_id) == {"pageID": page_id, "outcome": "submitted", "value": value}


@pytest.mark.covers("session.overlay.submit")
def test_submit_refusals(ctl):
    sid = _new(ctl)
    assert_error(ctl.request("session.overlay.submit", sid, value="x"), "no overlay")
    _open(ctl, sid)
    assert_error(ctl.request("session.overlay.submit", sid, value="x"), "the overlay is not an html page")
    _closed(ctl, sid)
    *_, page_id = _open_page(ctl, sid)
    assert_error(ctl.request("session.overlay.submit", sid), "session.overlay.submit requires a value")
    assert _outcome(ctl, page_id)["outcome"] == "pending"
    _closed(ctl, sid)


@pytest.mark.covers("session.overlay.reload", "session.overlay.navigate")
def test_reload_and_navigate_a_page(ctl):
    sid = _new(ctl)
    _, title, page_id = _open_page(ctl, sid)
    _loaded(ctl, sid, title)
    for args in ({}, {"current": True}):
        assert assert_ok(ctl.request("session.overlay.reload", sid, **args))["id"] == sid
        node = _loaded(ctl, sid, title)
        assert node["htmlOverlays"][0]["id"] == page_id
    assert_error(ctl.request("session.overlay.navigate", sid, to="back"), "no page to go back to")
    assert_error(ctl.request("session.overlay.navigate", sid, to="forward"), "no page to go forward to")
    for args in ({}, {"to": "sideways"}):
        assert_error(
            ctl.request("session.overlay.navigate", sid, **args),
            "session.overlay.navigate requires back, forward, browser or finder",
        )
    _closed(ctl, sid)


@pytest.mark.covers("session.overlay.reload", "session.overlay.navigate")
def test_page_commands_need_a_page(ctl):
    sid = _new(ctl)
    for cmd, args in [("session.overlay.reload", {}), ("session.overlay.navigate", {"to": "back"})]:
        assert_error(ctl.request(cmd, sid, **args), "no overlay")
        _open(ctl, sid)
        assert_error(ctl.request(cmd, sid, **args), "the overlay is not an html page")
        _closed(ctl, sid)


@pytest.mark.covers("session.overlay.open", "field:tree.session.htmlOverlays", "field:tree.session.overlaySizePercent")
def test_page_flags_read_back(ctl):
    sid = _new(ctl)
    path, title = _page(ctl, directory="site")
    grant = path.parent
    result = assert_ok(
        ctl.request(
            "session.overlay.open",
            sid,
            html=str(path),
            cwd=str(grant),
            navigation=True,
            javascript=True,
            sizePercent=50,
            color="#102030",
        )
    )
    node = _loaded(ctl, sid, title)
    assert node["overlaySizePercent"] == 50
    assert_subset(
        node["htmlOverlays"],
        [{"id": result["pageID"], "file": str(path), "cwd": str(grant), "navigation": True, "javascript": True}],
    )
    _closed(ctl, sid)

    path, title = _page(ctl)
    assert_ok(ctl.request("session.overlay.open", sid, html=str(path), chromeless=True))
    page = _loaded(ctl, sid, title)["htmlOverlays"][0]
    assert page["chromeless"] is True
    assert_absent(page, "navigation")
    _closed(ctl, sid)


@pytest.mark.covers("session.overlay.open", "field:tree.session.htmlOverlays", "field:tree.session.paneOverlays")
def test_page_in_a_pane_slot(ctl):
    sid = _split(ctl)
    path, title, page_id = _open_page(ctl, sid, pane="right")
    node = _loaded(ctl, sid, title)
    assert node["paneOverlays"] == ["right"] and node["overlay"] is False
    assert_subset(node["htmlOverlays"], [{"pane": "right", "id": page_id, "file": str(path)}])
    assert "overlay-right" not in _kinds(node)
    assert_error(
        ctl.request("session.overlay.result", sid, pane="right"), "no overlay result: the slot holds an html page"
    )
    assert assert_ok(ctl.request("session.overlay.submit", sid, pane="right", value="pane"))["id"] == sid
    assert_absent(_node(ctl, sid), "paneOverlays", "htmlOverlays")
    assert _outcome(ctl, page_id) == {"pageID": page_id, "outcome": "submitted", "value": "pane"}


class _Page(BaseHTTPRequestHandler):
    def do_GET(self):
        body = b"<!doctype html><title>Served Page</title><body><p>served</p></body>"
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


@pytest.fixture
def served_page():
    """A page on 127.0.0.1, served by this process for the one test that loads a URL."""
    with serve(_Page) as server:
        yield f"http://127.0.0.1:{server.server_port}/page.html"


@pytest.mark.covers("session.overlay.open", "field:tree.session.htmlOverlays")
@pytest.mark.parametrize("browse", [False, True], ids=["pinned", "browse"])
def test_url_page_reads_back_its_address(ctl, served_page, browse):
    sid = _new(ctl)
    result = assert_ok(
        ctl.request("session.overlay.open", sid, url=served_page, **({"browse": True} if browse else {}))
    )
    node = _loaded(ctl, sid, "Served Page")
    assert node["overlay"] is True
    (page,) = node["htmlOverlays"]
    assert_subset(
        page, {"id": result["pageID"], "url": served_page, "browse": browse, "persistent": False, "page": ANY}
    )
    assert_absent(page, "file", "cwd")
    _closed(ctl, sid)


@pytest.mark.covers("session.overlay.result")
@pytest.mark.parametrize(
    ("page", "error"),
    [("nope", "invalid page id"), (str(uuid.uuid4()), "no such page")],
    ids=["invalid", "unknown"],
)
def test_page_result_needs_a_known_page(ctl, page, error):
    assert_error(ctl.request("session.overlay.result", page=page), error)


# -- refusals

FILE = "/agterm-conformance/site/page.html"
URL = "http://127.0.0.1:9/page.html"


@pytest.mark.covers("session.overlay.open")
@pytest.mark.parametrize(
    ("args", "error"),
    [
        pytest.param({}, "session.overlay.open requires a command", id="no-command"),
        pytest.param({"command": ""}, "session.overlay.open requires a command", id="empty-command"),
        pytest.param({"html": FILE, "url": URL}, "session.overlay.open takes --html or --url, not both", id="html-url"),
        pytest.param(
            {"command": "true", "html": FILE}, "session.overlay.open takes a command or --html, not both", id="cmd-html"
        ),
        pytest.param(
            {"command": "true", "url": URL}, "session.overlay.open takes a command or --url, not both", id="cmd-url"
        ),
        pytest.param(
            {"html": FILE, "wait": True}, "session.overlay.open: --wait cannot be combined with --html", id="wait-html"
        ),
        pytest.param(
            {"url": URL, "wait": True}, "session.overlay.open: --wait cannot be combined with --url", id="wait-url"
        ),
        pytest.param(
            {"url": URL, "cwd": "/tmp"}, "session.overlay.open: --cwd cannot be combined with --url", id="cwd-url"
        ),
        pytest.param(
            {"url": "ftp://127.0.0.1/x"},
            "session.overlay.open: --url must be an absolute http or https URL",
            id="ftp-url",
        ),
        pytest.param(
            {"url": "page.html"}, "session.overlay.open: --url must be an absolute http or https URL", id="relative-url"
        ),
        pytest.param(
            {"command": "true", "navigation": True},
            "session.overlay.open: --navigation requires --html or --url",
            id="navigation-program",
        ),
        pytest.param(
            {"command": "true", "javascript": True},
            "session.overlay.open: --js requires --html or --url",
            id="js-program",
        ),
        pytest.param(
            {"command": "true", "chromeless": True},
            "session.overlay.open: --chromeless requires --html",
            id="chromeless-program",
        ),
        pytest.param(
            {"url": URL, "chromeless": True}, "session.overlay.open: --chromeless requires --html", id="chromeless-url"
        ),
        pytest.param(
            {"html": FILE, "chromeless": True, "navigation": True},
            "session.overlay.open: --chromeless cannot be combined with --navigation",
            id="chromeless-navigation",
        ),
        pytest.param(
            {"command": "true", "persistent": True},
            "session.overlay.open: --persistent requires --url",
            id="persistent-program",
        ),
        pytest.param(
            {"html": FILE, "persistent": True},
            "session.overlay.open: --persistent requires --url",
            id="persistent-html",
        ),
        pytest.param(
            {"command": "true", "browse": True},
            "session.overlay.open: --browse requires --url",
            id="browse-program",
        ),
        pytest.param({"html": FILE, "browse": True}, "session.overlay.open: --browse requires --url", id="browse-html"),
        pytest.param(
            {"html": "site/page.html"},
            "session.overlay.open: html file must be an absolute path",
            id="relative-html",
        ),
        pytest.param(
            {"html": FILE, "cwd": "site"}, "session.overlay.open: cwd must be an absolute path", id="relative-cwd"
        ),
        pytest.param(
            {"html": FILE, "cwd": "/"}, "session.overlay.open: cwd must not be / or the home directory", id="root-cwd"
        ),
        pytest.param(
            {"html": FILE, "cwd": FILE},
            "session.overlay.open: cwd must be a directory containing the html file",
            id="cwd-is-file",
        ),
        pytest.param(
            {"html": FILE, "cwd": "/agterm-conformance/other"},
            "session.overlay.open: html file is outside cwd",
            id="outside-cwd",
        ),
        pytest.param({"command": "true", "color": "red"}, "invalid color: red (#rrggbb)", id="color"),
        pytest.param(
            {"command": "true", "pane": "scratch"}, "session.overlay: --pane must be left or right", id="pane"
        ),
        pytest.param(
            {"command": "true", "pane": "left", "sizePercent": 50},
            "session.overlay.open: --pane is mutually exclusive with --size-percent",
            id="pane-size",
        ),
    ],
)
def test_open_refusals(ctl, args, error):
    sid = _new(ctl)
    assert_error(ctl.request("session.overlay.open", sid, **args), error)
    node = _node(ctl, sid)
    assert node["overlay"] is False
    assert_absent(node, "paneOverlays", "htmlOverlays")


@pytest.mark.covers("session.overlay.close", "session.overlay.result", "session.overlay.text")
@pytest.mark.parametrize(
    ("cmd", "args"),
    [
        ("session.overlay.close", {"pane": "scratch"}),
        ("session.overlay.result", {"pane": "scratch"}),
        ("session.overlay.text", {"pane": "scratch"}),
        ("session.overlay.reload", {"pane": "scratch"}),
        ("session.overlay.submit", {"pane": "scratch", "value": "x"}),
    ],
)
def test_overlay_pane_selector_refusals(ctl, cmd, args):
    assert_error(ctl.request(cmd, _new(ctl), **args), "session.overlay: --pane must be left or right")


@pytest.mark.covers(
    "session.overlay.open",
    "session.overlay.close",
    "session.overlay.resize",
    "session.overlay.result",
    "session.overlay.text",
    "session.overlay.submit",
    "session.overlay.reload",
)
@pytest.mark.parametrize(
    ("cmd", "args"),
    [
        ("session.overlay.open", {"command": HOLD}),
        ("session.overlay.close", {}),
        ("session.overlay.resize", {"sizePercent": 50}),
        ("session.overlay.result", {}),
        ("session.overlay.text", {}),
        ("session.overlay.submit", {"value": "x"}),
        ("session.overlay.reload", {}),
    ],
)
def test_unknown_session_target(ctl, cmd, args):
    assert_error(ctl.request(cmd, UNKNOWN, **args), f"no such session: {UNKNOWN}")
