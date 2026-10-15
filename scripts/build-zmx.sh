#!/usr/bin/env bash
# Build the pinned, patched zmx multiplexer behind live sessions into the vendored prefix
# ${AGTERM_VENDOR:-~/.cache/agterm-rs/vendor}/zmx-<key>/, a port of ../agterm-linux scripts/setup-linux-zmx.sh and
# of the zmx half of upstream's scripts/setup.sh. The key hashes the ZMX_ pins of build/pins.env, build/patches/zmx/
# and build/patches/zmx/ghostty/, so a zmx patch never rebuilds libghostty and a complete prefix is reused as is.
#
# The prefix holds bin/zmx, share/doc/zmx/LICENSE and share/agterm/zmx-BUILD: upstream's `.zmx-build-stamp`
# (`<ZMX_REV> <target> <patch digest>`), which the app reads as the bundled zmx build id (`ZmxBuildRecord`).
#
# The patches are upstream's (scripts/zmx-patches), except 0003-first-attach-snapshot.patch, this build's own: a new
# daemon's first attach is sent the screen the program drew before the client connected (zmx sends a snapshot on a
# re-attach only, so a prompt printed in that window never reached the first client), every snapshot carries the
# sequence the daemon's parser is in the middle of, so the client continues where the daemon does, and a client sent
# output between its connect and its Init (first attach or re-attach) gets a reset ahead of the snapshot that repeats
# it (CAN, origin mode and scrolling region off, home, erase below).
#
# Usage: scripts/build-zmx.sh                 build the prefix unless it is complete, then print its path
#        scripts/build-zmx.sh --print-prefix  print the prefix path only (scripts/stage.sh)
#        scripts/build-zmx.sh --verify        exit 0 when the prefix is complete, 1 otherwise
#
# ZMX_ZIG_VERSION comes from AGTERM_ZMX_ZIG, a matching zig on PATH, mise, or the pinned ziglang.org tarball.
set -euo pipefail

# shellcheck source=lib/native.sh
source "$(dirname "$0")/lib/native.sh"

PATCHES="$NATIVE_ROOT/build/patches/zmx"
# patches for the ghostty revision zmx itself pins, applied to a private checkout of it
GHOSTTY_PATCHES="$PATCHES/ghostty"
PREFIX="$VENDOR_ROOT/zmx-$(prefix_key ZMX "$PATCHES" "$GHOSTTY_PATCHES")"
ARCH="$(host_zig_arch)"
# The glibc floor the libghostty build targets, so one payload runs on every supported distribution.
TARGET="$ARCH-linux-gnu.2.39"
PATCH_DIGEST="$(cat "$PATCHES"/*.patch "$GHOSTTY_PATCHES"/*.patch | sha256_of | cut -c1-16)"
BUILD_ID="$ZMX_REV $TARGET $PATCH_DIGEST"

zmx_prefix_is_complete() {
  local prefix="$1"
  [[ -x "$prefix/bin/zmx" && -s "$prefix/share/doc/zmx/LICENSE" && -s "$prefix/share/agterm/zmx-BUILD" ]] \
    && [[ "$(cat "$prefix/share/agterm/zmx-BUILD")" == "$BUILD_ID" ]]
}

case "${1:-}" in
  --print-prefix)
    echo "$PREFIX"
    exit 0
    ;;
  --verify)
    zmx_prefix_is_complete "$PREFIX"
    exit
    ;;
  "") ;;
  *) die "usage: scripts/build-zmx.sh [--print-prefix | --verify]" ;;
esac

if zmx_prefix_is_complete "$PREFIX"; then
  echo "complete zmx prefix already vendored at $PREFIX" >&2
  echo "$PREFIX"
  exit 0
fi

for command in curl tar patch sha256sum; do
  command -v "$command" >/dev/null || die "$command is required to build zmx"
done
mkdir -p "$VENDOR_ROOT"
ZIG_SHA_VAR="ZMX_ZIG_SHA256_${ARCH^^}"
ZIG="$(find_zig "$ZMX_ZIG_VERSION" "${!ZIG_SHA_VAR}" "${AGTERM_ZMX_ZIG:-}")"

# both scratch dirs live in the vendor root, beside the final prefix, not in a shared /tmp
BUILD_ROOT="$(mktemp -d "$VENDOR_ROOT/.zmx-build.XXXXXX")"
# 0002 points zmx's ghostty dependency at the sibling `../zmx-ghostty`, so both trees share one root.
BUILD_DIR="$BUILD_ROOT/zmx"
ZMX_GHOSTTY="$BUILD_ROOT/zmx-ghostty"
# the stage becomes the prefix in one atomic rename
STAGE="$(mktemp -d "$VENDOR_ROOT/.zmx-stage.XXXXXX")"
trap 'rm -rf "$BUILD_ROOT" "$STAGE"' EXIT

# zmx's ghostty comes through zig's own package fetcher (a shallow git fetch of the full commit id, as upstream's setup.sh
# and the fork's setup-linux-zmx.sh check out), into a cache of this build only: no git is needed, GitHub's on-the-fly
# source archive of a non-release revision is slow enough to be reset mid-transfer, and the hash zig computes is
# checked against the one zmx's build.zig.zon pins.
FETCH_CACHE="$BUILD_ROOT/zig-cache"
mkdir -p "$BUILD_ROOT/fetch" "$FETCH_CACHE"
# `zig fetch` looks for a build root even without --save
: >"$BUILD_ROOT/fetch/build.zig"

# Fetch URL into DEST and print the package hash.
fetch_package() {
  local url="$1" dest="$2" hash package
  hash="$(cd "$BUILD_ROOT/fetch" && "$ZIG" fetch --global-cache-dir "$FETCH_CACHE" "$url")" || die "cannot fetch $url"
  package="$FETCH_CACHE/p/$hash"
  mkdir -p "$dest"
  if [[ -d "$package" ]]; then
    cp -R "$package/." "$dest/" || die "cannot copy $package"
  else
    tar -xzf "$package.tar.gz" -C "$dest" --strip-components=1 || die "cannot unpack $package.tar.gz"
  fi
  echo "$hash"
}

# zmx itself as the source archive of its full commit id, as build-ghostty.sh fetches libghostty: zig's package
# fetcher keeps only the files a package's build.zig.zon lists, and zmx's leaves out its LICENSE.
echo "fetching zmx $ZMX_REV..." >&2
mkdir -p "$BUILD_DIR"
curl -fsSL --retry 3 "$ZMX_REPO/archive/$ZMX_REV.tar.gz" | tar -xz -C "$BUILD_DIR" --strip-components=1

# read before the zmx patches repoint the dependency to the private, patched tree
zmx_ghostty_rev="$(sed -n 's|.*ghostty-org/ghostty#\([0-9a-f]\{40\}\)".*|\1|p' "$BUILD_DIR/build.zig.zon")"
zmx_ghostty_hash="$(sed -n 's|.*\.hash = "\(ghostty-[^"]*\)".*|\1|p' "$BUILD_DIR/build.zig.zon")"
[[ -n "$zmx_ghostty_rev" && -n "$zmx_ghostty_hash" ]] || die "no ghostty revision and hash in zmx's build.zig.zon"
echo "fetching zmx's ghostty $zmx_ghostty_rev..." >&2
fetched="$(fetch_package "git+https://github.com/ghostty-org/ghostty#$zmx_ghostty_rev" "$ZMX_GHOSTTY")"
[[ "$fetched" == "$zmx_ghostty_hash" ]] || die "zmx's ghostty fetched as $fetched, pinned as $zmx_ghostty_hash"

for patch in "$GHOSTTY_PATCHES"/*.patch; do
  echo "applying ghostty/$(basename "$patch")..." >&2
  patch -d "$ZMX_GHOSTTY" -p1 --batch --forward --fuzz=0 --no-backup-if-mismatch --quiet <"$patch"
done
for patch in "$PATCHES"/*.patch; do
  echo "applying $(basename "$patch")..." >&2
  patch -d "$BUILD_DIR" -p1 --batch --forward --fuzz=0 --no-backup-if-mismatch --quiet <"$patch"
done

# ReleaseSafe as upstream's setup.sh (the fork builds ReleaseFast): the daemon outlives the app, and a safety
# check that stops it is better than memory corruption inside a session that holds the user's work.
echo "building zmx with zig $ZMX_ZIG_VERSION..." >&2
(
  cd "$BUILD_DIR"
  "$ZIG" build -Doptimize=ReleaseSafe -Dtarget="$TARGET"
) >&2

install -Dm755 "$BUILD_DIR/zig-out/bin/zmx" "$STAGE/bin/zmx"
install -Dm644 "$BUILD_DIR/LICENSE" "$STAGE/share/doc/zmx/LICENSE"
mkdir -p "$STAGE/share/agterm"
printf '%s\n' "$BUILD_ID" >"$STAGE/share/agterm/zmx-BUILD"
"$STAGE/bin/zmx" version >/dev/null || die "the built zmx does not run"

chmod 755 "$STAGE"
rm -rf "$PREFIX"
mv "$STAGE" "$PREFIX"
echo "→ vendored zmx $BUILD_ID into $PREFIX" >&2
echo "$PREFIX"
