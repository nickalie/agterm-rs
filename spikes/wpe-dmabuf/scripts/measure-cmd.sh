#!/usr/bin/env bash
# Measures any command's process tree: measure-cmd.sh WARMUP SECONDS OUT.json CMD...
# (used for the bare WebKitGTK baseline, scripts/webkitgtk-check.py). The command's stdout goes to OUT.log.
set -uo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
warmup="$1"; seconds="$2"; out="$3"; shift 3
while IFS= read -r name; do unset "$name"; done < <(compgen -e | grep -E '^(AGTERM_|ZMX_)' || true)
systemd-run --user --slice=agterm-agents.slice --scope --quiet -- "$@" >"${out%.json}.log" 2>&1 &
pid=$!
sleep "$warmup"
python3 -I "$here/scripts/measure.py" "$pid" "$seconds" "$out"
wait "$pid"
