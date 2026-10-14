#!/usr/bin/env bash
# Resolve the upstream tag this checkout targets and the one to sync to.
#
#   resolve-tags.sh [TARGET_TAG]
#
# CURRENT is `upstream:` of conformance/catalog.yaml (the line reference-macos.yml reads). TARGET is the argument,
# else the latest published, non-draft, non-prerelease release of umputun/agterm.
# Prints CURRENT=, TARGET=, STATUS= (newer | up-to-date | older) and, for newer, RELEASES= (every release tag after
# CURRENT up to TARGET, oldest first, space separated).
# Exit 0 when TARGET is newer, 3 when there is nothing to sync, 1 on an error.
set -euo pipefail

REPO="umputun/agterm"
root="$(git rev-parse --show-toplevel)"
catalog="$root/conformance/catalog.yaml"
[[ -f "$catalog" ]] || { echo "resolve-tags: no $catalog" >&2; exit 1; }

current="$(sed -nE "s/^upstream:[[:space:]]*[\"']?([^\"'[:space:]#]+).*/\1/p" "$catalog" | head -n 1)"
[[ -n "$current" ]] || { echo "resolve-tags: no upstream: line in $catalog" >&2; exit 1; }

releases="$(gh release list --repo "$REPO" --exclude-drafts --exclude-pre-releases --limit 100 \
  --json tagName --jq '.[].tagName')" || { echo "resolve-tags: gh release list failed" >&2; exit 1; }

target="${1:-$(head -n 1 <<<"$releases")}"
[[ -n "$target" ]] || { echo "resolve-tags: no release found in $REPO" >&2; exit 1; }
grep -qxF "$target" <<<"$releases" || { echo "resolve-tags: $target is not a published release of $REPO" >&2; exit 1; }

echo "CURRENT=$current"
echo "TARGET=$target"
if [[ "$current" == "$target" ]]; then
  echo "STATUS=up-to-date"
  exit 3
fi
# sort -V orders v0.9.0 before v0.10.0
if [[ "$(printf '%s\n%s\n' "$current" "$target" | sort -V | tail -n 1)" != "$target" ]]; then
  echo "STATUS=older"
  exit 3
fi
echo "STATUS=newer"
between="$(printf '%s\n%s\n' "$releases" "$current" | sort -uV | awk -v lo="$current" -v hi="$target" '
  $0 == lo { on = 1; next }
  on { print }
  $0 == hi { exit }')"
echo "RELEASES=$(tr '\n' ' ' <<<"$between" | sed 's/ $//')"
