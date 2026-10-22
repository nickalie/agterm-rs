#!/usr/bin/env python3
"""Check from screenshots of the real app that the title bar, the sidebar and the terminal draw one color, the
terminal theme's background, as upstream's window does (`WindowAppearance.sync`), and that a non-neutral sidebar tint
washes the sidebar alone. A Rust-build check outside the catalog: no reference runs it.

  python conformance/tools/chrome_probe.py [--bin PATH] [--timeout SECONDS] [--keep DIR]

The staged build runs under the suite's isolation (tools/run-rust-linux.sh's: a private headless weston and session
bus, a fresh root), the weston started with its debug protocol so `weston-screenshooter` may read the output. Each
launch maximizes its window (`window.zoom`, read back as `zoomed`), reads the sidebar's width from `tree`, and waits
for the terminal's first frame on screen: the cursor, in a color only the terminal draws, inside the deck. Then it
samples the most common color of three boxes clear of any text: the middle of the title bar, the sidebar below its
rows, the terminal below its prompt.

- `theme`: launched with a seeded theme and the default (compact) toolbar, the three equal the theme's background.
- `theme-change`: `theme.set` to another theme; once the terminal shows the new background, the title bar and the
  sidebar show it too.
- `tint`: relaunched with the normal toolbar and the sidebar tint at Darker 5, the title bar and the terminal keep
  the background and the sidebar shows it under a 0.30 black wash.

The build renders on lavapipe only: $CONFORMANCE_LAVAPIPE_ICD, else the ICD `scripts/fetch-lavapipe.sh --print-icd`
finds (nothing is fetched here). It logs its terminal's first frame under the target `ngterm::surface`.

--bin defaults to the stage run-rust-linux.sh builds (target/conformance-stage/bin/ngterm); the themes are read from
its share/ghostty/themes. --keep saves every screenshot taken there. Prints a JSON report; exits 0 when every check
holds.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import struct
import subprocess
import sys
import time
import zlib
from collections import Counter
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from pathlib import Path
from typing import Any

REPO = Path(__file__).resolve().parents[2]
DEFAULT_BIN = REPO / "target" / "conformance-stage" / "bin" / "ngterm"
FETCH_LAVAPIPE = REPO / "scripts" / "fetch-lavapipe.sh"
# The harness's lavapipe variable (`impls.base.LAVAPIPE_ICD_VARIABLE`), spelled here: importing the harness scrubs every
# NGTERM_* variable, which the script still needs, so lavapipe is found before that import.
LAVAPIPE_ICD_VARIABLE = "CONFORMANCE_LAVAPIPE_ICD"
# The output the window is maximized into: smaller than the suite's, so a screenshot decodes quickly in pure Python.
OUTPUT_SIZE = (1280, 800)
# Two bundled themes with clearly different backgrounds, the first dark, the second light.
THEME = "Dracula"
CHANGED_THEME = "Builtin Light"
# A cursor color no theme background or chrome draws, which marks the terminal's frame as on screen.
CURSOR = (0xFF, 0x00, 0xFF)
DARKER_5 = 10  # the Settings tint slider at Darker 5: a 0.30 black wash
TOLERANCE = 2  # per channel, for the compositor's rounding
LOG_FILTER = "info,ngterm::surface=debug"
DREW = "terminal drew its first frame"
POLL = 0.1


Rgb = tuple[int, int, int]
Box = tuple[int, int, int, int]  # left, top, right, bottom (exclusive)


class ProbeError(Exception):
    pass


# -- screenshots


@dataclass(frozen=True)
class Image:
    width: int
    height: int
    channels: int
    rows: list[bytes]

    def pixel(self, x: int, y: int) -> Rgb:
        row = self.rows[y]
        at = x * self.channels
        return (row[at], row[at + 1], row[at + 2])


def _paeth(a: int, b: int, c: int) -> int:
    p = a + b - c
    pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
    if pa <= pb and pa <= pc:
        return a
    return b if pb <= pc else c


def _unfilter(kind: int, line: bytes, prior: bytes, bpp: int) -> bytes:
    if kind == 0:
        return line
    if kind == 2:
        return bytes((x + b) & 0xFF for x, b in zip(line, prior))
    out = bytearray(line)
    for i in range(len(out)):
        a = out[i - bpp] if i >= bpp else 0
        if kind == 1:
            out[i] = (out[i] + a) & 0xFF
        elif kind == 3:
            out[i] = (out[i] + ((a + prior[i]) >> 1)) & 0xFF
        elif kind == 4:
            c = prior[i - bpp] if i >= bpp else 0
            out[i] = (out[i] + _paeth(a, prior[i], c)) & 0xFF
        else:
            raise ProbeError(f"PNG filter type {kind} is not one of 0-4")
    return bytes(out)


def read_png(data: bytes) -> Image:
    """An 8-bit, non-interlaced RGB or RGBA PNG, the kind weston-screenshooter writes."""
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ProbeError("not a PNG")
    at, header, compressed = 8, None, bytearray()
    while at < len(data):
        (length,) = struct.unpack(">I", data[at : at + 4])
        kind, body = data[at + 4 : at + 8], data[at + 8 : at + 8 + length]
        at += 12 + length
        if kind == b"IHDR":
            header = struct.unpack(">IIBBBBB", body)
        elif kind == b"IDAT":
            compressed += body
        elif kind == b"IEND":
            break
    if header is None:
        raise ProbeError("a PNG without IHDR")
    width, height, depth, color, _, _, interlace = header
    channels = {2: 3, 6: 4}.get(color)
    if depth != 8 or channels is None or interlace:
        raise ProbeError(f"unsupported PNG: depth {depth}, color type {color}, interlace {interlace}")
    raw = zlib.decompress(bytes(compressed))
    stride = width * channels
    rows, prior = [], bytes(stride)
    for y in range(height):
        start = y * (stride + 1)
        prior = _unfilter(raw[start], raw[start + 1 : start + 1 + stride], prior, channels)
        rows.append(prior)
    return Image(width, height, channels, rows)


def screenshot(env: Mapping[str, str], shots: Path, keep: Path | None, label: str) -> Image:
    """The instance's output, through weston's capture protocol."""
    program = shutil.which("weston-screenshooter")
    if program is None:
        raise ProbeError("no weston-screenshooter on PATH (it ships with weston)")
    shots.mkdir(exist_ok=True)
    before = set(shots.iterdir())
    run_env = {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "XDG_RUNTIME_DIR": env["XDG_RUNTIME_DIR"],
        "WAYLAND_DISPLAY": env["WAYLAND_DISPLAY"],
    }
    done = subprocess.run([program], cwd=shots, env=run_env, capture_output=True, text=True, timeout=30)
    taken = sorted(set(shots.iterdir()) - before)
    if done.returncode != 0 or "error" in done.stderr.lower() or len(taken) != 1:
        raise ProbeError(f"weston-screenshooter: status {done.returncode}, {done.stderr.strip()!r}, files {taken}")
    data = taken[0].read_bytes()
    if keep is not None:
        keep.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(taken[0], keep / f"{label}.png")
    taken[0].unlink()
    return read_png(data)


# -- sampling


def close(a: Rgb, b: Rgb, tolerance: int = TOLERANCE) -> bool:
    return all(abs(x - y) <= tolerance for x, y in zip(a, b))


def hex_rgb(rgb: Rgb) -> str:
    return "#{:02x}{:02x}{:02x}".format(*rgb)


def dominant(image: Image, box: Box, step: int = 3) -> tuple[Rgb, float]:
    """The most common color in `box` and its share of the sampled pixels."""
    left, top, right, bottom = box
    counts = Counter(image.pixel(x, y) for y in range(top, bottom, step) for x in range(left, right, step))
    if not counts:
        raise ProbeError(f"empty box {box}")
    color, count = counts.most_common(1)[0]
    return color, count / sum(counts.values())


def contains(image: Image, box: Box, color: Rgb) -> bool:
    left, top, right, bottom = box
    return any(close(image.pixel(x, y), color) for y in range(top, bottom) for x in range(left, right))


def washed(color: Rgb, white: bool, opacity: float) -> Rgb:
    """`color` under a white or black wash at `opacity` (`ngterm_core::chrome::Rgb::washed`)."""
    added = 255 * opacity if white else 0
    return tuple(min(255, max(0, round(channel * (1 - opacity) + added))) for channel in color)  # type: ignore[return-value]


def theme_background(themes: Path, name: str) -> Rgb:
    text = (themes / name).read_text()
    found = re.search(r"^\s*background\s*=\s*#?([0-9a-fA-F]{6})\s*$", text, re.MULTILINE)
    if found is None:
        raise ProbeError(f"theme {name!r} sets no background")
    digits = found.group(1)
    return (int(digits[0:2], 16), int(digits[2:4], 16), int(digits[4:6], 16))


@dataclass(frozen=True)
class Layout:
    """Where the regions sit in a window maximized over the whole output, the sidebar `sidebar` points wide."""

    width: int
    height: int
    sidebar: int

    @property
    def title(self) -> Box:
        # the middle of the bar: its title starts at the leading edge and its buttons sit at the trailing one
        return (self.width * 2 // 5, 2, self.width * 3 // 5, 12)

    @property
    def sidebar_body(self) -> Box:
        # below the rows of the launch's one session, above the footer
        return (8, self.height // 2, self.sidebar - 8, self.height * 4 // 5)

    @property
    def terminal(self) -> Box:
        # below the prompt line
        return (self.sidebar + 40, self.height // 2, self.width - 40, self.height * 4 // 5)

    @property
    def prompt(self) -> Box:
        # the deck's top-left corner, where the cursor sits after the shell's prompt
        return (self.sidebar + 1, 0, min(self.width, self.sidebar + 400), self.height // 4)


def regions(image: Image, layout: Layout) -> dict[str, Rgb]:
    sampled = {}
    for name in ("title", "sidebar_body", "terminal"):
        color, share = dominant(image, getattr(layout, name))
        if share < 0.9:
            raise ProbeError(f"the {name} box is not one color: {hex_rgb(color)} covers {share:.0%}")
        sampled[name] = color
    return sampled


# -- the app


def wait(what: str, timeout: float, attempt: Callable[[], Any]) -> Any:
    """Poll `attempt` until it returns a truthy value; the timeout names `what` and the last value."""
    deadline = time.monotonic() + timeout
    while True:
        value = attempt()
        if value:
            return value
        if time.monotonic() >= deadline:
            raise ProbeError(f"{what} within {timeout}s")
        time.sleep(POLL)


def ok(response: Any) -> dict[str, Any]:
    if not response.ok:
        raise ProbeError(f"request failed: {response.raw}")
    return response.result or {}


class Session:
    """One launched instance, maximized, with its screenshots."""

    def __init__(self, instance: Any, keep: Path | None, label: str, timeout: float) -> None:
        self.instance = instance
        self.keep = keep
        self.label = label
        self.timeout = timeout
        self.shots = instance.root / "shots"
        self.count = 0

    def shot(self) -> Image:
        self.count += 1
        return screenshot(self.instance.env, self.shots, self.keep, f"{self.label}-{self.count}")

    def maximize(self) -> Layout:
        (window,) = ok(self.instance.request("window.list"))["windows"]
        ok(self.instance.request("window.zoom", window["id"]))

        def zoomed() -> bool:
            listed = ok(self.instance.request("window.list"))["windows"]
            return any(node["id"] == window["id"] and node.get("zoomed") is True for node in listed)

        wait("the window did not read back zoomed", self.timeout, zoomed)
        tree = ok(self.instance.request("tree"))["tree"]
        if tree.get("sidebarVisible") is not True:
            raise ProbeError(f"the sidebar is not visible: {tree.get('sidebarVisible')!r}")
        return Layout(*OUTPUT_SIZE, sidebar=round(tree["sidebarWidth"]))

    def first_frame(self, layout: Layout) -> Image:
        """A screenshot showing the terminal's first frame: drawn (its log line), then on screen (its cursor)."""
        wait("the terminal drew no frame", self.timeout, lambda: DREW in self.instance.log.read_text(errors="replace"))

        def cursor_shown() -> Image | None:
            image = self.shot()
            return image if contains(image, layout.prompt, CURSOR) else None

        return wait("no cursor on screen", self.timeout, cursor_shown)

    def showing(self, layout: Layout, background: Rgb) -> Image:
        """A screenshot whose terminal shows `background`."""

        def shown() -> Image | None:
            image = self.shot()
            return image if close(dominant(image, layout.terminal)[0], background) else None

        return wait(f"the terminal never showed {hex_rgb(background)}", self.timeout, shown)


def same_as(sampled: Mapping[str, Rgb], expected: Mapping[str, Rgb]) -> dict[str, Any]:
    result: dict[str, Any] = {name: hex_rgb(color) for name, color in sampled.items()}
    result["expected"] = {name: hex_rgb(color) for name, color in expected.items()}
    result["ok"] = all(close(sampled[name], color) for name, color in expected.items())
    return result


def find_lavapipe(environ: Mapping[str, str], script: Path = FETCH_LAVAPIPE) -> tuple[str | None, str]:
    """The lavapipe ICD for a launch: the harness's variable, else what the repository's script finds without
    fetching; otherwise None and why."""
    if icd := environ.get(LAVAPIPE_ICD_VARIABLE):
        return icd, ""
    if not script.is_file():
        return None, f"no {script.name} in this tree and no ${LAVAPIPE_ICD_VARIABLE}"
    try:
        found = subprocess.run([str(script), "--print-icd"], capture_output=True, text=True, check=False, timeout=60)
    except (OSError, subprocess.TimeoutExpired) as error:
        return None, f"{script}: {error}"
    if found.returncode == 0 and (icd := found.stdout.strip()):
        return icd, ""
    return None, found.stderr.strip() or f"{script} --print-icd exited with {found.returncode}"


def make_adapter(bin: Path) -> Any:
    from agterm_conformance.impls.rust import RustAdapter

    class ChromeAdapter(RustAdapter):
        compositor_args = ("--debug", f"--width={OUTPUT_SIZE[0]}", f"--height={OUTPUT_SIZE[1]}")

        def platform_env(self, env: dict[str, str]) -> None:
            super().platform_env(env)
            env[RustAdapter.variable("LOG")] = LOG_FILTER

        def ghostty_config(self) -> str:
            return super().ghostty_config() + f"cursor-color = {hex_rgb(CURSOR)}\ncursor-style-blink = false\n"

    return ChromeAdapter(bin)


def run_launch(adapter: Any, settings: dict[str, Any], label: str, keep: Path | None, timeout: float,
               steps: Callable[[Session, Layout], dict[str, Any]]) -> dict[str, Any]:
    instance = adapter.launch(settings=settings)
    try:
        session = Session(instance, keep, label, timeout)
        return steps(session, session.maximize())
    finally:
        adapter.stop(instance)


def probe(bin: Path, timeout: float, keep: Path | None) -> dict[str, Any]:
    themes = bin.parent.parent / "share" / "ghostty" / "themes"
    report: dict[str, Any] = {"bin": str(bin), "checks": {}}
    icd, problem = find_lavapipe(os.environ)
    if icd is None:
        return {**report, "ok": False, "error": f"lavapipe: {problem}"}
    os.environ[LAVAPIPE_ICD_VARIABLE] = icd
    if not bin.is_file():
        return {**report, "ok": False, "error": f"no ngterm at {bin}"}
    try:
        background = theme_background(themes, THEME)
        changed = theme_background(themes, CHANGED_THEME)
    except (OSError, ProbeError) as error:
        return {**report, "ok": False, "error": f"themes: {error}"}
    try:
        adapter = make_adapter(bin)
    except ProbeError as error:
        return {**report, "ok": False, "error": str(error)}
    checks = report["checks"]

    def themed(session: Session, layout: Layout) -> dict[str, Any]:
        everywhere = dict.fromkeys(("title", "sidebar_body", "terminal"), background)
        checks["theme"] = same_as(regions(session.first_frame(layout), layout), everywhere)
        ok(session.instance.request("theme.set", name=CHANGED_THEME))
        everywhere = dict.fromkeys(everywhere, changed)
        checks["theme-change"] = same_as(regions(session.showing(layout, changed), layout), everywhere)
        return checks

    def tinted(session: Session, layout: Layout) -> dict[str, Any]:
        expected = {"title": background, "terminal": background, "sidebar_body": washed(background, False, 0.30)}
        result = same_as(regions(session.first_frame(layout), layout), expected)
        # the wash must be visible, not lost in the tolerance
        result["ok"] = result["ok"] and not close(expected["sidebar_body"], background, 2 * TOLERANCE)
        checks["tint"] = result
        return checks

    try:
        run_launch(adapter, {"theme": THEME}, "theme", keep, timeout, themed)
        tint = {"theme": THEME, "toolbarMode": "normal", "sidebarBackgroundShift": DARKER_5}
        run_launch(adapter, tint, "tint", keep, timeout, tinted)
    except Exception as error:  # noqa: BLE001 - the report names whatever stopped the probe
        return {**report, "ok": False, "error": f"{type(error).__name__}: {error}"}
    report["ok"] = len(checks) == 3 and all(check["ok"] for check in checks.values())
    return report


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--bin", type=Path, default=DEFAULT_BIN, help="the staged app binary (default: the stage's)")
    parser.add_argument("--timeout", type=float, default=30.0, help="seconds each wait may take")
    parser.add_argument("--keep", type=Path, help="a directory to save the screenshots in")
    args = parser.parse_args(argv)
    report = probe(args.bin.resolve(), args.timeout, args.keep)
    print(json.dumps(report, indent=2))
    return 0 if report.get("ok") else 1


if __name__ == "__main__":
    sys.exit(main())
