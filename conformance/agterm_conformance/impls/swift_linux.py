"""The installed Swift/GTK fork, launched the way `agterm-linux/tests/atspi_smoke.py` does."""

from __future__ import annotations

import os
from pathlib import Path

from agterm_conformance.impls.base import Adapter, Profile, login_shell_blocker

PARITY_LEVEL = "0.36.0"
# GTK4 cannot place a window at x/y, so `window-geometry` is absent, and Wayland's xdg-shell reports no
# minimized state (niri ignores the request too), so `window-minimize` is. The fork's own dispatcher and
# adapters lack the rest of upstream's set in swift_macos.py.
CAPABILITIES = frozenset({"posix-shell"})


class SwiftLinuxAdapter(Adapter):
    name = "swift-linux"
    # Without a unique id GApplication forwards the launch to the user's running instance.
    needs_app_id = True
    needs_runtime_dir = True

    def __init__(self, bin: Path | None, ctl: Path | None = None, resource_root: Path | None = None) -> None:
        super().__init__(bin, ctl)
        self._resource_root = resource_root
        self._profile: Profile | None = None

    @property
    def profile(self) -> Profile:
        if self._profile is None:
            blocker = self.live_sessions_blocker()
            live = frozenset() if blocker else frozenset({"live-sessions"})
            absent = {"live-sessions": blocker} if blocker else {}
            self._profile = Profile(PARITY_LEVEL, CAPABILITIES | live, absent)
        return self._profile

    def live_sessions_blocker(self) -> str | None:
        """The host prerequisites `LinuxZmxLaunch` checks: a zsh login shell and the staged zmx."""
        if blocker := login_shell_blocker():
            return blocker
        install = self._install_root()
        zmx = install / "libexec" / "zmx" if install is not None else None
        if zmx is None or not os.access(zmx, os.X_OK):
            return f"no executable zmx at {zmx}"
        return None

    def _install_root(self) -> Path | None:
        # The installed wrapper lives in `<install>/bin/`; resolving links keeps a PATH symlink working.
        return Path(os.path.realpath(self.bin)).parent.parent if self.bin is not None else None

    def default_ctl(self) -> Path | None:
        if self.bin is None:
            return None
        sibling = Path(self.bin).parent / "agtermctl"
        return sibling if sibling.exists() else None

    @property
    def resource_root(self) -> Path | None:
        if self._resource_root is not None:
            return self._resource_root
        install = self._install_root()
        candidate = install / "share" / "agterm" if install is not None else None
        return candidate if candidate is not None and candidate.is_dir() else None

    def platform_env(self, env: dict[str, str]) -> None:
        if self.resource_root is not None:
            env["AGTERM_RESOURCE_ROOT"] = os.fspath(self.resource_root)
