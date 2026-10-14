#!/usr/bin/env bash
# Run cargo for the spike. libghostty-vt-sys's build.rs runs `zig` from PATH (there is no env override) and builds
# Ghostty from GHOSTTY_SOURCE_DIR; the Ghostty commit 0.2.2 pins (a887df42) declares minimum_zig_version 0.15.2 and
# does not build with 0.16. So a Zig 0.15.2 goes first on PATH, and cargo is rustup's own proxy, not mise's shim
# (which puts its shims, zig's included, back in front of PATH). Runs inside the shared agents slice, at most 6 jobs.
#   spikes/gpui-term/cargo.sh build --release
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
zig_dir="${SPIKE_ZIG_DIR:-$HOME/.cache/agterm-rs/vendor/zig-x86_64-linux-0.15.2}"
[[ -x "$zig_dir/zig" ]] || { echo "no zig at $zig_dir (set SPIKE_ZIG_DIR)" >&2; exit 1; }
cargo="${SPIKE_CARGO:-$HOME/.cargo/bin/cargo}"
export PATH="$zig_dir:$HOME/.cargo/bin:$PATH"
export GHOSTTY_SOURCE_DIR="${GHOSTTY_SOURCE_DIR:-$("$here/fetch-ghostty-src.sh")}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-6}"
cd "$here"
exec systemd-run --user --slice=agterm-agents.slice --scope --quiet -- "$cargo" "$@"
