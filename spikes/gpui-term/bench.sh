#!/usr/bin/env bash
# Repeat the throughput measurement: N runs each of the spike through zmx, the spike on a plain shell and the GTK
# build's stage (default: the main checkout's conformance stage). Prints one JSON line per run.
set -uo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
runs="${1:-3}"
stage="${2:-$(cd "$here/../.." && git rev-parse --path-format=absolute --git-common-dir | xargs dirname)/target/conformance-stage}"
for i in $(seq 1 "$runs"); do
  for check in throughput throughput-plain; do
    "$here/run-check.sh" --only "$check" 2>/dev/null | python3 -c 'import json,sys; r=json.load(sys.stdin); c=next(iter(r["checks"].items())); print(json.dumps({"run": c[0], **c[1]}))'
  done
  "$here/run-check.sh" --gtk-baseline "$stage" 2>/dev/null | python3 -c 'import json,sys; print(json.dumps({"run": "gtk", **json.load(sys.stdin)}))'
done
