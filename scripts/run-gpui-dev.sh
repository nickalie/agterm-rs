#!/usr/bin/env bash
# Build, stage and launch this checkout's ngterm (the GPUI build, bin/ngterm) as an isolated dev instance, beside any
# installed ngterm.
#
# The instance gets a unique NGTERM_APP_ID (so the launch is not forwarded to a running ngterm), its own
# NGTERM_STATE_DIR (settings, windows, logs and the config dir with keymap.conf/hooks.conf live there) and its own
# NGTERM_CONTROL_SOCKET inside it. NGTERM_*, AGTERM_* and ZMX_* inherited from a live session are dropped first, all but
# the NGTERM_LOG level. The stage (scripts/stage.sh) is built into <root>/stage, the state into <root>/state, under a
# fresh /tmp/ngterm-dev-* root that is kept after the app quits (printed on start) and removed when the script fails
# before the launch.
#
# Usage: scripts/run-gpui-dev.sh [--release] [--state DIR] [-- NGTERM_ARGS...]
#   --release    build and stage the release profile (default: debug)
#   --state DIR  reuse a state dir from an earlier run, e.g. to check a relaunch
set -euo pipefail

# shellcheck source=lib/dev-instance.sh
source "$(dirname "$0")/lib/dev-instance.sh"

run_dev_instance "${BASH_SOURCE[0]}" "$@"
