#!/usr/bin/env python3
"""Map catalog items to the conformance tests that cover them and to the ratchet.

    coverage-map.py --since REV                  # items the catalog or ui-ids gained, lost or changed since REV
    coverage-map.py KEY [KEY ...]                # covers keys: `session.hud`, `field:tree.linkOpenMode`, `cli:x y`
    coverage-map.py --grep REGEX                 # every covers key matching REGEX (e.g. '^session\\.hud|hud')

Run it from the ngterm checkout with the suite's venv (`.venv/bin/python`, which has PyYAML). For each key it
prints the test functions whose `@pytest.mark.covers(...)` names it (read with `ast`, nothing is imported or run),
how many of their node ids `conformance/rust-passing.txt` lists, and whether `exemptions` excuses the key. A key
with no test and no exemption is what the coverage gate will fail on.
"""

from __future__ import annotations

import argparse
import ast
import re
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

import yaml

ROOT = Path(subprocess.run(["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True,
                           check=True).stdout.strip())
CONFORMANCE = ROOT / "conformance"
PREFIX = {"commands": "", "fields": "field:", "events": "event:", "cli": "cli:", "behaviors": "behavior:"}


def catalog_keys(catalog: dict, ui_ids: dict) -> dict[str, dict]:
    keys: dict[str, dict] = {}
    for section, prefix in PREFIX.items():
        for name, entry in (catalog.get(section) or {}).items():
            keys[f"{prefix}{name}"] = entry if isinstance(entry, dict) else {"value": entry}
    for section in ("ids", "manual"):
        for name, entry in (ui_ids.get(section) or {}).items():
            keys[f"ui:{name}"] = entry if isinstance(entry, dict) else {"value": entry}
    return keys


def load_at(rev: str | None) -> tuple[dict, dict]:
    def read(name: str) -> dict:
        if rev is None:
            return yaml.safe_load((CONFORMANCE / name).read_text())
        text = subprocess.run(["git", "show", f"{rev}:conformance/{name}"], capture_output=True, text=True,
                              check=True).stdout
        return yaml.safe_load(text)

    return read("catalog.yaml"), read("ui-ids.yaml")


def covers_index() -> dict[str, list[str]]:
    """covers key -> ['tests/protocol/test_x.py::test_name', ...]"""
    index: dict[str, list[str]] = defaultdict(list)
    for path in sorted((CONFORMANCE / "tests").rglob("test_*.py")):
        tree = ast.parse(path.read_text(), filename=str(path))
        rel = path.relative_to(CONFORMANCE).as_posix()
        for node in ast.walk(tree):
            if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) or not node.name.startswith("test"):
                continue
            for decorator in node.decorator_list:
                if isinstance(decorator, ast.Call) and ast.unparse(decorator.func).endswith("mark.covers"):
                    for arg in decorator.args:
                        if isinstance(arg, ast.Constant) and isinstance(arg.value, str):
                            index[arg.value].append(f"{rel}::{node.name}")
    return index


def ratchet_ids() -> list[str]:
    lines = (CONFORMANCE / "rust-passing.txt").read_text().splitlines()
    return [line for line in lines if line and not line.startswith("#")]


def changed_keys(rev: str) -> list[tuple[str, str]]:
    before = catalog_keys(*load_at(rev))
    after = catalog_keys(*load_at(None))
    out = []
    for key in sorted(set(before) | set(after)):
        old, new = before.get(key), after.get(key)
        if old is None:
            out.append((key, "added"))
        elif new is None:
            out.append((key, "dropped from the file"))
        elif new.get("status") == "removed-upstream" and old.get("status") != "removed-upstream":
            out.append((key, "removed-upstream"))
        elif old.get("status") == "removed-upstream" and new.get("status") != "removed-upstream":
            # gen_catalog does not count a key that comes back as `+new`, though upstream added it again
            out.append((key, "re-added"))
        elif {k: v for k, v in old.items() if k not in ("status", "at")} != {
            k: v for k, v in new.items() if k not in ("status", "at")
        }:
            out.append((key, "changed"))
    return out


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--since", metavar="REV", help="diff the catalog and ui-ids against this git revision")
    group.add_argument("--grep", metavar="REGEX", help="every covers key or catalog key matching REGEX")
    group.add_argument("keys", nargs="*", default=[])
    args = parser.parse_args()

    index = covers_index()
    listed = ratchet_ids()
    catalog, ui_ids = load_at(None)
    exemptions = catalog.get("exemptions") or {}
    if args.since:
        rows = changed_keys(args.since)
    elif args.grep:
        rx = re.compile(args.grep)
        rows = [(key, "") for key in sorted(set(index) | set(catalog_keys(catalog, ui_ids))) if rx.search(key)]
    else:
        rows = [(key, "") for key in args.keys]
    if not rows:
        print("no catalog items changed" if args.since else "no matching keys")
        return 0
    for key, change in rows:
        tests = sorted(set(index.get(key, [])))
        head = f"## `{key}`" + (f" ({change})" if change else "")
        print(head)
        if key in exemptions:
            print(f"- exempted: {exemptions[key]}")
        if not tests:
            print("- no covering test" + ("" if key in exemptions else " -> the coverage gate fails on it"))
        for test in tests:
            ids = [i for i in listed if i == test or i.startswith(test + "[")]
            print(f"- `{test}`: {len(ids)} id(s) in rust-passing.txt")
        print()
    return 0


if __name__ == "__main__":
    sys.exit(main())
