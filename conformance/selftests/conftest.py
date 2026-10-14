import shutil
import sys
from collections.abc import Iterator
from pathlib import Path

import pytest
from agterm_conformance.impls import base

FAKE_APP = Path(__file__).with_name("fake_app.py")
FAKE_COMPOSITOR = Path(__file__).with_name("fake_compositor.py")
FAKE_SESSION_BUS = Path(__file__).with_name("fake_session_bus.py")
REAL_COMPOSITOR_PROGRAM = base.compositor_program
REAL_SESSION_BUS_PROGRAM = base.session_bus_program


@pytest.fixture(scope="session")
def fake_compositor_bin(tmp_path_factory: pytest.TempPathFactory) -> Path:
    program = tmp_path_factory.mktemp("compositor") / "fake-weston"
    program.write_text(f"#!{sys.executable}\n" + FAKE_COMPOSITOR.read_text())
    program.chmod(0o755)
    return program


@pytest.fixture(scope="session")
def fake_session_bus_bin(tmp_path_factory: pytest.TempPathFactory) -> Path:
    # Named like the real one: the leftover check matches dbus-daemon by name.
    program = tmp_path_factory.mktemp("session-bus") / "dbus-daemon"
    program.write_text(f"#!{sys.executable}\n" + FAKE_SESSION_BUS.read_text())
    program.chmod(0o755)
    return program


@pytest.fixture(autouse=True)
def fake_compositor(fake_compositor_bin: Path, fake_session_bus_bin: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """A headless launch starts the fake weston and bus, whatever display the developer running the self-tests chose."""
    monkeypatch.delenv(base.DISPLAY_MODE_VARIABLE, raising=False)
    monkeypatch.setattr(base, "compositor_program", lambda: str(fake_compositor_bin))
    monkeypatch.setattr(base, "session_bus_program", lambda: str(fake_session_bus_bin))
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


@pytest.fixture(autouse=True)
def short_abort_grace(monkeypatch: pytest.MonkeyPatch) -> None:
    """A fake app ignoring SIGABRT is killed only once the grace for writing a core runs out."""
    monkeypatch.setattr(base, "ABORT_GRACE", 0.5)


@pytest.fixture
def real_compositor(fake_compositor: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(base, "compositor_program", REAL_COMPOSITOR_PROGRAM)
    monkeypatch.setattr(base, "session_bus_program", REAL_SESSION_BUS_PROGRAM)
