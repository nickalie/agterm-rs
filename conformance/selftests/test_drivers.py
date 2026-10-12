import importlib.util
import sys
from pathlib import Path
from typing import Any

import pytest
from agterm_conformance.drivers import ax
from agterm_conformance.drivers import base as drivers
from agterm_conformance.drivers.base import (
    Chord,
    Driver,
    DriverError,
    Element,
    ElementNotFound,
    NoDriver,
    ReadFailed,
    UiIds,
    UnknownUiId,
    driver_class,
    driver_name,
    make_driver,
    parse_chord,
)

spec = importlib.util.spec_from_file_location(
    "spike_b", Path(__file__).resolve().parent.parent / "tools" / "spike_b.py"
)
assert spec is not None and spec.loader is not None
spike = importlib.util.module_from_spec(spec)
sys.modules["spike_b"] = spike
spec.loader.exec_module(spike)

IDS = UiIds(
    [
        "add-session",
        "ask-button-{choice}",
        "settings-status-{status}",
        "settings-status-shape-{status}",
        "settings-status-reset",
        "pane-lead-cover-{pane}",
        "htmlOverlay.back",
    ]
)


class FakeDriver(Driver):
    name = "fake"

    def __init__(self, elements: list[Element], **kwargs: Any) -> None:
        super().__init__(4242, IDS, timeout=0.2, **kwargs)
        self.elements = elements
        self.menu_items: list[Element] = []
        self.menu_bar: dict[tuple[str, ...], Element] = {}
        self.tabs: list[Element] = []
        self.pressed: list[Element] = []
        self.posted: list[Chord] = []
        self.typed: list[tuple[Element, str]] = []
        self.hovered: list[Element] = []
        self.polls = 0

    def _elements(self, ui_id: str) -> list[Element]:
        self.polls += 1
        return [element for element in self.elements if element.ui_id == ui_id]

    def _identified(self) -> list[Element]:
        return [element for element in self.elements if element.ui_id]

    def _menu_item(self, name: str) -> Element | None:
        return next((item for item in self.menu_items if item.name == name), None)

    def _menu_command(self, path: tuple[str, ...]) -> Element | None:
        return self.menu_bar.get(path)

    def _tab(self, name: str) -> Element | None:
        return next((tab for tab in self.tabs if tab.name == name), None)

    def _press_element(self, element: Element) -> None:
        self.pressed.append(element)

    def _text(self, element: Element) -> str:
        return str(element.value)

    def _post_chord(self, chord: Chord) -> None:
        self.posted.append(chord)

    def _set_text(self, element: Element, text: str) -> None:
        self.typed.append((element, text))

    def _hover(self, element: Element) -> None:
        self.hovered.append(element)


# -- ids and templates


@pytest.mark.parametrize(
    ("ui_id", "key"),
    [
        ("add-session", "add-session"),
        ("ask-button-Yes", "ask-button-{choice}"),
        ("ask-button-Not now", "ask-button-{choice}"),
        ("settings-status-shape-idle", "settings-status-shape-{status}"),
        ("settings-status-idle", "settings-status-{status}"),
        ("settings-status-reset", "settings-status-reset"),
        ("htmlOverlay.back", "htmlOverlay.back"),
    ],
)
def test_key_for_maps_an_id_to_its_entry(ui_id: str, key: str) -> None:
    assert IDS.key_for(ui_id) == key


@pytest.mark.parametrize("ui_id", ["add-sessions", "ask-button-", "pane-lead-cover-{pane}", "htmlOverlayXback", ""])
def test_key_for_refuses_what_is_not_cataloged(ui_id: str) -> None:
    with pytest.raises(UnknownUiId):
        IDS.key_for(ui_id)


def test_expand_fills_every_placeholder() -> None:
    assert IDS.expand("ask-button-{choice}", choice="Yes") == "ask-button-Yes"
    assert IDS.expand("add-session") == "add-session"
    assert IDS.key_for(IDS.expand("pane-lead-cover-{pane}", pane="p1")) == "pane-lead-cover-{pane}"


@pytest.mark.parametrize(
    ("template", "values", "error", "message"),
    [
        ("ask-title", {}, UnknownUiId, "not in ui-ids.yaml"),
        ("ask-button-{choice}", {}, ValueError, "no value for choice"),
        ("ask-button-{choice}", {"choice": "Yes", "pane": "p"}, ValueError, "no placeholder pane"),
        ("add-session", {"choice": "Yes"}, ValueError, "no placeholder choice"),
        ("ask-button-{choice}", {"choice": ""}, ValueError, "non-empty string"),
        ("ask-button-{choice}", {"choice": 1}, ValueError, "non-empty string"),
    ],
)
def test_expand_refuses(template: str, values: dict[str, Any], error: type[Exception], message: str) -> None:
    with pytest.raises(error, match=message):
        IDS.expand(template, **values)


def test_the_committed_ui_ids_load_with_manual_ids_and_templates() -> None:
    ids = UiIds.load()
    assert {"add-session", "session-row", "htmlOverlay.close", "settings-interface-{element}"} <= ids.keys
    assert ids.key_for("settings-interface-sidebar") == "settings-interface-{element}"
    assert ids.key_for("settings-status-shape-working") == "settings-status-shape-{status}"


# -- chords


@pytest.mark.parametrize(
    ("spec", "chord"),
    [
        ("a", Chord("a")),
        ("return", Chord("return")),
        ("cmd+shift+t", Chord("t", frozenset({"cmd", "shift"}))),
        ("option+command+up", Chord("up", frozenset({"alt", "cmd"}))),
        ("ctrl+/", Chord("/", frozenset({"ctrl"}))),
        ("f12", Chord("f12")),
    ],
)
def test_parse_chord(spec: str, chord: Chord) -> None:
    assert parse_chord(spec) == chord


@pytest.mark.parametrize("spec", ["", "+", "cmd+", "hyper+a", "A", "cmd+shift", "ab", "f13", "é"])
def test_parse_chord_refuses(spec: str) -> None:
    with pytest.raises(ValueError, match="bad chord"):
        parse_chord(spec)


def test_ax_has_a_key_code_for_every_key_and_a_portable_name_for_every_role() -> None:
    assert drivers.NAMED_KEYS | drivers.PRINTABLE_KEYS <= ax.KEY_CODES.keys()
    assert set(ax.ROLES.values()) <= drivers.ROLES
    assert set(ax.FLAGS) == set(drivers.MODIFIERS.values())


@pytest.mark.parametrize(
    ("value", "plain"),
    [(None, None), (True, True), (3, 3), (0.5, 0.5), ("x", "x"), (b"raw", "b'raw'")],
)
def test_ax_values_become_plain_python(value: Any, plain: Any) -> None:
    assert ax._plain(value) == plain and type(ax._plain(value)) is type(plain)


# -- the driver interface


def test_find_returns_the_first_element_matching_id_and_attributes() -> None:
    menu = Element("add-session", "menu-button", "Add session")
    button = Element("add-session", "button", "Add session", enabled=True)
    driver = FakeDriver([menu, button])
    assert driver.find("add-session") is menu
    assert driver.find("add-session", role="button", enabled=True) is button


def test_find_waits_for_the_element() -> None:
    late = Element("ask-button-Yes", "button", "Yes")
    driver = FakeDriver([])

    def appear(ui_id: str) -> list[Element]:
        driver.polls += 1
        return [late] if driver.polls >= 3 else []

    driver._elements = appear  # type: ignore[method-assign]
    assert driver.find("ask-button-Yes") is late


def test_find_times_out_with_a_diagnosis() -> None:
    driver = FakeDriver([Element("add-session", "button")])
    with pytest.raises(ElementNotFound, match=r"no add-session \{'role': 'menu-button'\} in pid 4242"):
        driver.find("add-session", timeout=0.1, role="menu-button")


@pytest.mark.parametrize(
    ("ui_id", "attrs", "error", "message"),
    [
        ("session-row", {}, UnknownUiId, "session-row"),
        ("add-session", {"title": "x"}, ValueError, "unknown attribute title"),
        ("add-session", {"role": "AXButton"}, ValueError, "unknown role"),
    ],
)
def test_find_refuses_bad_queries_before_polling(
    ui_id: str, attrs: dict[str, Any], error: type[Exception], message: str
) -> None:
    driver = FakeDriver([])
    with pytest.raises(error, match=message):
        driver.find(ui_id, **attrs)
    assert driver.polls == 0


def test_click_and_text_resolve_an_id_or_take_an_element() -> None:
    element = Element("add-session", "button", value="label")
    driver = FakeDriver([element])
    driver.click("add-session")
    driver.click(element)
    assert driver.pressed == [element, element]
    assert driver.text("add-session") == "label"


def test_set_text_and_hover_resolve_an_id_or_take_an_element() -> None:
    field = Element("add-session", "text-field")
    driver = FakeDriver([field])
    driver.set_text("add-session", "needle")
    driver.hover(field)
    assert driver.typed == [(field, "needle")] and driver.hovered == [field]


def test_press_validates_every_chord_before_posting_any() -> None:
    driver = FakeDriver([])
    with pytest.raises(ValueError, match="bad chord"):
        driver.press("a", "nope")
    assert driver.posted == []
    driver.press("a", "cmd+n")
    assert driver.posted == [Chord("a"), Chord("n", frozenset({"cmd"}))]


def test_menu_item_waits_by_title() -> None:
    driver = FakeDriver([])
    item = Element(None, "menu-item", "New Session")
    driver.menu_items = [item]
    assert driver.menu_item("New Session") is item
    with pytest.raises(ElementNotFound, match="menu item 'Open'"):
        driver.menu_item("Open", timeout=0.1)


def test_menu_presses_the_command_at_the_end_of_its_path() -> None:
    driver = FakeDriver([])
    find = Element(None, "menu-item", "Find…")
    driver.menu_bar = {("View", "Find…"): find}
    driver.menu("View", "Find…")
    assert driver.pressed == [find]
    with pytest.raises(ElementNotFound, match="menu command 'Edit > Find…'"):
        driver.menu("Edit", "Find…", timeout=0.1)
    with pytest.raises(ValueError, match="needs a path"):
        driver.menu()


def test_tab_waits_by_title() -> None:
    driver = FakeDriver([])
    general = Element(None, "button", "General")
    driver.tabs = [general]
    assert driver.tab("General") is general
    with pytest.raises(ElementNotFound, match="tab 'Interface'"):
        driver.tab("Interface", timeout=0.1)


def test_all_lists_every_element_of_an_id_without_waiting() -> None:
    rows = [Element("add-session", "button", value=str(n)) for n in range(3)]
    driver = FakeDriver([*rows, Element("htmlOverlay.back", "button")])
    assert driver.all("add-session") == rows
    assert driver.all("ask-button-No") == []
    with pytest.raises(UnknownUiId):
        driver.all("session-row")


def test_a_miss_lists_what_carries_an_identifier() -> None:
    driver = FakeDriver(
        [Element("add-session", "popup", "Add session"), Element("htmlOverlay.back", "button", enabled=False)]
    )
    with pytest.raises(
        ElementNotFound,
        match=r"identified now: add-session\(popup name='Add session'\), htmlOverlay.back\(button disabled\)$",
    ):
        driver.find("ask-button-Yes", timeout=0.1)
    assert driver.inventory() == driver.elements


def test_a_miss_says_when_the_inventory_could_not_be_read() -> None:
    driver = FakeDriver([])

    def unread() -> list[Element]:
        raise ReadFailed("AXError -25204")

    driver._identified = unread  # type: ignore[method-assign]
    with pytest.raises(ElementNotFound, match="inventory unread: AXError -25204"):
        driver.find("add-session", timeout=0.1)


def test_absent_waits_until_no_element_carries_the_id() -> None:
    driver = FakeDriver([Element("add-session", "button")])
    driver.absent("htmlOverlay.back")
    with pytest.raises(AssertionError, match="fake: add-session still present in pid 4242"):
        driver.absent("add-session", timeout=0.1)


class FakeAx:
    """ApplicationServices for one app whose AXChildren answers each read with the next queued AXError."""

    def __init__(self, children_errors: list[int], child_error: int = ax.AX_SUCCESS) -> None:
        self.app, self.dialog = object(), object()
        self.children_errors = children_errors
        self.child_error = child_error

    def AXIsProcessTrusted(self) -> bool:
        return True

    def AXUIElementCreateApplication(self, pid: int) -> object:
        return self.app

    def AXUIElementSetMessagingTimeout(self, handle: object, seconds: float) -> int:
        return ax.AX_SUCCESS

    def AXUIElementCopyAttributeValue(self, handle: object, name: str, out: None) -> tuple[int, Any]:
        if handle is self.app:
            if name != "AXChildren":
                return ax.AX_ATTRIBUTE_UNSUPPORTED, None
            error = self.children_errors.pop(0) if self.children_errors else ax.AX_SUCCESS
            return error, [self.dialog] if error == ax.AX_SUCCESS else None
        if self.child_error != ax.AX_SUCCESS:
            return self.child_error, None
        return {"AXIdentifier": (ax.AX_SUCCESS, "add-session"), "AXRole": (ax.AX_SUCCESS, "AXButton")}.get(
            name, (ax.AX_NO_VALUE, None)
        )


def fake_ax_driver(monkeypatch: pytest.MonkeyPatch, bindings: FakeAx) -> ax.AxDriver:
    monkeypatch.setattr(ax, "_bindings", lambda: (bindings, None))
    return ax.AxDriver(4242, IDS, timeout=0.2)


def test_ax_absent_never_accepts_a_read_the_app_did_not_answer(monkeypatch: pytest.MonkeyPatch) -> None:
    driver = fake_ax_driver(monkeypatch, FakeAx([ax.AX_CANNOT_COMPLETE] * 1000))
    with pytest.raises(AssertionError, match="still present.*AXChildren in pid 4242 failed: AXError -25204"):
        driver.absent("add-session", timeout=0.1)
    with pytest.raises(ReadFailed):
        driver.attribute(driver.app, "AXChildren")


def test_ax_retries_a_failed_read(monkeypatch: pytest.MonkeyPatch) -> None:
    bindings = FakeAx([ax.AX_CANNOT_COMPLETE, ax.AX_CANNOT_COMPLETE])
    element = fake_ax_driver(monkeypatch, bindings).find("add-session")
    assert (element.ui_id, element.role, element.name) == ("add-session", "button", None)
    assert bindings.children_errors == []


def test_ax_treats_a_vanished_child_as_absent_but_not_a_vanished_app(monkeypatch: pytest.MonkeyPatch) -> None:
    fake_ax_driver(monkeypatch, FakeAx([], child_error=ax.AX_INVALID_ELEMENT)).absent("add-session")
    driver = fake_ax_driver(monkeypatch, FakeAx([ax.AX_INVALID_ELEMENT]))
    with pytest.raises(ReadFailed, match="AXError -25202"):
        driver.attribute(driver.app, "AXChildren")


# -- selection


@pytest.mark.parametrize(
    ("impl", "platform", "name"),
    [
        ("swift-macos", "linux", "ax"),
        ("swift-linux", "darwin", "atspi"),
        ("rust", "darwin", "ax"),
        ("rust", "linux", "atspi"),
        ("rust", "win32", "uia"),
    ],
)
def test_driver_name_follows_the_reference_platform_else_the_host(impl: str, platform: str, name: str) -> None:
    assert driver_name(impl, platform) == name


def test_driver_name_refuses_an_unknown_platform() -> None:
    with pytest.raises(NoDriver, match="'sunos5'"):
        driver_name("rust", "sunos5")


@pytest.mark.parametrize(
    ("impl", "platform", "message"),
    [
        ("swift-linux", "linux", "atspi ui driver is not written yet"),
        ("rust", "win32", "uia ui driver is not written yet"),
        ("swift-macos", "linux", "needs darwin, this host is linux"),
    ],
)
def test_driver_class_says_why_there_is_none(impl: str, platform: str, message: str) -> None:
    with pytest.raises(NoDriver, match=message):
        driver_class(impl, platform)


def test_ax_is_the_driver_for_macos() -> None:
    assert driver_class("swift-macos", "darwin") is ax.AxDriver
    assert driver_class("rust", "darwin") is ax.AxDriver


def test_make_driver_hands_the_pid_and_ids_over(monkeypatch: pytest.MonkeyPatch) -> None:
    created: list[tuple[int, UiIds | None, dict[str, Any]]] = []

    class Recording:
        def __init__(self, pid: int, ui_ids: UiIds | None, **kwargs: Any) -> None:
            created.append((pid, ui_ids, kwargs))

    monkeypatch.setattr(drivers, "driver_class", lambda impl, platform: Recording)
    make_driver("swift-macos", 42, IDS, platform="darwin", timeout=3.0)
    assert created == [(42, IDS, {"timeout": 3.0})]


def test_ax_without_pyobjc_is_a_driver_error(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setitem(sys.modules, "ApplicationServices", None)
    with pytest.raises(DriverError, match="pyobjc-framework-ApplicationServices"):
        ax.AxDriver(1, IDS)
    with pytest.raises(DriverError):
        ax.trust()


def test_ax_refuses_an_unknown_post_mode() -> None:
    with pytest.raises(ValueError, match="post must be one of"):
        ax.AxDriver(1, IDS, post="tap")


# -- spike B's verdict


GOOD = {
    "addSession": {"uiId": "add-session"},
    "menuItemPress": 0,
    "newSession": "s2",
    "keystrokes": [{"landed": False}, {"landed": True}],
}


@pytest.mark.parametrize(
    ("change", "ok"),
    [
        ({}, True),
        ({"addSession": {"uiId": None}}, False),
        ({"menuItemPress": -25204}, False),
        ({"newSession": None}, False),
        ({"keystrokes": [{"landed": False}]}, False),
        ({"stopError": "leftover"}, False),
    ],
)
def test_spike_b_passes_only_when_every_step_held(change: dict[str, Any], ok: bool) -> None:
    assert spike.passed({**GOOD, **change}) is ok


def test_spike_b_reports_a_socket_failure(monkeypatch: pytest.MonkeyPatch) -> None:
    class Instance:
        pid, version = 7, "0"

        def request(self, cmd: str) -> Any:
            raise spike.ClientError("connect failed: no such file")

        def log_tail(self, lines: int) -> str:
            return "app exited"

    stopped: list[Any] = []

    class Adapter:
        def launch(self) -> Instance:
            return Instance()

        def stop(self, instance: Instance) -> None:
            stopped.append(instance)

    monkeypatch.setattr(spike, "trust", lambda: {"accessibility": True, "postEvent": True})
    monkeypatch.setattr(spike, "AxDriver", lambda pid, timeout: None)
    report = spike.run(Adapter(), 0.1)
    assert report["error"] == "ClientError: connect failed: no such file"
    assert report["logTail"] == "app exited" and len(stopped) == 1
    assert not spike.passed(report)


def test_spike_b_ancestry_starts_at_this_process() -> None:
    chain = spike.ancestry()
    assert chain and chain[0].startswith(f"{spike.os.getpid()} ")


# -- the `ui` fixture


def run_ui_suite(pytester: pytest.Pytester, fake_bin: Path, *args: str) -> pytest.RunResult:
    return pytester.runpytest(
        "-p", "no:cacheprovider", "-rs", "--impl", "swift-linux", "--bin", str(fake_bin), *args
    )  # fmt: skip


def test_ui_fixture_skips_as_unsupported_before_anything_launches(
    pytester: pytest.Pytester, roots: list[Path], fake_bin: Path
) -> None:
    pytester.makepyfile(
        """
        def test_ui(ui):
            raise AssertionError("must not run")

        def test_ui_after_ctl(ctl, ui):
            raise AssertionError("must not run")
        """
    )
    result = run_ui_suite(pytester, fake_bin)
    result.assert_outcomes(skipped=2)
    result.stdout.fnmatch_lines_random(
        ["*unsupported: the atspi ui driver is not written yet*", "*2 skip(unsupported)*"]
    )
    assert roots == []


def test_ui_fixture_drives_the_ctl_instance(
    pytester: pytest.Pytester, roots: list[Path], fake_bin: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    class Recording:
        def __init__(self, pid: int, ui_ids: UiIds | None) -> None:
            self.pid, self.ui_ids = pid, ui_ids

    monkeypatch.setattr(drivers, "driver_class", lambda impl, platform=sys.platform: Recording)
    pytester.makepyfile(
        """
        def test_ui(ctl, ui):
            assert ui.pid == ctl.instance.pid
            assert ui.ui_ids.keys == {"next-to-the-catalog"}
        """
    )
    (pytester.path / "spec").mkdir()
    (pytester.path / "spec" / "ui-ids.yaml").write_text("ids:\n  next-to-the-catalog: {}\n")
    run_ui_suite(pytester, fake_bin, "--catalog", "spec/catalog.yaml").assert_outcomes(passed=1)


def test_ui_for_skips_as_unsupported_before_anything_launches(
    pytester: pytest.Pytester, roots: list[Path], fake_bin: Path
) -> None:
    pytester.makepyfile(
        """
        def test_ui_for(launch, ui_for):
            raise AssertionError("must not run")
        """
    )
    result = run_ui_suite(pytester, fake_bin)
    result.assert_outcomes(skipped=1)
    result.stdout.fnmatch_lines(["*unsupported: the atspi ui driver is not written yet*"])
    assert roots == []


def test_ui_for_drives_the_instance_it_is_given(
    pytester: pytest.Pytester, roots: list[Path], fake_bin: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    class Recording:
        def __init__(self, pid: int, ui_ids: UiIds | None) -> None:
            self.pid = pid

    monkeypatch.setattr(drivers, "driver_class", lambda impl, platform=sys.platform: Recording)
    pytester.makepyfile(
        """
        def test_ui_for(launch, ui_for):
            first, second = launch(), launch()
            assert ui_for(second).pid == second.instance.pid != first.instance.pid
            assert ui_for(first.instance).pid == first.instance.pid
        """
    )
    (pytester.path / "spec").mkdir()
    (pytester.path / "spec" / "ui-ids.yaml").write_text("ids: {}\n")
    run_ui_suite(pytester, fake_bin, "--catalog", "spec/catalog.yaml").assert_outcomes(passed=1)
