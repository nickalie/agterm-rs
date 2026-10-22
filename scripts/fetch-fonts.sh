#!/usr/bin/env bash
# Unpack the fonts Ghostty embeds (FONTS_* of build/pins.env: JetBrains Mono 2.304 and Symbols Nerd Font 3.4.0, the
# package deps of the Ghostty at LIBGHOSTTY_VT_REV) into the vendored prefix
# ${NGTERM_VENDOR:-~/.cache/ngterm/vendor}/fonts-<key>/, for ngterm-gpui to embed: its build script asks
# `--print-prefix` and fails with a pointer here when the prefix is incomplete. The key hashes the FONTS_ pins only.
# Nothing is built: each archive is verified against its pinned sha256 and the faces are copied out.
#
# The prefix holds share/fonts/ (JetBrains Mono's static Regular, Bold, Italic and BoldItalic faces, and
# SymbolsNerdFontMono-Regular.ttf, the one-cell variant of the symbols font), share/doc/fonts/ (JetBrains Mono's SIL
# Open Font License and Nerd Fonts' MIT license) and share/ngterm/fonts-PINS (the FONTS_ pin lines it came from).
#
# Usage: scripts/fetch-fonts.sh                 unpack the prefix unless it is complete, then print its path
#        scripts/fetch-fonts.sh --print-prefix  print the prefix path only (ngterm-gpui's build script)
#        scripts/fetch-fonts.sh --verify        exit 0 when the prefix is complete, 1 otherwise
set -euo pipefail

# shellcheck source=lib/native.sh
source "$(dirname "$0")/lib/native.sh"

PREFIX="$(fonts_prefix)"

case "${1:-}" in
  --print-prefix)
    echo "$PREFIX"
    exit 0
    ;;
  --verify)
    fonts_prefix_is_complete "$PREFIX"
    exit
    ;;
  "") ;;
  *) die "usage: scripts/fetch-fonts.sh [--print-prefix | --verify]" ;;
esac

if fonts_prefix_is_complete "$PREFIX"; then
  echo "complete terminal fonts prefix already vendored at $PREFIX" >&2
  echo "$PREFIX"
  exit 0
fi

for command in curl tar sha256sum; do
  command -v "$command" >/dev/null || die "$command is required to fetch the terminal fonts"
done
mkdir -p "$VENDOR_ROOT"
# both scratch dirs live in the vendor root, beside the final prefix, not in a shared /tmp
WORK="$(mktemp -d "$VENDOR_ROOT/.fonts-fetch.XXXXXX")"
# the stage becomes the prefix in one atomic rename
STAGE="$(mktemp -d "$VENDOR_ROOT/.fonts-stage.XXXXXX")"
trap 'rm -rf "$WORK" "$STAGE"' EXIT

# Download $2 into $WORK/$1.tar.gz, check it against sha256 $3 and unpack it into $WORK/$1/.
fetch() {
  local name="$1" url="$2" sha="$3"
  echo "fetching $url..." >&2
  curl -fsSL -A ngterm-dev -o "$WORK/$name.tar.gz" "$url"
  echo "$sha  $WORK/$name.tar.gz" | sha256sum --check --quiet >&2 || die "$url: sha256 mismatch"
  mkdir "$WORK/$name"
  # the archives carry macOS extended attributes GNU tar warns about
  tar -xzf "$WORK/$name.tar.gz" -C "$WORK/$name" --warning=no-unknown-keyword
}

fetch jetbrains "$FONTS_JETBRAINS_MONO_URL" "$FONTS_JETBRAINS_MONO_SHA256"
fetch symbols "$FONTS_NERD_SYMBOLS_URL" "$FONTS_NERD_SYMBOLS_SHA256"

mkdir -p "$STAGE/share/fonts" "$STAGE/share/doc/fonts" "$STAGE/share/ngterm"
for face in Regular Bold Italic BoldItalic; do
  cp "$WORK/jetbrains/fonts/ttf/JetBrainsMono-$face.ttf" "$STAGE/share/fonts/"
done
cp "$WORK/symbols/SymbolsNerdFontMono-Regular.ttf" "$STAGE/share/fonts/"
cp "$WORK/jetbrains/OFL.txt" "$STAGE/share/doc/fonts/JetBrainsMono-OFL.txt"
cp "$WORK/symbols/LICENSE" "$STAGE/share/doc/fonts/SymbolsNerdFont-LICENSE"
pin_lines FONTS >"$STAGE/share/ngterm/fonts-PINS"
fonts_prefix_is_complete "$STAGE" || die "the font archives did not hold the expected files"

rm -rf "$PREFIX"
mv "$STAGE" "$PREFIX"
echo "terminal fonts vendored at $PREFIX" >&2
echo "$PREFIX"
