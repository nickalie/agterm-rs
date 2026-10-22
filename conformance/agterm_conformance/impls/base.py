"""Implementation adapter contract: isolated launch, socket wait, own-pid stop, leftover-process check.

Safety is an allowlist: every path the instance is told about lives under a fresh `mkdtemp` root, and the
guards refuse a launch env that points anywhere else before anything is spawned.
"""

from __future__ import annotations

import functools
import json
import os
import pwd
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import uuid
from collections.abc import Iterable, Mapping, Sequence
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
from agterm_conformance.plugin import RUST_PREFIX, SCRUB_PREFIXES, UPSTREAM_PREFIX
from agterm_conformance.vocabulary import (
    LIVE,
    UPSTREAM,
    Vocabulary,
)

# Both references refuse to bind at `path.utf8.count >= 104` (Darwin's sun_path, mirrored on Linux).
SOCKET_PATH_CAP = 104
# `/tmp`, not `$TMPDIR`: a macOS runner's `/var/folders/...` temp dir eats most of the socket cap.
# `CONFORMANCE_TMP` (not `AGTERM_`- or `NGTERM_`-prefixed, which the plugin scrubs) gives run-reference-linux.sh its own
# parent.
TEMP_PARENT = os.environ.get("CONFORMANCE_TMP") or "/tmp"
MINIMAL_PATH = "/usr/bin:/bin"
# The startup variables a Live pane's shell integration hands on to the login shell (zsh's ZDOTDIR, bash's ENV and
# HISTFILE), which would point the pane at the user's real startup and history files.
SHELL_STARTUP_VARIABLES = ("ZDOTDIR", "GHOSTTY_ZSH_ZDOTDIR", "ENV", "BASH_ENV", "GHOSTTY_BASH_ENV", "HISTFILE")
DEFAULT_APP_ID = "com.nickalie.ngterm"
# The Linux fork's id, which its installed GTK build (and an older install of this one) still holds.
FORK_APP_ID = "io.github.melonamin.agterm"
SESSION_HOST = "agterm-session-host"
# The references' live-session processes: upstream's zmx daemons and the macOS session host. A vocabulary with a
# session server (the Rust build's) names its own leftovers instead (`find_leftovers`).
LEFTOVER_NAMES = frozenset({"zmx", SESSION_HOST})
# What a session server leaves in its namespace directory once it stopped: the lock file it held (kept on purpose,
# crates/ngterm-session/src/endpoint/unix.rs) and, should it have stopped unclean, its socket.
SERVER_NAMESPACE_FILES = frozenset({"server.lock", "server.sock"})
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
# bus's FileManager1 instead, which no mimeapps entry reaches: no test may drive it under the `session` display.
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
# Every Linux instance draws into a headless weston of its own inside its private runtime dir and talks to a session
# bus of its own there, so no window or notification reaches the user's desktop; `session` uses the user's display and
# bus instead, for debugging only.
DISPLAY_MODE_VARIABLE = "CONFORMANCE_LINUX_DISPLAY"
DISPLAY_MODES = ("headless", "session")
WESTON_SOCKET = "wayland-0"
# GL: under the no-op renderer the fork dies with SIGSEGV before serving.
WESTON_RENDERER = "gl"
# weston runs on glvnd's Mesa vendor where one is installed, and its clients follow it onto that device. Through
# NVIDIA's EGL GDK settles on a GLES context while the fork's GtkGLArea allows desktop GL only, so no pane realizes.
GLVND_VENDOR_DIRS = ("/usr/share/glvnd/egl_vendor.d", "/etc/glvnd/egl_vendor.d")
EGL_VENDOR_VARIABLE = "__EGL_VENDOR_LIBRARY_FILENAMES"
# The Rust build's GPUI frontend renders through wgpu on Vulkan, with GL beside it, and in this harness on
# Mesa's lavapipe and llvmpipe only, never on the machine's GPU driver: the Vulkan loader gets lavapipe's ICD manifest
# as its one driver (run-rust-linux.sh resolves it through scripts/fetch-lavapipe.sh into this variable, which is not
# AGTERM_- or NGTERM_-prefixed so the plugin keeps it), GL is forced onto llvmpipe, and every inherited loader or
# driver selection is dropped. No lavapipe fails the launch; there is no fallback.
LAVAPIPE_ICD_VARIABLE = "CONFORMANCE_LAVAPIPE_ICD"
LAVAPIPE_LIBRARY_PREFIX = "libvulkan_lvp"
# Inherited selections that would steer the loader, GL or wgpu onto another device or add layers to the process.
GPU_SELECTION_PREFIXES = ("VK_", "WGPU_", "__NV_", "__GLX_", "DRI_PRIME", "MESA_VK_DEVICE_SELECT", "MESA_LOADER_")
# GPUI panics on a Wayland compositor without a `wl_seat`, which headless weston offers only with this flag.
FAKE_SEAT_ARG = "--fake-seat"
WESTON_SIZE = (1920, 1080)
# No idle lock, no panel, and no `weston-keyboard` input-method client: the desktop shell's own helper is its only one.
WESTON_CONFIG = "[core]\nidle-time=0\n\n[shell]\npanel-position=none\nlocking=false\n\n[input-method]\npath=\n"
DBUS_SOCKET = "bus"
# No service directories: nothing is activated, so a notification, Show in Files or the AT-SPI bus lookup finds no
# owner instead of starting a desktop service. The fork's GApplication still registers its unique id here.
DBUS_CONFIG = """<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:path={path}</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"""
HEADLESS_HELPERS = ("weston", "dbus-daemon")
LAUNCH_TIMEOUT = 30.0
STOP_TIMEOUT = 10.0
# How long an app outliving STOP_TIMEOUT gets to write its core after SIGABRT before it is killed.
ABORT_GRACE = 10.0
PROC_ROOT = Path("/proc")
REAP_SETTLE = 8.0
LEFTOVER_GRACE = 5.0
POLL_INTERVAL = 0.05

# The capability vocabulary `requires(...)` and `Adapter.has(...)` may name. A reason for each absence lives on the
# test marker, or beside the branch a test takes on `has`.
CAPABILITIES: Mapping[str, str] = {
    "posix-shell": "panes run /bin/sh, so `session type` + `session.text` round-trips are deterministic",
    "live-sessions": "restoreMode live keeps panes in live sessions that outlive the app (upstream's zmx daemons; the "
    "Rust build's one session server per instance, ngterm-session)",
    "live-any-shell": "Rust-only: Live sessions in a zsh, bash, fish or PowerShell login shell, each started with its "
    "shell integration (Ghostty's, ngterm's own for PowerShell); claimed where the password-database login shell is "
    "one of them and ngterm-session is staged",
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


def check_app_id(app_id: str | None, prefix: str = UPSTREAM_PREFIX) -> str:
    """Missing, default or malformed ids reach the user's running instance or abort GApplication. `prefix` names the
    implementation's variables (`Adapter.env_prefix`), so the refusal names the one it reads."""
    variable = f"{prefix}APP_ID"
    if not app_id:
        raise LaunchRefused(f"{variable} is required: without it the launch is forwarded to the running app")
    if app_id in (DEFAULT_APP_ID, FORK_APP_ID):
        raise LaunchRefused(f"{variable} {app_id} is the installed app's id")
    elements = app_id.split(".")
    if len(app_id) > 255 or len(elements) < 2 or not all(_APP_ID_ELEMENT.fullmatch(e) for e in elements):
        raise LaunchRefused(f"{variable} {app_id!r} is not a valid GApplication id")
    return app_id


def unique_app_id() -> str:
    return f"{DEFAULT_APP_ID}.conformance.i{uuid.uuid4().hex[:12]}"


# The harness-owned variables, after the implementation's prefix (`Adapter.env_prefix`: upstream's and the fork's
# `AGTERM_`, the Rust build's `NGTERM_`); everything else under a scrubbed prefix is inherited and refused.
OWN_VARIABLE_NAMES = (
    "STATE_DIR",
    "CONTROL_SOCKET",
    "APP_ID",
    "RESOURCE_ROOT",
    "UITEST_FORCE_SIDEBAR_VISIBLE",
    "UITEST_SHOW_WELCOME",
    # The Rust build's log filter, set only by tools/ghostty_probe.py and tools/chrome_probe.py; an inherited one is
    # still scrubbed.
    "LOG",
)


def own_variables(prefix: str) -> frozenset[str]:
    """The harness-owned variables of an implementation whose variables start with `prefix`, with each vocabulary's
    UI-test opt-in (set only for the references that read one, swift_macos.py)."""
    return frozenset({*(prefix + name for name in OWN_VARIABLE_NAMES), UPSTREAM.ui_test_opt_in, LIVE.ui_test_opt_in})


# Every implementation's harness-owned variables.
OWN_VARIABLES = own_variables(UPSTREAM_PREFIX) | own_variables(RUST_PREFIX)


def display_mode(environ: Mapping[str, str] | None = None) -> str:
    mode = (os.environ if environ is None else environ).get(DISPLAY_MODE_VARIABLE) or DISPLAY_MODES[0]
    if mode not in DISPLAY_MODES:
        raise LaunchRefused(f"{DISPLAY_MODE_VARIABLE}={mode!r} is not one of {', '.join(DISPLAY_MODES)}")
    return mode


def isolated_runtime_env(
    env: dict[str, str], runtime_dir: str, parent: Mapping[str, str], headless: bool = False
) -> None:
    """Point `XDG_RUNTIME_DIR` at `runtime_dir`, keeping the session's sockets reachable by absolute path.

    Wayland accepts an absolute `WAYLAND_DISPLAY`; D-Bus (and through it the AT-SPI bus) takes an explicit
    address. X11 and an inherited `XAUTHORITY` are absolute already. `headless` points Wayland and D-Bus at the
    instance's own weston and bus instead and drops X11, so GTK cannot fall back to the user's Xwayland.
    """
    parent_runtime = parent.get("XDG_RUNTIME_DIR")
    env["XDG_RUNTIME_DIR"] = runtime_dir
    display = parent.get("WAYLAND_DISPLAY") or "wayland-0"  # libwayland's default, relative to the runtime dir
    if headless:
        env["WAYLAND_DISPLAY"] = os.path.join(runtime_dir, WESTON_SOCKET)
        env["GDK_BACKEND"] = "wayland"
        env.pop("DISPLAY", None)
        env["DBUS_SESSION_BUS_ADDRESS"] = f"unix:path={os.path.join(runtime_dir, DBUS_SOCKET)}"
        env.pop("AT_SPI_BUS_ADDRESS", None)  # GTK would reach the user's accessibility bus directly
        return
    if not os.path.isabs(display):
        if parent_runtime:
            env["WAYLAND_DISPLAY"] = os.path.join(parent_runtime, display)
        else:
            env.pop("WAYLAND_DISPLAY", None)  # unresolvable without a runtime dir, as it was in the parent
    if not parent.get("DBUS_SESSION_BUS_ADDRESS") and parent_runtime:
        env["DBUS_SESSION_BUS_ADDRESS"] = f"unix:path={os.path.join(parent_runtime, 'bus')}"


# Where the app's config, data, state and cache resolve; each must lie under the instance root.
XDG_HOMES = ("XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME")


def check_launch_env(
    env: Mapping[str, str],
    root: str | os.PathLike[str],
    needs_app_id: bool,
    needs_runtime_dir: bool = False,
    headless: bool = False,
    prefix: str = UPSTREAM_PREFIX,
) -> None:
    """`prefix` is the implementation's (`Adapter.env_prefix`): its harness-owned variables are the only ones under a
    scrubbed prefix the launch may carry, another implementation's included."""
    own = own_variables(prefix)
    for name in sorted(env):
        if name.startswith(SCRUB_PREFIXES) and name not in own:
            raise LaunchRefused(f"{name} would leak into the instance: only harness-set {prefix}* are allowed")
    rooted = [f"{prefix}STATE_DIR", f"{prefix}CONTROL_SOCKET", "HOME", *XDG_HOMES]
    rooted += ["XDG_RUNTIME_DIR"] if needs_runtime_dir else []
    if headless:
        if "DISPLAY" in env:
            raise LaunchRefused("DISPLAY is set: a headless instance would reach the user's X server")
        if "AT_SPI_BUS_ADDRESS" in env:
            raise LaunchRefused("AT_SPI_BUS_ADDRESS is set: a headless instance would reach the user's a11y bus")
        rooted.append("WAYLAND_DISPLAY")
        address = env.get("DBUS_SESSION_BUS_ADDRESS", "")
        if not address.startswith("unix:path=") or "," in address or ";" in address:
            raise LaunchRefused(f"DBUS_SESSION_BUS_ADDRESS {address!r} is not the instance's own unix:path= bus")
        try:
            check_under_root(address.removeprefix("unix:path="), root)
        except ClientError as error:
            raise LaunchRefused(f"DBUS_SESSION_BUS_ADDRESS: {error}") from None
    for name in rooted:
        if not env.get(name):
            raise LaunchRefused(f"{name} must be set")
        try:
            check_under_root(env[name], root)
        except ClientError as error:
            raise LaunchRefused(f"{name}: {error}") from None
    socket_bytes = len(os.fsencode(env[f"{prefix}CONTROL_SOCKET"]))
    if socket_bytes >= SOCKET_PATH_CAP:
        raise LaunchRefused(f"socket path is {socket_bytes} bytes, cap {SOCKET_PATH_CAP - 1}: the app would not bind")
    if needs_app_id:
        check_app_id(env.get(f"{prefix}APP_ID"), prefix)


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
    # The live-session socket directory the vocabulary derives from the state dir; leftovers are looked for under it.
    live_dir: str
    version: dict[str, Any] = field(default_factory=dict)
    # What the implementation calls the live-session group (agterm_conformance.vocabulary).
    vocabulary: Vocabulary = UPSTREAM
    # The prefix of the implementation's own variables (`Adapter.env_prefix`).
    env_prefix: str = UPSTREAM_PREFIX
    # The instance's own headless weston and session bus; a relaunch keeps both.
    compositor: subprocess.Popen[bytes] | None = None
    session_bus: subprocess.Popen[bytes] | None = None

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
        return _tail(self.log, lines)

    def variable(self, name: str) -> str:
        """The implementation's name for its variable `name` (`STATE_DIR`: `AGTERM_STATE_DIR`, `NGTERM_STATE_DIR`)."""
        return self.env_prefix + name


class Adapter:
    """One implementation under test. Subclasses supply the executable and platform env."""

    name: ClassVar[str] = ""
    # The name the implementation gives its state directory under the platform's application support directory, its
    # control socket (`<name>.sock`) and its live-session socket directories (`/tmp/<name>-zmx-*` for upstream and the
    # fork, `/tmp/<name>-live-*` for the Rust build: the vocabulary's).
    app_dir_name: ClassVar[str] = "agterm"
    # The implementation's CLI: the executable beside its binary and the name its usage lines and help print.
    ctl_name: ClassVar[str] = "agtermctl"
    # The prefix of the implementation's own variables, the ones the harness sets (`STATE_DIR`, `CONTROL_SOCKET`,
    # `APP_ID`, ...) and the ones its panes carry: upstream's and the fork's `AGTERM_`, the Rust build's `NGTERM_`.
    env_prefix: ClassVar[str] = UPSTREAM_PREFIX
    needs_app_id: ClassVar[bool] = False
    # A Linux desktop app: its runtime dir is the live session's, so the instance gets a private one.
    needs_runtime_dir: ClassVar[bool] = False
    # Arguments appended to the headless weston's command line, set only by tools/chrome_probe.py (`--debug`, which
    # lets weston-screenshooter read the output); `weston_args` adds an adapter's own.
    compositor_args: ClassVar[tuple[str, ...]] = ()
    # The names the implementation answers the shared tests' live-session group by (agterm_conformance.vocabulary).
    vocabulary: ClassVar[Vocabulary] = UPSTREAM

    def __init__(self, bin: Path | None, ctl: Path | None = None) -> None:
        # Absolute now: the child starts in the isolated HOME, where a relative path would not resolve.
        self.bin = _absolute(bin)
        self._ctl = _absolute(ctl)

    @classmethod
    def variable(cls, name: str) -> str:
        """The implementation's name for its variable `name` (`STATE_DIR`: `AGTERM_STATE_DIR`, `NGTERM_STATE_DIR`)."""
        return cls.env_prefix + name

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

    @property
    def headless(self) -> bool:
        return self.needs_runtime_dir and display_mode() == "headless"

    def weston_args(self) -> tuple[str, ...]:
        """What the headless weston's command line gets appended: `compositor_args`, and an adapter's own."""
        return self.compositor_args

    def platform_env(self, env: dict[str, str]) -> None:
        """Hook for adapter-specific variables; `env` is already isolated."""

    def first_run_env(self, env: dict[str, str]) -> None:
        """Hook for what a first-run launch needs beyond a state dir with no prior state."""

    def ghostty_config(self) -> str:
        """The agterm-scoped `<state>/config/ghostty.conf` the instance starts with."""
        return GHOSTTY_SHELL_CONFIG + GHOSTTY_ISOLATION_CONFIG

    # -- launch

    def build_env(self, root: Path, app_id: str | None = None) -> dict[str, str]:
        home, state = root / "home", root / "state"
        env = scrubbed_environ()
        # A Live pane's zsh restores an inherited ZDOTDIR (as GHOSTTY_ZSH_ZDOTDIR) and sources the user's startup
        # files from it, a bash one its ENV and HISTFILE; without them they read the isolated HOME.
        for name in SHELL_STARTUP_VARIABLES:
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
        )
        env[self.variable("STATE_DIR")] = os.fspath(state)
        env[self.variable("CONTROL_SOCKET")] = os.fspath(state / f"{self.app_dir_name}.sock")
        if self.needs_app_id:
            env[self.variable("APP_ID")] = app_id or unique_app_id()
        if self.needs_runtime_dir:
            isolated_runtime_env(env, os.fspath(root / RUNTIME_DIR_NAME), os.environ, self.headless)
            # An inherited portal opt-in would route launches through the user's session, past the mimeapps no-op.
            env["GIO_USE_PORTALS"] = "0"
            env.pop("GTK_USE_PORTAL", None)
        self.platform_env(env)
        return env

    def launch(
        self,
        timeout: float = LAUNCH_TIMEOUT,
        app_id: str | None = None,
        settings: Mapping[str, Any] | None = None,
        first_run: bool = False,
    ) -> Instance:
        """`settings` seeds `<state>/settings.json`, which both references read at launch (`restoreMode`, ...).

        `first_run` leaves out the prior-state marker and asks for the welcome dialog an isolated launch suppresses.
        The dialog is modal and holds the socket unanswered, so such a launch returns without waiting for it: the
        caller dismisses the dialog, then calls `serving`.
        """
        self.executable()  # refuses before anything exists
        headless = self.headless
        root = make_root()
        compositor: subprocess.Popen[bytes] | None = None
        session_bus: subprocess.Popen[bytes] | None = None
        try:
            env = self.build_env(root, app_id)
            if first_run:
                self.first_run_env(env)
            check_launch_env(env, root, self.needs_app_id, self.needs_runtime_dir, headless, self.env_prefix)
            app_id = env.get(self.variable("APP_ID"))
            state_dir = env[self.variable("STATE_DIR")]
            _prepare_tree(root, Path(env["HOME"]), Path(state_dir), self.ghostty_config(), settings, first_run)
            command = self.instance_executable(root)
            if self.needs_runtime_dir:
                os.mkdir(env["XDG_RUNTIME_DIR"], 0o700)  # the XDG spec's mode
                write_noop_launchers(Path(env["XDG_CONFIG_HOME"]), Path(env["XDG_DATA_HOME"]))
            if headless:
                session_bus = start_session_bus(root, env, timeout)
                compositor = start_compositor(root, env, timeout, self.weston_args())
            if app_id is not None:
                _claim_app_id(app_id)
        except BaseException as error:
            _note_headless_stop(error, compositor, session_bus, root)
            shutil.rmtree(root, ignore_errors=True)
            raise
        log = root / "app.log"
        try:
            process = _spawn(command, env, log, "wb")
        except BaseException as error:
            _release_app_id(app_id)
            _note_headless_stop(error, compositor, session_bus, root)
            shutil.rmtree(root, ignore_errors=True)
            raise
        instance = Instance(
            process=process,
            root=root,
            state_dir=Path(state_dir),
            home=Path(env["HOME"]),
            socket=Path(env[self.variable("CONTROL_SOCKET")]),
            env=env,
            log=log,
            app_id=app_id,
            live_dir=self.vocabulary.socket_directory(state_dir, self.app_dir_name),
            vocabulary=self.vocabulary,
            env_prefix=self.env_prefix,
            compositor=compositor,
            session_bus=session_bus,
        )
        return instance if first_run else self._serving(instance, timeout)

    def serving(self, instance: Instance, timeout: float = LAUNCH_TIMEOUT) -> Instance:
        """Wait until a `first_run` launch answers on its socket; a failure stops it and keeps its root."""
        return self._serving(instance, timeout)

    def relaunch(self, instance: Instance, timeout: float = LAUNCH_TIMEOUT) -> Instance:
        """Quit `instance` with SIGTERM, then start the binary again on the same root, env and state directory.

        Nothing is reaped and no leftover is checked in between: a Live daemon outliving the quit is what the next
        launch adopts. A relaunch failing before the new process exists reports what the old one left instead.
        The returned instance replaces `instance`; stop that one, never the old.
        """
        command = self.instance_executable(instance.root)
        problem = _terminate_app(instance)
        _release_app_id(instance.app_id)
        try:
            if problem is not None:
                raise StopError(problem)
            check_launch_env(
                instance.env,
                instance.root,
                self.needs_app_id,
                self.needs_runtime_dir,
                instance.compositor is not None,
                instance.env_prefix,
            )
            if instance.app_id is not None:
                _claim_app_id(instance.app_id)
            try:
                process = _spawn(command, instance.env, instance.log, "ab")
            except BaseException:
                _release_app_id(instance.app_id)
                raise
        except BaseException as error:
            _note_headless_stop(error, instance.compositor, instance.session_bus, instance.root)
            if (leftovers := _leftovers_error(instance.live_dir, vocabulary=instance.vocabulary)) is not None:
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
            live_dir=instance.live_dir,
            vocabulary=instance.vocabulary,
            env_prefix=instance.env_prefix,
            compositor=instance.compositor,
            session_bus=instance.session_bus,
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
        """Reap daemons, SIGTERM the launched pid, end what `stop_helpers` owns, then require no live-session process
        left (`find_leftovers`).

        The compositor and session bus go last, once the app is gone, and must leave no process of their own.
        """
        headless_problem: str | None = None
        try:
            steps: list[str] = []
            try:
                if (code := instance.process.poll()) is not None:
                    steps = [f"the app had already exited with {code}"]
                elif find_leftovers(instance.live_dir, instance.vocabulary) or _lists_daemons(instance):
                    # Listed too: a daemon still starting has no process yet and would outlive the quit.
                    steps = reap_live_sessions(instance)
                else:
                    steps = ["nothing listed or running to reap before the quit"]
            finally:
                problem = _terminate_app(instance)
            helper_problem = self.stop_helpers(instance)
            leftovers = _leftovers_error(instance.live_dir, steps, instance.vocabulary)
        finally:
            _release_app_id(instance.app_id)
            headless_problem = stop_headless(instance.compositor, instance.session_bus, instance.root)
        if leftovers is not None:
            raise leftovers
        if problem is not None:
            raise StopError(problem)
        if helper_problem is not None:
            raise StopError(helper_problem)
        if headless_problem is not None:
            raise StopError(headless_problem)
        if cleanup:
            shutil.rmtree(instance.root, ignore_errors=True)
            remove_live_dir(instance.live_dir)

    def stop_helpers(self, instance: Instance) -> str | None:
        """Hook: end a helper the implementation detaches on purpose once the app is gone; returns what went wrong."""
        return None


def compositor_program() -> str:
    if program := shutil.which("weston"):
        return program
    raise LaunchError(f"no weston on PATH: install it, or set {DISPLAY_MODE_VARIABLE}=session to draw on this desktop")


@functools.cache
def weston_takes_fake_seat(program: str) -> bool:
    """Whether `program --help` lists `--fake-seat` (weston 15.0 and later); asked once per program."""
    env = {name: value for name, value in os.environ.items() if name not in ("WAYLAND_DISPLAY", "DISPLAY")}
    try:
        shown = subprocess.run(
            [program, "--help"], env=env, stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=10
        )
    except (OSError, subprocess.TimeoutExpired):
        return False
    return FAKE_SEAT_ARG in shown.stdout + shown.stderr


def require_fake_seat(program: str) -> None:
    """Refuse a weston without `--fake-seat`: older ones exit on the unknown option, and GPUI panics without a seat."""
    if not weston_takes_fake_seat(program):
        raise LaunchError(
            f"{program} does not take {FAKE_SEAT_ARG}, which the GPUI build needs (weston 15.0 or later; Ubuntu 24.04 "
            "ships 13): build one with scripts/build-weston.sh and put its bin/ first on PATH"
        )


def weston_argv(program: str, root: Path, extra: Sequence[str] = ()) -> list[str]:
    """The headless weston's command line; `extra` comes last, so an option there overrides the one before it."""
    width, height = WESTON_SIZE
    return [
        program,
        "--backend=headless",
        f"--renderer={WESTON_RENDERER}",
        f"--socket={WESTON_SOCKET}",
        f"--config={root / 'weston.ini'}",
        f"--log={root / 'weston.log'}",
        f"--width={width}",
        f"--height={height}",
        *extra,
    ]


def start_compositor(
    root: Path, env: Mapping[str, str], timeout: float = LAUNCH_TIMEOUT, extra: Sequence[str] = ()
) -> subprocess.Popen[bytes]:
    """Start the instance's weston in its runtime dir and return once its socket accepts a connection."""
    (root / "weston.ini").write_text(WESTON_CONFIG)
    socket_path = os.path.join(env["XDG_RUNTIME_DIR"], WESTON_SOCKET)
    check_under_root(socket_path, root)
    # Its own env without a display: given a WAYLAND_DISPLAY or DISPLAY, weston would nest into that server.
    weston_env = {name: value for name, value in env.items() if name not in ("WAYLAND_DISPLAY", "DISPLAY")}
    if vendor := mesa_egl_vendor():
        weston_env[EGL_VENDOR_VARIABLE] = vendor
    process = _spawn(weston_argv(compositor_program(), root, extra), weston_env, root / "weston.out", "wb")
    _wait_serving(socket_path, process, timeout, "weston", root / "weston.log")
    return process


def session_bus_program() -> str:
    if program := shutil.which("dbus-daemon"):
        return program
    raise LaunchError(
        f"no dbus-daemon on PATH: install it, or set {DISPLAY_MODE_VARIABLE}=session to use this desktop's bus"
    )


def session_bus_argv(program: str, root: Path) -> list[str]:
    return [program, "--nofork", "--nopidfile", "--nosyslog", f"--config-file={root / 'dbus.conf'}"]


def start_session_bus(root: Path, env: Mapping[str, str], timeout: float = LAUNCH_TIMEOUT) -> subprocess.Popen[bytes]:
    """Start the instance's private session bus in its runtime dir and return once it accepts a connection."""
    socket_path = os.path.join(env["XDG_RUNTIME_DIR"], DBUS_SOCKET)
    check_under_root(socket_path, root)
    (root / "dbus.conf").write_text(DBUS_CONFIG.format(path=socket_path))
    bus_env = {name: value for name, value in env.items() if name not in ("WAYLAND_DISPLAY", "DISPLAY")}
    process = _spawn(session_bus_argv(session_bus_program(), root), bus_env, root / "dbus.log", "wb")
    _wait_serving(socket_path, process, timeout, "dbus-daemon", root / "dbus.log")
    return process


def _wait_serving(path: str, process: subprocess.Popen[bytes], timeout: float, label: str, log: Path) -> None:
    try:
        wait_for_unix_socket(path, process, timeout, log, label)
    except BaseException as error:
        if problem := _stop_helper(process, label, log):
            error.add_note(problem)
        raise


def mesa_egl_vendor() -> str | None:
    for directory in GLVND_VENDOR_DIRS:
        if found := sorted(Path(directory).glob("*mesa*.json")):
            return os.fspath(found[0])
    return None


def lavapipe_icd(environ: Mapping[str, str] | None = None) -> str:
    """The lavapipe ICD manifest `CONFORMANCE_LAVAPIPE_ICD` names, checked to load lavapipe and nothing else."""
    path = (os.environ if environ is None else environ).get(LAVAPIPE_ICD_VARIABLE)
    hint = (
        f"set {LAVAPIPE_ICD_VARIABLE} to lavapipe's ICD manifest (`scripts/fetch-lavapipe.sh` prints it; "
        "run-rust-linux.sh sets it): the GPUI build renders on lavapipe only, "
        "never on a hardware driver"
    )
    if not path:
        raise LaunchError(f"no lavapipe: {hint}")
    try:
        manifest = json.loads(Path(path).read_text())
        library = manifest["ICD"]["library_path"]
    except (OSError, ValueError, KeyError, TypeError) as error:
        raise LaunchError(f"{LAVAPIPE_ICD_VARIABLE}={path} is no readable ICD manifest ({error}): {hint}") from None
    if not isinstance(library, str) or not os.path.basename(library).startswith(LAVAPIPE_LIBRARY_PREFIX):
        raise LaunchError(f"{LAVAPIPE_ICD_VARIABLE}={path} loads {library!r}, not lavapipe: {hint}")
    # A missing driver would leave wgpu no Vulkan device, and it would quietly render through GL instead.
    if icd_library(path, library) is None:
        raise LaunchError(f"{LAVAPIPE_ICD_VARIABLE}={path} names {library!r}, which is not there: {hint}")
    return os.path.abspath(path)


# Where the loader's dlopen finds a driver named without a slash, beyond LD_LIBRARY_PATH and the ldconfig cache.
LIBRARY_DIRS = ("/usr/local/lib", "/usr/lib64", "/usr/lib", "/lib64", "/lib")


def icd_library(manifest: str | os.PathLike[str], library: str) -> str | None:
    """The file an ICD manifest's `library_path` loads, by the Vulkan loader's rules, or None when there is none:
    absolute as is, with a slash relative to the manifest's directory, a bare name through the library search path."""
    if "/" in library:
        found = os.path.join(os.path.dirname(os.path.abspath(manifest)), library)
        return found if os.path.isfile(found) else None
    dirs = [d for d in os.environ.get("LD_LIBRARY_PATH", "").split(":") if d]
    dirs += sorted(str(d) for d in Path("/usr/lib").glob("*-linux-gnu*")) + list(LIBRARY_DIRS)
    for directory in dirs:
        if os.path.isfile(candidate := os.path.join(directory, library)):
            return candidate
    for program in ("ldconfig", "/sbin/ldconfig", "/usr/sbin/ldconfig"):
        try:
            cache = subprocess.run([program, "-p"], capture_output=True, text=True, timeout=10, check=False).stdout
        except (OSError, subprocess.TimeoutExpired):
            continue
        for line in cache.splitlines():
            name, _, target = line.strip().partition(" => ")
            if name.split(" ", 1)[0] == library and os.path.isfile(target):
                return target
        break
    return None


def software_rendering_env(env: dict[str, str], icd: str) -> None:
    """Make lavapipe the process's only Vulkan driver and llvmpipe its GL, dropping any inherited device selection."""
    for name in [name for name in env if name.startswith(GPU_SELECTION_PREFIXES)]:
        del env[name]
    env.pop(LAVAPIPE_ICD_VARIABLE, None)  # the harness's, not the instance's
    env["VK_DRIVER_FILES"] = icd
    env["VK_ICD_FILENAMES"] = icd  # the loader's name for it before 1.3.234
    env["LIBGL_ALWAYS_SOFTWARE"] = "1"
    if vendor := mesa_egl_vendor():
        env[EGL_VENDOR_VARIABLE] = vendor


def wait_for_unix_socket(
    path: str, process: subprocess.Popen[bytes], timeout: float, log: Path, label: str = "weston"
) -> None:
    deadline = time.monotonic() + timeout
    while True:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as probe:
            try:
                probe.connect(path)
                return
            except OSError as error:
                last = error
        if time.monotonic() >= deadline:
            raise LaunchError(f"{label} socket {path} not accepting after {timeout}s ({last}):\n{_tail(log)}")
        try:
            code = process.wait(timeout=POLL_INTERVAL)
        except subprocess.TimeoutExpired:
            continue
        raise LaunchError(f"{label} exited with {code} before serving {path}:\n{_tail(log)}")


def stop_headless(
    compositor: subprocess.Popen[bytes] | None, session_bus: subprocess.Popen[bytes] | None, root: Path
) -> str | None:
    """SIGTERM the compositor's and the bus's own pids, then wait for their clients to follow; returns what went wrong."""
    if compositor is None and session_bus is None:
        return None
    problems = [
        problem
        for process, label, log in (
            (compositor, "weston", root / "weston.log"),
            (session_bus, "dbus-daemon", root / "dbus.log"),
        )
        if process is not None and (problem := _stop_helper(process, label, log))
    ]
    # Weston's desktop-shell helper exits on its own once the socket closes.
    runtime_dir = os.fspath(root / RUNTIME_DIR_NAME)
    deadline = time.monotonic() + STOP_TIMEOUT
    while (left := find_headless_leftovers(runtime_dir)) and time.monotonic() < deadline:
        threading.Event().wait(POLL_INTERVAL)
    if left:
        described = ", ".join(f"{pid} {command}" for pid, _, command in left)
        problems.append(f"compositor or bus processes left under XDG_RUNTIME_DIR {runtime_dir}: {described}")
    return "; ".join(problems) or None


def _stop_helper(process: subprocess.Popen[bytes], label: str, log: Path) -> str | None:
    if (code := process.poll()) is not None:
        return f"{label} pid {process.pid} had already exited with {code}:\n{_tail(log)}"
    if _terminate(process):
        return f"{label} pid {process.pid} ignored SIGTERM for {STOP_TIMEOUT}s and was killed"
    return None


def _note_headless_stop(
    error: BaseException,
    compositor: subprocess.Popen[bytes] | None,
    session_bus: subprocess.Popen[bytes] | None,
    root: Path,
) -> None:
    if (problem := stop_headless(compositor, session_bus, root)) is not None:
        error.add_note(problem)


def find_headless_leftovers(runtime_dir: str) -> list[tuple[int, str, str]]:
    """(pid, name, command) of weston, its clients and dbus-daemon carrying this instance's runtime dir."""
    marker = f"XDG_RUNTIME_DIR={runtime_dir}"
    return [
        (pid, name, command)
        for pid, name, command, words in _processes()
        if name.startswith(HEADLESS_HELPERS) and marker in words
    ]


def _tail(log: Path, lines: int = 20) -> str:
    try:
        return "\n".join(log.read_text(errors="replace").splitlines()[-lines:])
    except OSError:
        return ""


def _leftovers_error(
    live_dir: str, steps: Iterable[str] = (), vocabulary: Vocabulary = UPSTREAM
) -> LeftoverProcesses | None:
    # A killed zmx daemon is confirmed gone by its socket and still exits for a moment after; a session server stops
    # by itself once its last session and client are gone, after its idle grace.
    deadline = time.monotonic() + LEFTOVER_GRACE + vocabulary.server_idle
    while (leftovers := find_leftovers(live_dir, vocabulary)) and time.monotonic() < deadline:
        threading.Event().wait(POLL_INTERVAL * 4)
    if not leftovers:
        return None
    # Cut: on macOS the command carries the whole `ps -E` environment.
    described = ", ".join(f"{pid} {command[:300]}" for pid, _, command in leftovers)
    reaping = f"; reaping: {'; '.join(steps)}" if steps else ""
    return LeftoverProcesses(f"processes left under {vocabulary.directory_variable} {live_dir}: {described}{reaping}")


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
    root: Path,
    home: Path,
    state: Path,
    ghostty_config: str,
    settings: Mapping[str, Any] | None = None,
    first_run: bool = False,
) -> None:
    for directory in (home / ".claude", home / ".codex", state / "config"):
        directory.mkdir(parents=True)
    # `FirstRunWelcome.hasPriorState` reads this directory: its presence skips the welcome dialog.
    if not first_run:
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
            raise LaunchRefused(f"the app id {app_id} is already used by a running instance")
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
    """Best effort over the protocol; returns what it did, for the leftover check to report with what survived.

    Re-lists after every kill: killing a left pane promotes its split survivor into the left slot, so a
    selector read before the kill no longer names that survivor. A claimed daemon zmx does not list yet
    (`observation: absent`, as right after it starts) refuses a kill as not running, and a `pendingClose` one
    refuses it outright, so both are waited for until `settle` runs out.
    """
    steps: list[str] = []
    deadline = time.monotonic() + settle
    try:
        steps.append(f"listed: {_listed(instance)}")
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
            kill = instance.vocabulary.command("zmx.kill")
            response = instance.request(kill, entry["sessionID"], **args)
            steps.append(f"{kill} {_daemon_key(entry)}: {'ok' if response.ok else response.error}")
        prune_command = instance.vocabulary.command("zmx.prune")
        prune = instance.request(prune_command)
        if not prune.ok:
            steps.append(f"{prune_command}: {prune.error}")
        steps.append(f"left listed: {_listed(instance)}")
    except ClientError as error:
        steps.append(f"reaping: {error}")
    return steps


def _listed(instance: Instance) -> str:
    listing = instance.request(instance.vocabulary.command("zmx.list"))
    inventory = _inventory(listing, instance.vocabulary)
    entries = inventory.get("entries") if isinstance(inventory, dict) else None
    if not isinstance(entries, list):
        return f"no inventory ({listing.error})"
    rows = [f"{e.get('daemon')} {e.get('state')}/{e.get('observation')}" for e in entries if isinstance(e, dict)]
    return ", ".join(rows) or "nothing"


def _inventory(listing: Response, vocabulary: Vocabulary) -> Any:
    """The `zmx.list` inventory a reply carries under the vocabulary's result key; None for a refusal."""
    return (listing.result or {}).get(vocabulary.result_key) if listing.ok else None


def _lists_daemons(instance: Instance) -> bool:
    try:
        listing = instance.request(instance.vocabulary.command("zmx.list"))
    except ClientError:
        return False
    inventory = _inventory(listing, instance.vocabulary)
    return bool(isinstance(inventory, dict) and inventory.get("entries"))


def _next_claimed(instance: Instance, tried: set[str], wait_absent: bool) -> tuple[dict[str, Any] | None, bool]:
    """The next claimed daemon to kill, and whether one not yet killable is still worth waiting for."""
    inventory = _inventory(instance.request(instance.vocabulary.command("zmx.list")), instance.vocabulary)
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


def _terminate_app(instance: Instance) -> str | None:
    """SIGTERM the app; the StopError message when it outlives STOP_TIMEOUT, else None.

    An app that stalls gets its threads snapshotted into its log and SIGABRT, so the system writes a core naming the
    stalled step, then SIGKILL once ABORT_GRACE runs out. Weston and the session bus keep the plain `_terminate`.
    """
    process = instance.process
    if process.poll() is not None:
        return None
    process.send_signal(signal.SIGTERM)
    try:
        process.wait(timeout=STOP_TIMEOUT)
        return None
    except subprocess.TimeoutExpired:
        pass
    snapshot = thread_snapshot(process.pid)
    process.send_signal(signal.SIGABRT)
    outlived_kill = ""
    try:
        process.wait(timeout=ABORT_GRACE)
    except subprocess.TimeoutExpired:
        process.kill()
        try:
            process.wait(timeout=STOP_TIMEOUT)
        except subprocess.TimeoutExpired:
            # Uninterruptible sleep, a driver call say: the snapshot taken before SIGABRT names where.
            outlived_kill = f" nor within {STOP_TIMEOUT}s of SIGKILL"
    # Appended once the app is gone (or stuck in the kernel): a first launch's stdout is not opened for appending, so a
    # later write of the app would land on top of the snapshot.
    try:
        with open(instance.log, "a", errors="replace") as sink:
            sink.write(snapshot)
        written, unwritten = f"thread snapshot in {instance.log}", ""
    except OSError as error:
        # The snapshot is what names the stalled step: carried in the message when the log takes no write.
        written, unwritten = f"thread snapshot not written to {instance.log} ({error}), below", f"\n{snapshot}"
    return (
        f"pid {process.pid} did not exit within {STOP_TIMEOUT}s of SIGTERM{outlived_kill}; {written}; "
        f"core requested ({_core_hint(process.pid)}){unwritten}"
    )


def thread_snapshot(pid: int) -> str:
    """Best effort: each thread's id, name, state and the kernel function it sleeps in, read from /proc."""
    header = f"--- conformance: pid {pid} outlived SIGTERM by {STOP_TIMEOUT}s; its threads:\n"
    try:
        tasks = sorted((PROC_ROOT / str(pid) / "task").iterdir(), key=lambda task: (len(task.name), task.name))
    except OSError as error:
        return f"{header}  thread snapshot unavailable: {error}\n"
    lines = [
        f"  tid={task.name} comm={_proc_line(task / 'comm')} state={_proc_state(task)} "
        f"wchan={_proc_line(task / 'wchan')}\n"
        for task in tasks
    ]
    return header + "".join(lines)


def _proc_line(path: Path) -> str:
    try:
        return path.read_text(errors="replace").strip() or "-"
    except OSError:
        return "?"


def _proc_state(task: Path) -> str:
    try:
        status = (task / "status").read_text(errors="replace")
    except OSError:
        return "?"
    return next((line.split(":", 1)[1].strip() for line in status.splitlines() if line.startswith("State:")), "?")


def _core_hint(pid: int) -> str:
    if sys.platform == "darwin":
        return f"a crash report for pid {pid} in ~/Library/Logs/DiagnosticReports"
    return f"coredumpctl info {pid}"


def find_leftovers(live_dir: str, vocabulary: Vocabulary = UPSTREAM) -> list[tuple[int, str, str]]:
    """(pid, name, command) of this instance's live-session processes: under a vocabulary with a session server, that
    server (`<server> server <live_dir>`) and every process carrying the socket directory in the directory variable
    (`NGTERM_LIVE_DIR`: its sessions' programs and their attach clients); else zmx / agterm-session-host processes
    carrying it (`ZMX_DIR`)."""
    marker = f"{vocabulary.directory_variable}={live_dir}"
    if vocabulary.server is not None:
        return [
            (pid, name, command)
            for pid, name, command, words in _processes()
            if marker in words or serves_namespace(words, live_dir, vocabulary.server)
        ]
    return [
        (pid, name, command)
        for pid, name, command, words in _processes()
        if name in LEFTOVER_NAMES and (marker in words or hosts_zmx_dir(words, live_dir))
    ]


def serves_namespace(words: Iterable[str], live_dir: str, server: str) -> bool:
    """`<server> server <live_dir>`, the argv a session server of that namespace runs with (its environment carries
    no `NGTERM_*`: the spawn drops them)."""
    words = list(words)
    return len(words) >= 3 and os.path.basename(words[0]) == server and words[1:3] == ["server", live_dir]


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


def remove_live_dir(path: str) -> None:
    """The instance's live-session socket directory, outside its root, once nothing runs there: zmx's own
    `logs/*.log` and a stopped session server's lock file go with it; anything else keeps the directory for
    diagnosis."""
    logs = os.path.join(path, "logs")
    try:
        names = set(os.listdir(path))
        if names - {"logs"} - SERVER_NAMESPACE_FILES or os.path.islink(logs):
            return
        for name in names & SERVER_NAMESPACE_FILES:
            entry = os.path.join(path, name)
            if os.path.islink(entry) or os.path.isdir(entry):
                return
        for name in names & SERVER_NAMESPACE_FILES:
            os.unlink(os.path.join(path, name))
        if os.path.isdir(logs):
            entries = list(os.scandir(logs))
            if not all(entry.name.endswith(".log") and entry.is_file(follow_symlinks=False) for entry in entries):
                return
            for entry in entries:
                os.unlink(entry.path)
            os.rmdir(logs)
        os.rmdir(path)
    except OSError:
        pass
