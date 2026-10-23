import json
import shutil
import sys
from collections.abc import Iterator
from pathlib import Path

import pytest
from agterm_conformance import impls, plugin
from agterm_conformance.impls import base
from upstream_adapter import UpstreamVocabularyAdapter

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
def upstream_impl(monkeypatch: pytest.MonkeyPatch) -> str:
    """`--impl upstream` for the in-process suite runs a self-test starts: the self-test double in upstream's
    vocabulary (upstream_adapter.py), with its static profile, registered beside the real adapters."""
    monkeypatch.setattr(plugin, "IMPLS", (*plugin.IMPLS, UpstreamVocabularyAdapter.name))
    monkeypatch.setitem(impls.ADAPTERS, UpstreamVocabularyAdapter.name, UpstreamVocabularyAdapter)
    return UpstreamVocabularyAdapter.name


@pytest.fixture
def fake_bin(tmp_path: Path) -> Path:
    launcher = tmp_path / "install" / "bin" / "fake-app"
    launcher.parent.mkdir(parents=True)
    launcher.write_text(f"#!{sys.executable}\n" + FAKE_APP.read_text())
    launcher.chmod(0o755)
    return launcher


@pytest.fixture
def make_icd(tmp_path: Path):
    """Writes an ICD manifest into a directory, next to the stand-in driver it names by absolute path."""

    def write(directory: Path | None = None, name: str = "lvp_icd.json", library: str = "libvulkan_lvp.so") -> Path:
        directory = directory or tmp_path / "lavapipe"
        (directory / "lib").mkdir(parents=True, exist_ok=True)
        (directory / "lib" / library).write_text("stand-in driver")
        manifest = directory / name
        manifest.write_text(json.dumps({"ICD": {"library_path": str(directory / "lib" / library)}}))
        return manifest

    return write


@pytest.fixture(scope="session")
def stand_in_lavapipe_icd(tmp_path_factory: pytest.TempPathFactory) -> Path:
    """A lavapipe ICD manifest beside the stand-in driver it names, outside every test's own tmp_path."""
    directory = tmp_path_factory.mktemp("stand-in-lavapipe")
    (directory / "lib").mkdir()
    (directory / "lib" / "libvulkan_lvp.so").write_text("stand-in driver")
    manifest = directory / "lvp_icd.json"
    manifest.write_text(json.dumps({"ICD": {"library_path": str(directory / "lib" / "libvulkan_lvp.so")}}))
    return manifest


@pytest.fixture(autouse=True)
def stand_in_lavapipe(stand_in_lavapipe_icd: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """The Rust build renders on lavapipe only, so every Rust launch needs its manifest: a stand-in, as run-linux.sh
    hands the harness the real one. A test of a missing or wrong lavapipe removes or replaces it."""
    monkeypatch.setenv(base.LAVAPIPE_ICD_VARIABLE, str(stand_in_lavapipe_icd))
    return stand_in_lavapipe_icd


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
