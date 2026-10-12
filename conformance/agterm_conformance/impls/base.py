"""Implementation adapter contract: isolated launch, socket wait, own-pid stop, leftover-process check.

Safety is an allowlist: every path the instance is told about lives under a fresh `mkdtemp` root, and the
guards refuse a launch env that points anywhere else before anything is spawned.
"""

from __future__ import annotations

import json
import os
import pwd
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
import uuid
from collections.abc import Iterable, Mapping
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, ClassVar

from agterm_conformance.client import (
    Client,
    ClientError,
    ClientTimeout,
    ConnectFailed,
    NoResponse,
    Response,
    check_under_root,
)
from agterm_conformance.plugin import SCRUB_PREFIXES

# Both references refuse to bind at `path.utf8.count >= 104` (Darwin's sun_path, mirrored on Linux).
SOCKET_PATH_CAP = 104
# `/tmp`, not `$TMPDIR`: a macOS runner's `/var/folders/...` temp dir eats most of the socket cap.
# `CONFORMANCE_TMP` (not `AGTERM_`-prefixed, which the plugin scrubs) gives run-reference-linux.sh its own parent.
TEMP_PARENT = os.environ.get("CONFORMANCE_TMP") or "/tmp"
MINIMAL_PATH = "/usr/bin:/bin"
ZSH_STARTUP_VARIABLES = ("ZDOTDIR", "GHOSTTY_ZSH_ZDOTDIR")
DEFAULT_APP_ID = "io.github.melonamin.agterm"
SESSION_HOST = "agterm-session-host"
LEFTOVER_NAMES = frozenset({"zmx", SESSION_HOST})
# With SHELL=/bin/sh this is belt and braces: either alone gives a reference-linux pane /bin/sh
# (conformance/docs/spike-macos.md).
GHOSTTY_SHELL_CONFIG = "command = /bin/sh\n"
# A macOS pane runs a login /bin/sh, which reads this after /etc/profile: that prompt carries the host name, long
# enough on a CI runner that a split pane wraps the typed line and the redraw mangles it.
SHELL_PROFILE = "PS1='$ '\n"
# A selection (`session.selectall`) would otherwise land in the desktop's own primary selection, and an OSC 52 write
# (`agtermctl clipboard set` in a Live pane) in the desktop clipboard the user's session shares.
GHOSTTY_ISOLATION_CONFIG = "copy-on-select = false\nclipboard-write = deny\n"
# The private `XDG_RUNTIME_DIR` under the root: the Linux fork writes its ghostty config layers there
# (`GhosttyApp.writeTempConf`/`writeDefaultsConf`), which in the shared one are the live app's own files.
RUNTIME_DIR_NAME = "run"
# Every URI or file the instance hands the desktop (`session.reveal`, a clicked link, a page's browser hand-off) goes to
# `true`: the system defaults resolve through XDG_DATA_DIRS, which the isolated XDG_DATA_HOME does not replace, and
# opened real windows on the user's desktop. GTK's Show in Files (`session.overlay.navigate` finder) asks the session
# bus's FileManager1 instead, which no mimeapps entry reaches: no test may drive it.
NOOP_DESKTOP_ID = "agterm-conformance-noop.desktop"
NOOP_MIME_TYPES = (
    "inode/directory",
    "x-scheme-handler/http",
    "x-scheme-handler/https",
    "x-scheme-handler/file",
    "text/html",
    "text/plain",
    "application/octet-stream",
)
LAUNCH_TIMEOUT = 30.0
STOP_TIMEOUT = 10.0
REAP_SETTLE = 8.0
POLL_INTERVAL = 0.05

# The capability vocabulary `requires(...)` and `Adapter.has(...)` may name. A reason for each absence lives on the
# test marker, or beside the branch a test takes on `has`.
CAPABILITIES: Mapping[str, str] = {
    "posix-shell": "panes run /bin/sh, so `session type` + `session.text` round-trips are deterministic",
    "live-sessions": "restoreMode live keeps panes in zmx daemons that outlive the app",
    "window-geometry": "window x/y position is settable through `window.move`, read back as `geometry` and restored",
    "window-minimize": "`window.minimize` and `window.new --minimized` take effect and read back as `minimized`",
    "select-takes-frontmost": "`window.select`/`window.go` make their window `active` without waiting on window focus",
    "status-shape": "`session.status` validates `shape` and reads it back as `statusShape`",
    "type-rejects-nul": "`session.type` refuses text carrying a NUL rather than typing the run before it",
    "resize-echoes-ratio": "`session.resize` answers with the applied ratio in `result.ratio`",
    "drop-placement": "`session.move --after/--before` places like a sidebar drop, also above its own anchor",
    "overlay-size-validated": "`session.overlay.open` refuses a `sizePercent` outside 1...100 rather than clamping it",
    "modal-blocks-covers": "a pending GUI ask refuses control-driven covers such as `session.search` with `ask pending`",
    "pick-names-pending-ask": "`pick.open` under a pending GUI ask answers `ask already pending`, naming the ask",
    "quick-validates-input": "`quick.type` refuses a NUL, `quick.text` `--all` with `--lines` and a `--lines` below 1",
    "sidebar-min-width": "`sidebar.width` narrows the sidebar down to the 160-point floor and reads that back",
    "theme-slots-by-sync": "`theme.set`/`theme.list` report `theme` only while not following the system appearance, "
    "`light`/`dark` only while following",
    "keymap-menu": "`keymap.list` reports the menu bar's live key equivalents in `menu`",
    "config-reload-counts": "`config.reload` answers with the ghostty config's diagnostic count in `result.count`",
    "hidden-split-holds-focus": "`session.focus` on a hidden split reads back as `splitFocused`; upstream's shown "
    "left pane takes the focus back while its window is key",
    "capture-reads-command-panes": "`restore.capture` records a `--command` session's program; upstream reads only "
    "the foreground group's leader, there the setuid `login` wrapper whose argv a user cannot read",
}


class LaunchError(Exception):
    pass


class LaunchRefused(LaunchError):
    """A guard rejected the launch env before anything was spawned."""


class StopError(Exception):
    pass


class LeftoverProcesses(StopError):
    pass


@dataclass(frozen=True)
class Profile:
    """What an implementation claims: `parity_level` None means it is below every `since` version.

    `absent` says why a capability the implementation has is missing on this host, for the skip reason.
    """

    parity_level: str | None
    capabilities: frozenset[str]
    absent: Mapping[str, str] = field(default_factory=dict, compare=False, hash=False)

    def at_level(self, since: str) -> bool:
        return self.parity_level is not None and version_tuple(since) <= version_tuple(self.parity_level)


NO_PROFILE = Profile(parity_level=None, capabilities=frozenset())


def version_tuple(version: str) -> tuple[int, ...]:
    match = re.fullmatch(r"v?(\d+(?:\.\d+)*)", version.strip())
    if not match:
        raise ValueError(f"not a dotted version: {version!r}")
    parts = [int(part) for part in match.group(1).split(".")]
    while len(parts) > 1 and parts[-1] == 0:
        parts.pop()
    return tuple(parts)


def zmx_socket_directory(state_dir: str | os.PathLike[str]) -> str:
    """`ZmxSupport.socketDirectory`: FNV-1a 64 of the canonical state dir, under `/tmp`."""
    return f"/tmp/agterm-zmx-{fnv1a64(_foundation_resolved(os.path.realpath(state_dir)))}"


def fnv1a64(text: str) -> str:
    value = 14_695_981_039_346_656_037
    for byte in text.encode():
        value = ((value ^ byte) * 1_099_511_628_211) & 0xFFFF_FFFF_FFFF_FFFF
    return f"{value:016x}"


def _foundation_resolved(path: str) -> str:
    # Darwin Foundation's resolvingSymlinksInPath drops a leading /private when the shorter path exists.
    if sys.platform == "darwin" and path.startswith("/private/") and os.path.exists(path[len("/private") :]):
        return path[len("/private") :]
    return path


def login_shell_blocker() -> str | None:
    """Why Live sessions cannot run for this account, or None: both references wrap a pane in zmx only when
    the password-database login shell is zsh (`ZmxSupport.configuration`), and the pane then execs it."""
    shell = pwd.getpwuid(os.getuid()).pw_shell
    if os.path.basename(shell) != "zsh":
        return f"the password-database login shell is {shell or 'unset'}, not zsh"
    if not os.access(shell, os.X_OK):
        return f"the login shell {shell} is not executable"
    return None


def scrubbed_environ(environ: Mapping[str, str] | None = None) -> dict[str, str]:
    source = os.environ if environ is None else environ
    return {name: value for name, value in source.items() if not name.startswith(SCRUB_PREFIXES)}


_APP_ID_ELEMENT = re.compile(r"[A-Za-z_-][A-Za-z0-9_-]*")
_app_ids_in_use: set[str] = set()
_app_ids_lock = threading.Lock()


def check_app_id(app_id: str | None) -> str:
    """Missing, default or malformed ids reach the user's running instance or abort GApplication."""
    if not app_id:
        raise LaunchRefused("AGTERM_APP_ID is required: without it the launch is forwarded to the running app")
    if app_id == DEFAULT_APP_ID:
        raise LaunchRefused(f"AGTERM_APP_ID {app_id} is the installed app's id")
    elements = app_id.split(".")
    if len(app_id) > 255 or len(elements) < 2 or not all(_APP_ID_ELEMENT.fullmatch(e) for e in elements):
        raise LaunchRefused(f"AGTERM_APP_ID {app_id!r} is not a valid GApplication id")
    return app_id


def unique_app_id() -> str:
    return f"{DEFAULT_APP_ID}.conformance.i{uuid.uuid4().hex[:12]}"


# Harness-owned variables; everything else under AGTERM_*/ZMX_* is inherited and refused.
OWN_VARIABLES = frozenset(
    {
        "AGTERM_STATE_DIR",
        "AGTERM_CONTROL_SOCKET",
        "AGTERM_APP_ID",
        "AGTERM_RESOURCE_ROOT",
        "AGTERM_UITEST_FORCE_SIDEBAR_VISIBLE",
        "AGTERM_UITEST_ENABLE_ZMX",
    }
)


def isolated_runtime_env(env: dict[str, str], runtime_dir: str, parent: Mapping[str, str]) -> None:
    """Point `XDG_RUNTIME_DIR` at `runtime_dir`, keeping the session's sockets reachable by absolute path.

    Wayland accepts an absolute `WAYLAND_DISPLAY`; D-Bus (and through it the AT-SPI bus) takes an explicit
    address. X11 and an inherited `XAUTHORITY` are absolute already.
    """
    parent_runtime = parent.get("XDG_RUNTIME_DIR")
    env["XDG_RUNTIME_DIR"] = runtime_dir
    display = parent.get("WAYLAND_DISPLAY") or "wayland-0"  # libwayland's default, relative to the runtime dir
    if not os.path.isabs(display):
        if parent_runtime:
            env["WAYLAND_DISPLAY"] = os.path.join(parent_runtime, display)
        else:
            env.pop("WAYLAND_DISPLAY", None)  # unresolvable without a runtime dir, as it was in the parent
    if not parent.get("DBUS_SESSION_BUS_ADDRESS") and parent_runtime:
        env["DBUS_SESSION_BUS_ADDRESS"] = f"unix:path={os.path.join(parent_runtime, 'bus')}"


def check_launch_env(
    env: Mapping[str, str], root: str | os.PathLike[str], needs_app_id: bool, needs_runtime_dir: bool = False
) -> None:
    for name in sorted(env):
        if name.startswith(SCRUB_PREFIXES) and name not in OWN_VARIABLES:
            raise LaunchRefused(f"{name} would leak into the instance: only harness-set AGTERM_* are allowed")
    rooted = ["AGTERM_STATE_DIR", "AGTERM_CONTROL_SOCKET", "HOME"] + (["XDG_RUNTIME_DIR"] if needs_runtime_dir else [])
    for name in rooted:
        if not env.get(name):
            raise LaunchRefused(f"{name} must be set")
        try:
            check_under_root(env[name], root)
        except ClientError as error:
            raise LaunchRefused(f"{name}: {error}") from None
    socket_bytes = len(os.fsencode(env["AGTERM_CONTROL_SOCKET"]))
    if socket_bytes >= SOCKET_PATH_CAP:
        raise LaunchRefused(f"socket path is {socket_bytes} bytes, cap {SOCKET_PATH_CAP - 1}: the app would not bind")
    if needs_app_id:
        check_app_id(env.get("AGTERM_APP_ID"))


@dataclass
class Instance:
    process: subprocess.Popen[bytes]
    root: Path
    state_dir: Path
    home: Path
    socket: Path
    env: dict[str, str]
    log: Path
    app_id: str | None
    zmx_dir: str
    version: dict[str, Any] = field(default_factory=dict)

    @property
    def pid(self) -> int:
        return self.process.pid

    def client(self, timeout: float | None = None) -> Client:
        if timeout is None:
            return Client(self.socket, self.root)
        return Client(self.socket, self.root, timeout=timeout)

    def request(self, cmd: str, target: str | None = None, **args: Any) -> Response:
        return self.client().request(cmd, target, **args)

    def log_tail(self, lines: int = 20) -> str:
        try:
            return "\n".join(self.log.read_text(errors="replace").splitlines()[-lines:])
        except OSError:
            return ""


class Adapter:
    """One implementation under test. Subclasses supply the executable and platform env."""

    name: ClassVar[str] = ""
    needs_app_id: ClassVar[bool] = False
    # A Linux desktop app: its runtime dir is the live session's, so the instance gets a private one.
    needs_runtime_dir: ClassVar[bool] = False

    def __init__(self, bin: Path | None, ctl: Path | None = None) -> None:
        # Absolute now: the child starts in the isolated HOME, where a relative path would not resolve.
        self.bin = _absolute(bin)
        self._ctl = _absolute(ctl)

    # -- what the implementation claims

    @property
    def profile(self) -> Profile:
        raise NotImplementedError

    @property
    def capabilities(self) -> frozenset[str]:
        return self.profile.capabilities

    def has(self, capability: str) -> bool:
        """For a test that branches on a capability; a name outside the vocabulary would pick a branch silently."""
        if capability not in CAPABILITIES:
            raise ValueError(f"unknown capability {capability!r} (known: {', '.join(sorted(CAPABILITIES))})")
        return capability in self.capabilities

    @property
    def parity_level(self) -> str | None:
        return self.profile.parity_level

    @property
    def available(self) -> bool:
        return self.bin is not None

    @property
    def ctl_path(self) -> Path | None:
        return self._ctl if self._ctl is not None else self.default_ctl()

    def default_ctl(self) -> Path | None:
        return None

    def executable(self) -> list[str]:
        if self.bin is None:
            raise LaunchError(f"{self.name}: no binary to launch")
        return [os.fspath(self.bin)]

    def instance_executable(self, root: Path) -> list[str]:
        """The command one instance runs, given its root; a relaunch on the same root gets the same one."""
        return self.executable()

    def platform_env(self, env: dict[str, str]) -> None:
        """Hook for adapter-specific variables; `env` is already isolated."""

    def ghostty_config(self) -> str:
        """The agterm-scoped `<state>/config/ghostty.conf` the instance starts with."""
        return GHOSTTY_SHELL_CONFIG + GHOSTTY_ISOLATION_CONFIG

    # -- launch

    def build_env(self, root: Path, app_id: str | None = None) -> dict[str, str]:
        home, state = root / "home", root / "state"
        env = scrubbed_environ()
        # A Live pane's zsh restores an inherited ZDOTDIR (as GHOSTTY_ZSH_ZDOTDIR) and sources the user's startup
        # files from it; without one it reads the isolated HOME.
        for name in ZSH_STARTUP_VARIABLES:
            env.pop(name, None)
        env.update(
            HOME=os.fspath(home),
            XDG_CONFIG_HOME=os.fspath(home / ".config"),
            XDG_DATA_HOME=os.fspath(home / ".local/share"),
            XDG_STATE_HOME=os.fspath(home / ".local/state"),
            XDG_CACHE_HOME=os.fspath(home / ".cache"),
            PATH=MINIMAL_PATH,
            SHELL="/bin/sh",
            BROWSER="true",
            AGTERM_STATE_DIR=os.fspath(state),
            AGTERM_CONTROL_SOCKET=os.fspath(state / "agterm.sock"),
        )
        if self.needs_app_id:
            env["AGTERM_APP_ID"] = app_id or unique_app_id()
        if self.needs_runtime_dir:
            isolated_runtime_env(env, os.fspath(root / RUNTIME_DIR_NAME), os.environ)
            # An inherited portal opt-in would route launches through the user's session, past the mimeapps no-op.
            env["GIO_USE_PORTALS"] = "0"
            env.pop("GTK_USE_PORTAL", None)
        self.platform_env(env)
        return env

    def launch(
        self, timeout: float = LAUNCH_TIMEOUT, app_id: str | None = None, settings: Mapping[str, Any] | None = None
    ) -> Instance:
        """`settings` seeds `<state>/settings.json`, which both references read at launch (`restoreMode`, ...)."""
        self.executable()  # refuses before anything exists
        root = make_root()
        try:
            env = self.build_env(root, app_id)
            check_launch_env(env, root, self.needs_app_id, self.needs_runtime_dir)
            app_id = env.get("AGTERM_APP_ID")
            _prepare_tree(root, Path(env["HOME"]), Path(env["AGTERM_STATE_DIR"]), self.ghostty_config(), settings)
            command = self.instance_executable(root)
            if self.needs_runtime_dir:
                os.mkdir(env["XDG_RUNTIME_DIR"], 0o700)  # the XDG spec's mode
                write_noop_launchers(Path(env["XDG_CONFIG_HOME"]), Path(env["XDG_DATA_HOME"]))
            if app_id is not None:
                _claim_app_id(app_id)
        except BaseException:
            shutil.rmtree(root, ignore_errors=True)
            raise
        log = root / "app.log"
        try:
            process = _spawn(command, env, log, "wb")
        except BaseException:
            _release_app_id(app_id)
            shutil.rmtree(root, ignore_errors=True)
            raise
        instance = Instance(
            process=process,
            root=root,
            state_dir=Path(env["AGTERM_STATE_DIR"]),
            home=Path(env["HOME"]),
            socket=Path(env["AGTERM_CONTROL_SOCKET"]),
            env=env,
            log=log,
            app_id=app_id,
            zmx_dir=zmx_socket_directory(env["AGTERM_STATE_DIR"]),
        )
        return self._serving(instance, timeout)

    def relaunch(self, instance: Instance, timeout: float = LAUNCH_TIMEOUT) -> Instance:
        """Quit `instance` with SIGTERM, then start the binary again on the same root, env and state directory.

        Nothing is reaped and no leftover is checked in between: a Live daemon outliving the quit is what the next
        launch adopts. A relaunch failing before the new process exists reports what the old one left instead.
        The returned instance replaces `instance`; stop that one, never the old.
        """
        command = self.instance_executable(instance.root)
        killed = _terminate(instance.process)
        _release_app_id(instance.app_id)
        try:
            if killed:
                raise StopError(f"pid {instance.pid} ignored SIGTERM for {STOP_TIMEOUT}s and was killed")
            check_launch_env(instance.env, instance.root, self.needs_app_id, self.needs_runtime_dir)
            if instance.app_id is not None:
                _claim_app_id(instance.app_id)
            try:
                process = _spawn(command, instance.env, instance.log, "ab")
            except BaseException:
                _release_app_id(instance.app_id)
                raise
        except BaseException as error:
            if (leftovers := _leftovers_error(instance.zmx_dir)) is not None:
                raise leftovers from error
            raise
        relaunched = Instance(
            process=process,
            root=instance.root,
            state_dir=instance.state_dir,
            home=instance.home,
            socket=instance.socket,
            env=instance.env,
            log=instance.log,
            app_id=instance.app_id,
            zmx_dir=instance.zmx_dir,
        )
        return self._serving(relaunched, timeout)

    def _serving(self, instance: Instance, timeout: float) -> Instance:
        try:
            instance.version = wait_for_socket(instance, timeout)
        except BaseException:
            self.stop(instance, cleanup=False)
            raise
        return instance

    # -- stop

    def stop(self, instance: Instance, cleanup: bool = True) -> None:
        """Reap daemons, SIGTERM the launched pid only, then require no zmx/session-host left behind."""
        try:
            refusals: list[str] = []
            try:
                if (code := instance.process.poll()) is not None:
                    refusals = [f"the app had already exited with {code}"]
                elif find_leftovers(instance.zmx_dir) or _lists_daemons(instance):
                    # Listed too: a daemon still starting has no process yet and would outlive the quit.
                    refusals = reap_live_sessions(instance)
            finally:
                killed = _terminate(instance.process)
            helper_problem = self.stop_helpers(instance)
            leftovers = _leftovers_error(instance.zmx_dir, refusals)
        finally:
            _release_app_id(instance.app_id)
        if leftovers is not None:
            raise leftovers
        if killed:
            raise StopError(f"pid {instance.pid} ignored SIGTERM for {STOP_TIMEOUT}s and was killed")
        if helper_problem is not None:
            raise StopError(helper_problem)
        if cleanup:
            shutil.rmtree(instance.root, ignore_errors=True)
            _remove_empty_dir(instance.zmx_dir)

    def stop_helpers(self, instance: Instance) -> str | None:
        """Hook: end a helper the implementation detaches on purpose once the app is gone; returns what went wrong."""
        return None


def _leftovers_error(zmx_dir: str, refusals: Iterable[str] = ()) -> LeftoverProcesses | None:
    leftovers = find_leftovers(zmx_dir)
    if not leftovers:
        return None
    # Cut: on macOS the command carries the whole `ps -E` environment.
    described = ", ".join(f"{pid} {command[:300]}" for pid, _, command in leftovers)
    reaping = f"; reaping: {'; '.join(refusals)}" if refusals else ""
    return LeftoverProcesses(f"processes left under ZMX_DIR {zmx_dir}: {described}{reaping}")


def _absolute(path: Path | None) -> Path | None:
    return Path(os.path.abspath(path)) if path is not None else None


def make_root() -> Path:
    return Path(tempfile.mkdtemp(prefix="agt-", dir=TEMP_PARENT))


def _spawn(command: list[str], env: dict[str, str], log: Path, mode: str) -> subprocess.Popen[bytes]:
    with open(log, mode) as sink:
        return subprocess.Popen(
            command,
            env=env,
            cwd=env["HOME"],
            stdin=subprocess.DEVNULL,
            stdout=sink,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )


def _prepare_tree(
    root: Path, home: Path, state: Path, ghostty_config: str, settings: Mapping[str, Any] | None = None
) -> None:
    for directory in (home / ".claude", home / ".codex", state / "config"):
        directory.mkdir(parents=True)
    # `FirstRunWelcome.hasPriorState` reads this directory: its presence skips the welcome dialog.
    (state / "windows").mkdir()
    (state / "config" / "ghostty.conf").write_text(ghostty_config)
    (home / ".profile").write_text(SHELL_PROFILE)
    if settings is not None:
        (state / "settings.json").write_text(json.dumps(settings))


def write_noop_launchers(config_home: Path, data_home: Path) -> None:
    types = ";".join(NOOP_MIME_TYPES)
    # Absolute: xdg-utils resolves Exec's first word to a file and skips an entry naming a shell builtin.
    noop = shutil.which("true", path=MINIMAL_PATH) or "true"
    applications = data_home / "applications"
    applications.mkdir(parents=True, exist_ok=True)
    (applications / NOOP_DESKTOP_ID).write_text(
        f"[Desktop Entry]\nType=Application\nName=agterm conformance no-op\nExec={noop} %u\nNoDisplay=true\n"
        f"MimeType={types};\n"
    )
    entries = "".join(f"{mime}={NOOP_DESKTOP_ID};\n" for mime in NOOP_MIME_TYPES)
    config_home.mkdir(parents=True, exist_ok=True)
    (config_home / "mimeapps.list").write_text(f"[Default Applications]\n{entries}\n[Added Associations]\n{entries}")


def _claim_app_id(app_id: str) -> None:
    with _app_ids_lock:
        if app_id in _app_ids_in_use:
            raise LaunchRefused(f"AGTERM_APP_ID {app_id} is already used by a running instance")
        _app_ids_in_use.add(app_id)


def _release_app_id(app_id: str | None) -> None:
    if app_id is not None:
        with _app_ids_lock:
            _app_ids_in_use.discard(app_id)


def wait_for_socket(instance: Instance, timeout: float) -> dict[str, Any]:
    """Poll `version` until it answers; the process wait doubles as the poll interval and an exit check."""
    deadline = time.monotonic() + timeout
    last: Exception | None = None
    while True:
        try:
            per_request = max(POLL_INTERVAL, min(2.0, deadline - time.monotonic()))
            response = instance.client(timeout=per_request).request("version")
        except (ConnectFailed, NoResponse, ClientTimeout) as error:
            last = error
        else:
            if response.ok:
                return response.result or {}
            raise LaunchError(f"version refused during launch: {response.error!r}")
        if time.monotonic() >= deadline:
            raise LaunchError(
                f"socket {instance.socket} not serving after {timeout}s ({last}); app log:\n{instance.log_tail()}"
            )
        try:
            code = instance.process.wait(timeout=POLL_INTERVAL)
        except subprocess.TimeoutExpired:
            continue
        raise LaunchError(f"app exited with {code} before serving {instance.socket}; app log:\n{instance.log_tail()}")


def reap_live_sessions(instance: Instance, settle: float = REAP_SETTLE) -> list[str]:
    """Best effort over the protocol; returns the refusals, and whatever survives is reported by the leftover check.

    Re-lists after every kill: killing a left pane promotes its split survivor into the left slot, so a
    selector read before the kill no longer names that survivor. A claimed daemon zmx does not list yet
    (`observation: absent`, as right after it starts) refuses a kill as not running, and a `pendingClose` one
    refuses it outright, so both are waited for until `settle` runs out.
    """
    refusals: list[str] = []
    deadline = time.monotonic() + settle
    try:
        tried: set[str] = set()
        while True:
            entry, waiting = _next_claimed(instance, tried, time.monotonic() < deadline)
            if entry is None:
                if not waiting:
                    break
                threading.Event().wait(POLL_INTERVAL)
                continue
            tried.add(_daemon_key(entry))
            args: dict[str, Any] = {"pane": entry["pane"], "force": True}
            if entry.get("windowID"):
                args["window"] = entry["windowID"]
            response = instance.request("zmx.kill", entry["sessionID"], **args)
            if not response.ok:
                refusals.append(f"zmx.kill {_daemon_key(entry)}: {response.error}")
        prune = instance.request("zmx.prune")
        if not prune.ok:
            refusals.append(f"zmx.prune: {prune.error}")
        listing = instance.request("zmx.list")
        inventory = (listing.result or {}).get("zmx") if listing.ok else None
        for entry in inventory.get("entries") or [] if isinstance(inventory, dict) else []:
            if isinstance(entry, dict):
                refusals.append(f"still listed: {entry.get('daemon')} {entry.get('state')}/{entry.get('observation')}")
    except ClientError as error:
        refusals.append(f"reaping: {error}")
    return refusals


def _lists_daemons(instance: Instance) -> bool:
    try:
        listing = instance.request("zmx.list")
    except ClientError:
        return False
    inventory = (listing.result or {}).get("zmx") if listing.ok else None
    return bool(isinstance(inventory, dict) and inventory.get("entries"))


def _next_claimed(instance: Instance, tried: set[str], wait_absent: bool) -> tuple[dict[str, Any] | None, bool]:
    """The next claimed daemon to kill, and whether one not yet killable is still worth waiting for."""
    listing = instance.request("zmx.list")
    inventory = (listing.result or {}).get("zmx") if listing.ok else None
    entries = inventory.get("entries") if isinstance(inventory, dict) else None
    waiting = False
    for entry in entries if isinstance(entries, list) else []:
        # A closed session's daemon outlives it for the undo window (3s upstream), then the app kills it.
        if wait_absent and isinstance(entry, dict) and entry.get("state") == "pendingClose":
            waiting = True
        if (
            isinstance(entry, dict)
            and entry.get("state") == "claimed"
            and entry.get("sessionID")
            and entry.get("pane")
            and _daemon_key(entry) not in tried
        ):
            if wait_absent and entry.get("observation") == "absent":
                waiting = True
                continue
            return entry, False
    return None, waiting


def _daemon_key(entry: dict[str, Any]) -> str:
    return str(entry.get("daemon") or (entry["sessionID"], entry["pane"], entry.get("windowID")))


def _terminate(process: subprocess.Popen[bytes]) -> bool:
    if process.poll() is not None:
        return False
    process.send_signal(signal.SIGTERM)
    try:
        process.wait(timeout=STOP_TIMEOUT)
        return False
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=STOP_TIMEOUT)
        return True


def find_leftovers(zmx_dir: str) -> list[tuple[int, str, str]]:
    """(pid, name, command) of zmx / agterm-session-host processes carrying this instance's `ZMX_DIR`."""
    marker = f"ZMX_DIR={zmx_dir}"
    return [
        (pid, name, command)
        for pid, name, command, words in _processes()
        if name in LEFTOVER_NAMES and (marker in words or hosts_zmx_dir(words, zmx_dir))
    ]


def hosts_zmx_dir(words: Iterable[str], zmx_dir: str) -> bool:
    """`agterm-session-host host <zmx_dir>`, the argv `ClientConnector.startHost` spawns the macOS host with."""
    words = list(words)
    return any(
        os.path.basename(word) == SESSION_HOST and words[index + 1 : index + 3] == ["host", zmx_dir]
        for index, word in enumerate(words)
    )


@dataclass(frozen=True)
class ProcessIdentity:
    """The running image's resolved path, and argv followed by the environment (one `ps -E` line split on macOS)."""

    executable: str
    words: tuple[str, ...]


def process_identity(pid: int) -> ProcessIdentity | None:
    """None when the process is gone or unreadable."""
    if os.path.isdir("/proc/self"):
        try:
            executable = os.readlink(f"/proc/{pid}/exe").removesuffix(" (deleted)")
            words = _proc_words(pid)
        except OSError:
            return None
    else:
        executable = _darwin_pidpath(pid)
        line = subprocess.run(
            ["ps", "-E", "-ww", "-o", "command=", "-p", str(pid)], capture_output=True, text=True, check=False
        ).stdout.strip()
        if executable is None or not line:
            return None
        words = tuple(line.split(" "))
    return ProcessIdentity(os.path.realpath(executable), words)


def _darwin_pidpath(pid: int) -> str | None:
    import ctypes

    buffer = ctypes.create_string_buffer(4096)  # PROC_PIDPATHINFO_MAXSIZE
    length = ctypes.CDLL(None).proc_pidpath(ctypes.c_int(pid), buffer, ctypes.c_uint32(len(buffer)))
    return buffer.value.decode(errors="replace") if length > 0 else None


def _processes() -> Iterable[tuple[int, str, str, list[str]]]:
    """(pid, program name, command, argv and environ words) for every readable process."""
    if os.path.isdir("/proc/self"):
        yield from _proc_processes()
    else:
        yield from _ps_processes()


def _proc_processes() -> Iterable[tuple[int, str, str, list[str]]]:
    for entry in os.scandir("/proc"):
        if not entry.name.isdigit():
            continue
        try:
            words = _proc_words(int(entry.name))
            executable = os.readlink(f"/proc/{entry.name}/exe")
        except OSError:
            continue  # gone, or another user's process
        executable = executable.removesuffix(" (deleted)")
        yield int(entry.name), os.path.basename(executable), executable, list(words)


def _proc_words(pid: int) -> tuple[str, ...]:
    words: list[str] = []
    for part in ("cmdline", "environ"):
        with open(f"/proc/{pid}/{part}", "rb") as source:
            words.extend(item.decode(errors="replace") for item in source.read().split(b"\0") if item)
    return tuple(words)


def _ps_processes() -> Iterable[tuple[int, str, str, list[str]]]:
    # macOS `ps -E` appends the environment to the command line for the user's own processes.
    output = subprocess.run(
        ["ps", "-E", "-ww", "-o", "pid=,command=", "-u", str(os.getuid())],
        capture_output=True,
        text=True,
        check=False,
    ).stdout
    for line in output.splitlines():
        pid, _, command = line.strip().partition(" ")
        if pid.isdigit():
            # A command line cannot tell a space inside the executable path from an argument separator.
            yield (int(pid), os.path.basename(command.split(" ", 1)[0]), command, command.split(" "))


def _remove_empty_dir(path: str) -> None:
    try:
        os.rmdir(path)
    except OSError:
        pass
