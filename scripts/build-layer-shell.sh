#!/usr/bin/env bash
# Build the pinned gtk4-layer-shell (the C library the `gtk4-layer-shell` crate links, under the quick terminal's
# layer-shell panel) into the vendored prefix ${AGTERM_VENDOR:-~/.cache/agterm-rs/vendor}/gtk4-layer-shell-<key>/.
# The key hashes the LAYER_SHELL_ pins of build/pins.env, so a complete prefix is reused as is. Ubuntu 24.04 has no
# package, and the build needs no root: meson, ninja, a C compiler and the GTK 4 and Wayland development files.
#
# Cargo finds the prefix through scripts/pkg-config.sh, which .cargo/config.toml makes the pkg-config of every build.
#
# Usage: scripts/build-layer-shell.sh                 build the prefix unless it is complete, then print its path
#        scripts/build-layer-shell.sh --print-prefix  print the prefix path only
#        scripts/build-layer-shell.sh --verify        exit 0 when the prefix is complete, 1 otherwise
set -euo pipefail

# shellcheck source=lib/native.sh
source "$(dirname "$0")/lib/native.sh"

PREFIX="$(layer_shell_prefix)"

case "${1:-}" in
  --print-prefix)
    echo "$PREFIX"
    exit 0
    ;;
  --verify)
    layer_shell_prefix_is_complete "$PREFIX"
    exit
    ;;
  "") ;;
  *) die "usage: scripts/build-layer-shell.sh [--print-prefix | --verify]" ;;
esac

if layer_shell_prefix_is_complete "$PREFIX"; then
  echo "complete gtk4-layer-shell prefix already vendored at $PREFIX" >&2
  echo "$PREFIX"
  exit 0
fi

for command in curl tar sha256sum meson ninja; do
  command -v "$command" >/dev/null || die "$command is required to build gtk4-layer-shell"
done
mkdir -p "$VENDOR_ROOT"

# both scratch dirs live in the vendor root, beside the final prefix, not in a shared /tmp
BUILD_DIR="$(mktemp -d "$VENDOR_ROOT/.layer-shell-build.XXXXXX")"
# the DESTDIR install root: $STAGE$PREFIX becomes the prefix in one atomic rename
STAGE="$(mktemp -d "$VENDOR_ROOT/.layer-shell-stage.XXXXXX")"
trap 'rm -rf "$BUILD_DIR" "$STAGE"' EXIT

echo "fetching gtk4-layer-shell $LAYER_SHELL_VERSION..." >&2
curl -fsSLo "$BUILD_DIR/source.tar.gz" "$LAYER_SHELL_REPO/archive/refs/tags/v$LAYER_SHELL_VERSION.tar.gz"
echo "$LAYER_SHELL_SHA256  $BUILD_DIR/source.tar.gz" | sha256sum --check --quiet >&2
mkdir "$BUILD_DIR/source"
tar -xzf "$BUILD_DIR/source.tar.gz" -C "$BUILD_DIR/source" --strip-components=1

# Configured with the final prefix, installed through DESTDIR: the generated .pc file names the prefix it ends up
# in. Only the shared library, its header and the .pc file: no introspection, vapi, examples, docs or tests.
echo "building gtk4-layer-shell $LAYER_SHELL_VERSION..." >&2
meson setup "$BUILD_DIR/out" "$BUILD_DIR/source" --prefix="$PREFIX" --libdir=lib --buildtype=release \
  -Dintrospection=false -Dvapi=false -Dexamples=false -Ddocs=false -Dtests=false >&2
ninja -C "$BUILD_DIR/out" >&2
DESTDIR="$STAGE" meson install -C "$BUILD_DIR/out" --no-rebuild --quiet >&2

layer_shell_prefix_is_complete "$STAGE$PREFIX" || die "the gtk4-layer-shell install is incomplete in $STAGE$PREFIX"
chmod 755 "$STAGE$PREFIX"
rm -rf "$PREFIX"
mv "$STAGE$PREFIX" "$PREFIX"
echo "→ vendored gtk4-layer-shell $LAYER_SHELL_VERSION into $PREFIX" >&2
echo "$PREFIX"
