"""pytest plugin: implementation options, markers and the live-session environment scrub."""

from __future__ import annotations

import os
from collections.abc import MutableMapping
from dataclasses import dataclass
from pathlib import Path

import pytest

IMPLS = ("swift-macos", "swift-linux", "rust")
# A rust run without a binary is legal: the adapter reports nothing and every test skips.
BIN_REQUIRED = frozenset({"swift-macos", "swift-linux"})
SCRUB_PREFIXES = ("AGTERM_", "ZMX_")
IMPL_FIXTURE = "impl_options"

MARKERS = (
    "since(version): upstream version that introduced the behavior; "
    "skipped as not-yet-at-level above the adapter's parity_level",
    "requires(capability, reason=...): skipped as unsupported when the adapter lacks the capability",
    "covers(*items): catalog items this test covers, read by the coverage gate",
)


def scrub_environ(environ: MutableMapping[str, str]) -> list[str]:
    removed = sorted(name for name in environ if name.startswith(SCRUB_PREFIXES))
    for name in removed:
        del environ[name]
    return removed


# At import, before any hook runs: a run started inside a live agterm shell must not hand its
# AGTERM_SOCKET / AGTERM_SESSION_ID / ZMX_DIR to anything this process spawns.
SCRUBBED = scrub_environ(os.environ)


@dataclass(frozen=True)
class ImplOptions:
    impl: str | None
    bin: Path | None
    ctl: Path | None
    coverage_gate: bool


_OPTIONS_KEY = pytest.StashKey[ImplOptions]()


def get_impl_options(config: pytest.Config) -> ImplOptions:
    return config.stash[_OPTIONS_KEY]


def require_implementation(options: ImplOptions) -> None:
    if options.impl is None:
        raise pytest.UsageError(f"--impl is required to run conformance tests (one of: {', '.join(IMPLS)})")
    if options.bin is None and options.impl in BIN_REQUIRED:
        raise pytest.UsageError(f"--bin is required for --impl={options.impl}")


def pytest_addoption(parser: pytest.Parser) -> None:
    group = parser.getgroup("agterm-conformance")
    group.addoption("--impl", choices=IMPLS, default=None, help="implementation under test")
    group.addoption("--bin", type=Path, default=None, help="app binary or bundle to launch")
    group.addoption("--ctl", type=Path, default=None, help="agtermctl to drive the CLI layer (default: the adapter's)")
    group.addoption(
        "--coverage-gate", action="store_true", default=False, help="check catalog coverage without launching anything"
    )


def pytest_configure(config: pytest.Config) -> None:
    for line in MARKERS:
        config.addinivalue_line("markers", line)
    options = ImplOptions(
        impl=config.getoption("impl"),
        bin=config.getoption("bin"),
        ctl=config.getoption("ctl"),
        coverage_gate=config.getoption("coverage_gate"),
    )
    for flag, path in (("--bin", options.bin), ("--ctl", options.ctl)):
        if path is not None and not path.exists():
            raise pytest.UsageError(f"{flag} {path} does not exist")
    config.stash[_OPTIONS_KEY] = options


@pytest.hookimpl(trylast=True)
def pytest_collection_modifyitems(config: pytest.Config, items: list[pytest.Item]) -> None:
    options = get_impl_options(config)
    if options.coverage_gate:
        return
    # trylast: -k/-m deselection has already run, so only tests that will execute demand --impl.
    if any(IMPL_FIXTURE in getattr(item, "fixturenames", ()) for item in items):
        require_implementation(options)


def pytest_report_header(config: pytest.Config) -> list[str]:
    options = get_impl_options(config)
    lines = [f"agterm impl: {options.impl or '-'}, bin: {options.bin or '-'}, ctl: {options.ctl or '-'}"]
    if SCRUBBED:
        lines.append(f"scrubbed from env: {', '.join(SCRUBBED)}")
    return lines


@pytest.fixture(scope="session")
def impl_options(pytestconfig: pytest.Config) -> ImplOptions:
    return get_impl_options(pytestconfig)
