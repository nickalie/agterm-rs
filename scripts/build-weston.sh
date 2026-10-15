#!/usr/bin/env bash
# Build the pinned weston (WESTON_* of build/pins.env) into the vendored prefix
# ${AGTERM_VENDOR:-~/.cache/agterm-rs/vendor}/weston-<key>/, for hosts whose weston predates 15.0: the GPUI build needs
# a `wl_seat`, which headless weston offers only with `--fake-seat`, new in 15.0 (Ubuntu 24.04 ships 13). The key
# hashes the WESTON_ pins and the host arch, so a complete prefix is reused as is. A system weston that takes
# --fake-seat needs none of this.
#
# Only what the suite's headless instances use is built: the headless backend, the GL renderer, the desktop shell (its
# module, helper client and weston-screenshooter), no other backend, shell, Xwayland, remoting, clients or tests. The
# two dependencies Ubuntu 24.04 has too old, wayland-protocols and libdisplay-info, are unpacked from their pinned
# tarballs into the source's subprojects/ and built in; meson may download nothing. Needs no root: meson, ninja, a C
# compiler, python3 and the development files of wayland, pixman, xkbcommon, libinput, libevdev, libudev, libdrm, EGL,
# GLES, gbm, cairo, pango and libpng (Ubuntu: see .github/workflows/rust-linux.yml).
#
# Usage: scripts/build-weston.sh                 build the prefix unless it is complete, then print its path
#        scripts/build-weston.sh --print-prefix  print the prefix path only
#        scripts/build-weston.sh --verify        exit 0 when the prefix is complete, 1 otherwise
set -euo pipefail

# shellcheck source=lib/native.sh
source "$(dirname "$0")/lib/native.sh"

PREFIX="$(weston_prefix)"

case "${1:-}" in
  --print-prefix)
    echo "$PREFIX"
    exit 0
    ;;
  --verify)
    weston_prefix_is_complete "$PREFIX"
    exit
    ;;
  "") ;;
  *) die "usage: scripts/build-weston.sh [--print-prefix | --verify]" ;;
esac

if weston_prefix_is_complete "$PREFIX"; then
  echo "complete weston prefix already vendored at $PREFIX" >&2
  echo "$PREFIX"
  exit 0
fi

for command in curl tar xz sha256sum meson ninja python3; do
  command -v "$command" >/dev/null || die "$command is required to build weston"
done
mkdir -p "$VENDOR_ROOT"

# both scratch dirs live in the vendor root, beside the final prefix, not in a shared /tmp
BUILD_DIR="$(mktemp -d "$VENDOR_ROOT/.weston-build.XXXXXX")"
# the DESTDIR install root: $STAGE$PREFIX becomes the prefix in one atomic rename
STAGE="$(mktemp -d "$VENDOR_ROOT/.weston-stage.XXXXXX")"
trap 'rm -rf "$BUILD_DIR" "$STAGE"' EXIT

# fetch NAME URL SHA256 DIR: the verified tarball unpacked into DIR
fetch() {
  local name="$1" url="$2" sha="$3" dir="$4"
  echo "fetching $name..." >&2
  curl -fsSLo "$BUILD_DIR/$name.tar.xz" "$url"
  echo "$sha  $BUILD_DIR/$name.tar.xz" | sha256sum --check --quiet >&2 || die "$name sha256 mismatch ($url)"
  mkdir -p "$dir"
  tar -xJf "$BUILD_DIR/$name.tar.xz" -C "$dir" --strip-components=1
}

fetch "weston-$WESTON_VERSION" "$WESTON_URL" "$WESTON_SHA256" "$BUILD_DIR/source"
# weston's protocol/meson.build falls back to the `wayland-protocols` subproject, its libdisplay-info dependency to
# `display-info`: a directory of that name in subprojects/ wins over the wrap file, so meson fetches nothing
fetch "wayland-protocols-$WESTON_WAYLAND_PROTOCOLS_VERSION" "$WESTON_WAYLAND_PROTOCOLS_URL" \
  "$WESTON_WAYLAND_PROTOCOLS_SHA256" "$BUILD_DIR/source/subprojects/wayland-protocols"
rm -f "$BUILD_DIR/source/subprojects/display-info.wrap"
fetch "libdisplay-info-$WESTON_DISPLAY_INFO_VERSION" "$WESTON_DISPLAY_INFO_URL" "$WESTON_DISPLAY_INFO_SHA256" \
  "$BUILD_DIR/source/subprojects/display-info"

# Configured with the final prefix, installed through DESTDIR. The compositor finds libweston (and the subproject's
# libdisplay-info) in the prefix's lib/ through an rpath; its modules carry their own.
echo "building weston $WESTON_VERSION..." >&2
LDFLAGS="${LDFLAGS:-} -Wl,-rpath,$PREFIX/lib" meson setup "$BUILD_DIR/out" "$BUILD_DIR/source" \
  --prefix="$PREFIX" --libdir=lib --buildtype=release --wrap-mode=nodownload \
  -Dbackend-default=headless -Dbackend-headless=true -Dbackend-drm=false \
  -Ddeprecated-backend-drm-screencast-vaapi=false -Dbackend-pipewire=false -Dbackend-rdp=false \
  -Ddeprecated-screenshare=false -Dbackend-vnc=false -Dbackend-wayland=false -Dbackend-x11=false \
  -Drenderer-gl=true -Drenderer-vulkan=false -Dxwayland=false -Dsystemd=false -Dremoting=false -Dpipewire=false \
  -Dshell-desktop=true -Ddeprecated-shell-fullscreen=false -Dshell-ivi=false -Dshell-kiosk=false -Dshell-lua=false \
  -Dcolor-management-lcms=false -Dimage-jpeg=false -Dimage-webp=false -Dtools=[] -Ddemo-clients=false \
  -Dsimple-clients=[] -Dwcap-decode=false -Dtests=false -Ddoc=false -Dperfetto=false \
  -Dwayland-protocols:tests=false -Ddisplay-info:werror=false >&2
ninja -C "$BUILD_DIR/out" >&2
DESTDIR="$STAGE" meson install -C "$BUILD_DIR/out" --no-rebuild --quiet >&2
mkdir -p "$STAGE$PREFIX/share/agterm"
pin_lines WESTON >"$STAGE$PREFIX/share/agterm/weston-PINS"

# Its compiled-in paths (rpath, modules, the shell helper) name the final prefix, so it is checked once there; an
# incomplete one is removed again, never left for a later run to reuse.
chmod 755 "$STAGE$PREFIX"
rm -rf "$PREFIX"
mv "$STAGE$PREFIX" "$PREFIX"
missing="$(unresolved_libraries "$PREFIX/bin/weston")"
[[ -z "$missing" ]] || { rm -rf "$PREFIX"; die "the built weston needs libraries this host lacks: $missing"; }
weston_prefix_is_complete "$PREFIX" || { rm -rf "$PREFIX"; die "the weston install is incomplete in $PREFIX"; }
echo "→ vendored weston $WESTON_VERSION into $PREFIX" >&2
echo "$PREFIX"
