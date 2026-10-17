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
  - On attach, the server sends a snapshot of the pane, then its raw output, which is herdr's `AttachTerminal` mode.
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

- herdr (`https://github.com/herdrdev/herdr`, Apache-2.0, v0.9.5; a local clone at `~/develop/projects/temp/herdr`,
  `143bf5f`, 2026-10-16):
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
    uses, and herdr's async runtime if Task 2 keeps it (`tokio`);
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
  protocol   herdr's wire and endpoint (Unix socket │ named pipe, owner-only), version-negotiated
  api        list / new / kill (batch, force) / screen / type / print
  attach     snapshot, then the raw output stream; input and resize back (herdr's AttachTerminal)
  handoff    update without ending sessions: Unix by herdr's SCM_RIGHTS, Windows per Task 2's design
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
  - On Windows, the directory lives under `%LOCALAPPDATA%` (Task 2 fixes it), and the endpoint is a named pipe
    carrying the same hash.
- **In-process attach.** `agterm-term` opens the session's stream, claims or follows the lead as `agterm-core`'s
  lead policy says, sends its grid size, and from then on maps:
  - snapshot and Output → `vt_write`;
  - `Pane::write` → Input;
  - `Pane::resize` → Resize;
  - Role changes → the lead state core already tracks.

  Plain panes keep the PTY path.
- **Platforms.**
  - **Linux**: spawn with `setsid` in `pre_exec`, PTY via `openpty`.
  - **macOS**: `posix_spawn` with `POSIX_SPAWN_SETSID` and `responsibility_spawnattrs_setdisclaim`, so live sessions
    keep their own permission identity across app updates, as upstream's session host does.
  - **Windows**: ConPTY. The server detaches with herdr's `DETACHED_PROCESS` (plus `CREATE_NEW_PROCESS_GROUP`, and
    `CREATE_BREAKAWAY_FROM_JOB` where the launcher's job allows it, agterm's additions). Each pane's shell tree runs in
    a job object with `KILL_ON_JOB_CLOSE`, so killing a session ends its processes, as SIGHUP does on
    Unix.
- **Updates.**
  - The build id is a hash of the staged `agterm-session` binary, written to `share/agterm/session-BUILD`.
  - When the app finds a server of another build, it asks it to hand off to the new binary: herdr's `SCM_RIGHTS`
    handoff on Unix, Task 2's design on Windows.
  - A handoff that fails falls back to `build_record.rs`'s existing "predates the last update" path, which offers the
    usual reset and kills nothing behind the user's back.

## Technical Details

### `agterm-vt`

- **Crate**: `crates/agterm-vt`.
  - Targets: the lib, plus two feature-gated generator bins (`gen-unicode`, `gen-terminfo`) that the generator
    scripts run.
  - Under the workspace lints.
  - Dependencies: `bitflags`, `tracing`, `png`, `miniz_oxide`, `libc` and `windows-sys` for the page allocator's
    per-OS forms (`mmap`, `VirtualAlloc`), `hashbrown` and `memchr` only if needed.
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

### `agterm-session`

- **Source and license.** herdr is pinned in `build/pins.env` (`HERDR_REPO`, `HERDR_REV`) for provenance and future
  syncs.
  - herdr's files carry no license header, and it ships no NOTICE. So each derived file gets a header that names its
    herdr source file, says it is licensed under Apache-2.0, and says it was modified (Apache-2.0 §4).
  - herdr's `LICENSE` goes into the root `NOTICE`, and `agterm-session` declares `MIT AND Apache-2.0`.
  - herdr's own libghostty-vt binding is not ported.
- **What Task 2 settles from herdr's code** (and writes here before Task 20):
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

- [ ] create the crate under the workspace lints. Its crate doc states the provenance, the module-header convention
      and the unsafe policy
- [ ] write the root `NOTICE` with Ghostty's and rustty's MIT texts; uucode, herdr and (if needed) wuffs are added by
      their tasks. Check that every crate inherits `license.workspace` (MIT, user 2026-10-16), except
      `agterm-session`, which declares `MIT AND Apache-2.0` for herdr's code
- [ ] add `RUSTTY_REPO`/`RUSTTY_REV` to `build/pins.env`, with a comment saying what the pin is for
- [ ] inventory, recorded in this plan:
  - the keep or cut verdict for every rustty-vt module named in Context, by the "part of libghostty-vt at the pin and
    reachable from agterm" rule;
  - the module graph (`use crate::`) of the kept set, its strongly connected groups, and any correction to Tasks
    4–15's boundaries;
  - the public functions of `agterm-ghostty-vt` that `agterm-term` calls, each mapped to the rustty item or
    `rustty-vt-c` logic that implements it, and to its task;
  - the drift files of every followed Ghostty directory (Technical Details), each assigned to a task
- [ ] calibrate: copy the kept files (`.rs` only, no build scripts, no `-sys` crates) into a scratch crate after
      reading them, and run clippy with the workspace lints and `clippy.toml` in a sandbox (no network, scratch HOME).
      Record the counts per lint and per module here
- [ ] write a smoke test and a doc test of the provenance constants
- [ ] run gates - must pass before task 2

### Task 2: Study herdr's session server and spike its risky parts

**Files:**
- Create: `docs/spikes/20261018-session-server.md`
- Modify: this plan (Part B details, ➕ tasks), `build/pins.env` (`HERDR_*`), `NOTICE` (herdr's Apache-2.0 section)

- [ ] pin herdr (`HERDR_REPO`, `HERDR_REV`, the current `main`) and read its server at that commit: `src/server`,
      `src/protocol`, `src/ipc.rs`, `src/client`, `src/session.rs`, `src/pane.rs` with `src/pane/**` (the per-pane
      runtime), `src/terminal`, `src/pty`, `src/server/handoff.rs` with `src/handoff_runtime.rs` and
      `src/api/server.rs`, `src/platform`, `src/api`, `src/cli`, `vendor/portable-pty`, and `src/persist` and
      `src/remote` only to record gaps
- [ ] write the findings into Technical Details > `agterm-session` > "What Task 2 settles": the module map with
      verdicts, the wire, the runtime, any gaps. Adjust Tasks 20–32 and add ➕ tasks where the code asks for
      it
- [ ] probe the Windows handoff in the `Win11-admin` VM (and on `windows-latest`), with a throwaway program, not
      merged: a detached server holds a ConPTY shell, and a second server process takes it over (duplicated handles)
      while the shell keeps running. Record go or no-go
- [ ] probe on macOS, in the `macOS 26` VM and on a runner: the server spawned with `posix_spawn` + setsid + disclaim
      survives its launcher; a handoff over `SCM_RIGHTS` keeps the shell
- [ ] decide the Windows namespace directory
- [ ] record baselines on the dev box: `agterm-term`'s `bench.rs` (plain and through zmx), a 50 MiB `cat` through a
      live zmx pane in the GPUI build, the idle RSS of zmx daemons for N panes
- [ ] add herdr's section to `NOTICE`
- [ ] write the spike note. A no-go that blocks Part B stops it with a ⚠️ and a codex-discussion; Part A goes on

### Task 3: Cross-platform CI for `agterm-vt`

**Files:**
- Create: `.github/workflows/cross-platform.yml`

- [ ] add a workflow with an `ubuntu`/`macos`/`windows` matrix: fmt, clippy `-D warnings` (all features) and tests
      of `agterm-vt`. Later tasks add their crates and generator checks to it
- [ ] add a Miri job on a nightly pinned by date, over the memory core's tests. It is filled from Task 4
- [ ] push a branch and get it green on all three
- [ ] run gates - must pass before task 4

### Task 4: VT memory core: page allocator, offset buffers, bitmap allocator, pools, lists

**Files:**
- Create: `crates/agterm-vt/src/mem/*.rs`, `crates/agterm-vt/src/datastruct/*.rs`, `crates/agterm-vt/src/os/*.rs`

- [ ] port the page allocator from rustty-os, with its Linux, macOS (`mmap`) and Windows (`VirtualAlloc`) forms, all
      in this change
- [ ] port the following behind safe types, with every `unsafe` block carrying its own `// SAFETY:` and one
      operation:
  - from rustty-vt: `alloc`, `mem`, `bitmap_allocator`, `ref_counted_set`, `hash_map`, `zig_list`;
  - from rustty-render-types: `OffsetBuf` and what it needs;
  - from rustty-datastruct and rustty-util: what those need
- [ ] replace the zigstd and allocator-api2 shims with `std` where the semantics allow. Note each shim that remains,
      with its reason
- [ ] port their rustty tests
- [ ] add tests for the safe wrappers' error paths: exhaustion, out-of-range offsets refused, a double free
      impossible by type
- [ ] Miri green locally and in Task 3's job; the macOS and Windows forms debugged in the VMs and green on CI
- [ ] run gates - must pass before task 5

### Task 5: Unicode lookups, UTF-8 decoder, scalar SIMD paths

**Files:**
- Create: `crates/agterm-vt/src/unicode/**`, `crates/agterm-vt/src/simd/**`

- [ ] port the props, grapheme-break, case-folding and symbol lookups with rustty's tables, as rustty dumped them
      (`12752b2ac`). Task 6 regenerates them at the pin
- [ ] port the UTF-8 decoder and the scalar forms of rustty-simd that the stream uses (ESC search, codepoint width,
      base64)
- [ ] port their tests, including the FNV checksum test
- [ ] run gates - must pass before task 6

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
- [ ] port the XTGETTCAP map built from the same entry (used by Task 11's handler)
- [ ] `scripts/gen-ghostty-terminfo.sh [--check]` writes the checked-in source; add `--check` to CI
- [ ] capture today's entry from the Zig-built prefix once, as a fixture (`infocmp -x -A <prefix>/share/terminfo
      xterm-ghostty`), so the check outlives Task 18. Test that the checked-in source, compiled with
      `tic -x -o <tempdir>`, gives the same `infocmp` output
- [ ] add XTGETTCAP tests: a known capability, an unknown one, hex-encoded names
- [ ] run gates - must pass before task 8

### Task 8: Page, PageList, style, hyperlink, resize and reflow (without page compression)

**Files:**
- Create: `crates/agterm-vt/src/terminal/page*.rs`, `crates/agterm-vt/src/terminal/page_list/**`,
  `crates/agterm-vt/src/terminal/{style,hyperlink,point,size,color,x11_color}.rs`

- [ ] port `page_impl`/`page` (cells, rows, grapheme map, style set, hyperlink set, string storage) over Task 4's
      core, safe above it
- [ ] port `pagelist`: scrollback with its limits, pins, iteration, erase, scroll, resize with reflow, page reuse.
      Split whatever goes over the limits
- [ ] cut idle compression out of `PageList` (`NodeData::Compressed`, `compress`), and keep everything else that
      touches node data
- [ ] absorb this group's drift
- [ ] port the rustty tests (`pagelist/tests/*` minus compression, `page_tests`, `ref_geometry_golden_tests`)
- [ ] add tests for the drift and for the compression cut (no compressed node is ever produced; scrollback limits
      still hold)
- [ ] run gates - must pass before task 9

### Task 9: Parser and stream: parse table, CSI/DCS/OSC/APC parsing, SGR, charsets, modes

**Files:**
- Create: `crates/agterm-vt/src/terminal/{parser,parse_table,stream,stream_continuation,csi,dcs,sgr,charsets,modes,
  ansi,device_attributes,device_status,size_report,focus,apc}.rs`, `crates/agterm-vt/src/terminal/osc/**`,
  `crates/agterm-vt/src/terminal/kitty/graphics_command.rs`

- [ ] port the parser state machine and the stream dispatch, generic over its handler, without the tmux and
      glyph-protocol branches
- [ ] port every OSC parser agterm reaches today (title, pwd, hyperlinks, colors, clipboard, notifications, progress,
      `program_status`, `context_signal`, semantic prompt, Kitty's 5522 clipboard) and the Kitty graphics command
      parser
- [ ] absorb the drift for `stream.zig`, `osc/parsers/*`, `modes.zig`, `csi.zig`, `device_status.zig` and
      `size_report.zig`
- [ ] port the stream, OSC, parser, SGR, modes and graphics-command tests
- [ ] add tests for each drifted parser
- [ ] run gates - must pass before task 10

### Task 10: Screen with its formatter, selection and Kitty storage (one dependency group)

**Files:**
- Create: `crates/agterm-vt/src/terminal/{screen,screen_set,tabstops,cursor,selection,selection_codepoints,
  highlight}.rs`, `crates/agterm-vt/src/terminal/screen/**`, `crates/agterm-vt/src/terminal/formatter/**`,
  `crates/agterm-vt/src/terminal/kitty/{graphics_storage,graphics_image,graphics_unicode}.rs` (names as Task 1's
  graph settles them)

- [ ] port `Screen`: primary and alternate, cursor, saved cursor, selection storage, semantic prompt marks, the Kitty
      image storage it owns
- [ ] port the formatter (plain, VT, HTML; viewport, screen, selection, ranges; the snapshot's extras) with the drift
      for `formatter.zig` and `Screen.zig`
- [ ] check zmx's `ghostty/0001-formatter-unstyled-blanks` against the pin. If the pin lacks it, add it as a
      formatter option, with tests
- [ ] port the selection operations (cell ranges, word, line, all, adjust)
- [ ] Kitty images: loading (direct, file, temp file and shared memory, with the POSIX and Windows forms of what it
      reads), zlib through `miniz_oxide`, PNG through `png`, the swizzles in Rust, the drift for
      `kitty/graphics_image.zig`
- [ ] Kitty storage: the placement-id namespace, a relative placement's parent key and H=/V= offsets
- [ ] port the screen, formatter, selection and Kitty storage and image tests
- [ ] add tests for word boundaries, wide characters, soft wraps in plain output and HTML escaping
- [ ] add tests for each swizzle on known pixels, compressed payloads (good and corrupt), PNG (good and truncated),
      and the exposed parent and namespace fields
- [ ] run gates - must pass before task 11

### Task 11: Terminal with its stream handler, effects and Kitty execution (one dependency group)

**Files:**
- Create: `crates/agterm-vt/src/terminal/terminal.rs`, `crates/agterm-vt/src/terminal/terminal/**`,
  `crates/agterm-vt/src/terminal/stream_terminal/**`, `crates/agterm-vt/src/terminal/{effects,clipboard}.rs`,
  `crates/agterm-vt/src/terminal/kitty/graphics_exec.rs` (plus what Task 1 keeps: DnD, grants, animation)

- [ ] port `Terminal`: print, wrap, erase, insert/delete, margins, scroll regions, resize, viewport scrolling (by
      prompt too), with the drift for `Terminal.zig`
- [ ] port `stream_terminal`'s handler without its snapshot and glyph branches, with XTGETTCAP from Task 7. Absorb
      the drift for `stream_terminal.zig` and `kitty/clipboard_write.zig`
- [ ] define the effects trait with the C API's callback set, called synchronously with a read view of the terminal,
      and a no-op default
- [ ] port Kitty command execution and the kept Kitty modules
- [ ] restore Ghostty's identity wherever rustty renamed it
- [ ] port the terminal, `stream_terminal/tests/*` and Kitty execution tests
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

### Task 14: Input encoders and paste

**Files:**
- Create: `crates/agterm-vt/src/input/{key,key_encode,key_mods,mouse,mouse_encode,paste,focus}.rs`

- [ ] port the key encoder (legacy, application modes, modifyOtherKeys, Kitty flags), the mouse encoder (all
      tracking modes and formats) and focus reports
- [ ] port paste framing and safety, with the drift for `input/paste.zig` and `terminal/paste.zig`
- [ ] port the encoder and paste tests
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
  - `reset`, `set_mode_default`, the Kitty media and storage-limit settings, `version`
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

- [ ] wire: rename the group `zmx.*` → `live.*` (`live.list`, `prune`, `kill`, `reset`, `tree`, `attach`, `present`,
      `screen`; `Group::Live`), the result key `zmx` → `live`, the tree key `backedByZmx` → `backedByLive`
- [ ] `agtermctl zmx …` → `agtermctl live …`, with help, usage and output texts, and the far side's command lines in
      `remote/ssh.rs`, `remote/tree.rs` and `agterm-app/src/remote.rs`, with their tests (or remote attach breaks
      while its unit tests stay green). A new build then can't view an older build's sessions either; record that
- [ ] rename `agterm-core::zmx` → `agterm-core::live` with its commands, types and constants (`ZmxCmd`,
      `ControlZmxInventory`, `ZmxRuntime`, `UNAVAILABLE`, …), and `AGTERM_UITEST_ENABLE_ZMX` →
      `AGTERM_UITEST_ENABLE_LIVE`. Every error string that names zmx gets the live-session vocabulary (`"live.kill
      requires --force"`, `"this pane has no live session…"`), and each changed upstream string is listed in this task
- [ ] what the zmx binary itself speaks stays until Task 28 (and its parsers until Task 32), untouched by the sweep:
  - the `attach` subcommand and its variables (`ZMX_DIR`, `ZMX_SESSION*`, `ZMX_MANAGED*`, `ZMX_NO_DETACH_KEY`,
    `ZMX_TRACK_ENV`);
  - the socket directory;
  - the role-title prefix `zmx-role;` that the patched zmx writes and `lead.rs` parses;
  - the `list` and `screen` output formats (`lifecycle::parse_list`, `screen::parse_screen`);
  - `bin/zmx`, `share/agterm/zmx-BUILD`, `AGTERM_ZMX_PATH`;
  - the `agterm-zmx` crate, beyond its imports
- [ ] the suite keeps upstream's vocabulary in its tests, catalog and `covers(...)` markers, because they are shared
      with the Swift references:
  - each adapter exposes a vocabulary object: command names, result and tree keys, CLI argv, error-text templates,
    environment names, the socket-directory attribute, and the leading client;
  - `base.py`, `cli_format.py` and the shared tests read every vocabulary-dependent value from it;
  - the Swift adapters' vocabulary is upstream's, and the Rust adapter's is `live`;
  - no test id changes, so the ratchet, `catalog.yaml` and the catalog gate need no edit
- [ ] the bundled `agterm` skill gets the live names through a patch in `build/patches/skill-agterm/` that
      `sync-resources.sh` reapplies after every upstream sync, with a `--check`
- [ ] record the deviation from upstream (the names, the changed strings, the vocabulary objects, the skill patch) in
      `conformance/docs/rust-build.md`, and in the upstream-sync skill's areas reference: an upstream `zmx.*` change
      ports onto `live.*`
- [ ] tests:
  - the dispatcher's tests on the new names with full error strings;
  - `agtermctl`'s parse and output tests;
  - `agterm-server`'s and the gpui process tests on the new names;
  - the remote command-line tests;
  - harness selftests for each adapter's vocabulary
- [ ] full conformance run in the background: every ratchet id green, ids unchanged
- [ ] run gates

### Task 20: `agterm-session` crate: wire and endpoint

**Files:**
- Create: `crates/agterm-session/Cargo.toml` (license `MIT AND Apache-2.0`), `crates/agterm-session/src/{lib,protocol,endpoint}.rs`,
  `crates/agterm-session/tests/{protocol,endpoint}.rs`
- Modify: `Cargo.toml` (`interprocess`, `windows-sys`, and the runtime Task 2 chose), `.github/workflows/cross-platform.yml`

- [ ] port herdr's wire (framing, encoding, version negotiation) with the message families Task 2 kept: hello, the
      api requests and replies, attach (snapshot, output, input, resize, role), handoff control
- [ ] port herdr's endpoint (`src/ipc.rs`, `src/server/socket_paths.rs`) onto agterm's namespace: Unix sockets under
      the instance's socket directory; on Windows, named pipes with herdr's owner-only SDDL, plus
      `FILE_FLAG_FIRST_PIPE_INSTANCE` (agterm's addition)
- [ ] give every herdr-derived file the header from Development Approach > Provenance
- [ ] add the crate to `cross-platform.yml`
- [ ] tests:
  - each message round-trips;
  - version mismatch refused with its message;
  - a truncated frame and an oversized length refused;
  - an over-long Unix socket path refused;
  - a second pipe instance under a taken name refused
- [ ] run gates

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

- [ ] port herdr's autodetect and spawn: a client that finds no server starts one and waits for its endpoint; a
      second start under the same namespace finds the first. The server's argv carries its namespace
      (`agterm-session server <socket dir>`), so the harness can tell an instance's server from any other
- [ ] per OS:
  - Linux: `setsid` in `pre_exec`, stdio to null, no fork after threads;
  - macOS: `posix_spawn`, `POSIX_SPAWN_SETSID`, `responsibility_spawnattrs_setdisclaim` (read upstream's
    `Responsibility.swift` and `ZmxLaunch.swift` first);
  - Windows: herdr's `DETACHED_PROCESS` and its in-server check for a kill-on-close job
    (`current_process_is_detached_server_daemon`). agterm adds `CREATE_NEW_PROCESS_GROUP` and tries
    `CREATE_BREAKAWAY_FROM_JOB`, falling back without it when the launcher's job refuses breakaway
- [ ] shutdown: the server exits when it holds no session and no client, and never while a session lives
- [ ] tests on every OS (Windows and macOS debugged in the VMs):
  - the launcher exits and the server lives;
  - two racing starts leave one server;
  - the empty server exits;
  - (macOS) the spawn attributes;
  - (Windows) a server launched from inside a job survives the job's close, and a job that refuses breakaway still
    gets a server
- [ ] run gates

### Task 23: Pane runtime: PTY, VT state, clients, leadership, snapshot

**Files:**
- Create: `crates/agterm-session/src/server/{pane,pty,clients,lead,snapshot}.rs`,
  `crates/agterm-session/tests/{pane,lead,snapshot}.rs`

- [ ] port herdr's PTY actor over `portable-pty` (with `vendor/portable-pty`'s fixes, compared against crates.io and
      the diff recorded) and the parts of its per-pane runtime (`src/pane.rs`, `src/pane/**`, `src/terminal/`) that
      Task 2 kept, with the session-creation inputs and child environment from Technical Details
- [ ] VT state in `agterm-vt` fed from the PTY; raw output fanned out to attached clients with bounded queues
- [ ] each pane's shell tree in a job object with `KILL_ON_JOB_CLOSE` on Windows; process group on Unix
- [ ] leadership: size ownership, role changes and unmanaged clients, as Technical Details says
- [ ] snapshot on attach, matching what the suite pins (Context)
- [ ] child exit: the session ends and its clients get the exit as `ExitPolicy` expects
- [ ] tests:
  - echo round trip;
  - resize reaching the child;
  - kill ending the child and its tree;
  - two clients with the lead passing between them, and a follower's resize ignored;
  - reattach showing the scrollback, then the screen with the cursor in place;
  - an alt-screen app;
  - color overrides kept;
  - DECSET 2026 not replayed
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
- [ ] Windows, per Task 2's verdict: the handoff Task 2 proved, or the documented fallback (sessions end on update,
      and the update path offers the reset)
- [ ] the build id: a hash of the staged binary. A client finding a server of another build starts the handoff,
      and a failed handoff falls back to `build_record.rs`'s reset offer
- [ ] tests on every OS:
  - a handoff keeps the shell, its screen and its scrollback;
  - a client attached before the handoff reattaches after it;
  - a failed handoff leaves the old server serving;
  - (Windows) replacing the staged binary while the server runs succeeds
- [ ] run gates

### Task 26: CLI binary with the kept attach contract

**Files:**
- Create: `crates/agterm-session/src/{bin/agterm-session.rs,cli.rs,attach_tty.rs}`,
  `crates/agterm-session/tests/cli.rs`

- [ ] subcommands: `server <socket dir>` (internal), `attach`, `list`, `kill`, `screen`, `type`, `print`, `version`
- [ ] `attach` honors the attach contract in Technical Details: the `AGTERM_LIVE_*` variables and their
      empty-means-unset reading, `attach <name> [argv…]` create-only with the gone guard, the detach key unless
      `AGTERM_LIVE_NO_DETACH_KEY`, the managed-claim opt-in, the role title
- [ ] `attach` as a terminal client (herdr's terminal attach): raw mode, resize forwarding
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
