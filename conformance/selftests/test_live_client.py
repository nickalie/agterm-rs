from pathlib import Path
from types import SimpleNamespace

import pytest
from agterm_conformance.live_client import leading_client
from agterm_conformance.wait import wait_until
from agterm_conformance.live_client import GONE_GUARD
from agterm_conformance.zmx_client import PtyProcess


def _script(tmp_path: Path, body: str) -> Path:
    path = tmp_path / "ngterm-session"
    path.write_text("#!/bin/sh\n" + body)
    path.chmod(0o755)
    return path


def _printed(process: PtyProcess, text: str) -> None:
    wait_until(lambda: text in process.output() or process.process.poll() is not None, timeout=5)
    assert text in process.output(), process.output()


@pytest.fixture
def instance(tmp_path: Path) -> SimpleNamespace:
    return SimpleNamespace(live_dir=str(tmp_path / "ngterm-live-x"), home=tmp_path / "home")


def test_leading_client_attaches_under_the_attach_contract_claiming_the_lead(
    tmp_path: Path, instance: SimpleNamespace
) -> None:
    fake = _script(
        tmp_path,
        'printf "%s|" "$@"; echo\n'
        'echo "env $NGTERM_LIVE_DIR $NGTERM_LIVE_MANAGED_CLAIM $NGTERM_LIVE_NO_DETACH_KEY '
        '[${NGTERM_LIVE_SESSION-unset}] [${NGTERM_LIVE_SESSION_PREFIX-unset}]"\n'
        'echo "nonce ${#NGTERM_LIVE_MANAGED} home $HOME zmx ${ZMX_DIR-unset}"\n'
        "exec cat\n",
    )
    endpoint = {"executable": str(fake), "socketDirectory": instance.live_dir}
    with leading_client(instance, endpoint, "ngterm-abc") as client:
        _printed(client, f"attach|ngterm-abc|/bin/sh|-c|{GONE_GUARD}|")
        _printed(client, f"env {instance.live_dir} 1 1 [] []")
        _printed(client, f"nonce 32 home {instance.home} zmx unset")


def test_leading_client_refuses_another_instances_namespace(instance: SimpleNamespace) -> None:
    with pytest.raises(ValueError, match="not this instance's"):
        leading_client(
            instance, {"executable": "/bin/true", "socketDirectory": "/tmp/ngterm-live-other"}, "ngterm-abc"
        )


def test_leading_client_refuses_a_relative_executable(instance: SimpleNamespace) -> None:
    with pytest.raises(ValueError, match="not absolute"):
        leading_client(instance, {"executable": "ngterm-session", "socketDirectory": instance.live_dir}, "ngterm-abc")
