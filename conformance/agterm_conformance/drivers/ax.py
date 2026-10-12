"""The macOS driver: the app's AX tree through pyobjc, AXPress for clicks, CGEvent keystrokes posted to the pid.

Needs the `ax` extra (pyobjc) and the accessibility grant for the process TCC holds responsible for the run
(conformance/docs/spike-macos.md, spike B).
"""

from __future__ import annotations

from collections import deque
from collections.abc import Iterator, Mapping
from typing import Any

from agterm_conformance.drivers.base import Chord, Driver, DriverError, Element, ReadFailed, UiIds
from agterm_conformance.wait import DEFAULT_TIMEOUT

ROLES: Mapping[str, str] = {
    "AXButton": "button",
    "AXMenuButton": "menu-button",
    "AXPopUpButton": "popup",
    "AXMenuItem": "menu-item",
    "AXCheckBox": "checkbox",
    "AXRadioButton": "radio",
    "AXTextField": "text-field",
    "AXTextArea": "text-field",
    "AXStaticText": "text",
    "AXSlider": "slider",
    "AXRow": "row",
    "AXCell": "row",
    "AXGroup": "group",
    "AXImage": "image",
    "AXWindow": "window",
}
# ANSI layout virtual key codes (HIToolbox Events.h).
KEY_CODES: Mapping[str, int] = {
    "a": 0, "s": 1, "d": 2, "f": 3, "h": 4, "g": 5, "z": 6, "x": 7, "c": 8, "v": 9, "b": 11, "q": 12, "w": 13,
    "e": 14, "r": 15, "y": 16, "t": 17, "1": 18, "2": 19, "3": 20, "4": 21, "6": 22, "5": 23, "=": 24, "9": 25,
    "7": 26, "-": 27, "8": 28, "0": 29, "]": 30, "o": 31, "u": 32, "[": 33, "i": 34, "p": 35, "return": 36,
    "l": 37, "j": 38, "'": 39, "k": 40, ";": 41, "\\": 42, ",": 43, "/": 44, "n": 45, "m": 46, ".": 47, "tab": 48,
    "space": 49, "`": 50, "delete": 51, "escape": 53, "enter": 76, "f5": 96, "f6": 97, "f7": 98, "f3": 99,
    "f8": 100, "f9": 101, "f11": 103, "f10": 109, "f12": 111, "home": 115, "page-up": 116, "forward-delete": 117,
    "f4": 118, "end": 119, "f2": 120, "page-down": 121, "f1": 122, "left": 123, "right": 124, "down": 125,
    "up": 126,
}  # fmt: skip
FLAGS: Mapping[str, int] = {"shift": 1 << 17, "ctrl": 1 << 18, "alt": 1 << 19, "cmd": 1 << 20}
POST_MODES = ("pid", "hid")
AX_SUCCESS = 0
AX_INVALID_ELEMENT = -25202
AX_CANNOT_COMPLETE = -25204
AX_ATTRIBUTE_UNSUPPORTED = -25205
AX_NO_VALUE = -25212
# The menu bar keeps every menu's items in the tree, closed or not; an open menu is found outside it.
MENU_BAR_ROLE = "AXMenuBar"
MAX_DEPTH = 64
MESSAGING_TIMEOUT = 2.0


def _bindings() -> tuple[Any, Any]:
    try:
        import ApplicationServices
        import Quartz
    except ImportError as error:
        raise DriverError(
            "the ax driver needs pyobjc-framework-ApplicationServices and pyobjc-framework-Quartz "
            f"(the conformance[ax] extra): {error}"
        ) from None
    return ApplicationServices, Quartz


def trust() -> dict[str, bool]:
    """What TCC grants this process: AX reads and actions, and CGEvent posting."""
    ax, quartz = _bindings()
    return {
        "accessibility": bool(ax.AXIsProcessTrusted()),
        "postEvent": bool(quartz.CGPreflightPostEventAccess()),
    }


class AxDriver(Driver):
    name = "ax"

    def __init__(
        self, pid: int, ui_ids: UiIds | None = None, timeout: float = DEFAULT_TIMEOUT, post: str = "pid"
    ) -> None:
        if post not in POST_MODES:
            raise ValueError(f"post must be one of {POST_MODES}, got {post!r}")
        super().__init__(pid, ui_ids, timeout)
        self.ax, self.quartz = _bindings()
        if not self.ax.AXIsProcessTrusted():
            raise DriverError("accessibility is not granted to the process responsible for this run")
        self.post = post
        self.app = self.ax.AXUIElementCreateApplication(pid)
        # AXPress on a menu button returns only once its menu closes; a short timeout keeps the run going.
        self.ax.AXUIElementSetMessagingTimeout(self.app, MESSAGING_TIMEOUT)

    def attribute(self, handle: Any, name: str) -> Any:
        """None when the element has no such value or has gone; ReadFailed when the app did not answer."""
        error, value = self.ax.AXUIElementCopyAttributeValue(handle, name, None)
        if error == AX_SUCCESS:
            return value
        if error in (AX_ATTRIBUTE_UNSUPPORTED, AX_NO_VALUE) or (error == AX_INVALID_ELEMENT and handle is not self.app):
            return None
        raise ReadFailed(f"{self.name}: reading {name} in pid {self.pid} failed: AXError {error}")

    def actions(self, element: Element) -> list[str]:
        error, names = self.ax.AXUIElementCopyActionNames(element.handle, None)
        return list(names or []) if error == AX_SUCCESS else []

    def perform(self, element: Element, action: str) -> int:
        """The AXError of `action`; a menu-opening press reports cannot-complete once the timeout passes."""
        return int(self.ax.AXUIElementPerformAction(element.handle, action))

    def activate(self) -> None:
        """Make the app frontmost, for keystrokes posted through the HID tap."""
        self.ax.AXUIElementSetAttributeValue(self.app, "AXFrontmost", True)

    def walk(self) -> Iterator[Any]:
        """Breadth first from the application element, skipping the menu bar."""
        queue: deque[tuple[Any, int]] = deque([(self.app, 0)])
        while queue:
            handle, depth = queue.popleft()
            yield handle
            if depth >= MAX_DEPTH or self.attribute(handle, "AXRole") == MENU_BAR_ROLE:
                continue
            queue.extend((child, depth + 1) for child in self.attribute(handle, "AXChildren") or ())

    def snapshot(self, handle: Any) -> Element:
        role = self.attribute(handle, "AXRole")
        name = self.attribute(handle, "AXTitle") or self.attribute(handle, "AXDescription") or None
        enabled = self.attribute(handle, "AXEnabled")
        return Element(
            ui_id=self.attribute(handle, "AXIdentifier") or None,
            role=ROLES.get(str(role), "other"),
            name=None if name is None else str(name),
            value=_plain(self.attribute(handle, "AXValue")),
            enabled=None if enabled is None else bool(enabled),
            handle=handle,
        )

    def _elements(self, ui_id: str) -> list[Element]:
        return [self.snapshot(handle) for handle in self.walk() if self.attribute(handle, "AXIdentifier") == ui_id]

    def _menu_item(self, name: str) -> Element | None:
        for handle in self.walk():
            if self.attribute(handle, "AXRole") == "AXMenuItem" and self.attribute(handle, "AXTitle") == name:
                return self.snapshot(handle)
        return None

    def _press_element(self, element: Element) -> None:
        error = self.perform(element, "AXPress")
        if error not in (AX_SUCCESS, AX_CANNOT_COMPLETE):
            raise DriverError(f"AXPress on {element} failed: AXError {error}")

    def _text(self, element: Element) -> str:
        value = element.value
        return value if isinstance(value, str) else element.name or ""

    def _post_chord(self, chord: Chord) -> None:
        q = self.quartz
        flags = 0
        for modifier in chord.modifiers:
            flags |= FLAGS[modifier]
        for down in (True, False):
            event = q.CGEventCreateKeyboardEvent(None, KEY_CODES[chord.key], down)
            q.CGEventSetFlags(event, flags)
            if self.post == "pid":
                q.CGEventPostToPid(self.pid, event)
            else:
                q.CGEventPost(q.kCGHIDEventTap, event)


def _plain(value: Any) -> Any:
    """pyobjc hands NSString/NSNumber over as str/int/float subclasses; an AXValue struct or element as text."""
    if value is None or isinstance(value, bool):
        return value
    for kind in (str, int, float):
        if isinstance(value, kind):
            return kind(value)
    return str(value)
