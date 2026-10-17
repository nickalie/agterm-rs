# Cross-platform session host: live sessions without zmx on Windows, Linux and macOS

## Overview

- Goal: live sessions (a pane's shell outliving the app and reattaching on the next launch) on Windows, Linux and
  macOS alike. Today they run on zmx, which is POSIX-only (`forkpty`, `fork`, `setsid`, `AF_UNIX`) and cannot build
  for Windows.
- Decision (user, 2026-10-16): Windows, Linux and macOS are equal targets; the GPUI frontend is the cross-platform
  one. The live-session layer is replaced on all three, not forked per platform.
- What changes: a new Rust crate `agterm-session` (protocol, daemon, client library, CLI binary `agterm-session`)
  replaces the zmx binary and the `agterm-zmx` crate that runs it.
- What stays (decisions of 2026-10-16):
  - **The zmx model**: one daemon per session, named and listed as today. Panes are isolated from each other's
    crashes, and an update kills nothing, because old daemons keep running their own binary.
  - **`agterm-core::zmx`'s policy**: naming, reap, prune, lead, reset, replay. Only its Unix-only spots gain Windows
    forms (`socket_directory`'s hardcoded `/tmp/agterm-zmx-`, the `/bin/sh -c` attach line, a few `cfg(unix)`
    reads).
  - **zmx's wire protocol, byte for byte**: the tags and frames of the pinned zmx with `build/patches/zmx` applied.
    zmx daemons already running on a user's machine keep working: the new client attaches to them.
  - **zmx's CLI contract**: subcommands, arguments, environment, output and exit codes. Three callers depend on it:
    - upstream's remote attach runs `<endpoint executable> attach <daemon> /bin/sh -c <guard>` over ssh, with
      `ZMX_SESSION=`, `ZMX_SESSION_PREFIX=`, `ZMX_NO_DETACH_KEY=1`, `ZMX_DIR`, `ZMX_MANAGED`, `ZMX_MANAGED_CLAIM`,
      and reads the role title `OSC 2;zmx-role;<token>:<role>:<gen>` (`agtermCore/RemoteSession.swift`);
    - `agtermctl clipboard set` runs `<dir of agtermctl>/zmx print <daemon>` (`agterm-ctl/src/clipboard.rs:184`);
    - `conformance/agterm_conformance/zmx_client.py` takes the executable from `zmx.tree` and drives it as the
      remote viewer does.

    The binary keeps that contract, so an upstream Mac attaching to our machine (or the reverse) keeps working.
- What comes from herdr (`~/develop/projects/temp/herdr`, Apache-2.0) as code, with its notices kept:
  - PTY over `portable-pty` (ConPTY on Windows) and herdr's fixes in `vendor/portable-pty`;
  - IPC over `interprocess` (Unix socket / named pipe, with herdr's owner-only pipe DACL);
  - daemon detach on Linux and Windows (`setsid`, `DETACHED_PROCESS`, the job-object check);
  - Windows process snapshots (Toolhelp32, `NtQueryInformationProcess` command lines, job objects).
- What does not come from herdr: its single server for every pane and its server-side TUI rendering. The first fails
  the Windows update case (herdr's handoff is `#[cfg(unix)]`); the second has no use for a GUI that draws cells
  itself.
- The GPUI pane talks to its daemon in process, over the socket, with no PTY and no `attach` subprocess (decision
  2026-10-16). The CLI `agterm-session attach` stays for remote viewers, terminals, scripts and the suite.
- Windows live sessions run PowerShell 7 with agterm's own integration script, a Rust-only extension like bash and
  fish today (decision 2026-10-16).
- Future (not in this plan, but the design must allow it): a TUI frontend for headless servers, and a headless core
  process that keeps the model and the control socket alive while no client is attached. The session crate stays
  frontend-agnostic and `agterm-term` stays toolkit-neutral for that.

## Context (from discovery)

- Starting point: `main` after `20261014-gpui-frontend.md` has merged (decision 2026-10-16). Paths below are as of
  the `gpui-frontend` branch at `e9b2f1e`.
- `crates/agterm-host/src/lib.rs:188` `SessionHost`: `is_available`, `list`, `kill`, `screen`, `type_input`,
  `kill_batch`, `foreground_job`, `job_running`, `end_job`.
  - The frontend builds the host in `agterm-gpui/src/session_host.rs` and `agterm-app/src/restore_launch.rs`.
- `crates/agterm-zmx`:
  - runs the zmx binary: `list`, `kill <names…> [--force]` (batched in `inventory.rs:33`), `screen [--all]`, `type`;
  - `launch.rs` reads the live-launch inputs: `AGTERM_ZMX_PATH`, `share/agterm/zmx-BUILD`, login shell, Ghostty
    resources, socket namespace;
  - `sweep.rs` hangs up a killed shell's foreground job.
- `crates/agterm-core/src/zmx/`:
  - `support.rs` builds the attach command (`/bin/sh -c` line at `:233`) and the configuration;
  - `lifecycle.rs` holds the `zmx list` parser, the leader map and the reap policy;
  - also `lead.rs`, `reset*`, `replay.rs`, `screen.rs`, `build_record.rs`;
  - `shell_integration.rs`: zsh as upstream; bash and fish as a Rust-only extension, capability `live-any-shell`;
  - `mod.rs:68` `socket_directory` hardcodes `/tmp/agterm-zmx-<FNV-1a 64>`.
  - Unix-only code also sits at `support.rs:76`, `restore/mode.rs:224` and `commands/hud.rs:534`.
- `crates/agterm-term`: the pane core.
  - `Pane::launch` runs a `Launch`: live is `zmx attach` in a `portable-pty` PTY, so the attach creates the daemon,
    inheriting the pane's environment (`TERM=xterm-ghostty`, `TERMINFO`, integration variables, `ZMX_DIR`), cwd and
    `ZMX_TRACK_ENV`; plain is the command or login shell.
  - Through zmx, 50 MiB `cat` takes 0.73–1.0 s against 0.49–0.53 s plain, at zmx's own ceiling.
- `crates/agterm-app/src/process_table.rs`: the Linux `ProcessTable` over `/proc`; its consumer is the quit-time
  command capture for replay. There is no macOS or Windows one.
- zmx (pinned `ZMX_REV` in `build/pins.env`, patched by `build/patches/zmx/0001-explicit-leadership`,
  `0002-private-ghostty-dependency`, `0003-first-attach-snapshot`, and `build/patches/zmx/ghostty/0001-formatter-
  unstyled-blanks` on its private Ghostty), ≈9.9k lines of Zig.
  - `src/ipc.zig`:
    - `Header = packed struct {tag: u8, len: u32}` is a u40 backing integer: `@sizeOf` is **8**, bytes 5–7 padding;
    - `Info` is 552 bytes;
    - `Resize` is 8 bytes (`rows, cols, xpixel, ypixel`), and `appendSizeMessage` sends a 4-byte legacy form and the
      8-byte form;
    - tags 0–25, non-exhaustive, unknown tags ignored.
  - `src/cfg.zig` `socketDir`: `ZMX_DIR`, else `$XDG_RUNTIME_DIR/zmx`, else `$TMPDIR/zmx-<uid>`. Directory modes come
    from `ZMX_DIR_MODE` and log modes from `ZMX_LOG_MODE`.
  - `src/daemonize.zig`: `forkpty`; sets `ZMX_SESSION`, fixes a `dumb`/missing `TERM`, unsets `ZMX_MANAGED` and
    `ZMX_MANAGED_CLAIM` in the child.
  - `src/loop.zig`: the daemon loop, clients, roles, `attachedScreen`.
  - `src/util.zig` `serializeTerminalState`: a two-phase scrollback + visible replay with color overrides,
    DECSET 2026 excluded.
  - `src/main.zig`: CLI and `list`. `list` unlinks stale sockets and prints "no sessions found" to stderr with exit 0.
  - A local patched tree for reading: `~/.cache/agterm-rs/task11/zmx-src`. The reference is always the pin plus the
    patches.
- Conformance:
  - `impls/base.py:50` finds leftover daemons by executable name (`LEFTOVER_NAMES = {"zmx", "agterm-session-host"}`).
  - Live sessions back the zmx ids, `test_live_any_shell`, `test_restore`, `test_relaunch` and the three
    `test_clipboard_set_*` ids.
- herdr pieces:
  - `src/pty/` and `vendor/portable-pty`: the PTY actor;
  - `src/ipc.rs`: `interprocess` local sockets;
  - `src/server/autodetect.rs`, `src/platform/{mod,windows}.rs`: daemon spawn and detach, job objects,
    `foreground_job`, process snapshots with command lines, the pipe SDDL;
  - the `windows-sys` features list in its `Cargo.toml`.
- Upstream macOS supervises live sessions with `agterm-session-host` (`ZmxSupport.attachCommand` wraps
  `<host> client <daemon> --`). Permission identity comes from `responsibility_spawnattrs_setdisclaim`
  (`AgtermResponsibility/Responsibility.swift`), not from the Mach bootstrap context that herdr sets.
- CI: `.github/workflows/{rust-linux,reference-macos,spike-macos,conformance-gate}.yml`; no Windows workflow yet.

## Development Approach

- **testing approach**: TDD for the protocol and the CLI contract: golden bytes and golden text first, taken from the
  pinned patched zmx, then the code. Regular (code, then tests in the same task) for the rest.
- complete each task fully before moving to the next; small, focused changes.
- **CRITICAL: every task MUST include new/updated tests** for its code changes:
  - unit tests for new and modified functions;
  - success and error paths;
  - an existing test updated when behavior changes.
- **CRITICAL: all tests must pass before the next task starts.** The gates of CLAUDE.md apply to every task:
  `cargo fmt --all --check`, clippy `-D warnings`, `scripts/lint-report.sh`, `cargo test --workspace`. Tasks that
  touch the GPUI binary or staging also run its full conformance run in the background, and no ratchet id goes red.
- **CRITICAL: update this plan when scope changes.**
- Isolation (CLAUDE.md > Never touch the user's running agterm) holds everywhere:
  - every test daemon runs under a private `ZMX_DIR` (and on Windows a private pipe prefix);
  - test reaps go through that namespace only, never `pkill`;
  - `AGTERM_*`/`ZMX_*` are stripped from every child environment.
- Windows and macOS code is compiled and tested on CI from Task 2 on (`windows-latest`, a macOS runner), not only
  cross-checked; Linux runs locally and on CI.
- Herdr-derived files keep herdr's Apache-2.0 header and a line naming the source file; the MIT workspace gets a
  `NOTICE` for them (Task 1).

## Testing Strategy

- **unit tests**: required in every task (see Development Approach).
- **protocol golden tests**: encoded frames are compared to bytes recorded from the pinned patched zmx (fixtures under
  `crates/agterm-session/tests/fixtures/`), so byte compatibility does not depend on zmx staying in the build.
- **interop tests** (Linux and macOS, Tasks 4–10, while zmx is still built):
  - zmx's own `attach` against the new daemon (Tasks 4–6);
  - the new client against a real zmx daemon (Task 7 on).

  They are dropped with zmx in Task 15, and the golden fixtures keep the guarantee.
- **host e2e tests** (all three OSes on CI): spawn a daemon detached from the test process, attach, type, read
  `screen`, detach, kill the test's parent side, reattach and see the same screen, `kill` and `kill --force`, `list`,
  `print`.
- **conformance**: the GPUI run on Linux keeps every ratchet id green: the zmx ids, `test_live_any_shell`,
  `test_restore`, `test_relaunch` and `test_clipboard_set_*`. Newly green ids go in with `check_ratchet.py --add`.
- **performance**: 50 MiB `cat` through a live pane is no slower than through zmx (0.73–1.0 s on the dev box), and
  an idle daemon's RSS is no larger than an idle zmx daemon's (both measured in Task 1).

## Progress Tracking

- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- update the plan if implementation deviates from the original scope

## Solution Overview

```
agterm-core::zmx (policy unchanged)          agtermctl zmx … / clipboard set (unchanged contract)
        │ SessionHost
agterm-session ──────────────────────────────────────────────────────────────
  proto      zmx wire format (8-byte header, tags 0–25, patched semantics), golden-tested
  transport  Unix socket <ZMX_DIR>/<name> │ named pipe \\.\pipe\<prefix>-<name> (interprocess, owner-only DACL)
  daemon     portable-pty (openpty │ ConPTY) + agterm-ghostty-vt state, clients, explicit leadership,
             snapshot on attach, screen/type/info/kill/print; detached per platform
  client     library: list/kill/screen/type/print + an attach stream (used in process by agterm-term)
  host       SessionHost impl (replaces agterm-zmx::ZmxClient), foreground job per platform
  bin        agterm-session: daemon │ attach │ list │ kill │ screen │ type │ print │ version (zmx's CLI contract)
agterm-term  Pane IO backend: PTY (plain panes) │ session stream (live panes, no PTY, no subprocess)
```

- One daemon per session, as zmx. The daemon is the same `agterm-session` binary started as `agterm-session daemon
  <name>` with the pane's argv, environment and cwd. It is small and links no GPUI, because one daemon runs per pane.
- Rollout in two steps, so the ratchet never goes red:
  1. Task 10 stages `agterm-session` in zmx's place. The GPUI pane still runs `attach` in a PTY, exactly as it runs
     `zmx attach` today.
  2. Task 13 swaps the pane to the in-process stream. The pane then creates a missing daemon itself (`daemon`
     with the argv, environment and cwd that `attach` used to inherit) and attaches over the socket.
- Namespace:
  - Unix keeps `/tmp/agterm-zmx-<FNV-1a 64 of the state dir>`.
  - On Windows, `socket_directory` resolves under `%LOCALAPPDATA%` (Task 1 fixes the exact path). That directory
    holds logs and per-session marker files, while the endpoints are named pipes `\\.\pipe\agterm-zmx-<hash>-<name>`
    created with `FILE_FLAG_FIRST_PIPE_INSTANCE` and an owner-only DACL. `list` enumerates `\\.\pipe\` by that prefix.
- In-process attach: `agterm-term` opens the session's socket, sends Manage (explicit leadership) and Init with its
  grid size, and from then on maps:
  - Output → `vt_write`;
  - `Pane::write` → Input;
  - `Pane::resize` → Resize;
  - Role changes → the lead state core already tracks.

  Plain panes keep the PTY path.
- Platforms:
  - **Linux**: spawn with `setsid` in `pre_exec`, PTY via `openpty`.
  - **macOS**: `posix_spawn` with `POSIX_SPAWN_SETSID` and `responsibility_spawnattrs_setdisclaim`, so a live
    session keeps its own permission identity across app updates, as upstream's session host does (Task 9).
  - **Windows**: ConPTY, `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB`. The shell tree
    runs in a job object with `KILL_ON_JOB_CLOSE`, so killing or losing the daemon ends its processes as SIGHUP does
    on Unix.
- Updates and the build id:
  - The build id is a hash of the staged `agterm-session` binary, written where `zmx-BUILD` was;
    `build_record.rs`'s existing "predates the last update" logic offers the usual reset and kills nothing behind
    the user's back.
  - On Windows a running `.exe` cannot be replaced, so daemons run from a copy keyed by the build id
    (`<data dir>/session-host/<build id>/agterm-session.exe`). The app installs that copy when it is missing and
    prunes copies no daemon runs. Task 1 confirms the scheme.
- Migration: the user's running zmx daemons stay attachable, because the protocol is the same.

## Technical Details

- **Frame**: an 8-byte header. Byte 0 is the tag, bytes 1–4 the little-endian `u32` length, bytes 5–7 padding:
  written as zero, ignored on read. Then `len` payload bytes.
- **Payloads**: every patched struct ported with the same layout and size, golden-tested:
  - `Resize`: 8 bytes, with Init and Resize both sent in the 4-byte legacy form and the 8-byte form, as
    `appendSizeMessage` does;
  - `Info`: 552 bytes;
  - `ManageFlags`, `RoleChange`, `ScreenHeader`, `ScreenMode`, `TypeResult`.

  Unknown tags are read and skipped, as the non-exhaustive enum requires.
- **Tags implemented**: every tag that a caller produces. The callers are:
  - `agterm-core` and `agtermctl` (`print` included);
  - upstream's `ZmxSupport`/`ZmxLifecycle`/`RemoteSession` command lines;
  - `zmx_client.py`;
  - zmx's own `attach`, EnvSet from `ZMX_TRACK_ENV` included.

  Task 2 lists the set from the sources before writing the codec. A tag outside it is skipped like an unknown one and
  becomes a ➕ task if a caller turns up.
- **Snapshot on attach**: zmx replays the terminal with `serializeTerminalState`:
  - scrollback first, unstyled;
  - then a clear and the visible screen with extras;
  - color overrides;
  - DECSET 2026 off;
  - `0003`'s reset prefix.

  The daemon reproduces it with libghostty-vt's C formatter (`ghostty_formatter_terminal_new`/`_format_alloc`) and
  `ghostty_terminal_continuation_*`. zmx's private Ghostty carries `0001-formatter-unstyled-blanks`, while
  libghostty-vt has no patches today (`pins.env`). Task 1 checks that patch against `LIBGHOSTTY_VT_REV`. If it is
  still needed, it moves to a new `build/patches/ghostty-vt/` with support in `build-ghostty-vt.sh` and the prefix
  key — never a hand-rolled VT encoder.
- **Child environment**: `ZMX_SESSION=<name>`; `TERM=xterm-256color` when unset or `dumb`; `ZMX_MANAGED` and
  `ZMX_MANAGED_CLAIM` removed. Empty means unset for `ZMX_SESSION`, `ZMX_SESSION_PREFIX` and the rest of the
  variables zmx reads that way. On Windows the same variables, with `TERM` set for ConPTY's VT mode.
- **CLI contract** (golden-tested against the pinned zmx):
  - `list`: text, stale-socket unlink, "no sessions found" on stderr with exit 0;
  - `kill <names…> [--force]` outcome lines;
  - `screen [--all]`, `type`;
  - `print <daemon>`: Output injection, exit 1 when the daemon is unresponsive;
  - `attach`:
    - the create-only payload, `ZMX_NO_DETACH_KEY`;
    - the role title `OSC 2;zmx-role;<token>:<role>:<gen>`;
    - exit codes;
    - `version`.
- **Foreground job** (`SessionHost::foreground_job`, `job_running`, `end_job`):
  - Linux: the existing `/proc` process-group logic, moved behind a per-OS module.
  - macOS: `sysctl` (`KERN_PROC_PGRP`, `KERN_PROCARGS2`), as upstream's `ForegroundProcess.swift` does.
  - Windows: herdr's process snapshot. The job is the shell's live descendants, and `end_job` terminates them. The
    hangup is ConPTY's `CTRL_CLOSE_EVENT` to the console's processes.
- **`ProcessTable`** for macOS and Windows beside `agterm-app/src/process_table.rs`. Its consumer is the quit-time
  command capture that feeds replay, which `agterm-app` runs on every OS. Windows argv comes from
  `NtQueryInformationProcess(ProcessCommandLineInformation)` split with `CommandLineToArgvW`, as herdr does.
- **PowerShell**: `ShellKind::Pwsh` in `shell_integration.rs` with:
  - an integration script (OSC 7 cwd, OSC 133 prompt/command marks, title);
  - launch arguments;
  - a pwsh rendering of the replay script (`replay.rs`);
  - the eligibility probe on Windows: `pwsh.exe` on `PATH`, else `powershell.exe`, else the existing "unsupported
    shell" rejection.
- **Dependencies** (popular-tools rule): `portable-pty` (already in), `interprocess`, `windows-sys` (Microsoft) with the
  features herdr uses. No new IPC or PTY crate beyond those.

## What Goes Where

- **Implementation Steps** (`[ ]`): code, tests, CI workflows, scripts and docs in this repo.
- **Post-Completion**: manual checks on a real Windows desktop and a Mac, the user's own migration off zmx daemons.

## Implementation Steps

### Task 1: Spike — Windows toolchain, ConPTY daemon, formatter parity, update scheme, baselines

**Files:**
- Create: `docs/spikes/20261016-session-host.md`
- Create: `NOTICE`

- [ ] on `windows-latest` (a throwaway workflow run): build `agterm-ghostty-vt-sys` with zig 0.16.0 from
      `build/pins.env` and run its tests; record what the libghostty-vt build needs on Windows
- [ ] throwaway probe (not merged):
  - a detached process holds a `portable-pty` ConPTY shell, with herdr's vendored fixes compared against crates.io
    0.9.0 and the diff recorded;
  - it serves a named pipe through `interprocess` with herdr's DACL;
  - the parent exits, and a second process reconnects and reads output;
  - the same on macOS with `posix_spawn` + setsid + disclaim
- [ ] formatter parity: compare libghostty-vt's C formatter and continuation API with zmx `serializeTerminalState`
      and `0003`'s reset prefix; check `build/patches/zmx/ghostty/0001-formatter-unstyled-blanks` against
      `LIBGHOSTTY_VT_REV`
- [ ] decide the Windows namespace directory and confirm the versioned daemon copy scheme (a running `.exe` cannot
      be replaced)
- [ ] baselines on the dev box: 50 MiB `cat` through a live zmx pane in the GPUI build, idle zmx daemon RSS
- [ ] the workspace is MIT like upstream (user, 2026-10-16; `LICENSE`, `license.workspace`); write the `NOTICE`
      for herdr's Apache-2.0 code and check that every crate the GPUI merge added inherits `license.workspace`
- [ ] write the spike note with go/no-go per item; a no-go stops the plan with a ⚠️ and a codex-discussion

### Task 2: `agterm-session` crate, the wire codec, core on Windows, CI

**Files:**
- Create: `crates/agterm-session/Cargo.toml`, `crates/agterm-session/src/{lib,proto}.rs`
- Create: `crates/agterm-session/tests/{proto.rs,fixtures/}`
- Create: `scripts/record-zmx-frames.sh`
- Create: `.github/workflows/windows.yml`
- Modify: `Cargo.toml` (workspace member, deps `interprocess`, `windows-sys`), the macOS workflow
- Modify: `crates/agterm-core/src/zmx/{mod,support}.rs`, `crates/agterm-core/src/restore/mode.rs`,
  `crates/agterm-core/src/commands/hud.rs` (Unix-only spots)

- [ ] list the tags and payloads the callers use (see Technical Details > Tags) and write the set into this task
- [ ] `scripts/record-zmx-frames.sh`: run the pinned patched zmx under a private `ZMX_DIR` and record each frame kind,
      the 4- and 8-byte size forms included, as fixtures
- [ ] write golden tests first:
  - encode/decode of every tag in the set against the fixtures;
  - the 8-byte header with zero padding;
  - the 552-byte `Info`;
  - an unknown tag skipped;
  - a truncated header and an oversized length refused
- [ ] implement `proto.rs` (header, tags, payload structs, a streaming decoder) until the tests pass
- [ ] `agterm-proto`, `agterm-host`, `agterm-core` and `agterm-session` build on Windows: `socket_directory` per OS
      (Task 1's directory), the Unix-only spots behind `cfg`; unit tests for the Windows namespace
- [ ] `windows.yml` and the macOS workflow build and test these crates; from here on every task's tests run there
- [ ] run gates

### Task 3: Transport and namespace

**Files:**
- Create: `crates/agterm-session/src/{transport,namespace}.rs`, `crates/agterm-session/tests/transport.rs`

- [ ] socket directory as zmx's `socketDir` (with `ZMX_DIR` first) on Unix, Task 2's directory on Windows; per-session
      endpoint `<dir>/<name>` on Unix and `\\.\pipe\agterm-zmx-<hash>-<name>` on Windows; logs in `<dir>/logs`
- [ ] permissions: `ZMX_DIR_MODE` and `ZMX_LOG_MODE` on Unix; on Windows an owner-only DACL (herdr's SDDL) and
      `FILE_FLAG_FIRST_PIPE_INSTANCE`, so another user cannot squat a name
- [ ] endpoint enumeration (directory scan on Unix, `\\.\pipe\` listing by prefix on Windows) and stale-endpoint
      handling as zmx's `list`: unlink a refused Unix socket; Windows has no stale state
- [ ] listener and connector over `interprocess`
- [ ] tests:
  - names with every allowed character;
  - an over-long Unix socket path refused as zmx refuses it;
  - enumeration ignoring foreign entries;
  - stale unlink;
  - a second pipe instance under a taken name refused
- [ ] run gates

### Task 4: Daemon — PTY, VT state, one client

**Files:**
- Create: `crates/agterm-session/src/daemon/{mod,pty,state}.rs`
- Create: `crates/agterm-session/tests/{daemon_single,interop_attach}.rs`

- [ ] PTY child over `portable-pty` (herdr-derived actor, notices kept) with the child environment of Technical Details
- [ ] VT state in `agterm-ghostty-vt` fed from the PTY; Output fan-out to attached clients
- [ ] Init and Resize (both size forms), Input, Detach, Kill, Info as zmx handles them for a single client; EnvSet
      stored and applied to later children as zmx does (`ZMX_TRACK_ENV`)
- [ ] child exit: the daemon reports it as zmx does and exits, removing its endpoint
- [ ] tests:
  - echo round trip;
  - resize reaching the child;
  - kill ends the child;
  - child exit ends the daemon;
  - interop (Linux, macOS): zmx's own `attach` drives the new daemon
- [ ] run gates

### Task 5: Daemon — several clients and explicit leadership

**Files:**
- Modify: `crates/agterm-session/src/daemon/mod.rs`
- Create: `crates/agterm-session/src/daemon/clients.rs`, `crates/agterm-session/tests/daemon_lead.rs`

- [ ] client set, DetachAll, per-client write queues bounded as zmx's
- [ ] Manage/Role, the patched explicit-leadership protocol (`0001-explicit-leadership.patch`):
  - who owns the size;
  - role changes sent to managed clients;
  - unmanaged clients behave as before
- [ ] tests port the patch's cases:
  - a second client claims the lead;
  - the leader detaches;
  - a follower's resize is ignored;
  - an unmanaged client mixed with managed ones;
  - interop with zmx `attach` as the second client
- [ ] run gates

### Task 6: Daemon — snapshot on attach, Screen, Type and Print

**Files:**
- Create: `crates/agterm-session/src/daemon/{snapshot,screen}.rs`, `crates/agterm-session/tests/daemon_screen.rs`
- Create: `build/patches/ghostty-vt/` with `scripts/build-ghostty-vt.sh` support (only if Task 1 found the patch still
  needed)

- [ ] snapshot on Init through the libghostty-vt formatter and continuation API, matching `serializeTerminalState` and
      `0003-first-attach-snapshot.patch` (its reset prefix included)
- [ ] Screen: `active`/`all`, `ScreenHeader` + text, empty payload for an unknown mode
- [ ] Type: CR as Return encoded for the keyboard mode, queued without taking the lead, `TypeResult`
- [ ] the injection `print` uses: Output fanned out to clients as zmx does
- [ ] tests:
  - reattach shows scrollback, then the visible screen with the cursor in place;
  - an alt-screen app;
  - color overrides kept;
  - DECSET 2026 not replayed;
  - Screen, Type and Print cases from the patches;
  - interop: zmx `attach` sees the same snapshot as from zmx's daemon
- [ ] run gates

### Task 7: CLI binary with zmx's contract

**Files:**
- Create: `crates/agterm-session/src/{bin/agterm-session.rs,cli.rs,attach_tty.rs}`
- Create: `crates/agterm-session/tests/{cli_golden.rs,fixtures/cli/}`

- [ ] golden tests first, recorded from the pinned zmx:
  - `list` text, stale unlink, "no sessions found";
  - `kill <names…> [--force]` lines, the batch form included;
  - `screen [--all]` output;
  - `type` and `print` exit codes and messages;
  - `version`;
  - `attach`'s empty-means-unset env, `ZMX_NO_DETACH_KEY`, role title, create-only payload and exit codes
- [ ] subcommands: `daemon <name>` (internal), `attach <name> [argv…]` creating the daemon when missing (as zmx),
      `list`, `kill`, `screen`, `type`, `print`, `version`
- [ ] `attach` as a terminal client:
  - raw mode, resize forwarding;
  - zmx's detach key unless `ZMX_NO_DETACH_KEY`;
  - the `ZMX_MANAGED`/`ZMX_MANAGED_CLAIM` opt-in;
  - the role title
- [ ] interop: the new CLI against a real zmx daemon
- [ ] run gates

### Task 8: Detach and supervision on Linux and Windows

**Files:**
- Create: `crates/agterm-session/src/spawn/{mod,linux,windows}.rs`, `crates/agterm-session/tests/spawn.rs`

- [ ] Linux: `setsid` in `pre_exec`, stdio to null, wait for the endpoint (herdr's `autodetect.rs`), no fork after
      threads
- [ ] Windows:
  - `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB`;
  - the shell tree in a job with `KILL_ON_JOB_CLOSE`;
  - herdr's check that the daemon is not in a kill-on-close job of its launcher;
  - the daemon started from the build-id copy
- [ ] tests on both OSes:
  - the launcher exits and the session lives;
  - a killed daemon takes its shell tree down;
  - (Windows) a daemon launched from inside a job survives the job's close;
  - (Windows) replacing the staged binary while a daemon runs succeeds
- [ ] run gates

### Task 9: macOS spawn with its own permission identity

**Files:**
- Create: `crates/agterm-session/src/spawn/macos.rs`
- Modify: `crates/agterm-session/src/spawn/mod.rs`

- [ ] read upstream's `AgtermResponsibility/Responsibility.swift`, `agterm/Ghostty/ZmxLaunch.swift` and
      `ZmxSupport.attachCommand`
- [ ] spawn the daemon with `posix_spawn`, `POSIX_SPAWN_SETSID` and `responsibility_spawnattrs_setdisclaim`; stdio to
      null; wait for the endpoint
- [ ] tests on the macOS runner:
  - the spawn attributes;
  - the launcher exits and the session lives;
  - a killed daemon takes its shell down
- [ ] run gates

### Task 10: Stage `agterm-session` in zmx's place (attach subprocess still in use)

**Files:**
- Modify: `crates/agterm-zmx/src/launch.rs` (executable and build id), `crates/agterm-ctl/src/clipboard.rs`
  (`zmx_path` → the staged `agterm-session`), `scripts/stage.sh`, `scripts/run-gpui-dev.sh`,
  `conformance/agterm_conformance/impls/base.py` (`LEFTOVER_NAMES`)

- [ ] stage `bin/agterm-session` and its build id (hash of the binary) in place of `bin/zmx` and
      `share/agterm/zmx-BUILD`; `AGTERM_ZMX_PATH` keeps naming a debug override
- [ ] `agterm-zmx` runs `agterm-session` with zmx's arguments; the GPUI pane still runs `agterm-session attach` in its
      PTY
- [ ] `agtermctl clipboard set` finds `agterm-session` beside itself; its error texts stay upstream's
- [ ] `LEFTOVER_NAMES` gains `agterm-session` (zmx stays in it while old daemons may exist)
- [ ] tests: the launch inputs, `zmx_path`, the clipboard e2e
- [ ] full GPUI conformance run in the background: every ratchet id green, the zmx, restore, relaunch,
      live-any-shell and clipboard ids included; newly green ids added
- [ ] run gates

### Task 11: Client library and the `SessionHost`

**Files:**
- Create: `crates/agterm-session/src/{client,host}.rs`, `crates/agterm-session/tests/host.rs`
- Modify: `crates/agterm-gpui/src/session_host.rs`, `crates/agterm-app/src/restore_launch.rs`

- [ ] client library:
  - list, with records straight from Info replies;
  - kill (batch), screen, type, print;
  - an attach stream: Manage + Init, then Output/Input/Resize/Role
- [ ] `SessionHost` over the library with the same errors and outcomes as `ZmxClient`:
  - the `HostError` texts;
  - `KillOutcome`;
  - never a blank screen on a failed read;
  - an empty namespace never read from a failed listing
- [ ] the frontend builds the new host; `agterm-zmx`'s tests are ported onto it, and the core tests on fakes stay as
      they are
- [ ] run gates and the GPUI conformance run in the background

### Task 12: Foreground job and process table on all three OSes

**Files:**
- Create: `crates/agterm-session/src/job/{mod,linux,macos,windows}.rs`
- Create: `crates/agterm-app/src/process_table/{macos,windows}.rs`
- Modify: `crates/agterm-app/src/process_table.rs` (becomes `process_table/linux.rs`)

- [ ] move `agterm-zmx/src/sweep.rs` behind the per-OS `job` module; macOS via `sysctl`; Windows via herdr's
      snapshot (descendants, command lines, terminate)
- [ ] `ProcessTable` for macOS and Windows, with the lossy-decode rule the Linux one documents; the quit-time capture
      in `agterm-app` uses it on every OS
- [ ] tests on every OS:
  - a foreground job found and ended;
  - an unreadable table never read as "no job";
  - a captured argv with spaces and quotes
- [ ] run gates

### Task 13: Live panes in `agterm-term` over the session stream

**Files:**
- Modify: `crates/agterm-core/src/zmx/support.rs` (structured launch output beside the shell line)
- Modify: `crates/agterm-term/src/{pane,launch,exit}.rs`
- Create: `crates/agterm-term/src/session_io.rs`

- [ ] core produces the live launch as structured data: daemon name, argv (the create-only payload), environment
      (`TERM=xterm-ghostty`, `TERMINFO`, integration variables, `ZMX_DIR`, the tracked env), cwd. The shell line
      stays for the CLI and remote paths, and the tests prove both forms agree
- [ ] the pane creates a missing daemon itself (`agterm-session daemon` with that argv, env and cwd, through Tasks
      8–9's spawn) and attaches with the in-process stream: no PTY, no subprocess, the same IO-thread batching and
      VT slicing as the PTY path
- [ ] daemon gone or refused follows the `ExitPolicy` the attach path follows today; the GPUI plan's first-paint race
      fix holds without the subprocess
- [ ] tests:
  - a live pane round trip, resize;
  - pane drop detaches and leaves the daemon running;
  - reattach shows the snapshot;
  - the daemon's environment and cwd equal what `attach` gave it
- [ ] measure 50 MiB `cat` against the Task 1 baseline; record it here
- [ ] run gates and the full GPUI conformance run in the background

### Task 14: PowerShell live sessions

**Files:**
- Modify: `crates/agterm-core/src/zmx/{shell_integration,replay}.rs`, `crates/agterm-core/src/restore/mode.rs`
- Create: `resources/shell-integration/pwsh/agterm.ps1` (path settled against the staged Ghostty resources)

- [ ] `ShellKind::Pwsh`: script (OSC 7, OSC 133, title), launch arguments, the replay script in pwsh
- [ ] eligibility on Windows (`pwsh.exe`, then `powershell.exe`, else the existing rejection with its reason)
- [ ] tests:
  - script rendering and quoting;
  - replay of an argv with spaces and quotes;
  - eligibility cases;
  - an e2e on the Windows runner: a live pwsh session reports its cwd, its foreground command is captured through
    Task 12's table and replayed in a recreated daemon
- [ ] run gates

### Task 15: Remove zmx

**Files:**
- Delete: `crates/agterm-zmx`, `scripts/build-zmx.sh`, `build/patches/zmx/`
- Modify: `build/pins.env` (drop `ZMX_*`), `crates/agterm-term/tests/launch.rs`, `README.md`,
  `conformance/docs/rust-build.md`, `.claude/skills/agterm-upstream-sync/references/areas.md`

- [ ] drop the zmx build, pins and patches (the formatter patch already moved in Task 6 if it was still needed); keep
      libghostty-vt's Zig toolchain pin
- [ ] the upstream-sync skill: a new upstream zmx change now means "port into `agterm-session`", recorded in its
      areas reference
- [ ] drop the interop tests; golden fixtures stay
- [ ] run gates and the full conformance run

### Task 16: Verify acceptance criteria

- [ ] live sessions on Linux, macOS and Windows CI: create, detach by quitting the app, reattach with the screen,
      kill, prune, reset, `clipboard set`
- [ ] a zmx daemon started by the previous build is attached by the new one (recorded once on the dev box before
      Task 15, in this plan)
- [ ] an upstream-style remote attach (`attach` over a local ssh stand-in with the remote env) gets the role title
      and the snapshot
- [ ] performance: 50 MiB `cat` and idle daemon RSS within the Task 1 baselines
- [ ] `cargo test --workspace`, clippy, fmt, `scripts/lint-report.sh`, the full conformance run; ratchet unchanged or
      grown

### Task 17: [Final] Update documentation

- [ ] README: build and staging without zmx, Windows notes
- [ ] CLAUDE.md:
  - crate map: `agterm-session` in place of `agterm-zmx`;
  - the "Linux first" line updated to three equal platforms;
  - isolation rules naming the Windows pipe prefix and the versioned daemon copies
- [ ] `conformance/docs/rust-build.md`: any capability claimed for pwsh live sessions, as `live-any-shell` was
- [ ] move this plan to `docs/plans/completed/`

## Post-Completion

**Manual verification**:
- a real Windows 11 desktop:
  - live pwsh panes survive closing the app;
  - a reboot replays captured commands;
  - an update with daemons running replaces nothing in use;
  - Windows Terminal attaches with `agterm-session attach`
- a real Mac: a tool in a live session keeps its microphone/camera permission across an app update
- an upstream Mac attaching to this machine's sessions over ssh (`agtermctl zmx attach HOST ID`), and the reverse
- the user's own machine: move off the installed agterm's zmx daemons (they stay attachable; reset when convenient)

**External**:
- the GPUI frontend on Windows and macOS is its own plan; this one gives it a working session host on both
- the future TUI and headless-core plans build on `agterm-session`'s client library and attach stream
