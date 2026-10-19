"""Where `agtermctl` looks for the socket: `--socket`, then `<AGTERM_STATE_DIR>/<name>.sock`, then the platform
default. Every run here resolves to a path under the instance's temp root.

The default branch runs only with HOME and the XDG/CFFIXED home variables pointed into the temp root, and only once
the default those variables resolve to is checked to lie under the root; otherwise it skips. What answers there is
a stand-in server in this process, so the run never reaches an app it did not start."""

import json
import os
import socket
import sys
import threading
from pathlib import Path

import pytest
from agterm_conformance.client import check_under_root
from agterm_conformance.match import assert_version_result

SENTINEL = {"app": {"version": "0.0.0-resolution-probe"}}


def _env(cli, **changes):
    """The instance's env without the variables that would decide the socket; `changes` add some back."""
    env = {name: value for name, value in cli.instance.env.items() if not name.startswith("AGTERM_")}
    for name, value in changes.items():
        if value is None:
            env.pop(name, None)
        else:
            env[name] = value
    return env


def _unreachable(result, path):
    assert result.returncode == 1 and result.stdout == "", (result.argv, result.returncode, result.stdout)
    assert result.stderr.startswith(f"Error: connect({path}) failed: "), result.stderr


@pytest.fixture
def empty_state(cli):
    path = cli.instance.root / "other-state"
    path.mkdir(exist_ok=True)
    return path


@pytest.mark.covers("cli:version", "behavior:cli.socket-resolution")
def test_state_dir_names_the_socket(cli, ctl):
    version = assert_version_result(ctl.request("version"))
    env = _env(cli, AGTERM_STATE_DIR=os.fspath(cli.instance.state_dir))
    result = cli.local("version", "--json", env=env)
    assert (result.returncode, json.loads(result.stdout)) == (0, {"ok": True, "result": version})


@pytest.mark.covers("cli:version", "behavior:cli.socket-resolution")
def test_socket_option_wins_over_the_state_dir(cli, ctl, empty_state, adapter):
    version = assert_version_result(ctl.request("version"))
    env = _env(cli, AGTERM_STATE_DIR=os.fspath(empty_state))
    result = cli.local("version", "--json", "--socket", os.fspath(cli.instance.socket), env=env)
    assert (result.returncode, json.loads(result.stdout)) == (0, {"ok": True, "result": version})
    _unreachable(cli.local("version", env=env), empty_state / f"{adapter.app_dir_name}.sock")


@pytest.mark.covers("behavior:cli.socket-resolution")
def test_agterm_socket_variable_is_not_read(cli, empty_state, adapter):
    # A pane's AGTERM_SOCKET (and the app's own AGTERM_CONTROL_SOCKET) names the live instance's socket; the CLI
    # resolves from AGTERM_STATE_DIR alone, so neither can route a command anywhere.
    socket_path = os.fspath(cli.instance.socket)
    env = _env(
        cli, AGTERM_STATE_DIR=os.fspath(empty_state), AGTERM_SOCKET=socket_path, AGTERM_CONTROL_SOCKET=socket_path
    )
    _unreachable(cli.local("version", env=env), empty_state / f"{adapter.app_dir_name}.sock")


def _default_socket(env, name):
    """The platform default the CLI resolves without AGTERM_STATE_DIR, or None when unknown here:
    `<base>/<name>/<name>.sock`, the name being the implementation's (`agterm` upstream and in the fork, `ngterm` in
    the Rust build).

    Linux (the fork): Foundation's application support directory, `$XDG_DATA_HOME` when it is absolute.
    macOS (upstream): `$HOME/Library/Application Support`.
    """
    if sys.platform.startswith("linux"):
        data = env.get("XDG_DATA_HOME", "")
        return Path(data) / name / f"{name}.sock" if os.path.isabs(data) else None
    if sys.platform == "darwin":
        home = env.get("HOME", "")
        return Path(home) / "Library/Application Support" / name / f"{name}.sock" if os.path.isabs(home) else None
    return None


class StandIn:
    """Answers one `version` request on `path` with the sentinel identity."""

    def __init__(self, path):
        self.path = path
        self.requests = []
        self.server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.server.bind(os.fspath(path))
        self.server.listen(1)
        self.server.settimeout(30)
        self.thread = threading.Thread(target=self._serve, daemon=True)
        self.thread.start()

    def _serve(self):
        try:
            connection, _ = self.server.accept()
        except OSError:
            return
        with connection:
            data = b""
            while not data.endswith(b"\n"):
                chunk = connection.recv(4096)
                if not chunk:
                    return
                data += chunk
            self.requests.append(json.loads(data))
            connection.sendall(json.dumps({"ok": True, "result": SENTINEL}).encode() + b"\n")

    def close(self):
        self.server.close()
        self.thread.join(5)


@pytest.mark.covers("cli:version", "behavior:cli.socket-resolution")
def test_default_socket_without_a_state_dir(cli, adapter):
    home = cli.instance.home
    env = _env(
        cli,
        HOME=os.fspath(home),
        CFFIXED_USER_HOME=os.fspath(home),
        XDG_DATA_HOME=os.fspath(home / ".local/share"),
        XDG_CONFIG_HOME=os.fspath(home / ".config"),
        XDG_STATE_HOME=os.fspath(home / ".local/state"),
        XDG_CACHE_HOME=os.fspath(home / ".cache"),
    )
    default = _default_socket(env, adapter.app_dir_name)
    if default is None:
        pytest.skip(f"no known default socket rule for {sys.platform}")
    try:
        check_under_root(default, cli.instance.root)
    except Exception as error:  # noqa: BLE001 - any doubt about where the default lands means do not run
        pytest.skip(f"the default socket {default} does not resolve under the temp root: {error}")
    default.parent.mkdir(parents=True, exist_ok=True)
    stand_in = StandIn(default)
    try:
        result = cli.local("version", "--json", env=env, default_socket=default)
    finally:
        stand_in.close()
        default.unlink(missing_ok=True)
    assert (result.returncode, json.loads(result.stdout)) == (0, {"ok": True, "result": SENTINEL}), result.stderr
    assert stand_in.requests == [{"cmd": "version"}]
