#!/usr/bin/env bash
# Report the complexity debt and refuse inner lint exceptions, over every .rs file under crates/.
#
# - Lists each outer #[expect(...)] that lifts a complexity lint (cognitive_complexity, too_many_lines,
#   excessive_nesting, too_many_arguments, type_complexity, fn_params_excessive_bools, struct_excessive_bools)
#   with its file, line, item, lint and reason, then the count. Markdown, so CI can append it to the job summary.
# - Fails (exit 1) on any inner #![allow(...)] or #![expect(...)] outside crates/agterm-ghostty-sys, the bindgen
#   crate that sets its own lints: an exception must be an outer #[expect] on the one item, with a reason.
#
# Usage: scripts/lint-report.sh [ROOT]   (default: the repository root)
set -euo pipefail

root="${1:-$(cd "$(dirname "$0")/.." && pwd)}"
[[ -d "$root/crates" ]] || { echo "no crates/ under $root" >&2; exit 2; }

exec python3 - "$root" <<'PY'
import re
import sys
from pathlib import Path

COMPLEXITY = {
    "cognitive_complexity",
    "too_many_lines",
    "excessive_nesting",
    "too_many_arguments",
    "type_complexity",
    "fn_params_excessive_bools",
    "struct_excessive_bools",
}
EXEMPT_CRATE = "agterm-ghostty-sys"
INNER = re.compile(r"#!\[\s*(allow|expect)\b")
OUTER = re.compile(r"#\[\s*expect\s*\(")
LINT = re.compile(r"clippy::(\w+)")
REASON = re.compile(r'reason\s*=\s*"((?:[^"\\]|\\.)*)"', re.S)
ITEM = re.compile(r"\b(fn|struct|enum|union|trait|impl|mod|type|const|static)\b\s*(<[^>]*>\s*)?([A-Za-z_][\w:<>, ]*)?")


def balanced(text: str, start: int) -> int:
    """Index just past the parenthesis closing the one opening at `start`, skipping string literals."""
    depth, index, in_string = 0, start, False
    while index < len(text):
        char = text[index]
        if in_string:
            if char == "\\":
                index += 1
            elif char == '"':
                in_string = False
        elif char == '"':
            in_string = True
        elif char == "(":
            depth += 1
        elif char == ")":
            depth -= 1
            if depth == 0:
                return index + 1
        index += 1
    return len(text)


def item_after(text: str, end: int) -> str:
    """The item an attribute ending at `end` applies to: the next line that is not an attribute or a comment."""
    for line in text[end:].splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith(("#[", "//", "]", ")")):
            continue
        match = ITEM.search(stripped)
        if match:
            return f"{match.group(1)} {(match.group(3) or '').strip()}".strip()
        return stripped[:60]
    return "?"


def code_only(line: str) -> bool:
    return not line.lstrip().startswith("//")


root = Path(sys.argv[1])
expects: list[tuple[str, int, str, str, str]] = []
inner: list[tuple[str, int, str]] = []
for path in sorted((root / "crates").rglob("*.rs")):
    relative = path.relative_to(root)
    if "target" in relative.parts:
        continue
    text = path.read_text(encoding="utf-8")
    exempt = relative.parts[1] == EXEMPT_CRATE
    for number, line in enumerate(text.splitlines(), 1):
        if not exempt and code_only(line) and INNER.search(line):
            inner.append((str(relative), number, line.strip()))
    for match in OUTER.finditer(text):
        line_start = text.rfind("\n", 0, match.start()) + 1
        if not code_only(text[line_start : match.start() + 1]):
            continue
        end = balanced(text, match.end() - 1)
        body = text[match.end() : end - 1]
        lints = [lint for lint in LINT.findall(body) if lint in COMPLEXITY]
        if not lints:
            continue
        found = REASON.search(body)
        reason = found.group(1).replace('\\"', '"') if found else "(no reason)"
        number = text.count("\n", 0, match.start()) + 1
        item = item_after(text, end)
        expects += [(str(relative), number, item, lint, reason) for lint in lints]

print(f"### complexity expects: {len(expects)}")
print()
for file, number, item, lint, reason in expects:
    print(f"- `{file}:{number}` `{item}` `clippy::{lint}`: {reason}")
if inner:
    print()
    print(f"### refused inner lint attributes: {len(inner)}")
    print()
    for file, number, line in inner:
        print(f"- `{file}:{number}` `{line}`")
    sys.exit(1)
PY
