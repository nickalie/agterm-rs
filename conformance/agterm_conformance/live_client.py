"""A second client of one of the Rust build's live sessions: the local stand-in for a viewer attached over ssh.

The Rust build's counterpart of `zmx_client.leading_client`: its sessions run in one session server per instance
(`agterm-session`), so the viewer is `agterm-session attach`, started as `agterm_core::remote::ssh::attach_command`
starts the viewer's on the origin, minus ssh: the attach contract's variables (`AGTERM_LIVE_*`, empty meaning unset),
explicit leadership claimed, and the create-only guard after the session's name.
"""

from __future__ import annotations

import os
import uuid
from collections.abc import Mapping
from typing import Any

from agterm_conformance.impls.base import MINIMAL_PATH
from agterm_conformance.zmx_client import GONE_GUARD, PtyProcess, ZmxInstance


def leading_client(instance: ZmxInstance, endpoint: Mapping[str, Any], session: str) -> PtyProcess:
    """`agterm-session attach <session>` claiming the lead, so the pane's own client in `instance` follows.

    `endpoint` is the `live.tree` read-back; one naming another instance's namespace is refused.
    """
    executable, directory = endpoint["executable"], endpoint["socketDirectory"]
    if directory != instance.live_dir:
        raise ValueError(f"endpoint socket directory {directory!r} is not this instance's {instance.live_dir!r}")
    if not os.path.isabs(executable):
        raise ValueError(f"endpoint executable {executable!r} is not absolute")
    env = {
        "PATH": MINIMAL_PATH,
        "HOME": os.fspath(instance.home),
        "TERM": "xterm-256color",
        # The variables the origin's own pane sets (`agterm_core::live::support`), empty meaning unset.
        "AGTERM_LIVE_SESSION": "",
        "AGTERM_LIVE_SESSION_PREFIX": "",
        "AGTERM_LIVE_NO_DETACH_KEY": "1",
        "AGTERM_LIVE_DIR": directory,
        "AGTERM_LIVE_MANAGED": uuid.uuid4().hex,
        "AGTERM_LIVE_MANAGED_CLAIM": "1",
    }
    return PtyProcess([executable, "attach", session, "/bin/sh", "-c", GONE_GUARD], env)
