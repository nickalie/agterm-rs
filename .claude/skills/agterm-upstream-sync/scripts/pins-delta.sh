#!/usr/bin/env bash
# Compare build/pins.env with the pins upstream's scripts/setup.sh carries at a tag, and print the new values.
#
#   pins-delta.sh --upstream DIR NEW_TAG
#
# ZMX_REV (with its date comment) comes from setup.sh at NEW_TAG. When it moved, the script reads that revision's
# build.zig.zon for `minimum_zig_version` (upstream names Zig by Homebrew formula, so the version comes from the zon,
# not from setup.sh); the Zig sha256 sums are ziglang.org's download/index.json entries. It changes no file: it prints
# `NAME="value"` lines to put into build/pins.env, or `PINS=unchanged`. The build script re-verifies every sum when it
# fetches (scripts/build-zmx.sh).
#
# Upstream's GHOSTTY_REV is reported, never pinned: agterm-rs pins its own Ghostty (LIBGHOSTTY_VT_REV, ghostty main,
# for libghostty-vt's C API and the resources), so a move upstream is a note to review the Ghostty-level changes
# between the two revisions, and a LIBGHOSTTY_VT_REV bump is a change of its own.
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
upstream="$root/.upstream/agterm"
tag=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --upstream) upstream="$2"; shift 2 ;;
    -*) echo "pins-delta: unknown option $1" >&2; exit 1 ;;
    *) [[ -z "$tag" ]] || { echo "usage: pins-delta.sh [--upstream DIR] NEW_TAG" >&2; exit 1; }; tag="$1"; shift ;;
  esac
done
[[ -n "$tag" ]] || { echo "usage: pins-delta.sh [--upstream DIR] NEW_TAG" >&2; exit 1; }
pins="${PINS_ENV:-$root/build/pins.env}"  # PINS_ENV: a copy to compare against, for testing

setup="$(git -C "$upstream" show "$tag:scripts/setup.sh")"
up_value() { sed -nE "s/^$1=\"([^\"]+)\".*/\1/p" <<<"$setup" | head -n 1; }
up_comment() { sed -nE "s/^$1=\"[^\"]+\"[[:space:]]*#[[:space:]]*(.*)$/\1/p" <<<"$setup" | head -n 1; }
pin_value() { sed -nE "s/^$1=\"([^\"]*)\".*/\1/p" "$pins" | head -n 1; }

fetch() { curl -fsSL --retry 4 --retry-delay 3 "$@"; }
zon() { fetch "https://raw.githubusercontent.com/$1/$2/build.zig.zon"; }
zig_sums() {
  local version="$1" index
  index="$(fetch https://ziglang.org/download/index.json)"
  python3 -c '
import json, sys
entry = json.loads(sys.argv[1]).get(sys.argv[2])
if entry is None:
    sys.exit(f"pins-delta: zig {sys.argv[2]} is not in ziglang.org/download/index.json")
for arch in ("x86_64", "aarch64"):
    print(arch.upper(), entry[f"{arch}-linux"]["shasum"])
' "$index" "$version"
}

changed=0
ghostty_new="$(up_value GHOSTTY_REV)"
echo "# upstream GHOSTTY_REV: $ghostty_new ($(up_comment GHOSTTY_REV)); not a pin here: the Ghostty pin is" \
  "LIBGHOSTTY_VT_REV=$(pin_value LIBGHOSTTY_VT_REV)"
for name in ZMX; do
  rev_new="$(up_value "${name}_REV")"
  rev_old="$(pin_value "${name}_REV")"
  [[ -n "$rev_new" ]] || { echo "pins-delta: no ${name}_REV in $tag:scripts/setup.sh" >&2; exit 1; }
  echo "# ${name}_REV: $rev_old -> $rev_new ($(up_comment "${name}_REV"))"
  [[ "$rev_new" == "$rev_old" ]] && continue
  changed=1
  repo="$(up_value "${name}_REPO")"; repo="${repo#https://github.com/}"
  manifest="$(zon "$repo" "$rev_new")"
  zig="$(sed -nE 's/.*\.minimum_zig_version = "([^"]+)".*/\1/p' <<<"$manifest" | head -n 1)"
  echo "${name}_REV=\"$rev_new\""
  echo "${name}_ZIG_VERSION=\"$zig\""
  while read -r arch sum; do echo "${name}_ZIG_SHA256_${arch}=\"$sum\""; done < <(zig_sums "$zig")
  echo "# zmx pins ghostty $(sed -nE 's/.*ghostty#([0-9a-f]+).*/\1/p' <<<"$manifest" | head -n 1): build/patches/zmx/ghostty applies to that tree"
done
[[ "$changed" == 1 ]] || echo "PINS=unchanged"
