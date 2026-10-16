#!/usr/bin/env bash
# Assemble the staging layout the conformance runner, scripts/run-{gtk,gpui}-dev.sh and the install check run from.
# The GTK frontend's (the default):
#
#   bin/{agterm,agtermctl,zmx}  lib/{libghostty.so,libgtk4-layer-shell.so.*}  share/ghostty/{themes,shell-integration}
#   share/terminfo  share/agterm/zmx-BUILD (the staged zmx's build id, with bin/zmx)  share/agterm/{agent-status,skills/agterm}
#
# from cargo's target dir and the vendored prefixes, plus agterm's own themes from build/themes and the agent
# integrations' payloads from resources/. Binaries are staged when they exist (agterm and agtermctl appear with their
# crates, zmx once scripts/build-zmx.sh filled its prefix); libghostty and its resources are required. gtk4-layer-shell
# is staged from wherever the built agterm resolves it when that is a vendored prefix (scripts/build-layer-shell.sh); a
# system package stays the system's. The binaries find lib/ through their $ORIGIN/../lib rpath.
#
# `--frontend gpui` stages the GPUI build instead, while both frontends coexist: bin/agterm-gpui in place of bin/agterm
# and no lib/ (libghostty-vt is linked statically; GPUI loads neither libghostty nor gtk4-layer-shell), with the same
# agtermctl, zmx and share/ (terminfo, themes and shell integration keep coming from the libghostty prefix at
# GHOSTTY_REV until the switch).
#
# Usage: scripts/stage.sh DESTINATION [--profile release|debug] [--frontend gtk|gpui]   (default: release, gtk)
set -euo pipefail

# shellcheck source=lib/native.sh
source "$(dirname "$0")/lib/native.sh"

usage() { die "usage: scripts/stage.sh DESTINATION [--profile release|debug] [--frontend gtk|gpui]"; }

(($# >= 1)) || usage
DEST="$1"
shift
PROFILE=release
FRONTEND=gtk
while (($#)); do
  (($# >= 2)) || usage
  case "$1" in
    --profile)
      [[ "$2" == release || "$2" == debug ]] || usage
      PROFILE="$2"
      ;;
    --frontend)
      [[ "$2" == gtk || "$2" == gpui ]] || usage
      FRONTEND="$2"
      ;;
    *) usage ;;
  esac
  shift 2
done
# the frontend's app binary
APP=agterm
[[ "$FRONTEND" == gtk ]] || APP=agterm-gpui
[[ "$DEST" = /* ]] || DEST="$PWD/$DEST"
if [[ -d "$DEST" ]] && find "$DEST" -mindepth 1 -print -quit | grep -q .; then
  die "staging destination is not empty: $DEST"
fi

GHOSTTY="$(ghostty_prefix)"
ghostty_prefix_is_complete "$GHOSTTY" || die "no complete libghostty prefix at $GHOSTTY: run scripts/build-ghostty.sh"
TARGET="${CARGO_TARGET_DIR:-$NATIVE_ROOT/target}/$PROFILE"

mkdir -p "$DEST/bin" "$DEST/share/ghostty"
if [[ "$FRONTEND" == gtk ]]; then
  mkdir -p "$DEST/lib"
  install -m644 "$GHOSTTY/lib/libghostty.so" "$DEST/lib/libghostty.so"
fi
cp -R "$GHOSTTY/share/ghostty/themes" "$GHOSTTY/share/ghostty/shell-integration" "$DEST/share/ghostty/"
cp -R "$GHOSTTY/share/terminfo" "$DEST/share/terminfo"

staged=()
for binary in "$APP" agtermctl; do
  if [[ -x "$TARGET/$binary" ]]; then
    install -m755 "$TARGET/$binary" "$DEST/bin/$binary"
    staged+=("$binary")
  fi
done
if [[ -x "$NATIVE_ROOT/scripts/build-zmx.sh" ]]; then
  ZMX_PREFIX="$("$NATIVE_ROOT/scripts/build-zmx.sh" --print-prefix)"
  # only a complete prefix: a zmx without its build id would read as one predating every Live sessions update
  if "$NATIVE_ROOT/scripts/build-zmx.sh" --verify; then
    install -m755 "$ZMX_PREFIX/bin/zmx" "$DEST/bin/zmx"
    install -Dm644 "$ZMX_PREFIX/share/agterm/zmx-BUILD" "$DEST/share/agterm/zmx-BUILD"
    staged+=(zmx)
  fi
fi

if [[ "$FRONTEND" == gtk && -x "$DEST/bin/agterm" ]]; then
  layer_shell="$(ldd "$DEST/bin/agterm" | awk '$1 ~ /^libgtk4-layer-shell\.so/ { print $3 }')" || layer_shell=""
  if [[ "$layer_shell" == "$VENDOR_ROOT"/* ]]; then
    cp -P "$(dirname "$layer_shell")"/libgtk4-layer-shell.so.* "$DEST/lib/"
  fi
fi

verify_ghostty_resources "$DEST/share" >&2
# after the check of Ghostty's own catalog: agterm's bundled theme(s), the seeded default `agterm` among them
# (upstream's stage_custom_themes)
install -m644 "$NATIVE_ROOT"/build/themes/* "$DEST/share/ghostty/themes/"
# the agent integrations' payloads (upstream's agent-status package and agent skill, see resources/): the Help menu
# installers copy them from share/agterm
mkdir -p "$DEST/share/agterm/skills"
cp -R "$NATIVE_ROOT/resources/agent-status" "$DEST/share/agterm/agent-status"
cp -R "$NATIVE_ROOT/resources/skills/agterm" "$DEST/share/agterm/skills/agterm"
if [[ "$FRONTEND" == gtk ]]; then
  echo "→ staged ${staged[*]:-no binaries} + libghostty ($PROFILE) into $DEST" >&2
else
  echo "→ staged ${staged[*]:-no binaries} of the GPUI build ($PROFILE) into $DEST" >&2
fi
