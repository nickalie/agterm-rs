#!/usr/bin/env bash
# Fetch the Ghostty source libghostty-vt-sys 0.2.2 pins (GHOSTTY_COMMIT in its build.rs) into a local prefix and
# print the directory to pass as GHOSTTY_SOURCE_DIR. Whole downloads are retried until the gzip stream is complete
# (GitHub's archive endpoint does not resume).
set -euo pipefail
rev=a887df42c56f6de86c0fe6da9c4eeca37931e083
root="${AGTERM_VENDOR:-$HOME/.cache/agterm-rs/vendor}"
dl="$root/ghostty-vt-dl"
src="$root/ghostty-vt-src-${rev:0:12}"
archive="$dl/ghostty-${rev:0:8}.tar.gz"
if [[ -f "$src/build.zig" ]]; then
  echo "$src"
  exit 0
fi
mkdir -p "$dl"
for attempt in $(seq 1 30); do
  gzip -t "$archive" 2>/dev/null && break
  echo "download attempt $attempt" >&2
  curl -fsSL --speed-limit 1000 --speed-time 60 -o "$archive.part" \
    "https://github.com/ghostty-org/ghostty/archive/$rev.tar.gz" && mv "$archive.part" "$archive" || true
done
gzip -t "$archive"
mkdir -p "$src.tmp"
tar -xzf "$archive" -C "$src.tmp" --strip-components=1
mv "$src.tmp" "$src"
echo "$src"
