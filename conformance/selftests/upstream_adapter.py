"""A self-test double: the base `Adapter` as a Linux desktop launch in upstream's vocabulary.

It runs the machinery the real adapters share (the isolation guards, the launch env, the headless weston and bus, the
socket wait, the stop and the leftover check) with upstream's `AGTERM_` variables, `agtermctl` and zmx names, which
swift_macos.py speaks, on the Linux host the self-tests run on. No implementation stands behind it; `RustAdapter` is
the Rust build's Linux launch with its own vocabulary.
"""

from __future__ import annotations

from pathlib import Path

from agterm_conformance.impls.base import Adapter, Profile

PROFILE = Profile("0.36.0", frozenset({"posix-shell"}))


class UpstreamVocabularyAdapter(Adapter):
    name = "upstream"
    needs_app_id = True
    needs_runtime_dir = True

    @property
    def profile(self) -> Profile:
        return PROFILE

    def default_ctl(self) -> Path | None:
        if self.bin is None:
            return None
        sibling = Path(self.bin).parent / self.ctl_name
        return sibling if sibling.exists() else None
