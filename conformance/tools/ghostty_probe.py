#!/usr/bin/env python3
"""Launch the staged Rust build under a private headless weston and session bus, the isolation of the Linux runs,
and report whether its terminal surface was created and drew a frame: a `version` answer alone does not show it,
since the app keeps serving when its terminal cannot render (the GTK build's GtkGLArea without an OpenGL context, the
GPUI build's terminal element without a frame).

  python conformance/tools/ghostty_probe.py [--frontend gtk|gpui] [--bin PATH] [--timeout SECONDS] [--arg=ARG ...]

Each --arg is appended to the app's command line (write `--arg=--flag` for one starting with a dash): agterm hands its
whole argv to libghostty as upstream does, and a launch with arguments of its own must still start and serve.

--frontend gpui probes the GPUI build (bin/agterm-gpui) on lavapipe only: $CONFORMANCE_LAVAPIPE_ICD, else the ICD
`scripts/fetch-lavapipe.sh --print-icd` finds (nothing is fetched here), else the launch fails naming the script.
--bin defaults to the frontend's stage run-rust-linux.sh builds (target/conformance-stage/bin/agterm,
target/conformance-stage-gpui/bin/agterm-gpui). Both frontends log their terminal surface at debug level under the
target `agterm::surface` (AGTERM_LOG); this reads the instance's log. Prints a JSON report; exits 0 when the first
frame was drawn.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import time
from collections.abc import Sequence
from pathlib import Path
from typing import Any

REPO = Path(__file__).resolve().parents[2]
DEFAULT_BINS = {
    "gtk": REPO / "target" / "conformance-stage" / "bin" / "agterm",
    "gpui": REPO / "target" / "conformance-stage-gpui" / "bin" / "agterm-gpui",
}
DEFAULT_BIN = DEFAULT_BINS["gtk"]
FETCH_LAVAPIPE = REPO / "scripts" / "fetch-lavapipe.sh"
# The surface's lines, at debug under the `agterm::surface` target (the GTK binary's module, the GPUI build's explicit
# target); the rest of the app stays at info.
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


def lavapipe_from_script(script: Path = FETCH_LAVAPIPE) -> str | None:
    """The lavapipe ICD the repository's script finds without fetching anything, or None."""
    try:
        found = subprocess.run([str(script), "--print-icd"], capture_output=True, text=True, check=False, timeout=60)
    except (OSError, subprocess.TimeoutExpired):
        return None
    return (found.stdout.strip() or None) if found.returncode == 0 else None


def probe(bin: Path, timeout: float, args: Sequence[str] = (), frontend: str = "gtk") -> dict[str, Any]:
    from agterm_conformance.impls.base import LAVAPIPE_ICD_VARIABLE
    from agterm_conformance.impls.rust import GPUI, RustAdapter

    class LoggingAdapter(RustAdapter):
        def platform_env(self, env: dict[str, str]) -> None:
            super().platform_env(env)
            env["AGTERM_LOG"] = LOG_FILTER

        def instance_executable(self, root: Path) -> list[str]:
            return [*super().instance_executable(root), *args]

    if not bin.is_file():
        return {"ok": False, "error": f"no agterm at {bin}"}
    if frontend == GPUI and not os.environ.get(LAVAPIPE_ICD_VARIABLE) and (icd := lavapipe_from_script()):
        os.environ[LAVAPIPE_ICD_VARIABLE] = icd
    adapter = LoggingAdapter(bin, frontend=frontend)
    report: dict[str, Any] = {"bin": str(bin), "frontend": frontend}
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
    parser.add_argument("--frontend", choices=tuple(DEFAULT_BINS), default="gtk", help="the Rust build's frontend")
    parser.add_argument("--bin", type=Path, default=None, help="the staged app binary (default: the frontend's stage)")
    parser.add_argument("--timeout", type=float, default=30.0, help="seconds the first frame may take")
    parser.add_argument("--arg", action="append", default=[], help="an argument appended to the app's command line")
    args = parser.parse_args(argv)
    bin = args.bin if args.bin is not None else DEFAULT_BINS[args.frontend]
    report = probe(bin.resolve(), args.timeout, args.arg, args.frontend)
    print(json.dumps(report, indent=2))
    return 0 if report.get("ok") else 1


if __name__ == "__main__":
    sys.exit(main())
