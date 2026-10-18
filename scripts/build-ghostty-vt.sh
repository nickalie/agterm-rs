#!/usr/bin/env bash
# Prepare the pinned, patched libghostty-vt source (Ghostty's VT library behind its official C API,
# include/ghostty/vt.h) and Ghostty's resources at the same revision in the vendored prefix
# ${AGTERM_VENDOR:-~/.cache/agterm-rs/vendor}/ghostty-vt-<key>/. The library itself is built by
# agterm-ghostty-vt-sys's build script (`zig build -Demit-lib-vt` over this prefix's tree, for cargo's target, into its
# OUT_DIR), which never reaches the network: everything that needs it or the shell happens here. The key hashes the
# LIBGHOSTTY_VT_ pins of build/pins.env (the one Ghostty pin), build/patches/ghostty-vt/, the prefix layout and the
# host arch, so a zmx pin never moves it and a complete prefix is reused as is.
#
# The patches (build/patches/ghostty-vt/*.patch, applied in name order with no fuzz) add what the app needs and the C
# API at the pin lacks; each names what it adds in its header. A patch change regenerates the bindings
# (scripts/gen-ghostty-vt-bindings.sh), like a pin bump.
#
# The prefix holds:
# - src/: the patched source tree, with the Zig packages a libghostty-vt build needs prefetched into its zig-pkg/ for
#   every target the build script maps (ZIG_TARGETS below), so `zig build` there needs no network;
# - include/ghostty/: the headers, copied from the tree (what its install step installs), for the bindings generator;
# - share/doc/ghostty-vt/LICENSE, share/agterm/ghostty-vt-PINS (the LIBGHOSTTY_VT_ pin lines it was prepared from);
# - share/agterm/ghostty-vt-STAMP: its inputs as `name=value` lines, which the build script reads: the pinned zig it
#   was prepared with (`zig=`, the real binary, never a version manager's shim), that zig's version, the Zig global
#   cache (`zig-global-cache=`, scripts/lib/native.sh ghostty_vt_zig_cache), the version string the build passes
#   (`version-string=`, the tree's build.zig.zon version, so the build never asks git) and each patch with its sha256;
# - the resources scripts/stage.sh stages, as Ghostty's own build installs them (src/build/GhosttyResources.zig):
#   share/ghostty/shell-integration from the source, share/ghostty/themes from the theme package the revision's
#   build.zig.zon declares (LIBGHOSTTY_VT_THEMES_*, its sha256 checked), and share/terminfo, the xterm-ghostty entry
#   Ghostty's terminfo source (src/terminfo/ghostty.zig) encodes, compiled with `tic`.
#
# Usage: scripts/build-ghostty-vt.sh                 prepare the prefix unless it is complete, then print its path
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
# The Zig triples of agterm-ghostty-vt-sys's target map (crates/agterm-ghostty-vt-sys/zig_build.rs ZIG_TARGETS, which a
# test keeps equal to this list), without their OS version suffixes: the packages each one's build needs are fetched.
ZIG_TARGETS=(x86_64-linux-gnu aarch64-linux-gnu x86_64-linux-musl aarch64-linux-musl x86_64-macos aarch64-macos
  x86_64-windows-msvc aarch64-windows-msvc)

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
  command -v "$command" >/dev/null || die "$command is required to prepare libghostty-vt and its resources"
done
mkdir -p "$VENDOR_ROOT"
ARCH="$(host_zig_arch)"
ZIG_SHA_VAR="LIBGHOSTTY_VT_ZIG_SHA256_${ARCH^^}"
ZIG="$(find_zig "$LIBGHOSTTY_VT_ZIG_VERSION" "${!ZIG_SHA_VAR}" "${AGTERM_GHOSTTY_VT_ZIG:-}")"
# The binary itself, as zig reports it: a version manager's shim found on PATH may resolve to another version from the
# directory cargo runs the build script in.
ZIG_EXE="$("$ZIG" env | sed -n 's/^ *\.zig_exe = "\(.*\)",$/\1/p')"
[[ -n "$ZIG_EXE" && -x "$ZIG_EXE" && "$("$ZIG_EXE" version)" == "$LIBGHOSTTY_VT_ZIG_VERSION" ]] \
  || die "$ZIG does not report its own binary (zig env .zig_exe)"
ZIG_CACHE="$(ghostty_vt_zig_cache)"
mkdir -p "$ZIG_CACHE"

# both scratch dirs live in the vendor root, beside the final prefix, not in a shared /tmp
BUILD_DIR="$(mktemp -d "$VENDOR_ROOT/.ghostty-vt-build.XXXXXX")"
# the stage becomes the prefix in one atomic rename
STAGE="$(mktemp -d "$VENDOR_ROOT/.ghostty-vt-stage.XXXXXX")"
trap 'rm -rf "$BUILD_DIR" "$STAGE"' EXIT
TREE="$STAGE/src"

# The source archive of the full commit id, verified against its pinned sha256 before anything is unpacked: the
# build needs no git.
echo "fetching ghostty $LIBGHOSTTY_VT_REV for libghostty-vt..." >&2
curl -fsSL --retry 3 -o "$BUILD_DIR/source.tar.gz" "$LIBGHOSTTY_VT_REPO/archive/$LIBGHOSTTY_VT_REV.tar.gz"
echo "$LIBGHOSTTY_VT_SHA256  $BUILD_DIR/source.tar.gz" | sha256sum --check --quiet >&2 \
  || die "ghostty $LIBGHOSTTY_VT_REV source archive sha256 mismatch"
mkdir "$TREE"
tar -xzf "$BUILD_DIR/source.tar.gz" -C "$TREE" --strip-components=1

for patch in "$PATCHES"/*.patch; do
  [[ -f "$patch" ]] || continue
  echo "applying $(basename "$patch")..." >&2
  patch -d "$TREE" -p1 --batch --forward --fuzz=0 --no-backup-if-mismatch --quiet <"$patch" \
    || die "$(basename "$patch") does not apply to ghostty $LIBGHOSTTY_VT_REV"
done

# The packages: Zig fetches a lazy dependency only when a build's configure step asks for it, so each mapped target's
# libghostty-vt configuration is run once (listing its steps builds nothing), which fetches what that target's build
# needs into the tree's zig-pkg/. The local cache is scratch: the tree is left as the build script will read it.
echo "fetching libghostty-vt's Zig packages with zig $LIBGHOSTTY_VT_ZIG_VERSION..." >&2
for target in "${ZIG_TARGETS[@]}"; do
  (
    cd "$TREE"
    "$ZIG_EXE" build --list-steps --cache-dir "$BUILD_DIR/zig-cache" --global-cache-dir "$ZIG_CACHE" \
      -Demit-lib-vt=true -Dtarget="$target" >/dev/null
  ) || die "cannot fetch the Zig packages of libghostty-vt for $target"
done
[[ -d "$TREE/zig-pkg" ]] || die "the libghostty-vt configuration fetched no Zig packages"

VERSION_STRING="$(sed -n 's/^ *\.version = "\([^"]*\)",$/\1/p' "$TREE/build.zig.zon" | head -n1)"
[[ -n "$VERSION_STRING" ]] || die "no .version in ghostty $LIBGHOSTTY_VT_REV's build.zig.zon"
mkdir -p "$STAGE/include" "$STAGE/share/doc/ghostty-vt" "$STAGE/share/agterm"
cp -R "$TREE/include/ghostty" "$STAGE/include/ghostty"
cp "$TREE/LICENSE" "$STAGE/share/doc/ghostty-vt/LICENSE"
pin_lines LIBGHOSTTY_VT >"$STAGE/share/agterm/ghostty-vt-PINS"

# The resources (the header names them). The vt build installs none of them, so they are taken as Ghostty's full build
# takes them: the shell integration as is, the themes from the package its build.zig.zon pins, the terminfo encoded by
# the revision's own Zig and compiled here.
mkdir -p "$STAGE/share/ghostty/themes"
cp -R "$TREE/src/shell-integration" "$STAGE/share/ghostty/shell-integration"
grep -F "$LIBGHOSTTY_VT_THEMES_URL" "$TREE/build.zig.zon" >/dev/null \
  || die "ghostty $LIBGHOSTTY_VT_REV no longer declares the pinned theme package (LIBGHOSTTY_VT_THEMES_URL)"
echo "fetching the pinned Ghostty theme package..." >&2
curl -fsSL --retry 3 -o "$BUILD_DIR/ghostty-themes.tgz" "$LIBGHOSTTY_VT_THEMES_URL"
echo "$LIBGHOSTTY_VT_THEMES_SHA256  $BUILD_DIR/ghostty-themes.tgz" | sha256sum --check --quiet >&2 \
  || die "Ghostty theme package sha256 mismatch"
tar -xzf "$BUILD_DIR/ghostty-themes.tgz" -C "$STAGE/share/ghostty/themes" --strip-components=1
pin_lines LIBGHOSTTY_VT >"$STAGE/share/ghostty/themes/.agterm-resource-manifest"

# What Ghostty's build-data tool runs for `+terminfo` (src/main_build_data.zig), alone: the entry's encoder needs
# nothing but std, so this needs none of the build's dependencies. The helper sits in the tree only while it runs.
cat >"$TREE/src/agterm_terminfo.zig" <<'ZIG'
const std = @import("std");

pub fn main(init: std.process.Init) !void {
    var buffer: [1024]u8 = undefined;
    var stdout_writer = std.Io.File.stdout().writerStreaming(init.io, &buffer);
    try @import("terminfo/ghostty.zig").ghostty.encode(&stdout_writer.interface);
    try stdout_writer.end();
}
ZIG
echo "encoding Ghostty's terminfo..." >&2
(
  cd "$TREE"
  "$ZIG_EXE" run --cache-dir "$BUILD_DIR/zig-cache" --global-cache-dir "$ZIG_CACHE" src/agterm_terminfo.zig
) >"$BUILD_DIR/ghostty.terminfo"
rm "$TREE/src/agterm_terminfo.zig"
[[ -s "$BUILD_DIR/ghostty.terminfo" ]] || die "Ghostty's terminfo encoder wrote nothing"
tic -x -o "$STAGE/share/terminfo" "$BUILD_DIR/ghostty.terminfo"

{
  echo "# The inputs of this libghostty-vt prefix (scripts/build-ghostty-vt.sh); agterm-ghostty-vt-sys builds from it."
  echo "zig=$ZIG_EXE"
  echo "zig-version=$LIBGHOSTTY_VT_ZIG_VERSION"
  echo "zig-global-cache=$ZIG_CACHE"
  echo "version-string=$VERSION_STRING"
  for patch in "$PATCHES"/*.patch; do
    [[ -f "$patch" ]] || continue
    echo "patch=$(basename "$patch") $(sha256_of <"$patch")"
  done
} >"$STAGE/share/agterm/ghostty-vt-STAMP"

ghostty_vt_prefix_is_complete "$STAGE" || die "the staged libghostty-vt prefix is incomplete"
chmod 755 "$STAGE"
rm -rf "$PREFIX"
mv "$STAGE" "$PREFIX"
echo "→ vendored the libghostty-vt source into $PREFIX" >&2
echo "$PREFIX"
