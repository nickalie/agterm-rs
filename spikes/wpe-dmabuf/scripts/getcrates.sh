#!/usr/bin/env bash
# Download and unpack crates.io crates for reading: getcrates.sh VERSION NAME...
set -euo pipefail
dir="$(cd "$(dirname "$0")/.." && pwd)/.cache/crates"
mkdir -p "$dir"
cd "$dir"
ver="$1"; shift
for c in "$@"; do
  [[ -d "$c-$ver" ]] && continue
  curl -sSfL -A agterm-spike -o "$c-$ver.crate" "https://static.crates.io/crates/$c/$c-$ver.crate"
  tar xzf "$c-$ver.crate"
done
ls "$dir"
