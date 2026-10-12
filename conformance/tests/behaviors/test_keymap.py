"""When `keymap.conf` takes effect: read at launch, and after an edit only once `keymap.reload` runs. Each test runs
its own instance; the file is the instance's `<state>/config/keymap.conf`, and every custom command only writes a
file in the instance root, by absolute path."""

import os
import shlex

import pytest
from agterm_conformance.match import assert_error, assert_ok
from agterm_conformance.wait import wait_until


def _keymap(ctl):
    return ctl.instance.state_dir / "config" / "keymap.conf"


def _command(name, marker):
    """A `command` line that writes the session it ran for into `marker`."""
    tmp, done = shlex.quote(os.fspath(marker) + ".tmp"), shlex.quote(os.fspath(marker))
    return f'command "{name}" printf \'%s\' "$AGT_SESSION_ID" > {tmp} && mv {tmp} {done}\n'


def _commands(ctl):
    return [command["name"] for command in assert_ok(ctl.request("keymap.list"))["keymap"]["commands"]]


def _ran(ctl, name, marker):
    sid = assert_ok(ctl.request("tree"))["tree"]["workspaces"][0]["sessions"][0]["id"]
    assert assert_ok(ctl.request("keymap.run", sid, name=name)) == {"id": sid}
    assert wait_until(lambda: marker.exists() and marker.read_text()) == sid


@pytest.mark.covers("behavior:keymap.loaded-at-launch")
def test_keymap_file_is_read_at_launch(launch):
    ctl = launch()
    marker = ctl.instance.root / "launch-marker"
    _keymap(ctl).write_text(_command("from-launch", marker))
    ctl = launch.relaunch(ctl)
    assert _commands(ctl) == ["from-launch"]
    _ran(ctl, "from-launch", marker)


@pytest.mark.covers("behavior:keymap.edit-needs-reload")
def test_an_edit_takes_effect_only_on_reload(launch):
    ctl = launch()
    first, second = ctl.instance.root / "first-marker", ctl.instance.root / "second-marker"
    _keymap(ctl).write_text(_command("first", first))
    assert_ok(ctl.request("keymap.reload"))
    assert _commands(ctl) == ["first"]

    _keymap(ctl).write_text(_command("second", second))
    assert _commands(ctl) == ["first"]
    assert_error(ctl.request("keymap.run", name="second"), "no custom command named second")
    _ran(ctl, "first", first)

    assert_ok(ctl.request("keymap.reload"))
    assert _commands(ctl) == ["second"]
    assert_error(ctl.request("keymap.run", name="first"), "no custom command named first")
    _ran(ctl, "second", second)
