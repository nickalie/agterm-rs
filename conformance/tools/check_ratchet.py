#!/usr/bin/env python3
"""Check a conformance run against the ratchet: every id in rust-passing.txt must still pass.

Reads the JSON lines `pytest --nodeid-report PATH` writes and the ratchet list (one pytest node id per line,
verbatim and sorted, `#` header comments first). A listed id that failed, errored, skipped, xfailed or did not
run breaks the ratchet, except a skip for a host-dependent capability the host lacks (counted as not run); a
collection error or a `--must-pass` problem breaks the run. Prints the breaks, the ids that pass but are not
listed yet, and a verdict; `--add` merges those ids into the list when the ratchet holds, and never removes one.
The report's run header names its mode: `full` and `ratchet-only` (`--ratchet-only`: the listed ids plus a task's
extras ran) judge every listed id and warn about one not collected; `partial` (a narrower selection) judges only
the listed ids that ran, so it can neither break nor shrink the list through what it left out. Appends a markdown
summary to `$GITHUB_STEP_SUMMARY` when it is set. Exit 0: holds, 1: broken, 2: usage error or a bad input file.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

DEFAULT_LIST = Path(__file__).resolve().parents[1] / "rust-passing.txt"
OUTCOMES = frozenset({"passed", "failed", "error", "skipped", "xfailed", "xpassed", "not-run"})
PASSING = frozenset({"passed", "xpassed"})
SKIP_CATEGORIES = frozenset({"unsupported", "not-yet-at-level", "other"})
# The run header's `mode` (agterm_conformance.selection.MODES): full, ratchet-only (`--ratchet-only`) or partial.
MODES = frozenset({"full", "ratchet-only", "partial"})
# Capabilities a build has or lacks depending on the host it runs on, not on the build.
HOST_DEPENDENT = frozenset({"live-sessions", "live-any-shell"})
_CAPABILITY = re.compile(r"unsupported: \S+ lacks ([A-Za-z0-9-]+)")


class InputError(Exception):
    """A missing or malformed report or list; the message names the file and, where it applies, the line."""


@dataclass(frozen=True)
class TestRecord:
    nodeid: str
    outcome: str
    skip: str | None
    reason: str | None

    def describe(self) -> str:
        """The outcome, with the reason for a skip; the id goes last in a printed line since it may end in spaces."""
        return f"{self.outcome} ({self.reason})" if self.outcome == "skipped" and self.reason else self.outcome


@dataclass
class Report:
    partial: bool
    # full, ratchet-only (the list's ids and a task's extras ran: every listed id is judged) or partial
    mode: str = "full"
    tests: dict[str, TestRecord] = field(default_factory=dict)
    collection_errors: list[tuple[str, str]] = field(default_factory=list)
    must_pass: list[str] = field(default_factory=list)


@dataclass
class RatchetList:
    header: list[str]
    ids: list[str]


@dataclass
class Verdict:
    partial: bool
    listed: int
    mode: str = "full"
    breaks: list[str] = field(default_factory=list)
    not_run: list[str] = field(default_factory=list)
    not_collected: list[str] = field(default_factory=list)
    newly_passing: list[str] = field(default_factory=list)

    @property
    def holds(self) -> bool:
        return not self.breaks

    def line(self) -> str:
        if not self.holds:
            return f"ratchet broken: {len(self.breaks)} break(s) over {self.listed} listed id(s)"
        notes = [f"{len(self.not_run)} not run on this host"] if self.not_run else []
        if self.not_collected:
            notes.append(f"{len(self.not_collected)} not collected")
        suffix = f" ({', '.join(notes)})" if notes else ""
        return f"ratchet holds: {self.listed} listed id(s){suffix}, {self.mode} run"


def _field(record: dict[str, Any], key: str, types: tuple[type, ...], where: str) -> Any:
    value = record.get(key)
    if not isinstance(value, types):
        raise InputError(f"{where}: {key!r} is missing or has the wrong type")
    return value


def read_report(path: Path) -> Report:
    try:
        text = path.read_bytes().decode("utf-8")
    except (OSError, UnicodeDecodeError) as error:
        raise InputError(f"{path}: cannot read the report: {error}") from error
    lines = text.split("\n")
    if lines and lines[-1] == "":
        lines.pop()
    report: Report | None = None
    for number, line in enumerate(lines, start=1):
        where = f"{path}:{number}"
        try:
            record = json.loads(line)
        except json.JSONDecodeError as error:
            raise InputError(f"{where}: not JSON: {error}") from error
        if not isinstance(record, dict):
            raise InputError(f"{where}: not a JSON object")
        kind = record.get("kind")
        if kind == "run":
            if report is not None:
                raise InputError(f"{where}: a second run header")
            partial = _field(record, "partial", (bool,), where)
            # a report from before the mode field: its partial flag says it all
            mode = record.get("mode", "partial" if partial else "full")
            if mode not in MODES or (mode == "full") == partial:
                raise InputError(f"{where}: run mode {mode!r} does not fit partial={partial}")
            report = Report(partial=partial, mode=mode)
            continue
        if report is None:
            raise InputError(f"{where}: a {kind!r} record before the run header")
        if kind == "test":
            test = TestRecord(
                nodeid=_field(record, "nodeid", (str,), where),
                outcome=_field(record, "outcome", (str,), where),
                skip=_field(record, "skip", (str, type(None)), where),
                reason=_field(record, "reason", (str, type(None)), where),
            )
            if test.outcome not in OUTCOMES:
                raise InputError(f"{where}: unknown outcome {test.outcome!r}")
            if test.skip is not None and test.skip not in SKIP_CATEGORIES:
                raise InputError(f"{where}: unknown skip category {test.skip!r}")
            if test.nodeid in report.tests:
                raise InputError(f"{where}: a second test record for {test.nodeid}")
            report.tests[test.nodeid] = test
        elif kind == "collection-error":
            report.collection_errors.append(
                (_field(record, "nodeid", (str,), where), _field(record, "message", (str,), where))
            )
        elif kind == "must-pass":
            report.must_pass.append(_field(record, "problem", (str,), where))
        else:
            raise InputError(f"{where}: unknown kind {kind!r}")
    if report is None:
        raise InputError(f"{path}: no run header")
    return report


def parse_list(text: str, path: Path) -> RatchetList:
    """Ids verbatim: never stripped, since a parametrized id may end in spaces."""
    lines = text.split("\n")
    if lines[-1] == "":
        lines.pop()  # the file's final newline
    header: list[str] = []
    ids: list[str] = []
    seen: set[str] = set()
    for number, line in enumerate(lines, start=1):
        where = f"{path}:{number}"
        if "\r" in line:
            raise InputError(f"{where}: a carriage return (the list uses \\n line ends)")
        if line == "":
            raise InputError(f"{where}: an empty line")
        if line.startswith("#"):
            if ids:
                raise InputError(f"{where}: a comment after the first id (comments go in the header)")
            header.append(line)
            continue
        if "::" not in line:
            raise InputError(f"{where}: not a pytest node id (no '::'): {line!r}")
        if line in seen:
            raise InputError(f"{where}: a duplicate id: {line}")
        if ids and line < ids[-1]:
            raise InputError(f"{where}: out of order (the ids are sorted): {line}")
        seen.add(line)
        ids.append(line)
    return RatchetList(header, ids)


def read_list(path: Path) -> RatchetList:
    try:
        text = path.read_bytes().decode("utf-8")
    except (OSError, UnicodeDecodeError) as error:
        raise InputError(f"{path}: cannot read the ratchet list: {error}") from error
    return parse_list(text, path)


def render_list(ratchet: RatchetList) -> str:
    return "".join(f"{line}\n" for line in [*ratchet.header, *sorted(ratchet.ids)])


def host_dependent_skip(test: TestRecord) -> bool:
    if test.outcome != "skipped" or test.skip != "unsupported" or test.reason is None:
        return False
    match = _CAPABILITY.match(test.reason)
    return match is not None and match.group(1) in HOST_DEPENDENT


def judge(report: Report, ratchet: RatchetList) -> Verdict:
    verdict = Verdict(partial=report.partial, listed=len(ratchet.ids), mode=report.mode)
    for nodeid in ratchet.ids:
        test = report.tests.get(nodeid)
        if test is None:
            # A ratchet-only run selected every listed id, so one it lacks is gone, as in a full run. A narrower
            # selection leaves the unselected ids unjudged: such a run neither breaks nor shrinks the list.
            if report.mode != "partial":
                verdict.not_collected.append(nodeid)
        elif host_dependent_skip(test):
            verdict.not_run.append(f"{test.describe()}: {nodeid}")
        elif test.outcome not in PASSING:
            verdict.breaks.append(f"{test.describe()}: {nodeid}")
    for nodeid, message in report.collection_errors:
        last = next((line for line in reversed(message.splitlines()) if line.strip()), "")
        verdict.breaks.append(f"collection error: {nodeid}: {last.strip()}")
    verdict.breaks.extend(f"must-pass: {problem}" for problem in report.must_pass)
    listed = set(ratchet.ids)
    verdict.newly_passing = sorted(
        nodeid for nodeid, test in report.tests.items() if test.outcome in PASSING and nodeid not in listed
    )
    return verdict


def print_verdict(verdict: Verdict) -> None:
    for line in verdict.breaks:
        print(f"BROKEN {line}")
    for line in verdict.not_run:
        print(f"not run on this host: {line}")
    for nodeid in verdict.not_collected:
        print(f"warning: listed but not collected: {nodeid}")
    if verdict.newly_passing:
        print(f"newly passing, not in the list ({len(verdict.newly_passing)}):")
        for nodeid in verdict.newly_passing:
            print(nodeid)
    print(verdict.line())


def summary_markdown(verdict: Verdict) -> str:
    parts = ["### Conformance ratchet", "", f"**{verdict.line()}**", ""]
    for title, lines in (
        ("Breaks", verdict.breaks),
        ("Not run on this host", verdict.not_run),
        ("Listed but not collected", verdict.not_collected),
        (f"Newly passing ({len(verdict.newly_passing)}), not in the list yet", verdict.newly_passing),
    ):
        if lines:
            parts += [f"{title}:", "", "```text", *lines, "```", ""]
    return "\n".join(parts) + "\n"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--report", type=Path, required=True, help="the file `pytest --nodeid-report` wrote")
    parser.add_argument("--list", type=Path, default=DEFAULT_LIST, help=f"the ratchet list (default: {DEFAULT_LIST})")
    parser.add_argument("--add", action="store_true", help="merge the newly passing ids into the list")
    args = parser.parse_args(argv)
    try:
        report = read_report(args.report)
        ratchet = read_list(args.list)
    except InputError as error:
        print(f"check_ratchet: {error}", file=sys.stderr)
        return 2
    verdict = judge(report, ratchet)
    print_verdict(verdict)
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as stream:
            stream.write(summary_markdown(verdict))
    if not verdict.holds:
        if args.add:
            print("check_ratchet: --add skipped: the ratchet is broken", file=sys.stderr)
        return 1
    if args.add and verdict.newly_passing:
        merged = RatchetList(ratchet.header, [*ratchet.ids, *verdict.newly_passing])
        text = render_list(merged)
        try:
            parse_list(text, args.list)  # an id the list format cannot hold must not corrupt the file
        except InputError as error:
            print(f"check_ratchet: --add refused: {error}", file=sys.stderr)
            return 2
        args.list.write_bytes(text.encode("utf-8"))
        print(f"added {len(verdict.newly_passing)} id(s) to {args.list}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
