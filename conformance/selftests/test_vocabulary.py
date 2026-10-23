"""Each adapter's vocabulary: upstream's names pass through for the Swift references, the Rust build's are `live`."""

import re
from pathlib import Path

import pytest
from agterm_conformance import cli_format
from agterm_conformance.impls import ADAPTERS
from agterm_conformance.impls.rust import RustAdapter
from agterm_conformance.vocabulary import (
    LIVE,
    LIVE_TEXTS,
    NAME_TEXTS,
    SERVER_IDLE_GRACE,
    UPSTREAM,
    live_socket_directory,
    upstream_socket_directory,
)

REPO = Path(__file__).resolve().parents[2]
COMMANDS = ("list", "prune", "kill", "reset", "tree", "attach", "present", "screen")


def test_the_rust_adapter_speaks_live_and_the_reference_upstream():
    assert RustAdapter.vocabulary is LIVE
    assert {name: adapter.vocabulary for name, adapter in ADAPTERS.items() if adapter is not RustAdapter} == {
        "swift-macos": UPSTREAM,
    }


@pytest.mark.parametrize("command", COMMANDS)
def test_upstream_names_pass_through_and_live_renames_the_group(command):
    assert UPSTREAM.command(f"zmx.{command}") == f"zmx.{command}"
    assert LIVE.command(f"zmx.{command}") == f"live.{command}"
    assert UPSTREAM.cli("zmx", command, "--json") == ("zmx", command, "--json")
    assert LIVE.cli("zmx", command, "--json") == ("live", command, "--json")


def test_keys_and_variables_per_vocabulary():
    assert (UPSTREAM.result_key, UPSTREAM.backed_key, UPSTREAM.ui_test_opt_in) == (
        "zmx",
        "backedByZmx",
        "AGTERM_UITEST_ENABLE_ZMX",
    )
    assert (LIVE.result_key, LIVE.backed_key, LIVE.ui_test_opt_in) == ("live", "backedByLive", "NGTERM_UITEST_ENABLE_LIVE")
    # The attach contract and the namespace: upstream's zmx's, and the Rust build's session server's.
    assert (UPSTREAM.session_variable, UPSTREAM.directory_variable) == ("ZMX_SESSION", "ZMX_DIR")
    assert (LIVE.session_variable, LIVE.directory_variable) == ("NGTERM_LIVE_SESSION", "NGTERM_LIVE_DIR")
    assert (UPSTREAM.daemon_prefix, LIVE.daemon_prefix) == ("agterm-", "ngterm-")
    for vocabulary in (UPSTREAM, LIVE):
        assert vocabulary.session_variable.startswith(vocabulary.variable_prefix)
        assert vocabulary.directory_variable.startswith(vocabulary.variable_prefix)
    assert UPSTREAM.socket_directory("/nonexistent-agterm/state", "agterm") == upstream_socket_directory(
        "/nonexistent-agterm/state"
    )
    assert LIVE.socket_directory("/nonexistent-agterm/state", "ngterm") == live_socket_directory(
        "/nonexistent-agterm/state"
    )
    assert upstream_socket_directory("/nonexistent-agterm/state").startswith("/tmp/agterm-zmx-")
    # the same hash under the session server's prefix: ngterm_core::live::socket_directory's test pins its value
    assert live_socket_directory("/nonexistent-agterm/state") == "/tmp/ngterm-live-22c4fb0b9e3e04dc"
    assert upstream_socket_directory("/nonexistent-agterm/state").endswith("-22c4fb0b9e3e04dc")
    # one server per namespace for the Rust build, a daemon per session for upstream
    assert (UPSTREAM.server, UPSTREAM.server_idle) == (None, 0.0)
    assert (LIVE.server, LIVE.server_idle) == ("ngterm-session", SERVER_IDLE_GRACE)


@pytest.mark.parametrize("name", ["tree", "session.new", "zmx", "zmx.", "live.list"])
def test_only_upstream_group_commands_translate(name):
    for vocabulary in (UPSTREAM, LIVE):
        with pytest.raises(ValueError, match="not a command of upstream's zmx group"):
            vocabulary.command(name)


@pytest.mark.parametrize("args", [(), ("live", "list"), ("session", "new")])
def test_only_upstream_group_command_lines_translate(args):
    with pytest.raises(ValueError, match="agtermctl zmx"):
        LIVE.cli(*args)


def test_upstream_texts_pass_through_with_their_fields():
    assert UPSTREAM.text("zmx.kill requires --force") == "zmx.kill requires --force"
    assert UPSTREAM.text("could not read the zmx screen of {name}", name="x") == "could not read the zmx screen of x"
    assert UPSTREAM.text("invalid host") == "invalid host"


def test_live_texts_rename_every_zmx_and_refuse_an_unlisted_one():
    assert LIVE.text("zmx.kill requires --force") == "live.kill requires --force"
    assert LIVE.text("could not read the zmx screen of {name}", name="x") == "could not read the live session screen of x"
    # A text naming no zmx is the same in both.
    assert LIVE.text("invalid host") == "invalid host"
    with pytest.raises(KeyError, match="no text for 'the zmx thing'"):
        LIVE.text("the zmx thing")
    for upstream, live in LIVE_TEXTS.items():
        assert "zmx" in upstream and "zmx" not in live, (upstream, live)
        assert re.findall(r"{\w+}", upstream) == re.findall(r"{\w+}", live), (upstream, live)


def _rust_sources() -> str:
    return "\n".join(path.read_text() for path in (REPO / "crates").rglob("*.rs"))


def test_every_live_text_is_what_the_rust_build_says():
    # Each translated text, up to its first field, is a literal in the Rust sources: a rewording there fails here, not
    # only in a full conformance run.
    sources = _rust_sources()
    for live in [*LIVE_TEXTS.values(), *NAME_TEXTS.values()]:
        assert live.split("{", 1)[0] in sources, live


def test_the_live_vocabulary_renders_the_live_inventory():
    restore = {"configured": "live", "requestedAtLaunch": "live", "active": "live", "restartRequired": False}
    inventory = {"restore": restore, "inventoryComplete": True, "entries": []}
    assert cli_format.human({"live": inventory}, vocabulary=LIVE) == cli_format.zmx(inventory, LIVE)
    assert cli_format.human({"zmx": inventory}) == cli_format.zmx(inventory)
    # Under the other vocabulary's key the inventory is not recognized as one.
    assert cli_format.human({"live": inventory}) == "ok"


def test_each_vocabulary_leads_with_its_own_hosts_client(monkeypatch):
    seen = []
    monkeypatch.setattr("agterm_conformance.zmx_client.leading_client", lambda *args: seen.append(("zmx", *args)))
    monkeypatch.setattr("agterm_conformance.live_client.leading_client", lambda *args: seen.append(("live", *args)))
    for vocabulary in (UPSTREAM, LIVE):
        vocabulary.leading_client("instance", {"socketDirectory": "/d"}, "agterm-x")
    assert seen == [
        ("zmx", "instance", {"socketDirectory": "/d"}, "agterm-x"),
        ("live", "instance", {"socketDirectory": "/d"}, "agterm-x"),
    ]


def test_the_server_idle_grace_is_the_session_servers():
    lifecycle = (REPO / "crates/ngterm-session/src/server/lifecycle.rs").read_text()
    match = re.search(r"pub const IDLE_GRACE: Duration = Duration::from_secs\((\d+)\);", lifecycle)
    assert match is not None and float(match.group(1)) == SERVER_IDLE_GRACE


def test_the_daemon_prefix_and_attach_contract_are_the_session_servers():
    live = (REPO / "crates/ngterm-core/src/live/mod.rs").read_text()
    assert f'pub const NAME_PREFIX: &str = "{LIVE.daemon_prefix}";' in live
    assert f'pub const SESSION_VARIABLE: &str = "{LIVE.session_variable}";' in live
    assert f'pub const DIRECTORY_VARIABLE: &str = "{LIVE.directory_variable}";' in live
