from pathlib import Path
from types import SimpleNamespace

import pytest
from agterm_conformance.wait import wait_until
from agterm_conformance.zmx_client import GONE_GUARD, PtyProcess, leading_client


def _script(tmp_path: Path, body: str) -> Path:
    path = tmp_path / "prog"
    path.write_text("#!/bin/sh\n" + body)
    path.chmod(0o755)
    return path


def _printed(process: PtyProcess, text: str) -> None:
    wait_until(lambda: text in process.output() or process.process.poll() is not None, timeout=5)
    assert text in process.output(), process.output()


def test_pty_process_runs_on_a_sized_terminal(tmp_path: Path) -> None:
    program = _script(tmp_path, '[ -t 0 ] && [ -t 1 ] && echo "tty $(stty size)"; exec cat\n')
    with PtyProcess([str(program)], {"PATH": "/usr/bin:/bin"}, columns=100, rows=30) as process:
        _printed(process, "tty 30 100")
        assert process.process.poll() is None
    assert process.process.returncode is not None


def test_pty_process_drains_output_so_the_program_never_blocks(tmp_path: Path) -> None:
    program = _script(tmp_path, "head -c 300000 /dev/zero | tr '\\0' x; echo; echo drained; exec cat\n")
    with PtyProcess([str(program)], {"PATH": "/usr/bin:/bin"}) as process:
        _printed(process, "drained")


def test_pty_process_stop_kills_a_program_that_ignores_sigterm(tmp_path: Path) -> None:
    program = _script(tmp_path, "trap '' TERM; echo armed; read line\n")
    process = PtyProcess([str(program)], {"PATH": "/usr/bin:/bin"})
    _printed(process, "armed")
    process.stop(timeout=0.5)
    assert process.process.returncode == -9
    process.stop()


@pytest.fixture
def instance(tmp_path: Path) -> SimpleNamespace:
    return SimpleNamespace(live_dir=str(tmp_path / "zmx"), home=tmp_path / "home")


def test_leading_client_attaches_claiming_the_lead(tmp_path: Path, instance: SimpleNamespace) -> None:
    fake_zmx = _script(
        tmp_path,
        'printf "%s|" "$@"; echo\n'
        'echo "env $ZMX_DIR $ZMX_MANAGED_CLAIM $ZMX_NO_DETACH_KEY [${ZMX_SESSION-unset}] [${ZMX_SESSION_PREFIX-unset}]"\n'
        'echo "nonce ${#ZMX_MANAGED} home $HOME"\n'
        "exec cat\n",
    )
    endpoint = {"executable": str(fake_zmx), "socketDirectory": instance.live_dir}
    with leading_client(instance, endpoint, "agterm-abc") as client:
        _printed(client, f"attach|agterm-abc|/bin/sh|-c|{GONE_GUARD}|")
        _printed(client, f"env {instance.live_dir} 1 1 [] []")
        _printed(client, f"nonce 32 home {instance.home}")


def test_leading_client_refuses_another_instances_daemons(instance: SimpleNamespace) -> None:
    with pytest.raises(ValueError, match="not this instance's"):
        leading_client(instance, {"executable": "/bin/true", "socketDirectory": "/tmp/agterm-zmx-other"}, "agterm-abc")


def test_leading_client_refuses_a_relative_executable(instance: SimpleNamespace) -> None:
    with pytest.raises(ValueError, match="not absolute"):
        leading_client(instance, {"executable": "zmx", "socketDirectory": instance.live_dir}, "agterm-abc")
