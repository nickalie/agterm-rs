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
        self.menu: list[Element] = []
        self.pressed: list[Element] = []
        self.posted: list[Chord] = []
        self.polls = 0

    def _elements(self, ui_id: str) -> list[Element]:
        self.polls += 1
        return [element for element in self.elements if element.ui_id == ui_id]

    def _menu_item(self, name: str) -> Element | None:
        return next((item for item in self.menu if item.name == name), None)

    def _press_element(self, element: Element) -> None:
        self.pressed.append(element)

    def _text(self, element: Element) -> str:
        return str(element.value)

    def _post_chord(self, chord: Chord) -> None:
        self.posted.append(chord)


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
    driver.menu = [item]
    assert driver.menu_item("New Session") is item
    with pytest.raises(ElementNotFound, match="menu item 'Open'"):
        driver.menu_item("Open", timeout=0.1)


def test_absent_waits_until_no_element_carries_the_id() -> None:
    driver = FakeDriver([Element("add-session", "button")])
    driver.absent("htmlOverlay.back")
    with pytest.raises(AssertionError):
        driver.absent("add-session", timeout=0.1)


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


def test_spike_b_ancestry_starts_at_this_process() -> None:
    chain = spike.ancestry()
    assert chain and chain[0].startswith(f"{spike.os.getpid()} ")


# -- the `ui` fixture


def run_ui_suite(pytester: pytest.Pytester, fake_bin: Path) -> pytest.RunResult:
    return pytester.runpytest("-p", "no:cacheprovider", "-rs", "--impl", "swift-linux", "--bin", str(fake_bin))


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
            assert "add-session" in ui.ui_ids.keys
        """
    )
    run_ui_suite(pytester, fake_bin).assert_outcomes(passed=1)
