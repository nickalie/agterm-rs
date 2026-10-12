import shutil
import sys
from collections.abc import Iterator
from pathlib import Path

import pytest
from agterm_conformance.impls import base

FAKE_APP = Path(__file__).with_name("fake_app.py")
FAKE_COMPOSITOR = Path(__file__).with_name("fake_compositor.py")
REAL_COMPOSITOR_PROGRAM = base.compositor_program


@pytest.fixture(scope="session")
def fake_compositor_bin(tmp_path_factory: pytest.TempPathFactory) -> Path:
    program = tmp_path_factory.mktemp("compositor") / "fake-weston"
    program.write_text(f"#!{sys.executable}\n" + FAKE_COMPOSITOR.read_text())
    program.chmod(0o755)
    return program


@pytest.fixture(autouse=True)
def fake_compositor(fake_compositor_bin: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """A headless launch starts the fake weston, whatever display the developer running the self-tests chose."""
    monkeypatch.delenv(base.DISPLAY_MODE_VARIABLE, raising=False)
    monkeypatch.setattr(base, "compositor_program", lambda: str(fake_compositor_bin))
    return fake_compositor_bin


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


@pytest.fixture(autouse=True)
def short_leftover_grace(monkeypatch: pytest.MonkeyPatch) -> None:
    """Fake daemons never exit on their own; the grace a real one gets would only slow every leftover test."""
    monkeypatch.setattr(base, "LEFTOVER_GRACE", 0.1)


@pytest.fixture
def real_compositor(fake_compositor: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(base, "compositor_program", REAL_COMPOSITOR_PROGRAM)
