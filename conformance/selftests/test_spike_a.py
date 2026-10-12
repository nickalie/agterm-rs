import importlib.util
import json
import re
import sys
from pathlib import Path
from types import SimpleNamespace

import pytest
from agterm_conformance.impls.base import GHOSTTY_SHELL_CONFIG, LaunchError, LeftoverProcesses, StopError
from agterm_conformance.impls.swift_linux import SwiftLinuxAdapter
from agterm_conformance.impls.swift_macos import SwiftMacosAdapter

spec = importlib.util.spec_from_file_location(
    "spike_a", Path(__file__).resolve().parent.parent / "tools" / "spike_a.py"
)
assert spec is not None and spec.loader is not None
spike = importlib.util.module_from_spec(spec)
sys.modules["spike_a"] = spike
spec.loader.exec_module(spike)


@pytest.mark.parametrize(
    ("mechanism", "shell", "config"),
    [
        ("both", "/bin/sh", GHOSTTY_SHELL_CONFIG),
        ("config", spike.DECOY_SHELL, GHOSTTY_SHELL_CONFIG),
        ("env", "/bin/sh", ""),
        ("none", spike.DECOY_SHELL, ""),
    ],
)
def test_mechanism_controls_shell_env_and_ghostty_config(
    tmp_path: Path, mechanism: str, shell: str, config: str
) -> None:
    adapter = spike.with_mechanism(SwiftLinuxAdapter(tmp_path / "agterm-linux"), mechanism)
    assert isinstance(adapter, SwiftLinuxAdapter) and adapter.bin == tmp_path / "agterm-linux"
    env = adapter.build_env(tmp_path)
    assert env["SHELL"] == shell and env["AGTERM_APP_ID"]
    assert adapter.ghostty_config() == config


def test_mechanism_keeps_the_macos_launch_mode(tmp_path: Path) -> None:
    adapter = spike.with_mechanism(SwiftMacosAdapter(tmp_path / "agterm.app", launch_mode="plain"), "env")
    assert adapter.launch_mode == "plain"
    assert "AGTERM_UITEST_FORCE_SIDEBAR_VISIBLE" not in adapter.build_env(tmp_path)


def test_probe_line_matches_shell_output_not_the_echoed_input() -> None:
    echoed = "$ " + spike.PROBE.rstrip("\n")
    assert not re.search(spike.PROBE_LINE, echoed)
    found = re.search(spike.PROBE_LINE, f"{echoed}\nagt-shell|/bin/sh|sh|agterm-linux.bi|5.3.20(1)-release\n$ ")
    assert found is not None
    assert found.groups() == ("/bin/sh", "sh", "agterm-linux.bi", "5.3.20(1)-release")


class FakeAdapter:
    name = "fake"

    def __init__(self, launch_error: Exception | None = None, stop_error: Exception | None = None) -> None:
        self.launch_error, self.stop_error = launch_error, stop_error

    def build_env(self, root: Path, app_id: str | None = None) -> dict[str, str]:
        return {}

    def launch(self) -> SimpleNamespace:
        if self.launch_error is not None:
            raise self.launch_error
        return SimpleNamespace(pid=4242, version={"version": "0.36.0"}, env={}, log_tail=lambda lines: "log")

    def stop(self, instance: SimpleNamespace) -> None:
        if self.stop_error is not None:
            raise self.stop_error


@pytest.mark.parametrize(
    ("adapter", "field", "message", "probed"),
    [
        (FakeAdapter(launch_error=LaunchError("no socket")), "launchError", "no socket", False),
        (FakeAdapter(stop_error=LeftoverProcesses("zmx left")), "stopError", "zmx left", True),
        (FakeAdapter(stop_error=StopError("ignored SIGTERM")), "stopError", "ignored SIGTERM", True),
    ],
    ids=["launch", "leftovers", "sigterm"],
)
def test_launch_and_stop_failures_still_write_the_report(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, adapter: FakeAdapter, field: str, message: str, probed: bool
) -> None:
    monkeypatch.setattr(spike, "make", lambda args: adapter)
    monkeypatch.setattr(spike, "probe", lambda instance, timeout: {"argv0": "sh", "isSh": True})
    out = tmp_path / "report.json"
    assert spike.main(["--impl", "swift-linux", "--bin", str(tmp_path / "agterm-linux"), "--out", str(out)]) == 1
    report = json.loads(out.read_text())
    assert report[field] == message
    assert ("argv0" in report) == probed and ("logTail" in report) == probed


def test_a_clean_stop_after_a_sh_probe_exits_zero(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(spike, "make", lambda args: FakeAdapter())
    monkeypatch.setattr(spike, "probe", lambda instance, timeout: {"isSh": True})
    assert spike.main(["--impl", "swift-linux", "--bin", str(tmp_path / "agterm-linux")]) == 0
