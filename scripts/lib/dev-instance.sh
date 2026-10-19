# The isolated dev instance of scripts/run-gpui-dev.sh: build, stage and launch this checkout's agterm beside any
# installed agterm (the entry script's header describes the instance). Sourced, not run.

# shellcheck source=native.sh
source "$(dirname "${BASH_SOURCE[0]}")/native.sh"

# run_dev_instance USAGE_SCRIPT [--release] [--state DIR] [-- AGTERM_ARGS...]
#   USAGE_SCRIPT  the entry script, whose header comment --help prints
run_dev_instance() {
  local usage_script="$1"
  shift
  local profile=debug state="" args=()
  while (($#)); do
    case "$1" in
      --release) profile=release; shift ;;
      --state) state="$2"; shift 2 ;;
      --) shift; args=("$@"); break ;;
      -h | --help) sed -n '2,/^set /{/^set /d;s/^# \{0,1\}//p}' "$usage_script"; exit 0 ;;
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
  local socket="$state/ngterm.sock"
  # sun_path holds 108 bytes on Linux, but the app refuses 104 or more, as upstream's macOS limit.
  ((${#socket} < 104)) || die "socket path too long (${#socket} bytes, the limit is 103): $socket"

  # the workspace without what links WPE WebKit (ngtermctl among it), then the app with its HTML overlays (which need a
  # WPE WebKit, README > WPE WebKit)
  local profile_args=()
  [[ "$profile" == release ]] && profile_args=(--release)
  (cd "$NATIVE_ROOT" && cargo build "${profile_args[@]}" "${WORKSPACE_WITHOUT_WPE[@]}" \
    && cargo build "${profile_args[@]}" -p agterm-gpui)
  "$NATIVE_ROOT/scripts/stage.sh" "$root/stage" --profile "$profile"
  local app="$root/stage/bin/agterm"
  [[ -x "$app" ]] || die "no bin/agterm staged in $root/stage"

  local name
  while IFS= read -r name; do
    unset "$name"
  done < <(compgen -e | grep -E '^(AGTERM_|ZMX_)' | grep -vx AGTERM_LOG || true)
  AGTERM_APP_ID="com.nickalie.ngterm.dev.i$(od -An -N6 -tx1 /dev/urandom | tr -d ' \n')"
  export AGTERM_APP_ID
  export AGTERM_STATE_DIR="$state"
  export AGTERM_CONTROL_SOCKET="$socket"

  cat >&2 <<EOF
→ agterm dev instance ($profile)
  app id: $AGTERM_APP_ID
  state:  $state
  socket: $socket
  ctl:    $root/stage/bin/ngtermctl <command> --socket $socket
EOF
  trap - EXIT
  exec "$app" "${args[@]}"
}
