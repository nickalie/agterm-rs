#!/usr/bin/env bash
# Build, stage and launch this checkout's agterm as an isolated dev instance, beside any installed agterm.
#
# The instance gets a unique AGTERM_APP_ID (so the launch is not forwarded to a running agterm), its own
# AGTERM_STATE_DIR (settings, windows, logs and the config dir with keymap.conf/hooks.conf live there) and its own
# AGTERM_CONTROL_SOCKET inside it. AGTERM_* and ZMX_* inherited from a live agterm session are dropped first, all but
# the AGTERM_LOG level. The stage is built into <root>/stage, the state into <root>/state, under a fresh
# /tmp/agterm-dev-* root that is kept after the app quits (printed on start) and removed when the script fails before
# the launch.
#
# Usage: scripts/run-dev.sh [--release] [--state DIR] [-- AGTERM_ARGS...]
#   --release    build and stage the release profile (default: debug)
#   --state DIR  reuse a state dir from an earlier run, e.g. to check a relaunch
set -euo pipefail

# shellcheck source=lib/native.sh
source "$(dirname "$0")/lib/native.sh"

profile=debug
state=""
args=()
while (($#)); do
  case "$1" in
    --release) profile=release; shift ;;
    --state) state="$2"; shift 2 ;;
    --) shift; args=("$@"); break ;;
    -h | --help) sed -n '2,14s/^# \{0,1\}//p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) die "unknown argument: $1 (see --help)" ;;
  esac
done

root="$(mktemp -d /tmp/agterm-dev-XXXXXX)"
# Removed when the script fails before the app starts; exec drops the trap, so a launched instance keeps it.
trap 'rm -rf "$root"' EXIT
if [[ -z "$state" ]]; then
  state="$root/state"
fi
mkdir -p "$state"
state="$(cd "$state" && pwd)"
socket="$state/agterm.sock"
# sun_path holds 108 bytes on Linux, but the app refuses 104 or more, as upstream's macOS limit.
((${#socket} < 104)) || die "socket path too long (${#socket} bytes, the limit is 103): $socket"

cargo_args=(build --workspace)
[[ "$profile" == release ]] && cargo_args+=(--release)
(cd "$NATIVE_ROOT" && cargo "${cargo_args[@]}")
"$NATIVE_ROOT/scripts/stage.sh" "$root/stage" --profile "$profile"
app="$root/stage/bin/agterm"
[[ -x "$app" ]] || die "no agterm binary staged in $root/stage: the app crate does not build one yet"

while IFS= read -r name; do unset "$name"; done < <(compgen -e | grep -E '^(AGTERM_|ZMX_)' | grep -vx AGTERM_LOG || true)
export AGTERM_APP_ID="io.github.melonamin.agterm.dev.i$(od -An -N6 -tx1 /dev/urandom | tr -d ' \n')"
export AGTERM_STATE_DIR="$state"
export AGTERM_CONTROL_SOCKET="$socket"

cat >&2 <<EOF
→ agterm dev instance ($profile)
  app id: $AGTERM_APP_ID
  state:  $state
  socket: $socket
  ctl:    $root/stage/bin/agtermctl <command> --socket $socket
EOF
trap - EXIT
exec "$app" "${args[@]}"
