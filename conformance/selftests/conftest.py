import shutil
import sys
from collections.abc import Iterator
from pathlib import Path

import pytest
from agterm_conformance.impls import base

FAKE_APP = Path(__file__).with_name("fake_app.py")


@pytest.fixture
def roots(monkeypatch: pytest.MonkeyPatch) -> Iterator[list[Path]]:
    """Every temp root a launch made, removed afterwards whatever the test left."""
    created: list[Path] = []
    make_root = base.make_root

    def recording() -> Path:
        created.append(make_root())
        return created[-1]

    monkeypatch.setattr(base, "make_root", recording)
    yield created
    for root in created:
        shutil.rmtree(root, ignore_errors=True)


@pytest.fixture
def fake_bin(tmp_path: Path) -> Path:
    launcher = tmp_path / "install" / "bin" / "agterm-linux"
    launcher.parent.mkdir(parents=True)
    launcher.write_text(f"#!{sys.executable}\n" + FAKE_APP.read_text())
    launcher.chmod(0o755)
    return launcher
