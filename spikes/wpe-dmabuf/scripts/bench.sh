#!/usr/bin/env bash
# One measured spike run: bench.sh NAME SECONDS [SPIKE ARGS...]
# Starts the spike (scripts/run.sh) with --duration SECONDS+4 --out results/raw/NAME.json, samples its process tree
# (scripts/measure.py) for SECONDS after a 5 s warm-up into results/raw/NAME.measure.json, and waits for it to exit.
set -uo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
name="$1"; seconds="$2"; shift 2
raw="$here/results/raw"
mkdir -p "$raw"
"$here/scripts/run.sh" "$@" --duration "$((seconds + 4))" --out "$raw/$name.json" >"$raw/$name.log" 2>&1 &
pid=$!
sleep 5
python3 -I "$here/scripts/measure.py" "$pid" "$seconds" "$raw/$name.measure.json" >/dev/null
wait "$pid"
echo "exit $?"
python3 -I "$here/scripts/summary.py" "$raw/$name.json"
python3 -I -c 'import json,sys; d=json.load(open(sys.argv[1])); print("  measure:", json.dumps(d))' "$raw/$name.measure.json"
