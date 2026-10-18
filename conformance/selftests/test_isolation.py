"""The suite reaches nothing outside an instance's temp root: what each launch env names, and where connects live."""

import ast
from pathlib import Path

import pytest
from agterm_conformance.impls import ADAPTERS
from agterm_conformance.impls.base import XDG_HOMES, check_launch_env, upstream_socket_directory

CONFORMANCE = Path(__file__).parents[1]
PACKAGE = CONFORMANCE / "agterm_conformance"
# Every socket connect the harness makes, by file and enclosing function. The client's re-check the root on
# each connect; `wait_for_unix_socket` probes the compositor and bus sockets its callers checked.
CONNECT_SITES = {
    ("client.py", "open_stream"),
    ("client.py", "_exchange"),
    ("impls/base.py", "wait_for_unix_socket"),
}
CONNECT_CALLS = {"connect", "connect_ex", "create_connection", "urlopen"}


def _calls(tree: ast.AST) -> list[tuple[str, int]]:
    names = []
    for node in ast.walk(tree):
        if isinstance(node, ast.Call):
            func = node.func
            name = func.attr if isinstance(func, ast.Attribute) else func.id if isinstance(func, ast.Name) else ""
            names.append((name, node.lineno))
    return names


def _connect_sites(path: Path) -> set[tuple[str, str]]:
    tree = ast.parse(path.read_text())
    sites = set()
    for function in ast.walk(tree):
        if isinstance(function, ast.FunctionDef | ast.AsyncFunctionDef):
            if any(name in CONNECT_CALLS for name, _ in _calls(function)):
                sites.add((path.relative_to(PACKAGE).as_posix(), function.name))
    return sites


def test_the_harness_connects_only_where_the_root_is_checked() -> None:
    sites = set().union(*(_connect_sites(path) for path in PACKAGE.rglob("*.py")))
    assert sites == CONNECT_SITES
    for file, function in CONNECT_SITES:
        tree = ast.parse((PACKAGE / file).read_text())
        (node,) = [n for n in ast.walk(tree) if isinstance(n, ast.FunctionDef) and n.name == function]
        if file == "client.py":
            assert "check_under_root" in {name for name, _ in _calls(node)}, function


def test_conformance_tests_reach_the_app_only_through_fixtures() -> None:
    # No socket connect, no own process: a test talks to its instance through `ctl`/`cli`/`ui`, which carry the
    # root check and `--socket`.
    offenders = []
    for path in sorted((CONFORMANCE / "tests").rglob("*.py")):
        tree = ast.parse(path.read_text())
        for name, line in _calls(tree):
            if name in CONNECT_CALLS | {"Popen", "system", "Client"}:
                offenders.append(f"{path.relative_to(CONFORMANCE)}:{line} {name}")
        for node in ast.walk(tree):
            if isinstance(node, ast.Import | ast.ImportFrom):
                modules = [node.module] if isinstance(node, ast.ImportFrom) else [a.name for a in node.names]
                if "subprocess" in modules:
                    offenders.append(f"{path.relative_to(CONFORMANCE)}:{node.lineno} import subprocess")
    assert offenders == []


@pytest.mark.parametrize("name", sorted(ADAPTERS))
def test_every_location_a_launch_env_names_is_under_the_root(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, name: str
) -> None:
    for variable, value in {
        "HOME": "/home/user",
        "XDG_RUNTIME_DIR": "/run/user/7",
        "WAYLAND_DISPLAY": "wayland-1",
        "DISPLAY": ":0",
        "DBUS_SESSION_BUS_ADDRESS": "unix:path=/run/user/7/bus",
        "AT_SPI_BUS_ADDRESS": "unix:path=/run/user/7/at-spi/bus",
        "AGTERM_SOCKET": "/home/user/.local/share/agterm/agterm.sock",
        "AGTERM_STATE_DIR": "/home/user/.local/share/agterm",
        "ZMX_DIR": "/tmp/agterm-zmx-live",
        **{xdg: f"/home/user/{xdg.lower()}" for xdg in XDG_HOMES},
    }.items():
        monkeypatch.setenv(variable, value)
    adapter = ADAPTERS[name](tmp_path / "app")
    root = tmp_path / "root"
    env = adapter.build_env(root)
    linux = adapter.needs_runtime_dir
    check_launch_env(env, root, adapter.needs_app_id, linux, headless=linux)

    located = ["AGTERM_STATE_DIR", "AGTERM_CONTROL_SOCKET", "HOME", *XDG_HOMES, "CFFIXED_USER_HOME"]
    located += ["XDG_RUNTIME_DIR", "WAYLAND_DISPLAY"] if linux else []
    for variable in located:
        if variable in env:
            assert Path(env[variable]).is_relative_to(root), variable
    if linux:
        assert Path(env["DBUS_SESSION_BUS_ADDRESS"].removeprefix("unix:path=")).is_relative_to(root)
        assert not {"DISPLAY", "AT_SPI_BUS_ADDRESS"} & env.keys()
    assert not {"AGTERM_SOCKET", "ZMX_DIR"} & env.keys()
    # The app derives its zmx socket directory from the state dir: per root, never the live one.
    assert upstream_socket_directory(env["AGTERM_STATE_DIR"]) != upstream_socket_directory("/home/user/.local/share/agterm")
