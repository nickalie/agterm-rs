#!/usr/bin/env bash
# Carry upstream's OLD..NEW changes into the repository's copies of upstream files.
#
#   sync-resources.sh [--upstream DIR] [--check] [--patches DIR] OLD_TAG NEW_TAG
#
# Each copy gets upstream's own diff, applied with `git apply --directory`, so the Linux glue a copy carries (the
# Codex adapter's python3/jq reader in agterm-codex-status.sh, the Linux chords in ghostty-defaults.conf) survives
# wherever upstream did not touch the same lines. Nothing is applied partially: a diff that does not apply cleanly
# is left out whole, saved under --patches DIR (default: a temp dir, printed), and reported as `conflict` for a plan
# task to merge by hand. --check applies nothing.
#
# Output, one line per copy: `<status> <upstream path> -> <copy>` with status unchanged | applied | would-apply |
# already-applied | conflict; then `unmapped` for every agterm/Resources entry or LICENSE change no mapping covers.
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
upstream="$root/.upstream/agterm"
check=0
patches=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --upstream) upstream="$2"; shift 2 ;;
    --check) check=1; shift ;;
    --patches) patches="$2"; shift 2 ;;
    -*) echo "sync-resources: unknown option $1" >&2; exit 1 ;;
    *) break ;;
  esac
done
[[ $# -eq 2 ]] || { echo "usage: sync-resources.sh [--upstream DIR] [--check] [--patches DIR] OLD_TAG NEW_TAG" >&2; exit 1; }
old="$1" new="$2"
patches="${patches:-$(mktemp -d "${TMPDIR:-/tmp}/agterm-sync-patches.XXXXXX")}"
mkdir -p "$patches"

# upstream directory | pathspec inside it | the repository's copy
MAPPINGS=(
  "agterm/Resources/agent-status|.|resources/agent-status"
  "plugins/agterm/skills/agterm|.|resources/skills/agterm"
  "agterm/Resources/hud|.|crates/agterm-gtk/resources/hud"
  "agterm/Resources/custom-themes|.|build/themes"
  "agterm/Resources|ghostty-defaults.conf|crates/agterm-core/src/ghostty_config"
  "scripts/zmx-patches|*.patch|build/patches/zmx"
  ".|LICENSE|resources/LICENSE.upstream"
)

cd "$root"
for mapping in "${MAPPINGS[@]}"; do
  IFS='|' read -r source spec copy <<<"$mapping"
  name="$(tr '/' '_' <<<"$copy")"
  patch="$patches/$name.patch"
  if [[ "$copy" == resources/LICENSE.upstream ]]; then
    # a single renamed file: compare contents instead of applying a diff
    if git -C "$upstream" diff --quiet "$old" "$new" -- LICENSE; then
      echo "unchanged LICENSE -> $copy"
    elif [[ "$check" == 1 ]]; then
      echo "would-apply LICENSE -> $copy"
    else
      git -C "$upstream" show "$new:LICENSE" >"$copy"
      echo "applied LICENSE -> $copy"
    fi
    continue
  fi
  relative=$([[ "$source" == . ]] && echo "" || echo "--relative=$source")
  # shellcheck disable=SC2086
  git -C "$upstream" diff --binary $relative "$old" "$new" -- "$source/$spec" >"$patch"
  label="$source$([[ "$spec" == . ]] || echo "/$spec")"
  if [[ ! -s "$patch" ]]; then
    rm -f "$patch"
    echo "unchanged $label -> $copy"
    continue
  fi
  if ! git apply --check --directory="$copy" "$patch" 2>"$patch.err"; then
    if git apply --check --reverse --directory="$copy" "$patch" 2>/dev/null; then
      rm -f "$patch" "$patch.err"
      echo "already-applied $label -> $copy"
      continue
    fi
    echo "conflict $label -> $copy (patch: $patch, why: $(head -n 1 "$patch.err"))"
    continue
  fi
  rm -f "$patch.err"
  if [[ "$check" == 1 ]]; then
    echo "would-apply $label -> $copy"
  else
    git apply --directory="$copy" "$patch"
    echo "applied $label -> $copy"
  fi
done

# a new entry under agterm/Resources has no copy yet: someone has to decide where it goes
comm -13 <(git -C "$upstream" ls-tree --name-only "$old" agterm/Resources/ | sort) \
  <(git -C "$upstream" ls-tree --name-only "$new" agterm/Resources/ | sort) | sed 's/^/unmapped (new upstream resource) /'
echo "PATCHES=$patches"
