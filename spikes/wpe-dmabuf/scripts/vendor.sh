#!/usr/bin/env bash
# Recreates vendor/gpui-pre and vendor/gpui-pre-wgpu: the crates.io 0.3.9 sources with patches/*.patch applied.
# `--diff` regenerates the patches from the current vendor/ trees instead.
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
"$here/scripts/getcrates.sh" 0.3.9 gpui-pre gpui-pre-wgpu >/dev/null
for crate in gpui-pre gpui-pre-wgpu; do
  pristine="$here/.cache/crates/$crate-0.3.9"
  if [[ "${1:-}" == --diff ]]; then
    work="$(mktemp -d)"
    ln -s "$pristine" "$work/a"
    ln -s "$here/vendor/$crate" "$work/b"
    (cd "$work" && diff -ruN a/ b/ > "$here/patches/$crate.patch") || true
    rm -rf "$work"
    continue
  fi
  rm -rf "$here/vendor/$crate"
  mkdir -p "$here/vendor"
  cp -r "$pristine" "$here/vendor/$crate"
  patch -s -d "$here/vendor/$crate" -p1 < "$here/patches/$crate.patch"
done
