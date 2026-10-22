"""The Rust build: it reports its own parity level and capabilities on `version`; its GPUI frontend renders through
Vulkan, on lavapipe only, and needs a seat on its compositor."""

from __future__ import annotations

import sys
from collections.abc import Mapping
from pathlib import Path
from typing import Any

from agterm_conformance.impls import base
from agterm_conformance.impls.base import (
    RUST_PREFIX,
    FAKE_SEAT_ARG,
    LAUNCH_TIMEOUT,
    NO_PROFILE,
    Adapter,
    Instance,
    LaunchError,
    Profile,
    lavapipe_icd,
    software_rendering_env,
    version_tuple,
)
from agterm_conformance.vocabulary import LIVE

# `version` -> result.conformance = {"parityLevel": "0.36.0", "capabilities": ["posix-shell", ...]}.
# Optional: a build without it, like a run without a binary, is below every level.
CONFORMANCE_FIELD = "conformance"

class ProfileError(ValueError):
    pass


def profile_from_version(result: Mapping[str, Any] | None) -> Profile:
    block = (result or {}).get(CONFORMANCE_FIELD)
    if block is None:
        return NO_PROFILE
    if not isinstance(block, Mapping):
        raise ProfileError(f"version.{CONFORMANCE_FIELD} is not an object: {block!r}")
    level = block.get("parityLevel")
    capabilities = block.get("capabilities", [])
    if level is not None:
        if not isinstance(level, str):
            raise ProfileError(f"parityLevel is not a string: {level!r}")
        try:
            version_tuple(level)
        except ValueError as error:
            raise ProfileError(str(error)) from None
    if not isinstance(capabilities, list) or not all(isinstance(item, str) for item in capabilities):
        raise ProfileError(f"capabilities is not a list of strings: {capabilities!r}")
    return Profile(parity_level=level, capabilities=frozenset(capabilities))


class RustAdapter(Adapter):
    name = "rust"
    app_dir_name = "ngterm"
    ctl_name = "ngtermctl"
    env_prefix = RUST_PREFIX
    # `live.*`, `ngtermctl live`, `result.live`, `backedByLive`: upstream's `zmx` group renamed
    # (conformance/docs/rust-build.md > The `live` group).
    vocabulary = LIVE
    needs_app_id = sys.platform.startswith("linux")
    needs_runtime_dir = sys.platform.startswith("linux")

    def __init__(self, bin: Path | None, ctl: Path | None = None) -> None:
        super().__init__(bin, ctl)
        self._profile: Profile | None = None
        self._probe_error: Exception | None = None

    def weston_args(self) -> tuple[str, ...]:
        # through the module: the self-tests swap in their fake weston there
        base.require_fake_seat(base.compositor_program())
        return (*super().weston_args(), FAKE_SEAT_ARG)

    def platform_env(self, env: dict[str, str]) -> None:
        super().platform_env(env)
        software_rendering_env(env, lavapipe_icd())

    @property
    def profile(self) -> Profile:
        """Resolved once: from the first launched instance, or by a probe launch when none ran yet."""
        if self.bin is None:
            return NO_PROFILE
        if self._probe_error is not None:
            # A fresh exception: re-raising the cached one would grow its traceback with every test.
            raise LaunchError(f"{self.name}: probe launch failed: {self._probe_error}") from self._probe_error
        if self._profile is None:
            # Remembered: a build that cannot launch must not cost every test another launch timeout.
            try:
                instance = self.launch()
            except Exception as error:
                self._probe_error = error
                raise
            self.stop(instance)
        assert self._profile is not None
        return self._profile

    def launch(
        self,
        timeout: float = LAUNCH_TIMEOUT,
        app_id: str | None = None,
        settings: Mapping[str, Any] | None = None,
        first_run: bool = False,
    ) -> Instance:
        instance = super().launch(timeout, app_id, settings, first_run)
        return instance if first_run else self._profiled(instance)

    def serving(self, instance: Instance, timeout: float = LAUNCH_TIMEOUT) -> Instance:
        return self._profiled(super().serving(instance, timeout))

    def _profiled(self, instance: Instance) -> Instance:
        if self._profile is None:
            try:
                self._profile = profile_from_version(instance.version)
            except ProfileError:
                self.stop(instance)
                raise
        return instance

    def default_ctl(self) -> Path | None:
        if self.bin is None:
            return None
        sibling = Path(self.bin).parent / self.ctl_name
        return sibling if sibling.exists() else None
