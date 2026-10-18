# Zig-free cross-platform core: a Rust VT core and a Rust session host

## Overview

- **Goal**: no Zig anywhere in the build, and live sessions on Windows, Linux and macOS alike. Today Zig builds two
  things:
  - libghostty-vt: `scripts/build-ghostty-vt.sh` builds the pinned, patched library with Zig 0.16 and runs `zig run`
    on Ghostty's terminfo source;
  - zmx, the live-session daemon: `scripts/build-zmx.sh`. zmx is also POSIX-only (`forkpty`, `fork`, `setsid`,
    `AF_UNIX`) and cannot build for Windows.

  This plan replaces both with Rust in two parts:
  - **Part A, the VT core**: a new crate `agterm-vt` holds Ghostty's terminal in Rust, taken as code from rustty. It
    replaces libghostty-vt and its two crates.
  - **Part B, the session server**: a new crate `agterm-session` holds live sessions on herdr's architecture
    (`https://github.com/herdrdev/herdr`, Apache-2.0), taken as code: one background server per agterm instance
    owns every live pane's PTY and VT state. It replaces the zmx binary and the `agterm-zmx` crate. Its VT state is
    `agterm-vt`, where herdr uses libghostty-vt.
- **Why one plan** (user, 2026-10-18; it merges `20261017-rust-vt-core` and `20261016-cross-platform-session-host`):
  - the session server needs a VT state, a formatter and the stream continuation on all three OSes, and `agterm-vt`
    gives them without Zig;
  - one cross-platform CI workflow and one `NOTICE` serve both parts;
  - Part A lands first, so Part B builds on the new core only, and Part B's last task removes the last Zig.
- **Platforms** (user, 2026-10-16): Windows, Linux and macOS are equal targets, and the GPUI frontend is the
  cross-platform one. Neither the VT core nor the live-session layer is forked per platform.

### Part A decisions (user, 2026-10-17)

- **Scope: VT and terminfo.** Both of the Ghostty build's Zig uses go.
  - The terminfo entry `xterm-ghostty` is encoded by Rust code ported from rustty's terminfo crate.
  - Ghostty's themes and shell integration stay files taken from the pinned source archive and its theme package.
    Neither needs Zig.
- **The code comes from rustty** (`https://github.com/AirelyDev/rustty`, MIT), a Zig→Rust port of Ghostty.
  - It is taken as code, not as a dependency: copied, trimmed to what agterm uses, and reworked until it passes the
    workspace lints.
  - From then on it is our code, citing the Ghostty revision it follows.
- **The wrapper layer goes.** `agterm-term` (and the session server) call `agterm-vt`'s Rust API directly.
  - That API is Ghostty's own (`Terminal`, `Screen`, `PageList`, `RenderState`, the formatter, the search, the
    encoders, the Kitty storage), plus the embedder logic that rustty keeps in its C layer and agterm reaches today.
    It is not a re-creation of the C API.
  - `agterm-ghostty-vt` and `agterm-ghostty-vt-sys` are deleted.
- **One crate.**
  - The parts of rustty's leaf crates that the terminal uses (util, datastruct, unicode, simd, os, lib, zigstd,
    render-types, identity, terminfo) become private modules of `agterm-vt`.
  - Nothing unused is copied: no xev, font, z2d, wuffs, tmux viewer, snapshot codec, APC glyph protocol, inspector
    or C ABI exports.
- **Under the workspace lints.** The ported code passes the full `[workspace.lints]` and the `clippy.toml`
  thresholds. It is not vendored outside the lints, and it is not excused crate-wide.
- **All three OSes at once.** `agterm-vt` is `any` in the crate map. It builds and runs its tests on macOS and
  Windows, not only through `cargo check`.
- **The Ghostty pin stays.**
  - `LIBGHOSTTY_VT_REV` is `81598385` (2026-10-13). rustty's oracle is ghostty `246f70287` (2026-10-09), and its
    Unicode tables come from `12752b2ac`.
  - The port absorbs everything between those and the pin, for every Ghostty directory a ported module follows.
  - The app keeps every VT feature it has, because a Ghostty pin move is a change of its own.
- **The patch `build/patches/ghostty-vt/0001` disappears.**
  - It only exposed, through the C API, the Kitty placement-id namespace and a relative placement's parent and
    offsets.
  - With the storage in Rust, `agterm-term` reads those fields directly.
- **Names stay** (planning decision, 2026-10-18, after review): `LIBGHOSTTY_VT_*`, `scripts/build-ghostty-vt.sh`,
  `ghostty_vt_prefix` and the prefix layout keep their names. Only the Zig pins and the library leave them.
  - The pin still names the revision of Ghostty's VT library sources that `agterm-vt` follows.
  - 58 files cite `LIBGHOSTTY_VT_REV` as their Ghostty revision.
  - Renaming to `GHOSTTY_REV` would collide with upstream's pin of that name, which `pins-delta.sh` reports.
  - Keeping the names also keeps every reader of the pin and the prefix working: the live process tests, the WPE
    provider test, the lavapipe selftest, the config-key test.

### Part B decisions (user, 2026-10-18; they replace the session plan's of 2026-10-16)

- **herdr's architecture, whole.** Live sessions follow herdr's design, taken as code with herdr's notices:
  - one background session server per agterm instance (its namespace is keyed by the state dir). It is started on
    demand, detached from the app, and it owns every live pane's PTY and VT state;
  - clients connect to it, detach and reattach, and the GPUI app is one of them;
  - the server's control socket API, its update handoff (the PTYs pass to the new server without killing a session),
    its spawn and detach per OS, its process inspection;
  - herdr's PTY actor over `portable-pty` (ConPTY on Windows) with its fixes in `vendor/portable-pty`, and IPC over
    `interprocess` (Unix socket and named pipe, owner-only DACL).
- **The GPUI pane receives the raw stream and keeps its own VT** (user, 2026-10-18).
  - On attach, the server sends a snapshot of the pane, then its raw output. (Task 2: this is not herdr's
    `AttachTerminal`, which renders server-side; the raw mode is agterm's, on herdr's connection framework.)
    `agterm-term` feeds both into its own `agterm-vt` terminal, as it feeds a PTY today.
  - Rendering, selection, search and Kitty graphics stay in `agterm-term` and `agterm-gpui`.
  - The server's VT exists for the snapshot, screen reads, typing and a future headless core.
  - herdr's surface deltas (server-side rendering for TUI clients) are not ported. The future TUI-frontend plan adds
    them as another attach mode, and the protocol leaves room for one.
- **Live sessions stay a mode.** Only `restoreMode live` panes run in the server. Plain panes keep the direct PTY in
  `agterm-term`, and an instance with live sessions off starts no server.
- **What changes for callers.**
  - **No zmx anywhere** (user, 2026-10-18), not even in the control protocol:
    - the code, scripts, environment variables, paths and docs of the Rust build carry no `zmx`;
    - upstream's control-protocol group `zmx.*` becomes `live.*`, and `agtermctl zmx` becomes `agtermctl live`, a
      deliberate deviation from upstream that `conformance/docs/rust-build.md` records;
    - the attach contract's variables become `AGTERM_LIVE_*`, its role title `agterm-live-role`, the socket
      directory `/tmp/agterm-live-<hash>`;
    - `zmx` stays only where upstream is described or run, and in the environment scrub that keeps the user's
      installed zmx-based agterm away. Upstream is described in the suite's tests, catalog and markers, which are
      shared with the Swift references and translated by each adapter's vocabulary object, and in the upstream-sync
      tooling. It is run by the Swift adapters, which drive upstream's own zmx.
  - zmx's daemon wire protocol goes. The server speaks herdr's wire.
  - An upstream Mac can no longer view this machine's sessions, or the reverse, because both sides' names differ.
    The same goes for a new build and an older Rust build.
  - Old zmx sessions are not migrated (user, 2026-10-18). The new build does not attach to them.
- **What stays.**
  - The shape of the attach contract that `agterm-core`, the remote path and the suite drive (Technical Details >
    Attach contract), under its new names. `agterm-session attach` honors it, so `agterm-core::live::support`,
    `agterm-core::remote::ssh` (upstream's `attachCommand`) and the suite's leading client need only the rename.
  - The semantics, fields and error strings of upstream's control-protocol group, under `live.*`, as far as they do
    not name zmx. A "daemon" in its results becomes a server session, named as `agterm-core::live` names them.
  - `agterm-core`'s restore and replay and its remote attach are upstream's and stay. herdr's layout persistence
    and its ssh remote are not ported beside them. Task 2 records any concrete gap as a ➕ task.
  - The live-session policy (today's `agterm-core::zmx`, `agterm-core::live` from Task 19): naming, reap, prune,
    lead, reset, replay. Only its Unix-only spots gain Windows forms: `socket_directory`'s hardcoded `/tmp` path, the
    `/bin/sh -c` attach line, a few `cfg(unix)` reads.
  - `agtermctl clipboard set` keeps its behavior over the server's API. Its error texts stay upstream's where they
    name no zmx, and Task 28 records each one that changes.
  - The conformance ids stay green and keep their names, because the tests keep upstream's vocabulary and each
    adapter translates it (Task 19). That covers the 43 zmx ids, `test_live_any_shell`, `test_restore`, `test_relaunch`
    and `test_clipboard_set_*` move with the harness's Rust adapter onto the new server.
- **Windows live sessions run PowerShell 7** with agterm's own integration script, a Rust-only extension like bash
  and fish today (decision of 2026-10-16, kept).
- **Details come from herdr's code at execution time.** Task 2 studies herdr's server at a pinned commit and writes
  its findings into this plan before Part B's code tasks start: the module map, the keep/cut verdicts, the wire
  protocol, the Windows handoff. Part B's tasks below are written at the level of decisions. The study refines them
  and adds ➕ tasks where needed.
- **Future** (not in this plan, but the design must allow it): a TUI frontend for headless servers (herdr's surface
  deltas), and a headless core process that keeps the model and the control socket alive while no client is
  attached. The session server is that core's natural home. `agterm-vt` and `agterm-session` stay
  frontend-agnostic, and `agterm-term` stays toolkit-neutral.

## Context (from discovery)

Paths are as of `main` at `57adfbe`, spot-checked on 2026-10-18.

### VT (Part A)

- `crates/agterm-ghostty-vt` is about 5.9k lines over the C API, with about 100 public functions (Task 1 counts them). Its tests are 49 in
  `src/` and 21 in `tests/` (`effects.rs`, `link.rs`, `sys.rs`, `sys_sealed.rs`).
- `crates/agterm-ghostty-vt-sys` holds the checked-in bindgen output, a `build.rs` that links the prefix, and
  `tests/prefix_keys.rs`. Those are the only tests that the Ghostty and zmx prefixes are keyed independently.
- `crates/agterm-term` is the only consumer.
  - 14 modules `use agterm_ghostty_vt as vt`: `pane`, `render`, `effects`, `input`, `selection`, `gesture`,
    `search`, `kitty`, `text`, `wheel`, `exit`, `actions`, `lib`, `png`.
  - Beyond the obvious types, it calls:
    - write control: `vt_write_until_ground`, `at_ground`, `reset`, `set_mode_default`;
    - state queries: `cursor_at_prompt`, `viewport_top`, `viewport_at_bottom`, `total_rows`, `hyperlink_uri`;
    - Kitty configuration: `set_kitty_image_media`, `set_kitty_image_storage_limit`;
    - `version()`, which `launch.rs` and `pane.rs` use.
  - Its tests run on real PTYs.
  - `crates/agterm-term/src/bench.rs` is the pane-level harness: `cat` of 50 MiB plain and through zmx, a Kitty PNG
    burst, resize reflow, VT lock latency. It is run by hand with `--ignored`.
  - `agterm-gpui` sees only `agterm-term`'s own types.
- Readers of the pin and the prefix, beyond the scripts:
  - `crates/agterm-gpui/tests/{privileges,quit}.rs` call `ghostty_vt_prefix`. Without it, the live-session process
    tests skip silently;
  - `crates/agterm-wpe-sys/tests/provider.rs` copies and runs `build-ghostty-vt.sh --print-prefix` and bumps
    `LIBGHOSTTY_VT_REV`;
  - `conformance/selftests/test_fetch_lavapipe.py` edits `LIBGHOSTTY_VT_REV`;
  - `crates/agterm-core/src/ghostty_config/{tests,mod}.rs` check and re-export the generated constant;
  - 48 source and doc files cite `LIBGHOSTTY_VT_REV`.
- Scripts:
  - `scripts/build-ghostty-vt.sh`: the Zig build, `zig run` for terminfo, the themes and the shell integration;
  - `scripts/gen-ghostty-vt-bindings.sh`;
  - `scripts/lib/native.sh`: `ghostty_vt_prefix*`, `prefix_key` (hashes pins and `*.patch` only), `find_zig`,
    `host_zig_arch`;
  - `scripts/stage.sh`;
  - `scripts/lint-report.sh` (excludes the -sys crate; lists complexity `#[expect]`s only);
  - `.claude/skills/agterm-upstream-sync`: `SKILL.md`, `scripts/pins-delta.sh`, `references/{areas,plan-template}.md`.
- Pins and CI:
  - `build/pins.env`: `LIBGHOSTTY_VT_*` with its Zig pins;
  - `build/patches/ghostty-vt/`;
  - `.github/workflows/rust-linux.yml`: Zig loops over `LIBGHOSTTY_VT ZMX`, and runs `build-ghostty-vt.sh` in three
    jobs. Its cache keys hash the pins.
- rustty at `b57bcee7` (2026-10-12), oracle ghostty `246f70287`:
  - **Size**: `rustty-vt` is about 264k lines with tests, with 3456 `#[test]`s. Its largest parts are `pagelist` 33k,
    `kitty` 30k, `terminal` 23k, `snapshot` 21k, `osc` 15k, `screen` 15k, `search` 13k, `stream_terminal` 10k,
    `input` 10k, `tmux` 9k and `apc` 5k.
  - **The modules depend on each other in cycles, as Ghostty's Zig files do**:
    - `screen` uses `formatter` and `kitty`;
    - `terminal` uses `kitty`, `apc::glyph` and `stream_terminal`;
    - `stream_terminal` builds XTGETTCAP from `rustty_terminfo` and reaches `snapshot`;
    - `pagelist` uses `compress` (LZ4 idle compression, `NodeData::Compressed`).
  - **Modules no plan has placed yet**: `compress`, `kitty/dnd*` (OSC 72), `kitty/clipboard_grants`,
    `kitty/graphics_animation`, `string_map`, `xt_checksum`, `log_capture`, `fastprint`, `fixed_writer`.
  - **`rustty-vt-c`**: about 48k lines of logic besides the exports, and agterm reaches part of it:
    - `terminal/write.rs`: `vt_write_until_ground`, the continuation;
    - `terminal/effects.rs`: the effects plumbing;
    - `kitty_graphics.rs`: `placement_render_info`;
    - `render.rs`, `search.rs`, `selection_gesture.rs`;
    - the getters.
  - **Dependencies**: util, datastruct (9k), unicode (6k, generated tables), simd (3k), lib (4k), zigstd (7k),
    render-types, identity, terminfo, os (16k; only its `page_allocator`, `path`/`file`/`tempdir` and per-OS bits),
    font (only `opentype`, for the glyph protocol), oniguruma (optional, off), wuffs (C: Kitty zlib, PNG,
    swizzles).
  - **Unicode tables**: dumped from Zig uucode 0.2.0 @ `9d555245` (Unicode 18) with Ghostty's `uucode_config.zig` at
    `12752b2ac`. rustty has no Rust generator.
  - **Unsafe**: about 7.9k occurrences of `unsafe`, most of them in page memory.
  - **Lints**: its workspace allows a list of "port-shape" clippy lints that ours denies.
  - **Identity**: it renames Ghostty's identity to rustty's (its rule R2), for example the terminfo entry.
  - **Toolchain**: 1.93.1, no nightly features. Ours is 1.98.1.
  - **Trust**: the clone is untrusted data. Its build scripts and proc macros (`crates-sys`, bindgen, wuffs) are
    never run.

### Session host (Part B)

- herdr (`https://github.com/herdrdev/herdr`, Apache-2.0, v0.9.5; pinned by Task 2 as `HERDR_REV` `143bf5f`,
  master of 2026-10-16, cloned at `.upstream/herdr`):
  - `src/server/`: the background server (`autodetect.rs` spawn and detach, `socket_paths.rs`, `clients.rs`,
    `client_transport.rs`, `client_accept.rs`, `terminal_attach.rs` raw attach, `alt_screen_read.rs`, `handoff.rs`,
    `headless*`, `shutdown.rs`);
  - `src/protocol/`: the wire (`wire.rs`, `endpoint.rs`) and the TUI surface deltas (`surface_*`, `render_ansi.rs`,
    not ported);
  - `src/pane.rs` with `src/pane/**` (about 18.8k lines: the per-pane runtime, PTY spawn, OSC, XTGETTCAP, Kitty
    keyboard), `src/terminal/` (runtime registry, history reads), `src/pty/` (the PTY actor and backends),
    `vendor/portable-pty`;
  - `src/ipc.rs` (local sockets and the named-pipe listener with its SDDL), `src/client/` (the client side of
    `AttachTerminal`: `mod.rs`, `attach.rs`, `handshake.rs`), `src/session.rs` (session naming and stop);
  - the update handoff (`src/server/handoff.rs`, `src/api/server.rs`, with the per-pane state struct in
    `src/handoff_runtime.rs`), `#[cfg(unix)]` only, passing PTY descriptors over `SCM_RIGHTS`. Windows has none, so an
    update or server restart ends the sessions there;
  - Windows detach: `DETACHED_PROCESS` only, with an in-server check for a kill-on-close job
    (`src/platform/windows.rs`);
  - licensing: Apache-2.0 in `Cargo.toml` and `LICENSE`. Its files carry no headers, and there is no NOTICE;
  - `src/persist/`: layout snapshots restored after a server or machine restart (the processes do not survive);
  - `src/remote/`: remote attach over ssh, running herdr on the remote host;
  - `src/platform/`: per-OS spawn, job objects, process snapshots with command lines, the pipe SDDL;
  - `src/api/`, `src/cli/`: the socket API and the CLI;
  - `crates/ghostty-vt`: herdr's binding to a vendored libghostty-vt (Zig). `agterm-vt` replaces it;
  - dependencies of note: `tokio`, `interprocess`, `portable-pty` (vendored), `bincode`, `windows-sys`.
  - The server and protocol modules are about 36k lines with the TUI parts.
- `crates/agterm-host/src/lib.rs` `SessionHost`: `is_available`, `list`, `kill`, `screen`, `type_input`,
  `kill_batch`, `foreground_job`, `job_running`, `end_job`. The frontend builds the host in
  `agterm-gpui/src/session_host.rs` and `agterm-app/src/restore_launch.rs`.
- `crates/agterm-zmx`:
  - runs the zmx binary: `list`, `kill <names…> [--force]` (batched in `inventory.rs`), `screen [--all]`, `type`;
  - `launch.rs` reads the live-launch inputs: `AGTERM_ZMX_PATH`, `share/agterm/zmx-BUILD`, the login shell, the
    Ghostty resources, the socket namespace;
  - `sweep.rs` hangs up a killed shell's foreground job.
- `crates/agterm-core/src/zmx/`:
  - `support.rs` builds the attach command (run through `/bin/sh -c`) and the configuration;
  - `lifecycle.rs` holds the `zmx list` parser, the leader map and the reap policy;
  - also `lead.rs`, `reset*`, `replay.rs`, `screen.rs`, `build_record.rs`;
  - `shell_integration.rs`: zsh as upstream; bash and fish as a Rust-only extension (capability `live-any-shell`);
  - `mod.rs` `socket_directory` hardcodes `/tmp/agterm-zmx-<FNV-1a 64>`.
- `crates/agterm-core/src/commands/zmx*` and `crates/agterm-ctl/src/zmx.rs`: upstream's `zmx` command group over
  `SessionHost`.
- Unix-only spots in `agterm-core`: `zmx/support.rs` (`cfg(unix)`), `restore/mode.rs` (`cfg(unix)`),
  `commands/hud.rs` (`std::os::unix::fs`).
- `crates/agterm-term`, the pane core:
  - `Pane::launch` runs a `Launch`. A live launch is `zmx attach` in a `portable-pty` PTY, so the attach creates the
    daemon. The daemon inherits the pane's environment (`TERM=xterm-ghostty`, `TERMINFO`, the integration variables,
    `ZMX_DIR`), its cwd and `ZMX_TRACK_ENV`.
  - A plain launch is the command or the login shell.
  - Measured on 2026-10-16: a 50 MiB `cat` takes 0.73–1.0 s through zmx, against 0.49–0.53 s plain.
- What the suite pins about reattach comes from zmx's `serializeTerminalState`, with its patches:
  - the scrollback unstyled, then a clear and the visible screen with extras;
  - color overrides;
  - DECSET 2026 off;
  - `0003-first-attach-snapshot`'s reset prefix;
  - zmx's private Ghostty patch `0001-formatter-unstyled-blanks`.

  The new snapshot keeps that behavior.
- `crates/agterm-app/src/process_table.rs`: the Linux `ProcessTable` over `/proc`. Its consumer is the quit-time
  command capture for replay. There is no macOS or Windows one.
- Upstream macOS supervises live sessions with `agterm-session-host`. Permission identity comes from
  `responsibility_spawnattrs_setdisclaim` (`AgtermResponsibility/Responsibility.swift`), not from the Mach bootstrap
  context that herdr sets.
- Conformance:
  - the harness keys live sessions on zmx: `impls/base.py` (`zmx_socket_directory`, `LEFTOVER_NAMES`, leftover
    detection and the reap by `zmx.kill`), `zmx_client.py` (it drives the zmx executable from `zmx.tree` as a leading
    client), `impls/swift_linux.py`, `cli_format.py`;
  - tests: `tests/protocol/test_zmx.py`, `test_live_any_shell.py`, `tests/cli/test_cli_only.py`,
    `test_cli_output.py`, `tests/behaviors/test_relaunch.py`, `tests/ui/test_live.py`; 43 ratchet ids name `zmx`.
- CI: `.github/workflows/{rust-linux,reference-macos,spike-macos,conformance-gate}.yml`. There is no Rust workflow
  for macOS or Windows yet.

### Patterns and dependencies

- Patterns found:
  - the workspace takes every `crates/*` as a member (`members = ["crates/*"]`);
  - every `agterm-ghostty-vt` module cites the C header it covers;
  - `agterm-term` keeps the terminal behind a lock, because handles are `Send` but not `Sync`;
  - effects are callbacks the terminal runs synchronously inside `vt_write`;
  - PNG decoding is a process-global hook, and `agterm-term/src/png.rs` decodes with `png`;
  - logging goes through `tracing`, and `scripts/check-tracing-features.sh` keeps `log` out of the graph;
  - generated code changes only through its generator script, with `--check` in CI (`gen-wpe-bindings.sh`).
- Dependencies (popular-tools rule):
  - already in the workspace: `png`, `bitflags`, `tracing`, `portable-pty`;
  - new: `miniz_oxide` (zlib, replacing wuffs'), `interprocess`, `windows-sys` (Microsoft) with the features herdr
    uses, `postcard` (Task 2: in place of herdr's unmaintained `bincode`), and `wmi` for the Windows spawn from a
    kill-on-close job; no async runtime (Task 2 chose threads);
  - only where needed: `hashbrown` (page memory, only if needed), `memchr` (only if measurement asks for it);
  - nothing C, nothing Zig.

## Development Approach

- **testing approach**:
  - regular (code, then tests in the same task) everywhere. The VT port's tasks bring rustty's tests along with
    the code, then add what agterm needs.
- complete each task fully before moving to the next; small, focused changes.
- **CRITICAL: every task MUST include new/updated tests** for its code changes:
  - unit tests for new and modified functions;
  - success and error paths;
  - an existing test updated when behavior changes.
- **CRITICAL: all tests must pass before the next task starts.** The gates of CLAUDE.md apply to every task:
  - `cargo fmt --all --check`, clippy `-D warnings` (including `--all-features`, so every `agterm-vt` feature lints
    clean), `scripts/lint-report.sh`, `cargo test --workspace`;
  - tasks that touch the GPUI binary or staging also run the full conformance run in the background, and no ratchet
    id goes red.
- **The VT port follows the module graph, not a layer order.** Ghostty's terminal modules use each other in cycles.
  - Each VT task ports one strongly connected group whole: everything it needs exists after earlier tasks, so its
    tests pass when it ends.
  - Task 1 computes the groups from rustty's `use crate::` graph, with the cut modules removed, and corrects the
    task boundaries below if the graph differs.
  - No stub modules: `todo!`/`unimplemented!` are denied, and a stub would fail the gates.
- **CRITICAL: update this plan when scope changes.**
- **Backward compatibility**:
  - until the switch (Task 17), the old VT stack keeps working untouched;
  - until the switch (Task 28), zmx stays the session host; until its removal (Task 32), it stays built;
  - the app keeps every behavior throughout.
- **Windows and macOS**:
  - code for those OSes is debugged and iterated on in the local VMware VMs `Win11-admin` and `macOS 26` (user,
    2026-10-18), driven through `vmrun` from the dev box, admin password for both 1234, it's throw away VМs, so having password here is fine;
  - it is gated on CI runners (Task 3's workflow) from the first task that has such code, not only cross-checked.
- **Isolation** (CLAUDE.md > Never touch the user's running agterm) holds everywhere, in the VMs too:
  - every test session server runs under a private namespace (socket directory; on Windows, also a private pipe
    prefix);
  - test reaps go through that namespace's server only, never `pkill` and never by process name;
  - inherited `AGTERM_*` and `ZMX_*` are stripped from every child environment; the server then sets the session
    variables of the attach contract itself;
  - `tic` and `infocmp` in tests use a temp directory (`-o`, `-A`), never `~/.terminfo`.
- **Untrusted source**:
  - rustty's files are read and copied, never built in place;
  - nothing of its build scripts, proc macros or `-sys` crates runs;
  - test data is copied only after its license is checked.
- **Provenance**:
  - every ported `agterm-vt` module names the Ghostty file it follows at the pin and the rustty path it came from
    (MIT);
  - the Unicode generator names uucode (MIT);
  - herdr-derived files get a header naming their herdr source file, Apache-2.0 and "modified" (herdr's files have
    none to keep);
  - the root `NOTICE` carries Ghostty, rustty, uucode and herdr, plus wuffs (Apache-2.0) if the swizzles follow its
    code closely.

## Testing Strategy

- **unit tests**: required in every task (see Development Approach). For the VT port, these are rustty's tests for
  the ported modules, plus tests for what this port changes: the identity restore, the absorbed drift, the replaced
  wuffs paths, the cut branches.
- **VT differential tests** (Task 16):
  - one corpus of byte streams goes through both `agterm-vt` and the old `agterm-ghostty-vt`;
  - the tests live in a test-only crate, `crates/agterm-vt-diff` (`publish = false`), that depends on both. The
    cross-platform workflow builds `-p agterm-vt` only and never needs the Zig prefix, while `rust-linux.yml`, which
    has the prefix, builds the whole workspace;
  - they compare text (plain, VT, HTML), cell styles, cursor, modes, replies to the PTY, effects, selections, search
    matches and Kitty placements;
  - before the old crates go (Task 18), the results become golden fixtures, so the coverage outlives the oracle.
- **protocol tests**: every message of the ported wire round-trips, version negotiation refuses a mismatch, and
  malformed frames are refused.
- **server e2e tests** (all three OSes on CI):
  - start the server detached from the test process, create a session, attach, type, read `screen`, detach;
  - kill the test's side, reattach and see the same screen;
  - `kill` and `kill --force`, `list`, `print`, and an update handoff.
- **conformance**: the GPUI run on Linux, in the background, keeps every ratchet id green, including the live-session ids (renamed in Task 19),
  `test_live_any_shell`, `test_restore`, `test_relaunch` and `test_clipboard_set_*`. Newly green ids go in through
  `check_ratchet.py`.
- **cross-platform**:
  - from Task 3 on, a workflow runs `agterm-vt` on macOS and Windows; from Task 20 `agterm-session`, and from Task
    21 `agterm-proto`, `agterm-host` and `agterm-core`;
  - the VT memory core's tests also run under Miri, with the nightly date pinned in the workflow.
- **performance** (numbers recorded in this plan, so later checks compare against records, not a deleted library):
  - VT: `agterm-term/src/bench.rs` runs before and after the switch (Task 17), and the new core may be at most 10%
    slower;
  - VT micro: a small `#[ignore]`d parse and render-state measurement in `agterm-vt` runs against libghostty-vt
    while it exists (Task 16) and records both numbers;
  - session: a 50 MiB `cat` through a live pane is no slower than through zmx, and the server's idle RSS for N panes
    is no larger than N idle zmx daemons' (baselines from Task 2).

## Progress Tracking

- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- update the plan if implementation deviates from the original scope

## Solution Overview

### Part A: `agterm-vt`

- **Source and provenance.**
  - The source is rustty at a commit pinned in `build/pins.env` (`RUSTTY_REPO`, `RUSTTY_REV`). The pin records
    provenance and tells future syncs where to start. Nothing fetches it at build time.
  - Every module's doc header cites the Ghostty file at `LIBGHOSTTY_VT_REV` and the rustty path it came from.
- **What is kept.**
  - A rustty module is kept when it is part of libghostty-vt at the pin and reachable from what agterm calls today,
    so the behavior on agterm's paths stays the library's.
  - Everything else is cut, with its branches removed from the kept modules: the snapshot codec, the tmux viewer, the
    APC glyph protocol, and the idle page compression (libghostty-vt has it, agterm never enables it).
  - Task 1 records the verdict for each module named in Context, including DnD, clipboard grants, animation and the
    small helpers.
- **Shape.**
  - Modules mirror Ghostty's file layout (`terminal/page`, `terminal/page_list`, `terminal/screen`,
    `terminal/terminal`, `terminal/stream`, `terminal/parser`, `terminal/osc`, `terminal/render`,
    `terminal/formatter`, `terminal/search`, `terminal/kitty`, `input/key_encode`, `input/mouse_encode`, `input/paste`,
    `terminfo`, `unicode`), so a Ghostty diff maps onto one module.
  - rustty's leaf crates fold in as private support modules (`mem`, `datastruct`, `simd`, `os`), cut to the items in
    use.
  - An `embed` module holds the logic rustty keeps in its C layer and agterm reaches (Task 15).
  - The public API is what `agterm-term` and the session server use, nothing wider.
- **Lints without excusing.**
  - rustty's port-shape allowances are not carried over.
  - Ported functions over the `clippy.toml` limits (80 lines, cognitive complexity 15, 6 arguments, nesting 5) are
    split.
  - An outer `#[expect(clippy::..., reason = "...")]` on one item is allowed only where Ghostty's own structure
    demands it: the parser's state table, a generated table. `scripts/lint-report.sh` lists the complexity ones, and
    each task's review reads the rest.
  - Casts follow pedantic: `try_from` where a value can be out of range, and a documented `as` only where the range
    is proven.
- **Unsafe policy.**
  - The ~7.9k `unsafe` sites shrink to a small audited core. Page memory sits behind a handful of types in
    `mem`/`datastruct`: the single-buffer page layout and `OffsetBuf`, the bitmap allocator, the ref-counted set, the
    page-local hash map, intrusive node lists and the page pool.
  - Every `unsafe` block carries its `// SAFETY:` and one operation.
  - Above that core, `page`, `page_list`, `screen` and `terminal` stay safe Rust.
  - The core's tests run under Miri.
  - The page layout itself is kept: it is what makes Ghostty's scrollback, resize and reflow behave and perform as
    they do.
- **Effects.**
  - The callbacks the C API ran inside `vt_write` become a trait implemented by the embedder (`agterm-term`; the
    session server uses a near-empty one). They cover replies, title, pwd, bell, notifications, progress, program
    status, clipboard, synchronized output, color-scheme and size reports.
  - The terminal calls the trait synchronously from `stream_terminal`'s handler with a read view of itself, the role
    `HeldTerminal` played.
  - No process globals. Logging goes to `tracing`, and Kitty images decode through `png` inside `agterm-vt`.
- **Identity.**
  - rustty's renames are undone. agterm-vt reports what libghostty-vt reports at the pin: `xterm-ghostty`, its
    XTGETTCAP answers, device attributes, the version string behind XTVERSION.
  - The differential tests and the terminfo check pin this.
- **Terminfo as generated source.**
  - `scripts/gen-ghostty-terminfo.sh [--check]` runs `agterm-vt`'s generator and writes the checked-in
    `crates/agterm-vt/terminfo/xterm-ghostty.terminfo`.
  - `build-ghostty-vt.sh` only runs `tic` on that file (`-x -o <prefix>/share/terminfo`), so the native script never
    needs cargo.
  - `prefix_key` gains the file's hash.
- **No SIMD library.**
  - The scalar paths stay, plus `memchr` (pure Rust, all three OSes) where measurement shows a hot scan.
  - The performance gate decides whether more optimization comes before the switch.
- **Zig leaves the Ghostty build in one change (Task 18).**
  - `scripts/build-ghostty-vt.sh` keeps its name and options and stops building a library. It fetches and checks the
    source archive and the theme package, installs the shell integration and the themes, and compiles the checked-in
    terminfo.
  - The prefix keeps its layout (`share/ghostty/...`, `share/terminfo`, `share/doc/ghostty-vt/LICENSE`,
    `share/agterm/ghostty-vt-PINS`) without `lib/` and `include/`.
  - `LIBGHOSTTY_VT_ZIG_*` goes, and the rest of the pin stays.

### Part B: `agterm-session`

```
agterm-core::live (policy unchanged)       agtermctl live … / clipboard set (upstream semantics, renamed)
        │ SessionHost
agterm-session ──────────────────────────────────────────────────────────────────
  server     one per instance (herdr's server): started on demand, detached per OS,
             owns every live pane: PTY (portable-pty: openpty │ ConPTY) + agterm-vt state
  protocol   herdr's framing, postcard, a stable version envelope; one endpoint (Unix socket │ named pipe)
  api        list / new / kill (batch, force) / screen / type / print
  attach     snapshot + continuation, then the offset-tagged raw output; input, resize, metadata back
  handoff    update without ending sessions: Unix by herdr's SCM_RIGHTS, Windows by duplicated handles
  client     library: the api + the attach stream (used in process by agterm-term)
  host       SessionHost impl (replaces today's zmx client), foreground job per OS
  bin        agterm-session: server │ attach (the kept attach contract) │ list │ kill │ screen │ type │ print │ version
agterm-term  Pane IO backend: PTY (plain panes) │ attach stream (live panes, no PTY, no subprocess)
agterm-vt    the VT state of both the pane and the server
```

- **One server per instance.** It is the `agterm-session` binary started as `agterm-session server` and detached
  from the app, so live panes outlive the app. The app, `agtermctl`, the suite and remote viewers are its clients.
  The server links no GPUI.
- **Rollout in two steps**, so the ratchet never goes red:
  1. Task 28 stages `agterm-session` in zmx's place. The GPUI pane still runs `agterm-session attach` in a PTY,
     as it runs today's attach subprocess.
  2. Task 30 swaps the pane to the in-process attach stream. The pane asks the server to create its session (argv,
     environment, cwd, the tracked env) and attaches over the socket.
- **Namespace.**
  - Unix uses `/tmp/agterm-live-<FNV-1a 64 of the state dir>` for the socket directory, the shape of today's, so
    the harness and the isolation rules keep it.
  - On Windows, the directory is `%LOCALAPPDATA%\agterm\live\<the same hash>`, and the endpoint is the named
    pipe `agterm-live-<FNV-1a 64 of the directory path>` (Task 2, "What Task 2 settles" > Windows namespace).
- **In-process attach.** `agterm-term` opens the session's stream, claims or follows the lead as `agterm-core`'s
  lead policy says, sends its grid size, and from then on maps:
  - snapshot (with its continuation) and Output → `vt_write`, with the pane's own PTY replies suppressed (the
    server answers queries; Task 2 > Attach);
  - `Pane::write` → Input;
  - `Pane::resize` → Resize;
  - Role changes → the lead state core already tracks.

  Plain panes keep the PTY path.
- **Platforms.**
  - **Linux**: spawn with `setsid` in `pre_exec`, PTY via `openpty`.
  - **macOS**: `posix_spawn` with `POSIX_SPAWN_SETSID` and `responsibility_spawnattrs_setdisclaim`, so live sessions
    keep their own permission identity across app updates, as upstream's session host does.
  - **Windows**: ConPTY. The server detaches with herdr's `DETACHED_PROCESS` plus `CREATE_NEW_PROCESS_GROUP`;
    inside a kill-on-close job it adds `CREATE_BREAKAWAY_FROM_JOB`, and where the job refuses breakaway it starts
    through herdr's WMI `Win32_Process.Create` (Task 2). Each pane's shell tree runs in a job object with
    `KILL_ON_JOB_CLOSE`, so killing a session ends its processes, as SIGHUP does on Unix.
- **Updates.**
  - The build id is a hash of the staged `agterm-session` binary, written to `share/agterm/session-BUILD`.
  - When the app finds a server of another build, it asks it to hand off to the new binary: herdr's `SCM_RIGHTS`
    handoff on Unix; on Windows the duplicated-handle handoff Task 2 proved, behind its layout gate.
  - A handoff that fails falls back to `build_record.rs`'s existing "predates the last update" path, which offers the
    usual reset and kills nothing behind the user's back.

## Technical Details

### `agterm-vt`

- **Crate**: `crates/agterm-vt`.
  - Targets: the lib, plus two feature-gated generator bins (`gen-unicode`, `gen-terminfo`) that the generator
    scripts run.
  - Under the workspace lints.
  - Dependencies: `bitflags`, `tracing`, `png`, `miniz_oxide`, `libc` and `windows-sys` for the page allocator's
    per-OS forms (`mmap`, `VirtualAlloc`), `hashbrown` and `memchr` only if needed. Task 4 added `bytemuck`: every
    value stored in page memory is `Pod`, read through checked casts, which keeps the page structures free of
    `unsafe`.
- **Public API**: the surface `agterm-term` uses today, mapped onto Ghostty's types. Task 1 completes it by
  inventorying the public functions of `agterm-ghostty-vt` that `agterm-term` calls. The session server also needs the
  stream continuation.
  - `Terminal`: `new(options)`, `vt_write`, `resize`, viewport scrolling (`Top`, `Bottom`, `Row`, `Rows`, `Prompts`),
    modes (`Mode::dec`, `SYNC_OUTPUT`, `GRAPHEME_CLUSTER`, `COLOR_SCHEME_REPORT`), colors, scrollback limits,
    `select_all`, mouse shape, `paste`.
  - Embedder logic from rustty's C layer:
    - writing: `vt_write_until_ground`, `at_ground`, `reset`, `set_mode_default`;
    - queries: `cursor_at_prompt`, `viewport_top`, `viewport_at_bottom`, `total_rows`, `hyperlink_uri`;
    - Kitty: `set_kitty_image_media`, `set_kitty_image_storage_limit`, `placement_render_info`;
    - `version`.
  - `RenderState`:
    - an update in one call or two phases, and dirty tracking;
    - rows and cells: text, width (`Narrow`/`Wide`/`SpacerHead`/`SpacerTail`), `Style` (`StyleColor`, `StyleFlags`,
      `Underline`);
    - cursor (`CursorStyle`) and colors.
  - Selection: `Point`/`PointTag`, `Selection`, `SelectionAdjust`, `SelectionGesture`.
  - Formatting: `Format`/`FormatOptions`/`Region` (plain, VT, HTML; viewport, screen, selection, range), with the
    extras the snapshot needs (cursor, modes, colors, unstyled scrollback).
  - The stream continuation: the state a replay must re-establish after the formatted content.
  - `Search`: matches over both screens, with ticks that do not lock the terminal.
  - Input: `KeyEncoder`, `MouseEncoder`, `encode_focus`, `paste_is_safe`.
  - Kitty graphics: images, placements, `KittyPlacementKey` with its id namespace, a relative placement's parent and
    H=/V= offsets, layers, render info.
  - The effects trait and its request and reply types.

  Names follow Ghostty and rustty. `agterm-term` adopts them in Task 17.
- **Drift to absorb.**
  - Task 1 diffs every Ghostty directory a ported module follows: `src/terminal`, `src/input`, `src/terminfo`,
    `src/unicode`, `src/datastruct`, `src/simd`, `src/os`, `src/lib`, `src/build/uucode_config.zig` and
    `build.zig.zon`'s uucode.
  - The range is from rustty's revision for that module (`246f702`; `12752b2ac` for the Unicode tables) to
    `LIBGHOSTTY_VT_REV`.
  - Each file is assigned to its task. C-API-only files count only for the behavior they expose.
  - Task 33 checks the coverage.
- **Unicode tables.** These are new work: a Rust port of uucode's UCD parsing and derivations, and of Ghostty's
  `uucode_config.zig`, at the uucode version and the UCD that the pin's `build.zig.zon` names.
  - `scripts/gen-vt-unicode.sh [--check]` runs it.
  - Two checks prove it: run at rustty's inputs, it reproduces rustty's tables byte for byte; run at the pin's inputs,
    it writes the tables agterm ships.
- **Kitty without wuffs.**
  - zlib payloads inflate through `miniz_oxide`.
  - PNG decodes through `png`.
  - The pixel swizzles (gray, gray+alpha and RGB to RGBA, RGBA source-over) are written out in Rust from wuffs'
    semantics.
- **Resources prefix.** Same layout, same name, without `lib/` and `include/`. Its key hashes the
  `LIBGHOSTTY_VT_*` pins, the checked-in terminfo source and any `build/patches/ghostty-vt/` left (none). The CI
  cache keys follow the same inputs.

#### What Task 1 settles (rustty `b57bcee7`, Ghostty pin `81598385`; 2026-10-18)

Sources read: rustty cloned at `.upstream/rustty` (`b57bcee7`) and Ghostty's history at `.upstream/ghostty`, both
git-ignored through `.git/info/exclude`. The calibration workspace and its scripts are in `.scratch/vt-calibrate/`
(git-ignored: `build.py` builds it, `run.sh` runs clippy sandboxed, `report.py` and `sizes.py` aggregate). Module paths
below are rustty-vt's (`crates/rustty-vt/src/`).

- **Verdicts.** The rule: in libghostty-vt at the pin and reachable from what agterm calls. `vt_write` reaches the
  whole stream handler, so what the handler does with a sequence is reachable even with no embedder callback.
  - Cut, with the branches that reached them:
    - `snapshot`: the codec. Its `io` writers (`Writer`, `FixedWriter`, `AllocatingWriter`, `StackFallbackWriter`,
      `WriteFailed`) are used by the kept `clipboard`, `paste`, `input/paste`, `kitty/clipboard_response` and
      `stream_terminal` (21 references); a small writer support module (or `std::io::Write`) replaces them in Task 11.
      `snapshot/continuation` goes; `stream_continuation` stays.
    - `tmux`, with `dcs`'s tmux branch.
    - `compress` and `pagelist/compress.rs`, and `mem` with them: page compression is `mem`'s only user, and the
      root's `compression_enabled` goes too.
    - `apc/glyph`. `apc` stays for Kitty graphics. The kept `osc/parsers/program_status.rs` takes its base64 decoder
      from `apc/glyph/request`; it moves to Task 5's base64.
    - `string_map`: Oniguruma-gated, off in libghostty-vt, and agterm matches links itself.
    - `xt_checksum`: DECRQCRA and XTCHECKSUM are answered only when `xt_checksum_report` is set, which is off by
      default and never set by agterm (`stream_terminal.zig` at the pin). The handler's two branches and `Terminal`'s
      checksum go, and those sequences fall through as unhandled, which agterm cannot tell apart.
    - `search/thread.rs`: the app's background search thread. libghostty-vt's search API (`rustty-vt-c/src/search.rs`)
      does not use it.
    - `fuzz_harness`, `pagelist/bench.rs`, `examples/`, `fuzz/`: rustty's harnesses. Task 16 takes the fuzz seeds as
      data.
    - `input/key.rs`'s `imgui` mapping (rustty's GUI glue).
  - Kept, of the modules Context names:
    - `kitty/dnd*`: OSC 72 is parsed and answered on the PTY inside `vt_write` with no embedder callback (`kittyDnd`
      in `stream_terminal.zig` at the pin). The drag-and-drop event callback has no C API, so it gets no effect.
    - `kitty/clipboard_grants`: OSC 5522 session password grants, part of the clipboard read and write path agterm
      installs.
    - `kitty/graphics_animation`: storage, image and exec use it, and animation commands run inside `vt_write`.
    - `fastprint` (style, formatter) and `fixed_writer` (dcs, stream_terminal).
    - `log_capture`, as a test-only helper reworked over `tracing`, because rustty's tests assert on log lines.
  - Adapted:
    - `build_options` and `options` keep only the constants the kept code branches on. There are no VT feature flags:
      every libghostty-vt feature agterm uses is on, and the cut ones are gone.
    - `sys` has no process-global hooks. PNG decodes through `png` (Task 10) and logs go to `tracing`. Its global raw
      allocator, which `alloc` uses, comes with Task 4.
  - Every other rustty-vt module is kept: `alloc`, `ansi`, `apc`, `bitmap_allocator`, `charsets`, `clipboard`,
    `color`, `csi`, `cursor`, `dcs`, `device_attributes`, `device_status`, `focus`, `formatter`, `fraction`,
    `hash_map`, `highlight`, `hyperlink`, `input/*`, `kitty/*` (the rest), `modes`, `mouse`, `osc` (every parser),
    `page`, `page_impl`, `pagelist`, `parse_table`, `parser`, `paste`, `point`, `protocol_int`, `ref_counted_set`,
    `render`, `screen`, `screen_set`, `search`, `selection`, `selection_codepoints`, `selection_gesture`, `sgr`,
    `size_report`, `stream`, `stream_continuation`, `stream_terminal`, `style`, `tabstops`, `terminal`,
    `utf8_decoder`, `x11_color`, `zig_list`, and the tests `page_tests` and `ref_geometry_golden_tests`.
  - Leaf crates, cut to the modules the kept code reaches:
    - util: `fastmem`, `quirks`, `tripwire`;
    - datastruct: `array_list`, `limited_allocator`, `intrusive_linked_list`, `untouched_pool`, `segmented_list`,
      `comptime_int_set`, `comparison`;
    - unicode: all of it;
    - simd: the scalar `base64`, `codepoint_width`, `index_of`, `vt`;
    - os: `page_allocator` (with `mach` on macOS and `windows`), `file`, `path`, `tempdir`, `string_encoding`;
    - lib: `allocator`, `packed`, `enum`, `parse_int`, `target`;
    - zigstd: `arena_allocator`, `memory_pool`, `array_hash_map`, `atomic`, `heap`, `wyhash`, `fmt`, plus
      `tracking_allocator` for tests;
    - render-types: `term_size` (sizes, `Offset`, `OffsetBuf`);
    - identity, with Ghostty's identity restored;
    - terminfo: `entry` and `source`.

    Not taken: rustty-font, -z2d, -global, -build-config, -xev, -wuffs, -args. rustty-synthetic's `rand` feeds only
    `hash_map`'s tests and is replaced there.
  - Size of the kept code: about 121k lines of code, 130k lines of tests, 3,388 `#[test]`s and 7,855 `unsafe`
    occurrences (of rustty-vt's 264k lines and 3,456 tests, plus the leaf modules).
- **Module graph** (`use crate::` edges of production code with comments stripped, the cut modules removed):
  - Acyclic leaves: `build_options` → `options` → `sys` → `alloc` ⇄ `zig_list`; `bitmap_allocator`, `hash_map`,
    `ref_counted_set`; `ansi`, `charsets`, `csi`, `cursor`, `device_attributes`, `device_status`, `size_report`,
    `focus`, `fixed_writer`, `utf8_decoder`, `point`, `protocol_int`, `mouse`, `tabstops`; `color` ⇄ `x11_color`
    (→ `fraction`); `modes`.
  - The parser group: `osc` ⇄ `kitty/color`, `parser` ⇄ `parse_table` (→ `osc`), `sgr` (→ `color`, `parser`),
    `kitty/graphics_command`, `apc`, `stream_continuation`, `stream` (→ `kitty/key`, `mouse`). Nothing here uses
    page memory.
  - The page group: `page` ⇄ `style` ⇄ `hyperlink` ⇄ `fastprint`, `page_impl`, `pagelist`.
    - `style` uses `sgr::Underline`, so the parser group comes first.
    - `pagelist` reaches `highlight` only through `highlight_semantic_content`, which `screen/select.rs` and
      `pagelist/tests/iter_clone.rs` call.
  - One large component, about 85k lines with tests: `screen`, `screen_set`, `selection`, `highlight`, `formatter`,
    Kitty `graphics_storage`/`_image`/`_pixel`, `terminal`, `stream_terminal`, `dcs`, `clipboard`, `paste`,
    `input/paste`, the Kitty clipboard modules. Hanging off it: `graphics_exec`, `graphics_unicode`,
    `graphics_render`, the Kitty DnD modules, `render`, `search`, `selection_gesture` and the key and mouse encoders.
  - The production edges from its screen-level part into its terminal-level part are exactly three:
    - `screen.rs:62` uses `terminal::PrintStringError`;
    - `formatter.rs:527` defines `TerminalFormatter` over `*const Terminal`. Screen uses only the page, page-list and
      screen formatters (`screen.rs:47-48`);
    - Kitty storage takes the `Terminal` in `scroll_margins_begin` (`graphics_storage.rs:700`), in the placement
      geometry (`cell_offset`, `pixel_size`, `grid_size`, `clip_top`, `rect` in `graphics_storage/placement.rs`) and
      in command deletion (`clear_screen`, `delete` and their helpers `delete_visible_placements`, `delete_intersecting`
      in `graphics_storage/delete.rs`). Eviction (`evict_image`, `evict_image_except`, `delete_if_unused`, same file)
      takes no `Terminal`, and storage needs it.
  - Tests that build a `Terminal`: formatter's `page_plain`, `terminal_screen`, `html_codepoint_pinmap` and
    `page_vt_pagelist`, Kitty storage's and `graphics_unicode`'s. Screen's tests and selection's do not.
- **Corrections to Tasks 4–15** (applied to the task sections below; codex: agreed, transcript
  `~/.local/state/plan-exec-revmux/zig-free-core/discussions/task-1-vt-task-boundaries/transcript.md`):
  1. Task 4 drops `mem`.
  2. Tasks 8 and 9 swap their content, numbers staying in file order.
     - Task 8 is now the parser and stream group, plus `color`, `x11_color`, `fraction`, `protocol_int`, `mouse`,
       `kitty/color` and `kitty/key`. It imports `kitty/graphics_command` directly, not through the `kitty/graphics`
       aggregate, which re-exports execution. `dcs` leaves it.
     - Task 9 is now the page group, without `highlight_semantic_content` and its tests.
  3. The large component stays two tasks, by item extraction. The retained module graph joins screen-level and
     terminal-level code in one component; Tasks 10 and 11 use these extractions and refactors to make smaller
     ones, and the task gates apply to the resulting graph:
     - `PrintStringError` is defined at screen level, and Terminal re-exports it;
     - `TerminalFormatter` comes with Task 11;
     - Kitty placement geometry takes the terminal's geometry as a value, and the scroll-margin hooks take the
       screen and its margins, not the `Terminal`. Safe Rust cannot hand storage the `*mut Terminal` that owns it
       anyway;
     - Kitty command deletion moves to Task 11 with `graphics_exec`, and eviction stays in Task 10;
     - `highlight` and `highlight_semantic_content`, with its tests, come with Task 10;
     - the tests that build a `Terminal` move to Task 11. Task 10 adapts representative cases of the refactored
       geometry, eviction and formatting to screen or geometry fixtures;
     - `dcs`, `clipboard`, `paste` and `input/paste` (from the parser task and Task 14), the Kitty clipboard and DnD modules and
       the `snapshot::io` replacement are Task 11's.
  4. Task 14 keeps the key and mouse encoders and focus. Paste moved to Task 11.
- **Owners.** Each kept module has one owner: the first task in file order whose code uses it. That task's Files
  list names it, and later tasks only use it. Beyond the modules the corrections above place:
  - Task 4: `build_options` and `options`, which `sys` reads, with only the constants the kept code branches on
    (Adapted, above); `sys` itself with its raw allocator. Leaf modules: util `quirks`; datastruct `array_list`,
    `limited_allocator`; os `page_allocator` (with `mach` and `windows`); lib `allocator`; zigstd `arena_allocator`,
    `array_hash_map`, `atomic`, `wyhash`; render-types `term_size`'s offsets (`Offset`, `OffsetSlice`, `OffsetBuf`,
    `IntFromBase`).
    Task 4 changed this (codex-agreed, see Task 4): `sys` is not created there, since its raw allocator only served
    the PNG decode seam, so `sys` is born in Task 10 with the PNG decode; `limited_allocator` and lib `allocator`
    (the C allocator ABI) are not ported (Task 10 caps the `png` decode with `png`'s own limits); zigstd
    `arena_allocator` moves to Task 9 with `memory_pool`, its only user; util `quirks` is `debug_assert!`, datastruct
    `array_list` and rustty-vt's `zig_list` are `std`'s `Vec` behind `mem::alloc`, and zigstd `atomic`'s cache line
    went with Zig's growth policy; `IntFromBase` is gone (the base is always the page's byte slice).
  - Task 5: rustty-vt's `utf8_decoder` and the root's `unicode` re-exports; leaf crates unicode and simd.
  - Task 7: identity, which the terminfo entry reads first, with Ghostty's identity restored; leaf crate terminfo.
  - Task 8: `protocol_int` (Kitty graphics command numbers), and `log_capture`, the test helper over `tracing` that
    Tasks 8 to 14 use. Leaf modules: datastruct `segmented_list`, `comptime_int_set`; os `string_encoding`; lib
    `enum`, `parse_int`, `target`; zigstd `fmt`.
  - Task 9: leaf modules util `fastmem`, `tripwire`; datastruct `intrusive_linked_list`, `untouched_pool`; lib
    `packed`; zigstd `memory_pool`, `heap`, `tracking_allocator` (tests); render-types `term_size`'s cell-count types
    (`size.rs`).
  - Task 10: `sys`'s PNG decode, as `png` in place of the `decode_png` hook. Leaf modules: datastruct `comparison`;
    os `file`, `path`, `tempdir`.
  - Task 11: `sys`'s secure-random fill (`random_secure_fill`, `io_native`, `io_failing`) without the global hook,
    over os `file`'s CSPRNG (`random_secure`, `random_secure_threaded` on Windows), used by the clipboard grants,
    paste and `Terminal`; the `kitty/graphics` aggregate, which re-exports execution. Task 10's Kitty modules import
    their siblings directly.
  - Task 15: `options`' version constants, for `version`.
- **Public functions of `agterm-ghostty-vt`.** There are 105 outside `sys`, plus `sys::install` and
  `sys::installed`. `agterm-term` calls 100 of the 105 and `sys::install`. It never calls `Key::index`,
  `paste_is_safe`, `Search::status`, `Terminal::title` or `Terminal::pwd`; Task 15 exports those only if the
  session server needs them. "vt-c" below is `crates/rustty-vt-c/src`, the C layer whose logic Task 15 ports.

  | `agterm-ghostty-vt` | C API | rustty item / vt-c logic | task |
  |---|---|---|---|
  | `Terminal::new` | `terminal_new` | `terminal::Terminal::init` + `stream_terminal::Stream` in vt-c `terminal.rs` (the wrapper) | 11, 15 |
  | `set_effects`, the `Effects` trait (15 callbacks), `HeldTerminal`, `ColorScheme::report` | `terminal_set` (`USERDATA`, `WRITE_PTY` … `RESET`) | `stream_terminal::Handler`'s effects, vt-c `terminal/effects.rs` | 11 (trait), 15 |
  | `vt_write` | `terminal_vt_write` | `stream_terminal::Stream::next_slice` | 11, 15 |
  | `vt_write_until_ground`, `at_ground` | `terminal_vt_write_until_ground`, `DATA_VT_GROUND` | vt-c `terminal/write.rs` over `stream::Stream` and `stream_continuation` | 15 |
  | `resize` | `terminal_resize` | `Terminal::resize` + cell pixel size (vt-c `terminal.rs`) | 11, 15 |
  | `reset` | `terminal_reset` | `Terminal::full_reset` + the handler's reset (vt-c `terminal.rs`) | 15 |
  | `scroll_viewport(Scroll)` | `terminal_scroll_viewport` (`TOP`, `BOTTOM`, `ROW`, `DELTA`, `DELTA_PROMPT`) | `Terminal::scroll_viewport` (`terminal::ScrollViewport`); prompts are drift `9fa2e3aaf` | 11, 15 |
  | `set_scrollback_limits`, `scrollback_limits` | `SCROLLBACK_MAX_BYTES`/`_LINES` | `PageList`'s size limits, vt-c `terminal/options.rs`, `terminal/data.rs` | 9, 15 |
  | `set_kitty_image_storage_limit`, `set_kitty_image_media` | `KITTY_IMAGE_STORAGE_LIMIT`, `KITTY_IMAGE_MEDIUM_*` | `ImageStorage::set_limit`, `LoadingImage` media limits, vt-c `terminal/options.rs` | 10, 15 |
  | `set_mode`, `set_mode_default`, `mode`, `Mode::{dec, ansi}` | `MODE`, `MODE_DEFAULT`, `DATA_MODE` | `modes::{Mode, ModeState}` on `Terminal.modes` | 8, 15 |
  | `colors`, `set_default_colors`, `set_default_palette`, `set_default_cursor`, `default_palette` | `COLOR_*`, `DEFAULT_CURSOR_*`, `DATA_COLOR_*` | `Terminal.colors` (`color::DynamicRGB`, palette), the screen's default cursor style | 8, 10, 15 |
  | `cols`, `rows`, `cursor`, `total_rows`, `viewport_at_bottom`, `alternate_screen`, `mouse_tracking`, `mouse_shape`, `cursor_at_prompt`, `title`, `pwd` | `terminal_get` (`DATA_*`) | vt-c `terminal/data.rs` over `Terminal`, `ScreenSet`, `PageList`, `modes`, `mouse::Shape` and the semantic prompt state | 15 |
  | `paste` | `terminal_paste` | vt-c `paste.rs` over `paste::Request` and `input::paste` | 11, 15 |
  | `paste_is_safe` | `paste_is_safe` | `input::paste::is_safe` | 11 |
  | `format` (`Format`, `FormatOptions`, `Region`) | `formatter_terminal_new`, `formatter_format_alloc` | `formatter::TerminalFormatter` over the screen formatters, vt-c `formatter.rs` | 10, 11, 15 |
  | `selection`, `set_selection`, `clear_selection`, `viewport_top` (`Point`, `PointTag`, `Selection`) | `terminal_get`/`set` `SELECTION`, `terminal_grid_ref`, `terminal_point_from_grid_ref` | the screen's `selection::Selection`, `point::Point` ⇄ `pagelist::Pin`, vt-c `selection.rs`, `grid_ref.rs` | 10, 15 |
  | `select_word`, `select_line`, `select_all`, `adjust_selection` | `terminal_select_*`, `terminal_selection_adjust` | `screen/select.rs`, `selection_codepoints`, `Selection::adjust` | 10, 15 |
  | `hyperlink_uri` | `grid_ref_hyperlink_uri` | vt-c `grid_ref.rs` over the page's hyperlink set | 9, 15 |
  | `RenderState::{new, update, begin_update, update_held, dirty, clean, size, colors, cursor, rows}`, `PendingUpdate::end`, `Rows::next_row`, `RowRef::{is_dirty, id, wrapped, selection, cells, text}`, `Cells::next_cell`, `CellRef::{width, has_hyperlink, has_styling, style, foreground, background, grapheme}` | `render_state_*`, `row_get`, `cell_get` | `render::RenderState` and vt-c `render.rs` (row iterator, cells, two-phase update); cells over `page::Cell` | 12, 15 |
  | `kitty_graphics()` (on `Terminal` and `HeldTerminal`), `KittyGraphics::{generation, placements, image}`, `KittyPlacement::key`, `KittyLayer::of`, `KittyRenderInfo` | `kitty_graphics_*`, `kitty_graphics_placement_render_info` | `kitty::graphics_storage::ImageStorage`, `graphics_render`, vt-c `kitty_graphics.rs` (`placement_render_info`) | 10, 15 |
  | `KittyIdNamespace`, `KittyParent` (patch 0001) | the patched C API | `graphics_storage`'s `PlacementKey` and `PlacementIdTag`, the relative placement's parent and offsets | 10 |
  | `KeyEncoder::{new, encode}`, `Key::{from_index, index}`, `KeyEvent`, `KeyMods`, `KeyAction` | `key_encoder_*`, `key_event_*` | `input::key_encode` with its options from the terminal, `input::key`, `input::key_mods` | 14 |
  | `encode_focus` | `focus_encode` | `focus` | 14 |
  | `MouseEncoder::{new, encode, reset}`, `MouseEvent`, `MouseGeometry`, `MouseButton`, `MouseAction` | `mouse_encoder_*`, `mouse_event_*` | `input::mouse_encode` with its options from the terminal and last-cell tracking, `input::mouse` | 14 |
  | `Search::{new, close, set_needle, feed, run, tick, select, status, total, selected_index}` | `search_*` | `search::{screen, active, viewport, pagelist, sliding_window, terminal}`, vt-c `search.rs` (the split between feed and tick) | 13 |
  | `SelectionGesture::{new, close, reset, apply, state}` and the `Gesture*` types | `selection_gesture_*` | `selection_gesture`, vt-c `selection_gesture.rs` | 13 |
  | `version` | `build_info` (`VERSION_STRING`) | `options`' library version from rustty-identity, with Ghostty's identity restored | 7, 15 |
  | `sys::install` (PNG decoder, log sink) | `sys_set` | gone: PNG decodes through `png` inside the crate, and logs go to `tracing` | 10 |
  | `Rgb`, `MouseShape`, `ScrollbackLimits`, `TerminalColors`, `CursorStyle`, `Error` | types | `color::RGB`, `mouse::Shape`, `screen::CursorStyle`, the crate's errors | 8–10, 15 |
- **Drift** (Ghostty `246f70287..81598385` over every followed directory; 29 commits). There is none in `src/unicode`,
  `src/datastruct`, `src/simd`, `src/terminfo` or `src/build/uucode_config.zig`. `build.zig.zon` changed only the
  theme package: uucode stays 0.2.0 @ `9d555245` at the pin, and nothing Unicode moved since `12752b2ac` either.
  Each task first checks whether rustty already carries a change; a change it carries counts as absorbed.

  | Ghostty file | commits | task |
  |---|---|---|
  | `terminal/csi.zig`, `terminal/size_report.zig`, `terminal/stream.zig` | `6104c78f1` CSI 19 t; `d0c0d4008` TBC without a parameter (stream) | 8 |
  | `terminal/device_status.zig` | `fbda61b87` DECXCPR (`CSI ? 6 n`) | 8 |
  | `terminal/modes.zig` | `d432b01a3` DECRQM reports unsupported ANSI modes as permanently reset; `b21f82c0e` their assertion | 8 |
  | `terminal/osc/parsers/context_signal.zig` | `46d6c3b50` unescape metadata fields | 8 |
  | `terminal/osc/parsers/program_status.zig` | `b7207b2ef` program status protocol revision 0.4 | 8 |
  | `terminal/PageList.zig` | `4144f6dea` capacity log demoted to debug | 9 |
  | `terminal/Screen.zig` | `65ff54c67` use-after-free when scrollback prunes the cursor page; `6dae79db9` wrapped grapheme data re-attached in one allocation | 10 |
  | `terminal/formatter.zig` | `bae1c01fc` pending wrap restored over a blank cell; `8d8b1ce00` wide char wrapped onto the next page when copying | 10 |
  | `terminal/kitty/graphics_image.zig` | `cd432ba5c` `/private/tmp` accepted for temp files on macOS (`1de05f1a7` is a Zig std rename) | 10 |
  | `terminal/Terminal.zig` | `65ff54c67`, `6dae79db9`; `ff779564c` ED2 at a prompt skips blank rows; `42a65647b` pending wrap cleared when printing with DECAWM off; `0df8c4868` DECSTR turns off ISO protection; `9fa2e3aaf` prompt-based viewport scrolling; `b10ba7ea2` grapheme data kept when widening wraps | 11 |
  | `terminal/stream_terminal.zig` | `5c9a3d8a9`, `d432b01a3`, `fbda61b87`, `6104c78f1`; `3175f6733` CSI 8 t resize requests; `b7207b2ef` | 11 |
  | `terminal/kitty/clipboard_write.zig` | `44c4c8f55` memory retained by repeated walias updates bounded | 11 |
  | `input/paste.zig`, `terminal/paste.zig` | `5c9a3d8a9` paste protection treats CR like a newline | 11 |
  | `terminal/render.zig` | `4af7e19c7` wide char selection rendering matches copying | 12 |
  | `terminal/c/terminal.zig`, `terminal/c/types.zig`, `terminal/c/paste.zig` | `3175f6733` the resize-window callback (an effect with a no-op default); `b7207b2ef`, `9fa2e3aaf`, `5c9a3d8a9` as exposed through the C API | 15 |
  | `terminal/snapshot/page.zig`, `terminal/tmux/viewer.zig` | `6f3a4fade`; `1a93d5395`, `8a7a91fbd`, `968b2bf19` | cut |
  | `os/open.zig` | `f9e8ef02d` macOS untrusted links (the app's URL opener, not ported) | none |
  | `os/{TempDir,cgroup,homedir,i18n,path,resourcesdir,systemd}.zig`, `lib/tinyio/*` | `1de05f1a7` `std.Io.Dir.max_path_bytes` rename, no behavior | none |
- **Calibration.** The kept files and the leaf crates they need, copied as Rust sources only, were checked by
  clippy 1.98.1 under `[workspace.lints]` and `clippy.toml`:
  - the setup:
    - no network, a scratch HOME and CARGO_HOME with vendored crates.io sources;
    - no rustty build script or `-sys` crate: empty stubs stand in for them, and a signatures-only stub for
      rustty-wuffs;
    - every lint at warn, so each crate builds and each hit is reported;
    - rustty's 341 `#[allow]`s in kept code stripped first (579 in the copies);
    - edition 2021 as rustty has it, with `rust_2024_compatibility` added.
  - Coverage: Linux only, so `cfg(target_os = "macos"/"windows")` code is not counted. Counts are kept code only,
    with test code (`#[cfg(test)]` modules, test files) apart.
  - Production code: 7,454 hits of 121 lints, among them:
    - unsafe: `undocumented_unsafe_blocks` 2288, `multiple_unsafe_ops_per_block` 545, `not_unsafe_ptr_arg_deref`
      94, `missing_safety_doc` 13. rustty writes `// SAFETY(tag):`, which clippy does not accept, and the unsafe
      core shrinks anyway;
    - docs: `doc_markdown` 699, `missing_errors_doc` 276, `missing_panics_doc` 175, `empty_line_after_doc_comments`
      77;
    - `must_use_candidate` 628;
    - robustness: `unwrap_used` 299, `panic` 54, `expect_used` 29;
    - casts: `cast_lossless` 267, `cast_possible_truncation` 200, `cast_sign_loss` 37, `cast_possible_wrap` 25;
    - complexity: `excessive_nesting` 186, `too_many_lines` 86, `cognitive_complexity` 61, `struct_excessive_bools`
      15, `too_many_arguments` 11;
    - Zig naming: `non_upper_case_globals` 60, `non_snake_case` 52, `non_camel_case_types` 34;
    - shape: `manual_let_else` 171, `single_match_else` 91, `semicolon_if_nothing_returned` 75, `manual_assert_eq`
      72;
    - edition 2024: `edition_2024_expr_fragment_specifier` 14, `keyword_idents_2024` 4,
      `missing_unsafe_on_extern` 4, `tail_expr_drop_order` 1.
  - Test code: 8,323 hits of 79 lints. Most are unsafe: `undocumented_unsafe_blocks` 4923,
    `multiple_unsafe_ops_per_block` 906. Then `manual_assert_eq` 365, `cast_possible_truncation` 337,
    `non_snake_case` 230, `cast_lossless` 186, `unreadable_literal` 162, `borrow_as_ptr` 135,
    `cognitive_complexity` 120, `manual_let_else` 115, `too_many_lines` 82.
  - Per corrected task (the module lists of the sections below). Columns: production hits, of them unsafe (the two
    unsafe-block lints) and complexity (the seven complexity lints), then test-code hits, stripped `#[allow]`s and
    the top production lints. Size columns: production and test lines, `#[test]`s, `unsafe` occurrences.

    | task | prod | unsafe | complexity | test | allows | top production lints | lines prod/test | tests | `unsafe` |
    |---|---|---|---|---|---|---|---|---|---|
    | 4 memory core | 888 | 324 | 3 | 594 | 54 | undocumented_unsafe_blocks 243, doc_markdown 112, must_use_candidate 99 | 16156/4046 | 193 | 823 |
    | 5 Unicode, UTF-8, SIMD | 156 | 13 | 8 | 60 | 21 | must_use_candidate 25, doc_markdown 23, cast_lossless 12 | 6427/2278 | 69 | 16 |
    | 7 terminfo, identity | 31 | 0 | 2 | 8 | 0 | unwrap_used 9, doc_markdown 5 | 1187/315 | 10 | 0 |
    | 8 parser and stream | 1014 | 41 | 69 | 601 | 61 | must_use_candidate 160, doc_markdown 137, manual_let_else 56 | 26772/20772 | 798 | 85 |
    | 9 page and page list | 2064 | 1180 | 118 | 3255 | 65 | undocumented_unsafe_blocks 952, multiple_unsafe_ops_per_block 228, doc_markdown 153 | 16064/29263 | 494 | 3242 |
    | 10 screen, formatter, selection, Kitty storage | 1424 | 527 | 75 | 1405 | 64 | undocumented_unsafe_blocks 442, unwrap_used 101, doc_markdown 98 | 19626/26387 | 594 | 1350 |
    | 11 terminal, handler, effects, Kitty exec, paste | 1137 | 574 | 45 | 1774 | 31 | undocumented_unsafe_blocks 477, multiple_unsafe_ops_per_block 97, doc_markdown 75 | 19121/31399 | 875 | 1801 |
    | 12 render state | 178 | 72 | 15 | 45 | 5 | undocumented_unsafe_blocks 59, cast_possible_truncation 22 | 2473/2487 | 42 | 69 |
    | 13 search, selection gesture | 354 | 101 | 11 | 481 | 24 | undocumented_unsafe_blocks 89, doc_markdown 42, unwrap_used 32 | 7910/5935 | 122 | 431 |
    | 14 input encoders | 208 | 1 | 15 | 6 | 14 | doc_markdown 46, must_use_candidate 44, cast_lossless 35 | 5020/4359 | 145 | 1 |
    | rustty-vt's integration tests | 0 | 0 | 0 | 94 | 2 | | 0/2683 | 46 | 37 |

    The modules with the most production hits: `pagelist` 1272 (test code 2724), `terminal` 763 (1395), `screen`
    669 (753), `page_impl` 523, `osc` 334, `search` 287, `kitty/graphics_storage` 223, `ref_counted_set` 182,
    `hash_map` 179, `render` 178, `stream` 173, `formatter` 143 (382), `page` 132.

### `agterm-session`

- **Source and license.** herdr is pinned in `build/pins.env` (`HERDR_REPO`, `HERDR_REV`) for provenance and future
  syncs.
  - herdr's files carry no license header, and it ships no NOTICE. So each derived file gets a header that names its
    herdr source file, says it is licensed under Apache-2.0, and says it was modified (Apache-2.0 §4).
  - herdr's `LICENSE` goes into the root `NOTICE`, and `agterm-session` declares `MIT AND Apache-2.0`.
  - herdr's own libghostty-vt binding is not ported.
- **What Task 2 settles from herdr's code** (written below, "What Task 2 settles"):
  - the module map with keep, cut or adapt for each module. Cut: TUI rendering, surface deltas, ratatui, plugins,
    agent detection, workspace and layout UI, persist, remote. Adapt: the server, the pane runtime, the client
    side, headless;
  - the wire: framing, encoding, version negotiation, the message families that the attach stream and the api need;
  - the async runtime: herdr's `tokio`, or threads as `agterm-term` uses; kept as herdr has it unless it conflicts
    with the GPUI main loop or the per-pane IO thread;
  - the Windows handoff: whether ConPTY sessions can pass to a new server (duplicated pipe handles, the pseudoconsole
    and its `conhost`, the job object), proven in the `Win11-admin` VM. If they cannot, sessions on Windows end on
    update. That is recorded as a ⚠️ limitation, and the update path offers the reset;
  - any gap that agterm's upstream restore, replay or remote attach leaves, which herdr's persist or remote would
    fill. Each one becomes a ➕ task with its reason, or is recorded as none.
- **Session creation.** A session is created with the pane's argv (the create-only payload), environment
  (`TERM=xterm-ghostty`, `TERMINFO`, the integration variables, the tracked env) and cwd. The child environment keeps
  today's rules where `agterm-core` and the suite rely on them: the session name variable, `TERM` fixed when unset or
  `dumb`, the managed-claim variables removed.
- **Snapshot on attach.** Built with `agterm-vt`'s formatter and stream continuation, matching what the suite pins
  today (Context), never with a hand-rolled VT encoder. herdr's history and alt-screen reads are the code source.
- **API.** Exactly what `SessionHost` and `clipboard set` need, since the `live.*` group's commands run on
  `SessionHost`:
  - list, with every `LiveSessionRecord` field (`clients`, `leader_pid`, `created_at`, …);
  - kill (one, batch, `force`) with every `KillOutcome`. `StaleSocket` has no meaning with one server: it maps to the
    session-gone outcome the core treats the same way, and the mapping is tested;
  - screen (`active`/`all`) with `LiveScreen`'s `revision`, cursor and `alternate`, which the suite waits on;
  - type: CR as Return, encoded for the keyboard mode, queued without taking the lead;
  - foreground job;
  - print, for `agtermctl clipboard set`: an OSC 52 write into the session's output.
- **Attach contract** (kept; `agterm-session attach` honors it, and Task 26 tests it):
  - `attach <name> [creation argv…]`: create-only, with the gone guard `agterm-core::remote::ssh` appends;
  - the variables `AGTERM_LIVE_DIR`, `AGTERM_LIVE_SESSION`, `AGTERM_LIVE_SESSION_PREFIX`,
    `AGTERM_LIVE_NO_DETACH_KEY`, `AGTERM_LIVE_MANAGED`, `AGTERM_LIVE_MANAGED_CLAIM`, read as today's are (empty
    means unset). The `AGTERM_LIVE_` prefix stays clear of `AGTERM_SESSION_ID`, the sidebar session's id;
  - the lead role as the reserved-prefix title `OSC 2;agterm-live-role;<token>:<role>:<gen>` (today's `lead.rs`
    format under the new prefix);
  - `AGTERM_LIVE_SESSION` and `AGTERM_LIVE_DIR` set in each session's children, which `agtermctl clipboard set`
    reads.
- **Leadership.** Size ownership with several clients follows today's explicit-leadership semantics,
  because `agterm-core`'s lead policy and the suite pin them. herdr's resize ownership is adapted to it.
- **Foreground job** (`SessionHost::foreground_job`, `job_running`, `end_job`):
  - Linux: the existing `/proc` process-group logic, moved behind a per-OS module.
  - macOS: `sysctl` (`KERN_PROC_PGRP`, `KERN_PROCARGS2`), as upstream's `ForegroundProcess.swift` does.
  - Windows: herdr's process snapshot. The job is the shell's live descendants, and `end_job` terminates them.
- **`ProcessTable` for macOS and Windows**, beside the Linux one, for the quit-time command capture on every OS.
  Windows argv comes from `NtQueryInformationProcess(ProcessCommandLineInformation)`, split with
  `CommandLineToArgvW`, as herdr does.
- **PowerShell**: `ShellKind::Pwsh` in `shell_integration.rs`, with:
  - an integration script: OSC 7 cwd, OSC 133 prompt and command marks, title;
  - launch arguments;
  - a pwsh rendering of the replay script (`replay.rs`);
  - an eligibility probe on Windows: `pwsh.exe` on `PATH`, else `powershell.exe`, else the existing "unsupported
    shell" rejection.

#### What Task 2 settles (herdr `143bf5f`; 2026-10-18)

Sources read: herdr cloned at `.upstream/herdr` (`143bf5fd`, master of 2026-10-16, v0.9.5 + 24), git-ignored like
rustty. Probes, logs and the design discussion: `docs/spikes/20261018-session-server.md` (the spike note, with the
probes' sources in `docs/spikes/20261018-session-server-probes.md`). Paths below are herdr's (`src/`).

- **A correction to the Part B decisions.** herdr's `AttachTerminal` is not a raw stream. The server renders the
  pane into a virtual buffer and sends ANSI diffs (`server/headless/render.rs` `render_terminal_virtual` →
  `FrameData` → `ServerMessage::Terminal(TerminalFrame)`), on a render lane of capacity one that drops frames
  (`server/client_transport.rs` `ClientWriterQueue`), with one attached owner per terminal (`--takeover`). The user
  decision stands (snapshot, then the raw output, into the pane's own `agterm-vt`); the raw attach mode is agterm's,
  built on herdr's connection framework (handshake, client registry and modes, input/resize path, writer threads).
  herdr's direct terminal attach is also Unix-only (`platform/mod.rs` `capabilities`), and so is its handoff.
- **Module map.** Keep = ported close to herdr's code; adapt = ported and reworked; cut = not ported.

  | herdr module | verdict | what agterm-session takes, or why not |
  |---|---|---|
  | `protocol/wire.rs` | adapt | framing (u32 LE length, max size checked before allocating, partial reads reassembled, trailing bytes refused), `FramingError`, version check; the messages are agterm's (below); bincode → postcard |
  | `protocol/endpoint.rs`, `render_ansi.rs`, `surface_*` | cut | the client-shell endpoint generation and server-side rendering for TUI clients (the future TUI plan) |
  | `ipc.rs` | keep | `interprocess` local sockets, the named-pipe listener with its SDDL, stale-socket detection, socket file identity, peer-closed probe |
  | `server/socket_paths.rs` | adapt | `prepare_socket_path`, owner-only permissions; paths from agterm's namespace |
  | `server/autodetect.rs` | adapt | find a server or spawn one detached and wait for its endpoint; the second of two racing servers loses the bind and exits |
  | `server/client_accept.rs`, `client_transport.rs`, `clients.rs` | adapt | accept loop, handshake with timeout, a reader and a writer thread per connection, connection modes (pending, attached), attach owners; the writer queue becomes byte-bounded and lossless for the raw lane |
  | `server/headless.rs`, `headless/bootstrap.rs`, `headless/lifecycle.rs` | adapt | the coordinator's event handling for clients, attach, resize locks and handoff, the server and import-server entry points, `perform_live_handoff`; everything that renders, notifies or drives the TUI goes |
  | `server/handoff.rs`, `handoff_runtime.rs` | keep (Unix) | token-authenticated private socket, JSON manifest, `SCM_RIGHTS` batches of 64, validated/restored/ready/committed/owned steps and their timeouts; the per-pane state carries agterm's snapshot |
  | `server/shutdown.rs` | keep | `ServerStop` and the logged shutdown reason |
  | `server/alt_screen_read.rs`, `terminal/history_read.rs` | cut | herdr harvests alt-screen history by scrolling the app; agterm reads the server VT |
  | `server/headless/{render,retained_surface,client_views,native_graphics,notifications,surface_interest,endpoint_requests}.rs`, `client_shell*`, `client_commands.rs`, `render_stream.rs`, `pane_input.rs`, `keybindings.rs`, `notifications.rs`, `clipboard_image.rs`, `terminal_attach.rs` | cut | TUI client shell, rendering, notifications, image paste |
  | `pane.rs` | adapt | pane spawn (argv, env, cwd), child wait, `shutdown_pane_processes` (HUP, TERM, KILL over the session's processes), `from_handoff_fd`; agent detection, compression tasks and render signalling go |
  | `pane/terminal.rs` | adapt (reference) | the libghostty-vt wrapper is replaced by `agterm-vt`; its history seeding on import is the model for the snapshot replay |
  | `pane/{osc,xtgettcap,kitty_keyboard,cursor,input,agent_detection,background_agent}.rs` | cut | `agterm-vt`'s stream handler answers queries and tracks keyboard modes; agent detection is herdr's product |
  | `terminal/runtime.rs`, `runtime_registry.rs` | adapt | the per-pane runtime facade (write, resize, snapshot, handoff export) and the registry by session name |
  | `terminal/{state,metadata,title}.rs` | cut | agent state and metadata |
  | `pty/actor.rs`, `pty/actor/unix.rs`, `pty/backend*`, `pty/fd.rs` | keep | the PTY actor thread (poll + wake pipe on Unix, reader/writer threads on Windows), quiesce/release for handoff, CLOEXEC master duplication; `tokio::sync::mpsc` → std channels, herdr's Windows async input forwarder → a writer-thread queue (agterm has no delayed-Enter submission) |
  | `client/handshake.rs`, `client/mod.rs`, `client/terminal_setup.rs`, `client/input/windows_vti.rs` | adapt | the client library's connect + hello; the `agterm-session attach` terminal client (raw mode, resize watcher, Windows console VT input) |
  | `client/attach.rs`, `client/shell/**` | cut | herdr's Ctrl+B prefix keys and TUI; the detach key is the attach contract's |
  | `session.rs` | cut | herdr's named sessions are agterm's namespace; only its bounded stop-and-wait pattern is reused |
  | `platform/windows.rs` | adapt | `launch_server_daemon_command` with the WMI path, `current_job_kills_processes_on_close`, `current_process_is_detached_server_daemon`, the pipe security descriptor (owner DACL + integrity label), timed pipe connect, process snapshot, `session_processes`, foreground job, `command_line_to_argv`; notifications, clipboard, power, IME, config backup go |
  | `platform/linux.rs`, `platform/macos.rs`, `platform/unix_common.rs`, `platform/mod.rs` | adapt | `session_processes`, foreground process group (`/proc`; `sysctl` on macOS), `signal_processes`, `setsid` detach, SIGHUP ignore, nofile limit; logind inhibitor, power and shutdown hooks go |
  | `platform/macos/launchd_spawn.rs` | not ported, recorded fallback | a one-shot launchd job against herdr#4920; the probe did not reproduce that kill for a setsid + disclaim server (see Platforms) |
  | `platform/macos/bootstrap.rs` | cut | herdr moves the server into the user's Mach bootstrap context; agterm uses upstream's responsibility disclaim |
  | `api/server.rs` | adapt | the accept-loop and connection-thread pattern, the live-handoff request; the JSON-lines API itself is replaced by the binary API |
  | `api/{schema,event_hub,subscriptions,wait,status,client}.rs`, `cli/**` | cut | herdr's public API and CLI; agterm-session's API and CLI are the plan's |
  | `persist/**`, `remote/**` | cut | see Gaps |
  | `vendor/portable-pty` | adapt | patches 0003 (malformed Windows environments) and 0004 (directories on `PATH`); 0001 reduced to "never load a bare `conpty.dll` from the search path, use kernel32's ConPTY" (agterm ships no ConPTY bundle); 0002 (raw `cmd.exe` tails) not needed, agterm launches argv; plus agterm's own patch exporting and importing a ConPTY master's handles for the handoff |
  | `crates/ghostty-vt` | cut | herdr's libghostty-vt binding; `agterm-vt` replaces it |

- **Attach** (Task 23, agreed with Codex, transcript in the spike note):
  - one serialization point per pane orders parsing, output offsets, snapshot capture, subscriber registration,
    resize and role changes;
  - wire: `SnapshotBegin{epoch, offset, size}`, bounded chunks, `SnapshotEnd`, then contiguous offset-tagged
    `Output`;
  - the snapshot is the formatter's restoration plus `agterm-vt`'s validated, replay-safe stream continuation (VT
    parser and UTF-8 decoder state). Continuation capture has a byte cap and an unavailable outcome: it is never
    truncated or replaced by empty state. When unavailable, the snapshot waits, within a deadline, while the PTY
    keeps draining; with no valid cut by then, attach or resync fails without harm, and a handoff aborts before its
    commit;
  - per-client raw queues are byte-bounded and lossless, and never block the PTY reader. On overflow the server
    resyncs in band: it finishes or abandons the frame being written, cuts a new snapshot and replaces only unsent
    output (control messages keep their order), and replies tagged with a superseded epoch are dropped. A recovery
    budget (snapshot bytes, suffix bytes, resyncs per interval) or a passed write deadline ends in disconnect. A
    disconnect would cost the leader its lead, which is why resync comes first;
  - terminal replies: the server VT answers state and capability queries (DA1/2/3, DSR/CPR, DECRQM, DECRQSS,
    XTVERSION, XTGETTCAP) always, so the session has one identity and no stalled client blocks a reply.
    Frontend-dependent replies (OSC 4/10/11/12 colours, XTWINOPS pixel sizes, Kitty graphics query replies) come
    from the server too, out of metadata the reply-owner client supplies on attach and on change (theme colours,
    cell size, graphics support), with stable defaults when there is no reply owner. Only what the whole
    server/client path implements is advertised. OSC 52 reads and anything that needs a user's decision go as a
    single-recipient request to the reply owner (the leader, else the first managed client), denied when none;
  - attached VTs never write replies: `agterm-term` gets an attach effects policy for live panes, set in the
    launch configuration before the first byte arrives (Task 28 runs the attach CLI in the pane's PTY; Task 30
    attaches in process). `agterm-session attach` in an arbitrary terminal filters the server-owned query sequences
    out of its output, fragment-safe, so the outer terminal does not answer them (Task 26);
  - on Windows, conhost answers some queries itself; Task 23 records, per supported Windows version, which queries
    reach the output pipe.
- **Wire.** postcard (serde) in place of herdr's bincode: bincode 3.0.0 on crates.io is a `compile_error!`
  tombstone and its README says development ceased. herdr's framing guards are kept. A stable envelope (magic and
  protocol version, decodable across versions) precedes any postcard, so a client can talk to a server of another
  build and ask it to hand off; protocol compatibility is separate from build identity, and capabilities are
  advertised. One listener per namespace takes a separate connection per role (api request, attach), so bulk
  output never delays a control call. The handoff keeps herdr's private socket, token and JSON-lines manifest (a
  named pipe on Windows).
  - message families: hello/welcome (envelope, build id, capabilities, error); api request/response by id; attach
    (attach, snapshot begin/chunk/end, output, input, reply metadata and delegated requests, resize, role, detach,
    exit); shutdown; handoff control.
- **Runtime: threads, no tokio.** herdr's async surface is one `tokio::select!` loop (`server/headless.rs` `run`)
  over channels, a deadline and a render notifier; its transport, API connections, PTY actors and handoff are
  already blocking threads. The reduced server is one coordinator on `recv_timeout(next deadline)`, blocking
  connection workers and a PTY reader and writer per pane. The client library stays synchronous inside
  `agterm-term`'s IO thread.
- **Windows handoff: conditional GO.** Proven in the `Win11-admin` VM (10.0.26100.9711) and on `windows-latest`
  (10.0.26100) and `windows-2022` (10.0.20348): a detached server holding a ConPTY shell in a kill-on-close job
  passes it to a second server, which receives by `DuplicateHandle` the input write end, the output read end, the pseudoconsole's three
  internal handles (kernelbase's `{hSignal, hPtyReference, hConPtyProcess}`, undocumented), the job and the shell
  process. After the old server exits, the shell and conhost live, the shell keeps its state, a resize through an
  HPCON rebuilt from the three handles reaches the child, closing the job ends the shell and closing the console
  handles ends conhost. Guards Task 25 must build: a layout gate checked before any dereference (support matrix by
  ConPTY implementation, architecture and Windows version; unknown → no handoff); custom ownership of the rebuilt
  struct (never `ClosePseudoConsole` on it); handoff bound to the replacement's process handle; versioned manifest;
  prepare/ready/commit with a rollback that never touches the shared job or console; quiesced reader, input,
  resize and replies; already-read bytes and the VT continuation transferred. Gate failure keeps the old server
  serving and offers the reset. If gating proves unreliable, the escape hatch is a stable per-namespace ConPTY
  owner process.
- **Windows spawn.** herdr spawns with `DETACHED_PROCESS` and, inside a kill-on-close job, goes through WMI
  `Win32_Process.Create` (`platform/windows.rs` `launch_server_daemon_command`). agterm: no kill-on-close job →
  plain detached spawn (`DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`); in one → add `CREATE_BREAKAWAY_FROM_JOB`;
  refused → WMI; WMI fails → error. Never a silent retry inside the job: in the probe a job without
  `JOB_OBJECT_LIMIT_BREAKAWAY_OK` refused breakaway with `ERROR_ACCESS_DENIED`, and the server started without it
  died when the job closed. Readiness, then lifetime, is validated.
- **Windows namespace** (decided): the directory `%LOCALAPPDATA%\agterm\live\<FNV-1a 64 of the resolved state dir>`
  (`%USERPROFILE%\AppData\Local` when `LOCALAPPDATA` is unset), the same hash as Unix; the endpoint
  `\\.\pipe\agterm-live-<FNV-1a 64 of the case-folded namespace directory path>`, derivable by a client that holds
  only `AGTERM_LIVE_DIR`. The listener uses herdr's owner-only DACL (`D:P(A;;GA;;;SY)(A;;GA;;;OW)`) with the
  integrity label of `local_server_security_descriptor` and `FILE_FLAG_FIRST_PIPE_INSTANCE`; a client checks the
  server's user SID (`GetNamedPipeServerProcessId`). A private state dir gives a private directory and pipe, which
  is the Windows form of the isolation rule's private pipe prefix.
- **Platforms.**
  - macOS: `posix_spawn` with `POSIX_SPAWN_SETSID` (0x0400; the libc crate exports it for Linux only) and
    `responsibility_spawnattrs_setdisclaim` proven in the `macOS 26` VM (26.7) and on `macos-15` and `macos-26`: the
    server outlives its launcher, leads its session, is responsible for itself, and an `SCM_RIGHTS` handoff keeps
    the shell with its state, resize and hangup. herdr#4920 (a terminal app's quit kills a setsid server through its
    launchd job) did not reproduce with Terminal.app: after a normal quit (Apple Events `quit`, as Cmd-Q sends) a
    setsid child and a setsid + disclaim child both lived on; after a SIGTERM to Terminal.app the setsid + disclaim
    child lived and a plain child died by the tab's hangup; children of a LaunchServices app outlived its exit. The
    launchd one-shot job stays the recorded fallback, and Post-Completion's macOS check covers agterm's own quit.
  - Every PTY master is close-on-exec, and the macOS server spawn adds `POSIX_SPAWN_CLOEXEC_DEFAULT`. In the probe a
    stray inherited master that nobody read held the exiting shell in the tty drain (`ps` state `E`), so it never
    ended.
  - Unix handoff: the new server is not the shell's parent, so a session's exit status after a handoff is unknown
    (herdr reports `ChildExitReason::Handoff`), and launchd or a subreaper reaps the shell. On Windows the
    duplicated process handle keeps the status.
  - Windows kill: herdr kills a pane's processes from a snapshot. agterm's per-pane `KILL_ON_JOB_CLOSE` job (Solution
    Overview) is its own addition, and the job handle passes in the handoff, or the old server's exit would kill
    every shell.
- **Server lifetime.** herdr's server runs until stopped. agterm's exits when it holds no session and no client
  (Task 22), as planned.
- **Gaps** in upstream's restore, replay and remote attach that herdr's `persist` or `remote` would fill: none.
  - `persist` saves layouts and, optionally, screen history to disk, and restores them into fresh shells after a
    server or machine restart. Upstream's restore and replay cover the restart case, and it keeps no screen history
    across a reboot, so this would go beyond parity.
  - `remote` runs herdr on an ssh host behind a bridge client. `agterm-core::remote::ssh` (upstream's
    `attachCommand`) covers remote attach.
- **Baselines** (dev box: CachyOS, Linux 7.2.9, inside `agterm-agents.slice`, 10 CPUs, 10 GB; zmx pin `8bab1f0`
  with its patches):
  - `agterm-term`'s `bench.rs`, a `cat` of 50 MiB (`cat_s`, three runs): plain 0.462, 0.486 and 0.430 s; through
    `zmx attach` 0.801, 0.899 and 0.906 s;
  - a 50 MiB `cat` in the GPUI build (release, headless weston on lavapipe, through the suite's runner, `cat_s`,
    three runs): plain pane 1.503, 1.670 and 1.627 s; live zmx pane 2.466, 2.630 and 2.253 s;
  - idle zmx daemons, each holding an idle `/bin/sh` after 20 000 lines: 1 daemon 14.6 MiB RSS (13.9 MiB PSS);
    4: 54.3 MiB (45.6 MiB PSS); 16: 222.9 MiB RSS, 182.4 MiB PSS (13.9 MiB RSS, 11.4 MiB PSS each).

  Targets: `agterm-session`'s 50 MiB `cat` through a live pane is no slower than the zmx numbers, and the server's
  idle RSS for N such panes is no larger than N daemons' (Task 30 measures, Task 33 checks).

## What Goes Where

- **Implementation Steps** (`[ ]`): code, tests, CI workflows, scripts and docs in this repo, including debugging in
  the VMs.
- **Post-Completion**: manual end-to-end checks, the user's old sessions, future syncs.

## Implementation Steps

### Task 1: Scaffold `agterm-vt`, record provenance, inventory and calibrate

**Files:**
- Create: `crates/agterm-vt/Cargo.toml`, `crates/agterm-vt/src/lib.rs`, `NOTICE`
- Modify: `Cargo.toml` (`[workspace.dependencies]`; members are the `crates/*` glob), `Cargo.lock`, `build/pins.env`
  (`RUSTTY_*`)

- [x] create the crate under the workspace lints. Its crate doc states the provenance, the module-header convention
      and the unsafe policy
- [x] write the root `NOTICE` with Ghostty's and rustty's MIT texts; uucode, herdr and (if needed) wuffs are added by
      their tasks. Check that every crate inherits `license.workspace` (MIT, user 2026-10-16), except
      `agterm-session`, which declares `MIT AND Apache-2.0` for herdr's code (checked 2026-10-18: all 16 crates
      inherit it; `agterm-session` does not exist yet, Task 20 declares its license)
- [x] add `RUSTTY_REPO`/`RUSTTY_REV` to `build/pins.env`, with a comment saying what the pin is for
- [x] inventory, recorded in this plan (Technical Details > `agterm-vt` > "What Task 1 settles"):
  - the keep or cut verdict for every rustty-vt module named in Context, by the "part of libghostty-vt at the pin and
    reachable from agterm" rule;
  - the module graph (`use crate::`) of the kept set, its strongly connected groups, and any correction to Tasks
    4–15's boundaries;
  - the public functions of `agterm-ghostty-vt` that `agterm-term` calls, each mapped to the rustty item or
    `rustty-vt-c` logic that implements it, and to its task;
  - the drift files of every followed Ghostty directory (Technical Details), each assigned to a task
- [x] calibrate: copy the kept files (`.rs` only, no build scripts, no `-sys` crates) into a scratch crate after
      reading them, and run clippy with the workspace lints and `clippy.toml` in a sandbox (no network, scratch HOME).
      Record the counts per lint and per module here (done as a scratch workspace of rustty-vt and its leaf crates,
      `.scratch/vt-calibrate/`, counted over the kept modules only; see "What Task 1 settles" > Calibration)
- [x] write a smoke test and a doc test of the provenance constants
- [x] run gates - must pass before task 2

### Task 2: Study herdr's session server and spike its risky parts

**Files:**
- Create: `docs/spikes/20261018-session-server.md`, `docs/spikes/20261018-session-server-probes.md` (the probes'
  sources)
- Modify: this plan (Part B details, ➕ tasks), `build/pins.env` (`HERDR_*`), `NOTICE` (herdr's Apache-2.0 section)

- [x] pin herdr (`HERDR_REPO`, `HERDR_REV`, the current `main`) and read its server at that commit: `src/server`,
      `src/protocol`, `src/ipc.rs`, `src/client`, `src/session.rs`, `src/pane.rs` with `src/pane/**` (the per-pane
      runtime), `src/terminal`, `src/pty`, `src/server/handoff.rs` with `src/handoff_runtime.rs` and
      `src/api/server.rs`, `src/platform`, `src/api`, `src/cli`, `vendor/portable-pty`, and `src/persist` and
      `src/remote` only to record gaps
- [x] write the findings into Technical Details > `agterm-session` > "What Task 2 settles": the module map with
      verdicts, the wire, the runtime, any gaps. Adjust Tasks 20–32 and add ➕ tasks where the code asks for
      it
- [x] probe the Windows handoff in the `Win11-admin` VM (and on `windows-latest`), with a throwaway program, not
      merged: a detached server holds a ConPTY shell, and a second server process takes it over (duplicated handles)
      while the shell keeps running. Record go or no-go
- [x] probe on macOS, in the `macOS 26` VM and on a runner: the server spawned with `posix_spawn` + setsid + disclaim
      survives its launcher; a handoff over `SCM_RIGHTS` keeps the shell
- [x] decide the Windows namespace directory
- [x] record baselines on the dev box: `agterm-term`'s `bench.rs` (plain and through zmx), a 50 MiB `cat` through a
      live zmx pane in the GPUI build, the idle RSS of zmx daemons for N panes
- [x] add herdr's section to `NOTICE`
- [x] write the spike note. A no-go that blocks Part B stops it with a ⚠️ and a codex-discussion; Part A goes on

### Task 3: Cross-platform CI for `agterm-vt`

**Files:**
- Create: `.github/workflows/cross-platform.yml`

- [x] add a workflow with an `ubuntu`/`macos`/`windows` matrix: fmt, clippy `-D warnings` (all features) and tests
      of `agterm-vt`. Later tasks add their crates and generator checks to it (job `crates` on `ubuntu-26.04`,
      `macos-26`, `windows-2025`; a crate joins through `PACKAGES`; pushes to `ci/**` run it without a PR)
- [x] add a Miri job on a nightly pinned by date, over the memory core's tests. It is filled from Task 4 (job
      `miri`, `nightly-2026-10-18`, Linux: `cargo miri test -p agterm-vt --lib` filtered to `mem::`, `datastruct::`,
      `os::`; no such tests exist yet, and libtest passes the empty selection. Task 4 fills it)
- [x] push a branch and get it green on all three (throwaway `ci/agterm-vt-task3`, run 39104822219: all four jobs
      green; the branch deleted after)
- [x] run gates - must pass before task 4

### Task 4: VT memory core: page allocator, offset buffers, bitmap allocator, pools, lists

**Files:**
- Create: `crates/agterm-vt/src/mem/*.rs`, `crates/agterm-vt/src/datastruct/*.rs`, `crates/agterm-vt/src/os/*.rs`,
  `crates/agterm-vt/src/{sys,build_options,options}.rs`

- [x] port the page allocator from rustty-os, with its Linux, macOS (`mmap`) and Windows (`VirtualAlloc`) forms, all
      in this change (`os/page_allocator.rs`: an owned `PageMemory` unmapped on drop; Unix `mmap`, macOS tagged
      `VM_MEMORY_APPLICATION_SPECIFIC_1`, Windows `VirtualAlloc`/`VirtualFree` via `windows-sys`, the zeroed heap under
      Miri)
- [x] port the following behind safe types, with every `unsafe` block carrying its own `// SAFETY:` and one
      operation (codex-agreed design, transcript
      `~/.local/state/plan-exec-revmux/zig-free-core/discussions/task-4-memory-core-api/transcript.md`: page memory is
      a byte slice and every value stored in it is `bytemuck::Pod`, read through bounds- and alignment-checked casts
      (`mem/offset.rs`), so the bitmap allocator, ref-counted set and hash map have no `unsafe` at all; `alloc` hands
      out a consuming `Allocation<T>`, contexts get the page's bytes per call instead of a stored page pointer, items
      keep the reference stride; the only `unsafe` is the page allocator, `mem::alloc::try_box` and the test allocator.
      Not created here: `sys` (its raw allocator served only the PNG decode seam, so it is born in Task 10), and
      `limited_allocator`, lib `allocator` (the C ABI), `quirks`, `array_list`, `atomic`, `IntFromBase`; zigstd
      `arena_allocator` moves to Task 9 with the pool, its only user; Technical Details > Owners records the moves.
      `build_options`/`options` hold `Artifact` and `ARTIFACT = Lib`, libghostty-vt's, plus `SLOW_RUNTIME_SAFETY`):
  - from rustty-vt: `alloc`, `bitmap_allocator`, `ref_counted_set`, `hash_map`, `zig_list` (`mem` is cut with page
    compression, Task 1), and `sys` with its global raw allocator, which `alloc` uses (its PNG decode comes with
    Task 10, its secure-random fill with Task 11);
  - from rustty-vt: `build_options` and `options`, which `sys` reads (`options::ARTIFACT`,
    `build_options::Artifact`), cut to the constants the kept code branches on, with no feature flags (Task 1 >
    Adapted). Later tasks drop a branch on an always-on feature rather than add a flag; the version constants come
    with Task 15;
  - from rustty-render-types: `term_size`'s offsets (`Offset`, `OffsetSlice`, `OffsetBuf`, `IntFromBase`);
  - the leaf modules those need (Task 1 > Owners): util `quirks`; datastruct `array_list`, `limited_allocator`; lib
    `allocator`; zigstd `arena_allocator`, `array_hash_map`, `atomic`, `wyhash`
- [x] replace the zigstd and allocator-api2 shims with `std` where the semantics allow. Note each shim that remains,
      with its reason (allocator-api2, the runtime allocator handle and Zig's growth traces are gone: containers are
      `std`'s. Remaining: `mem::alloc::try_box`, since `Box::try_new` is unstable; `TryVecExt`/`try_dupe`/
      `AllocatingWriter`, thin names for Zig's `try` growth over `try_reserve*`; `datastruct::array_hash_map`, since
      `std` has no insertion-ordered map (now `Vec` + `HashMap`); `datastruct::wyhash`, since the page maps need Zig's
      digest bit for bit; `mem::testing`, a test-only global allocator for failure injection, which `std` lacks)
- [x] port their rustty tests (adapted to offsets over test buffers; rustty's C-ABI allocator tests and Zig
      allocation-trace tests went with what they tested; the hash map's PRNG-driven tests use a Zig-exact PRNG and keep
      their ground truth)
- [x] add tests for the safe wrappers' error paths: exhaustion, out-of-range offsets refused, a double free
      impossible by type (a static not-`Clone` assertion on `Allocation`, plus `free_slice` refusing a double free)
- [x] Miri green locally and in Task 3's job; the macOS and Windows forms debugged in the VMs and green on CI
      (Miri filter now `mem:: datastruct:: os::page_allocator::`, 132 tests; VMs macOS 26 x86-64 and Win11: fmt,
      clippy, 135 tests green; run 39111730195 green on ubuntu-26.04, macos-26, windows-2025 and miri)
- [x] run gates - must pass before task 5 (all green except the known load-dependent `agterm-gpui` html_overlay
      frame-rate test, 16.0 fps at load average 22-25)

### Task 5: Unicode lookups, UTF-8 decoder, scalar SIMD paths

**Files:**
- Create: `crates/agterm-vt/src/unicode/**`, `crates/agterm-vt/src/simd/**`,
  `crates/agterm-vt/src/terminal/utf8_decoder.rs` (rustty-vt's `utf8_decoder`)

- [x] port the props, grapheme-break, case-folding and symbol lookups with rustty's tables, as rustty dumped them
      (`12752b2ac`). Task 6 regenerates them at the pin (`unicode/{lut,props,props_table,symbols_table,grapheme,
      case_folding}.rs` and `unicode/generated/*`: the values rustty's, token for token, under headers naming their
      inputs, so Task 6's byte-for-byte reproduction holds its output to these files as they are; the lookups are
      safe Rust: rustty's two unchecked stage reads are checked reads, with a test holding the generator invariant;
      a code point is a `u32` masked to `u21`; nothing is public, `main.zig`'s re-exports are module paths)
- ➕ moved to Task 6 with the generator: `lut.zig`'s `Generator` and `writeZig` (with rustty's `write_zig` test),
      `Properties.eql`/`format`, the `props_uucode`/`symbols_uucode` generator mains (their out-of-range rule and
      spot checks run on the tables here), and `grapheme.zig`'s debug sweep `main`
- [x] port the UTF-8 decoder and the scalar forms of rustty-simd that the stream uses (ESC search, codepoint width,
      base64) (`terminal/utf8_decoder.rs`, `simd/{index_of,vt,codepoint_width,base64,base64_scalar}.rs` and
      `lanes`/`native_vector_bits`. "Scalar forms" means no SIMD library: where the two Ghostty lanes differ on a path
      the terminal reaches, the port behaves as the lane libghostty-vt ships (SIMD, the default). That is base64's
      non-strict `maxLen`/`decode` (Kitty graphics payloads): simdutf's forgiving base64 (whitespace skipped,
      padding validated, a one-character remainder refused, loose tail bits) written around Ghostty's scalar
      decoder, where the scalar lane rejects whitespace and accepts any trailing `=` run; codex-agreed, transcript
      `~/.local/state/plan-exec-revmux/zig-free-core/discussions/task-5-base64-lane/transcript.md`. Held to simdutf
      9.0.0 from Ghostty's `pkg/simdutf` at the pin by 53 recorded cases and, locally, a 200,000-input random corpus
      (all agree, in place too). `base64_scalar` also holds Zig 0.16's `std.base64.standard` and `standard_no_pad`
      decoders, the ones `program_status` (OSC 7501) decodes with (Task 1 > Verdicts moved them here from the cut
      glyph code): one type with Ghostty's copy, which only skips Zig's tail-bits check; Zig's decoder tests ported,
      and a local 200,000-input corpus run against Zig 0.16 itself agreed. The scalar lane's `log.warn` has no
      counterpart, so `log_capture` stays Task 8's. The UTF-8 decode and the byte search answer alike on both lanes;
      `simd.codepointWidth` is the scalar lane's uucode width and has no terminal caller)
- [x] port their tests, including the FNV checksum test (the checksums also pin the `Properties` stage and the case
      folding list; `vt.zig`'s SIMD-against-scalar differential holds the scalar lane to the standard library's
      lossy decode, which substitutes maximal subparts too; no `unsafe` in these modules, so the Miri filter stays
      the memory core's)
- [x] run gates - must pass before task 6 (all green except two load-dependent `agterm-gpui` tests that
      `agterm-vt` cannot reach, each in one run at load average 27-51 and green in the other or alone: the
      html_overlay frame-rate test, 15.7 fps, and the quit test's live-session survival; CI run 39119593502 green on
      ubuntu-26.04, macos-26, windows-2025 and miri)

### Task 6: Unicode table generator in Rust (uucode port)

**Files:**
- Create: `crates/agterm-vt/src/bin/gen-unicode.rs` and its modules (feature `gen`), `scripts/gen-vt-unicode.sh`
- Modify: `build/pins.env` (uucode and UCD pins with sha256s), `NOTICE` (uucode), `.github/workflows/cross-platform.yml`
  (`--check`), `crates/agterm-vt/src/unicode/generated/*`

- [ ] check the uucode version, `uucode_config.zig` and UCD at `LIBGHOSTTY_VT_REV` against what rustty dumped
      (uucode 0.2.0 @ `9d555245`, `12752b2ac`), and record the differences here
- [ ] port uucode's UCD parsing and derivations and Ghostty's `uucode_config.zig` that the tables need, with uucode's
      MIT attribution
- [ ] `scripts/gen-vt-unicode.sh [--check]`; its pinned inputs are fetched and checked by sha256 into the vendor root
- [ ] test that, run at rustty's inputs, the generator reproduces rustty's tables byte for byte
- [ ] regenerate at the pin's inputs. Update the checksum test, and add tests for the code points whose properties
      changed
- [ ] add a test that `--check` fails on a changed table, and add `--check` to CI
- [ ] run gates - must pass before task 7

### Task 7: Terminfo entry and its generated source

**Files:**
- Create: `crates/agterm-vt/src/terminfo/**`, `crates/agterm-vt/src/bin/gen-terminfo.rs` (feature `gen`),
  `scripts/gen-ghostty-terminfo.sh`, `crates/agterm-vt/terminfo/xterm-ghostty.terminfo`

- [ ] port rustty-terminfo's source model and encoder, and the Ghostty entry under Ghostty's names (`xterm-ghostty`,
      `ghostty`), with the drift of `src/terminfo`
- [ ] port rustty-identity, which the entry reads first (Task 1 > Owners), with Ghostty's identity restored:
      Ghostty's names and strings in place of rustty's profile
- [ ] port the XTGETTCAP map built from the same entry (used by Task 11's handler)
- [ ] `scripts/gen-ghostty-terminfo.sh [--check]` writes the checked-in source; add `--check` to CI
- [ ] capture today's entry from the Zig-built prefix once, as a fixture (`infocmp -x -A <prefix>/share/terminfo
      xterm-ghostty`), so the check outlives Task 18. Test that the checked-in source, compiled with
      `tic -x -o <tempdir>`, gives the same `infocmp` output
- [ ] add XTGETTCAP tests: a known capability, an unknown one, hex-encoded names
- [ ] run gates - must pass before task 8

### Task 8: Parser and stream: parse table, CSI/OSC/APC parsing, SGR, charsets, modes, colors

Before the page group (Task 1's graph): `style` uses `sgr`, and nothing here uses page memory. `dcs` moved to Task 11.

**Files:**
- Create: `crates/agterm-vt/src/terminal/{parser,parse_table,stream,stream_continuation,csi,sgr,charsets,modes,
  ansi,device_attributes,device_status,size_report,apc,color,x11_color,fraction,protocol_int,mouse}.rs`,
  `crates/agterm-vt/src/terminal/osc/**`, `crates/agterm-vt/src/terminal/kitty/{graphics_command,color,key}.rs`,
  `crates/agterm-vt/src/log_capture.rs` (test-only)
- Leaf modules it is first to use (Task 1 > Owners): datastruct `segmented_list`, `comptime_int_set`; os
  `string_encoding`; lib `enum`, `parse_int`, `target`; zigstd `fmt`

- [ ] port the parser state machine and the stream dispatch, generic over its handler, without the tmux and
      glyph-protocol branches
- [ ] port every OSC parser agterm reaches today (title, pwd, hyperlinks, colors, clipboard, notifications, progress,
      `program_status`, `context_signal`, semantic prompt, Kitty's 5522 clipboard) and the Kitty graphics command
      parser, imported directly by `apc` (not through the `kitty/graphics` aggregate). `program_status` decodes
      base64 with Task 5's decoder, since rustty took its decoder from the cut glyph code
- [ ] port what the group uses beside them: the colors (`color`, `x11_color` with its `rgb.txt`, `fraction`), the
      mouse shape, Kitty's OSC 21 colors and its keyboard flags, and `protocol_int` (the digit-separator filter the
      Kitty graphics command parser runs before parsing a number)
- [ ] port `log_capture` as a test-only helper over `tracing` (Task 1 > Verdicts): rustty's tests assert on log
      lines, here and in Tasks 9 to 14
- [ ] absorb the drift for `stream.zig`, `osc/parsers/*`, `modes.zig`, `csi.zig`, `device_status.zig` and
      `size_report.zig`
- [ ] port the stream, OSC, parser, SGR, modes, color and graphics-command tests
- [ ] add tests for each drifted parser
- [ ] run gates - must pass before task 9

### Task 9: Page, PageList, style, hyperlink, resize and reflow (without page compression)

**Files:**
- Create: `crates/agterm-vt/src/terminal/page*.rs`, `crates/agterm-vt/src/terminal/page_list/**`,
  `crates/agterm-vt/src/terminal/{style,hyperlink,point,size,fastprint}.rs` (`size`: render-types `term_size`'s
  cell-count types; its offsets came with Task 4)
- Leaf modules it is first to use (Task 1 > Owners): util `fastmem`, `tripwire`; datastruct `intrusive_linked_list`,
  `untouched_pool`; lib `packed`; zigstd `memory_pool`, `heap`, `tracking_allocator` (tests)

- [ ] port `page_impl`/`page` (cells, rows, grapheme map, style set, hyperlink set, string storage) over Task 4's
      core, safe above it
- [ ] port `pagelist`: scrollback with its limits, pins, iteration, erase, scroll, resize with reflow, page reuse.
      Split whatever goes over the limits. `highlight_semantic_content`, its one edge into `highlight`, comes with
      Task 10 together with its tests
- [ ] cut idle compression out of `PageList` (`NodeData::Compressed`, `compress`), and keep everything else that
      touches node data
- [ ] absorb this group's drift
- [ ] port the rustty tests (`pagelist/tests/*` minus compression, `page_tests`, `ref_geometry_golden_tests`)
- [ ] add tests for the drift and for the compression cut (no compressed node is ever produced; scrollback limits
      still hold)
- [ ] run gates - must pass before task 10

### Task 10: Screen with its formatter, selection and Kitty storage (one dependency group)

**Files:**
- Create: `crates/agterm-vt/src/terminal/{screen,screen_set,tabstops,cursor,selection,selection_codepoints,
  highlight}.rs`, `crates/agterm-vt/src/terminal/screen/**`, `crates/agterm-vt/src/terminal/formatter/**`,
  `crates/agterm-vt/src/terminal/kitty/{graphics_storage,graphics_image,graphics_pixel,graphics_unicode,
  graphics_render,graphics_animation,windows}.rs`, `crates/agterm-vt/src/terminal/kitty/graphics_storage/**`, the
  os support it reads files through (`file`, `path`, `tempdir`)
- Modify: `crates/agterm-vt/src/sys.rs` (PNG decode)
- Leaf modules it is first to use (Task 1 > Owners): datastruct `comparison`, and the os ones above

The screen-level part of the component Task 1 found, by its item extractions (Technical Details > `agterm-vt` >
"What Task 1 settles"): nothing here takes a `Terminal`.

- [ ] port `Screen`: primary and alternate, cursor, saved cursor, selection storage, semantic prompt marks, the Kitty
      image storage it owns. `PrintStringError` is defined here, and Task 11's `Terminal` re-exports it. `highlight`
      comes too, with pagelist's `highlight_semantic_content` and its tests (`pagelist/tests/iter_clone.rs`)
- [ ] port the formatter (plain, VT, HTML; page, page list, screen, selection, ranges; the snapshot's extras) with
      the drift for `formatter.zig` and `Screen.zig`. `TerminalFormatter` comes with Task 11
- [ ] check zmx's `ghostty/0001-formatter-unstyled-blanks` against the pin. If the pin lacks it, add it as a
      formatter option, with tests
- [ ] port the selection operations (cell ranges, word, line, all, adjust)
- [ ] Kitty images: loading (direct, file, temp file and shared memory, with the POSIX and Windows forms of what it
      reads), zlib through `miniz_oxide`, PNG through `png` (in `sys`, in place of its `decode_png` hook), the
      swizzles in Rust, the drift for
      `kitty/graphics_image.zig`
- [ ] Kitty storage: the placement-id namespace, a relative placement's parent key and H=/V= offsets, eviction
      (`evict_image`, `evict_image_except`, `delete_if_unused`) and animation. Placement geometry (`cell_offset`,
      `pixel_size`, `grid_size`, `clip_top`, `rect`) takes the terminal's geometry as a value, and the scroll-margin
      hooks take the screen and its margins, not the `Terminal`. Command deletion (`clear_screen`, `delete` and
      their helpers) comes with Task 11
- [ ] port the screen, formatter, selection and Kitty storage and image tests that need no `Terminal`. The ones
      built on a `Terminal` move to Task 11: formatter's `page_plain`, `terminal_screen`, `html_codepoint_pinmap` and
      `page_vt_pagelist`, and Kitty storage's and `graphics_unicode`'s. Adapt representative cases of the
      refactored geometry, eviction and formatting to screen or geometry fixtures here
- [ ] add tests for word boundaries, wide characters, soft wraps in plain output and HTML escaping
- [ ] add tests for each swizzle on known pixels, compressed payloads (good and corrupt), PNG (good and truncated),
      and the exposed parent and namespace fields
- [ ] run gates - must pass before task 11

### Task 11: Terminal with its stream handler, effects and Kitty execution (one dependency group)

**Files:**
- Create: `crates/agterm-vt/src/terminal/terminal.rs`, `crates/agterm-vt/src/terminal/terminal/**`,
  `crates/agterm-vt/src/terminal/stream_terminal/**`, `crates/agterm-vt/src/terminal/{effects,clipboard,dcs,paste,
  fixed_writer}.rs`, `crates/agterm-vt/src/input/paste.rs`, `crates/agterm-vt/src/terminal/kitty/{graphics_exec,
  clipboard*,dnd*}.rs`, `crates/agterm-vt/src/terminal/kitty/graphics.rs` (the aggregate, which re-exports
  execution), a writer support module in place of rustty's `snapshot::io`
- Modify: `crates/agterm-vt/src/sys.rs` (secure-random fill), Task 10's os `file` (its CSPRNG)

The terminal-level part of the component Task 1 found, with what Task 10 left for it.

- [ ] port `Terminal`: print, wrap, erase, insert/delete, margins, scroll regions, resize, viewport scrolling (by
      prompt too), with the drift for `Terminal.zig`. It re-exports Task 10's `PrintStringError`
- [ ] port `stream_terminal`'s handler without its snapshot, glyph, tmux and DECRQCRA branches, with XTGETTCAP from
      Task 7, and `dcs` (DECRQSS, XTGETTCAP requests) without its tmux branch. Absorb the drift for
      `stream_terminal.zig` and `kitty/clipboard_write.zig`. rustty's `snapshot::io` writers, used by the handler,
      clipboard and paste, become a small writer support module (or `std::io::Write`)
- [ ] define the effects trait with the C API's callback set, called synchronously with a read view of the terminal,
      and a no-op default
- [ ] port paste framing and safety (`paste`, `input/paste`; moved from Task 14), with the drift for
      `input/paste.zig` and `terminal/paste.zig`
- [ ] port Kitty command execution with the storage's command deletion (`clear_screen`, `delete` and their helpers),
      and the kept Kitty modules: the 5522 clipboard with its grants, DnD (OSC 72), and the `kitty/graphics`
      aggregate. Task 10's Kitty modules import their siblings directly
- [ ] port `sys`'s secure-random fill (`random_secure_fill`, `io_native`, `io_failing`) without its global
      `random_secure` hook, over the OS CSPRNG in os `file` (`random_secure`; `random_secure_threaded` on Windows),
      which the clipboard grants, paste and `Terminal` use, with its Linux, macOS and Windows forms in this change
- [ ] port `TerminalFormatter` over Task 10's formatters
- [ ] restore Ghostty's identity wherever rustty renamed it
- [ ] port the terminal, `stream_terminal/tests/*`, dcs, paste and Kitty execution tests, and the tests Task 10 moved
      here (formatter's and Kitty storage's built on a `Terminal`)
- [ ] add tests that each effect fires with the payload the C API delivered (`agterm-ghostty-vt`'s
      `tests/effects.rs` as the list), and for the identity strings and the drift
- [ ] run gates - must pass before task 12

### Task 12: Render state

**Files:**
- Create: `crates/agterm-vt/src/terminal/render/**`

- [ ] port `RenderState`: an update in one call or two phases, dirty tracking, rows and cells, cursor, colors.
      Reading it does not hold the terminal
- [ ] absorb the drift for `render.zig`
- [ ] port the render-state cases that rustty-vt-c's `render/tests.rs` pins onto the Rust API
- [ ] add tests for two-phase updates and dirty rows after scroll, resize and alternate-screen switches
- [ ] run gates - must pass before task 13

### Task 13: Scrollback search and the selection gesture

**Files:**
- Create: `crates/agterm-vt/src/terminal/search/**`, `crates/agterm-vt/src/terminal/selection_gesture.rs`

- [ ] port the screen search and its sliding window over both screens' active area and scrollback, keeping the split
      between calls that touch the terminal and ticks that do not (rustty-vt-c's `search.rs` logic included)
- [ ] port `selection_gesture` (click counts by time and distance, drags, autoscroll), with rustty-vt-c's
      `selection_gesture.rs` logic
- [ ] port the search and gesture tests
- [ ] add tests for matches across soft wraps and page boundaries, both directions, a needle longer than a page, and
      gesture timing edges
- [ ] run gates - must pass before task 14

### Task 14: Input encoders

Paste moved to Task 11 (Task 1's graph: the stream handler uses it).

**Files:**
- Create: `crates/agterm-vt/src/input/{key,key_encode,key_mods,mouse,mouse_encode,focus,function_keys,kitty,
  config}.rs`

- [ ] port the key encoder (legacy, application modes, modifyOtherKeys, Kitty flags), the mouse encoder (all
      tracking modes and formats) and focus reports
- [ ] port the encoder and focus tests
- [ ] add tests for each `Key` and modifier side that `agterm-term`'s `input.rs` maps
- [ ] run gates - must pass before task 15

### Task 15: Embedder layer and the public API surface

**Files:**
- Create: `crates/agterm-vt/src/embed/**`, `crates/agterm-vt/tests/api.rs`
- Modify: `crates/agterm-vt/src/lib.rs`

- [ ] port the logic of `rustty-vt-c` that Task 1's inventory maps here, onto the Rust API:
  - `terminal/write.rs`: `vt_write_until_ground`, `at_ground`, the continuation;
  - `terminal/effects.rs` plumbing, wherever the trait needs it;
  - `kitty_graphics.rs` `placement_render_info`;
  - the getters (`cursor_at_prompt`, `viewport_top`, `viewport_at_bottom`, `total_rows`, `hyperlink_uri`);
  - `reset`, `set_mode_default`, the Kitty media and storage-limit settings, `version` (with `options`' version
    constants over Task 7's identity)
- [ ] export the surface (Technical Details, completed by Task 1) and keep everything else crate-private. Handles are
      `Send` and not `Sync`
- [ ] port the compile-fail doc tests from `agterm-ghostty-vt`'s `lib.rs`
- [ ] port rustty-vt-c's tests for this logic (`terminal/tests/get.rs` and others) onto the Rust API
- [ ] write `tests/api.rs`, which uses every entry point that `agterm-term` calls once, end to end
- [ ] run clippy and `scripts/lint-report.sh`; read every non-complexity `#[expect]`
- [ ] run gates - must pass before task 16

### Task 16: Differential tests and the micro performance check against libghostty-vt

**Files:**
- Create: `crates/agterm-vt-diff/` (test-only crate depending on `agterm-vt` and `agterm-ghostty-vt`, its
  `tests/differential/**` and `tests/corpus/**`)

- [ ] build the corpus:
  - per-area escape streams: SGR, cursor, erase, scroll regions, alternate screen, wraps and resize, wide and
    grapheme text, OSC effects, Kitty, semantic prompts;
  - captured program output;
  - rustty's fuzz seeds, after their license is checked
- [ ] feed each stream to both cores and compare text, cells and styles, cursor, modes, replies, effects, selections,
      search matches and placements. Fix each difference in `agterm-vt`, or explain it here
- [ ] add the `#[ignore]`d parse and render-state measurement over both cores, record both numbers here, and optimize
      until the new core is within 10%
- [ ] add a recorder that writes each stream's expected results as golden files (used in Task 18)
- [ ] check that `cross-platform.yml` (`-p agterm-vt` only) stays green on all three OSes, and that `rust-linux.yml`
      runs the differential tests
- [ ] run gates - must pass before task 17

### Task 17: Switch `agterm-term` to `agterm-vt`

**Files:**
- Modify: `crates/agterm-term/Cargo.toml` (dependency, crate description),
  `crates/agterm-term/src/{lib,pane,render,effects,input,selection,gesture,search,kitty,text,wheel,exit,actions,
  launch}.rs`; `crates/agterm-gpui/src/terminal/*` if the types reach it
- Delete: `crates/agterm-term/src/png.rs`

- [ ] replace `agterm_ghostty_vt` with `agterm_vt` in every module, adopting the new names. The effects become the
      trait's implementation, and the log and PNG hooks go
- [ ] read the Kitty namespaces and parent links from the storage directly
- [ ] update `agterm-term`'s tests to the API. Behavior assertions stay as they are
- [ ] run `agterm-term`'s and `agterm-gpui`'s tests headless (`xvfb-run` with a private bus), green, with the live
      process tests running, not skipping
- [ ] run `bench.rs` and compare with Task 2's baseline. The new core may be at most 10% slower; record the numbers
      here
- [ ] full `conformance/tools/run-rust-linux.sh` in the background; every ratchet id green
- [ ] run gates - must pass before task 18

### Task 18: Remove libghostty-vt, its bindings and Zig from the Ghostty build

**Files:**
- Delete: `crates/agterm-ghostty-vt/`, `crates/agterm-ghostty-vt-sys/`, `scripts/gen-ghostty-vt-bindings.sh`,
  `build/patches/ghostty-vt/`
- Create: `crates/agterm-term/tests/prefix_keys.rs` (from the -sys crate's)
- Modify: `scripts/build-ghostty-vt.sh`, `scripts/lib/native.sh` (`prefix_key` with the terminfo file),
  `build/pins.env` (drop `LIBGHOSTTY_VT_ZIG_*`; the comment), `Cargo.toml`, `Cargo.lock`, `scripts/stage.sh`,
  `scripts/lint-report.sh`, `.github/workflows/rust-linux.yml` (the Zig loop over `ZMX` only, no library build, cache
  keys), `crates/agterm-wpe-sys/tests/provider.rs` (the files it copies),
  `.claude/skills/agterm-upstream-sync/{SKILL.md,scripts/pins-delta.sh,references/areas.md,references/plan-template.md}`,
  `crates/agterm-vt-diff/` (deleted), `crates/agterm-vt/tests/golden/**` (created)

- [ ] turn the differential tests into golden tests over Task 16's files, in `agterm-vt`'s own tests, and delete
      `agterm-vt-diff`
- [ ] delete both crates, the bindings generator and the patch directory
- [ ] `build-ghostty-vt.sh` (same name and options) stops building a library: the source archive and theme package
      with their sha256s, the shell integration, the themes, and `tic -x -o` over the checked-in terminfo. Nothing in
      it runs Zig or cargo
- [ ] `prefix_key` hashes the terminfo file. Port `prefix_keys.rs`: the Ghostty and zmx prefixes stay independent,
      and a terminfo change rekeys only the Ghostty one
- [ ] update every reader: `rust-linux.yml` (in this task, so CI never runs a deleted step), `provider.rs`,
      `stage.sh`, `lint-report.sh`, the upstream-sync skill. The gpui live tests, the lavapipe selftest and the
      config-key test keep working through the kept names; check each one
- [ ] add a check that the prefix builds with no `zig` on PATH and a fresh vendor root
- [ ] run gates and the full conformance run - must pass before task 19

### Task 19: Rename zmx out of agterm's code and control protocol, with a vocabulary per suite adapter

**Files:**
- Modify (proto and core): `crates/agterm-proto/src/{command,result,tree}.rs` and `projection.rs` (`Group::Zmx`, the
  `zmx.*` group, `result.zmx`, `backedByZmx`), `crates/agterm-core/src/zmx/` → `crates/agterm-core/src/live/`,
  `crates/agterm-core/src/commands/zmx*` → `commands/live*` (and `commands/tests/zmx.rs`), `crates/agterm-core/src/html/bridge.rs`
  (the `ZmxCmd` allowlists), `crates/agterm-core/src/remote/{ssh,tree}.rs` with their tests (the far side's
  `agtermctl zmx present` and `agtermctl zmx tree`), `crates/agterm-core/src/restore/mode.rs` (`AGTERM_UITEST_ENABLE_ZMX`)
- Modify (callers): `crates/agterm-zmx/src/{client,launch,inventory,sweep}.rs` (imports only), `crates/agterm-term/src/launch.rs`,
  `crates/agterm-app/src/remote.rs`, `crates/agterm-gpui/src/*`, `crates/agterm-server/tests/server.rs`,
  `crates/agterm-gpui/tests/{privileges,quit}.rs`
- Modify (CLI): `crates/agterm-ctl/src/zmx.rs` → `live.rs`, `crates/agterm-ctl/src/{main,output,clipboard,restore,stream_bridge}.rs`,
  `crates/agterm-ctl/tests/{cli,cli_local}.rs`
- Modify (skill): `resources/skills/agterm/SKILL.md` through a patch that `sync-resources.sh` reapplies
  (`build/patches/skill-agterm/`)
- Modify (suite): `conformance/agterm_conformance/impls/{base,rust,swift_linux,swift_macos}.py` (the vocabulary
  objects), `conformance/agterm_conformance/{cli_format,zmx_client}.py`, the shared tests that read
  vocabulary-dependent values (`tests/protocol/{test_zmx,test_restore,test_live_any_shell}.py`,
  `tests/cli/{test_cli_only,test_cli_output}.py`, `tests/behaviors/test_relaunch.py`, `tests/ui/test_live.py`),
  `conformance/selftests/*`
- Modify (docs): `conformance/docs/rust-build.md`, `.claude/skills/agterm-upstream-sync/references/areas.md`

- [x] wire: rename the group `zmx.*` → `live.*` (`live.list`, `prune`, `kill`, `reset`, `tree`, `attach`, `present`,
      `screen`; `Group::Live`), the result key `zmx` → `live`, the tree key `backedByZmx` → `backedByLive`
- [x] `agtermctl zmx …` → `agtermctl live …`, with help, usage and output texts, and the far side's command lines in
      `remote/ssh.rs`, `remote/tree.rs` and `agterm-app/src/remote.rs`, with their tests (or remote attach breaks
      while its unit tests stay green). A new build then can't view an older build's sessions either; record that
- [x] rename `agterm-core::zmx` → `agterm-core::live` with its commands, types and constants (`ZmxCmd`,
      `ControlZmxInventory`, `ZmxRuntime`, `UNAVAILABLE`, …), and `AGTERM_UITEST_ENABLE_ZMX` →
      `AGTERM_UITEST_ENABLE_LIVE`. Every error string that names zmx gets the live-session vocabulary (`"live.kill
      requires --force"`, `"this pane has no live session…"`), and each changed upstream string is listed in this task
  - changed upstream strings (Task 19): `zmx.kill …` (`requires an explicit --target`, `requires --pane
    left|right`, `requires --force`, `needs a session id/window id; 'active' is not accepted`), `zmx.screen requires a
    daemon name`, `zmx.reset …` (`requires --force`, `requires Live sessions mode …`, `could not read the live session
    list`, `refused: the pane inventory is incomplete`, `found no live session to reset`), `zmx.present requires a
    session` and `zmx.attach requires a host / a remote session` all under `live.`; `zmx is unavailable in this
    instance` → `live sessions are unavailable in this instance`; `could not (re-)read the zmx session list…` →
    `… the live session list…`; `could not read the zmx screen of NAME` → `could not read the live session screen of
    NAME`; `… zmx cleaned up a stale socket …` → `… the session host cleaned up …`; `the pane's zmx daemon did not
    accept the input` → `the pane's live session …`; `the remote agterm is too old to report its zmx endpoint` → `…
    its live-session endpoint`; `the zmx executable path is not absolute` / `is unavailable` → `the live-session
    executable …`; `this pane has no zmx daemon; …` → `this pane has no live session; …`; `no zmx next to this
    agtermctl` → `no live-session executable next to this agtermctl`; `zmx print exited N: …` → `the live-session
    print exited N: …`; the CLI help of `live` and `restore mode`. The table is in `conformance/docs/rust-build.md` >
    The `live` group
- [x] what the zmx binary itself speaks stays until Task 28 (and its parsers until Task 32), untouched by the sweep:
  - the `attach` subcommand and its variables (`ZMX_DIR`, `ZMX_SESSION*`, `ZMX_MANAGED*`, `ZMX_NO_DETACH_KEY`,
    `ZMX_TRACK_ENV`);
  - the socket directory;
  - the role-title prefix `zmx-role;` that the patched zmx writes and `lead.rs` parses;
  - the `list` and `screen` output formats (`lifecycle::parse_list`, `screen::parse_screen`);
  - `bin/zmx`, `share/agterm/zmx-BUILD`, `AGTERM_ZMX_PATH`;
  - the `agterm-zmx` crate, beyond its imports
- [x] the suite keeps upstream's vocabulary in its tests, catalog and `covers(...)` markers, because they are shared
      with the Swift references:
  - each adapter exposes a vocabulary object: command names, result and tree keys, CLI argv, error-text templates,
    environment names, the socket-directory attribute, and the leading client;
  - `base.py`, `cli_format.py` and the shared tests read every vocabulary-dependent value from it;
  - the Swift adapters' vocabulary is upstream's, and the Rust adapter's is `live`;
  - no test id changes, so the ratchet, `catalog.yaml` and the catalog gate need no edit
- [x] the bundled `agterm` skill gets the live names through a patch in `build/patches/skill-agterm/` that
      `sync-resources.sh` reapplies after every upstream sync, with a `--check`
- [x] record the deviation from upstream (the names, the changed strings, the vocabulary objects, the skill patch) in
      `conformance/docs/rust-build.md`, and in the upstream-sync skill's areas reference: an upstream `zmx.*` change
      ports onto `live.*`
- [x] tests:
  - the dispatcher's tests on the new names with full error strings;
  - `agtermctl`'s parse and output tests;
  - `agterm-server`'s and the gpui process tests on the new names;
  - the remote command-line tests;
  - harness selftests for each adapter's vocabulary
- [x] full conformance run in the background: every ratchet id green, ids unchanged
- [x] run gates

### Task 20: `agterm-session` crate: wire and endpoint

**Files:**
- Create: `crates/agterm-session/Cargo.toml` (license `MIT AND Apache-2.0`), `crates/agterm-session/src/{lib,protocol,endpoint}.rs`,
  `crates/agterm-session/tests/{protocol,endpoint}.rs`
- Modify: `Cargo.toml` (`interprocess`, `windows-sys`, `postcard`; no async runtime, Task 2),
  `.github/workflows/cross-platform.yml`

- [x] port herdr's wire (framing, version negotiation) with the message families Task 2 kept: hello, the api
      requests and replies, attach (snapshot begin/chunk/end, offset-tagged output, input, reply metadata and
      delegated requests, resize, role, exit), handoff control. Encoding postcard, behind a stable envelope (magic
      and protocol version) that every version decodes; build identity and capabilities in the welcome; one
      listener with a connection per role (Task 2 > Wire)
  - settled with Codex (transcript `discussions/task-20-wire-envelope`): a frozen 16-byte header on every frame
    (`AGLS` | protocol | kind | length); kind 1 is the stable JSON control layer every version reads (`hello.v1`
    with role `control`/`api`/`attach`, `welcome.v1` with build, capabilities, pid and an optional error code,
    `handoff.request.v1` {import_exe, expected_build, expected_protocol}, `handoff.result.v1`), kind 2 the role's
    postcard messages after an exact protocol match. The `control` role is accepted across protocols, so a new
    build can ask an old server to hand off (Task 25 advertises `handoff.request.v1` once it implements it); only
    the header, the kinds and the control operations are frozen (golden fixtures), the postcard families change
    with `PROTOCOL_VERSION`
  - the Unix endpoint also refuses a namespace directory that is not this user's (a symlink, another owner) and
    closes one others can reach; the Windows client refuses a pipe whose server runs as another user (Task 2 >
    Windows namespace)
- [x] port herdr's endpoint (`src/ipc.rs`, `src/server/socket_paths.rs`) onto agterm's namespace: Unix sockets under
      the instance's socket directory; on Windows, named pipes with herdr's owner-only SDDL, plus
      `FILE_FLAG_FIRST_PIPE_INSTANCE` (agterm's addition)
- [x] give every herdr-derived file the header from Development Approach > Provenance
- [x] add the crate to `cross-platform.yml`
- [x] tests:
  - each message round-trips;
  - version mismatch refused with its message, and the envelope of a newer protocol still read;
  - a truncated frame and an oversized length refused;
  - an over-long Unix socket path refused;
  - a second pipe instance under a taken name refused
- [x] run gates

### Task 21: `agterm-proto`, `agterm-host` and `agterm-core` on Windows

**Files:**
- Modify: `crates/agterm-core/src/live/{mod,support}.rs` (and their tests), `crates/agterm-core/src/restore/mode.rs`,
  `crates/agterm-core/src/commands/hud.rs`, `crates/agterm-core/src/ghostty_config/launching.rs`
  (`OsStringExt`, non-test code), the Unix-only test code in `crates/agterm-core/src/{html/profile,link_policy}.rs`,
  `crates/agterm-core/src/commands/tests/hud.rs`, `crates/agterm-core/tests/persistence.rs`,
  `crates/agterm-host/src/fakes.rs`, `.github/workflows/cross-platform.yml`

- [ ] `socket_directory` per OS (Task 2's Windows directory). The Unix-only spots go behind `cfg` with their Windows
      forms beside them
- [ ] core tests that assume `/bin/sh -c` or `/tmp` gain their Windows forms, or are gated with the reason
- [ ] add `agterm-proto`, `agterm-host` and `agterm-core` to `cross-platform.yml`. `agterm-server` (AF_UNIX,
      `flock`) and `agterm-ctl` stay out, recorded with their reason: the control server's Windows form belongs to
      the GPUI-on-Windows plan
- [ ] add unit tests for the Windows namespace and for each `cfg` pair
- [ ] run gates

### Task 22: Server lifecycle: start on demand, detach per OS, one per namespace

**Files:**
- Create: `crates/agterm-session/src/server/{mod,lifecycle,spawn/{mod,linux,macos,windows}}.rs`,
  `crates/agterm-session/tests/lifecycle.rs`
- Modify: `Cargo.toml` (`wmi`, Windows only, for herdr's WMI spawn; Task 2)

- [ ] port herdr's autodetect and spawn: a client that finds no server starts one and waits for its endpoint; a
      second start under the same namespace finds the first. The server's argv carries its namespace
      (`agterm-session server <socket dir>`), so the harness can tell an instance's server from any other
- [ ] per OS:
  - Linux: `setsid` in `pre_exec`, stdio to null, no fork after threads;
  - macOS: `posix_spawn`, `POSIX_SPAWN_SETSID`, `responsibility_spawnattrs_setdisclaim` (read upstream's
    `Responsibility.swift` and `ZmxLaunch.swift` first);
  - Windows: herdr's `DETACHED_PROCESS` and its in-server check for a kill-on-close job
    (`current_process_is_detached_server_daemon`). agterm adds `CREATE_NEW_PROCESS_GROUP`; inside a kill-on-close
    job it adds `CREATE_BREAKAWAY_FROM_JOB`, and where the job refuses breakaway it starts through herdr's WMI
    `Win32_Process.Create`; when that fails too, the start fails with its reason. Never a plain retry inside the
    job: Task 2's probe shows that server dies with the job. Readiness, then lifetime, is validated
  - macOS also sets `POSIX_SPAWN_CLOEXEC_DEFAULT`, and every PTY master is close-on-exec (Task 2: a stray
    inherited master held an exiting shell in the tty drain)
- [ ] shutdown: the server exits when it holds no session and no client, and never while a session lives
- [ ] tests on every OS (Windows and macOS debugged in the VMs):
  - the launcher exits and the server lives;
  - two racing starts leave one server;
  - the empty server exits;
  - (macOS) the spawn attributes;
  - (Windows) a server launched from inside a job survives the job's close; a job that refuses breakaway still
    gets a server, through WMI; nested jobs; a WMI failure reported; the argv quoting, cwd and environment kept
- [ ] run gates

### Task 23: Pane runtime: PTY, VT state, clients, leadership, snapshot

**Files:**
- Create: `crates/agterm-session/src/server/{pane,pty,clients,lead,snapshot}.rs`,
  `crates/agterm-session/tests/{pane,lead,snapshot}.rs`, `vendor/portable-pty/`, `build/patches/portable-pty/`,
  `scripts/vendor-portable-pty.sh` (Task 2)

- [ ] port herdr's PTY actor over `portable-pty` (with `vendor/portable-pty`'s fixes, compared against crates.io and
      the diff recorded) and the parts of its per-pane runtime (`src/pane.rs`, `src/pane/**`, `src/terminal/`) that
      Task 2 kept, with the session-creation inputs and child environment from Technical Details
- [ ] VT state in `agterm-vt` fed from the PTY; raw output fanned out to attached clients with bounded queues
- [ ] each pane's shell tree in a job object with `KILL_ON_JOB_CLOSE` on Windows; process group on Unix
- [ ] leadership: size ownership, role changes and unmanaged clients, as Technical Details says
- [ ] snapshot on attach, matching what the suite pins (Context)
- [ ] ➕ the attach design of Task 2 > Attach: one serialization point per pane; `SnapshotBegin{epoch, offset}`,
      chunks, `SnapshotEnd`, offset-tagged output; the snapshot carries `agterm-vt`'s replay-safe continuation, with
      its byte cap and unavailable outcome (bounded wait, then a harmless failure); byte-bounded lossless client
      queues, in-band resync with a recovery budget, then disconnect
- [ ] ➕ terminal replies (Task 2 > Attach): the server VT answers state and capability queries always; the
      frontend-dependent ones from the reply owner's metadata (theme colours, cell size, graphics support, with
      stable defaults); OSC 52 reads as a single-recipient request to the reply owner. On Windows, record per
      supported version which queries conhost answers itself and which reach the output pipe
- [ ] ➕ vendor `portable-pty` the way `vendor-gpui.sh` vendors gpui (`vendor/` + `build/patches/portable-pty/` +
      a `--check`): herdr's patches 0003 and 0004, 0001 reduced to kernel32's ConPTY only (no bare `conpty.dll`
      from the search path), and agterm's patch that exports and imports a ConPTY master's handles for Task 25
- [ ] child exit: the session ends and its clients get the exit as `ExitPolicy` expects
- [ ] tests:
  - echo round trip;
  - resize reaching the child;
  - kill ending the child and its tree;
  - two clients with the lead passing between them, and a follower's resize ignored;
  - reattach showing the scrollback, then the screen with the cursor in place;
  - an alt-screen app;
  - color overrides kept;
  - DECSET 2026 not replayed;
  - continuation: a cut inside a UTF-8 sequence, a CSI and an OSC; an overlong sequence; embedded C0 controls;
    recovery after a terminator; original VT + suffix ≡ restored VT + suffix;
  - a slow client resynced in band, keeping its role, and one past the budget disconnected, with the PTY reader
    never blocked;
  - one reply per query with two clients attached and with none
- [ ] run gates

### Task 24: Server API: what `SessionHost` and `clipboard set` need

**Files:**
- Create: `crates/agterm-session/src/server/api.rs`, `crates/agterm-session/tests/api.rs`

- [ ] port herdr's socket API shape (`src/api/`) for exactly the operations in Technical Details > API
- [ ] list with each `LiveSessionRecord` field (`clients`, `leader_pid`, `created_at`, …)
- [ ] kill: one session, a batch, `force`, and every `KillOutcome`, with `StaleSocket` mapped as Technical Details
      says
- [ ] screen (`active`/`all`) from the server's VT with `LiveScreen`'s `revision`, cursor and `alternate`; type
      (encoded for the keyboard mode, without taking the lead); print (an OSC 52 write into the output)
- [ ] tests for each operation's success and error paths: an unknown session, a dead one, a batch with both, and a
      `revision` that moves only when the screen does
- [ ] run gates

### Task 25: Update handoff without ending sessions

**Files:**
- Create: `crates/agterm-session/src/server/handoff/{mod,unix,windows}.rs`, `crates/agterm-session/tests/handoff.rs`

- [ ] port herdr's handoff on Unix (`src/server/handoff.rs`, `src/handoff_runtime.rs`, `src/api/server.rs`): the
      manifest, the `SCM_RIGHTS` batches, the per-pane runtime state, the commit and its timeouts. The VT state passes
      as a snapshot that the new server replays into its `agterm-vt`
- [ ] Windows, per Task 2's conditional GO: duplicate into the new server the ConPTY pipe ends, the pseudoconsole's
      three internal handles, the pane's job and the shell process, behind Task 2's guards (layout gate before any
      dereference, custom ownership of the rebuilt HPCON, never `ClosePseudoConsole` on it, handoff bound to the
      replacement's process handle, prepare/ready/commit, rollback that never touches the shared job or console).
      A failed gate keeps the old server serving and offers the reset
- [ ] after a Unix handoff the new server is not the shell's parent: its exit reports no status (herdr's
      `ChildExitReason::Handoff`); check that `ExitPolicy` and `agterm-term/src/exit.rs` treat it as a plain exit
- [ ] the build id: a hash of the staged binary. A client finding a server of another build starts the handoff,
      and a failed handoff falls back to `build_record.rs`'s reset offer
- [ ] tests on every OS:
  - a handoff keeps the shell, its screen and its scrollback;
  - a client attached before the handoff reattaches after it;
  - a failed handoff leaves the old server serving;
  - (Windows) replacing the staged binary while the server runs succeeds;
  - (Windows) a layout the gate does not know refuses the handoff before any dereference;
  - continuous output across a handoff loses no byte
- [ ] run gates

### Task 26: CLI binary with the kept attach contract

**Files:**
- Create: `crates/agterm-session/src/{bin/agterm-session.rs,cli.rs,attach_tty.rs}`,
  `crates/agterm-session/tests/cli.rs`

- [ ] subcommands: `server <socket dir>` (internal), `attach`, `list`, `kill`, `screen`, `type`, `print`, `version`
- [ ] `attach` honors the attach contract in Technical Details: the `AGTERM_LIVE_*` variables and their
      empty-means-unset reading, `attach <name> [argv…]` create-only with the gone guard, the detach key unless
      `AGTERM_LIVE_NO_DETACH_KEY`, the managed-claim opt-in, the role title
- [ ] `attach` as a terminal client (herdr's terminal attach): raw mode, resize forwarding; on Windows, console VT
      input as herdr's `client/input/windows_vti.rs` reads it
- [ ] ➕ the output path filters out the queries the server answers (Task 2 > Attach), fragment-safe across reads,
      so the outer terminal does not answer them a second time; tested with split sequences and two attached CLIs
- [ ] tests:
  - each subcommand's output and exit codes;
  - `attach` round trip in a PTY, detach and reattach;
  - the gone guard on a vanished session;
  - the role title on a lead change;
  - an inherited `AGTERM_LIVE_SESSION` ignored when set empty
- [ ] run gates

### Task 27: Client library, `SessionHost` and the launch inputs (library only)

**Files:**
- Create: `crates/agterm-session/src/{client,host,launch}.rs`, `crates/agterm-session/tests/host.rs`

- [ ] client library: the API and an attach stream (snapshot, then Output/Input/Resize/Role)
- [ ] `SessionHost` over the library, with the same error kinds and outcomes as `ZmxClient`:
  - the `HostError` texts in live-session wording (today's say `zmx list failed`, `zmx screen answered no screen`,
    …), listing every core or suite assertion that reads them;
  - `KillOutcome`;
  - never a blank screen on a failed read;
  - never an empty namespace read from a failed listing
- [ ] the launch inputs move from `agterm_zmx::Runtime` and `bundled_build_id` into `agterm-session::launch`
      (executable, build id, login shell, Ghostty resources, namespace, the debug override), and `sweep`'s
      `parse_record` moves too. `agterm-zmx` re-exports them until Task 32, so no caller changes yet
- [ ] port `agterm-zmx`'s tests onto the new host and launch module
- [ ] the frontend still builds `ZmxClient`: nothing the app runs changes in this task
- [ ] run gates

### Task 28: Switch live sessions to `agterm-session` (attach subprocess still in use)

**Files:**
- Modify: `crates/agterm-gpui/src/session_host.rs`, `crates/agterm-app/src/restore_launch.rs`,
  `crates/agterm-term/src/launch.rs`, `crates/agterm-core/src/live/{support,lead}.rs`, `crates/agterm-core/src/remote/ssh.rs`,
  `crates/agterm-ctl/src/clipboard.rs`, `conformance/agterm_conformance/live_client.py` (new, for the Rust adapter),
  `scripts/stage.sh`, `crates/agterm-ctl/tests/cli_local.rs`, `crates/agterm-gpui/tests/{privileges,quit}.rs`,
  `crates/agterm-term/tests/launch.rs`, `conformance/agterm_conformance/impls/base.py`,
  `conformance/selftests/test_impls.py`, a selftest for `live_client.py`, `conformance/tests/{cli/test_cli_only,
  cli/test_cli_output,protocol/test_zmx,ui/test_live}.py`, `CLAUDE.md`, `.claude/skills/agterm-upstream-sync/references/safety.md`,
  `conformance/docs/rust-build.md`

- [ ] stage `bin/agterm-session` and `share/agterm/session-BUILD` in place of today's session-host binary and build
      record
- [ ] the attach line (`live/support.rs`, `lead.rs`) and the remote one (`remote/ssh.rs`) switch to the attach
      contract's `AGTERM_LIVE_*` names, role title and socket directory
- [ ] the frontend builds the new host (`session_host.rs`, `restore_launch.rs`), and the GPUI pane runs
      `agterm-session attach` in its PTY, as it ran today's attach subprocess
- [ ] ➕ `agterm-term`'s attach effects policy for live panes: the pane's VT writes no replies to the PTY, set in
      the launch configuration before the first byte (the server answers queries, Task 2 > Attach)
- [ ] `agtermctl clipboard set` finds `agterm-session` beside itself and uses the server's print. Record which of
      its error texts change against upstream's. Update `cli_local.rs`'s fake staging and its asserts
- [ ] the harness, in the Rust adapter's vocabulary only (the Swift adapters keep driving upstream's own binary):
  - leftovers are the instance's server, matched by its namespaced argv, plus its sessions' processes;
  - the socket directory is `/tmp/agterm-live-<hash>`;
  - the leading client is `live_client.py`, which drives `agterm-session attach` with the new names;
  - the environment that `test_cli_only.py` sets for `agtermctl clipboard set` comes from the vocabulary;
  - the shared tests that import `zmx_client.leading_client` or read `instance.zmx_dir` (`test_cli_only.py`,
    `test_cli_output.py`, `test_zmx.py`, `ui/test_live.py`) take both from the adapter;
  - the selftests follow
- [ ] the capability descriptions (`impls/base.py` and the table in `conformance/docs/rust-build.md`, which
      `selftests/test_docs.py` locks together) drop "zmx daemons" and "zmx is staged"
- [ ] the isolation docs follow the new namespace and the one-server reap: CLAUDE.md's isolation rules,
      `.claude/skills/agterm-upstream-sync/references/safety.md`, `conformance/docs/rust-build.md`'s socket-directory
      and leftover contract
- [ ] the gpui live process tests take the cargo-built `agterm-session` instead of the vendored session host, and
      their process-tree check follows one server instead of a daemon per session. They run, not skip.
      `agterm-term/tests/launch.rs`'s live tests run against `agterm-session attach`
- [ ] full GPUI conformance run in the background: every ratchet id green (the live-session ids, restore, relaunch,
      live-any-shell, clipboard); newly green ids added
- [ ] run gates

### Task 29: Foreground job and process table on all three OSes

**Files:**
- Create: `crates/agterm-session/src/job/{mod,linux,macos,windows}.rs`,
  `crates/agterm-app/src/process_table/{macos,windows}.rs`
- Modify: `crates/agterm-app/src/process_table.rs` (it becomes `process_table/linux.rs`), `crates/agterm-app/Cargo.toml`

- [ ] move `agterm-zmx/src/sweep.rs`'s hangup behind the per-OS `job` module: macOS through `sysctl`, Windows through
      herdr's snapshot (descendants, command lines, terminate)
- [ ] `ProcessTable` for macOS and Windows, with the lossy-decode rule the Linux one documents. The quit-time capture
      uses it on every OS, with `parse_record` from `agterm-session`
- [ ] tests on every OS:
  - a foreground job found and ended;
  - an unreadable table never read as "no job";
  - a captured argv with spaces and quotes
- [ ] run gates

### Task 30: Live panes in `agterm-term` over the attach stream

**Files:**
- Modify: `crates/agterm-core/src/live/support.rs` (structured launch output beside the shell line),
  `crates/agterm-term/src/{pane,launch,exit}.rs`, `crates/agterm-term/src/bench.rs`, `crates/agterm-term/Cargo.toml`,
  `crates/agterm-term/tests/launch.rs`
- Create: `crates/agterm-term/src/session_io.rs`

- [ ] core produces the live launch as structured data: session name, argv, environment and cwd. The shell line stays
      for the CLI and remote paths, and tests prove both forms agree
- [ ] the pane asks the server to create its session and attaches with the in-process stream: no PTY, no subprocess,
      the same IO-thread batching and VT slicing as the PTY path
- [ ] a session gone or refused follows the `ExitPolicy` the attach path follows today
- [ ] the pane sends the reply metadata (theme colours, cell size, graphics support) on attach and on change, and
      answers the server's delegated requests (OSC 52 read) as the reply owner; its own VT replies stay suppressed
- [ ] port `tests/launch.rs`'s live tests to the in-process attach: `every_fresh_live_launch_paints_its_prompt` (50
      sequential and the parallel fresh launches, the guard of the first-paint race), the session-gone and
      cannot-reach-socket cases
- [ ] add tests:
  - a live pane round trip and resize;
  - pane drop detaches and leaves the session running;
  - reattach shows the snapshot;
  - the session's environment and cwd equal what `attach` gave it
- [ ] `bench.rs` gains the in-process live path; measure against Task 2's baselines and record the numbers here
- [ ] run gates and the full GPUI conformance run in the background, with the gpui live process tests running, not
      skipping

### Task 31: PowerShell live sessions

**Files:**
- Modify: `crates/agterm-core/src/live/{shell_integration,replay}.rs`, `crates/agterm-core/src/restore/mode.rs`
- Create: `resources/shell-integration/pwsh/agterm.ps1` (path settled against the staged Ghostty resources)

- [ ] `ShellKind::Pwsh`: the script (OSC 7, OSC 133, title), launch arguments, the replay script in pwsh
- [ ] eligibility on Windows: `pwsh.exe`, then `powershell.exe`, else the existing rejection with its reason
- [ ] tests:
  - script rendering and quoting;
  - replay of an argv with spaces and quotes;
  - eligibility cases;
  - e2e on the Windows runner (debugged in the VM): a live pwsh session reports its cwd, and its foreground command
    is captured through Task 29's table and replayed in a recreated session
- [ ] run gates

### Task 32: Remove zmx, and with it the last Zig

**Files:**
- Delete: `crates/agterm-zmx`, `scripts/build-zmx.sh`, `build/patches/zmx/`
- Modify: `Cargo.toml` (the workspace dependency), `Cargo.lock`, `crates/{agterm-app,agterm-gpui,agterm-term}/Cargo.toml`,
  callers of `agterm-zmx`'s re-exports (`agterm-term/src/launch.rs`, `agterm-app/src/{restore_launch,process_table}.rs`,
  `agterm-gpui/src/session_host.rs`), `crates/agterm-core/src/live/lifecycle.rs` (`parse_list` loses its caller),
  `build/pins.env` (drop `ZMX_*`), `scripts/lib/native.sh` (`find_zig`, `host_zig_arch`, the zmx prefix),
  `.github/workflows/rust-linux.yml` (Zig install and cache), `crates/agterm-term/tests/{launch,prefix_keys}.rs`,
  `crates/agterm-term/src/bench.rs` (the zmx scenario), `crates/agterm-wpe-sys/tests/provider.rs` (`ZMX_REV`),
  `.claude/skills/agterm-upstream-sync/**`

- [ ] point every caller at `agterm-session` directly and delete the crate, the build script, the pins and patches,
      and every Zig helper and CI step. Nothing in the repo downloads or runs Zig anymore
- [ ] keep upstream's names only for the Swift adapters (`LEFTOVER_NAMES`, `zmx_client.py`)
- [ ] scripts and docs: `run-gpui-dev.sh`, the isolation rules and the harness keep scrubbing the user's installed
      agterm's `ZMX_*` from the environment, since that app still sets them. Nothing else in the Rust build names zmx
- [ ] upstream-sync skill: an upstream zmx change maps onto `live.*` and `agterm-session`'s attach contract, a herdr
      change
      means "port into `agterm-session`", and a Ghostty bump means "port into `agterm-vt`"
- [ ] tests: the gpui live process tests and `agterm-term`'s live tests run, not skip; a fresh clone stages and
      passes the tests with no `zig` on PATH
- [ ] run gates and the full conformance run

### Task 33: Verify acceptance criteria

- [ ] no Zig in the repo: the scripts, `build/pins.env`, CI. `agterm-vt` is the only VT core, and no
      `agterm-ghostty-vt*` or `agterm-zmx` remains
- [ ] `git grep -i zmx` finds only the allowed places:
  - the Swift adapters, their vocabulary and `zmx_client.py`;
  - the suite's shared tests, `catalog.yaml`, the `covers(...)` markers and `selftests/fixtures/upstream-mini/**`
    (upstream's vocabulary);
  - the upstream-sync tooling's upstream-path patterns;
  - the bundled skill's patch (its removed lines);
  - the scrub of the installed agterm's environment;
  - the deviation record;
  - docs or plans that describe upstream or the past
- [ ] every drift file from Task 1 is covered, and the identity strings match the pin's
- [ ] live sessions on Linux, macOS and Windows CI: create, detach by quitting the app, reattach with the screen,
      kill, prune, reset, `clipboard set`, an update handoff
- [ ] the remote paths green against `agterm-session attach`: `agterm-core`'s `commands/tests/{attach_remote,remote}.rs`
      and `remote/{ssh,tree}.rs` tests
- [ ] performance against the recorded numbers: VT within 10% (Tasks 16–17); the 50 MiB `cat` no slower than
      through zmx, and the server's idle RSS for N panes no larger than N idle zmx daemons (Task 2's baselines)
- [ ] every gate in README > Test, including both generator `--check`s; the full `conformance/tools/run-rust-linux.sh`
      in the background, with the ratchet unchanged or grown; `cross-platform.yml` and `rust-linux.yml` green

### Task 34: [Final] Update documentation

- [ ] README:
  - native dependencies without Zig (the Ghostty resources only);
  - the generators: `gen-vt-unicode.sh` and `gen-ghostty-terminfo.sh` with their `--check`; no bindings generator;
  - staging with `agterm-session`, the server's lifecycle, and Windows notes;
  - Test (the generator checks) and CI (`cross-platform.yml`)
- [ ] CLAUDE.md, as rules, not details:
  - crate map: remove `agterm-ghostty-vt-sys`, `agterm-ghostty-vt` and `agterm-zmx`; add `agterm-vt` and
    `agterm-session` as `any`; update the paragraph on the libghostty-vt crates' platform;
  - reference rule: Ghostty's sources at the pin are the oracle for the terminal, in place of "libghostty-vt's C
    API". rustty becomes the source the VT core was taken from, and herdr the source of the session server. zmx
    appears only in the rule that scrubs the installed agterm's environment;
  - isolation rules: the session server per namespace (never reap by name), the Windows pipe prefix, debugging in
    the VMs
- [ ] `conformance/docs/rust-build.md` and `conformance/README.md`: libghostty-vt and zmx mentions, the harness's Rust
      adapter, and any capability claimed for pwsh live sessions
- [ ] move this plan to `docs/plans/completed/`

## Post-Completion

*Items requiring manual intervention or external systems - no checkboxes, informational only*

**Manual verification**:
- Linux, in a dev instance through `scripts/run-gpui-dev.sh` with a private state dir:
  - heavy TUIs: nvim, htop, lazygit;
  - Kitty images (`kitten icat`, relative placements);
  - scrollback search;
  - selection by click, double, triple and drag with autoscroll;
  - the OSC 52 and 5522 clipboard;
  - program-status and progress reports;
  - live panes across an app quit and relaunch, and across an update;
  - feel and throughput against the installed build on a large `cat`.
- The `Win11-admin` VM:
  - live pwsh panes survive closing the app;
  - a reboot replays captured commands;
  - an update with sessions running (handoff, or the documented fallback);
  - a terminal attaches with `agterm-session attach`.
- The `macOS 26` VM (or a real Mac where the VM cannot grant devices): a tool in a live session keeps its microphone
  and camera permission across an app update.
- The user's own machine: the installed agterm's zmx sessions are not migrated. They keep running until closed or
  reset.
- The user-level `agterm` skill (outside this repository) names `agtermctl zmx …`. For the Rust build it needs the
  `agtermctl live …` names, while the installed upstream-style agterm keeps zmx.

**External and future**:
- The GPUI frontend on Windows and macOS is its own plan. This one gives it a working VT core and session server on
  both.
- The future TUI frontend ports herdr's surface deltas as a second attach mode. The future headless core builds on
  the session server, `agterm-session`'s client library and its attach stream.
- A Ghostty pin bump ports the diff of the followed directories into `agterm-vt` (rustty's matching commits are the
  first place to look), and reruns `gen-vt-unicode.sh` and `gen-ghostty-terminfo.sh`. A herdr bump ports its
  session-server changes into `agterm-session`.
