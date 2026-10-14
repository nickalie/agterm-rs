#!/usr/bin/env bash
# Syntax-checks csrc/wpe_shim.c against the WPE prefix headers.
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
inc="$here/.cache/prefix/usr/include/wpe-webkit-2.0"
# shellcheck disable=SC2046
gcc -fsyntax-only -Wall -Wextra -I "$inc" -I "$inc/wpe-platform" \
  $(pkg-config --cflags glib-2.0 gobject-2.0 gio-2.0 libsoup-3.0) "$here/csrc/wpe_shim.c"
