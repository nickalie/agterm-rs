#!/usr/bin/env python3
"""Spike A: launch a reference through its adapter, read `version` and `tree`, and round-trip a line through
the pane's shell with `session.type` + `session.text`.

`--mechanism` decides how the instance is told to run /bin/sh, so one run per mechanism shows which one the
reference honours: `both` is what the harness ships, `config` keeps only `command = /bin/sh` in the scoped
ghostty.conf (SHELL points at a decoy), `env` keeps only SHELL=/bin/sh, `none` keeps neither.
Prints a JSON report; exits 0 when the probe line came back from /bin/sh.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path
from typing import Any

from agterm_conformance.impls.base import GHOSTTY_SHELL_CONFIG, Adapter, Instance
from agterm_conformance.impls.swift_linux import SwiftLinuxAdapter
from agterm_conformance.impls.swift_macos import LAUNCH_MODES, SwiftMacosAdapter
from agterm_conformance.match import active_session_id, assert_ok
from agterm_conformance.wait import wait_text

MECHANISMS = ("both", "config", "env", "none")
# Typed output, not the echoed input: the echo still holds the `%s` placeholders the pattern excludes.
PROBE = (
    'printf \'agt-shell|%s|%s|%s|%s\\n\' "$0" "$(ps -o comm= -p $$)" "$(ps -o comm= -p $PPID)"'
    ' "${BASH_VERSION:-}${ZSH_VERSION:-}"\n'
)
PROBE_LINE = r"(?m)^agt-shell\|([^|%\n]*)\|([^|%\n]*)\|([^|%\n]*)\|([^|%\n]*)$"
DECOY_SHELL = "/bin/zsh" if sys.platform == "darwin" else "/bin/bash"


def with_mechanism(adapter: Adapter, mechanism: str) -> Adapter:
    """The same adapter, telling the instance about /bin/sh only through `mechanism`."""
    base = type(adapter)

    class Probe(base):  # type: ignore[valid-type, misc]
        def build_env(self, root: Path, app_id: str | None = None) -> dict[str, str]:
            env = super().build_env(root, app_id)
            env["SHELL"] = "/bin/sh" if mechanism in ("both", "env") else DECOY_SHELL
            return env

        def ghostty_config(self) -> str:
            return GHOSTTY_SHELL_CONFIG if mechanism in ("both", "config") else ""

    probe = Probe.__new__(Probe)
    probe.__dict__.update(adapter.__dict__)
    return probe


def make(args: argparse.Namespace) -> Adapter:
    if args.impl == "swift-linux":
        return SwiftLinuxAdapter(args.bin, args.ctl)
    return SwiftMacosAdapter(args.bin, args.ctl, launch_mode=args.launch_mode, enable_zmx=not args.no_zmx)


def probe(instance: Instance, timeout: float) -> dict[str, Any]:
    tree = assert_ok(instance.request("tree"))
    session = active_session_id(tree)
    typed = instance.request("session.type", session, text=PROBE)
    report: dict[str, Any] = {"session": session, "type": typed.raw}
    if not typed.ok:
        return report
    found = wait_text(instance, session, PROBE_LINE, timeout)
    argv0, comm, parent, shell_version = (part.strip() for part in found.groups())
    report.update(argv0=argv0, comm=comm, parent=parent, shellVersion=shell_version)
    # macOS `ps -o comm` prints the executable path, Linux its name; a login shell's argv0 starts with `-`.
    report["isSh"] = os.path.basename(argv0.lstrip("-")) == "sh" and os.path.basename(comm) == "sh"
    report["windows"] = assert_ok(instance.request("window.list"))
    return report


def run(adapter: Adapter, mechanism: str, timeout: float) -> dict[str, Any]:
    report: dict[str, Any] = {
        "impl": adapter.name,
        "mechanism": mechanism,
        "launchMode": getattr(adapter, "launch_mode", None),
    }
    adapter = with_mechanism(adapter, mechanism)
    instance = adapter.launch()
    report.update(pid=instance.pid, version=instance.version, env={k: instance.env.get(k) for k in ("SHELL", "HOME")})
    try:
        report.update(probe(instance, timeout))
    except AssertionError as error:
        report["error"] = str(error)
    finally:
        report["logTail"] = instance.log_tail(40)
        adapter.stop(instance)
    return report


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--impl", choices=("swift-linux", "swift-macos"), required=True)
    parser.add_argument("--bin", type=Path, required=True)
    parser.add_argument("--ctl", type=Path)
    parser.add_argument("--mechanism", choices=MECHANISMS, default="both")
    parser.add_argument("--launch-mode", choices=LAUNCH_MODES, default="uitest", help="swift-macos only")
    parser.add_argument("--no-zmx", action="store_true", help="swift-macos uitest mode without zmx")
    parser.add_argument("--timeout", type=float, default=20.0)
    parser.add_argument("--out", type=Path, help="also write the JSON report here")
    args = parser.parse_args(argv)
    report = run(make(args), args.mechanism, args.timeout)
    text = json.dumps(report, indent=2, sort_keys=True, default=repr)
    print(text)
    if args.out is not None:
        args.out.write_text(text + "\n")
    return 0 if report.get("isSh") else 1


if __name__ == "__main__":
    sys.exit(main())
