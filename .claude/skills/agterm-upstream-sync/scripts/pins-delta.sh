#!/usr/bin/env bash
# Report how the pins upstream's scripts/setup.sh carries moved between two tags. None of them is a pin here, so the
# script changes no file and fetches nothing: each move it prints is review work for the sync's plan.
#
#   pins-delta.sh [--upstream DIR] OLD_TAG NEW_TAG
#
# ZMX_REV (with its date comment): upstream's zmx, which this build does not run. ngterm-session carries what the app
# relies on of it (the attach contract, the snapshot, upstream's explicit leadership patch), so a move, and a change
# to upstream's scripts/zmx-patches/ between the tags (listed by file), is a plan task porting the behavior it changes
# into ngterm-session under `live.*` and the attach contract; the script prints zmx's compare URL for it. A change to
# zmx-patches/ghostty/ is a change to the libghostty-vt patch taken from it (build/patches/ghostty-vt/README.md).
#
# Upstream's GHOSTTY_REV is reported too, never pinned: ngterm pins its own Ghostty (LIBGHOSTTY_VT_REV, ghostty
# main, for libghostty-vt's C API and the resources), so a move upstream is a note to review the Ghostty-level changes
# between the two revisions, and a LIBGHOSTTY_VT_REV bump is a change of its own.
#
# The last line is `PINS=unchanged`, or `PINS=moved` with the moves above it.
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
upstream="$root/.upstream/agterm"
tags=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --upstream) upstream="$2"; shift 2 ;;
    -*) echo "pins-delta: unknown option $1" >&2; exit 1 ;;
    *) tags+=("$1"); shift ;;
  esac
done
[[ ${#tags[@]} -eq 2 ]] || { echo "usage: pins-delta.sh [--upstream DIR] OLD_TAG NEW_TAG" >&2; exit 1; }
old="${tags[0]}" new="${tags[1]}"
pins="${PINS_ENV:-$root/build/pins.env}"  # PINS_ENV: a copy to compare against, for testing

setup_old="$(git -C "$upstream" show "$old:scripts/setup.sh")"
setup_new="$(git -C "$upstream" show "$new:scripts/setup.sh")"
value() { sed -nE "s/^$2=\"([^\"]+)\".*/\1/p" <<<"$1" | head -n 1; }
comment() { sed -nE "s/^$2=\"[^\"]+\"[[:space:]]*#[[:space:]]*(.*)$/\1/p" <<<"$1" | head -n 1; }
pin_value() { sed -nE "s/^$1=\"([^\"]*)\".*/\1/p" "$pins" | head -n 1; }

moved=0
for name in ZMX_REV GHOSTTY_REV; do
  rev_old="$(value "$setup_old" "$name")"
  rev_new="$(value "$setup_new" "$name")"
  [[ -n "$rev_new" ]] || { echo "pins-delta: no $name in $new:scripts/setup.sh" >&2; exit 1; }
  echo "# upstream $name: ${rev_old:-none} -> $rev_new ($(comment "$setup_new" "$name"))"
  [[ "$rev_new" == "$rev_old" ]] && continue
  moved=1
  case "$name" in
    ZMX_REV)
      repo="$(value "$setup_new" ZMX_REPO)"
      echo "# zmx moved: port what it changes into ngterm-session" \
        "(${repo:-https://github.com/neurosnap/zmx}/compare/$rev_old...$rev_new)"
      ;;
    GHOSTTY_REV)
      echo "# not a pin here: the Ghostty pin is LIBGHOSTTY_VT_REV=$(pin_value LIBGHOSTTY_VT_REV); review the" \
        "Ghostty-level changes between the two revisions"
      ;;
  esac
done
patches="$(git -C "$upstream" diff --name-only "$old" "$new" -- scripts/zmx-patches)"
if [[ -n "$patches" ]]; then
  moved=1
  while read -r path; do echo "# upstream zmx patch changed: $path"; done <<<"$patches"
fi
[[ "$moved" == 1 ]] && echo "PINS=moved" || echo "PINS=unchanged"
