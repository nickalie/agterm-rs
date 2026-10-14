#!/usr/bin/env bash
# Runs the spike with the WPE WebKit prefix overlaid on /usr/lib: a distro WPE build looks for its helper processes
# only at the compiled-in /usr/lib/wpe-webkit-2.0, so bwrap overlays the extracted package there (no root needed).
# AGTERM_*/ZMX_* are stripped; the process runs in the shared agents slice. Arguments go to the spike binary.
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
prefix="$here/.cache/prefix/usr"
bin="${SPIKE_BIN:-$here/target/release/wpe-dmabuf-spike}"
[[ -d "$prefix/lib/wpe-webkit-2.0" ]] || { echo "no WPE prefix at $prefix (scripts/fetch-wpe.sh)" >&2; exit 1; }
while IFS= read -r name; do unset "$name"; done < <(compgen -e | grep -E '^(AGTERM_|ZMX_)' || true)
export RUST_LOG="${RUST_LOG:-info,zbus=warn,tracing=warn,gpui::window::a11y=warn}"
exec systemd-run --user --slice=agterm-agents.slice --scope --quiet -- \
  bwrap --dev-bind / / --overlay-src "$prefix/lib" --overlay-src /usr/lib --ro-overlay /usr/lib -- "$bin" "$@"
