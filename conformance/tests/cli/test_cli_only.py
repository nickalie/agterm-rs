"""The two `agtermctl` subcommands that never open the socket: `clipboard set` and `terminfo install`.

Both run through `cli.local`, without `--socket` (they refuse it) and in the instance's env, whose
`AGTERM_STATE_DIR` is under the temp root. `clipboard set` writes nothing outside a Live pane, and inside one the
instance's terminals drop the OSC 52 write (`clipboard-write = deny`), so the desktop clipboard is never touched.
`terminfo install` talks to a stand-in `ssh` on PATH that runs the remote command against the isolated HOME."""

import base64
import os
import pwd
import shlex
import shutil
import stat
import uuid
from pathlib import Path

import pytest
from agterm_conformance.match import assert_ok, find_session
from agterm_conformance.wait import wait_until
from agterm_conformance.zmx_client import leading_client

# `TerminalClipboard.maxEncodedBytes`: under the 8 MiB above which libghostty drops an OSC whole.
MAX_ENCODED = 8 * 1024 * 1024 - 64
ENTRY = "xterm-ghostty"
NOT_LIVE = "this pane has no zmx daemon; run it in a main or split pane started under Live sessions"


def _failed(result, message, code=1):
    """A local failure: `Error: <message>` on stderr and no usage block under it."""
    assert (result.returncode, result.stdout, result.stderr) == (code, "", f"Error: {message}\n"), result.argv


def _usage(result, message=None):
    assert (result.returncode, result.stdout) == (64, ""), (result.argv, result.returncode, result.stderr)
    assert result.stderr.startswith(f"Error: {message}\n" if message else "Error: "), result.stderr
    assert "\nUsage: agtermctl " in result.stderr, result.stderr


def _env(cli, **extra):
    env = {name: value for name, value in cli.instance.env.items() if not name.startswith("ZMX_")}
    env.update(extra)
    return env


# -- clipboard set


@pytest.mark.covers("cli:clipboard set")
def test_clipboard_set_refuses_outside_a_live_pane(cli):
    _failed(cli.local("clipboard", "set", "hello", input=""), NOT_LIVE)
    _failed(cli.local("clipboard", "set", input="hello"), NOT_LIVE)
    # A ZMX_SESSION agterm would never have named is not printed into.
    env = _env(cli, ZMX_SESSION="someone-elses-session", ZMX_DIR=cli.instance.zmx_dir)
    _failed(cli.local("clipboard", "set", "hello", env=env, input=""), NOT_LIVE)


@pytest.mark.covers("cli:clipboard set")
def test_clipboard_set_checks_the_text_before_the_pane(cli):
    _failed(cli.local("clipboard", "set", input=""), "nothing to copy")
    _failed(cli.local("clipboard", "set", "", input=""), "nothing to copy")
    size = 6_300_000
    encoded = len(base64.b64encode(b"x" * size))
    assert encoded > MAX_ENCODED
    message = f"text is too large to copy through the terminal: {encoded} bytes encoded, the limit is {MAX_ENCODED}"
    _failed(cli.local("clipboard", "set", input="x" * size), message)
    # Local-only: there is no socket to name.
    _usage(cli.local("clipboard", "set", "--socket", os.fspath(cli.instance.socket), "hello", input=""))


@pytest.mark.requires(
    "live-sessions", reason="needs panes in zmx daemons, which the references create only for a zsh login shell"
)
@pytest.mark.covers("cli:clipboard set")
def test_clipboard_set_reaches_every_terminal_attached_to_the_pane(cli, launch):
    ctl = launch(settings={"restoreMode": "live"})
    local = cli.on(ctl.instance)
    sid = assert_ok(ctl.request("session.new"))["id"]
    ctl.wait_tree(lambda tree: find_session(tree, sid).get("backedByZmx") is True)
    rows = assert_ok(ctl.request("zmx.list"))["zmx"]["entries"]
    (daemon,) = [row["daemon"] for row in rows if row.get("sessionID") == sid and row["pane"] == "left"]
    endpoint = assert_ok(ctl.request("zmx.tree"))["remote"]["endpoint"]
    text = f"clip-{uuid.uuid4().hex[:8]}"
    sequence = f"\x1b]52;c;{base64.b64encode(text.encode()).decode()}\x07"
    with leading_client(ctl.instance, endpoint, daemon) as viewer:
        env = _env(local, ZMX_SESSION=daemon, ZMX_DIR=ctl.instance.zmx_dir)
        result = local.local("clipboard", "set", text, env=env, input="")
        assert (result.returncode, result.stdout, result.stderr) == (0, "", "")
        wait_until(lambda: sequence in viewer.output())
    assert_ok(ctl.request("session.close", sid))


# -- terminfo install


def _account_terminfo():
    """The real account's own terminfo directories, which the install must leave alone."""
    home = Path(pwd.getpwuid(os.getuid()).pw_dir)
    return [home / ".terminfo", home / ".local/share/terminfo"]


def _snapshot(directories):
    state = {}
    for directory in directories:
        if not directory.exists():
            state[directory] = None
            continue
        state[directory] = sorted(
            (os.fspath(path.relative_to(directory)), path.stat().st_mtime_ns, path.stat().st_size)
            for path in directory.rglob("*")
        )
    return state


@pytest.fixture
def fake_ssh(cli):
    """A stand-in `ssh` first on PATH: it records its argv and runs the remote command line the way sshd hands it to
    the account's shell, with HOME the instance's isolated HOME and nothing else inherited (TERMINFO included)."""
    root = cli.instance.root
    folder = root / f"fake-ssh-{uuid.uuid4().hex[:8]}"
    folder.mkdir()
    record = folder / "argv"
    script = folder / "ssh"
    script.write_text(
        "#!/bin/sh\n"
        f"for arg; do printf '%s\\n' \"$arg\"; done > {shlex.quote(os.fspath(record))}\n"
        'case "$FAKE_SSH" in exit*) exit "${FAKE_SSH#exit}" ;; signal) kill -TERM $$ ;; esac\n'
        "for last; do :; done\n"
        f'exec /usr/bin/env -i HOME={shlex.quote(os.fspath(cli.instance.home))} PATH=/usr/bin:/bin /bin/sh -c "$last"\n'
    )
    script.chmod(script.stat().st_mode | stat.S_IXUSR)

    def env(**extra):
        return _env(cli, PATH=f"{folder}:{cli.instance.env['PATH']}", **extra)

    env.record = record
    yield env
    shutil.rmtree(cli.instance.home / ".terminfo", ignore_errors=True)


@pytest.mark.covers("cli:terminfo install")
def test_terminfo_install_compiles_the_entry_into_the_remote_home(cli, fake_ssh):
    account = _account_terminfo()
    before = _snapshot(account)
    args = ("terminfo", "install", "-p", "2222", "-i", "id_one", "-i", "id_two", "-J", "jump", "-F", "cfg", "box")
    result = cli.local(*args, env=fake_ssh(), input="")
    # The remote tic's own warnings come through on stderr, as ssh passes them on.
    assert (result.returncode, result.stdout) == (0, f"installed {ENTRY} on box\n"), result.stderr
    installed = [cli.instance.home / ".terminfo" / letter / ENTRY for letter in ("x", "78")]
    assert any(path.is_file() and path.stat().st_size > 0 for path in installed), installed
    assert _snapshot(account) == before

    argv = fake_ssh.record.read_text().splitlines()
    # The connection options pass through; the remote command line comes last, after the destination.
    assert argv[-2] == "box" and shlex.split(argv[-1])[:2] == ["/bin/sh", "-c"]
    options = argv[:-2]
    for pair in (["-p", "2222"], ["-i", "id_one"], ["-i", "id_two"], ["-J", "jump"], ["-F", "cfg"]):
        assert any(options[i : i + 2] == pair for i in range(len(options) - 1)), (pair, options)
    assert "-T" in options


@pytest.mark.covers("cli:terminfo install")
def test_terminfo_install_reports_how_ssh_ended(cli, fake_ssh):
    failed = cli.local("terminfo", "install", "box", env=fake_ssh(FAKE_SSH="exit255"), input="")
    assert (failed.returncode, failed.stdout, failed.stderr) == (
        255,
        "",
        f"ssh exited 255; {ENTRY} was not installed\n",
    )
    killed = cli.local("terminfo", "install", "box", env=fake_ssh(FAKE_SSH="signal"), input="")
    message = f"ssh was killed by signal 15; {ENTRY} was not installed\n"
    assert (killed.returncode, killed.stdout, killed.stderr) == (128 + 15, "", message)
    assert not (cli.instance.home / ".terminfo").exists()


@pytest.mark.covers("cli:terminfo install")
@pytest.mark.parametrize("destination", ["-oProxyCommand=x", "two words", ""])
def test_terminfo_install_refuses_a_destination_ssh_would_misread(cli, fake_ssh, destination):
    result = cli.local("terminfo", "install", "--", destination, env=fake_ssh(), input="")
    _usage(result, f"destination must be a host, user@host or an ssh alias, not '{destination}'")
    assert not fake_ssh.record.exists()
