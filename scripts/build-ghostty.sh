#!/usr/bin/env bash
# Build the pinned, patched libghostty and its resources (themes, shell integration, terminfo) into the vendored
# prefix ${AGTERM_VENDOR:-~/.cache/agterm-rs/vendor}/ghostty-<key>/, a port of ../agterm-linux
# scripts/setup-linux.sh. The key hashes the GHOSTTY_ pins of build/pins.env and build/patches/ghostty/, so a
# complete prefix is reused as is.
#
# Usage: scripts/build-ghostty.sh                 build the prefix unless it is complete, then print its path
#        scripts/build-ghostty.sh --print-prefix  print the prefix path only (agterm-ghostty-sys/build.rs)
#        scripts/build-ghostty.sh --verify        exit 0 when the prefix is complete, 1 otherwise
#
# Zig 0.15.2 comes from AGTERM_GHOSTTY_ZIG, a matching zig on PATH, mise, or the pinned ziglang.org tarball.
set -euo pipefail

# shellcheck source=lib/native.sh
source "$(dirname "$0")/lib/native.sh"

PREFIX="$(ghostty_prefix)"
PATCHES="$NATIVE_ROOT/build/patches/ghostty"

case "${1:-}" in
  --print-prefix)
    echo "$PREFIX"
    exit 0
    ;;
  --verify)
    ghostty_prefix_is_complete "$PREFIX"
    exit
    ;;
  "") ;;
  *) die "usage: scripts/build-ghostty.sh [--print-prefix | --verify]" ;;
esac

if ghostty_prefix_is_complete "$PREFIX"; then
  echo "complete libghostty prefix already vendored at $PREFIX" >&2
  echo "$PREFIX"
  exit 0
fi

for command in curl tar patch sha256sum tic infocmp; do
  command -v "$command" >/dev/null || die "$command is required to build libghostty"
done
mkdir -p "$VENDOR_ROOT"
ARCH="$(host_zig_arch)"
ZIG_SHA_VAR="GHOSTTY_ZIG_SHA256_${ARCH^^}"
ZIG="$(find_zig "$GHOSTTY_ZIG_VERSION" "${!ZIG_SHA_VAR}" "${AGTERM_GHOSTTY_ZIG:-}")"

# both scratch dirs live in the vendor root, beside the final prefix, not in a shared /tmp
BUILD_DIR="$(mktemp -d "$VENDOR_ROOT/.ghostty-build.XXXXXX")"
# the stage becomes the prefix in one atomic rename
STAGE="$(mktemp -d "$VENDOR_ROOT/.ghostty-stage.XXXXXX")"
trap 'rm -rf "$BUILD_DIR" "$STAGE"' EXIT

# The source archive of the full commit id, not a git checkout: the build needs no git, and ghostty's build.zig
# falls back to its build.zig.zon version outside a repository.
echo "fetching ghostty $GHOSTTY_REV..." >&2
curl -fsSL "$GHOSTTY_REPO/archive/$GHOSTTY_REV.tar.gz" | tar -xz -C "$BUILD_DIR" --strip-components=1
grep -F "$GHOSTTY_THEMES_URL" "$BUILD_DIR/build.zig.zon" >/dev/null \
  || die "pinned Ghostty source no longer declares the recorded theme dependency"

for patch in "$PATCHES"/*.patch; do
  echo "applying $(basename "$patch")..." >&2
  patch -d "$BUILD_DIR" -p1 --batch --forward --fuzz=0 --no-backup-if-mismatch --quiet <"$patch"
done

echo "building libghostty with zig $GHOSTTY_ZIG_VERSION..." >&2
# Zig defaults to Debug, whose terminal integrity checks make sustained PTY output unusably slow.
(
  cd "$BUILD_DIR"
  "$ZIG" build -Doptimize=ReleaseFast -Dapp-runtime=none -Dtarget="$ARCH-linux-gnu.2.39" \
    -Demit-themes=false -Demit-terminfo=true
) >&2

mkdir -p "$STAGE/lib" "$STAGE/include" "$STAGE/share/ghostty/themes"
cp "$BUILD_DIR/zig-out/lib/ghostty-internal.so" "$STAGE/lib/libghostty.so"
cp -R "$BUILD_DIR/zig-out/include/." "$STAGE/include/"
cp -R "$BUILD_DIR/src/shell-integration" "$STAGE/share/ghostty/shell-integration"

echo "fetching pinned Ghostty theme dependency..." >&2
curl -fsSLo "$BUILD_DIR/ghostty-themes.tgz" "$GHOSTTY_THEMES_URL"
echo "$GHOSTTY_THEMES_SHA256  $BUILD_DIR/ghostty-themes.tgz" | sha256sum --check --quiet >&2
tar -xzf "$BUILD_DIR/ghostty-themes.tgz" -C "$STAGE/share/ghostty/themes" --strip-components=1
pin_lines GHOSTTY >"$STAGE/share/ghostty/themes/.agterm-resource-manifest"

TERMINFO_SOURCE="$BUILD_DIR/zig-out/share/terminfo/ghostty.terminfo"
[[ -s "$TERMINFO_SOURCE" ]] || die "Ghostty build did not generate ghostty.terminfo"
# Keep the compiler input beside Ghostty's generator for an explicit source-to-tic provenance chain.
cp "$TERMINFO_SOURCE" "$BUILD_DIR/src/terminfo/ghostty.terminfo"
tic -x -o "$STAGE/share/terminfo" "$BUILD_DIR/src/terminfo/ghostty.terminfo"

verify_ghostty_resources "$STAGE/share" >&2
chmod 755 "$STAGE"
rm -rf "$PREFIX"
mv "$STAGE" "$PREFIX"
echo "→ vendored libghostty and its resources into $PREFIX" >&2
echo "$PREFIX"
