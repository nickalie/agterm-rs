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
