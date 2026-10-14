#!/usr/bin/env bash
# Make the upstream clone hold both tags with the history between them, then check out the new one.
#
#   prepare-upstream.sh [--upstream DIR] [--no-checkout] OLD_TAG NEW_TAG
#
# DIR defaults to <repo>/.upstream/agterm, the read-only reference clone (git-ignored through .git/info/exclude).
# A missing clone is created blobless (full history, blobs fetched on demand); a shallow one is unshallowed, since
# `git log OLD..NEW` needs the history. The clone is never written to beyond fetching and checking out NEW_TAG:
# local changes in it stop the script. github.com drops large fetches now and then, so every network step retries.
# Prints UPSTREAM=, OLD_SHA=, NEW_SHA=, COMMITS=.
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
upstream="$root/.upstream/agterm"
checkout=1
while [[ $# -gt 0 ]]; do
  case "$1" in
    --upstream) upstream="$2"; shift 2 ;;
    --no-checkout) checkout=0; shift ;;
    -*) echo "prepare-upstream: unknown option $1" >&2; exit 1 ;;
    *) break ;;
  esac
done
[[ $# -eq 2 ]] || { echo "usage: prepare-upstream.sh [--upstream DIR] [--no-checkout] OLD_TAG NEW_TAG" >&2; exit 1; }
old="$1" new="$2"
URL="https://github.com/umputun/agterm"

retry() {
  local attempt
  for attempt in 1 2 3 4; do
    "$@" && return 0
    echo "prepare-upstream: attempt $attempt failed: $*" >&2
    sleep $((attempt * 5))
  done
  return 1
}

if [[ ! -d "$upstream/.git" ]]; then
  mkdir -p "$(dirname "$upstream")"
  clone() { rm -rf "$upstream"; git clone -q --filter=blob:none --no-checkout "$URL" "$upstream"; }
  retry clone
else
  if [[ -n "$(git -C "$upstream" status --porcelain --untracked-files=no)" ]]; then
    echo "prepare-upstream: $upstream has local changes; it is read-only, restore it first" >&2
    exit 1
  fi
  if [[ "$(git -C "$upstream" rev-parse --is-shallow-repository)" == true ]]; then
    retry git -C "$upstream" fetch -q --unshallow --tags origin
  else
    retry git -C "$upstream" fetch -q --tags origin
  fi
fi

for tag in "$old" "$new"; do
  git -C "$upstream" rev-parse -q --verify "refs/tags/$tag^{commit}" >/dev/null \
    || { echo "prepare-upstream: tag $tag not found in $upstream" >&2; exit 1; }
done

# a blobless clone fetches the blobs a diff needs lazily: fetch them now, with retries, so later reads stay local
retry git -C "$upstream" diff --numstat "$old" "$new" >/dev/null

if [[ "$checkout" == 1 ]]; then
  retry git -C "$upstream" -c advice.detachedHead=false checkout -q "$new"
fi

echo "UPSTREAM=$upstream"
echo "OLD_SHA=$(git -C "$upstream" rev-parse "$old^{commit}")"
echo "NEW_SHA=$(git -C "$upstream" rev-parse "$new^{commit}")"
echo "COMMITS=$(git -C "$upstream" rev-list --count "$old..$new")"
