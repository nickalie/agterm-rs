"""The coverage gate: `covers(...)` declarations against catalog.yaml and ui-ids.yaml, with nothing launched.

Markers are the only source of coverage. A catalog item's `status` matters only as `removed-upstream`, which
retires the item; `covered`/`uncovered` is informational and never checked. The optional baseline lists the
uncovered items a transitional CI run accepts: it may only shrink, so an entry that is covered, exempted or
gone fails the gate until it is deleted (plan Task 9).
"""

from __future__ import annotations

from collections import Counter
from collections.abc import Iterable, Mapping
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import yaml

# Section -> covers prefix; a command is named bare.
SECTIONS = {
    "commands": "",
    "fields": "field:",
    "events": "event:",
    "cli": "cli:",
    "behaviors": "behavior:",
    "ui": "ui:",
}
CATALOG_SECTIONS = ("commands", "fields", "events", "cli", "behaviors")
UI_SECTIONS = ("ids", "manual")
REMOVED = "removed-upstream"


class CatalogError(Exception):
    pass


@dataclass(frozen=True)
class Item:
    section: str
    removed: bool
    mode: str | None = None


@dataclass(frozen=True)
class Catalog:
    items: dict[str, Item]
    exemptions: dict[str, Any]


@dataclass(frozen=True)
class Declaration:
    nodeid: str
    targets: tuple[str, ...]
    streams: tuple[str, ...] = ()


@dataclass
class SectionStats:
    active: int = 0
    covered: int = 0
    exempted: int = 0
    baselined: int = 0
    uncovered: int = 0

    def line(self, name: str) -> str:
        pct = 100.0 * self.covered / self.active if self.active else 100.0
        return (
            f"{name}: {self.covered}/{self.active} covered ({pct:.1f}%), {self.exempted} exempted, "
            f"{self.baselined} baselined, {self.uncovered} uncovered"
        )


@dataclass
class Report:
    sections: dict[str, SectionStats] = field(default_factory=lambda: {name: SectionStats() for name in SECTIONS})
    uncovered: list[str] = field(default_factory=list)
    baselined: list[str] = field(default_factory=list)
    stream_only_unqualified: dict[str, list[str]] = field(default_factory=dict)
    errors: list[str] = field(default_factory=list)

    @property
    def ok(self) -> bool:
        return not self.uncovered and not self.errors

    def summary_lines(self) -> list[str]:
        return [stats.line(name) for name, stats in self.sections.items()]

    def failure_text(self) -> str:
        parts: list[str] = []
        if self.uncovered:
            parts.append(f"{len(self.uncovered)} uncovered (cover, exempt with a reason, or baseline):")
            parts.extend(f"  {key}" for key in self.uncovered)
        for key, nodeids in sorted(self.stream_only_unqualified.items()):
            if key in self.uncovered:
                parts.append(f"{key} is named only without streams=({key!r},): {', '.join(nodeids)}")
        if self.errors:
            parts.append(f"{len(self.errors)} declaration/catalog errors:")
            parts.extend(f"  {error}" for error in self.errors)
        return "\n".join(parts)


def _read_yaml(path: Path) -> Any:
    try:
        return yaml.safe_load(path.read_text(encoding="utf-8"))
    except (OSError, yaml.YAMLError) as error:
        raise CatalogError(f"{path}: {error}") from None


def _section(doc: Mapping[str, Any], name: str, path: Path) -> Mapping[str, Any]:
    section = doc.get(name)
    if section is None:
        return {}
    if not isinstance(section, Mapping):
        raise CatalogError(f"{path}: `{name}` is not a mapping")
    return section


def load_catalog(catalog_path: Path, ui_path: Path) -> Catalog:
    items: dict[str, Item] = {}
    problems: list[str] = []

    def add(section: str, path: Path, name: str, entries: Mapping[str, Any]) -> None:
        for key, entry in entries.items():
            if not isinstance(entry, Mapping) or not isinstance(entry.get("status"), str):
                problems.append(f"{path}: {name}.{key} needs a status")
                continue
            covers_key = SECTIONS[section] + str(key)
            if covers_key in items:
                problems.append(f"{path}: {covers_key} is listed twice")
                continue
            mode = entry.get("mode")
            items[covers_key] = Item(section, entry["status"] == REMOVED, mode if isinstance(mode, str) else None)

    catalog = _read_yaml(catalog_path)
    if not isinstance(catalog, Mapping):
        raise CatalogError(f"{catalog_path}: not a mapping")
    for name in CATALOG_SECTIONS:
        add(name, catalog_path, name, _section(catalog, name, catalog_path))
    ui = _read_yaml(ui_path)
    if not isinstance(ui, Mapping):
        raise CatalogError(f"{ui_path}: not a mapping")
    for name in UI_SECTIONS:
        add("ui", ui_path, name, _section(ui, name, ui_path))
    if problems:
        raise CatalogError("\n".join(problems))
    return Catalog(items, dict(_section(catalog, "exemptions", catalog_path)))


def load_baseline(path: Path) -> list[str]:
    """One covers key per line; `#` starts a comment."""
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as error:
        raise CatalogError(f"coverage baseline {path}: {error}") from None
    keys = (line.split("#", 1)[0].strip() for line in text.splitlines())
    return [key for key in keys if key]


def evaluate(catalog: Catalog, declarations: Iterable[Declaration], baseline: list[str] | None = None) -> Report:
    report = Report()
    covered: set[str] = set()
    unqualified: dict[str, list[str]] = {}
    for declaration in declarations:
        for target in declaration.targets:
            item = catalog.items.get(target)
            if item is None:
                report.errors.append(f"{declaration.nodeid}: covers unknown item {target!r}")
            elif item.removed:
                report.errors.append(f"{declaration.nodeid}: covers {target!r}, which is {REMOVED}")
            elif item.mode is not None and target not in declaration.streams:
                unqualified.setdefault(target, []).append(declaration.nodeid)
            else:
                covered.add(target)
        for target in declaration.streams:
            item = catalog.items.get(target)
            if item is not None and not item.removed and item.mode is None:
                report.errors.append(f"{declaration.nodeid}: streams names {target!r}, which has no stream mode")

    exempted: set[str] = set()
    for key, reason in catalog.exemptions.items():
        item = catalog.items.get(key)
        if not isinstance(reason, str) or not reason.strip():
            report.errors.append(f"exemption {key!r} needs a non-empty reason")
        elif item is None:
            report.errors.append(f"exemption {key!r} names no catalog item")
        elif item.removed:
            report.errors.append(f"exemption {key!r} names an item that is {REMOVED}")
        else:
            exempted.add(key)

    accepted: set[str] = set()
    if baseline is not None:
        for key, count in Counter(baseline).items():
            item = catalog.items.get(key)
            if count > 1:
                report.errors.append(f"baseline lists {key!r} {count} times")
            if item is None:
                report.errors.append(f"baseline entry {key!r} names no catalog item")
            elif item.removed:
                report.errors.append(f"baseline entry {key!r} is {REMOVED}: delete it")
            elif key in covered:
                report.errors.append(f"baseline entry {key!r} is covered: delete it")
            elif key in exempted:
                report.errors.append(f"baseline entry {key!r} is exempted: delete it")
            else:
                accepted.add(key)

    for key, item in catalog.items.items():
        if item.removed:
            continue
        stats = report.sections[item.section]
        stats.active += 1
        if key in covered:
            stats.covered += 1
        elif key in exempted:
            stats.exempted += 1
        elif key in accepted:
            stats.baselined += 1
            report.baselined.append(key)
        else:
            stats.uncovered += 1
            report.uncovered.append(key)
    report.stream_only_unqualified = {key: ids for key, ids in unqualified.items() if key not in covered}
    return report
