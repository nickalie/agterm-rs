#!/usr/bin/env python3
"""Launch the staged Rust build under a private headless weston and session bus, the isolation of the Linux runs,
and report whether its libghostty surface was created and drew a frame: a `version` answer alone does not show it,
since the app keeps serving when its GtkGLArea gets no OpenGL context.

  python conformance/tools/ghostty_probe.py [--bin PATH] [--timeout SECONDS] [--arg=ARG ...]

Each --arg is appended to the app's command line (write `--arg=--flag` for one starting with a dash): agterm hands its
whole argv to libghostty as upstream does, and a launch with arguments of its own must still start and serve.

--bin defaults to the stage run-rust-linux.sh builds (target/conformance-stage/bin/agterm). The instance logs the
terminal surface at debug level (AGTERM_LOG); this reads its log. Prints a JSON report; exits 0 when the first frame
was drawn.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import time
from pathlib import Path
from collections.abc import Sequence
from typing import Any

REPO = Path(__file__).resolve().parents[2]
DEFAULT_BIN = REPO / "target" / "conformance-stage" / "bin" / "agterm"
# The surface's lines, at debug in the binary's `agterm::surface` module; the rest of the app stays at info.
LOG_FILTER = "info,agterm::surface=debug"
CREATED = "terminal surface created"
DREW = "terminal drew its first frame"
ANSI = re.compile(r"\x1b\[[0-9;]*m")
FAILURES = ("the terminal's GtkGLArea has no OpenGL context", "could not create the terminal surface")


def plain(text: str) -> str:
    """`text` without the colour codes the app's log writes into a file too."""
    return ANSI.sub("", text)


def read_log(text: str) -> dict[str, Any]:
    """What the instance log says about its surface: created, drew a frame, and the first GL or creation failure."""
    text = plain(text)
    error = next((line.strip() for line in text.splitlines() if any(f in line for f in FAILURES)), None)
    return {"created": CREATED in text, "drew": DREW in text, "error": error}


def probe(bin: Path, timeout: float, args: Sequence[str] = ()) -> dict[str, Any]:
    from agterm_conformance.impls.rust import RustAdapter

    class LoggingAdapter(RustAdapter):
        def platform_env(self, env: dict[str, str]) -> None:
            super().platform_env(env)
            env["AGTERM_LOG"] = LOG_FILTER

        def instance_executable(self, root: Path) -> list[str]:
            return [*super().instance_executable(root), *args]

    if not bin.is_file():
        return {"ok": False, "error": f"no agterm at {bin}"}
    adapter = LoggingAdapter(bin)
    report: dict[str, Any] = {"bin": str(bin)}
    if args:
        report["args"] = list(args)
    try:
        instance = adapter.launch()
    except Exception as error:  # noqa: BLE001 - the report names whatever stopped the probe
        return {**report, "ok": False, "error": f"launch: {type(error).__name__}: {error}"}
    try:
        deadline = time.monotonic() + timeout
        while True:
            seen = read_log(instance.log.read_text(errors="replace"))
            exited = instance.process.poll()
            if seen["drew"] or seen["error"] or exited is not None or time.monotonic() >= deadline:
                break
            time.sleep(0.2)
        report.update(seen, ok=bool(seen["drew"]) and seen["error"] is None)
        if exited is not None:
            report["exit"] = exited
        if not report["ok"]:
            if not seen["error"]:
                report["error"] = f"no first frame within {timeout}s" if exited is None else "the app exited"
            report["log"] = plain(instance.log_tail(40)).splitlines()
    finally:
        try:
            adapter.stop(instance)
        except Exception as error:  # noqa: BLE001 - a failed stop fails the probe too
            report.update(ok=False, cleanup=f"{type(error).__name__}: {error}")
    return report


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--bin", type=Path, default=DEFAULT_BIN, help="the staged agterm binary")
    parser.add_argument("--timeout", type=float, default=30.0, help="seconds the first frame may take")
    parser.add_argument("--arg", action="append", default=[], help="an argument appended to the app's command line")
    args = parser.parse_args(argv)
    report = probe(args.bin.resolve(), args.timeout, args.arg)
    print(json.dumps(report, indent=2))
    return 0 if report.get("ok") else 1


if __name__ == "__main__":
    sys.exit(main())
