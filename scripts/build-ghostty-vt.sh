#!/usr/bin/env bash
# Build the pinned, patched libghostty-vt (Ghostty's VT library behind its official C API, include/ghostty/vt.h) and
# Ghostty's resources at the same revision into the vendored prefix
# ${AGTERM_VENDOR:-~/.cache/agterm-rs/vendor}/ghostty-vt-<key>/. The key hashes the LIBGHOSTTY_VT_ pins of
# build/pins.env (the one Ghostty pin), build/patches/ghostty-vt/ and the host arch, so a zmx pin never rebuilds it and
# a complete prefix is reused as is.
#
# The patches (build/patches/ghostty-vt/*.patch, applied in name order with no fuzz) add what the app needs and the C
# API at the pin lacks; each names what it adds in its header. A patch change regenerates the bindings
# (scripts/gen-ghostty-vt-bindings.sh), like a pin bump.
#
# The prefix holds lib/libghostty-vt.a (linked statically, so staging gains nothing), include/ghostty/,
# share/doc/ghostty-vt/LICENSE, share/ngterm/ghostty-vt-PINS (the LIBGHOSTTY_VT_ pin lines it was built from) and
# the resources scripts/stage.sh stages, as Ghostty's own build installs them (src/build/GhosttyResources.zig):
# share/ghostty/shell-integration from the source, share/ghostty/themes from the theme package the revision's
# build.zig.zon declares (LIBGHOSTTY_VT_THEMES_*, its sha256 checked), and share/terminfo, the xterm-ghostty entry
# Ghostty's terminfo source (src/terminfo/ghostty.zig) encodes, compiled with `tic`.
#
# Usage: scripts/build-ghostty-vt.sh                 build the prefix unless it is complete, then print its path
#        scripts/build-ghostty-vt.sh --print-prefix  print the prefix path only (agterm-ghostty-vt-sys/build.rs)
#        scripts/build-ghostty-vt.sh --verify        exit 0 when the prefix is complete, 1 otherwise
#
# LIBGHOSTTY_VT_ZIG_VERSION comes from AGTERM_GHOSTTY_VT_ZIG, a matching zig on PATH, mise, or the pinned ziglang.org
# tarball.
set -euo pipefail

# shellcheck source=lib/native.sh
source "$(dirname "$0")/lib/native.sh"

PREFIX="$(ghostty_vt_prefix)"
PATCHES="$NATIVE_ROOT/build/patches/ghostty-vt"

case "${1:-}" in
  --print-prefix)
    echo "$PREFIX"
    exit 0
    ;;
  --verify)
    ghostty_vt_prefix_is_complete "$PREFIX"
    exit
    ;;
  "") ;;
  *) die "usage: scripts/build-ghostty-vt.sh [--print-prefix | --verify]" ;;
esac

if ghostty_vt_prefix_is_complete "$PREFIX"; then
  echo "complete libghostty-vt prefix already vendored at $PREFIX" >&2
  echo "$PREFIX"
  exit 0
fi

for command in curl tar patch sha256sum tic infocmp; do
  command -v "$command" >/dev/null || die "$command is required to build libghostty-vt and its resources"
done
mkdir -p "$VENDOR_ROOT"
ARCH="$(host_zig_arch)"
ZIG_SHA_VAR="LIBGHOSTTY_VT_ZIG_SHA256_${ARCH^^}"
ZIG="$(find_zig "$LIBGHOSTTY_VT_ZIG_VERSION" "${!ZIG_SHA_VAR}" "${AGTERM_GHOSTTY_VT_ZIG:-}")"

# both scratch dirs live in the vendor root, beside the final prefix, not in a shared /tmp
BUILD_DIR="$(mktemp -d "$VENDOR_ROOT/.ghostty-vt-build.XXXXXX")"
# the stage becomes the prefix in one atomic rename
STAGE="$(mktemp -d "$VENDOR_ROOT/.ghostty-vt-stage.XXXXXX")"
trap 'rm -rf "$BUILD_DIR" "$STAGE"' EXIT

# The source archive of the full commit id, verified against its pinned sha256 before anything is unpacked: the
# build needs no git, and ghostty's build.zig falls back to its build.zig.zon version outside a repository.
echo "fetching ghostty $LIBGHOSTTY_VT_REV for libghostty-vt..." >&2
curl -fsSLo "$BUILD_DIR/source.tar.gz" "$LIBGHOSTTY_VT_REPO/archive/$LIBGHOSTTY_VT_REV.tar.gz"
echo "$LIBGHOSTTY_VT_SHA256  $BUILD_DIR/source.tar.gz" | sha256sum --check --quiet >&2 \
  || die "ghostty $LIBGHOSTTY_VT_REV source archive sha256 mismatch"
mkdir "$BUILD_DIR/src"
tar -xzf "$BUILD_DIR/source.tar.gz" -C "$BUILD_DIR/src" --strip-components=1

for patch in "$PATCHES"/*.patch; do
  [[ -f "$patch" ]] || continue
  echo "applying $(basename "$patch")..." >&2
  patch -d "$BUILD_DIR/src" -p1 --batch --forward --fuzz=0 --no-backup-if-mismatch --quiet <"$patch" \
    || die "$(basename "$patch") does not apply to ghostty $LIBGHOSTTY_VT_REV"
done

echo "building libghostty-vt with zig $LIBGHOSTTY_VT_ZIG_VERSION..." >&2
# Zig defaults to Debug, whose terminal integrity checks make sustained PTY output unusably slow; the glibc floor is
# the libghostty build's, so one binary runs on every supported distribution.
(
  cd "$BUILD_DIR/src"
  "$ZIG" build -Demit-lib-vt=true -Doptimize=ReleaseFast -Dtarget="$ARCH-linux-gnu.2.39"
) >&2

OUT="$BUILD_DIR/src/zig-out"
[[ -s "$OUT/lib/libghostty-vt.a" ]] || die "the libghostty-vt build did not produce lib/libghostty-vt.a"
[[ -s "$OUT/include/ghostty/vt.h" ]] || die "the libghostty-vt build did not install include/ghostty/vt.h"
mkdir -p "$STAGE/lib" "$STAGE/include" "$STAGE/share/doc/ghostty-vt" "$STAGE/share/ngterm"
cp "$OUT/lib/libghostty-vt.a" "$STAGE/lib/"
cp -R "$OUT/include/ghostty" "$STAGE/include/ghostty"
cp "$BUILD_DIR/src/LICENSE" "$STAGE/share/doc/ghostty-vt/LICENSE"
pin_lines LIBGHOSTTY_VT >"$STAGE/share/ngterm/ghostty-vt-PINS"

# The resources (the header names them). The vt build installs none of them, so they are taken as Ghostty's full build
# takes them: the shell integration as is, the themes from the package its build.zig.zon pins, the terminfo encoded by
# the revision's own Zig and compiled here.
mkdir -p "$STAGE/share/ghostty/themes"
cp -R "$BUILD_DIR/src/src/shell-integration" "$STAGE/share/ghostty/shell-integration"
grep -F "$LIBGHOSTTY_VT_THEMES_URL" "$BUILD_DIR/src/build.zig.zon" >/dev/null \
  || die "ghostty $LIBGHOSTTY_VT_REV no longer declares the pinned theme package (LIBGHOSTTY_VT_THEMES_URL)"
echo "fetching the pinned Ghostty theme package..." >&2
curl -fsSLo "$BUILD_DIR/ghostty-themes.tgz" "$LIBGHOSTTY_VT_THEMES_URL"
echo "$LIBGHOSTTY_VT_THEMES_SHA256  $BUILD_DIR/ghostty-themes.tgz" | sha256sum --check --quiet >&2 \
  || die "Ghostty theme package sha256 mismatch"
tar -xzf "$BUILD_DIR/ghostty-themes.tgz" -C "$STAGE/share/ghostty/themes" --strip-components=1
pin_lines LIBGHOSTTY_VT >"$STAGE/share/ghostty/themes/.agterm-resource-manifest"

# What Ghostty's build-data tool runs for `+terminfo` (src/main_build_data.zig), alone: the entry's encoder needs
# nothing but std, so this needs none of the build's dependencies.
cat >"$BUILD_DIR/src/src/agterm_terminfo.zig" <<'ZIG'
const std = @import("std");

pub fn main(init: std.process.Init) !void {
    var buffer: [1024]u8 = undefined;
    var stdout_writer = std.Io.File.stdout().writerStreaming(init.io, &buffer);
    try @import("terminfo/ghostty.zig").ghostty.encode(&stdout_writer.interface);
    try stdout_writer.end();
}
ZIG
echo "encoding Ghostty's terminfo..." >&2
(cd "$BUILD_DIR/src" && "$ZIG" run src/agterm_terminfo.zig) >"$BUILD_DIR/ghostty.terminfo"
[[ -s "$BUILD_DIR/ghostty.terminfo" ]] || die "Ghostty's terminfo encoder wrote nothing"
tic -x -o "$STAGE/share/terminfo" "$BUILD_DIR/ghostty.terminfo"

ghostty_vt_prefix_is_complete "$STAGE" || die "the staged libghostty-vt prefix is incomplete"
chmod 755 "$STAGE"
rm -rf "$PREFIX"
mv "$STAGE" "$PREFIX"
echo "→ vendored libghostty-vt into $PREFIX" >&2
echo "$PREFIX"
