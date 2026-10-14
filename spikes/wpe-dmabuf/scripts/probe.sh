#!/usr/bin/env bash
# Builds csrc/probe.c with the shim and runs it headless (no window) under the WPE prefix overlay:
#   probe.sh DEVICE|- URL SECONDS   (extra env, e.g. WEBKIT_SKIA_ENABLE_CPU_RENDERING=1, passes through)
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
prefix="$here/.cache/prefix/usr"
inc="$prefix/include/wpe-webkit-2.0"
mkdir -p "$here/target"
# shellcheck disable=SC2046
gcc -O1 -Wall -I "$inc" -I "$inc/wpe-platform" $(pkg-config --cflags --libs glib-2.0 gobject-2.0 gio-2.0 libsoup-3.0) \
  -o "$here/target/probe" "$here/csrc/probe.c" "$here/csrc/wpe_shim.c" -L "$prefix/lib" -lWPEWebKit-2.0 \
  $(pkg-config --libs glib-2.0 gobject-2.0 gio-2.0)
while IFS= read -r name; do unset "$name"; done < <(compgen -e | grep -E '^(AGTERM_|ZMX_)' || true)
exec systemd-run --user --slice=agterm-agents.slice --scope --quiet -- \
  bwrap --dev-bind / / --overlay-src "$prefix/lib" --overlay-src /usr/lib --ro-overlay /usr/lib -- \
  timeout 120 "$here/target/probe" "$@"
