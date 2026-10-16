#!/usr/bin/env bash
# Build, stage and launch this checkout's GPUI agterm (bin/agterm-gpui) as an isolated dev instance, beside any
# installed agterm.
#
# The instance gets a unique AGTERM_APP_ID (so the launch is not forwarded to a running agterm), its own
# AGTERM_STATE_DIR (settings, windows, logs and the config dir with keymap.conf/hooks.conf live there) and its own
# AGTERM_CONTROL_SOCKET inside it. AGTERM_* and ZMX_* inherited from a live agterm session are dropped first, all but
# the AGTERM_LOG level. The stage (stage.sh --frontend gpui) is built into <root>/stage, the state into <root>/state,
# under a fresh /tmp/agterm-dev-* root that is kept after the app quits (printed on start) and removed when the script
# fails before the launch. scripts/run-gtk-dev.sh does the same for the GTK build.
#
# Usage: scripts/run-gpui-dev.sh [--release] [--state DIR] [-- AGTERM_ARGS...]
#   --release    build and stage the release profile (default: debug)
#   --state DIR  reuse a state dir from an earlier run, e.g. to check a relaunch
set -euo pipefail

# shellcheck source=lib/dev-instance.sh
source "$(dirname "$0")/lib/dev-instance.sh"

run_dev_instance gpui "${BASH_SOURCE[0]}" "$@"
