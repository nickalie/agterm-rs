"""Each adapter's vocabulary: upstream's names pass through for the Swift references, the Rust build's are `live`."""

import re
from pathlib import Path

import pytest
from agterm_conformance import cli_format
from agterm_conformance.impls import ADAPTERS
from agterm_conformance.impls.rust import RustAdapter
from agterm_conformance.vocabulary import LIVE, LIVE_TEXTS, UPSTREAM, upstream_socket_directory

REPO = Path(__file__).resolve().parents[2]
COMMANDS = ("list", "prune", "kill", "reset", "tree", "attach", "present", "screen")


def test_the_rust_adapter_speaks_live_and_the_references_upstream():
    assert RustAdapter.vocabulary is LIVE
    assert {name: adapter.vocabulary for name, adapter in ADAPTERS.items() if adapter is not RustAdapter} == {
        "swift-macos": UPSTREAM,
        "swift-linux": UPSTREAM,
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
    assert (LIVE.result_key, LIVE.backed_key, LIVE.ui_test_opt_in) == ("live", "backedByLive", "AGTERM_UITEST_ENABLE_LIVE")
    # The attach contract, the namespace and the leading client are the bundled zmx's until the session server.
    for vocabulary in (UPSTREAM, LIVE):
        assert (vocabulary.session_variable, vocabulary.directory_variable) == ("ZMX_SESSION", "ZMX_DIR")
        assert vocabulary.session_variable.startswith(vocabulary.variable_prefix)
        assert vocabulary.socket_directory("/nonexistent-agterm/state") == upstream_socket_directory(
            "/nonexistent-agterm/state"
        )


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
    for live in LIVE_TEXTS.values():
        assert live.split("{", 1)[0] in sources, live


def test_the_live_vocabulary_renders_the_live_inventory():
    restore = {"configured": "live", "requestedAtLaunch": "live", "active": "live", "restartRequired": False}
    inventory = {"restore": restore, "inventoryComplete": True, "entries": []}
    assert cli_format.human({"live": inventory}, vocabulary=LIVE) == cli_format.zmx(inventory)
    assert cli_format.human({"zmx": inventory}) == cli_format.zmx(inventory)
    # Under the other vocabulary's key the inventory is not recognized as one.
    assert cli_format.human({"live": inventory}) == "ok"


def test_both_vocabularies_lead_with_the_zmx_client(monkeypatch):
    seen = []
    monkeypatch.setattr("agterm_conformance.zmx_client.leading_client", lambda *args: seen.append(args) or "client")
    for vocabulary in (UPSTREAM, LIVE):
        assert vocabulary.leading_client("instance", {"socketDirectory": "/d"}, "agterm-x") == "client"
    assert seen == [("instance", {"socketDirectory": "/d"}, "agterm-x")] * 2
