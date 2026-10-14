#!/usr/bin/env bash
# Baseline: the GTK build's WebKitGTK overlay showing URL, measured like the spike.
#   gtk-baseline.sh URL SECONDS OUT.json
# Launches a staged GTK agterm (GTK_STAGE, default the main checkout's conformance stage) as an isolated dev
# instance: unique app id, private state dir and socket under a throwaway /tmp root, AGTERM_*/ZMX_* stripped. Every
# agtermctl call names that socket. On exit it quits the instance and reaps only the zmx processes carrying its
# ZMX_DIR.
set -euo pipefail
url="$1"; seconds="$2"; out="$3"
here="$(cd "$(dirname "$0")/.." && pwd)"
stage="${GTK_STAGE:-/home/nick/develop/projects/my/agterm-rs/target/conformance-stage}"
root="$(mktemp -d /tmp/agterm-wpe-baseline-XXXXXX)"
state="$root/state"
socket="$state/agterm.sock"
mkdir -p "$state"
state="$(realpath "$state")"
# agterm-core zmx::socket_directory: /tmp/agterm-zmx-<FNV-1a 64 of the resolved state dir>.
zmx_dir="$(python3 -I -c 'import sys
v = 14695981039346656037
for b in sys.argv[1].encode(): v = ((v ^ b) * 1099511628211) & 0xFFFFFFFFFFFFFFFF
print(f"/tmp/agterm-zmx-{v:016x}")' "$state")"
while IFS= read -r name; do unset "$name"; done < <(compgen -e | grep -E '^(AGTERM_|ZMX_)' || true)
export AGTERM_APP_ID="io.github.melonamin.agterm.dev.wpespike$(od -An -N4 -tx1 /dev/urandom | tr -d ' \n')"
export AGTERM_STATE_DIR="$state"
export AGTERM_CONTROL_SOCKET="$socket"
ctl=("$stage/bin/agtermctl")

systemd-run --user --slice=agterm-agents.slice --scope --quiet -- "$stage/bin/agterm" >"$root/agterm.log" 2>&1 &
pid=$!
cleanup() {
  kill -TERM "$pid" 2>/dev/null || true
  for _ in $(seq 50); do kill -0 "$pid" 2>/dev/null || break; sleep 0.2; done
  kill -KILL "$pid" 2>/dev/null || true
  # zmx sessions outlive the app by design: reap only this instance's (its ZMX_DIR is under the private state).
  for p in $(pgrep -u "$USER" -x zmx || true); do
    if tr '\0' '\n' < "/proc/$p/environ" 2>/dev/null | grep -qx "ZMX_DIR=$zmx_dir"; then
      kill "$p" 2>/dev/null || true
    fi
  done
  rm -rf "$zmx_dir"
}
trap cleanup EXIT

for _ in $(seq 100); do [[ -S "$socket" ]] && break; sleep 0.1; done
[[ -S "$socket" ]] || { echo "no socket: $socket" >&2; cat "$root/agterm.log" >&2; exit 1; }
sleep 2
"${ctl[@]}" session overlay open --url "$url" --js --socket "$socket"
sleep 2
python3 -I "$here/scripts/measure.py" "$pid" "$seconds" "$out"
echo "log: $root/agterm.log"
