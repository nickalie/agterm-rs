#!/usr/bin/env bash
# Assemble the staging layout the conformance runner, scripts/run-gpui-dev.sh and the install check run from:
#
#   bin/{agterm,agtermctl,zmx}  share/ghostty/{themes,shell-integration}  share/terminfo
#   share/agterm/zmx-BUILD (the staged zmx's build id, with bin/zmx)  share/agterm/{agent-status,skills/agterm}
#
# from cargo's target dir and the vendored prefixes, plus agterm's own themes from build/themes and the agent
# integrations' payloads from resources/. Binaries are staged when they exist (agterm and agtermctl appear with
# their crates, zmx once scripts/build-zmx.sh filled its prefix); Ghostty's resources are required. There is no lib/:
# libghostty-vt is linked statically. The terminfo, themes and shell integration are Ghostty's at LIBGHOSTTY_VT_REV,
# from the libghostty-vt prefix (scripts/build-ghostty-vt.sh).
#
# Usage: scripts/stage.sh DESTINATION [--profile release|debug]   (default: release)
set -euo pipefail

# shellcheck source=lib/native.sh
source "$(dirname "$0")/lib/native.sh"

usage() { die "usage: scripts/stage.sh DESTINATION [--profile release|debug]"; }

(($# >= 1)) || usage
DEST="$1"
shift
PROFILE=release
while (($#)); do
  (($# >= 2)) || usage
  case "$1" in
    --profile)
      [[ "$2" == release || "$2" == debug ]] || usage
      PROFILE="$2"
      ;;
    *) usage ;;
  esac
  shift 2
done
[[ "$DEST" = /* ]] || DEST="$PWD/$DEST"
if [[ -d "$DEST" ]] && find "$DEST" -mindepth 1 -print -quit | grep -q .; then
  die "staging destination is not empty: $DEST"
fi

GHOSTTY="$(ghostty_vt_prefix)"
ghostty_vt_prefix_is_complete "$GHOSTTY" \
  || die "no complete libghostty-vt prefix at $GHOSTTY: run scripts/build-ghostty-vt.sh"
TARGET="${CARGO_TARGET_DIR:-$NATIVE_ROOT/target}/$PROFILE"

mkdir -p "$DEST/bin" "$DEST/share/ghostty"
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
  ZMX_PREFIX="$("$NATIVE_ROOT/scripts/build-zmx.sh" --print-prefix)"
  # only a complete prefix: a zmx without its build id would read as one predating every Live sessions update
  if "$NATIVE_ROOT/scripts/build-zmx.sh" --verify; then
    install -m755 "$ZMX_PREFIX/bin/zmx" "$DEST/bin/zmx"
    install -Dm644 "$ZMX_PREFIX/share/agterm/zmx-BUILD" "$DEST/share/agterm/zmx-BUILD"
    staged+=(zmx)
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
echo "→ staged ${staged[*]:-no binaries} ($PROFILE) into $DEST" >&2
