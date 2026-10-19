# Cross-platform sessions: libghostty-vt on all three OSes and a Rust session server in place of zmx

## Overview

- **Goal**: live sessions and the terminal core on Windows, Linux and macOS alike, without zmx.
  - **Part A, the VT core**: keep libghostty-vt (Ghostty's VT library behind its C API) and build it with Zig for
    every cargo target, as herdr does (`.upstream/herdr/crates/ghostty-vt/build.rs`). `agterm-ghostty-vt-sys` and
    `agterm-ghostty-vt` become `any`, and so does `agterm-term`, the pane core above them.
  - **Part B, the session server**: `agterm-session` on herdr's architecture (`https://github.com/herdrdev/herdr`,
    Apache-2.0), taken as code: one background server per agterm instance owns every live pane's PTY and VT state.
    It replaces the zmx binary and the `agterm-zmx` crate. Its VT state is libghostty-vt, as herdr's is.
  - Zig stays, for libghostty-vt alone. zmx, its build and its Zig pins go.
- **Why** (user, 2026-10-18): this replaces `20261018-zig-free-core`, paused after its Tasks 1–8 and 19–22.
  - libghostty-vt already builds for Windows and macOS (herdr ships it there); only agterm's build script was
    Linux-only. Porting the terminal to Rust (rustty's port, about 264k lines to bring under the workspace lints)
    is not needed for the platforms, so it stops.
  - That port's code (`crates/agterm-vt`, Tasks 4–8) lives on in the branch `rust-vt-port`; it leaves this branch
    (Task 7). The old plan and its records stay there too: `docs/plans/20261018-zig-free-core.md` and
    `docs/zig-free-core-paused.md` on `rust-vt-port` and `origin/zig-free-core`.
  - Work continues on the branch `cross-platform-sessions`, cut from `zig-free-core` at `e67b0ca`, so the done Part B
    tasks and the cross-platform CI carry over (Tasks 1–6 below, done).
- **Platforms** (user, 2026-10-16): Windows, Linux and macOS are equal targets. Neither the VT core nor the
  live-session layer is forked per platform. The GPUI frontend on Windows and macOS is the next plan; this one gives
  it a terminal core, a pane core and a session server on both.

### Part A decisions (user, 2026-10-18)

- **libghostty-vt stays the terminal.** Its C API is the oracle as today (CLAUDE.md > Reference rule); the bindings
  stay checked-in bindgen output (`scripts/gen-ghostty-vt-bindings.sh`), and the patch `build/patches/ghostty-vt/0001`
  stays.
- **Logic patches are allowed when recorded** (user, 2026-10-18), an exception to "a Zig patch only exposes stored
  data" for parity and fixes Rust cannot reach: upstream's unstyled-blanks formatter patch and herdr's libghostty-vt
  fixes (0004 hosted Wuffs on Windows MSVC, 0008 cursor hyperlink across page replacement, the rest after review).
  Each sits in `build/patches/ghostty-vt/` with its reason, upstream status and a remove-when condition, in the
  style of herdr's `vendor/libghostty-vt.patches.md`. A gap Rust can fill is still filled in Rust.
- **The library is built from the sys crate's `build.rs`, like herdr's** (user's choice of the three build options):
  - `scripts/build-ghostty-vt.sh` keeps what needs the network or the shell: the pinned source archive and theme
    package with their sha256s, the patches, the Zig packages the build would fetch (prefetched into the prefix), the
    staged resources (shell integration, themes, terminfo). It writes a patched source tree into its keyed prefix and
    builds no library;
  - `build.rs` runs `zig build -Demit-lib-vt` over that tree for the cargo `TARGET` (herdr's target map), with its
    local cache and output under `OUT_DIR`, and links the static archive. No network in `build.rs`;
  - it works with plain cargo on all three OSes, and cross-targets follow cargo's.
- **Zig is pinned per OS and architecture** in `build/pins.env`, sha256-checked as today, found or fetched by the
  script (`scripts/lib/native.sh` `find_zig`).

### Part B decisions (user, 2026-10-18; carried from the paused plan)

- **herdr's architecture, whole.** Live sessions follow herdr's design, taken as code with herdr's notices:
  - one background session server per agterm instance (its namespace is keyed by the state dir). It is started on
    demand, detached from the app, and it owns every live pane's PTY and VT state;
  - clients connect to it, detach and reattach, and the GPUI app is one of them;
  - the server's control socket API, its update handoff (the PTYs pass to the new server without killing a session),
    its spawn and detach per OS, its process inspection;
  - herdr's PTY actor over `portable-pty` (ConPTY on Windows) with its fixes in `vendor/portable-pty`, and IPC over
    `interprocess` (Unix socket and named pipe, owner-only DACL).
- **The GPUI pane receives the raw stream and keeps its own VT** (user, 2026-10-18).
  - On attach, the server sends a snapshot of the pane, then its raw output (agterm's raw mode on herdr's connection
    framework; herdr's `AttachTerminal` renders server-side). `agterm-term` feeds both into its own libghostty-vt
    terminal, as it feeds a PTY today.
  - Rendering, selection, search and Kitty graphics stay in `agterm-term` and `agterm-gpui`.
  - The server's VT exists for the snapshot, screen reads, typing and a future headless core.
  - herdr's surface deltas (server-side rendering for TUI clients) are not ported. The future TUI-frontend plan adds
    them as another attach mode, and the protocol leaves room for one.
- **Live sessions stay a mode.** Only `restoreMode live` panes run in the server. Plain panes keep the direct PTY in
  `agterm-term`, and an instance with live sessions off starts no server.
- **What changes for callers.**
  - **No zmx anywhere** (user, 2026-10-18), not even in the control protocol. Done in Task 3: upstream's `zmx.*`
    group is `live.*`, `agtermctl zmx` is `agtermctl live` (a deviation `conformance/docs/rust-build.md` records),
    the attach contract's variables are `AGTERM_LIVE_*`, the role title `agterm-live-role`, the socket directory
    `/tmp/agterm-live-<hash>`. `zmx` stays only where upstream is described or run, and in the environment scrub
    that keeps the user's installed zmx-based agterm away.
  - zmx's daemon wire protocol goes. The server speaks herdr's wire.
  - An upstream Mac can no longer view this machine's sessions, or the reverse; nor can a new build and an older
    Rust build.
  - Old zmx sessions are not migrated (user, 2026-10-18).
- **What stays.**
  - The shape of the attach contract that `agterm-core`, the remote path and the suite drive (Technical Details >
    Attach contract), under its new names.
  - The semantics, fields and error strings of upstream's control-protocol group, under `live.*`, as far as they do
    not name zmx.
  - `agterm-core`'s restore, replay and remote attach (upstream's). herdr's layout persistence and ssh remote are not
    ported (Task 2: no gap they would fill).
  - The live-session policy in `agterm-core::live`: naming, reap, prune, lead, reset, replay.
  - `agtermctl clipboard set` keeps its behavior over the server's API.
  - The conformance ids stay green and keep their names (each adapter translates the vocabulary, Task 3).
- **Windows live sessions run PowerShell 7** with agterm's own integration script, a Rust-only extension like bash
  and fish today (decision of 2026-10-16, kept).
- **Future** (not in this plan, but the design allows it): a TUI frontend for headless servers (herdr's surface
  deltas), and a headless core process that keeps the model and the control socket alive while no client is
  attached. `agterm-session` stays frontend-agnostic, and `agterm-term` stays toolkit-neutral.

## Context (from discovery)

Paths are as of `zig-free-core` at `e67b0ca`, spot-checked on 2026-10-18.

### VT (Part A)

- `scripts/build-ghostty-vt.sh`: downloads the pinned Ghostty archive (`LIBGHOSTTY_VT_REV` `81598385`, sha256),
  applies `build/patches/ghostty-vt/*.patch` with no fuzz, and builds with
  `zig build -Demit-lib-vt=true -Doptimize=ReleaseFast -Dtarget="$ARCH-linux-gnu.2.39"` (Linux only). It stages the
  shell integration, the themes (`LIBGHOSTTY_VT_THEMES_*`) and the terminfo: `zig run` over Ghostty's
  `src/terminfo/ghostty.zig` encoder, then `tic -x`. It needs `curl tar patch sha256sum tic infocmp`.
- `scripts/lib/native.sh`: `find_zig` (a matching zig from `AGTERM_GHOSTTY_VT_ZIG`, `PATH`, mise, or the pinned
  ziglang.org archive), `host_zig_arch`, `prefix_key`. `build/pins.env` pins `LIBGHOSTTY_VT_ZIG_VERSION` 0.16.0 with
  sha256s for Linux x86_64 and aarch64 only (`ZMX_ZIG_*` repeats them).
- `crates/agterm-ghostty-vt-sys/build.rs`: never builds; asks `build-ghostty-vt.sh --print-prefix` (through `bash`),
  checks `lib/libghostty-vt.a` and `include/ghostty/vt.h`, links `static=ghostty-vt`, exports `DEP_GHOSTTY_VT_ROOT`
  (no consumer in the workspace). `src/bindings.rs` is bindgen output generated on Linux x86_64 with
  `--no-layout-tests`, so it carries no size, alignment or offset checks; the generator reads
  `$prefix/include/ghostty/vt.h`, and `snapshot.h` (`ghostty_snapshot_*`) is not generated.
- `scripts/lib/native.sh` `ghostty_vt_prefix_is_complete` checks `lib/libghostty-vt.a` and the headers;
  `verify_ghostty_resources` requires `infocmp` and `share/terminfo/x/xterm-ghostty`. The script uses bash-4
  constructs (`${ARCH^^}`), `sha256sum --check` and GNU `patch` flags. `crates/agterm-term/src/launch.rs` hardcodes
  `../terminfo/x/xterm-ghostty`; macOS `tic` writes hashed directories (`terminfo/78/…`).
- `zig build -Demit-lib-vt` fetches packages at the pin: `uucode` (non-lazy, a GitHub URL in `build.zig.zon`), and
  through the path packages `highway`, `simdutf` and `wuffs` lazy tarballs from `deps.files.ghostty.org`; without
  `--cache-dir` Zig writes `.zig-cache` into the build root.
- `crates/agterm-ghostty-vt`: the safe wrapper (terminal, render state, effects, key/mouse encoders, selection and its
  gesture, formatter, search, Kitty graphics, paste). Not wrapped yet, though the C API has them: the stream
  continuation (`ghostty_terminal_continuation_{alloc,buf,write}`, `OPT_CONTINUATION_MAX_BYTES`), the formatter's
  extras (`format.rs` hardwires `no_extras()`: no cursor, modes, palette, scrolling region, charsets, Kitty keyboard
  state), `TERMINFO_NAME` (its XTGETTCAP `TN` answer), the `COLOR_SCHEME` callback, the binary snapshot codec
  (`vt/snapshot.h`: encode, decode with `ready`/`next`, `OPT_RETAIN_CONTINUATION`, `COMPRESS_HISTORY`, CRCs; format
  version 1, no compatibility promise across pins). The clipboard read callback is synchronous: the stream waits
  until it returns (`terminal.h`).
- `crates/agterm-term`: the pane core over the wrapper; built and tested on Linux only. It depends on the Unix-only
  `agterm-zmx` for its launch inputs (`launch.rs` `from_runtime(&agterm_zmx::Runtime)`); its IO thread polls the PTY
  master with a wake pipe (`pty.rs`: `F_DUPFD_CLOEXEC`, `pipe2`, which Apple's libc lacks); `password_input` uses
  `tcgetattr`; seed commands run through `COMMAND_SHELL = "/bin/sh"`; `effects.rs` and `scan.rs` stop the stream before
  a clipboard read's dispatch byte, so the synchronous callback never blocks the IO thread.
- `crates/agterm-app` also depends on `agterm-zmx` (`process_table.rs` uses `agterm_zmx::sweep::parse_record`) and is
  not in `cross-platform.yml`.
- herdr carries libghostty-vt patches (`.upstream/herdr/vendor/libghostty-vt.patches.md`): 0002 modifyOtherKeys
  query, 0004 hosted Wuffs builds (MSVC libc), 0005 bounded word selection, 0006 clear screen keeping the cursor
  line, 0007 encoded PNG retention, 0008 cursor hyperlink across page replacement (a crash; the pin's `Screen.zig` is
  still unfixed).
- herdr's `crates/ghostty-vt/build.rs` (herdr `143bf5f`): maps cargo targets to Zig triples (linux-gnu/musl x86_64 and
  aarch64, macos x86_64 and aarch64, windows-msvc x86_64 and aarch64), runs `zig build --prefix $OUT_DIR/zig-out
  -Demit-lib-vt -Doptimize=ReleaseFast -Dsimd=… -Dtarget=… -Dversion-string=…`, takes an optional `--libc` file for
  MSVC (`LIBGHOSTTY_VT_WINDOWS_LIBC`), copies the `.a` alone into a directory on Apple (the linker prefers a sibling
  dylib), and links `ghostty-vt-static` on MSVC, `ghostty-vt` elsewhere. Its source is vendored in-tree.
- `.github/workflows/rust-linux.yml` installs the pinned Zig toolchains and builds the libghostty-vt and zmx prefixes,
  cached under `~/.cache/agterm-rs/vendor` keyed like the prefixes.

### On this branch from the paused plan (done, reviewed, CI green on all three OSes)

- `.github/workflows/cross-platform.yml`: job `crates` on `ubuntu-26.04`, `macos-26`, `windows-2025` (fmt, clippy
  `-D warnings` all features, tests of the crates in `PACKAGES`), a Miri job, generator `--check` steps; pushes to
  `ci/**` run it without a PR. `PACKAGES` today: `agterm-vt agterm-session agterm-proto agterm-host agterm-core`.
- `crates/agterm-vt` (rustty's port, Tasks 4–8 of the paused plan), its generators `scripts/gen-vt-unicode.sh` and
  `scripts/gen-ghostty-terminfo.sh`, their pins (`RUSTTY_*`, `UUCODE_*`), NOTICE entries (rustty, uucode, X.Org
  `rgb.txt`) and README lines. No crate depends on it.
- `crates/agterm-session`: `protocol.rs` (envelope, postcard, framing, `MessagePart`, 256 MiB cap), `endpoint/`
  (Unix socket with `server.lock` flock and a peer uid check; Windows named pipe with owner-only DACL and server SID
  check), `server/{mod,lifecycle}.rs`, `server/spawn/{mod,linux,macos,windows}.rs`, `src/bin/agterm-session.rs`
  (`server <dir>` only), tests `protocol`, `endpoint`, `lifecycle`.
- `agterm-proto`, `agterm-host`, `agterm-core` on Windows: `live::socket_directory` per OS (`%LOCALAPPDATA%\agterm\live\
  <hash>`), `live/owner_only.rs` (protected owner-only DACL), Windows forms of the Unix-only spots, `.gitattributes`
  (LF everywhere).
- The zmx rename: `agterm-core::live`, `live.*`, `agtermctl live`, `AGTERM_LIVE_*`, the suite's per-adapter
  vocabulary.
- `docs/spikes/20261018-session-server.md` and `-probes.md`: Task 2's study of herdr, probes and baselines.

### Session host (Part B)

- herdr (`HERDR_REV` `143bf5f`, cloned at `.upstream/herdr`): `src/server/` (spawn, socket paths, clients, transport,
  accept, headless coordinator, handoff, shutdown), `src/protocol/wire.rs`, `src/pane.rs` + `src/pane/**` (per-pane
  runtime, PTY spawn, OSC, XTGETTCAP, Kitty keyboard), `src/terminal/` (runtime registry), `src/pty/` (the PTY actor
  and backends), `src/ipc.rs`, `src/client/`, `src/platform/` (per-OS spawn, jobs, process snapshots, pipe SDDL),
  `src/api/`, `vendor/portable-pty`, `crates/ghostty-vt` (its libghostty-vt binding, the model for Part A's build).
- `crates/agterm-host/src/lib.rs` `SessionHost`: `is_available`, `list`, `kill`, `screen`, `type_input`, `kill_batch`,
  `foreground_job`, `job_running`, `end_job`. The frontend builds the host in `agterm-gpui/src/session_host.rs` and
  `agterm-app/src/restore_launch.rs`.
- `crates/agterm-zmx`: runs the zmx binary (`list`, `kill`, `screen`, `type`), `launch.rs` reads the live-launch
  inputs, `sweep.rs` hangs up a killed shell's foreground job.
- `crates/agterm-core/src/live/`: `support.rs` (the attach command through `/bin/sh -c`), `lifecycle.rs` (list parser,
  leader map, reap policy), `lead.rs`, `reset*`, `replay.rs`, `screen.rs`, `build_record.rs`, `shell_integration.rs`.
- What the suite pins about reattach comes from zmx's `serializeTerminalState` with its patches: the scrollback
  unstyled, then a clear and the visible screen with extras; color overrides; DECSET 2026 off;
  `0003-first-attach-snapshot`'s reset prefix; zmx's private Ghostty patch `0001-formatter-unstyled-blanks` (the
  formatter closes the style before buffering a blank cell, so blank runs are written unstyled). No suite test pins
  the blanks; they are upstream behavior (`.upstream/agterm/scripts/zmx-patches/README.md`), a parity matter, and
  no formatter option controls them at the pin (`src/terminal/formatter.zig`).
- `crates/agterm-app/src/process_table.rs`: the Linux `ProcessTable` over `/proc`, used by the quit-time command
  capture. No macOS or Windows one.
- Conformance: the harness's Rust adapter drives live sessions through the vocabulary of Task 3; `zmx_client.py` and
  the Swift adapters drive upstream's own zmx.

### Patterns and dependencies

- the workspace takes every `crates/*` as a member;
- every `agterm-ghostty-vt` module cites the C header it covers; generated bindings change only through their script;
- `agterm-term` keeps the terminal behind a lock (handles are `Send`, not `Sync`); effects run synchronously inside
  `vt_write`;
- logging through `tracing`; `scripts/check-tracing-features.sh` keeps `log` out of the graph;
- dependencies already in: `portable-pty`, `interprocess`, `windows-sys`, `postcard`, `wmi` (Windows), `libc`. New:
  none expected beyond `vendor/portable-pty` (Task 13).

## Development Approach

- **testing approach**: regular (code, then tests in the same task).
- complete each task fully before moving to the next; small, focused changes.
- **CRITICAL: every task MUST include new/updated tests** for its code changes: unit tests for new and modified
  functions, success and error paths, an existing test updated when behavior changes.
- **CRITICAL: all tests must pass before the next task starts.** The gates of CLAUDE.md apply to every task
  (README > Test): fmt, locked metadata, clippy `-D warnings` on the workspace and on the frontend without its default
  features, the lint report, the tracing-features check, the vendored-source checks, the workspace tests headless,
  the suite's selftests. Tasks that touch the GPUI binary, `agterm-term`, the libghostty-vt build or staging also run
  the full `conformance/tools/run-rust-linux.sh` in the background, and no ratchet id goes red.
- **CRITICAL: update this plan when scope changes.**
- **Backward compatibility**: until the switch (Task 25) zmx stays the session host; until its removal (Task 28) it
  stays built. The app keeps every behavior throughout, and Linux's libghostty-vt build keeps its glibc floor.
- **Windows and macOS**: code for those OSes is debugged in the local VMware VMs `Win11-admin`
  (`~/vmware/Win11-admin/Win11-admin.vmx`) and `macOS 26` (`~/vmware/macOS 26/macOS 26.vmx`), driven through `vmrun`
  from the dev box (the macOS VM also over ssh), user `admin` with password `1234` on both (user, 2026-10-18: they
  are throwaway VMs, so the password belongs here), and gated on `cross-platform.yml`'s runners, not only
  cross-checked.
- **Isolation** (CLAUDE.md > Never touch the user's running agterm) holds everywhere, in the VMs too: every test
  session server runs under a private namespace; reaps go through that namespace's server only, never by process
  name; inherited `AGTERM_*` and `ZMX_*` are stripped from every child environment; `tic` and `infocmp` in tests use a
  temp directory.
- **Provenance**: herdr-derived files get a header naming their herdr source file, Apache-2.0 and "modified"; the
  root `NOTICE` carries herdr; the build script ported from herdr's `build.rs` cites it.

## Testing Strategy

- **unit tests**: required in every task.
- **libghostty-vt build**: the sys crate's link test and the wrapper's tests run on all three OSes in
  `cross-platform.yml`; the bindings carry bindgen's compile-time layout assertions (Task 8 turns them on), so a
  target whose C layout differs from the checked-in bindings fails to build; a build with networking off succeeds
  once the prefix exists.
- **protocol tests** (done, Task 4): every message round-trips, version negotiation refuses a mismatch, malformed
  frames are refused.
- **server e2e tests** (all three OSes on CI): start the server detached from the test process, create a session,
  attach, type, read `screen`, detach; kill the test's side, reattach and see the same screen; `kill` and
  `kill --force`, `list`, `print`, and an update handoff.
- **conformance**: the GPUI run on Linux, in the background, keeps every ratchet id green, including the live-session
  ids, `test_live_any_shell`, `test_restore`, `test_relaunch` and `test_clipboard_set_*`.
- **performance**: a 50 MiB `cat` through a live pane is no slower than through zmx, and the server's idle RSS for N
  panes is no larger than N idle zmx daemons' (Task 2's baselines, Technical Details).

## Progress Tracking

- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- update the plan if implementation deviates from the original scope

## Solution Overview

### Part A: libghostty-vt everywhere

```
scripts/build-ghostty-vt.sh   fetch + sha256 the source, themes and Zig packages, apply patches, stage resources
        │ keyed prefix: patched source tree + Zig package cache + include/ + share/ (shell integration, themes, terminfo)
agterm-ghostty-vt-sys/build.rs   zig build -Demit-lib-vt -Dtarget=<cargo TARGET> (cache under OUT_DIR), link the archive
agterm-ghostty-vt                safe wrapper, extended for the session server (continuation, extras, snapshot codec)
agterm-pty                       herdr's PTY actor over the vendored portable-pty (openpty │ ConPTY)
agterm-term                      pane core: plain PTY over agterm-pty or, from Task 26, the attach stream
```

- **One source of truth for the build.** The script prepares; cargo builds. A crate that depends on the sys crate
  builds with plain `cargo build` once the script has run, on every OS, and no build reaches the network.
- **Targets**: herdr's map, with `-Dcpu=baseline` so no build targets the host's own CPU. Linux keeps `-gnu.2.39`
  (today's glibc floor); macOS takes its deployment target from `MACOSX_DEPLOYMENT_TARGET`, else rustc's default for
  the arch, and passes it in `-Dtarget=<arch>-macos.<ver>`; Windows builds for `windows-msvc` and links
  `ghostty-vt-static`.
- **Bindings**: one checked-in `bindings.rs` with bindgen's layout assertions; per-target files from the generator
  only if a target's layout differs (Task 10 decides from the CI result).
- **Patches**: `build/patches/ghostty-vt/` holds 0001 and the recorded logic patches (Task 9), with a `README.md` in
  herdr's patches.md form.
- **Resources per OS**: the terminfo source is staged everywhere, its `tic`-compiled database where `tic` exists
  (Windows programs do not read terminfo; Task 10 records what a WSL or ssh session inside a pane sees). Readers
  accept both terminfo layouts (`x/` and macOS's hashed `78/`).
- **One PTY layer.** herdr's PTY actor (poll + wake pipe on Unix, reader and writer threads on Windows) over the
  vendored `portable-pty` is a crate of its own, `agterm-pty` (`any`), because plain panes in `agterm-term` and live
  panes in the server both need it and neither should depend on the other for it.

### Part B: `agterm-session`

```
agterm-core::live (policy unchanged)       agtermctl live … / clipboard set (upstream semantics, renamed)
        │ SessionHost
agterm-session ──────────────────────────────────────────────────────────────────
  server     one per instance (herdr's server): started on demand, detached per OS,            [done: Task 6]
             owns every live pane: PTY (agterm-pty) + libghostty-vt state
  protocol   herdr's framing, postcard, a stable version envelope; one endpoint (socket │ pipe) [done: Task 4]
  api        list / new / kill (batch, force) / screen / type / print
  attach     snapshot + continuation, then the offset-tagged raw output; input, resize, metadata back
  handoff    update without ending sessions: Unix by herdr's SCM_RIGHTS, Windows by duplicated handles
  client     library: the api + the attach stream (used in process by agterm-term)
  host       SessionHost impl (replaces today's zmx client), foreground job and process table per OS
  launch     the live-launch inputs (executable, build id, login shell, resources, namespace)
  bin        agterm-session: server │ attach (the kept attach contract) │ list │ kill │ screen │ type │ print │ version
agterm-term  Pane IO backend: PTY (plain panes) │ attach stream (live panes, no PTY, no subprocess)
agterm-ghostty-vt  the VT state of both the pane and the server
```

- **One server per instance**, the `agterm-session` binary started as `agterm-session server <dir>` and detached from
  the app, so live panes outlive the app. The app, `agtermctl`, the suite and remote viewers are its clients. The
  server links libghostty-vt, not GPUI. A libghostty-vt crash ends every live session at once, where zmx lost one,
  which is why Task 9 carries herdr's crash fixes and the server's VT runs the same pin and patches as the app's.
- **Rollout in two steps**, so the ratchet never goes red:
  1. Task 25 stages `agterm-session` in zmx's place. The GPUI pane still runs `agterm-session attach` in a PTY, and
     that CLI client is a full reply owner (Task 22), so no live pane loses its theme replies or clipboard reads.
  2. Task 26 swaps the pane to the in-process attach stream.
- **Namespace** (done): Unix `/tmp/agterm-live-<FNV-1a 64 of the state dir>`; Windows
  `%LOCALAPPDATA%\agterm\live\<the same hash>` with the pipe `agterm-live-<FNV-1a 64 of the case-folded directory>`.
- **In-process attach.** `agterm-term` opens the session's stream, claims or follows the lead as `agterm-core`'s lead
  policy says, sends its grid size, then maps snapshot and Output → `vt_write` (its own PTY replies suppressed; the
  server answers queries), `Pane::write` → Input, `Pane::resize` → Resize, Role changes → the lead state core tracks.
- **Platforms.**
  - Linux: `setsid` detach (done), PTY via `openpty`.
  - macOS: `posix_spawn` with `POSIX_SPAWN_SETSID` and `responsibility_spawnattrs_setdisclaim` (done).
  - Windows: ConPTY; the server detaches with `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`, job breakaway or WMI
    (done). Each pane's shell tree runs in a job object with `KILL_ON_JOB_CLOSE`.
- **Updates.** The build id is a hash of the staged `agterm-session` binary (`share/agterm/session-BUILD`). A client
  finding a server of another build asks it to hand off; a failed handoff falls back to `build_record.rs`'s
  "predates the last update" reset offer.

## Technical Details

### libghostty-vt build

- **Script** (`build-ghostty-vt.sh`, same name and options):
  - source archive and theme package with their sha256s, patches with no fuzz, resources staged as today;
  - the Zig packages fetched into a package cache inside the prefix (`zig build --fetch`, or `zig fetch` per
    dependency), so `build.rs` never reaches the network;
  - the prefix holds the patched tree, the package cache, `include/` (copied from the tree, for the bindings
    generator), `share/`, and a stamp of its inputs; `ghostty_vt_prefix_is_complete` and
    `verify_ghostty_resources` check that layout per OS; `--print-prefix` is unchanged;
  - portable shell: no bash-4 constructs (stock macOS has bash 3.2), `sha256sum` or `shasum -a 256` per OS, `patch`
    flags both GNU and BSD accept (checked), a named unzip tool on Windows, a missing tool reported with its reason.
- **Windows shell**: `build.rs` runs the script with Git Bash found explicitly (never `bash` from `PATH`, which can be
  WSL's `System32\bash.exe`), and converts the printed prefix with `cygpath -m`; `AGTERM_GHOSTTY_VT_PREFIX` overrides
  the lookup on every OS.
- **Zig**: `LIBGHOSTTY_VT_ZIG_SHA256_<OS>_<ARCH>` for linux, macos and windows on x86_64 and aarch64 (ziglang.org's
  0.16.0 archives, looked up live when written). CI runs Linux x86_64, macOS arm64 and Windows x64; the other three
  get a cross `zig build` check on the Linux job, named as such. `find_zig` keeps its `build-zmx.sh` caller working
  until Task 28.
- **Caches**: Zig's global cache sits in the vendor root keyed by the Zig version (safe for concurrent processes, never
  the user's own cache), and `actions/cache` saves it with the prefix; the local `--cache-dir` and `--prefix` go
  under `OUT_DIR`. Task 10 measures the cold build per runner against the jobs' timeouts.
- **`build.rs`** (from herdr's, cited): target map; `-Dtarget` as Solution Overview says; `-Dcpu=baseline`;
  `-Doptimize=ReleaseFast` (Debug's integrity checks make PTY output unusably slow); `-Dsimd=true`; Apple: the `.a`
  copied alone into a link directory; MSVC: `ghostty-vt-static`, plus the system libraries the link test shows it
  needs (recorded in the build script's doc); `rerun-if-changed` on the prefix stamp and the patches. A missing prefix
  keeps today's error that names the script. `DEP_GHOSTTY_VT_ROOT` goes, since nothing reads it.
- **VM prerequisites**: VS Build Tools in `Win11-admin` (installed), Xcode command-line tools in `macOS 26` (highway's
  `apple_sdk`).
- **CI**: `cross-platform.yml` installs the pinned Zig per runner (sha256-checked), runs the script, restores the
  caches, and adds `agterm-ghostty-vt-sys`, `agterm-ghostty-vt`, `agterm-pty`, `agterm-term` and (Task 15)
  `agterm-app` to `PACKAGES`. `rust-linux.yml` keeps its prefix step.

### `agterm-session`

- **Source and license.** herdr is pinned in `build/pins.env` (`HERDR_REPO`, `HERDR_REV`). Each derived file gets a
  header naming its herdr source file, Apache-2.0 and "modified"; herdr's `LICENSE` goes into `NOTICE`, and
  `agterm-session` declares `MIT AND Apache-2.0`. herdr's own libghostty-vt binding is not ported: the server uses
  `agterm-ghostty-vt`.
- **Session creation.** A session is created with the pane's argv (create-only), environment (`TERM=xterm-ghostty`,
  `TERMINFO`, the integration variables, the tracked env) and cwd. The child environment keeps today's rules: the
  session name variable, `TERM` fixed when unset or `dumb`, the managed-claim variables removed.
- **Snapshot on attach.** Two encodings, both libghostty-vt's, never a hand-rolled VT encoder:
  - the formatter's VT restoration with its extras (cursor, modes, palette, scrolling region, charsets, Kitty
    keyboard state) and the stream continuation, matching what the suite pins (Context). The unstyled blanks come
    from upstream's formatter patch (Task 9). This is what `agterm-session attach` (CLI, remote) writes;
  - the binary snapshot codec, where both ends run the same pin: the exact state, continuation and history. Task 21
    evaluates it for the handoff and Task 26 for the in-process attach, and records the choice; across a pin change
    the formatter path or the reset offer applies.
- **API.** Exactly what `SessionHost` and `clipboard set` need:
  - list, with every `LiveSessionRecord` field (`clients`, `leader_pid`, `created_at`, …);
  - kill (one, batch, `force`) with every `KillOutcome`; `StaleSocket` maps to the session-gone outcome, tested;
  - screen (`active`/`all`) with `LiveScreen`'s `revision`, cursor and `alternate`;
  - type: CR as Return, encoded for the keyboard mode, queued without taking the lead;
  - foreground job;
  - print, for `agtermctl clipboard set`: an OSC 52 write into the session's output.
- **Attach contract** (kept; `agterm-session attach` honors it):
  - `attach <name> [creation argv…]`: create-only, with the gone guard `agterm-core::remote::ssh` appends;
  - the variables `AGTERM_LIVE_DIR`, `AGTERM_LIVE_SESSION`, `AGTERM_LIVE_SESSION_PREFIX`,
    `AGTERM_LIVE_NO_DETACH_KEY`, `AGTERM_LIVE_MANAGED`, `AGTERM_LIVE_MANAGED_CLAIM` (empty means unset);
  - the lead role as the title `OSC 2;agterm-live-role;<token>:<role>:<gen>`;
  - `AGTERM_LIVE_SESSION` and `AGTERM_LIVE_DIR` set in each session's children.
- **Leadership.** Size ownership with several clients follows today's explicit-leadership semantics (zmx patch
  `0001-explicit-leadership`), which `agterm-core`'s lead policy and the suite pin; herdr's resize ownership is
  adapted to it.
- **Terminal replies.**
  - The server VT answers state and capability queries (DA1/2/3, DSR/CPR, DECRQM, DECRQSS, XTVERSION, XTGETTCAP with
    `TERMINFO_NAME`, CSI ? 996 n through the `COLOR_SCHEME` callback) through libghostty-vt's effects, so the session
    has one identity. Frontend-dependent replies (OSC 4/10/11/12 colours, XTWINOPS pixel sizes) come from metadata
    the reply-owner client supplies on attach and on change, with stable defaults when there is none.
  - OSC 52 reads: the clipboard callback is synchronous, so the server does what `agterm-term` does locally. A shared
    scan (`scan.rs` moved into `agterm-ghostty-vt`, not copied) stops the stream before the read's dispatch byte; the
    PTY keeps draining into the pane's queue, readers and subscribers see the output up to that point, and the read
    goes as a single-recipient request to the reply owner (the leader, else the first managed client) with a
    deadline; no owner or a passed deadline denies it, and parsing resumes.
  - Kitty graphics: the reply owner's VT answers APC G (acknowledgements and query replies), and the server VT keeps
    image media off, so a `t=t` temp file or `t=s` shared memory is read once, by the client that shows it.
  - Attached VTs other than the reply owner's write no replies; `agterm-session attach` in an arbitrary terminal
    filters the server-owned query sequences out of its output, fragment-safe, and answers as reply owner from the
    outer terminal (Task 22).
- **Foreground job**: Linux `/proc` process groups behind a per-OS module; macOS `sysctl` (`KERN_PROC_PGRP`,
  `KERN_PROCARGS2`) as upstream's `ForegroundProcess.swift`; Windows herdr's process snapshot (the shell's live
  descendants; `end_job` terminates them).
- **`ProcessTable` for all three OSes** lives beside the job module in `agterm-session` (herdr's platform snapshot is
  there); the Linux one moves from `agterm-app`, which uses it. Windows argv from
  `NtQueryInformationProcess(ProcessCommandLineInformation)` split with `CommandLineToArgvW`, as herdr does.
- **PowerShell**: `ShellKind::Pwsh` with an integration script (OSC 7, OSC 133, title), launch arguments, a pwsh
  replay script, and an eligibility probe on Windows (`pwsh.exe`, else `powershell.exe`, else the existing
  rejection).

#### What Task 2 settled (herdr `143bf5f`; 2026-10-18)

Sources: `.upstream/herdr`; probes, logs and the design discussion in `docs/spikes/20261018-session-server.md` and
`-probes.md`. Paths are herdr's (`src/`).

- **Module map.** Keep = ported close to herdr's code; adapt = ported and reworked; cut = not ported.

  | herdr module | verdict | what agterm-session takes, or why not |
  |---|---|---|
  | `protocol/wire.rs` | adapt (done) | framing, `FramingError`, version check; bincode → postcard |
  | `protocol/endpoint.rs`, `render_ansi.rs`, `surface_*` | cut | server-side rendering for TUI clients (the future TUI plan) |
  | `ipc.rs` | keep (done) | local sockets, the named-pipe listener with its SDDL, stale-socket detection, peer-closed probe |
  | `server/socket_paths.rs`, `server/autodetect.rs` | adapt (done) | owner-only paths from agterm's namespace; find or spawn detached |
  | `server/client_accept.rs`, `client_transport.rs`, `clients.rs` | adapt | accept loop, handshake with timeout, a reader and a writer thread per connection, connection modes, attach owners; the writer queue byte-bounded and lossless for the raw lane |
  | `server/headless.rs`, `headless/bootstrap.rs`, `headless/lifecycle.rs` | adapt | the coordinator's client, attach, resize-lock and handoff handling; everything that renders, notifies or drives the TUI goes |
  | `server/handoff.rs`, `handoff_runtime.rs` | keep (Unix) | token-authenticated private socket, JSON manifest, `SCM_RIGHTS` batches of 64, validated/restored/ready/committed/owned steps and their timeouts; the per-pane state carries agterm's snapshot |
  | `server/shutdown.rs` | keep | `ServerStop` and the logged shutdown reason |
  | `server/alt_screen_read.rs`, `terminal/history_read.rs` | cut | agterm reads the server VT |
  | `server/headless/{render,…}.rs`, `client_shell*`, `terminal_attach.rs`, notifications, image paste | cut | TUI client shell and rendering |
  | `pane.rs` | adapt | pane spawn (argv, env, cwd), child wait, `shutdown_pane_processes` (HUP, TERM, KILL), `from_handoff_fd`; agent detection and render signalling go |
  | `pane/terminal.rs` | adapt | herdr's libghostty-vt use is the model for the server VT over `agterm-ghostty-vt`, and its history seeding on import for the snapshot replay |
  | `pane/{osc,xtgettcap,kitty_keyboard}.rs` | adapt where needed | what herdr answers itself because libghostty-vt leaves it to the embedder; Task 20 checks each against the C API's effects and keeps only those |
  | `pane/{cursor,input,agent_detection,background_agent}.rs` | cut | agent detection is herdr's product; input encoding is the wrapper's |
  | `terminal/runtime.rs`, `runtime_registry.rs` | adapt | the per-pane runtime facade (write, resize, snapshot, handoff export) and the registry by session name |
  | `pty/actor.rs`, `pty/actor/unix.rs`, `pty/backend*`, `pty/fd.rs` | keep (in `agterm-pty`) | the PTY actor thread (poll + wake pipe on Unix, reader/writer threads on Windows), quiesce/release for handoff, CLOEXEC master duplication; `tokio::sync::mpsc` → std channels |
  | `client/handshake.rs`, `client/mod.rs`, `client/terminal_setup.rs`, `client/input/windows_vti.rs` | adapt | the client library's connect + hello; the `agterm-session attach` terminal client |
  | `client/attach.rs`, `client/shell/**`, `session.rs` | cut | herdr's prefix keys, TUI and named sessions |
  | `platform/windows.rs` | adapt | spawn with the WMI path (done), pipe security (done), process snapshot, `session_processes`, foreground job, `command_line_to_argv` |
  | `platform/{linux,macos,unix_common,mod}.rs` | adapt | `session_processes`, foreground process group, `signal_processes`, SIGHUP ignore, nofile limit |
  | `api/server.rs` | adapt | the accept-loop pattern and the live-handoff request; the JSON-lines API is replaced by the binary API |
  | `api/{schema,…}.rs`, `cli/**`, `persist/**`, `remote/**` | cut | herdr's public API and CLI; persist and remote fill no gap |
  | `vendor/portable-pty` | adapt | patches 0003 (malformed Windows environments) and 0004 (directories on `PATH`); 0001 reduced to kernel32's ConPTY only; 0002 not needed; plus agterm's patch exporting and importing a ConPTY master's handles for the handoff |
  | `crates/ghostty-vt`, `vendor/libghostty-vt.patches.md` | model for Tasks 8–9 | its `build.rs` is ported into `agterm-ghostty-vt-sys`, its patches reviewed against the pin; its binding is not ported |

- **Attach** (agreed with Codex, transcript in the spike note):
  - one serialization point per pane orders parsing, output offsets, snapshot capture, subscriber registration,
    resize and role changes;
  - wire: `SnapshotBegin{epoch, offset, size}`, bounded chunks, `SnapshotEnd`, then contiguous offset-tagged
    `Output`;
  - the snapshot carries libghostty-vt's validated, replay-safe stream continuation. Continuation capture has a byte
    cap and an unavailable outcome: never truncated or replaced by empty state. When unavailable, the snapshot
    waits, within a deadline, while the PTY keeps draining; with no valid cut by then, attach or resync fails
    without harm, and a handoff aborts before its commit;
  - per-client raw queues are byte-bounded and lossless and never block the PTY reader. On overflow the server
    resyncs in band (new snapshot, replacing only unsent output; replies tagged with a superseded epoch dropped),
    within a recovery budget; past it, disconnect;
  - terminal replies as Technical Details > Terminal replies says; on Windows, conhost answers some queries itself,
    and Task 20 records which reach the output pipe per supported Windows version.
- **Wire** (done): a stable envelope (magic, protocol version) precedes postcard; capabilities advertised; one
  listener per namespace, a separate connection per role (api request, attach). Message families: hello/welcome,
  api request/response by id, attach (attach, snapshot begin/chunk/end, output, input, reply metadata and delegated
  requests, resize, role, detach, exit), shutdown, handoff control. The handoff keeps herdr's private socket, token
  and JSON-lines manifest (a named pipe on Windows).
- **Runtime: threads, no tokio.** One coordinator on `recv_timeout(next deadline)`, blocking connection workers, a PTY
  reader and writer per pane. The client library stays synchronous inside `agterm-term`'s IO thread.
- **Windows handoff: conditional GO.** Proven in `Win11-admin` (10.0.26100.9711) and on `windows-latest` and
  `windows-2022`: a second server receives by `DuplicateHandle` the input write end, the output read end, the
  pseudoconsole's three internal handles (kernelbase's `{hSignal, hPtyReference, hConPtyProcess}`, undocumented), the
  job and the shell process; the shell keeps its state and resize works through a rebuilt HPCON. Guards Task 21 must
  build: a layout gate before any dereference (support matrix by ConPTY implementation, architecture and Windows
  version; unknown → no handoff); custom ownership of the rebuilt struct (never `ClosePseudoConsole` on it); handoff
  bound to the replacement's process handle; versioned manifest; prepare/ready/commit with a rollback that never
  touches the shared job or console; quiesced reader, input, resize and replies; already-read bytes and the VT
  continuation transferred. A failed gate keeps the old server serving and offers the reset.
- **macOS**: setsid + responsibility disclaim proven in `macOS 26` and on `macos-15`/`macos-26`; an `SCM_RIGHTS`
  handoff keeps the shell with its state, resize and hangup. herdr#4920 did not reproduce; the launchd one-shot job
  stays the recorded fallback. Every PTY master is close-on-exec, and the macOS spawn adds
  `POSIX_SPAWN_CLOEXEC_DEFAULT` (a stray inherited master held an exiting shell in the tty drain).
- **Unix handoff**: the new server is not the shell's parent, so a session's exit status after a handoff is unknown
  (herdr's `ChildExitReason::Handoff`). On Windows the duplicated process handle keeps it.
- **Windows kill**: the per-pane `KILL_ON_JOB_CLOSE` job passes in the handoff, or the old server's exit would kill
  every shell.
- **Gaps** herdr's `persist` or `remote` would fill: none.
- **Baselines** (dev box, zmx pin `8bab1f0`): `agterm-term`'s `bench.rs` 50 MiB `cat`: plain 0.462/0.486/0.430 s,
  through `zmx attach` 0.801/0.899/0.906 s; the GPUI build (headless weston on lavapipe): plain pane
  1.503/1.670/1.627 s, live zmx pane 2.466/2.630/2.253 s; idle zmx daemons holding `/bin/sh` after 20 000 lines:
  13.9 MiB RSS each (1: 14.6 MiB, 4: 54.3 MiB, 16: 222.9 MiB). Targets: no slower, no larger (Task 26 measures,
  Task 29 checks).

## What Goes Where

- **Implementation Steps** (`[ ]` checkboxes): code changes, tests, CI and documentation in this repository.
- **Post-Completion** (no checkboxes): manual checks in a dev instance and the VMs, and changes outside the repo.

## Implementation Steps

Tasks 1–6 were done under the paused plan (numbers 3, 2, 19, 20, 21, 22 there); their notes are in
`docs/plans/20261018-zig-free-core.md` on `rust-vt-port`.

Order after Task 7, as two lanes that meet:
- **lane A (VT)**: 8 → 9 → 10 → 11;
- **lane B (no VT)**: 12 (launch inputs off `agterm-zmx`) → 13 (`agterm-pty`) → 15 (process table and job);
- **then**: 14 (`agterm-term` on all three) needs 10, 12 and 13; 16 → 17 → … → 23 need 11 and 13; 24 needs 23;
  25 needs 22, 24 and 14; 26 needs 25; 27 needs 15 and 25; 28 needs 26; 29 and 30 last.

### Task 1: Cross-platform CI (paused plan's Task 3)

- [x] `.github/workflows/cross-platform.yml`: `ubuntu-26.04`/`macos-26`/`windows-2025` matrix with fmt, clippy and
      tests of the crates in `PACKAGES`; a Miri job; pushes to `ci/**` run it without a PR
- [x] green on all three

### Task 2: Study herdr's session server and spike its risky parts (paused plan's Task 2)

- [x] herdr pinned (`HERDR_REV`), its module map and verdicts, the wire, the runtime choice, the Windows handoff
      (conditional GO), the spawn per OS, baselines (Technical Details > What Task 2 settled)
- [x] spike note and probes in `docs/spikes/20261018-session-server*.md`

### Task 3: Rename zmx out of agterm's code and control protocol (paused plan's Task 19)

- [x] `agterm-core::live`, the `live.*` group, `agtermctl live`, `AGTERM_LIVE_*`, the role title, the socket
      directory; a vocabulary per suite adapter; the deviation recorded; ratchet green

### Task 4: `agterm-session` crate: wire and endpoint (paused plan's Task 20)

- [x] envelope, postcard framing with its guards, message parts; Unix socket endpoint with `flock` and peer uid;
      Windows named pipe with owner-only DACL and server SID check; tests on all three OSes

### Task 5: `agterm-proto`, `agterm-host` and `agterm-core` on Windows (paused plan's Task 21)

- [x] Windows namespace directory, owner-only DACL, `cfg` pairs for the Unix-only spots, the three crates in
      `cross-platform.yml`; agterm-server and agterm-ctl kept out with the reason recorded

### Task 6: Server lifecycle: start on demand, detach per OS, one per namespace (paused plan's Task 22)

- [x] `connect_or_start` / `connect_existing`, `Lifecycle` (startup and idle grace, final stop), detach per OS
      (setsid + close-on-exec; posix_spawn with SETSID, CLOEXEC_DEFAULT and responsibility disclaim; CreateProcessW
      with breakaway or WMI), the `server.detached.v1` lifetime check; tests on all three OSes

### Task 7: Take the Rust VT port off this branch

**Files:**
- Delete: `crates/agterm-vt/`, `scripts/gen-vt-unicode.sh`, `scripts/gen-ghostty-terminfo.sh`,
  `docs/plans/20261018-zig-free-core.md`, `docs/zig-free-core-paused.md`
- Modify: `Cargo.toml` (the `agterm-vt` member's dependencies, the `bytemuck` workspace dependency only it uses, the
  `windows-sys` `Win32_System_Memory` comment naming its page allocator), `Cargo.lock`,
  `.github/workflows/cross-platform.yml` (`PACKAGES`, the generator steps, the Miri job), `build/pins.env`
  (`RUSTTY_*`, `UUCODE_*`), `NOTICE` (rustty, uucode, X.Org `rgb.txt`), `README.md`, `.gitattributes` if it names the
  crate's files

- [x] delete the crate, its generators and their pins, CI steps and NOTICE entries; keep herdr's NOTICE entry and
      everything Part B uses (`.gitattributes`, the Windows `cfg` pairs, `cross-platform.yml`'s other crates)
- [x] remove the paused plan and its note from this branch (both stay on `rust-vt-port`); this plan's Overview names
      where they are
- [x] `git grep` finds no `agterm-vt`, generator or pin outside this plan and historical records (the spike notes)
- [x] run gates; push a `ci/` branch and get `cross-platform.yml` green (the Miri job removed with its reason)

### Task 8: Build libghostty-vt from the sys crate's build script (Linux, behavior unchanged)

**Files:**
- Modify: `scripts/build-ghostty-vt.sh`, `scripts/lib/native.sh` (`ghostty_vt_prefix_is_complete`,
  `verify_ghostty_resources`, `find_zig` keeping `build-zmx.sh` working), `scripts/gen-ghostty-vt-bindings.sh`
  (headers from the prefix's `include/`, layout tests on), `crates/agterm-ghostty-vt-sys/{build.rs,Cargo.toml,
  src/bindings.rs}`, `crates/agterm-ghostty-vt-sys/tests/*`, `.github/workflows/rust-linux.yml`, `scripts/stage.sh`
  and `scripts/lint-report.sh` (if they read the library), the upstream-sync skill's pins-delta (if it names it)

- [x] the script stops building the library: it prepares the patched tree, prefetches the Zig packages into the
      prefix, copies `include/`, stages the resources and writes an input stamp, as Technical Details says;
      `--print-prefix` unchanged
- [x] `build.rs` ported from herdr's (cited): target map, `zig build -Demit-lib-vt` with Linux's glibc floor,
      `-Dcpu=baseline`, ReleaseFast and SIMD, `--prefix` and `--cache-dir` under `OUT_DIR`, the global cache in the
      vendor root, the package cache from the prefix, the static link; the zig binary from `find_zig`'s pin (via the
      prefix stamp), never an unpinned one; a missing prefix keeps today's error naming the script;
      `DEP_GHOSTTY_VT_ROOT` dropped
- [x] `gen-ghostty-vt-bindings.sh` drops `--no-layout-tests`, also emits `snapshot.h`, and the bindings are
      regenerated through it (its `--check` holds)
- [x] every reader of the old prefix follows (`stage.sh`, the lint report, `rust-linux.yml`'s cache and steps)
- [x] tests: the link test; a changed patch or pin rebuilds; a missing prefix fails with the named error; a build with
      networking off succeeds once the prefix exists; the path and argument builder puts prefix and cache dirs under
      `OUT_DIR` per target (a unit test of the pure function)
- [x] run gates and the full conformance run in the background (staging changes): ratchet green

### Task 9: The recorded Ghostty patches

**Files:**
- Create: `build/patches/ghostty-vt/README.md`, `build/patches/ghostty-vt/0002-…` onward
- Modify: the patches' tests in `crates/agterm-ghostty-vt` (and `agterm-term` where a behavior shows there)

- [x] review `.upstream/herdr/vendor/libghostty-vt.patches.md` against the pin: carry 0004 (hosted Wuffs, MSVC libc)
      and 0008 (cursor hyperlink across page replacement); decide 0002, 0005, 0006 and 0007 against what agterm uses,
      each recorded with its reason
- [x] carry upstream's unstyled-blanks formatter patch (`.upstream/agterm/scripts/zmx-patches/`, the zmx patch
      `ghostty/0001-formatter-unstyled-blanks`), rebased onto the pin
- [x] `README.md` lists every patch (0001 too) with its reason, upstream issue or PR, the base, the files, a
      remove-when condition and its verification, as herdr's patches.md does; the script applies them in name order
- [x] tests: each patch's behavior through the wrapper (the hyperlink case that crashed, blank runs written unstyled,
      what the carried others change); the bindings `--check` after any header change
- [x] run gates and the full conformance run in the background

### Task 10: libghostty-vt on macOS and Windows

**Files:**
- Modify: `build/pins.env` (`LIBGHOSTTY_VT_ZIG_SHA256_<OS>_<ARCH>`), `scripts/lib/native.sh` (`find_zig` per OS,
  `.zip` on Windows, per-OS checks), `scripts/build-ghostty-vt.sh` (portable shell, per-OS tools, terminfo per OS),
  `crates/agterm-ghostty-vt-sys/build.rs` (Git Bash, `cygpath -m`, the prefix override, Apple and MSVC links, the
  deployment target), `crates/agterm-ghostty-vt-sys/tests/prefix_keys.rs`, `crates/agterm-wpe-sys/tests/provider.rs`
  (bash-driven tests per OS or gated with the reason), `crates/agterm-term/src/launch.rs` (both terminfo layouts),
  `.github/workflows/cross-platform.yml` (Zig, the prefix step and caches, `PACKAGES`, the cross `zig build` checks),
  possibly `scripts/gen-ghostty-vt-bindings.sh` (per-target bindings)

- [x] Zig pins for macOS and Windows on x86_64 and aarch64, looked up live from ziglang.org with their sha256s;
      `find_zig` fetches and unpacks them per OS (`LIBGHOSTTY_VT_ZIG_SHA256_<OS>_<ARCH>`, `zig_pin_sha256`; `.tar.xz`
      with xz or macOS's bsdtar, `.zip` with Git Bash's unzip)
- [x] the script runs on stock macOS (bash 3.2, `shasum`, BSD `patch`) and under Git Bash on Windows, debugged in
      the VMs; the terminfo source staged everywhere, the compiled database where `tic` exists; readers accept both
      terminfo layouts
      - ⚠️ debugged in `Win11-admin` (Git Bash has `patch`, `unzip`, `tic`/`infocmp`; its `tic` writes the hashed
        `78/` layout too); the `macOS 26` VM could not start (its `.vmx` held by the open VMware Workstation UI), so
        macOS was debugged on the `macos-26` runner
- [x] `build.rs`: Git Bash found explicitly on Windows and the printed prefix converted; `AGTERM_GHOSTTY_VT_PREFIX`
      overrides the lookup; Apple links the archive alone from its own directory with the deployment target in
      `-Dtarget`; MSVC links `ghostty-vt-static` and the system libraries the link test shows it needs (none: it links
      alone; `PKG_CONFIG` is removed from zig's environment, since Zig cannot run the workspace's shell wrapper on
      Windows)
      - ⚠️ the macOS deployment target is floored at 13.0, not rustc's default: Ghostty's build substitutes its own
        floor (`Config.zig` `osVersionMin`) for any `-Dtarget` version on a macOS host, so `.cargo/config.toml` sets
        `MACOSX_DEPLOYMENT_TARGET = "13.0"` and `zig_target` refuses an older one
- [x] the bindings' layout assertions build on all three; if a target differs, the generator emits per-target
      bindings (decided from the CI result, recorded: no target differs, so one `bindings.rs` stays)
- [x] `agterm-ghostty-vt-sys` and `agterm-ghostty-vt` join `cross-platform.yml` with the caches; their tests run on
      `macos-26` and `windows-2025`; the three untested Zig targets get a cross `zig build` check on the Linux job;
      the cold build per runner measured against the jobs' timeouts and recorded (the workflow's comment)
      - ⚠️ aarch64 Windows is cross-built on the Windows job, not the Linux one: Zig builds windows-msvc only against
        an MSVC installation (from Linux, simdutf and Wuffs' translate-c find no MSVC headers); aarch64 Linux and
        x86_64 macOS cross-build on the Linux job
- [x] tests on every OS: the wrapper's existing tests; no "built for newer macOS" link warning; the terminfo lookup on
      macOS's hashed layout; the bash-driven tests run or are gated with the reason (the sys crate's run on all three
      through `script.rs`'s bash; `agterm-wpe-sys`'s provider tests are Linux-only, like the crate; the macOS link
      check reads each archive member's `LC_BUILD_VERSION`; the key test expects macOS's Option, which sends its
      text unless `macos-option-as-alt`)
- [x] run gates; `cross-platform.yml` green on all three

### Task 11: Extend `agterm-ghostty-vt` for the session server

**Files:**
- Modify: `crates/agterm-ghostty-vt/src/{terminal,format,effects,lib}.rs`
- Create: `crates/agterm-ghostty-vt/src/{continuation,snapshot,scan}.rs` (`scan.rs` moved from `agterm-term`)
- Modify: `crates/agterm-term/src/{effects,scan}.rs` (use the moved scan)

- [x] the stream continuation: capture with `OPT_CONTINUATION_MAX_BYTES`, its unavailable outcome, and replay
- [x] the formatter's extras (cursor, modes, palette, scrolling region, charsets, Kitty keyboard state) as options
      beside today's `no_extras()`, which stays the default for current callers
- [x] `TERMINFO_NAME`, the `COLOR_SCHEME` callback and the clipboard-read scan (`scan.rs` moved from `agterm-term`,
      which then uses it from here)
      - the `COLOR_SCHEME` callback was already wrapped (`Effects::color_scheme`, with its CSI ? 996 n test); this
        task adds nothing to it
- [x] the binary snapshot codec over `snapshot.h`: encode with its options, decode with `ready`/`next`, its errors
      - encode has no options in the C API; the options are the decoder's (`SnapshotOptions`). The incremental
        decoder lives inside the `Terminal` its READY returned (it borrows that terminal until FINISH), so safe code
        cannot free or swap the terminal under it
- [x] tests on all three OSes: continuation cuts inside a UTF-8 sequence, a CSI and an OSC, an overlong sequence,
      embedded C0, recovery after a terminator, original VT + suffix ≡ restored VT + suffix; the formatter with
      extras restoring cursor and modes into a fresh terminal; XTGETTCAP `TN`; CSI ? 996 n; a snapshot round trip
      with history and continuation, and a corrupt one refused; `agterm-term`'s clipboard-read tests unchanged
- [x] run gates (full conformance run in the background, since `agterm-term` changes)

### Task 12: Launch inputs out of `agterm-zmx`

**Files:**
- Create: `crates/agterm-session/src/launch.rs`, its tests
- Modify: `crates/agterm-zmx/src/launch.rs` (re-exports), `crates/agterm-term/{Cargo.toml,src/launch.rs}`,
  `crates/agterm-app/{Cargo.toml,src/process_table.rs,src/restore_launch.rs}`, `crates/agterm-zmx/src/sweep.rs`
  (`parse_record` moves)

- [x] `agterm-session::launch` holds the live-launch inputs (executable, build id, login shell with its Windows form,
      Ghostty resources, namespace, the debug override) and `parse_record`; `agterm-zmx` re-exports them until
      Task 28
      - `parse_record` sits in a module of its own, `agterm-session::procfs` (a `/proc` stat parser is no launch
        input); the Windows login shell is herdr's: the first launchable `pwsh.exe` on `PATH`, else
        `powershell.exe` (`launch/windows_shell.rs`); `Runtime.zmx_executable` is `executable`, and `client()`
        stays in `agterm-zmx` as `launch::client(&Runtime)`
- [x] `agterm-term` and `agterm-app` take them from `agterm-session`; `agterm-zmx` becomes a Unix-only dependency
      (`[target.'cfg(unix)'.dependencies]`) with `cfg` at its use sites, so both crates can build on Windows
      - neither crate's library uses `agterm-zmx` any more: `agterm-app` drops it, `agterm-term` keeps it as a Unix-only
        dev-dependency for the real-zmx test that kills a session
- [x] port the moved functions' tests (login shell per OS, build id, resources, namespace); the frontend's behavior
      unchanged
- [x] run gates and the full conformance run in the background

### Task 13: `agterm-pty`: herdr's PTY actor over the vendored `portable-pty`

**Files:**
- Create: `crates/agterm-pty/` (`src/{lib,actor,unix,windows}.rs`, tests), `vendor/portable-pty/`,
  `build/patches/portable-pty/`, `scripts/vendor-portable-pty.sh`
- Modify: `Cargo.toml`, `Cargo.lock`, `.github/workflows/cross-platform.yml` (`PACKAGES`), README's vendored-source
  checks

- [x] vendor `portable-pty` the way `vendor-gpui.sh` vendors gpui (`vendor/` + `build/patches/portable-pty/` + a
      `--check` in the gates): herdr's patches 0003 and 0004, 0001 reduced to kernel32's ConPTY only, and agterm's
      patch exporting and importing a ConPTY master's handles (for Task 21)
      - renumbered 0001-0003 (herdr's 0001, 0003, 0004), plus agterm's 0004 (the child created in its job through
        `PROC_THREAD_ATTRIBUTE_JOB_LIST`, which the job bullet below needs: assigning after `CreateProcessW` races
        the child's own spawns) and 0005 (`take_pipes`, unsafe `console_handles`, `from_handoff` with a rebuilt HPCON
        that never reaches `ClosePseudoConsole`); the Windows job runs the patches' own tests
- [x] port herdr's PTY actor: poll + wake pipe on Unix (`pipe` + `fcntl` where `pipe2` is missing, as on macOS),
      reader and writer threads on Windows, quiesce/release for the handoff, close-on-exec master duplication; std
      channels
      - `src/unix/{mod,fd,actor}.rs` and `src/windows/{mod,actor}.rs` in place of single files; input and replies in
        one byte-bounded outbox (`agterm-term`'s backpressure rule) in place of herdr's command channel; a quiesced
        actor neither reads nor writes; resizes on the caller's thread (ordered before later input); on Windows a
        quiesce cancels the blocked read (`CancelSynchronousIo`), a read completed meanwhile is held and delivered on
        rollback or returned by the release (`Released::pending_output`), the release leaves the console open;
        `child_exited` closes the pseudoconsole, which never ends by itself; release needs a quiesced actor
- [x] spawn with argv, environment and cwd; resize; exit status; on Windows each child tree in a `KILL_ON_JOB_CLOSE`
      job, on Unix a process group
      - Unix waits with `waitid(WNOWAIT)` before reaping, so `Killer` never signals a group whose id was released
- [x] tests on all three OSes: echo round trip, resize reaching the child, kill ending the child and its tree, exit
      codes, an environment and cwd as given, quiesce and release
- [x] run gates

### Task 14: `agterm-term` on macOS and Windows

**Files:**
- Modify: `crates/agterm-term/src/{pty,pane,launch,exit,password_input,…}.rs`, its tests,
  `crates/agterm-term/Cargo.toml`, `.github/workflows/cross-platform.yml` (`PACKAGES` += `agterm-term`)

- [x] plain panes over `agterm-pty` on all three OSes, replacing `pty.rs`'s own wake-pipe loop
      - the actor's read callback only copies into a pane mailbox (`mailbox.rs`: output, clipboard answers, child
        exit, end, shutdown under one lock); the IO worker feeds the VT from it and parks on clipboard reads there,
        with the synchronized-output hold's deadline as its timeout. Flow control is the producer's: past 1 MiB unfed
        the callback pauses the actor's reads (new `agterm_pty::ReadPause`, `ActorHandle::wake_reader`), the worker
        resumes them at 256 KiB, and the actor keeps writing input meanwhile. Replies go through
        `ActorHandle::write_reply` (safe: the callback never takes the VT lock). `PaneCommand` is `agterm_pty::Command`
        (new `raw_args`); `foreground_pid`/`password_input` read a close-on-exec master duplicate the pane keeps (decided
        with Codex, transcript in the run's `discussions/task-14-windows-forms`). The 50 MiB `cat` bench: 0.45/0.47/0.38 s
        (baseline 0.46/0.49/0.43)
- [x] Windows forms beside each Unix-only spot: the command shell for seed commands (`COMMAND_SHELL`: `cmd.exe /C` on
      Windows, decided and recorded), the login shell (from Task 12), signals (job termination for SIGHUP/SIGKILL),
      `password_input` (console mode instead of `tcgetattr`), the terminfo variables (Task 10)
      - command lines run as `%ComSpec% /D /S /C "<line>"` (no AutoRun; `/S` plus outer quotes keeps the line's own
        quotes), the tail passed verbatim through herdr's raw-tail patch, carried as `build/patches/portable-pty/0006`;
        the login shell runs directly as Ghostty runs it on Windows (a value naming a file whole, else split on
        whitespace), with no POSIX shell integration; `PATH` joins with `;`; the hostname is `GetComputerNameW`'s;
        `Killer::hang_up` (Unix: SIGHUP to the process group; Windows: the job ends) on drop, `kill_tree` on a failed
        start
      - ⚠️ `password_input` is `false` on Windows, and `foreground_pid` `None`: the pseudoconsole gives its parent no
        handle on the console's input buffer (only `AttachConsole`, process-wide, could read it), and has no foreground
        process group (Task 15's job module is where Windows' foreground job comes from)
      - macOS: Apple's `/bin/bash` is not detected for the bash integration, as Ghostty's `detectShell` skips it
        (`agterm-core::live::shell_integration`)
- [x] tests on all three OSes: launch, echo round trip, resize, exit codes, selection and search on a real PTY, the
      password-input detection; tests that need a Unix tool get a Windows form or are gated with the reason
      - `tests/platform.rs` on all three; the other test files and the unit tests driving POSIX scripts are
        `cfg(unix)` (byte-exact VT checks that Windows' pseudoconsole re-renders), `/proc` checks Linux-only, the
        bench Linux-only; `agterm-pty` gains a pause test on all three
- [x] run gates (full conformance run in the background); `cross-platform.yml` green on all three
      - ratchet holds (667 ids, 14 not run on this host); `cross-platform.yml` run 39207394390 green

### Task 15: Foreground job and process table on all three OSes

**Files:**
- Create: `crates/agterm-session/src/{job,process_table}/{mod,linux,macos,windows}.rs`
- Modify: `crates/agterm-app/src/process_table.rs` (uses `agterm-session`'s), `crates/agterm-app/Cargo.toml`,
  `crates/agterm-zmx/src/sweep.rs` (hangup through the job module), `.github/workflows/cross-platform.yml` (`PACKAGES`
  += `agterm-app` if it builds on all three after Task 12; otherwise the reason recorded)

- [ ] the `job` module: Linux `/proc` process groups, macOS `sysctl`, Windows herdr's snapshot (descendants, command
      lines, terminate); `sweep.rs`'s hangup moves behind it
- [ ] `ProcessTable` for all three OSes beside it, with the Linux one's lossy-decode rule; `agterm-app`'s quit-time
      capture uses it
- [ ] tests on every OS: a foreground job found and ended; an unreadable table never read as "no job"; a captured
      argv with spaces and quotes
- [ ] run gates

### Task 16: Pane runtime: spawn, VT state, fan-out, exit

**Files:**
- Create: `crates/agterm-session/src/server/{pane,clients}.rs`, `crates/agterm-session/tests/pane.rs`
- Modify: `crates/agterm-session/Cargo.toml` (`agterm-ghostty-vt`, `agterm-pty`)

- [x] port the parts of herdr's per-pane runtime Task 2 kept (`pane.rs`, `terminal/runtime.rs`,
      `runtime_registry.rs`) over `agterm-pty`, with the session-creation inputs and child environment from Technical
      Details
      - `server::pane::Pane` is herdr's runtime facade and pane runtime in one; `server::pane::Panes` the registry by
        session name, which forgets a session once its program ended. `SessionSpec.env` is the program's whole
        environment, as zmx's first attach client gave it (`agterm-pty`'s `Command` gains `env_clear` for it), with
        `AGTERM_LIVE_SESSION` and `AGTERM_LIVE_DIR` set, `AGTERM_LIVE_MANAGED{,_CLAIM}` removed and `TERM` set to zmx's
        `xterm-256color` when unset or `dumb`; without a `cwd` the program starts in that environment's home
        directory. The creating client is attached before the program runs (`Pane::spawn` returns its attachment),
        as an attach that creates its session is. A kill or a dropped pane hangs the process group up (on Windows
        ends the job), then kills it after herdr's 250 ms grace (a kill without `force` stops at the hangup)
- [x] VT state in `agterm-ghostty-vt` fed from the PTY, with its image media off; raw output fanned out to attached
      clients over byte-bounded lossless queues that never block the PTY reader
      - `server::clients`: one queue per client, bounded in output bytes; output that does not fit is never dropped
        silently: the queue keeps what it has and ends with `Overflowed { offset }` (then only the exit), for Task 18's
        resync. The terminal's `write_pty` replies go back to the program (Task 20 adds the rest of the replies)
- [x] child exit: the session ends and its clients get the exit as `ExitPolicy` expects
      - after the PTY's end (within 1 s, `agterm-term`'s grace), every client gets `Exit` with the program's code and
        signal, after all of its output, and is detached; the runtime `ExitPolicy` weighs is the client's, as today
- [x] tests (all three OSes): echo round trip through a client; resize reaching the child; kill ending the child and
      its tree; a session's environment and cwd as given; two clients receiving the same bytes; the PTY reader never
      blocked by a stalled client
- [x] run gates
      - `cross-platform.yml` green on all three (run 39210421250, branch `ci/cps-task-16`, deleted after)

### Task 17: Leadership

**Files:**
- Create: `crates/agterm-session/src/server/lead.rs`, `crates/agterm-session/tests/lead.rs`

- [ ] size ownership, role changes and unmanaged clients with today's explicit-leadership semantics (Technical
      Details), herdr's resize ownership adapted to them
- [ ] the role as the attach contract's title format, sent on change
- [ ] tests: two clients with the lead passing between them; a follower's resize ignored; an unmanaged client; the
      lead leaving on detach
- [ ] run gates

### Task 18: Attach, snapshot and resync

**Files:**
- Create: `crates/agterm-session/src/server/snapshot.rs`, `crates/agterm-session/tests/snapshot.rs`

- [ ] one serialization point per pane; `SnapshotBegin{epoch, offset, size}`, bounded chunks, `SnapshotEnd`,
      offset-tagged output
- [ ] the formatter snapshot with extras and the continuation (Task 11), with its byte cap and unavailable outcome
      (bounded wait, then a harmless failure), matching what the suite pins (Context), unstyled blanks included
- [ ] in-band resync on queue overflow with a recovery budget, then disconnect; replies of a superseded epoch dropped
- [ ] tests: reattach showing the scrollback, then the screen with the cursor in place; an alt-screen app; color
      overrides kept; DECSET 2026 not replayed; blank runs unstyled; a cut mid-sequence restored; a slow client
      resynced in band keeping its role, and one past the budget disconnected
- [ ] run gates

### Task 19: Server API: what `SessionHost` and `clipboard set` need

**Files:**
- Create: `crates/agterm-session/src/server/api.rs`, `crates/agterm-session/tests/api.rs`

- [ ] port herdr's socket API shape (`src/api/`) for exactly the operations in Technical Details > API
- [ ] list with each `LiveSessionRecord` field; kill (one, batch, `force`) with every `KillOutcome` and the
      `StaleSocket` mapping
- [ ] screen (`active`/`all`) from the server's VT with `revision`, cursor and `alternate`; type (encoded for the
      keyboard mode, without taking the lead); print (an OSC 52 write into the output)
- [ ] tests for each operation's success and error paths: an unknown session, a dead one, a batch with both, and a
      `revision` that moves only when the screen does
- [ ] run gates

### Task 20: Terminal replies

**Files:**
- Create: `crates/agterm-session/src/server/replies.rs`, `crates/agterm-session/tests/replies.rs`

- [ ] the server VT answers state and capability queries through libghostty-vt's effects; herdr's
      `pane/{osc,xtgettcap,kitty_keyboard}.rs` checked against the C API, porting only what the library leaves to the
      embedder
- [ ] frontend-dependent replies from the reply owner's metadata (theme colours, cell size), with stable defaults
- [ ] OSC 52 reads as Technical Details says: scan and stop, the output up to that point delivered, a single-recipient
      request with a deadline, denied with no owner or on timeout, parsing resumed
- [ ] Kitty graphics: APC G answered by the reply owner's VT, the server VT's media off
- [ ] on Windows, record per supported version which queries conhost answers itself and which reach the output pipe
- [ ] tests: one reply per query with two clients attached and with none; theme colours from metadata and defaults;
      an OSC 52 read granted, denied and timed out, with output flowing meanwhile; a `t=t` image shown in an attached
      pane with exactly one acknowledgement
- [ ] run gates

### Task 21: Update handoff without ending sessions

**Files:**
- Create: `crates/agterm-session/src/server/handoff/{mod,unix,windows}.rs`, `crates/agterm-session/tests/handoff.rs`

- [ ] port herdr's handoff on Unix: the manifest, the `SCM_RIGHTS` batches, the per-pane runtime state, the commit
      and its timeouts
- [ ] the VT state passes as the binary snapshot when both servers run the same pin, else as the formatter snapshot
      replayed (evaluated, recorded); a pin change the formatter cannot carry offers the reset
- [ ] Windows, per Task 2's conditional GO and its guards (Technical Details); a failed gate keeps the old server
      serving and offers the reset
- [ ] after a Unix handoff the shell's exit reports no status (`ChildExitReason::Handoff`); `ExitPolicy` and
      `agterm-term/src/exit.rs` treat it as a plain exit
- [ ] the build id: a hash of the staged binary; a client finding a server of another build starts the handoff, and a
      failed handoff falls back to `build_record.rs`'s reset offer
- [ ] tests on every OS: a handoff keeps the shell, its screen and its scrollback; a client attached before reattaches
      after; a failed handoff leaves the old server serving; (Windows) replacing the staged binary while the server
      runs succeeds; (Windows) an unknown layout refuses before any dereference; continuous output across a handoff
      loses no byte
- [ ] run gates

### Task 22: CLI binary with the kept attach contract

**Files:**
- Modify: `crates/agterm-session/src/bin/agterm-session.rs`
- Create: `crates/agterm-session/src/{cli,attach_tty}.rs`, `crates/agterm-session/tests/cli.rs`

- [ ] subcommands: `server <socket dir>` (exists), `attach`, `list`, `kill`, `screen`, `type`, `print`, `version`
- [ ] `attach` honors the attach contract: the `AGTERM_LIVE_*` variables (empty means unset), create-only with the
      gone guard, the detach key unless `AGTERM_LIVE_NO_DETACH_KEY`, the managed-claim opt-in, the role title
- [ ] `attach` as a terminal client: raw mode, resize forwarding; on Windows, console VT input as herdr's
      `client/input/windows_vti.rs` reads it
- [ ] `attach` as a full reply owner: it queries its outer terminal for the theme colours and cell size and sends them
      as metadata, and relays delegated OSC 52 reads to the outer terminal and back; the server-owned queries are
      filtered out of its output, fragment-safe across reads
- [ ] tests: each subcommand's output and exit codes; `attach` round trip in a PTY, detach and reattach; the gone
      guard; the role title on a lead change; an inherited `AGTERM_LIVE_SESSION` ignored when set empty; split query
      sequences with two attached CLIs; OSC 10/11 answered with the outer terminal's colours; an OSC 52 read relayed
- [ ] run gates

### Task 23: Client library and `SessionHost` (library only)

**Files:**
- Create: `crates/agterm-session/src/{client,host}.rs`, `crates/agterm-session/tests/host.rs`

- [ ] client library: the API and an attach stream (snapshot, then Output/Input/Resize/Role, reply metadata and
      delegated requests)
- [ ] `SessionHost` over the library, with the same error kinds and outcomes as `ZmxClient`: the `HostError` texts in
      live-session wording (listing every core or suite assertion that reads them), `KillOutcome`, never a blank
      screen on a failed read, never an empty namespace from a failed listing
- [ ] port `agterm-zmx`'s host tests onto the new host; the frontend still builds `ZmxClient`
- [ ] run gates

### Task 24: Server e2e on all three OSes

**Files:**
- Create: `crates/agterm-session/tests/e2e.rs`

- [ ] through the binary and the client library: start the server detached from the test, create a session, attach,
      type, read `screen`, detach; kill the test's side, reattach and see the same screen; `kill` and `kill --force`,
      `list`, `print`
- [ ] run on `ubuntu-26.04`, `macos-26` and `windows-2025` (debugged in the VMs), each under a private namespace
- [ ] run gates

### Task 25: Switch live sessions to `agterm-session` (attach subprocess still in use)

**Files:**
- Modify: `crates/agterm-gpui/src/session_host.rs`, `crates/agterm-app/src/restore_launch.rs`,
  `crates/agterm-term/src/launch.rs`, `crates/agterm-core/src/live/{support,lead,build_record}.rs`,
  `crates/agterm-core/src/remote/ssh.rs`, `crates/agterm-ctl/src/clipboard.rs`, `scripts/stage.sh`,
  `conformance/agterm_conformance/live_client.py` (new), `conformance/agterm_conformance/impls/base.py`, the selftests,
  `conformance/tests/{cli/test_cli_only,cli/test_cli_output,protocol/test_zmx,ui/test_live}.py`,
  `crates/agterm-ctl/tests/cli_local.rs`, `crates/agterm-gpui/tests/{privileges,quit}.rs`,
  `crates/agterm-term/tests/launch.rs`, `CLAUDE.md`, `.claude/skills/agterm-upstream-sync/references/safety.md`,
  `conformance/docs/rust-build.md`

- [ ] stage `bin/agterm-session` and `share/agterm/session-BUILD` in place of today's session-host binary and record
- [ ] the attach line and the remote one use the attach contract; the frontend builds the new host, and the GPUI pane
      runs `agterm-session attach` in its PTY
- [ ] `agterm-term`'s attach effects policy for live panes: the pane's VT writes no replies, set before the first byte;
      the CLI attach in the pane's PTY is the reply owner (Task 22), so theme replies and clipboard reads keep working
- [ ] `agtermctl clipboard set` uses the server's print; record which of its error texts change against upstream's
- [ ] the harness's Rust adapter: leftovers are the instance's server (by its namespaced argv) and its sessions'
      processes; the leading client is `live_client.py`; the shared tests take the client and the directory from the
      adapter; selftests follow
- [ ] the capability descriptions and the isolation docs follow the new server and its one-server reap
- [ ] the gpui live process tests take the cargo-built `agterm-session`; their process-tree check follows one server;
      they run, not skip
- [ ] full GPUI conformance run in the background: every ratchet id green; newly green ids added
- [ ] run gates

### Task 26: Live panes in `agterm-term` over the attach stream

**Files:**
- Modify: `crates/agterm-core/src/live/support.rs`, `crates/agterm-term/src/{pane,launch,exit}.rs`,
  `crates/agterm-term/src/bench.rs`, `crates/agterm-term/Cargo.toml`, `crates/agterm-term/tests/launch.rs`
- Create: `crates/agterm-term/src/session_io.rs`

- [ ] core produces the live launch as structured data (session name, argv, environment, cwd); the shell line stays
      for the CLI and remote paths, and tests prove both agree
- [ ] the pane asks the server to create its session and attaches in process: no PTY, no subprocess, the same
      IO-thread batching and VT slicing as the PTY path; a gone or refused session follows today's `ExitPolicy`;
      the snapshot encoding evaluated (binary codec at the same pin, else the formatter) and recorded
- [ ] the pane sends the reply metadata on attach and on change, answers delegated requests (OSC 52 read) as the
      reply owner, and its VT answers Kitty graphics; its other VT replies stay suppressed
- [ ] port `tests/launch.rs`'s live tests to the in-process attach (`every_fresh_live_launch_paints_its_prompt`, the
      session-gone and cannot-reach-socket cases); add: a live pane round trip and resize, pane drop leaving the
      session running, reattach showing the snapshot, the session's environment and cwd as given; on all three OSes
- [ ] `bench.rs` gains the in-process live path; measure against Task 2's baselines and record the numbers here
- [ ] run gates and the full GPUI conformance run in the background

### Task 27: PowerShell live sessions

**Files:**
- Modify: `crates/agterm-core/src/live/{shell_integration,replay}.rs`, `crates/agterm-core/src/restore/mode.rs`
- Create: `resources/shell-integration/pwsh/agterm.ps1` (path settled against the staged Ghostty resources)

- [ ] `ShellKind::Pwsh`: the script (OSC 7, OSC 133, title), launch arguments, the replay script in pwsh
- [ ] eligibility on Windows: `pwsh.exe`, then `powershell.exe`, else the existing rejection with its reason
- [ ] tests: script rendering and quoting; replay of an argv with spaces and quotes; eligibility cases; e2e on the
      Windows runner (debugged in the VM) at the session and core level: a live pwsh session reports its cwd, and its
      foreground command, read through Task 15's table, is replayed by `agterm-core`'s replay into a recreated
      session
- [ ] run gates

### Task 28: Remove zmx

**Files:**
- Delete: `crates/agterm-zmx`, `scripts/build-zmx.sh`, `build/patches/zmx/`
- Modify: `Cargo.toml`, `Cargo.lock`, `crates/{agterm-app,agterm-gpui,agterm-term}/Cargo.toml`, the callers of
  `agterm-zmx`'s re-exports, `crates/agterm-core/src/live/{lifecycle,build_record}.rs`, `build/pins.env` (drop
  `ZMX_*`), `scripts/lib/native.sh` (the zmx prefix), `.github/workflows/rust-linux.yml` (the zmx prefix and its Zig),
  `crates/agterm-ghostty-vt-sys/tests/prefix_keys.rs` (it runs `build-zmx.sh`), `crates/agterm-gpui/tests/
  {privileges,quit}.rs`, `crates/agterm-term/tests/launch.rs`, `crates/agterm-term/src/bench.rs`,
  `crates/agterm-wpe-sys/tests/provider.rs`, `conformance/docs/rust-build.md`, `.claude/skills/agterm-upstream-sync/**`

- [ ] point every caller at `agterm-session` directly and delete the crate, the build script, the pins and patches;
      Zig remains only for libghostty-vt (`LIBGHOSTTY_VT_ZIG_*`)
- [ ] keep upstream's names only for the Swift adapters (`LEFTOVER_NAMES`, `zmx_client.py`) and the scrub of the
      installed agterm's `ZMX_*`
- [ ] upstream-sync skill: an upstream zmx change maps onto `live.*` and the attach contract; a herdr change means
      "port into `agterm-session`"
- [ ] tests: the gpui live process tests and `agterm-term`'s live tests run, not skip; `prefix_keys.rs` keeps the
      Ghostty prefix independent of anything zmx left
- [ ] run gates and the full conformance run

### Task 29: Verify acceptance criteria

- [ ] libghostty-vt builds from the sys crate's build script on Linux, macOS and Windows with networking off once the
      prefix exists; `agterm-ghostty-vt-sys`, `agterm-ghostty-vt`, `agterm-pty`, `agterm-term`, `agterm-session` (and
      `agterm-app` if Task 15 joined it) test green on all three in `cross-platform.yml`
- [ ] every Ghostty patch is listed in `build/patches/ghostty-vt/README.md` with its remove-when condition
- [ ] no `agterm-zmx`, zmx build or `ZMX_*` pin remains; `git grep -i zmx` finds only the allowed places (the Swift
      adapters and `zmx_client.py`, the suite's upstream vocabulary, the upstream-sync tooling's patterns, the
      environment scrub, the deviation record, docs describing upstream or the past)
- [ ] live sessions: on Linux through the app and the suite (create, quit and relaunch, reattach with the screen,
      kill, prune, reset, `clipboard set`, an update handoff); on macOS and Windows CI at the session-server API and
      `agterm-core` level (create, detach, reattach with the screen, print, kill, list, handoff), since `agterm-ctl`
      and the app are not built there yet
- [ ] the remote paths green against `agterm-session attach` (`agterm-core`'s attach-remote and remote tests)
- [ ] performance against Task 2's baselines: the 50 MiB `cat` no slower than through zmx, idle RSS for N panes no
      larger than N idle zmx daemons
- [ ] every gate in README > Test; the full `conformance/tools/run-rust-linux.sh` in the background with the ratchet
      unchanged or grown; `cross-platform.yml` and `rust-linux.yml` green

### Task 30: [Final] Update documentation

- [ ] README: building libghostty-vt per OS (Zig pin, the script, then cargo; Git Bash on Windows; the VM
      prerequisites), the recorded patches, staging with `agterm-session`, the server's lifecycle, Windows and macOS
      notes, CI (`cross-platform.yml`)
- [ ] CLAUDE.md, as rules, not details: crate map (`agterm-ghostty-vt-sys`, `agterm-ghostty-vt`, `agterm-pty`,
      `agterm-term` and `agterm-session` as `any`; `agterm-zmx` gone; the paragraph on the libghostty-vt crates'
      platform); the patch rule with its recorded-exception form; the reference rule names herdr as the source of the
      session server; the isolation rules for the session server per namespace and the Windows pipe
- [ ] `conformance/docs/rust-build.md` and `conformance/README.md`: zmx mentions, the harness's Rust adapter, any
      capability claimed for pwsh live sessions
- [ ] move this plan to `docs/plans/completed/`

## Post-Completion

*Items requiring manual intervention or external systems - no checkboxes, informational only*

**Manual verification**:
- Linux, in a dev instance through `scripts/run-gpui-dev.sh` with a private state dir: live panes across an app quit
  and relaunch and across an update; heavy TUIs (nvim, htop, lazygit) in live panes; Kitty images in a live pane;
  the OSC 52 clipboard and `clipboard set`; throughput on a large `cat` against the installed build.
- The `Win11-admin` VM: live pwsh sessions survive their client ending; a reboot replays captured commands; an update
  with sessions running (handoff, or the documented fallback); a terminal attaches with `agterm-session attach`.
- The `macOS 26` VM (or a real Mac where the VM cannot grant devices): a tool in a live session keeps its microphone
  and camera permission across an update.
- The user's own machine: the installed agterm's zmx sessions are not migrated; they keep running until closed.
- The user-level `agterm` skill (outside this repository) names `agtermctl zmx …`; the Rust build needs
  `agtermctl live …`.

**External and future**:
- The GPUI frontend on Windows and macOS is the next plan: it builds on `agterm-term`, `agterm-pty`, libghostty-vt
  and `agterm-session` from this one, and brings `agterm-app`, `agterm-server`, `agterm-ctl` and the app's own
  capture and replay to Windows and macOS.
- The Rust VT port (`rust-vt-port`) stays available if Zig ever has to go.
- The future TUI frontend ports herdr's surface deltas as a second attach mode; the future headless core builds on the
  session server and its client library.
- A herdr bump ports its session-server changes into `agterm-session` and its libghostty-vt patches through
  `build/patches/ghostty-vt/README.md`; a Ghostty pin bump stays a change of its own (CLAUDE.md), rebuilt on all
  three OSes, and drops each patch whose remove-when condition it meets.
