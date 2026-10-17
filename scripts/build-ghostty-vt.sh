#!/usr/bin/env bash
# Build the pinned, patched libghostty-vt (Ghostty's VT library behind its official C API, include/ghostty/vt.h) into
# the vendored prefix ${AGTERM_VENDOR:-~/.cache/agterm-rs/vendor}/ghostty-vt-<key>/. The key hashes the
# LIBGHOSTTY_VT_ pins of build/pins.env, build/patches/ghostty-vt/ and the host arch, so a GHOSTTY_ pin or a libghostty
# patch never rebuilds it, this one never rebuilds libghostty, and a complete prefix is reused as is.
#
# The patches (build/patches/ghostty-vt/*.patch, applied in name order with no fuzz) add what the GPUI build needs and
# the C API at the pin lacks; each names what it adds in its header. A patch change regenerates the bindings
# (scripts/gen-ghostty-vt-bindings.sh), like a pin bump.
#
# The prefix holds lib/libghostty-vt.a (linked statically, so staging gains nothing), include/ghostty/,
# share/doc/ghostty-vt/LICENSE and share/agterm/ghostty-vt-PINS (the LIBGHOSTTY_VT_ pin lines it was built from).
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

for command in curl tar patch sha256sum; do
  command -v "$command" >/dev/null || die "$command is required to build libghostty-vt"
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
mkdir -p "$STAGE/lib" "$STAGE/include" "$STAGE/share/doc/ghostty-vt" "$STAGE/share/agterm"
cp "$OUT/lib/libghostty-vt.a" "$STAGE/lib/"
cp -R "$OUT/include/ghostty" "$STAGE/include/ghostty"
cp "$BUILD_DIR/src/LICENSE" "$STAGE/share/doc/ghostty-vt/LICENSE"
pin_lines LIBGHOSTTY_VT >"$STAGE/share/agterm/ghostty-vt-PINS"

ghostty_vt_prefix_is_complete "$STAGE" || die "the staged libghostty-vt prefix is incomplete"
chmod 755 "$STAGE"
rm -rf "$PREFIX"
mv "$STAGE" "$PREFIX"
echo "→ vendored libghostty-vt into $PREFIX" >&2
echo "$PREFIX"
