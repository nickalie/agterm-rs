#!/usr/bin/env bash
# Regenerate agterm-vt's checked-in terminfo source (crates/agterm-vt/terminfo/xterm-ghostty.terminfo), Ghostty's
# entry xterm-ghostty as Ghostty's encoder writes it (`+terminfo`), with agterm-vt's `gen-terminfo` binary (feature
# `gen`): its port of Ghostty's terminfo source model, encoder and entry (src/terminfo at LIBGHOSTTY_VT_REV, in the
# crate's `terminfo` module). It needs nothing but cargo: no fetch, no Zig. Run after porting a change of Ghostty's
# src/terminfo into that module (a LIBGHOSTTY_VT_REV bump), then review the diff. The infocmp fixture
# (crates/agterm-vt/tests/fixtures/xterm-ghostty.infocmp) records the entry Ghostty's own Zig build compiled at the pin
# it was captured at; a ported change of the entry changes it too, by hand, with the capabilities Ghostty changed.
#
# Usage: scripts/gen-ghostty-terminfo.sh          write the source (only when it changes)
#        scripts/gen-ghostty-terminfo.sh --check  write nothing; exit 1 when the source is stale (CI, on every OS)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

CHECK=()
case "${1:-}" in
  --check) CHECK=(--check) ;;
  "") ;;
  *)
    echo "usage: scripts/gen-ghostty-terminfo.sh [--check]" >&2
    exit 1
    ;;
esac

cd "$ROOT"
cargo run --quiet --locked -p agterm-vt --features gen --bin gen-terminfo -- ${CHECK[@]+"${CHECK[@]}"}
