"""Temporary Task 19 probe: on a failed or errored test, keep a screenshot, the agterm processes and lsappinfo."""

from __future__ import annotations

import itertools
import subprocess
from pathlib import Path

import pytest

OUT = Path("probe-out")
_counter = itertools.count()


@pytest.hookimpl(hookwrapper=True)
def pytest_runtest_makereport(item, call):
    outcome = yield
    report = outcome.get_result()
    if report.failed:
        OUT.mkdir(exist_ok=True)
        stem = OUT / f"{next(_counter):02d}-{item.name[:60]}-{report.when}"
        subprocess.run(["screencapture", "-x", f"{stem}.png"], check=False)
        ps = subprocess.run(
            ["ps", "-axww", "-o", "pid=,ppid=,pgid=,stat=,lstart=,command="], capture_output=True, text=True, check=False
        ).stdout
        apps = subprocess.run(["lsappinfo", "list"], capture_output=True, text=True, check=False).stdout
        stem.with_suffix(".txt").write_text(
            "\n".join(line[:300] for line in ps.splitlines() if "agterm" in line or "zmx" in line)
            + "\n\n"
            + apps
        )


def pytest_configure(config):
    from agterm_conformance.impls import base

    original = base.wait_for_socket

    def diagnosed(instance, timeout):
        try:
            return original(instance, timeout)
        except base.LaunchError:
            OUT.mkdir(exist_ok=True)
            stem = OUT / f"launch-{next(_counter):02d}-{instance.pid}"
            subprocess.run(["screencapture", "-x", f"{stem}.png"], check=False)
            subprocess.run(["sample", str(instance.pid), "2", "-file", f"{stem}.sample.txt"], check=False)
            info = subprocess.run(
                ["lsappinfo", "info", "-app", f"pid={instance.pid}"], capture_output=True, text=True, check=False
            ).stdout
            ps = subprocess.run(
                ["ps", "-axww", "-o", "pid=,ppid=,stat=,command="], capture_output=True, text=True, check=False
            ).stdout
            tree = "\n".join(line[:300] for line in ps.splitlines() if "agterm" in line or "zmx" in line)
            files = subprocess.run(["find", str(instance.root)], capture_output=True, text=True, check=False).stdout
            stem.with_suffix(".txt").write_text(f"{info}\n\n{tree}\n\n{files}")
            raise

    base.wait_for_socket = diagnosed
