"""The installed Swift/GTK fork, launched the way `agterm-linux/tests/atspi_smoke.py` does."""

from __future__ import annotations

import os
from pathlib import Path

from agterm_conformance.impls.base import Adapter, Profile

# GTK4 cannot place a window at x/y, so `window-geometry` is absent. The fork's own dispatcher and adapters
# also lack upstream's `status-shape`, `type-rejects-nul`, `resize-echoes-ratio` and `drop-placement`.
PROFILE = Profile(parity_level="0.36.0", capabilities=frozenset({"posix-shell", "live-sessions"}))


class SwiftLinuxAdapter(Adapter):
    name = "swift-linux"
    # Without a unique id GApplication forwards the launch to the user's running instance.
    needs_app_id = True
    needs_runtime_dir = True

    def __init__(self, bin: Path | None, ctl: Path | None = None, resource_root: Path | None = None) -> None:
        super().__init__(bin, ctl)
        self._resource_root = resource_root

    @property
    def profile(self) -> Profile:
        return PROFILE

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
