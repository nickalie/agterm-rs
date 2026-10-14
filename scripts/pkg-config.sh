#!/usr/bin/env bash
# The pkg-config of every cargo build in this repository (.cargo/config.toml sets PKG_CONFIG to it; an exported
# PKG_CONFIG wins): the system pkg-config, with the vendored gtk4-layer-shell prefix (scripts/build-layer-shell.sh)
# first on PKG_CONFIG_PATH when it is complete. So the pinned build wins over a system package, and without it the
# system package is used; with neither, gtk4-layer-shell-sys fails naming `gtk4-layer-shell-0`.
#
# WPE WebKit the other way round: the system's wpe-webkit-2.0 wins when it is at least WPE_API_VERSION (the distro
# package is the one agterm runs with), else the vendored prefix of scripts/fetch-wpe.sh goes first on PKG_CONFIG_PATH
# when it is complete, ahead of an older system one; with neither, the WPE -sys crates fail naming `wpe-webkit-2.0`.
# Cargo does not see that choice change: after installing or removing either, run
# `cargo clean -p agterm-wpe-sys -p agterm-wpe-platform-sys -p agterm-wpe-javascriptcore-sys`.
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
WPE="$(wpe_prefix)"
if wpe_prefix_is_complete "$WPE" && ! pkg-config --atleast-version="$WPE_API_VERSION" wpe-webkit-2.0 2>/dev/null; then
  export PKG_CONFIG_PATH="$WPE/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
fi
exec pkg-config "$@"
