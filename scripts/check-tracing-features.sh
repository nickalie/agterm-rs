#!/usr/bin/env bash
# Fail when any crate in the workspace graph turns on tracing's `log` or `log-always` feature.
#
# Cargo unifies features across a workspace build, and with `log` every tracing macro of every crate expands with a
# subscriber-free fallback that clippy's cognitive_complexity counts (about 10 points per call site), so one
# dependency asking for it puts dozens of unrelated functions over the limit. calloop asked (vendor/calloop drops it,
# scripts/vendor-calloop.sh). A new requester is a decision to make here, not a reason to split functions around
# logging. Run before clippy; it resolves the graph as the gates do (all features, every target).
#
# Usage: scripts/check-tracing-features.sh [CARGO_METADATA_ARGS...]   (it always resolves the whole workspace)
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
cargo metadata --format-version 1 --all-features "$@" | python3 -I -c '
import json
import sys

metadata = json.load(sys.stdin)
names = {package["id"]: (package["name"], package["version"]) for package in metadata["packages"]}
banned = {"log", "log-always"}
found = []
for node in metadata["resolve"]["nodes"]:
    name, version = names[node["id"]]
    enabled = banned & set(node.get("features", []))
    if name == "tracing" and enabled:
        found.append("tracing %s: %s" % (version, ", ".join(sorted(enabled))))
        # the manifests asking for it (a feature of another crate forwarding to tracing/log shows as that crate)
        requesters = sorted(
            "%s %s" % (package["name"], package["version"])
            for package in metadata["packages"]
            for dep in package["dependencies"]
            if dep["name"] == "tracing" and banned & set(dep.get("features", []))
        )
        found.append("  requested by: " + (", ".join(requesters) or "a feature forwarding to it"))
if found:
    print("tracing has its log fallback enabled in the workspace graph (see scripts/check-tracing-features.sh):", file=sys.stderr)
    print("\n".join(found), file=sys.stderr)
    sys.exit(1)
print("tracing: no log fallback in the workspace graph")
'
