"""`--ratchet-only`: run just the ids a ratchet list holds, plus the extra selections a task names.

A full run of the Rust build waits out every read-back a red test makes (a build short of parity has hundreds), so a
task's routine check runs only what its ratchet already holds, plus what it is working on (`--also` a path or node-id
prefix, `--also-k` a `-k` expression), and judges the ratchet exactly as a full run does. The nodeid report then says
`"mode": "ratchet-only"`: `check_ratchet.py` judges every listed id, warns about a listed id that was not collected,
and adds only ids that ran and passed. A full run stays the default and is required at milestones
(conformance/README.md > Rust runner and ratchet).
"""

from __future__ import annotations

from collections.abc import Iterable, Sequence
from pathlib import Path

import pytest
from _pytest.mark import KeywordMatcher
from _pytest.mark import expression as _expression
from _pytest.mark.expression import Expression

# what a bad -k expression raises: ParseError up to pytest 8, SyntaxError since
_PARSE_ERRORS: tuple[type[Exception], ...] = (SyntaxError, getattr(_expression, "ParseError", SyntaxError))

# The nodeid report's run modes: every collected test ran, the ratchet's ids (and the extras) ran, or a narrower
# selection (-k, -m, node ids) ran.
FULL, RATCHET_ONLY, PARTIAL = "full", "ratchet-only", "partial"
MODES = (FULL, RATCHET_ONLY, PARTIAL)


def listed_ids(path: Path) -> frozenset[str]:
    """The ids of a ratchet list, verbatim (an id may end in spaces); `#` header lines and empty lines left out."""
    try:
        text = path.read_bytes().decode("utf-8")
    except (OSError, UnicodeDecodeError) as error:
        raise pytest.UsageError(f"--ratchet-only: cannot read {path}: {error}") from error
    return frozenset(line for line in text.split("\n") if line and not line.startswith("#"))


def selector_matches(nodeid: str, selector: str) -> bool:
    """Whether `selector` (a directory, a file, or a node-id prefix, as pytest spells them) names `nodeid`."""
    selector = selector.removeprefix("conformance/").rstrip("/")
    return (
        nodeid == selector
        or nodeid.startswith((f"{selector}::", f"{selector}/"))
        or ("::" in selector and nodeid.startswith(f"{selector}["))
    )


def compile_keywords(expressions: Sequence[str]) -> list[Expression]:
    compiled = []
    for expression in expressions:
        try:
            compiled.append(Expression.compile(expression))
        except _PARSE_ERRORS as error:
            raise pytest.UsageError(f"--also-k: bad expression {expression!r}: {error}") from error
    return compiled


class RatchetOnly:
    """Keeps the listed ids and the `--also`/`--also-k` selections; deselects the rest."""

    def __init__(self, listed: Iterable[str], also: Sequence[str] = (), also_k: Sequence[str] = ()) -> None:
        self.listed = frozenset(listed)
        self.also = list(also)
        self.keywords = compile_keywords(also_k)

    def wanted(self, item: pytest.Item) -> bool:
        if item.nodeid in self.listed or any(selector_matches(item.nodeid, s) for s in self.also):
            return True
        if not self.keywords:
            return False
        matcher = KeywordMatcher.from_item(item)
        return any(expression.evaluate(matcher) for expression in self.keywords)

    # trylast: after -k/-m, so this narrows their selection; registered after the plugin module, so before its
    # trylast hook, which then sees only what runs.
    @pytest.hookimpl(trylast=True)
    def pytest_collection_modifyitems(self, config: pytest.Config, items: list[pytest.Item]) -> None:
        report = config.pluginmanager.get_plugin("agterm-nodeid-report")
        if report is not None and not report.partial and not any("::" in arg for arg in config.args):
            # nothing narrowed the collection before: every listed id that exists is about to run
            report.mode = RATCHET_ONLY
        wanted = {item.nodeid for item in items if self.wanted(item)}
        kept = [item for item in items if item.nodeid in wanted]
        dropped = [item for item in items if item.nodeid not in wanted]
        if dropped:
            config.hook.pytest_deselected(items=dropped)
        items[:] = kept
