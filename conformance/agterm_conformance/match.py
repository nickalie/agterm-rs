"""Response matchers and id capture.

Comparison rules: extra fields are ignored, nil omission is pinned with `assert_absent`, numbers compare
with a tolerance (Swift encodes a whole Double as an integer), and lists are ordered unless wrapped in
`Unordered`. Ids are opaque: captured from responses and reused, never format-checked.
"""

from __future__ import annotations

import math
from collections.abc import Iterable, Mapping, Sequence
from typing import Any

from agterm_conformance.client import Response

DEFAULT_TOLERANCE = 1e-6


class _Any:
    def __repr__(self) -> str:
        return "ANY"


ANY: Any = _Any()
"""Matches any present value, including null."""


class Unordered:
    """Expected list matched as a multiset: same length, each expected item matched by a distinct actual item."""

    def __init__(self, items: Iterable[Any]) -> None:
        self.items = list(items)

    def __repr__(self) -> str:
        return f"Unordered({self.items!r})"


def assert_ok(resp: Response) -> dict[str, Any]:
    assert resp.ok, f"expected ok, got error {resp.error!r}: {resp.raw}"
    return resp.result or {}


def assert_error(resp: Response, text: str | None = None) -> None:
    """Dispatcher errors may still carry `result` (the `events.read` cursor anchor), so it is not checked."""
    assert resp.ok is False, f"expected ok:false, got {resp.raw}"
    if text is not None:
        assert resp.error == text, f"error text: expected {text!r}, got {resp.error!r}"


def assert_subset(actual: Any, expected: Any, *, tol: float = DEFAULT_TOLERANCE) -> None:
    mismatch = _mismatch(_unwrap(actual), expected, "$", tol)
    assert mismatch is None, mismatch


def assert_absent(node: Any, *paths: str) -> None:
    """Each dotted path's parent must exist (a typo must not pass) and its last key must be omitted, not null."""
    node = _unwrap(node)
    for path in paths:
        *parents, last = path.split(".")
        parent = _walk(node, parents, path)
        assert isinstance(parent, Mapping), f"{path}: parent is not an object: {parent!r}"
        assert last not in parent, f"{path}: expected omitted, got {parent[last]!r}"


def _mismatch(actual: Any, expected: Any, path: str, tol: float) -> str | None:
    if expected is ANY:
        return None
    if isinstance(expected, Unordered):
        return _unordered_mismatch(actual, expected.items, path, tol)
    if isinstance(expected, Mapping):
        if not isinstance(actual, Mapping):
            return f"{path}: expected an object, got {actual!r}"
        for key, value in expected.items():
            if key not in actual:
                return f"{path}.{key}: missing (have {sorted(actual)})"
            found = _mismatch(actual[key], value, f"{path}.{key}", tol)
            if found:
                return found
        return None
    if _is_list(expected):
        if not _is_list(actual):
            return f"{path}: expected a list, got {actual!r}"
        if len(actual) != len(expected):
            return f"{path}: expected {len(expected)} items, got {len(actual)}: {actual!r}"
        for index, (item, want) in enumerate(zip(actual, expected)):
            found = _mismatch(item, want, f"{path}[{index}]", tol)
            if found:
                return found
        return None
    if isinstance(expected, bool) or isinstance(actual, bool):
        # bool is an int subclass: JSON `true` must not match `1`.
        same = type(expected) is type(actual) and expected == actual
    elif _is_number(expected) and _is_number(actual):
        same = math.isclose(actual, expected, rel_tol=0.0, abs_tol=tol)
    else:
        same = actual == expected
    return None if same else f"{path}: expected {expected!r}, got {actual!r}"


def _unordered_mismatch(actual: Any, expected: list[Any], path: str, tol: float) -> str | None:
    if not _is_list(actual):
        return f"{path}: expected a list, got {actual!r}"
    if len(actual) != len(expected):
        return f"{path}: expected {len(expected)} items in any order, got {len(actual)}: {actual!r}"

    # Backtracking assignment: a greedy pick can strand a later item when expected entries overlap.
    def assign(index: int, used: frozenset[int]) -> bool:
        if index == len(expected):
            return True
        return any(
            assign(index + 1, used | {slot})
            for slot, item in enumerate(actual)
            if slot not in used and _mismatch(item, expected[index], path, tol) is None
        )

    if assign(0, frozenset()):
        return None
    return f"{path}: no order of {actual!r} matches {expected!r}"


def _is_list(value: Any) -> bool:
    return isinstance(value, Sequence) and not isinstance(value, (str, bytes))


def _is_number(value: Any) -> bool:
    return isinstance(value, (int, float)) and not isinstance(value, bool)


def _unwrap(node: Any) -> Any:
    if isinstance(node, Response):
        return assert_ok(node)
    return node


def _walk(node: Any, keys: Sequence[str], path: str) -> Any:
    for key in keys:
        if _is_list(node) and key.isdigit() and int(key) < len(node):
            node = node[int(key)]
        elif isinstance(node, Mapping) and key in node:
            node = node[key]
        else:
            raise AssertionError(f"{path}: no {key!r} in {node!r}")
    return node


def _tree(source: Any) -> Mapping[str, Any]:
    """Accepts a `tree` Response, its result, or the tree object itself."""
    node = _unwrap(source)
    if isinstance(node, Mapping) and "tree" in node:
        node = node["tree"]
    assert isinstance(node, Mapping) and "workspaces" in node, f"not a tree: {node!r}"
    return node


def workspaces(source: Any) -> list[Mapping[str, Any]]:
    return list(_tree(source)["workspaces"])


def sessions(source: Any, workspace: str | None = None) -> list[Mapping[str, Any]]:
    """All sessions in tree order, or only those of the workspace with id `workspace`."""
    nodes = workspaces(source)
    if workspace is not None:
        nodes = [node for node in nodes if node["id"] == workspace]
        assert nodes, f"no workspace {workspace!r} in tree"
    return [session for node in nodes for session in node["sessions"]]


def workspace_ids(source: Any) -> list[str]:
    return [node["id"] for node in workspaces(source)]


def session_ids(source: Any, workspace: str | None = None) -> list[str]:
    return [node["id"] for node in sessions(source, workspace)]


def find_session(source: Any, session_id: str) -> Mapping[str, Any]:
    found = [node for node in sessions(source) if node["id"] == session_id]
    assert len(found) == 1, f"expected one session {session_id!r}, found {len(found)}"
    return found[0]


def active_workspace_id(source: Any) -> str:
    return _single_active(workspaces(source), "workspace")


def active_session_id(source: Any) -> str:
    """The window's selected session; `active` is selection, so exactly one per tree."""
    return _single_active(sessions(source), "session")


def window_ids(source: Any) -> list[str]:
    """Windows are not in `tree`: this reads a `window.list` Response or result."""
    node = _unwrap(source)
    assert isinstance(node, Mapping) and "windows" in node, f"not a window list: {node!r}"
    return [window["id"] for window in node["windows"]]


def _single_active(nodes: list[Mapping[str, Any]], kind: str) -> str:
    active = [node["id"] for node in nodes if node.get("active") is True]
    assert len(active) == 1, f"expected one active {kind}, found {active}"
    return active[0]
