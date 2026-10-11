"""Polling waits over the control socket: no test sleeps, every wait has a deadline and a diagnosis.

`events.read` never blocks and, without a cursor, subscribes from now with no history. A test therefore
takes `events_baseline()` before its action and hands that cursor to `wait_event`, so an event the action
emits before the wait starts is still in the window the wait scans.
"""

from __future__ import annotations

import json
import threading
import time
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from typing import Any, Protocol

from agterm_conformance.client import Response
from agterm_conformance.match import assert_subset

DEFAULT_TIMEOUT = 10.0
POLL_INTERVAL = 0.05
# A cursor page bound; `events.read` accepts 1...1000.
PAGE_LIMIT = 1000
_SHOWN_CHARS = 2000
_SHOWN_EVENTS = 5


class Requester(Protocol):
    def request(self, cmd: str, target: str | None = None, **args: Any) -> Response: ...


class WaitTimeout(AssertionError):
    pass


@dataclass(frozen=True)
class Cursor:
    """An `events.read` position: `after` is the last sequence already seen in app run `run`."""

    run: str
    after: int

    def past(self, event: Mapping[str, Any]) -> Cursor:
        """The cursor just after `event`, to wait for a later one."""
        return Cursor(self.run, int(event["seq"]))


def _pause(seconds: float) -> None:
    threading.Event().wait(seconds)


def wait_tree(
    client: Requester, predicate: Callable[[dict[str, Any]], Any], timeout: float = DEFAULT_TIMEOUT
) -> dict[str, Any]:
    """Poll `tree` until `predicate(result)` is truthy and return that result.

    A predicate may also assert: an AssertionError means "not yet", and the last one is in the timeout message.
    """
    deadline = time.monotonic() + timeout
    last_result: dict[str, Any] | None = None
    last_failure = ""
    while True:
        response = client.request("tree")
        if not response.ok:
            raise AssertionError(f"tree failed while waiting: {response.error!r}")
        last_result = response.result or {}
        try:
            if predicate(last_result):
                return last_result
            last_failure = ""
        except AssertionError as error:
            last_failure = f"; last check: {error}"
        if time.monotonic() >= deadline:
            raise WaitTimeout(
                f"tree did not satisfy {_describe(predicate)} within {timeout}s{last_failure}; "
                f"last tree: {_shown(last_result)}"
            )
        _pause(POLL_INTERVAL)


def events_baseline(client: Requester) -> Cursor:
    """Anchor at the ring's tail before the action under test."""
    response = client.request("events.read")
    batch = _batch(response, "events baseline")
    return Cursor(run=batch["run"], after=int(batch["next"]))


def wait_event(
    client: Requester,
    cursor: Cursor,
    kind: str,
    match: Mapping[str, Any] | Callable[[dict[str, Any]], Any] | None = None,
    timeout: float = DEFAULT_TIMEOUT,
) -> dict[str, Any]:
    """The first `kind` event after `cursor` that `match` accepts: a subset of the event, or a predicate."""
    deadline = time.monotonic() + timeout
    after = cursor.after
    rejected: list[dict[str, Any]] = []
    while True:
        response = client.request("events.read", run=cursor.run, after=str(after), kinds=[kind], limit=PAGE_LIMIT)
        batch = _batch(response, f"waiting for {kind}")
        for event in batch["items"]:
            if _accepts(match, event):
                return event
            rejected.append(event)
        after = int(batch["next"])
        if time.monotonic() >= deadline:
            seen = (
                f"{len(rejected)} non-matching, last: {_shown(rejected[-_SHOWN_EVENTS:])}"
                if rejected
                else "none of that kind"
            )
            wanted = f" matching {_describe(match)}" if match is not None else ""
            raise WaitTimeout(f"no {kind} event{wanted} within {timeout}s after seq {cursor.after}; saw {seen}")
        _pause(POLL_INTERVAL)


def _batch(response: Response, context: str) -> dict[str, Any]:
    # A cursor failure (run changed, expired, ahead) cannot heal by polling: fail with the anchor.
    if not response.ok:
        raise AssertionError(f"events.read failed {context}: {response.error!r}, result {response.result}")
    batch = (response.result or {}).get("events")
    if not isinstance(batch, Mapping) or not {"run", "next", "items"} <= batch.keys():
        raise AssertionError(f"events.read returned no event batch {context}: {response.raw}")
    return dict(batch)


def _accepts(match: Mapping[str, Any] | Callable[[dict[str, Any]], Any] | None, event: dict[str, Any]) -> bool:
    if match is None:
        return True
    if callable(match):
        try:
            return bool(match(event))
        except AssertionError:
            return False
    try:
        assert_subset(event, match)
    except AssertionError:
        return False
    return True


def _describe(what: Any) -> str:
    if isinstance(what, Mapping):
        return _shown(what)
    return getattr(what, "__qualname__", None) or repr(what)


def _shown(value: Any) -> str:
    text = json.dumps(value, default=repr, sort_keys=True)
    return text if len(text) <= _SHOWN_CHARS else text[:_SHOWN_CHARS] + "..."
