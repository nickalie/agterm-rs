#!/usr/bin/env python3
"""Sort an upstream agterm diff OLD..NEW into the areas a port touches.

    classify-diff.py --upstream DIR OLD_TAG NEW_TAG [--json]

Reads the clone with git only (no checkout needed). Prints a markdown report: the changelog sections of every
release after OLD up to NEW, each commit with the areas its files fall in (the unit a plan task grows from), then
each area with its files (+added -deleted), the command groups a file belongs to, the Swift test functions added or
changed, and string literals added to sources that look like user-visible messages (error strings to confirm
against the Swift tests; `(moved)` when the same literal was deleted elsewhere, i.e. code that only moved).
Unknown paths land in `review`, never in `no-port`: a path this table does not know is a question, not an answer.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from collections import defaultdict
from dataclasses import dataclass, field

CORE = "agtermCore/Sources/agtermCore/"
CLI = "agtermCore/Sources/agtermctlKit/"

# (area, regex over the path, group derivation). First match wins; order matters.
RULES: list[tuple[str, str]] = [
    ("dispatcher", rf"^{CORE}ControlDispatcher\+(?P<group>\w+)\.swift$"),
    ("protocol", rf"^{CORE}(ControlDispatcher|Control\w*)\.swift$"),
    ("app-control", r"^agterm/Control/ControlServer(\+(?P<group>\w+))?\.swift$"),
    ("app-control", r"^agterm/Control/"),
    ("cli", rf"^{CLI}(?P<group>\w+?)(Commands)?(\+\w+)?\.swift$"),
    ("cli", r"^agtermctl/"),
    ("core", rf"^{CORE}"),
    ("tests-core", r"^agtermCore/Tests/agtermCoreTests/"),
    ("tests-cli", r"^agtermCore/Tests/agtermctlKitTests/"),
    ("tests-app", r"^agtermTests/"),
    ("tests-ui", r"^agtermUITests/"),
    ("vendored", r"^agterm/Resources/(agent-status|hud)/"),
    ("vendored", r"^agterm/Resources/ghostty-defaults\.conf$"),
    ("vendored", r"^plugins/agterm/skills/agterm/"),
    ("native-pins", r"^scripts/zmx-patches/"),
    ("native-pins", r"^scripts/setup\.sh$"),
    ("ui", r"^agterm/.*\.swift$"),
    ("ui", r"^agterm/Resources/"),
    ("contract-docs", r"^site/(commands|docs)\.html$"),
    ("contract-docs", r"^\.claude/rules/"),
    ("contract-docs", r"^(docs/troubleshooting\.md|cookbook/)"),
    ("upstream-design", r"^docs/(plans|backlog)/"),
    ("no-port", r"^(\.github/|packaging/|\.claude-plugin/|plugins/agterm/\.(claude|codex)-plugin/)"),
    # upstream's own `agterm` theme: this build ships Ghostty's themes only
    ("no-port", r"^agterm/Resources/custom-themes/"),
    ("no-port", r"^(README\.md|CHANGELOG\.md|CONTRIBUTING\.md|ARCHITECTURE\.md|CLAUDE\.md|Makefile|project\.yml)$"),
    ("no-port", r"^site/(?!commands\.html$|docs\.html$)"),
    ("no-port", r"^scripts/(?!setup\.sh$|zmx-patches/)"),
    ("no-port", r"^agterm\.xcodeproj/|\.(png|icns|xcassets|storyboard|plist|entitlements)$"),
]
COMPILED = [(area, re.compile(rx)) for area, rx in RULES]

AREA_TITLES = {
    "protocol": "Wire protocol and dispatcher core (`ngterm-proto`, `ngterm-core` dispatch)",
    "dispatcher": "Dispatcher, by command group (`ngterm-core/src/commands/<group>`)",
    "app-control": "App side of the control server (`ngterm-gpui`)",
    "cli": "agtermctl (`ngterm-ctl`)",
    "core": "Core model, store and on-disk formats (`ngterm-core`)",
    "tests-core": "Swift tests: agtermCore (exact strings, edge cases, field order)",
    "tests-cli": "Swift tests: agtermctlKit (CLI output and errors)",
    "tests-app": "Swift tests: app (ControlServer, views, Ghostty glue)",
    "tests-ui": "Swift UI tests (accessibility ids; the suite's ui layer)",
    "ui": "UI (`ngterm-gpui`)",
    "contract-docs": "Contract docs: commands.html, docs.html, .claude/rules",
    "vendored": "Vendored upstream copies (synced by sync-resources.sh)",
    "native-pins": "Upstream's pins and zmx patches (scripts/setup.sh and zmx-patches, reported by pins-delta.sh)",
    "upstream-design": "Upstream plans and backlog (design intent, read for context, not ported)",
    "no-port": "No port needed (CI, packaging, release metadata, site pages)",
    "review": "Unclassified - review by hand",
}
AREA_ORDER = list(AREA_TITLES)

# Core files whose names suggest an on-disk format or a persisted setting: a hint to check the format, not a verdict.
ON_DISK_HINT = re.compile(
    r"(Settings|Persist|Snapshot|ConfigPaths|Keymap|Hooks|Restore|Windows|Remotes|Denylist|Theme|Store\.swift)"
)
TEST_FUNC = re.compile(r"^\+\s*(?:@Test[^\n]*?\s+)?(?:@MainActor\s+)?func\s+(\w+)\s*\(")
MESSAGE_WORDS = re.compile(
    r"(requires|invalid|unknown|cannot|can't|not found|no longer|refus|must|already|failed|missing|not a|expected)",
    re.IGNORECASE,
)
STRING_LITERAL = re.compile(r'"((?:[^"\\]|\\.){6,})"')


@dataclass
class FileChange:
    path: str
    added: int
    deleted: int
    area: str
    group: str | None
    status: str = "M"
    tests: list[str] = field(default_factory=list)
    messages: list[str] = field(default_factory=list)


def git(upstream: str, *args: str) -> str:
    return subprocess.run(["git", "-C", upstream, *args], check=True, capture_output=True, text=True).stdout


def classify(path: str) -> tuple[str, str | None]:
    for area, rx in COMPILED:
        match = rx.search(path)
        if match:
            group = match.groupdict().get("group")
            return area, (group.lower() if group else None)
    return "review", None


def changelog_sections(upstream: str, new: str, versions: list[str]) -> str:
    try:
        text = git(upstream, "show", f"{new}:CHANGELOG.md")
    except subprocess.CalledProcessError:
        return "(no CHANGELOG.md at the new tag)"
    wanted = {v.lstrip("v") for v in versions}
    out, keep = [], False
    for line in text.splitlines():
        heading = re.match(r"^## v?(\d+\.\d+\.\d+)", line)
        if heading:
            keep = heading.group(1) in wanted
        if keep:
            out.append(line)
    return "\n".join(out).strip() or "(no CHANGELOG section for these versions)"


def release_versions(upstream: str, old: str, new: str) -> list[str]:
    tags = git(upstream, "tag", "--merged", new, "--sort=v:refname").split()
    tags = [t for t in tags if re.fullmatch(r"v?\d+\.\d+\.\d+", t)]
    if old in tags and new in tags:
        return tags[tags.index(old) + 1 : tags.index(new) + 1]
    return [new]


def collect(upstream: str, old: str, new: str) -> list[FileChange]:
    statuses = {}
    for line in git(upstream, "diff", "--name-status", "-M", old, new).splitlines():
        parts = line.split("\t")
        statuses[parts[-1]] = parts[0][0]
    changes = []
    for line in git(upstream, "diff", "--numstat", "-M", old, new).splitlines():
        added, deleted, path = line.split("\t", 2)
        if " => " in path:  # rename: numstat prints {a => b} or a => b
            path = re.sub(r"\{[^{}]* => ([^{}]*)\}", r"\1", path).split(" => ")[-1].replace("//", "/")
        area, group = classify(path)
        changes.append(
            FileChange(path, int(added) if added != "-" else 0, int(deleted) if deleted != "-" else 0, area, group,
                       statuses.get(path, "M"))
        )
    source_areas = {"protocol", "dispatcher", "app-control", "cli", "core"}
    patches = {
        change.path: git(upstream, "diff", "-U0", old, new, "--", change.path)
        for change in changes
        if change.area.startswith("tests-") or change.area in source_areas
    }
    # a literal deleted anywhere in the sources was moved, not added (a split file, an extracted helper)
    removed = {
        literal
        for path, patch in patches.items()
        if classify(path)[0] in source_areas
        for line in patch.splitlines()
        if line.startswith("-") and not line.startswith("---")
        for literal in STRING_LITERAL.findall(line)
    }
    for change in changes:
        patch = patches.get(change.path)
        if patch is None:
            continue
        if change.area.startswith("tests-"):
            change.tests = sorted({m.group(1) for line in patch.splitlines() if (m := TEST_FUNC.match(line))})
            continue
        found = []
        for line in patch.splitlines():
            if not line.startswith("+") or line.startswith("+++"):
                continue
            for literal in STRING_LITERAL.findall(line):
                if MESSAGE_WORDS.search(literal):
                    entry = f"{literal}` (moved)" if literal in removed else f"{literal}`"
                    if entry not in found:
                        found.append(entry)
        change.messages = found
    return changes


def commits_by_area(upstream: str, old: str, new: str) -> list[str]:
    """One block per non-merge commit: its subject and the areas of its files, the unit a plan task grows from."""
    out = git(upstream, "log", "--no-merges", "--reverse", "--format=@@%h %s", "--name-only", f"{old}..{new}")
    lines, subject, files = [], None, []

    def flush() -> None:
        if subject is None:
            return
        areas: dict[str, list[str]] = defaultdict(list)
        for path in files:
            area, group = classify(path)
            areas[area].append(f"{group}" if group else path.rsplit("/", 1)[-1])
        summary = "; ".join(f"{area}: {', '.join(sorted(set(names)))}" for area, names in sorted(areas.items()))
        lines.append(f"- {subject}\n  {summary}")

    for line in out.splitlines():
        if line.startswith("@@"):
            flush()
            subject, files = line[2:], []
        elif line.strip():
            files.append(line.strip())
    flush()
    return lines


def moved_only(change: FileChange) -> bool:
    """A new file whose user-visible strings were all deleted elsewhere: most likely code split out of another file."""
    return change.status == "A" and bool(change.messages) and all(m.endswith("(moved)") for m in change.messages)


def report_markdown(
    old: str, new: str, versions: list[str], log: list[str], notes: str, changes: list[FileChange]
) -> str:
    by_area: dict[str, list[FileChange]] = defaultdict(list)
    for change in changes:
        by_area[change.area].append(change)
    lines = [f"# upstream {old}..{new}", "", f"Releases: {', '.join(versions)}", "", "## Changelog", "", notes, ""]
    lines += ["## Commits by area", "", *log, ""]
    grouped = [c for c in changes if c.group and c.area in {"dispatcher", "app-control", "cli"}]
    groups = sorted({c.group for c in grouped if not moved_only(c)})
    moved = sorted({c.group for c in grouped if moved_only(c)} - set(groups))
    lines += ["## Command groups touched", "", ", ".join(groups) or "(none)", ""]
    if moved:
        lines += [f"Only moved code (a split or an extracted file, check before planning): {', '.join(moved)}", ""]
    lines += ["## Areas", ""]
    for area in AREA_ORDER:
        items = by_area.get(area)
        if not items:
            continue
        total_add = sum(c.added for c in items)
        total_del = sum(c.deleted for c in items)
        lines += [f"### {area}: {AREA_TITLES[area]}", "", f"{len(items)} files, +{total_add} -{total_del}", ""]
        for change in sorted(items, key=lambda c: c.path):
            tags = []
            if change.status == "A":
                tags.append("new")
            elif change.status == "D":
                tags.append("deleted")
            if change.group:
                tags.append(f"group {change.group}")
            if moved_only(change):
                tags.append("moved code?")
            if change.area == "core" and ON_DISK_HINT.search(change.path):
                tags.append("on-disk?")
            suffix = f" ({', '.join(tags)})" if tags else ""
            lines.append(f"- `{change.path}` +{change.added} -{change.deleted}{suffix}")
            for test in change.tests:
                lines.append(f"  - test `{test}`")
            for message in change.messages[:12]:
                lines.append(f"  - string `{message}")
            if len(change.messages) > 12:
                lines.append(f"  - ... {len(change.messages) - 12} more strings")
        lines.append("")
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--upstream", required=True, help="the upstream clone holding both tags")
    parser.add_argument("old")
    parser.add_argument("new")
    parser.add_argument("--json", action="store_true", help="print the file list as JSON instead of markdown")
    args = parser.parse_args()
    try:
        changes = collect(args.upstream, args.old, args.new)
        versions = release_versions(args.upstream, args.old, args.new)
        log = commits_by_area(args.upstream, args.old, args.new)
        notes = changelog_sections(args.upstream, args.new, versions)
    except subprocess.CalledProcessError as error:
        print(f"classify-diff: git failed: {error.stderr.strip()}", file=sys.stderr)
        return 1
    if args.json:
        print(json.dumps([change.__dict__ for change in changes], indent=1))
    else:
        print(report_markdown(args.old, args.new, versions, log, notes, changes))
    return 0


if __name__ == "__main__":
    sys.exit(main())
