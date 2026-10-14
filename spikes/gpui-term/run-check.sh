#!/usr/bin/env bash
# Run check.py inside the shared agents slice. check.py builds every child's environment from scratch (no AGTERM_*,
# ZMX_*, WAYLAND_DISPLAY or DISPLAY reaches the spike or its weston); the unset here is belt and braces.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
while IFS= read -r name; do unset "$name"; done < <(compgen -e | grep -E '^(AGTERM_|ZMX_|WAYLAND_DISPLAY$|DISPLAY$)' || true)
exec systemd-run --user --slice=agterm-agents.slice --scope --quiet -- python3 -I "$here/check.py" "$@"
