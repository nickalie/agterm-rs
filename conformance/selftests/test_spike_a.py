import importlib.util
import re
import sys
from pathlib import Path

import pytest
from agterm_conformance.impls.base import GHOSTTY_SHELL_CONFIG
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
