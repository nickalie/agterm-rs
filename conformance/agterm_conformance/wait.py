"""Polling waits over the control socket: no test sleeps, every wait has a deadline and a diagnosis.

`events.read` never blocks and, without a cursor, subscribes from now with no history. A test therefore
takes `events_baseline()` before its action and hands that cursor to `wait_event`, so an event the action
emits before the wait starts is still in the window the wait scans.
"""

from __future__ import annotations

import json
import re
import threading
import time
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from typing import Any, Protocol

from agterm_conformance.client import Response
from agterm_conformance.match import assert_subset, find_session

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


# Per pane, the session field that is reported only once the pane's terminal surface is up. A pane's slot (its
# `surfaces` entry, `hasSplit`, `scratch`) precedes its mount, so a command acting on the pane, or a read of its
# `paneID`, made before this field appears can answer `session not realized` or find no `paneID`.
MOUNTED_FIELD = {"left": "realized", "right": "splitFontSize", "scratch": "scratchFontSize"}


def wait_mounted(client: Requester, session_id: str, *panes: str, timeout: float = DEFAULT_TIMEOUT) -> dict[str, Any]:
    """Poll `tree` until each of `panes` (left|right|scratch) of `session_id` has its surface up; return that tree."""
    if unknown := [pane for pane in panes if pane not in MOUNTED_FIELD]:
        raise ValueError(f"no mount field for pane {unknown[0]!r}: use one of {sorted(MOUNTED_FIELD)}")

    def mounted(tree: dict[str, Any]) -> bool:
        node = find_session(tree, session_id)
        pending = [pane for pane in panes if not node.get(MOUNTED_FIELD[pane])]
        assert not pending, f"not mounted: {pending}"
        return True

    return wait_tree(client, mounted, timeout)


def wait_response(
    client: Requester,
    cmd: str,
    target: str | None,
    accept: Callable[[Response], Any],
    timeout: float = DEFAULT_TIMEOUT,
    **args: Any,
) -> Response:
    """Repeat one request until `accept(response)` is truthy and return that response.

    For a reply that settles after the command that set it up, such as an overlay surface coming up. `accept` may
    also assert: an AssertionError means "not yet", and the last one is in the timeout message.
    """
    deadline = time.monotonic() + timeout
    last_failure = ""
    while True:
        response = client.request(cmd, target, **args)
        try:
            if accept(response):
                return response
            last_failure = ""
        except AssertionError as error:
            last_failure = f"; last check: {error}"
        if time.monotonic() >= deadline:
            raise WaitTimeout(
                f"{cmd} did not satisfy {_describe(accept)} within {timeout}s{last_failure}; "
                f"last reply: {_shown(response.raw)}"
            )
        _pause(POLL_INTERVAL)


def wait_text(
    client: Requester,
    target: str | None,
    pattern: str | re.Pattern[str],
    timeout: float = DEFAULT_TIMEOUT,
    **args: Any,
) -> re.Match[str]:
    """Poll `session.text` for `target` until `pattern` matches the pane text, and return the match.

    Fails fast on `ok:false`: a target that does not resolve cannot heal by polling.
    """
    regex = re.compile(pattern) if isinstance(pattern, str) else pattern
    deadline = time.monotonic() + timeout
    while True:
        response = client.request("session.text", target, **args)
        if not response.ok:
            raise AssertionError(f"session.text failed while waiting: {response.error!r}")
        text = (response.result or {}).get("text")
        if not isinstance(text, str):
            raise AssertionError(f"session.text returned no text: {response.raw}")
        if found := regex.search(text):
            return found
        if time.monotonic() >= deadline:
            raise WaitTimeout(
                f"session.text of {target} did not match {regex.pattern!r} within {timeout}s; "
                f"last text: {text[-_SHOWN_CHARS:]!r}"
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
        next_after = _checked_page(batch, cursor.run, after, kind)
        for event in batch["items"]:
            if _accepts(match, event):
                return event
            rejected.append(event)
        after = next_after
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


def _checked_page(batch: Mapping[str, Any], run: str, after: int, kind: str) -> int:
    """The validated `next` of a whole `events.read` page, checked before any event of it is used.

    The run, the kinds filter and the cursor bounds are behavior under test, not something to trust:
    a `next` behind `after` would re-read events from before the baseline.
    """
    asked = f"events.read run {run} after seq {after} kinds [{kind!r}]"
    if batch["run"] != run:
        raise AssertionError(f"{asked} returned run {_shown(batch['run'])}")
    next_seq = batch["next"]
    if isinstance(next_seq, bool) or not isinstance(next_seq, int) or next_seq < after:
        raise AssertionError(f"{asked} returned next {_shown(next_seq)}")
    for event in batch["items"]:
        if not isinstance(event, Mapping) or event.get("kind") != kind:
            raise AssertionError(f"{asked} returned an event of another kind: {_shown(event)}")
        seq = event.get("seq")
        if isinstance(seq, bool) or not isinstance(seq, int) or not after < seq <= next_seq:
            raise AssertionError(f"{asked} returned next {next_seq} with an event outside it: {_shown(event)}")
    return next_seq


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
