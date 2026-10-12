"""pytest plugin: options, markers, the live-session env scrub, instance fixtures, skips, the summary and the gate."""

from __future__ import annotations

import json
import os
import queue
import re
import subprocess
import threading
from collections.abc import Callable, Iterator, Mapping, MutableMapping
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Any, Self

import pytest

from agterm_conformance import coverage, wait
from agterm_conformance.client import Response, SocketOutsideRoot, Stream, check_under_root
from agterm_conformance.match import assert_ok

if TYPE_CHECKING:
    from agterm_conformance.drivers.base import Driver
    from agterm_conformance.impls.base import Adapter, Instance

IMPLS = ("swift-macos", "swift-linux", "rust")
# A rust run without a binary is legal: the adapter reports nothing and every test skips.
BIN_REQUIRED = frozenset({"swift-macos", "swift-linux"})
SCRUB_PREFIXES = ("AGTERM_", "ZMX_")
IMPL_FIXTURE = "impl_options"
UI_FIXTURE = "ui"
# Skip reasons start with these, so the summary can tell a divergence from a missing feature.
UNSUPPORTED = "unsupported"
NOT_YET_AT_LEVEL = "not-yet-at-level"
CLI_TIMEOUT = 30.0
STOP_GRACE = 5.0

MARKERS = (
    (
        "since(version): upstream version that introduced the behavior; "
        "skipped as not-yet-at-level above the adapter's parity_level"
    ),
    "requires(capability, reason=...): skipped as unsupported when the adapter lacks the capability",
    (
        "covers(*items, streams=()): catalog items this test covers, read by the coverage gate; a stream-mode "
        "command counts only when also named in streams, by a test that drives the stream after the ok"
    ),
)
COVERS_KWARGS = frozenset({"streams"})


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
    macos_launch_mode: str = "uitest"
    catalog: Path | None = None
    ui_ids: Path | None = None
    coverage_baseline: Path | None = None


_OPTIONS_KEY = pytest.StashKey[ImplOptions]()
_ADAPTER_KEY = pytest.StashKey["Adapter"]()
_SUMMARY_KEY = pytest.StashKey[bool]()
_GATE_KEY = pytest.StashKey[coverage.Report]()
_COLLECTED_KEY = pytest.StashKey[list[pytest.Item]]()


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
        "--macos-launch-mode",
        choices=("uitest", "plain"),
        default="uitest",
        help="swift-macos: XCUITest sentinel with zmx (default) or a bare exec",
    )
    group.addoption(
        "--coverage-gate", action="store_true", default=False, help="check catalog coverage without launching anything"
    )
    group.addoption(
        "--must-pass",
        action="append",
        default=[],
        metavar="CAPABILITY",
        help="fail the run unless every collected test requiring CAPABILITY ran and passed (none collected fails too)",
    )
    group.addoption("--catalog", type=Path, default=None, help="coverage gate: catalog.yaml (default: <rootdir>/)")
    group.addoption(
        "--ui-ids",
        type=Path,
        default=None,
        help="ui-ids.yaml for the coverage gate and the ui driver (default: next to the catalog)",
    )
    group.addoption(
        "--coverage-baseline",
        type=Path,
        default=None,
        help="coverage gate: file of uncovered items to accept, one covers key per line; it may only shrink",
    )


def pytest_configure(config: pytest.Config) -> None:
    for line in MARKERS:
        config.addinivalue_line("markers", line)
    options = ImplOptions(
        impl=config.getoption("impl"),
        bin=config.getoption("bin"),
        ctl=config.getoption("ctl"),
        coverage_gate=config.getoption("coverage_gate"),
        macos_launch_mode=config.getoption("macos_launch_mode"),
        catalog=config.getoption("catalog"),
        ui_ids=config.getoption("ui_ids"),
        coverage_baseline=config.getoption("coverage_baseline"),
    )
    for flag, path in (("--bin", options.bin), ("--ctl", options.ctl)):
        if path is not None and not path.exists():
            raise pytest.UsageError(f"{flag} {path} does not exist")
    config.stash[_OPTIONS_KEY] = options
    if options.coverage_gate:
        config.pluginmanager.register(_GateCollector(), "agterm-coverage-gate-collector")
    elif must_pass := config.getoption("must_pass"):
        from agterm_conformance.impls.base import CAPABILITIES

        if unknown := sorted(set(must_pass) - set(CAPABILITIES)):
            raise pytest.UsageError(f"--must-pass: unknown capability {', '.join(unknown)}")
        config.pluginmanager.register(MustPass(frozenset(must_pass)), "agterm-must-pass")


def ui_ids_path(config: pytest.Config, options: ImplOptions) -> Path:
    """`--ui-ids`, else the ui-ids.yaml next to the catalog: one file for the gate and the ui driver."""
    catalog = options.catalog or config.rootpath / "catalog.yaml"
    return (options.ui_ids or catalog.parent / "ui-ids.yaml").resolve()


def gate_paths(config: pytest.Config, options: ImplOptions) -> tuple[Path, Path]:
    """The gate's catalog and ui-ids files; refuses a run that would not collect every conformance test.

    Deselection (-k, -m, --deselect) needs no check: declarations are read before it runs.
    --lf is refused with the ignores because it skips whole files during collection.
    """
    for flag, spelling in (("ignore", "--ignore"), ("ignore_glob", "--ignore-glob"), ("lf", "--lf")):
        if config.getoption(flag, None):
            raise pytest.UsageError(f"--coverage-gate reads every conformance test; drop {spelling}")
    catalog = (options.catalog or config.rootpath / "catalog.yaml").resolve()
    ui_ids = ui_ids_path(config, options)
    for flag, path in (
        ("--catalog", catalog),
        ("--ui-ids", ui_ids),
        ("--coverage-baseline", options.coverage_baseline),
    ):
        if path is not None and not path.is_file():
            raise pytest.UsageError(f"--coverage-gate: {flag} {path} is not a file")
    tests = catalog.parent / "tests"
    base = config.invocation_params.dir
    for arg in config.args:
        if "::" in arg:
            raise pytest.UsageError(f"--coverage-gate reads every conformance test; {arg} selects one")
    if not any(tests.is_relative_to((base / arg).resolve()) for arg in config.args):
        raise pytest.UsageError(f"--coverage-gate reads every conformance test; collect {tests} or a parent of it")
    return catalog, ui_ids


class MustPass:
    """`--must-pass`: a skip, a failure or a deselection of a test requiring one of `capabilities` fails the run."""

    def __init__(self, capabilities: frozenset[str]) -> None:
        self.capabilities = capabilities
        self.required: list[str] = []
        self.uncollected: set[str] = set(capabilities)
        self.passed: set[str] = set()
        self.spoiled: set[str] = set()
        self.problems: list[str] = []

    @pytest.hookimpl(tryfirst=True)
    def pytest_collection_modifyitems(self, items: list[pytest.Item]) -> None:
        # tryfirst: before -k/-m deselection, so a deselected test still counts as required.
        self.required = []
        self.uncollected = set(self.capabilities)
        for item in items:
            wanted = {name for marker in item.iter_markers("requires") for name in marker.args} & self.capabilities
            if wanted:
                self.required.append(item.nodeid)
                self.uncollected -= wanted

    def pytest_runtest_logreport(self, report: pytest.TestReport) -> None:
        if report.when == "call" and report.passed:
            self.passed.add(report.nodeid)
        elif not report.passed:
            self.spoiled.add(report.nodeid)

    @pytest.hookimpl(tryfirst=True)
    def pytest_sessionfinish(self, session: pytest.Session) -> None:
        self.problems = [f"no collected test requires {capability}" for capability in sorted(self.uncollected)]
        self.problems += [nodeid for nodeid in self.required if nodeid not in self.passed - self.spoiled]
        if self.problems and session.exitstatus == pytest.ExitCode.OK:
            session.exitstatus = pytest.ExitCode.TESTS_FAILED

    def pytest_terminal_summary(self, terminalreporter: Any) -> None:
        if self.problems:
            terminalreporter.write_sep("-", f"--must-pass {', '.join(sorted(self.capabilities))}: not passed")
            for problem in self.problems:
                terminalreporter.write_line(problem)


class _GateCollector:
    """A second plugin object: a module holds one implementation per hook, and the gate needs two."""

    @pytest.hookimpl(tryfirst=True)
    def pytest_collection_modifyitems(self, config: pytest.Config, items: list[pytest.Item]) -> None:
        config.stash[_COLLECTED_KEY] = list(items)


@pytest.hookimpl(trylast=True)
def pytest_collection_modifyitems(session: pytest.Session, config: pytest.Config, items: list[pytest.Item]) -> None:
    options = get_impl_options(config)
    if options.coverage_gate:
        collected = config.stash[_COLLECTED_KEY]
        for item in collected:
            check_markers(item)
        items[:] = [_coverage_gate_item(session, collected, options)]
        return
    for item in items:
        check_markers(item)
    # trylast: -k/-m deselection has already run, so only tests that will execute demand --impl.
    if any(_uses_impl(item) for item in items):
        require_implementation(options)
        config.stash[_SUMMARY_KEY] = True


def _uses_impl(item: pytest.Item) -> bool:
    return IMPL_FIXTURE in getattr(item, "fixturenames", ())


def get_adapter(config: pytest.Config) -> Adapter:
    """One adapter per run, so a Rust build's probed profile is resolved once."""
    if _ADAPTER_KEY not in config.stash:
        from agterm_conformance.impls import make_adapter  # impls imports this module

        options = get_impl_options(config)
        require_implementation(options)
        config.stash[_ADAPTER_KEY] = make_adapter(options)
    return config.stash[_ADAPTER_KEY]


def check_markers(item: pytest.Item) -> None:
    """A typo in a level or capability would silently skip or run a test on every implementation."""
    from agterm_conformance.impls.base import CAPABILITIES, version_tuple

    for since in item.iter_markers("since"):
        if len(since.args) != 1 or not isinstance(since.args[0], str):
            raise pytest.UsageError(f"{item.nodeid}: since takes one version string, got {since.args}")
        try:
            version_tuple(since.args[0])
        except ValueError as error:
            raise pytest.UsageError(f"{item.nodeid}: since: {error}") from None
    for covers in item.iter_markers("covers"):
        check_covers(item.nodeid, covers)
    for marker in item.iter_markers("requires"):
        if not marker.args:
            raise pytest.UsageError(f"{item.nodeid}: requires names no capability")
        for capability in marker.args:
            if capability not in CAPABILITIES:
                raise pytest.UsageError(
                    f"{item.nodeid}: unknown capability {capability!r} (known: {', '.join(sorted(CAPABILITIES))})"
                )


def check_covers(nodeid: str, marker: pytest.Mark) -> None:
    if not marker.args or not all(isinstance(arg, str) and arg for arg in marker.args):
        raise pytest.UsageError(f"{nodeid}: covers takes one or more item strings, got {marker.args}")
    unknown = set(marker.kwargs) - COVERS_KWARGS
    if unknown:
        raise pytest.UsageError(f"{nodeid}: covers takes no {', '.join(sorted(unknown))}")
    streams = marker.kwargs.get("streams", ())
    if not isinstance(streams, (tuple, list)) or not all(isinstance(s, str) for s in streams):
        raise pytest.UsageError(f"{nodeid}: covers streams must be a tuple of command names, got {streams!r}")
    if len(set(streams)) != len(streams):
        raise pytest.UsageError(f"{nodeid}: covers streams lists a command twice: {streams!r}")
    stray = [s for s in streams if s not in marker.args]
    if stray:
        raise pytest.UsageError(f"{nodeid}: covers streams names {', '.join(stray)} without covering it")


# -- coverage gate


class CoverageGateFailed(Exception):
    pass


class CoverageGate(pytest.Item):
    """Stands in for every collected test under --coverage-gate, so nothing they need is set up."""

    def __init__(self, *, report: coverage.Report, catalog: Path, **kwargs: Any) -> None:
        super().__init__(**kwargs)
        self.report = report
        self.catalog = catalog

    def runtest(self) -> None:
        if not self.report.ok:
            raise CoverageGateFailed(self.report.failure_text())

    def repr_failure(self, excinfo: pytest.ExceptionInfo[BaseException], style: Any = None) -> str:
        if isinstance(excinfo.value, CoverageGateFailed):
            return str(excinfo.value)
        return str(super().repr_failure(excinfo, style))

    def reportinfo(self) -> tuple[Path, int | None, str]:
        return self.catalog, None, "coverage gate"


def _coverage_gate_item(session: pytest.Session, items: list[pytest.Item], options: ImplOptions) -> CoverageGate:
    catalog_path, ui_path = gate_paths(session.config, options)
    tests = catalog_path.parent / "tests"
    declarations = [
        coverage.Declaration(item.nodeid, marker.args, tuple(marker.kwargs.get("streams", ())))
        for item in items
        if item.path.resolve().is_relative_to(tests)
        for marker in item.iter_markers("covers")
    ]
    try:
        catalog = coverage.load_catalog(catalog_path, ui_path)
        baseline = None if options.coverage_baseline is None else coverage.load_baseline(options.coverage_baseline)
    except coverage.CatalogError as error:
        raise pytest.UsageError(f"--coverage-gate: {error}") from None
    report = coverage.evaluate(catalog, declarations, baseline)
    session.config.stash[_GATE_KEY] = report
    return CoverageGate.from_parent(session, name="coverage-gate", report=report, catalog=catalog_path)


def skip_reason(item: pytest.Item, adapter: Adapter) -> str | None:
    """Level first: a build below a test's level is not judged on that test's capabilities."""
    since = item.get_closest_marker("since")
    if not adapter.available:
        return f"{NOT_YET_AT_LEVEL}: {adapter.name} has no binary"
    profile = adapter.profile
    if profile.parity_level is None:
        return f"{NOT_YET_AT_LEVEL}: {adapter.name} reports no parity level"
    if since is not None and not profile.at_level(since.args[0]):
        return f"{NOT_YET_AT_LEVEL}: since {since.args[0]}, {adapter.name} is at {profile.parity_level}"
    for marker in item.iter_markers("requires"):
        for capability in marker.args:
            if capability not in profile.capabilities:
                why = marker.kwargs.get("reason")
                here = profile.absent.get(capability)
                return (
                    f"{UNSUPPORTED}: {adapter.name} lacks {capability}"
                    + (f" on this host: {here}" if here else "")
                    + (f" ({why})" if why else "")
                )
    return None


def ui_skip_reason(adapter: Adapter) -> str | None:
    from agterm_conformance.drivers.base import NoDriver, driver_class

    try:
        driver_class(adapter.name)
    except NoDriver as error:
        return f"{UNSUPPORTED}: {error}"
    return None


def pytest_runtest_setup(item: pytest.Item) -> None:
    # Not tryfirst: pytest's own skip markers run first, and registering after `_pytest.runner` still
    # decides before fixture setup, so a skipped test launches nothing.
    if get_impl_options(item.config).coverage_gate or not _uses_impl(item):
        return
    adapter = get_adapter(item.config)
    reason = skip_reason(item, adapter)
    if reason is None and UI_FIXTURE in getattr(item, "fixturenames", ()):
        reason = ui_skip_reason(adapter)
    if reason is not None:
        # What pytest's own skip marker raises, so `-rs` points at the test, not this hook.
        raise pytest.skip.Exception(reason, _use_item_location=True)


def pytest_terminal_summary(terminalreporter: Any, exitstatus: int, config: pytest.Config) -> None:
    report = config.stash.get(_GATE_KEY, None)
    if report is not None:
        terminalreporter.write_sep("-", "agterm coverage")
        for line in report.summary_lines():
            terminalreporter.write_line(line)
        if report.baselined:
            terminalreporter.write_line(f"baselined, accepted for now ({len(report.baselined)}):")
            for key in report.baselined:
                terminalreporter.write_line(f"  {key}")
    if not config.stash.get(_SUMMARY_KEY, False):
        return
    terminalreporter.write_sep("-", "agterm conformance")
    terminalreporter.write_line(summary_line(terminalreporter.stats, get_impl_options(config).impl))


def summary_line(stats: Mapping[str, list[Any]], impl: str | None) -> str:
    failed = {report.nodeid for key in ("failed", "error") for report in stats.get(key, [])}
    passed = {report.nodeid for report in stats.get("passed", [])} - failed
    skips = {UNSUPPORTED: 0, NOT_YET_AT_LEVEL: 0, "other": 0}
    for report in stats.get("skipped", []):
        skips[_skip_category(report)] += 1
    counts = [
        f"{len(passed)} pass",
        f"{len(failed)} fail",
        *(f"{count} skip({category})" for category, count in skips.items()),
        f"{len(stats.get('rerun', []))} reruns",
    ]
    return f"{impl}: " + ", ".join(counts)


def _skip_category(report: Any) -> str:
    longrepr = report.longrepr
    message = longrepr[2] if isinstance(longrepr, tuple) and len(longrepr) == 3 else str(longrepr)
    message = message.removeprefix("Skipped: ")
    for category in (UNSUPPORTED, NOT_YET_AT_LEVEL):
        if message.startswith(f"{category}:"):
            return category
    return "other"


def pytest_report_header(config: pytest.Config) -> list[str]:
    options = get_impl_options(config)
    lines = [f"agterm impl: {options.impl or '-'}, bin: {options.bin or '-'}, ctl: {options.ctl or '-'}"]
    if SCRUBBED:
        lines.append(f"scrubbed from env: {', '.join(SCRUBBED)}")
    return lines


@pytest.fixture(scope="session")
def impl_options(pytestconfig: pytest.Config) -> ImplOptions:
    return get_impl_options(pytestconfig)


# -- instances


class Ctl:
    """The socket side of one instance, with the waits a test needs instead of sleeping."""

    def __init__(self, instance: Instance) -> None:
        self.instance = instance

    def request(self, cmd: str, target: str | None = None, **args: Any) -> Response:
        return self.instance.request(cmd, target, **args)

    def tree(self) -> dict[str, Any]:
        return assert_ok(self.request("tree"))

    def stream(self, cmd: str, target: str | None = None, **args: Any) -> Stream:
        """A stream-mode command (`zmx.present`, `session.overlay.job.run`): its reply, then its frames."""
        return self.instance.client().open_stream(cmd, target, **args)

    def wait_tree(
        self, predicate: Callable[[dict[str, Any]], Any], timeout: float = wait.DEFAULT_TIMEOUT
    ) -> dict[str, Any]:
        return wait.wait_tree(self, predicate, timeout)

    def wait_mounted(self, session_id: str, *panes: str, timeout: float = wait.DEFAULT_TIMEOUT) -> dict[str, Any]:
        return wait.wait_mounted(self, session_id, *panes, timeout=timeout)

    def wait_text(
        self, target: str | None, pattern: str | re.Pattern[str], timeout: float = wait.DEFAULT_TIMEOUT, **args: Any
    ) -> re.Match[str]:
        return wait.wait_text(self, target, pattern, timeout, **args)

    def wait_response(
        self,
        cmd: str,
        target: str | None,
        accept: Callable[[Response], Any],
        timeout: float = wait.DEFAULT_TIMEOUT,
        **args: Any,
    ) -> Response:
        return wait.wait_response(self, cmd, target, accept, timeout, **args)

    def events_baseline(self) -> wait.Cursor:
        return wait.events_baseline(self)

    def wait_event(
        self,
        cursor: wait.Cursor,
        kind: str,
        match: Mapping[str, Any] | Callable[[dict[str, Any]], Any] | None = None,
        timeout: float = wait.DEFAULT_TIMEOUT,
    ) -> dict[str, Any]:
        return wait.wait_event(self, cursor, kind, match, timeout)


@dataclass(frozen=True)
class CliResult:
    argv: list[str]
    returncode: int
    stdout: str
    stderr: str

    def json(self) -> Any:
        return json.loads(self.stdout)


class Cli:
    """`agtermctl` run in the instance's env, always with that instance's `--socket`."""

    def __init__(self, ctl_path: Path, instance: Instance) -> None:
        check_under_root(instance.socket, instance.root)
        self.ctl_path = ctl_path
        self.instance = instance

    def argv(self, *args: str) -> list[str]:
        # Before a `--`, where everything after it is the payload, not an option.
        args_list = list(args)
        end = args_list.index("--") if "--" in args_list else len(args_list)
        socket_option = ["--socket", os.fspath(self.instance.socket)]
        return [os.fspath(self.ctl_path), *args_list[:end], *socket_option, *args_list[end:]]

    def on(self, instance: Instance) -> Cli:
        return Cli(self.ctl_path, instance)

    def __call__(self, *args: str, input: str | None = None, timeout: float = CLI_TIMEOUT) -> CliResult:
        return self._run(self.argv(*args), self.instance.env, input, timeout)

    def local(
        self,
        *args: str,
        env: Mapping[str, str] | None = None,
        input: str | None = None,
        timeout: float = CLI_TIMEOUT,
        default_socket: Path | None = None,
    ) -> CliResult:
        """Without `--socket`: for the local-only subcommands, which refuse it, and for socket resolution itself.

        `env` (default: the instance's) must point `AGTERM_STATE_DIR` under the instance root, so even a command
        that did open a socket would resolve one there, never the user's. Without it the caller names the platform
        default that `env` resolves to as `default_socket`, which must lie under the root too.
        """
        env = dict(self.instance.env if env is None else env)
        if state_dir := env.get("AGTERM_STATE_DIR"):
            check_under_root(state_dir, self.instance.root)
        elif default_socket is not None:
            check_under_root(default_socket, self.instance.root)
        else:
            raise SocketOutsideRoot("refusing agtermctl without --socket: no AGTERM_STATE_DIR and no checked default")
        return self._run([os.fspath(self.ctl_path), *args], env, input, timeout)

    def spawn(self, *args: str, input: str | None = None, interactive: bool = False) -> CliProcess:
        """A command that keeps running (`events`, a blocking `pick`/`ask`/`--block`); read it as it goes.

        `input` is written and stdin closed; `interactive` keeps stdin open for `send` (a stream bridge)."""
        return CliProcess(self.argv(*args), self.instance.env, self.instance.home, input, interactive)

    def _run(self, argv: list[str], env: Mapping[str, str], input: str | None, timeout: float) -> CliResult:
        completed = subprocess.run(
            argv,
            env=env,
            cwd=self.instance.home,
            input=input,
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
        )
        return CliResult(argv, completed.returncode, completed.stdout, completed.stderr)


class CliProcess:
    """One running `agtermctl`: stdout read line by line on a thread, stopped (own pid only) by `close`."""

    def __init__(
        self, argv: list[str], env: Mapping[str, str], cwd: Path, input: str | None, interactive: bool = False
    ) -> None:
        self.argv = argv
        self.process = subprocess.Popen(
            argv,
            env=dict(env),
            cwd=cwd,
            stdin=subprocess.PIPE if input is not None or interactive else subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        if input is not None:
            self.send(input)
            if not interactive:
                assert self.process.stdin is not None
                self.process.stdin.close()
        self._lines: queue.Queue[str | None] = queue.Queue()
        self._stderr: str | None = None
        self._reader = threading.Thread(target=self._read, daemon=True)
        self._reader.start()

    def _read(self) -> None:
        assert self.process.stdout is not None
        for line in self.process.stdout:
            self._lines.put(line.rstrip("\n"))
        self._lines.put(None)

    def send(self, text: str) -> None:
        assert self.process.stdin is not None
        self.process.stdin.write(text)
        self.process.stdin.flush()

    def line(self, timeout: float = wait.DEFAULT_TIMEOUT) -> str:
        """The next stdout line; fails when none arrives in time or the process ended without one."""
        try:
            line = self._lines.get(timeout=timeout)
        except queue.Empty:
            raise AssertionError(f"no output line from {self.argv[1:]} within {timeout}s") from None
        if line is None:
            self._lines.put(None)
            raise AssertionError(f"{self.argv[1:]} ended (exit {self.process.wait()}): {self.stderr()}")
        return line

    def finish(self, timeout: float = CLI_TIMEOUT) -> CliResult:
        """Wait for the process to exit by itself; the stdout lines not yet read come back as `stdout`."""
        returncode = self.process.wait(timeout=timeout)
        self._reader.join(timeout)
        lines: list[str] = []
        while (line := self._lines.get(timeout=timeout)) is not None:
            lines.append(line)
        self._lines.put(None)
        stdout = "".join(f"{line}\n" for line in lines)
        return CliResult(self.argv, returncode, stdout, self.stderr())

    def stderr(self) -> str:
        """Everything the process wrote to stderr, once it has exited; empty while it runs."""
        if self._stderr is None and self.process.poll() is not None:
            assert self.process.stderr is not None
            self._stderr = self.process.stderr.read()
        return self._stderr or ""

    def close(self) -> None:
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=STOP_GRACE)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
        self._reader.join(STOP_GRACE)
        self.stderr()
        for stream in (self.process.stdin, self.process.stdout, self.process.stderr):
            if stream is not None:
                try:
                    stream.close()
                except BrokenPipeError:
                    pass  # stdin's unflushed bytes had no reader left

    def __enter__(self) -> Self:
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()


@pytest.fixture(scope="session")
def adapter(pytestconfig: pytest.Config, impl_options: ImplOptions) -> Adapter:
    return get_adapter(pytestconfig)


def _launched(adapter: Adapter) -> Iterator[Instance]:
    instance = adapter.launch()
    try:
        yield instance
    finally:
        adapter.stop(instance)


@pytest.fixture
def app(adapter: Adapter) -> Iterator[Instance]:
    """A pristine instance for this test alone."""
    yield from _launched(adapter)


@pytest.fixture(scope="module")
def shared_app(adapter: Adapter) -> Iterator[Instance]:
    """One instance for the module; the default target of `ctl` and `cli`."""
    yield from _launched(adapter)


class Launcher:
    """`launch(settings={...})` starts a further instance; `launch.relaunch(ctl)` quits one and starts it again on
    the same state directory. Every instance still running is stopped at teardown."""

    def __init__(self, adapter: Adapter) -> None:
        self.adapter = adapter
        self.launched: list[Instance] = []

    def __call__(self, **kwargs: Any) -> Ctl:
        instance = self.adapter.launch(**kwargs)
        self.launched.append(instance)
        return Ctl(instance)

    def relaunch(self, ctl: Ctl) -> Ctl:
        index = self.launched.index(ctl.instance)
        # Out of the list first: a failed relaunch has stopped its process, reported its leftovers and keeps the root.
        old = self.launched.pop(index)
        self.launched.insert(index, self.adapter.relaunch(old))
        return Ctl(self.launched[index])

    def stop_all(self) -> None:
        failure: Exception | None = None
        for instance in reversed(self.launched):
            try:
                self.adapter.stop(instance)
            except Exception as error:  # noqa: BLE001 - stop every instance, then report the first failure
                failure = failure or error
        if failure is not None:
            raise failure


@pytest.fixture
def launch(adapter: Adapter) -> Iterator[Launcher]:
    """Start further instances, such as one with seeded settings (`launch(settings={...})`); stopped at teardown."""
    launcher = Launcher(adapter)
    yield launcher
    launcher.stop_all()


def _target_instance(request: pytest.FixtureRequest) -> Instance:
    return request.getfixturevalue("app" if "app" in request.fixturenames else "shared_app")


@pytest.fixture
def ctl(request: pytest.FixtureRequest, adapter: Adapter) -> Ctl:
    """Drives `app` when the test asks for it, else `shared_app`."""
    return Ctl(_target_instance(request))


@pytest.fixture
def cli(request: pytest.FixtureRequest, adapter: Adapter) -> Cli:
    """Drives `app` when the test asks for it, else `shared_app`."""
    if adapter.ctl_path is None:
        pytest.fail(f"{adapter.name} has no agtermctl: pass --ctl")
    return Cli(adapter.ctl_path, _target_instance(request))


@pytest.fixture
def ui(request: pytest.FixtureRequest, adapter: Adapter, impl_options: ImplOptions) -> Driver:
    """The ui driver for the instance `ctl` drives (`pytest_runtest_setup` skips where none is written yet)."""
    from agterm_conformance.drivers.base import UiIds, make_driver

    ui_ids = UiIds.load(ui_ids_path(request.config, impl_options))
    return make_driver(adapter.name, _target_instance(request).pid, ui_ids)
