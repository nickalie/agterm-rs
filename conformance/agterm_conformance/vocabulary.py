"""What each implementation calls the live-session control group, so the shared tests can keep upstream's names.

The tests, the catalog and the `covers(...)` markers speak upstream's vocabulary (`zmx.list`, `result.zmx`,
`backedByZmx`, `agtermctl zmx`), because the Swift references answer to it. Each adapter carries a `Vocabulary` that
translates every vocabulary-dependent value a test or the harness sends or reads: the Swift references' is upstream's
own, and the Rust build's renames the group to `live` (conformance/docs/rust-build.md > The `live` group).

The live sessions' host differs too: the Swift references run upstream's zmx, a daemon per session under `ZMX_*`
variables in `/tmp/<name>-zmx-<hash>`, named `agterm-<pane>`; the Rust build runs one session server per instance
(`ngterm-session`) under its attach contract's `NGTERM_LIVE_*` variables in `/tmp/ngterm-live-<hash>`, its sessions
named `ngterm-<pane>`. Each vocabulary carries its host's environment names, socket directory, daemon names, leading
client and leftover processes.
"""

from __future__ import annotations

import os
import sys
from collections.abc import Mapping
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from collections.abc import Callable

    from agterm_conformance.zmx_client import PtyProcess

# How long the Rust build's session server outlives its last client and session (`IDLE_GRACE`,
# crates/ngterm-session/src/server/lifecycle.rs): the leftover check gives it that long to exit by itself.
SERVER_IDLE_GRACE = 3.0

# Upstream's name for the group, as the shared tests spell it.
UPSTREAM_GROUP = "zmx"


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


def upstream_socket_directory(state_dir: str | os.PathLike[str], name: str = "agterm") -> str:
    """`ZmxSupport.socketDirectory`: FNV-1a 64 of the canonical state dir, under `/tmp`, prefixed with the
    implementation's directory name (`Adapter.app_dir_name`: upstream's and the fork's `agterm`, the Rust build's
    `ngterm`)."""
    return f"/tmp/{name}-zmx-{fnv1a64(_foundation_resolved(os.path.realpath(state_dir)))}"


def live_socket_directory(state_dir: str | os.PathLike[str], name: str = "ngterm") -> str:
    """`ngterm_core::live::socket_directory` on Unix: upstream's hash under `/tmp/<name>-live-`, the Rust build's
    namespace of its session server."""
    return f"/tmp/{name}-live-{fnv1a64(_foundation_resolved(os.path.realpath(state_dir)))}"


def _zmx_leading_client(instance: Any, endpoint: Mapping[str, Any], daemon: str) -> PtyProcess:
    # Late: zmx_client imports the adapter base, which imports this module.
    from agterm_conformance import zmx_client

    return zmx_client.leading_client(instance, endpoint, daemon)


def _live_leading_client(instance: Any, endpoint: Mapping[str, Any], daemon: str) -> PtyProcess:
    # Late, as `_zmx_leading_client`.
    from agterm_conformance import live_client

    return live_client.leading_client(instance, endpoint, daemon)


@dataclass(frozen=True)
class Vocabulary:
    """One implementation's names for what the shared tests call by upstream's."""

    name: str
    # The control-protocol group (`<group>.list`) and the CLI's subcommand group.
    group: str
    # `result.<key>` of `<group>.list`.
    result_key: str
    # `tree.session.<key>` and `tree.session.surfaces[].<key>`.
    backed_key: str
    # The variable a UI-test launch sets to keep live sessions.
    ui_test_opt_in: str
    # The attach contract's variables a live pane's children carry, which the CLI's `clipboard set` reads.
    session_variable: str
    directory_variable: str
    # Their common prefix, which a test strips to stand outside any live pane.
    variable_prefix: str
    # The prefix of the daemon name the app gives a pane's live session (`<prefix><pane identity>`).
    daemon_prefix: str
    # Upstream error and output texts (`str.format` templates) to this implementation's; one not listed is unchanged.
    texts: Mapping[str, str] = field(default_factory=dict)
    # `socket_directory(state_dir, app_dir_name)`: the instance's namespace, named by the adapter's `app_dir_name`.
    socket_directory: Callable[[str | os.PathLike[str], str], str] = upstream_socket_directory
    # `leading_client(instance, endpoint, daemon)`: a second client of a daemon that claims its lead.
    leading_client: Callable[[Any, Mapping[str, Any], str], PtyProcess] = _zmx_leading_client
    # The executable of the one server per namespace that hosts every live session (`<server> server <directory>`),
    # or None where every session is a daemon of its own (zmx); with one, every process carrying the directory
    # variable is the namespace's too, and the leftover check gives the server `server_idle` seconds more to exit.
    server: str | None = None
    server_idle: float = 0.0

    def command(self, upstream: str) -> str:
        """The wire name of upstream's `zmx.<name>`."""
        group, dot, rest = upstream.partition(".")
        if group != UPSTREAM_GROUP or not dot or not rest:
            raise ValueError(f"{upstream!r} is not a command of upstream's {UPSTREAM_GROUP} group")
        return f"{self.group}.{rest}"

    def cli(self, *args: str) -> tuple[str, ...]:
        """The CLI argv of upstream's `agtermctl zmx ...`."""
        if not args or args[0] != UPSTREAM_GROUP:
            raise ValueError(f"{args!r} is not an `agtermctl {UPSTREAM_GROUP}` command line")
        return (self.group, *args[1:])

    def text(self, upstream: str, **fields: Any) -> str:
        """This implementation's form of upstream's text `upstream`, with `fields` filled in.

        A translating vocabulary refuses an unlisted text that names upstream's group: passing it through unchanged
        would let a test assert a string the implementation no longer prints.
        """
        translated = self.texts.get(upstream)
        if translated is None:
            if self.group != UPSTREAM_GROUP and UPSTREAM_GROUP in upstream:
                raise KeyError(f"the {self.name} vocabulary has no text for {upstream!r}")
            translated = upstream
        return translated.format(**fields) if fields else translated


UPSTREAM = Vocabulary(
    name="upstream",
    group=UPSTREAM_GROUP,
    result_key="zmx",
    backed_key="backedByZmx",
    ui_test_opt_in="AGTERM_UITEST_ENABLE_ZMX",
    session_variable="ZMX_SESSION",
    directory_variable="ZMX_DIR",
    variable_prefix="ZMX_",
    # `ZmxSupport.namePrefix`
    daemon_prefix="agterm-",
)

# The upstream texts the Rust build words in the live-session vocabulary, each as the shared tests template it; the
# full list of changed strings is in conformance/docs/rust-build.md.
LIVE_TEXTS: Mapping[str, str] = {
    "zmx.kill requires an explicit --target": "live.kill requires an explicit --target",
    "zmx.kill requires --pane left|right": "live.kill requires --pane left|right",
    "zmx.kill requires --force": "live.kill requires --force",
    "zmx.kill needs a session id; 'active' is not accepted": "live.kill needs a session id; 'active' is not accepted",
    "zmx.kill needs a window id; 'active' is not accepted": "live.kill needs a window id; 'active' is not accepted",
    "zmx.screen requires a daemon name": "live.screen requires a daemon name",
    "zmx.reset requires --force": "live.reset requires --force",
    "zmx.present requires a session": "live.present requires a session",
    "zmx.attach requires a host": "live.attach requires a host",
    "could not read the zmx screen of {name}": "could not read the live session screen of {name}",
    "could not read the zmx session list": "could not read the live session list",
    "zmx is unavailable in this instance": "live sessions are unavailable in this instance",
    "this pane has no zmx daemon; run it in a main or split pane started under Live sessions": (
        "this pane has no live session; run it in a main or split pane started under Live sessions"
    ),
}

# The upstream texts that name the app, in the Rust build's own name (crates/ngterm-ctl/src/output.rs).
NAME_TEXTS: Mapping[str, str] = {
    "restart agterm to apply the configured mode": "restart ngterm to apply the configured mode",
}

LIVE = Vocabulary(
    name="live",
    group="live",
    result_key="live",
    backed_key="backedByLive",
    ui_test_opt_in="NGTERM_UITEST_ENABLE_LIVE",
    # The session server's attach contract (crates/ngterm-session/src/cli.rs).
    session_variable="NGTERM_LIVE_SESSION",
    directory_variable="NGTERM_LIVE_DIR",
    variable_prefix="NGTERM_LIVE_",
    # `ngterm_core::live::daemon_name`
    daemon_prefix="ngterm-",
    texts={**LIVE_TEXTS, **NAME_TEXTS},
    socket_directory=live_socket_directory,
    leading_client=_live_leading_client,
    server="ngterm-session",
    server_idle=SERVER_IDLE_GRACE,
)
