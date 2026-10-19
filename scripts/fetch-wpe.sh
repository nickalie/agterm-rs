#!/usr/bin/env bash
# Unpack the pinned WPE WebKit (Arch's `wpewebkit` binary package, WPE_ARCH_PACKAGE_URL of build/pins.env) into the
# vendored prefix ${AGTERM_VENDOR:-~/.cache/agterm-rs/vendor}/wpe-<key>/, for a machine with an Arch-compatible
# userland and no system wpewebkit (no root needed). The key hashes the WPE_ pins and the host arch only. Nothing is
# built: the package is verified against its pinned sha256 and unpacked, its pkg-config files are relocated to the
# prefix, and scripts/pkg-config.sh then puts the prefix on PKG_CONFIG_PATH unless a system wpe-webkit-2.0 at least
# WPE_API_VERSION exists (the system package wins). A distro's wpewebkit is the way to run agterm; this prefix is for
# building, linking and testing the bindings (scripts/gen-wpe-bindings.sh reads its .gir files).
#
# The prefix holds lib/ (libWPEWebKit-2.0.so*, the helper processes in lib/wpe-webkit-2.0/, pkgconfig/), include/,
# share/gir-1.0/ and share/ngterm/wpe-PINS (the WPE_ pin lines it was unpacked from). A distro build has its helper
# path compiled in (/usr/lib/wpe-webkit-2.0), so pages run from this prefix only under
# `bwrap --ro-overlay /usr/lib` with the prefix's lib/ overlaid; linking and version checks need nothing more than the
# prefix.
#
# Usage: scripts/fetch-wpe.sh                 unpack the prefix unless it is complete, then print its path
#        scripts/fetch-wpe.sh --print-prefix  print the prefix path only
#        scripts/fetch-wpe.sh --verify        exit 0 when the prefix is complete, 1 otherwise
set -euo pipefail

# shellcheck source=lib/native.sh
source "$(dirname "$0")/lib/native.sh"

PREFIX="$(wpe_prefix)"

case "${1:-}" in
  --print-prefix)
    echo "$PREFIX"
    exit 0
    ;;
  --verify)
    wpe_prefix_is_complete "$PREFIX"
    exit
    ;;
  "") ;;
  *) die "usage: scripts/fetch-wpe.sh [--print-prefix | --verify]" ;;
esac

if wpe_prefix_is_complete "$PREFIX"; then
  echo "complete WPE WebKit prefix already vendored at $PREFIX" >&2
  echo "$PREFIX"
  exit 0
fi

[[ "$(uname -m)" == x86_64 ]] || die "the pinned WPE WebKit package is x86_64 only: install the distro's wpewebkit"
for command in curl tar zstd sha256sum getconf ldd; do
  command -v "$command" >/dev/null || die "$command is required to unpack WPE WebKit"
done
# The package is built against Arch's glibc; an older one cannot load it, so refuse before downloading.
glibc="$(getconf GNU_LIBC_VERSION | awk '{print $2}')"
[[ "$(printf '%s\n%s\n' "$WPE_ARCH_GLIBC" "$glibc" | sort -V | head -n1)" == "$WPE_ARCH_GLIBC" ]] \
  || die "the pinned WPE WebKit package needs glibc $WPE_ARCH_GLIBC, this host has $glibc: install the distro's" \
    "wpewebkit >= $WPE_API_VERSION instead (README > WPE WebKit)"

mkdir -p "$VENDOR_ROOT"
# both scratch dirs live in the vendor root, beside the final prefix, not in a shared /tmp
WORK="$(mktemp -d "$VENDOR_ROOT/.wpe-fetch.XXXXXX")"
# the stage becomes the prefix in one atomic rename
STAGE="$(mktemp -d "$VENDOR_ROOT/.wpe-stage.XXXXXX")"
trap 'rm -rf "$WORK" "$STAGE"' EXIT

echo "fetching WPE WebKit $WPE_VERSION..." >&2
curl -fsSLo "$WORK/wpewebkit.pkg.tar.zst" "$WPE_ARCH_PACKAGE_URL"
echo "$WPE_ARCH_PACKAGE_SHA256  $WORK/wpewebkit.pkg.tar.zst" | sha256sum --check --quiet >&2 \
  || die "WPE WebKit package sha256 mismatch ($WPE_ARCH_PACKAGE_URL)"
mkdir "$WORK/pkg"
tar --zstd -xf "$WORK/wpewebkit.pkg.tar.zst" -C "$WORK/pkg" usr
for dir in lib include share; do
  mv "$WORK/pkg/usr/$dir" "$STAGE/$dir"
done
# Relocate the pkg-config files from /usr to the final prefix (the stage is renamed into it below).
for pc in "$STAGE"/lib/pkgconfig/*.pc; do
  sed -i -e "s|^prefix=/usr\$|prefix=$PREFIX|" -e 's|^libdir=/usr/lib|libdir=${prefix}/lib|' \
    -e 's|^moduledir=/usr/lib|moduledir=${prefix}/lib|' "$pc"
  if grep -E '^[a-z_]+=/usr' "$pc" >&2; then
    die "$(basename "$pc") still points into /usr after relocation"
  fi
done
# Every library the engine needs must resolve on this host (the glibc check above is only the first of them).
missing="$(ldd "$STAGE/lib/libWPEWebKit-2.0.so.1" | awk '/not found/ {print $1}' | sort -u | tr '\n' ' ')"
[[ -z "$missing" ]] || die "WPE WebKit needs libraries this host lacks: ${missing% }; install them (Arch: the" \
  "wpewebkit package's dependencies) or the distro's wpewebkit"
mkdir -p "$STAGE/share/ngterm"
pin_lines WPE >"$STAGE/share/ngterm/wpe-PINS"

wpe_prefix_is_complete "$STAGE" || die "the staged WPE WebKit prefix is incomplete"
chmod 755 "$STAGE"
rm -rf "$PREFIX"
mv "$STAGE" "$PREFIX"
echo "→ vendored WPE WebKit $WPE_VERSION into $PREFIX" >&2
echo "$PREFIX"
