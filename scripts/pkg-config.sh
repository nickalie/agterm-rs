#!/usr/bin/env bash
# The pkg-config of every cargo build in this repository (.cargo/config.toml sets PKG_CONFIG to it; an exported
# PKG_CONFIG wins): the system pkg-config, with the vendored gtk4-layer-shell prefix (scripts/build-layer-shell.sh)
# first on PKG_CONFIG_PATH when it is complete. So the pinned build wins over a system package, and without it the
# system package is used; with neither, gtk4-layer-shell-sys fails naming `gtk4-layer-shell-0`.
#
# Host-only: the vendored prefix is built for this machine, and a custom PKG_CONFIG tells the pkg-config crate that
# cross builds are understood, so a cross build is refused instead of linking host libraries.
set -euo pipefail

if [[ -n "${HOST:-}" && -n "${TARGET:-}" && "$HOST" != "$TARGET" ]]; then
  echo "scripts/pkg-config.sh is host-only ($HOST, building for $TARGET): export PKG_CONFIG with a target-aware" \
    "pkg-config for cross builds" >&2
  exit 1
fi

# shellcheck source=lib/native.sh
source "$(dirname "$0")/lib/native.sh"

PREFIX="$(layer_shell_prefix)"
if layer_shell_prefix_is_complete "$PREFIX"; then
  export PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
fi
exec pkg-config "$@"
