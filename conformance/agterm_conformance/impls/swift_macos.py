"""The released upstream macOS app, exec'd directly from its bundle (never through `open`)."""

from __future__ import annotations

import os
import shutil
import signal
import subprocess
import sys
from pathlib import Path

from agterm_conformance.client import ClientError, check_under_root
from agterm_conformance.impls.base import (
    SESSION_HOST,
    STOP_TIMEOUT,
    Adapter,
    Instance,
    LaunchError,
    Profile,
    find_leftovers,
    hosts_zmx_dir,
    login_shell_blocker,
    process_identity,
)
from agterm_conformance.wait import WaitTimeout, wait_until

PARITY_LEVEL = "0.36.0"
BASE_CAPABILITIES = frozenset({"posix-shell", "window-geometry", "window-minimize", "sidebar-min-width"})
LAUNCH_MODES = ("uitest", "plain")
BUNDLE_DIR = "app"


class SwiftMacosAdapter(Adapter):
    """`uitest` with zmx by default (conformance/docs/spike-macos.md).

    `uitest` sets the XCUITest sentinel: window present (FB11763863), no welcome, no quit prompt, and zmx
    off unless `enable_zmx`. `plain` is a bare exec.
    """

    name = "swift-macos"
    session_host_timeout = STOP_TIMEOUT

    def __init__(
        self, bin: Path | None, ctl: Path | None = None, launch_mode: str = "uitest", enable_zmx: bool = True
    ) -> None:
        if launch_mode not in LAUNCH_MODES:
            raise ValueError(f"launch_mode must be one of {LAUNCH_MODES}, got {launch_mode!r}")
        super().__init__(bin, ctl)
        self.launch_mode = launch_mode
        self.enable_zmx = enable_zmx

    @property
    def profile(self) -> Profile:
        blocker = self.live_sessions_blocker()
        live = frozenset() if blocker else frozenset({"live-sessions"})
        return Profile(PARITY_LEVEL, BASE_CAPABILITIES | live, {"live-sessions": blocker} if blocker else {})

    def live_sessions_blocker(self) -> str | None:
        if self.launch_mode == "uitest" and not self.enable_zmx:
            return f"the XCUITest sentinel turns zmx off without {self.vocabulary.ui_test_opt_in}"
        return login_shell_blocker()

    def _macos_dir(self) -> Path | None:
        if self.bin is None:
            return None
        path = Path(self.bin)
        return path / "Contents" / "MacOS" if path.suffix == ".app" else path.parent

    def executable(self) -> list[str]:
        if self.bin is None:
            raise LaunchError(f"{self.name}: no binary to launch")
        path = Path(self.bin)
        return [os.fspath(path / "Contents" / "MacOS" / "agterm" if path.suffix == ".app" else path)]

    def default_ctl(self) -> Path | None:
        directory = self._macos_dir()
        return directory / "agtermctl" if directory is not None else None

    def platform_env(self, env: dict[str, str]) -> None:
        # Points NSHomeDirectory, and with it the defaults lookup, at the isolated home.
        env["CFFIXED_USER_HOME"] = env["HOME"]
        if self.launch_mode == "uitest":
            env["AGTERM_UITEST_FORCE_SIDEBAR_VISIBLE"] = "1"
            if self.enable_zmx:
                env[self.vocabulary.ui_test_opt_in] = "1"

    def first_run_env(self, env: dict[str, str]) -> None:
        # The sentinel suppresses the welcome; this opts back in (upstream WelcomeUITests).
        if self.launch_mode == "uitest":
            env["AGTERM_UITEST_SHOW_WELCOME"] = "1"

    def bundle(self) -> Path | None:
        if self.bin is None:
            return None
        path = Path(self.bin)
        return next((candidate for candidate in (path, *path.parents) if candidate.suffix == ".app"), None)

    def instance_bundle(self, root: Path) -> Path | None:
        bundle = self.bundle()
        return root / BUNDLE_DIR / bundle.name if bundle is not None else None

    def instance_executable(self, root: Path) -> list[str]:
        """The instance's own copy of the bundle: next to a running instance of the same bundle path, a launch keeps
        no window and binds no socket (conformance/docs/spike-macos.md)."""
        clone = self.instance_bundle(root)
        if clone is None:
            return self.executable()
        if not clone.exists():
            clone.parent.mkdir(parents=True, exist_ok=True)
            source = self.bundle()
            if sys.platform == "darwin":
                # An APFS clone: no data is copied.
                subprocess.run(["cp", "-c", "-R", os.fspath(source), os.fspath(clone)], check=True)
            else:
                shutil.copytree(source, clone, symlinks=True)
        return [os.fspath(clone / "Contents" / "MacOS" / "agterm")]

    def owns_session_host(self, instance: Instance, pid: int) -> bool:
        """`pid` runs the session host of this instance's own bundle copy for its ZMX_DIR, derived from a state dir
        in its root."""
        bundle = self.instance_bundle(instance.root)
        if bundle is None or instance.live_dir != instance.vocabulary.socket_directory(
            instance.state_dir, self.app_dir_name
        ):
            return False
        try:
            check_under_root(instance.state_dir, instance.root)
        except ClientError:
            return False
        identity = process_identity(pid)
        expected = os.path.realpath(bundle / "Contents" / "MacOS" / SESSION_HOST)
        return (
            identity is not None and identity.executable == expected and hosts_zmx_dir(identity.words, instance.live_dir)
        )

    def stop_helpers(self, instance: Instance) -> str | None:
        """The session host outlives the app by design and takes no stop request from outside its bundle, so the
        harness ends the one its instance started: by exact pid, ownership re-verified before every signal."""
        hosts = [
            pid
            for pid, name, _ in find_leftovers(instance.live_dir, instance.vocabulary)
            if name == SESSION_HOST and self.owns_session_host(instance, pid)
        ]
        killed = [pid for pid in hosts if self._end_session_host(instance, pid)]
        if killed:
            return f"{SESSION_HOST} {', '.join(map(str, killed))} ignored SIGTERM for {self.session_host_timeout}s"
        return None

    def _end_session_host(self, instance: Instance, pid: int) -> bool:
        """True when SIGTERM was not enough; the leftover check reports a host that outlives SIGKILL too."""
        if not self._signal_owned(instance, pid, signal.SIGTERM):
            return False
        try:
            wait_until(lambda: not self.owns_session_host(instance, pid), self.session_host_timeout)
            return False
        except WaitTimeout:
            if self._signal_owned(instance, pid, signal.SIGKILL):
                try:
                    wait_until(lambda: not self.owns_session_host(instance, pid), self.session_host_timeout)
                except WaitTimeout:
                    pass
            return True

    def _signal_owned(self, instance: Instance, pid: int, signum: int) -> bool:
        if not self.owns_session_host(instance, pid):
            return False  # gone, or the pid now names another process
        _send_signal(pid, signum)
        return True


def _send_signal(pid: int, signum: int) -> None:
    try:
        os.kill(pid, signum)
    except ProcessLookupError:
        pass
