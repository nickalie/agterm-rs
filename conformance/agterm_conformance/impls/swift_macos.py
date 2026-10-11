"""The released upstream macOS app, exec'd directly from its bundle (never through `open`)."""

from __future__ import annotations

import os
from pathlib import Path

from agterm_conformance.impls.base import Adapter, LaunchError, Profile

PARITY_LEVEL = "0.36.0"
BASE_CAPABILITIES = frozenset({"posix-shell", "window-geometry"})
LAUNCH_MODES = ("uitest", "plain")


class SwiftMacosAdapter(Adapter):
    """Interim choice until Task 19 confirms it on a Mac (docs/spike-macos.md): `uitest` with zmx.

    `uitest` sets the XCUITest sentinel: window present (FB11763863), no welcome, no quit prompt, and zmx
    off unless `enable_zmx`. `plain` is a bare exec.
    """

    name = "swift-macos"

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
        live = self.launch_mode == "plain" or self.enable_zmx
        return Profile(PARITY_LEVEL, BASE_CAPABILITIES | ({"live-sessions"} if live else set()))

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
        # Points NSHomeDirectory (and with it the defaults lookup) at the isolated home; confirmed in Task 19.
        env["CFFIXED_USER_HOME"] = env["HOME"]
        if self.launch_mode == "uitest":
            env["AGTERM_UITEST_FORCE_SIDEBAR_VISIBLE"] = "1"
            if self.enable_zmx:
                env["AGTERM_UITEST_ENABLE_ZMX"] = "1"
