# zig-free-core: paused

Paused on 2026-10-18 by the user's decision, partway through `docs/plans/20261018-zig-free-core.md`. The next plan
keeps libghostty-vt, builds it on Windows, macOS and Linux as herdr does, and drops the Rust VT port (Part A). This
branch is kept as a source to take parts from; it is not meant to be merged as it is.

## Done (each reviewed with revmux, fixed, gates and cross-platform CI green)

- Task 1: provenance, inventory and lint calibration of rustty against the Ghostty pin.
- Task 2: study of herdr's session server and spikes of its risky parts (Windows jobs and detach, ConPTY, pipes).
- Task 3: `.github/workflows/cross-platform.yml` (ubuntu, macos, windows and Miri).
- Tasks 4–8, `crates/agterm-vt` (Part A): memory core, Unicode lookups and the Rust uucode generator
  (`scripts/gen-vt-unicode.sh`), terminfo entry and identity (`scripts/gen-ghostty-terminfo.sh`), parser and stream
  with OSC/APC/SGR/modes/colors. No crate depends on it yet.
- Task 19: zmx renamed out of agterm's code and control protocol.
- Task 20, `crates/agterm-session`: wire protocol and endpoint (Unix socket with `flock` and peer uid; Windows
  named pipe).
- Task 21: `agterm-proto`, `agterm-host` and `agterm-core` build and test on Windows (namespace directory under
  `%LOCALAPPDATA%`, owner-only DACL, `cfg` pairs).
- Task 22: session server lifecycle: start on demand, detached per OS (setsid; posix_spawn with responsibility
  disclaim; CreateProcessW with job breakaway or WMI), one server per namespace, idle shutdown.

Not started: Tasks 9–18 (rest of the VT port and the switch to it) and 23–34 (pane runtime, server API, handoff,
CLI, client library, switch-over, process table, live panes, PowerShell, zmx removal, acceptance, docs).

## What carries over to the next plan

- Part B is independent of the VT choice: herdr itself runs on libghostty-vt. Tasks 19–22 (`agterm-session`, the
  Windows work in the core crates) apply as they are; the pane runtime (Task 23) takes libghostty-vt as its VT state.
- Task 3's cross-platform CI and Task 2's findings.
- Tasks 4–8 (`agterm-vt`) have no use once libghostty-vt stays, except as a reference.

## Open items found on the way

- `crates/agterm-core/src/live/owner_only.rs` (Windows owner-only DACL, `unsafe`) came in through a minor-finding fix
  in Task 21 and had no review round of its own.
- Hooks and custom commands launch through `/bin/sh -c` only (`crates/agterm-core/src/commands/hooks.rs`); there is
  no Windows form, and no task plans one.
- macOS: Ghostty does not inject its shell integration into Apple's `/bin/bash` 3.2; left to the macOS frontend work.

## Run records

Progress log, revmux rounds and Codex discussions: `~/.local/state/plan-exec-revmux/zig-free-core/` (outside the
repository).
