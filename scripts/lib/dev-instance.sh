# The isolated dev instance of scripts/run-gtk-dev.sh and scripts/run-gpui-dev.sh: build, stage and launch this
# checkout's agterm beside any installed agterm (the entry scripts' headers describe the instance). Sourced, not run.

# shellcheck source=native.sh
source "$(dirname "${BASH_SOURCE[0]}")/native.sh"

# run_dev_instance FRONTEND USAGE_SCRIPT [--release] [--state DIR] [-- AGTERM_ARGS...]
#   FRONTEND      gtk or gpui: stage.sh's --frontend, which picks bin/agterm or bin/agterm-gpui
#   USAGE_SCRIPT  the entry script, whose header comment --help prints
run_dev_instance() {
  local frontend="$1" usage_script="$2"
  shift 2
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

  local app_name=agterm
  [[ "$frontend" == gtk ]] || app_name=agterm-gpui

  root="$(mktemp -d /tmp/agterm-dev-XXXXXX)"
  # Removed when the script fails before the app starts; exec drops the trap, so a launched instance keeps it.
  trap 'rm -rf "$root"' EXIT
  if [[ -z "$state" ]]; then
    state="$root/state"
  fi
  mkdir -p "$state"
  state="$(cd "$state" && pwd)"
  local socket="$state/agterm.sock"
  # sun_path holds 108 bytes on Linux, but the app refuses 104 or more, as upstream's macOS limit.
  ((${#socket} < 104)) || die "socket path too long (${#socket} bytes, the limit is 103): $socket"

  # both frontends' workspace without what links WPE WebKit, then the GPUI frontend: with its HTML overlays (which need
  # a WPE WebKit, README > WPE WebKit) when it is the one launched, without them beside the GTK app
  local profile_args=()
  [[ "$profile" == release ]] && profile_args=(--release)
  local gpui_args=("${GPUI_WITHOUT_HTML[@]}")
  [[ "$frontend" == gtk ]] || gpui_args=(-p agterm-gpui)
  (cd "$NATIVE_ROOT" && cargo build "${profile_args[@]}" "${WORKSPACE_WITHOUT_WPE[@]}" \
    && cargo build "${profile_args[@]}" "${gpui_args[@]}")
  "$NATIVE_ROOT/scripts/stage.sh" "$root/stage" --profile "$profile" --frontend "$frontend"
  local app="$root/stage/bin/$app_name"
  [[ -x "$app" ]] || die "no $app_name binary staged in $root/stage: the app crate does not build one yet"

  local name
  while IFS= read -r name; do
    unset "$name"
  done < <(compgen -e | grep -E '^(AGTERM_|ZMX_)' | grep -vx AGTERM_LOG || true)
  export AGTERM_APP_ID="io.github.melonamin.agterm.dev.i$(od -An -N6 -tx1 /dev/urandom | tr -d ' \n')"
  export AGTERM_STATE_DIR="$state"
  export AGTERM_CONTROL_SOCKET="$socket"

  cat >&2 <<EOF
→ agterm dev instance ($frontend, $profile)
  app id: $AGTERM_APP_ID
  state:  $state
  socket: $socket
  ctl:    $root/stage/bin/agtermctl <command> --socket $socket
EOF
  trap - EXIT
  exec "$app" "${args[@]}"
}
