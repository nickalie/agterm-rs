"""scripts/fetch-lavapipe.sh, where the harness's GPUI runs get their only Vulkan driver: the order of its sources and
the vendored prefix's key and completeness. Each case runs the real script over a copy of the scripts and pins, with
its vendor root and the loader's ICD directories private to the test; nothing is fetched."""

import os
import platform
import re
import shutil
import subprocess
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
SCRIPT = "scripts/fetch-lavapipe.sh"


@pytest.fixture
def checkout(tmp_path: Path) -> Path:
    """The script, its library and the pins, as a repository of their own."""
    root = tmp_path / "repo"
    (root / "scripts" / "lib").mkdir(parents=True)
    (root / "build").mkdir()
    shutil.copy2(REPO / SCRIPT, root / SCRIPT)
    shutil.copy2(REPO / "scripts/lib/native.sh", root / "scripts/lib/native.sh")
    shutil.copy2(REPO / "build/pins.env", root / "build/pins.env")
    return root


@pytest.fixture
def icd_dirs(tmp_path: Path) -> Path:
    """The loader's ICD directory as the test fills it (empty: no distribution lavapipe)."""
    directory = tmp_path / "icd.d"
    directory.mkdir()
    return directory


def fetch(checkout: Path, icd_dirs: Path, *args: str, **env: str) -> subprocess.CompletedProcess[str]:
    base = {name: value for name, value in os.environ.items() if not name.startswith(("AGTERM_", "ZMX_"))}
    base.update(AGTERM_VENDOR=str(checkout.parent / "vendor"), AGTERM_VULKAN_ICD_DIRS=str(icd_dirs), **env)
    return subprocess.run(
        ["bash", str(checkout / SCRIPT), *args], env=base, capture_output=True, text=True, timeout=60, check=False
    )


def prefix(checkout: Path, icd_dirs: Path) -> Path:
    found = fetch(checkout, icd_dirs, "--print-prefix")
    assert found.returncode == 0, found.stderr
    return Path(found.stdout.strip())


def pin_lines(checkout: Path) -> str:
    lines = (checkout / "build/pins.env").read_text().splitlines()
    return "".join(f"{line}\n" for line in lines if line.startswith("LAVAPIPE_"))


def fill_prefix(path: Path, pins: str) -> None:
    """A complete vendored prefix: a stand-in driver (no libraries to resolve), its manifest, the pin manifest."""
    (path / "lib").mkdir(parents=True)
    (path / "lib" / "libvulkan_lvp.so").write_text("stand-in")
    (path / "lvp_icd.json").write_text(f'{{"ICD": {{"library_path": "{path}/lib/libvulkan_lvp.so"}}}}')
    (path / "share" / "agterm").mkdir(parents=True)
    (path / "share" / "agterm" / "lavapipe-PINS").write_text(pins)


def edit_pin(checkout: Path, name: str, value: str) -> None:
    pins = checkout / "build/pins.env"
    text, count = re.subn(rf'^{name}=".*"$', f'{name}="{value}"', pins.read_text(), flags=re.MULTILINE)
    assert count == 1, name
    pins.write_text(text)


def test_the_pins_name_an_arch_package_with_its_sum() -> None:
    pins = (REPO / "build/pins.env").read_text()
    archive = r"https://archive\.archlinux\.org/packages/v/vulkan-swrast/vulkan-swrast-"
    assert re.search(rf'^LAVAPIPE_ARCH_PACKAGE_URL="{archive}', pins, re.M)
    assert re.search(r'^LAVAPIPE_ARCH_PACKAGE_SHA256="[0-9a-f]{64}"$', pins, re.M)


def test_the_prefix_is_keyed_by_the_lavapipe_pins_alone(checkout: Path, icd_dirs: Path) -> None:
    base = prefix(checkout, icd_dirs)
    assert base.parent == checkout.parent / "vendor" and re.fullmatch(r"lavapipe-[0-9a-f]{16}", base.name)
    edit_pin(checkout, "GHOSTTY_REV", "0" * 40)
    assert prefix(checkout, icd_dirs) == base
    edit_pin(checkout, "LAVAPIPE_VERSION", "99.0.0")
    assert prefix(checkout, icd_dirs) != base


def test_the_override_wins(checkout: Path, icd_dirs: Path, tmp_path: Path) -> None:
    (icd_dirs / "lvp_icd.json").write_text("{}")
    mine = tmp_path / "mine.json"
    mine.write_text("{}")
    for args in (["--print-icd"], []):
        found = fetch(checkout, icd_dirs, *args, AGTERM_LAVAPIPE_ICD=str(mine))
        assert found.returncode == 0 and found.stdout == f"{mine}\n", found.stderr


def test_an_override_naming_nothing_is_an_error(checkout: Path, icd_dirs: Path, tmp_path: Path) -> None:
    (icd_dirs / "lvp_icd.json").write_text("{}")
    found = fetch(checkout, icd_dirs, AGTERM_LAVAPIPE_ICD=str(tmp_path / "absent.json"))
    assert found.returncode == 1 and "AGTERM_LAVAPIPE_ICD names no ICD manifest" in found.stderr


def test_the_distributions_lavapipe_for_this_arch_comes_next(checkout: Path, icd_dirs: Path) -> None:
    for name in ("nvidia_icd.json", "lvp_icd.i686.json", f"lvp_icd.{platform.machine()}.json", "lvp_icd.json"):
        (icd_dirs / name).write_text("{}")
    fill_prefix(prefix(checkout, icd_dirs), pin_lines(checkout))
    found = fetch(checkout, icd_dirs, "--print-icd")
    assert found.stdout == f"{icd_dirs / f'lvp_icd.{platform.machine()}.json'}\n", found.stderr
    (icd_dirs / f"lvp_icd.{platform.machine()}.json").unlink()
    assert fetch(checkout, icd_dirs, "--print-icd").stdout == f"{icd_dirs / 'lvp_icd.json'}\n"


def test_a_complete_vendored_prefix_comes_last(checkout: Path, icd_dirs: Path) -> None:
    (icd_dirs / "nvidia_icd.json").write_text("{}")  # never a hardware driver's
    vendored = prefix(checkout, icd_dirs)
    fill_prefix(vendored, pin_lines(checkout))
    assert fetch(checkout, icd_dirs, "--verify").returncode == 0
    for args in (["--print-icd"], []):
        found = fetch(checkout, icd_dirs, *args)
        assert found.returncode == 0 and found.stdout == f"{vendored / 'lvp_icd.json'}\n", found.stderr


def test_without_lavapipe_print_icd_names_every_source(checkout: Path, icd_dirs: Path) -> None:
    (icd_dirs / "nvidia_icd.json").write_text("{}")
    found = fetch(checkout, icd_dirs, "--print-icd")
    assert found.returncode == 1 and found.stdout == ""
    for source in ("mesa-vulkan-drivers", "vulkan-swrast", "scripts/fetch-lavapipe.sh", "AGTERM_LAVAPIPE_ICD"):
        assert source in found.stderr
    assert fetch(checkout, icd_dirs, "--verify").returncode == 1
    assert not (checkout.parent / "vendor").exists()  # --print-icd and --verify fetch nothing


def test_a_prefix_of_other_pins_is_not_complete(checkout: Path, icd_dirs: Path) -> None:
    fill_prefix(prefix(checkout, icd_dirs), 'LAVAPIPE_VERSION="0.0.1"\n')
    assert fetch(checkout, icd_dirs, "--verify").returncode == 1
    assert fetch(checkout, icd_dirs, "--print-icd").returncode == 1


def test_a_vendored_driver_this_host_cannot_load_is_an_error(checkout: Path, icd_dirs: Path) -> None:
    """A prefix unpacked before the host's libLLVM moved on: an error naming the library, never a silent fallback."""
    vendored = prefix(checkout, icd_dirs)
    fill_prefix(vendored, pin_lines(checkout))
    fake_ldd = checkout.parent / "bin" / "ldd"
    fake_ldd.parent.mkdir()
    fake_ldd.write_text("#!/bin/sh\necho '\tlibLLVM.so.22.1 => not found'\n")
    fake_ldd.chmod(0o755)
    found = fetch(checkout, icd_dirs, "--print-icd", PATH=f"{fake_ldd.parent}:{os.environ['PATH']}")
    assert found.returncode == 1 and "needs libraries this host lacks: libLLVM.so.22.1" in found.stderr


def test_an_unknown_argument_is_a_usage_error(checkout: Path, icd_dirs: Path) -> None:
    found = fetch(checkout, icd_dirs, "--install")
    assert found.returncode == 1 and "usage: scripts/fetch-lavapipe.sh" in found.stderr
