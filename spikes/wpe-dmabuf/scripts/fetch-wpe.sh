#!/usr/bin/env bash
# Downloads Arch's wpewebkit package (WPE WebKit with WPEPlatform), checks its signature against the pacman keyring
# and extracts it into .cache/prefix. No root: nothing is installed.
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
pkg="${WPE_PKG:-wpewebkit-2.54.1-2-x86_64.pkg.tar.zst}"
mirror="${WPE_MIRROR:-https://archlinux.cachyos.org/repo/extra/os/x86_64}"
mkdir -p "$here/.cache/dl" "$here/.cache/prefix"
cd "$here/.cache/dl"
[[ -f "$pkg" ]] || curl -sSfLO "$mirror/$pkg"
[[ -f "$pkg.sig" ]] || curl -sSfLO "$mirror/$pkg.sig"
gpg --homedir /etc/pacman.d/gnupg --verify "$pkg.sig" "$pkg"
tar -C "$here/.cache/prefix" -xf "$pkg"
