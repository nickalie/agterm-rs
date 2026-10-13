#!/usr/bin/env bash
# Assemble the staging layout the conformance runner, scripts/run-dev.sh and the install check run from:
#
#   bin/{agterm,agtermctl,zmx}  lib/libghostty.so  share/ghostty/{themes,shell-integration}  share/terminfo
#
# from cargo's target dir and the vendored prefixes, plus agterm's own themes from build/themes. Binaries are
# staged when they exist (agterm and agtermctl appear with their crates, zmx with scripts/build-zmx.sh); libghostty
# and its resources are required. The binaries find lib/ through their $ORIGIN/../lib rpath.
#
# Usage: scripts/stage.sh DESTINATION [--profile release|debug]   (default: release)
set -euo pipefail

# shellcheck source=lib/native.sh
source "$(dirname "$0")/lib/native.sh"

usage() { die "usage: scripts/stage.sh DESTINATION [--profile release|debug]"; }

(($# == 1 || $# == 3)) || usage
DEST="$1"
PROFILE=release
if (($# == 3)); then
  [[ "$2" == --profile && ("$3" == release || "$3" == debug) ]] || usage
  PROFILE="$3"
fi
[[ "$DEST" = /* ]] || DEST="$PWD/$DEST"
if [[ -d "$DEST" ]] && find "$DEST" -mindepth 1 -print -quit | grep -q .; then
  die "staging destination is not empty: $DEST"
fi

GHOSTTY="$(ghostty_prefix)"
ghostty_prefix_is_complete "$GHOSTTY" || die "no complete libghostty prefix at $GHOSTTY: run scripts/build-ghostty.sh"
TARGET="${CARGO_TARGET_DIR:-$NATIVE_ROOT/target}/$PROFILE"

mkdir -p "$DEST/bin" "$DEST/lib" "$DEST/share/ghostty"
install -m644 "$GHOSTTY/lib/libghostty.so" "$DEST/lib/libghostty.so"
cp -R "$GHOSTTY/share/ghostty/themes" "$GHOSTTY/share/ghostty/shell-integration" "$DEST/share/ghostty/"
cp -R "$GHOSTTY/share/terminfo" "$DEST/share/terminfo"

staged=()
for binary in agterm agtermctl; do
  if [[ -x "$TARGET/$binary" ]]; then
    install -m755 "$TARGET/$binary" "$DEST/bin/$binary"
    staged+=("$binary")
  fi
done
if [[ -x "$NATIVE_ROOT/scripts/build-zmx.sh" ]]; then
  ZMX="$("$NATIVE_ROOT/scripts/build-zmx.sh" --print-prefix)/bin/zmx"
  if [[ -x "$ZMX" ]]; then
    install -m755 "$ZMX" "$DEST/bin/zmx"
    staged+=(zmx)
  fi
fi

verify_ghostty_resources "$DEST/share" >&2
# after the check of Ghostty's own catalog: agterm's bundled theme(s), the seeded default `agterm` among them
# (upstream's stage_custom_themes)
install -m644 "$NATIVE_ROOT"/build/themes/* "$DEST/share/ghostty/themes/"
echo "→ staged ${staged[*]:-no binaries} + libghostty ($PROFILE) into $DEST" >&2
