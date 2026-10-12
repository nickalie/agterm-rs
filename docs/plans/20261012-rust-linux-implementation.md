# Rust agterm, Linux first

## Overview

- Implement agterm in Rust, starting with a Linux build that is a drop-in replacement for the Swift/GTK fork
  (`../agterm-linux`): same wire protocol, `agtermctl`, on-disk formats, control socket path, application id and
  state dir.
- Progress is measured by the conformance suite in `conformance/` (`--impl rust`): parity starts at 0% and grows
  task by task. A ratchet keeps every test that went green from going red again.
- The core stays platform-neutral, so the macOS (objc2/AppKit + libghostty) and Windows frontends can come later
  over the same crates. Both are out of scope for this plan.
- One deliberate extension beyond upstream: live sessions for bash and fish login shells (upstream accepts zsh only),
  under a Rust-only capability.

## Context (from discovery)

- `conformance/`: pytest suite, 654 protocol/cli/behaviors tests collected. The last reference-linux report
  (`conformance/reports/reference-linux/`, suite 919b99d) predates the newest tests; acceptance compares against a
  fresh run.
- Rust side of the contract: `conformance/docs/rust-build.md` (the `version` extension, launch env, SIGTERM within
  10 s, `ZMX_DIR=/tmp/agterm-zmx-<FNV-1a 64 of the resolved state dir>`, `agtermctl` beside `--bin`, capability
  vocabulary). Adapter: `conformance/agterm_conformance/impls/rust.py`.
- `conformance/docs/fork-bugs.md`: contracts the fork breaks. The Rust build implements upstream's contract there,
  not the fork's code (today: `workspace new --collapsed`).
- Catalog `conformance/catalog.yaml` (upstream v0.36.0): 110 commands in 22 groups (`session` alone has 41),
  10 events, 114 CLI subcommands, 14 behaviors. Exempt (no suite test): `remote.opened`, `remote.closed`,
  `zmx.attach`, `zmx.reset`, `debug.appearance`, `session.paste`, `session.overlay.copy`, `cli:session paste`,
  `zmx.tree.remote-host`, presentation fields, `field:result.liveReset`, `field:tree.liveReset`,
  `field:tree.session.remoteHost`.
- Port sources in `../agterm-linux`:
  - `agtermCore/Sources/agtermCore` (~32k lines, host-free): `ControlProtocol.swift`, `ControlResponse.swift`,
    `ControlPayloads.swift`, `ControlProjection.swift`, `ControlResolve.swift`, `ControlDispatcher*.swift`,
    `ControlEvents.swift`, `AppStore*.swift`, `WindowLibrary*.swift`, `Snapshot.swift`, `PersistenceStore.swift`,
    `SettingsStore.swift`, `SurfaceEnvironment.swift`, `Keymap.swift`, `Hooks.swift`, `HookScheduler.swift`,
    `Zmx*.swift`, `RestoreMode.swift`, `CommandRestore.swift`.
  - `agtermCore/Tests/agtermCoreTests`: the `ControlDispatcher*Tests` are the source of exact error strings.
  - `agtermCore/Sources/agtermctlKit`: the CLI (`Commands.swift`, `SocketClient.swift`, per-group `*Commands.swift`).
  - `agterm-linux/Sources/AgtermLinux` (~26k lines GTK4/libadwaita): `App.swift`, `GhosttyApp.swift`,
    `GhosttySurface*.swift` (`GtkGLArea`), `GhosttyResourceResolver.swift`, `ControlServer*.swift`,
    `ControlStreamOwner.swift`, `LinuxControlDispatcher*.swift`, `LinuxZmx*.swift`, `LinuxHtml*.swift` (WebKitGTK),
    `WindowManager.swift`; `agterm-linux/Sources/LinuxIntegrations`.
  - Build: `scripts/setup-linux.sh` (libghostty, Zig 0.15.2, shared `ghostty-internal.so` → `libghostty.so`),
    `scripts/setup-linux-zmx.sh` (zmx, Zig 0.16.0, plus a private checkout of the ghostty revision zmx pins),
    `scripts/verify-linux-resources.sh`, `scripts/stage-linux.sh`.
  - Pins and patches: `linux/ghostty-resources.env`, `linux/zmx.env`, `linux/patches/ghostty-*.patch` (3),
    `scripts/zmx-patches/000{1,2}-*.patch` (2), `scripts/zmx-patches/ghostty/0001-formatter-unstyled-blanks.patch`.
  - `ARCHITECTURE.md`: model, surface ownership, control API split.
- Fork defaults to keep: application id `io.github.melonamin.agterm` (`LinuxAppMetadata.swift`), overridable by
  `AGTERM_APP_ID`; state dir `PersistenceStore.defaultDirectory` (Foundation's application support dir + `agterm`,
  on Linux `$XDG_DATA_HOME/agterm`, else `~/.local/share/agterm`), overridden by `AGTERM_STATE_DIR`
  (`DebugStateDirectory.swift`); config dir for `keymap.conf`/`hooks.conf` from `ConfigPaths.swift`; control socket
  `<state dir>/agterm.sock` (`ControlResolve.socketPath`), owned through `flock` on `<socket>.lock`, with
  `<socket>.unavailable` advertised by a refused second instance (`ControlServer.swift`).
- Harness model for the Rust runner: `conformance/tools/run-reference-linux.sh` (private headless weston + dbus).

## Development Approach

- **testing approach**: Regular (code first, then tests in the same task). The red conformance tests already play the
  role of tests written ahead for each command group.
- Each command-group task follows one loop:
  1. read the Swift implementation and its unit tests for the group (error strings, edge cases, field order); check
     `conformance/docs/fork-bugs.md` and implement upstream's contract where the fork's differs;
  2. implement in `agterm-core` against the `agterm-host` traits, with unit tests on fakes;
  3. wire the GTK side in `agterm-gtk`;
  4. add the group's `agtermctl` subcommands;
  5. run the gate (below) and append newly green ids to `conformance/rust-passing.txt`.
- **Gate of every task from Task 7 on**: a full protocol/cli/behaviors run of `run-rust-linux.sh` in the background
  (an all-green reference run takes about 8 minutes; red tests hit launch and wait timeouts, so early Rust runs are
  slower: measure in Task 7 and restate here), with the ratchet passing. The ratchet appends the green ids; tests of
  the task's group that stay red because they use a later command are left to the runner's list of red ids.
- Idiomatic Rust, not a line-by-line port: one state owner on the GUI main thread, no Observation/actor emulation.
  Port only behavior-significant Swift tests.
- Complete each task fully before the next; small focused changes.
- **CRITICAL: every task MUST include new/updated tests** (cargo unit tests; ratchet entries where the task turns
  conformance tests green).
- **CRITICAL: all tests must pass before starting the next task**: `cargo test`, strict clippy (including the
  complexity limits), the lint report and the ratchet.
- A function over a complexity limit is split, not excused. An exception is a `#[expect(clippy::..., reason = "...")]`
  on that one item, with a reason a reviewer can check. `#[allow]` and module-level `#![allow]`/`#![expect]` are
  refused (clippy for the outer form, `lint-report.sh` for the inner one).
- **CRITICAL: a ratchet test may never go red.** If a later change breaks one, fix it in that task. There are no
  reruns: as the suite README says, protocol, cli and behaviors tests wait on read-back, so a flaky one is a bug in
  the build or the test and is fixed.
- **CRITICAL: update this plan file when scope changes during implementation.**
- Builds, the zig builds and suite runs take minutes: run them with `run_in_background`, never with blocking waits.
- The Rust build uses the fork's app id, state dir and socket. Never launch it by hand without `scripts/run-dev.sh`
  while the fork runs: the launch would be forwarded to the fork, or both would write one `windows.json`. The socket
  ownership lock (Task 3) keeps a stray instance from unlinking the fork's live socket, but not from sharing its
  state.

## Testing Strategy

- **unit**: `cargo test --workspace`. `agterm-core` runs against fake `Surface`, `MainLoop`, `SessionHost` and
  `ProcessLauncher` from `agterm-host` (feature `fakes`), so most of the protocol is testable without a window.
  `cargo test` never needs weston, GL or zig: native artifacts come from the vendored prefix.
- **conformance**: `conformance/tools/run-rust-linux.sh [-- PYTEST_ARGS]` builds the workspace, stages the prefix
  (below), and runs the suite with `--impl rust` on a private headless weston and dbus. Report in
  `conformance/reports/rust-linux/`. Its exit status is the ratchet's, not pytest's: hundreds of tests are red by
  design until the end.
- **ratchet**: `conformance/rust-passing.txt` holds pytest node ids that pass, one per line, verbatim (ids may
  contain spaces, e.g. `test_rename_requires_a_name[   ]`). pytest writes the run's outcomes itself (a harness
  option, `--nodeid-report PATH`, Task 5). `conformance/tools/check_ratchet.py`:
  - a listed id that failed, errored (a teardown error after a passing call included) or was skipped breaks the
    ratchet; the one exception is an `unsupported` skip for the host-dependent capabilities `live-sessions` and
    `live-any-shell` (parsed from the skip reason), which counts as not run;
  - a collection error breaks the run;
  - a listed id not collected is ignored when the run was given node-id or `-k` arguments, and a warning on a full
    run;
  - a `--must-pass` problem recorded in the report breaks the run;
  - it prints newly passing ids not yet listed, also into the CI job summary, so ids that only go green in CI (the
    live tests) can be added.
- **ui layer**: no Linux ui driver exists, so `tests/ui` skips on Linux. UI outside the suite is checked by hand
  against the fork (Tasks 33–36).
- **live sessions**: need a zsh login shell until Task 31. CI runs a job whose runner login shell is zsh (from
  Task 28), and after Task 31 one with fish; the default job's login shell is bash, which then claims
  `live-any-shell` too. `--must-pass live-sessions` is enforced from Task 32, when every live test can be green;
  `--must-pass live-any-shell` from Task 31.
- **lint and complexity**: `cargo clippy --workspace --all-targets --all-features -- -D warnings` with the workspace
  lint table and `clippy.toml` thresholds (see Technical Details). `scripts/lint-report.sh` lists every `#[expect]`
  on a complexity lint with its reason and fails on any inner `#![allow]`/`#![expect]` outside `agterm-ghostty-sys`;
  CI prints the list and its count in the job summary, so the complexity debt stays visible from run to run.

## Progress Tracking

- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- update plan if implementation deviates from original scope
- keep plan in sync with actual work done

## Solution Overview

- Cargo workspace at the repo root, next to `conformance/`:

  | crate | contents | platform |
  |---|---|---|
  | `agterm-proto` | wire types: two-level `Command` (group → command), args, `{ok, result, error}`, read-back fields, events, `tree` projection | any |
  | `agterm-host` | traits `Surface`, `SessionHost`, `MainLoop`, `ProcessLauncher`; fakes behind feature `fakes` | any |
  | `agterm-core` | model, target resolver, command dispatcher, event ring, keymap, hooks scheduler, persistence, settings, pane environment | any |
  | `agterm-server` | AF_UNIX control server, one thread per connection, stream handoff | any |
  | `agterm-ctl` → `agtermctl` | clap CLI, socket resolution, output formatting | any |
  | `agterm-ghostty-sys` | bindgen output only, links the prebuilt `libghostty.so`; not under the workspace lints | Linux, macOS |
  | `agterm-ghostty` | safe wrapper over the sys crate (app, config, surface, input, actions) | Linux, macOS |
  | `agterm-zmx` | `SessionHost` over the patched zmx | Linux, macOS |
  | `agterm-gtk` → `agterm` | GTK4/libadwaita frontend, `Surface` over libghostty in `GtkGLArea`, WebKitGTK | Linux |

- Threads: the GTK main loop owns the main thread and the whole state. Each control connection is read on its own
  thread and sends `(request, reply_tx)` through `MainLoop`. Streaming commands (`events.read`, `zmx.present`,
  `session.overlay.job.run`) hand their socket to a stream owner after the first `ok`.
- The dispatcher lives in `agterm-core`. Only work that needs a live terminal or widget goes through a trait.
- On-disk formats match the fork: `settings.json`, `windows.json` + `windows/<id>.json`, keymap, `hooks.conf`,
  `ghostty.conf`.
- Errors: one `ControlError` enum whose `Display` reproduces the Swift strings verbatim. A panic while handling a
  request becomes `ok:false` plus a log entry. No panic crosses a libghostty callback (`catch_unwind`, then abort with
  a log entry). A surface is freed exactly once, in `Surface::teardown`. Logs go through `tracing` to the state dir,
  with the level from `AGTERM_LOG`.

## Technical Details

- `version` result: `{"app": {"version", "commit"}, "conformance": {"parityLevel": "0.36.0", "capabilities": [...]}}`.
  `tree.app` carries the same `app` object. Capabilities follow the swift-linux column of `rust-build.md`:
  `posix-shell`, `hidden-split-holds-focus` and `capture-reads-command-panes` from Task 7 on. `live-sessions` comes
  in Task 28, only when the password-database login shell is zsh and zmx is staged. `live-any-shell` comes in
  Task 31, when that shell is zsh, bash or fish and zmx is staged.
- Launch contract: bind `AGTERM_CONTROL_SOCKET` (rust-build.md; the Linux fork itself never reads it), else
  `<state dir>/agterm.sock`. Use `AGTERM_APP_ID`, else `io.github.melonamin.agterm`. Honor `AGTERM_STATE_DIR`, `HOME` and the XDG dirs. Load
  `<state>/config/ghostty.conf`. Quit on SIGTERM without a prompt.
- Pane environment as `SurfaceEnvironment.swift`: session panes get `AGTERM_ENABLED`, `AGTERM_SESSION_ID`,
  `AGTERM_SOCKET`, `AGTERM_WINDOW_ID`, `AGTERM_WORKSPACE_ID`, `AGTERM_PANE`, `AGTERM_PANE_ID`, `TERM_PROGRAM`,
  `TERM_PROGRAM_VERSION`; the quick terminal gets its own variant without session and window ids. `AGTERM_SOCKET` is
  `<path>.unavailable` when the instance was refused the socket. `agtermctl --pane-id`, `--pane` forwarding and the
  agent-status hooks rely on it.
- Toolchains: Zig 0.15.2 for libghostty at the fork's `GHOSTTY_REV` (2026-07-09), Zig 0.16.0 for zmx (zmx's
  `minimum_zig_version`); they are not shared. ghostty `main` already requires 0.16.0, so a later pin bump would
  unify both on one Zig; that bump is a dedicated task after this plan (rebasing the three ghostty patches and a
  full suite run), not part of the first build. `build/pins.env` holds both Zig versions with sha256 (taken from
  ziglang.org: the fork records only the 0.16.0 one), `GHOSTTY_REV`, the themes URL and sha, `ZMX_REV`.
- Native build outside cargo: `scripts/build-ghostty.sh` and `scripts/build-zmx.sh` each fill their own vendored
  prefix `${AGTERM_VENDOR:-~/.cache/agterm-rs/vendor}/{ghostty,zmx}-<key>/`. The ghostty key hashes its pins and
  `build/patches/ghostty/`; the zmx key hashes its pins, `build/patches/zmx/` and `build/patches/zmx/ghostty/`, so a
  zmx patch never rebuilds libghostty. Builds never fetch or run zig from cargo, so clippy and rust-analyzer stay
  fast and offline.
- Linking: `agterm-ghostty-sys` declares `links = "ghostty"` and exports `cargo:root=<prefix>`; a missing prefix
  fails the build with "run scripts/build-ghostty.sh". The build scripts of `agterm-ghostty` (for its tests) and of
  the binary crates read `DEP_GHOSTTY_ROOT` and emit rpaths for both `<prefix>/lib` (dev and test builds) and
  `$ORIGIN/../lib` (staged builds); an rpath emitted by the sys crate alone would reach only its own targets.
- Patches: `build/patches/ghostty/` (3, onto `GHOSTTY_REV`), `build/patches/zmx/` (2, onto `ZMX_REV`),
  `build/patches/zmx/ghostty/` (1, onto the private checkout of the ghostty revision zmx's `build.zig.zon` pins).
- One staging layout, used by the runner, `run-dev.sh` and the install check (`scripts/stage.sh`):
  `bin/{agterm,agtermctl,zmx}`, `lib/libghostty.so`, `share/ghostty/{themes,shell-integration}`,
  `share/terminfo` (without it the resolver falls back to `xterm-256color`, as `GhosttyResourceResolver.swift`).
  `stage.sh` stages what exists: `bin/agterm` appears in Task 6, `bin/zmx` in Task 27.
- Ratchet file: one pytest node id per line, verbatim (no stripping or splitting on whitespace), sorted, lines
  starting with `#` are comments.
- `--nodeid-report PATH` (JSON lines): one record per collected id with its merged outcome over setup, call and
  teardown (a teardown error after a passing call is `error`; a module-scoped fixture's teardown error lands on the
  module's last test), skip kind and reason, xfail/xpass; plus records for collection errors and `--must-pass`
  problems. Implemented as a registered class like `MustPass`; refused under `--coverage-gate`.
- WebKitGTK: the fork `dlopen`s its WebKit plugin and starts without WebKitGTK installed; the Rust build links
  `webkit6` under the default `html` feature, so WebKitGTK becomes a runtime dependency. Documented in the README.
- Linter: clippy only (the standard Rust linter, shipped with the toolchain). Workspace `[workspace.lints]`, inherited
  by every crate except `agterm-ghostty-sys` with `[lints] workspace = true`:
  - `clippy::all` and `clippy::pedantic` deny (priority -1);
  - complexity: `cognitive_complexity`, `too_many_lines`, `excessive_nesting`, `too_many_arguments`,
    `type_complexity`, `fn_params_excessive_bools`, `struct_excessive_bools` deny;
  - robustness: `unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`, `dbg_macro` deny;
  - FFI: `undocumented_unsafe_blocks`, `multiple_unsafe_ops_per_block` deny; rust `unsafe_op_in_unsafe_fn` deny;
  - exceptions: `allow_attributes`, `allow_attributes_without_reason` deny, so only a reasoned outer `#[expect]` is
    possible, and a stale one fails the build.
- `clippy.toml` thresholds: `cognitive-complexity-threshold = 15`, `too-many-lines-threshold = 80`,
  `too-many-arguments-threshold = 6`, `excessive-nesting-threshold = 5` (it counts item nesting and closure bodies
  too; calibrated on the GTK code in Task 6, then frozen), `allow-unwrap-in-tests`, `allow-expect-in-tests`,
  `allow-panic-in-tests` = true. Test helpers in `tests/*.rs` and the `fakes` module use `Result`, not unwrap.
  Thresholds are lowered, never raised, after the code settles.
- `Command` is two-level (`Command::Session(SessionCmd)`, …) parsed from the dotted wire name; wire names come from a
  derive (`strum` `EnumString`/`IntoStaticStr`), so no hand-written 110-arm match exists. The top-level match has
  22 arms routing to one handler per group.
- Toolchain: `rust-toolchain.toml` pins an exact release (1.98.1 today) with clippy and rustfmt; bumping it is a
  dedicated change, since new clippy lints under `pedantic` + `-D warnings` can break every gate.
- `agterm-ghostty-sys` sets its own rustc lints (`non_camel_case_types`, `non_upper_case_globals`, `non_snake_case`
  allowed in its `Cargo.toml`); bindgen runs with `--no-layout-tests` and the 2024 edition (`unsafe extern "C"`).
- `rustfmt.toml`: `max_width = 120`, edition 2024.

## What Goes Where

- **Implementation Steps**: code, tests, scripts, CI, docs in this repo.
- **Post-Completion**: manual checks against the fork, CI on GitHub, the macOS and Windows follow-up brainstorms.

## Implementation Steps

### Task 1: Cargo workspace, lint config and the wire protocol crate

**Files:**
- Create: `Cargo.toml`, `rust-toolchain.toml`, `clippy.toml`, `rustfmt.toml`, `.gitignore` (append `target/`)
- Create: `crates/agterm-proto/Cargo.toml`, `crates/agterm-proto/src/{lib,request,response,command,events,version}.rs`
- Create: `crates/agterm-proto/tests/catalog.rs`

- [x] create the workspace (edition 2024) with `rust-toolchain.toml` pinning 1.98.1 with clippy and rustfmt
- [x] add the strict lint table to `[workspace.lints]`, `clippy.toml` thresholds and `rustfmt.toml` (Technical
      Details)
- [x] two-level `Command` with strum-derived wire names, request `{cmd, target?, args?}`, response
      `{ok, result?, error?}` (`ok:false` may carry `result`), `VersionResult` with `app` and `conformance`
- [x] write tests: serde round-trips for request/response shapes, including `ok:false` with `result`
- [x] write tests: the wire names equal `catalog.yaml` `commands` (dev-dependency `serde_norway` reads the YAML)
- [x] manual check (not kept in the tree): a scratch over-limit function fails clippy with the config (done in a scratch workspace: too_many_lines, too_many_arguments, excessive_nesting, unwrap_used and `#[allow]` all fail)
- [x] run `cargo test` and `cargo clippy --workspace --all-targets --all-features -- -D warnings`; both must pass
      before Task 2

### Task 2: Native builds, staging layout and the libghostty crates

**Files:**
- Create: `build/pins.env`, `build/patches/ghostty/*.patch`, `build/patches/zmx/*.patch`,
  `build/patches/zmx/ghostty/*.patch`
- Create: `scripts/build-ghostty.sh`, `scripts/stage.sh`
- Create: `crates/agterm-ghostty-sys/{Cargo.toml,build.rs,src/lib.rs,src/bindings.rs}`
- Create: `crates/agterm-ghostty/{Cargo.toml,build.rs,src/lib.rs,src/config.rs,src/app.rs}`,
  `crates/agterm-ghostty/tests/smoke.rs`

- [x] copy the pins (both Zig versions with sha256, `GHOSTTY_REV`, themes, `ZMX_REV`) and all six patches from the
      fork (ghostty patches numbered `0001`–`0003` in the fork's apply order; the OpenGL hunk got its missing
      trailing context line so `patch --fuzz=0` takes it)
- [x] `build-ghostty.sh`: port `setup-linux.sh` (Zig 0.15.2, patches, shared `libghostty.so`, themes,
      shell-integration, terminfo) into the vendored prefix; verify resources as `verify-linux-resources.sh` (fetches
      the source archive of `GHOSTTY_REV`, not a git checkout, and the pinned Zig tarball when no matching zig is
      installed; `scripts/gen-ghostty-bindings.sh` regenerates the checked-in bindings)
- [x] `stage.sh`: assemble the staging layout from `target/` and the vendored prefix
- [x] `agterm-ghostty-sys`: checked-in bindgen output for the subset the fork uses; `links = "ghostty"`, `build.rs`
      links the prefix and exports its root (Technical Details > Linking); own rustc lints, no workspace lints
- [x] `agterm-ghostty`: safe wrapper for init, config load and read, diagnostics
- [x] write tests: smoke test that inits libghostty, loads a config file and reads a value back
- [x] write tests: a missing config file reports diagnostics instead of panicking
- [x] check: without the prefix the build fails with the "run scripts/build-ghostty.sh" message
- [x] run `build-ghostty.sh` (background) and `cargo test -p agterm-ghostty`; must pass before Task 3

### Task 3: Host traits, core skeleton and the control server

**Files:**
- Create: `crates/agterm-host/{Cargo.toml,src/lib.rs,src/fakes.rs}`
- Create: `crates/agterm-core/{Cargo.toml,src/lib.rs,src/dispatch.rs,src/error.rs,src/paths.rs}`
- Create: `crates/agterm-server/{Cargo.toml,src/lib.rs,src/connection.rs,src/stream.rs}`
- Create: `crates/agterm-server/tests/server.rs`

- [x] `agterm-host`: the `Surface`, `SessionHost`, `MainLoop` and `ProcessLauncher` traits, plus in-memory fakes
      (feature `fakes`, no unwrap)
- [x] `agterm-core`: `Core` state owner, `dispatch(request) -> response` routing per group, `ControlError` with
      Swift strings, `version`, the malformed-request error (`protocol.malformed-request`, the fork's text); state
      dir resolution (`AGTERM_STATE_DIR`, else `$XDG_DATA_HOME/agterm`, else `~/.local/share/agterm`) per
      `PersistenceStore.swift` and `DebugStateDirectory.swift`; socket path (`AGTERM_CONTROL_SOCKET`, else
      `<state dir>/agterm.sock`) per `ControlResolve.socketPath`
- [x] `agterm-server`: take `flock(LOCK_EX|LOCK_NB)` on `<socket>.lock` before unlinking and binding, and on refusal
      advertise `<socket>.unavailable` (port `acquireOwnership` from `ControlServer.swift`); refuse paths of
      104 bytes or more; one thread per connection, one JSON line per request, hop to the main loop through
      `MainLoop`, catch panics into `ok:false`; a stream handoff seam (used from Task 12)
- [x] write tests: server round-trip over a temp socket with a fake main loop; several concurrent connections
- [x] write tests: malformed JSON, unknown command, oversized path, a panicking handler
- [x] write tests: state dir with `XDG_DATA_HOME` set and unset, `AGTERM_STATE_DIR` override, socket path fallback;
      a second server on the same path refuses and the first one's socket and lock survive
- [x] run `cargo test`; must pass before Task 4

### Task 4: agtermctl skeleton

**Files:**
- Create: `crates/agterm-ctl/{Cargo.toml,src/main.rs,src/client.rs,src/socket.rs,src/output.rs}`; later tasks add one
  module per command group (`src/<group>.rs`)
- Create: `crates/agterm-ctl/tests/cli.rs`

- [ ] clap command tree skeleton mirroring `agtermctlKit/Commands.swift` (global `--socket`, `--json` where upstream
      has it)
- [ ] socket resolution: `--socket`, then `AGTERM_STATE_DIR`, then the default, ported from `SocketClient.swift` and
      `ControlResolve.swift`; `agtermctl` reads neither `AGTERM_CONTROL_SOCKET` nor `AGTERM_SOCKET`
      (`test_agterm_socket_variable_is_not_read`), so it shares the state-dir code but not the app's socket env
- [ ] `agtermctl version` prints `version (commit)`; exit codes and the error format as upstream
- [ ] write tests: socket resolution order; exit code and stderr on a refused connection
- [ ] write tests: `version` against an in-process server from `agterm-server`
- [ ] run `cargo test`; must pass before Task 5

### Task 5: Rust runner, ratchet, dev launcher, lint report and the cargo CI job

**Files:**
- Create: `conformance/tools/run-rust-linux.sh`, `conformance/tools/check_ratchet.py`, `conformance/rust-passing.txt`
- Modify: `conformance/agterm_conformance/plugin.py` (`--nodeid-report`), `conformance/selftests/test_plugin_options.py`
- Create: `conformance/selftests/test_ratchet.py`, `conformance/selftests/test_run_rust_linux.py`
- Create: `scripts/run-dev.sh`, `scripts/lint-report.sh`
- Create: `.github/workflows/rust-linux.yml`
- Modify: `conformance/README.md`

- [ ] `--nodeid-report PATH` per Technical Details, as a registered class next to `MustPass`
- [ ] `run-rust-linux.sh`: cargo build (release), `stage.sh`, the suite with `--impl rust` under the same
      weston/dbus isolation as `run-reference-linux.sh` (keeping its leftover-instance-root check), report in
      `conformance/reports/rust-linux/`, then `check_ratchet.py`; exit non-zero on a ratchet failure or a leftover
      root, otherwise zero whatever pytest's own status
- [ ] `check_ratchet.py` per Testing Strategy; prints newly passing unlisted ids
- [ ] `scripts/run-dev.sh`: unique `AGTERM_APP_ID`, temp `AGTERM_STATE_DIR` and socket, then exec the staged build
- [ ] `scripts/lint-report.sh`: list complexity `#[expect]`s with file, item, reason and count; fail on inner
      `#![allow`/`#![expect` outside `agterm-ghostty-sys`
- [ ] CI job `rust-linux` (ubuntu-24.04), cargo part: both Zig versions by sha256, GTK4/libadwaita/WebKitGTK `-dev`
      packages, vendored prefix cached by the hash of `build/pins.env` and `build/patches/**`, fmt check, strict
      clippy, lint report in the job summary, `cargo test`
- [ ] write tests: `test_ratchet.py` (pass; failed, errored, teardown-error and skipped listed ids; host-dependent
      unsupported skip allowed, other unsupported skips not; collection error; not-collected on a partial and a full
      run; must-pass problem; ids containing spaces; unlisted new pass; malformed file)
- [ ] write tests: pytester selftests for `--nodeid-report` in `test_plugin_options.py` (phase merge, module
      teardown error, xfail/xpass, collection error, refused under `--coverage-gate`)
- [ ] write tests: `test_run_rust_linux.py` with `fake_app`, like `test_run_reference_linux.py` (exit status follows
      the ratchet, leftover root fails); runs before any `agterm` binary exists
- [ ] run selftests and `cargo test`; must pass before Task 6

### Task 6: A libghostty surface in a GTK window

**Files:**
- Create: `crates/agterm-gtk/{Cargo.toml,src/main.rs,src/ghostty_app.rs,src/surface.rs,src/input.rs}`
- Modify: `crates/agterm-ghostty/src/` (surface, input, actions)

- [ ] `adw::Application` with one window and a `GtkGLArea` hosting a libghostty surface that runs the configured
      command, ported from `GhosttyApp.swift`, `GhosttySurface*.swift`, `GhosttyActionDecoder.swift`
- [ ] keyboard, IME preedit, mouse, scroll, resize, focus and content scale, as the fork with its input-handoff and
      OpenGL patches
- [ ] calibrate `excessive-nesting-threshold` on this code; record the frozen value in Technical Details
- [ ] write tests: unit tests for the key and modifier translation (`ModifierKeyMods.swift`) and action decoding
- [ ] manual check through `run-dev.sh`: the shell renders, takes input, resizes, copy and paste work
- [ ] run `cargo test` and clippy; must pass before Task 7

### Task 7: App shell, server wiring and the GUI CI spike

**Files:**
- Create: `crates/agterm-gtk/src/{app,main_loop,signals}.rs`
- Create: `crates/agterm-core/src/pane_env.rs`
- Modify: `.github/workflows/rust-linux.yml`, `docs/plans/20261012-rust-linux-implementation.md` (measured gate duration)

- [ ] `AGTERM_APP_ID` or `io.github.melonamin.agterm`; `MainLoop` over the GLib main context; the control server on
      the resolved socket
- [ ] SIGTERM via `glib::unix_signal_add_local`: tear down the surfaces and quit without a prompt within 10 s
- [ ] `version` with `parityLevel: "0.36.0"` and the Linux capabilities (Technical Details)
- [ ] the pure pane-environment builder per `SurfaceEnvironment.swift` (session and quick-terminal variants, full
      variable set, `.unavailable` socket); the app's single surface gets the session variant. Wiring per surface
      kind follows in Tasks 11 (primary), 16 (split, scratch) and 17 (quick)
- [ ] CI: weston + dbus + mesa (`libgl1-mesa-dri`, `libegl-mesa0`); the first step is a spike:
      `run-rust-linux.sh -- -k test_version` on the runner. If headless weston's GL backend cannot host libghostty
      there, stop with ⚠️ and settle the display stack (xvfb + openbox, as the fork's CI) before going on. The spike
      also opens a WebKitGTK view, to learn early whether Ubuntu 24.04's AppArmor userns restriction breaks WebKit's
      sandbox on the runner
- [ ] CI: a full-suite job (`run-rust-linux.sh`, protocol/cli/behaviors) after the spike; the node-id report is
      uploaded as an artifact and new green ids are printed in the job summary
- [ ] write tests: the builder for every variant and optional field; socket path resolution in the app
- [ ] measure the full-run duration and restate it in Development Approach
- [ ] gate: full run in the background; `test_version.py` green except the `tree.app` test (⚠️ Task 10),
      `cli/test_socket_resolution.py` tests that need only `version` green; ratchet updated

### Task 8: Model

**Files:**
- Create: `crates/agterm-core/src/model/{mod,window_library,store,workspace,session,pane,naming}.rs`

- [ ] `WindowLibrary` → per-window `AppStore` → workspaces → sessions → panes, ported from `WindowLibrary*.swift`,
      `AppStore.swift`, `Workspace.swift`, `Session.swift`, `SessionNaming.swift`, `AppStore+Naming.swift`
- [ ] selection, recency (`RecencyStack.swift`, `AppStore+Recency.swift`), new-session placement
      (`AppStore+NewSessionPlacement.swift`), close reselection (`AppStore+CloseReselection.swift`)
- [ ] write tests: tree mutations, naming and display names (basename pins), placement
- [ ] write tests: reselection after close, recency order
- [ ] gate: full run; ratchet; must pass before Task 9

### Task 9: Persistence and settings

**Files:**
- Create: `crates/agterm-core/src/persist/{mod,snapshot,settings}.rs`
- Create: `crates/agterm-core/tests/fixtures/` (snapshots and settings written by the fork)

- [ ] snapshot JSON identical to `Snapshot.swift`: `windows.json` index + `windows/<id>.json`, migration and recovery
      (legacy, corrupt, missing) as `WindowLibrary` init; debounced save through `MainLoop`; no save on a cwd report
- [ ] `settings.json` read/write as `SettingsStore.swift`/`AppSettings.swift`, including seeded settings from the
      harness
- [ ] write tests: load fixtures produced by the fork; save → load round-trip is lossless
- [ ] write tests: corrupt index, missing window file, unknown fields handled as the fork does
- [ ] gate: full run; ratchet; must pass before Task 10

### Task 10: Tree projection and target resolution

**Files:**
- Create: `crates/agterm-core/src/{projection,resolve}.rs`
- Modify: `crates/agterm-ctl/src/main.rs`

- [ ] `tree` projection per `ControlProjection.swift` (field names, order and optionality as the catalog `fields`),
      `tree.app`
- [ ] target resolution per `ControlResolve.swift` and `LinuxControlDispatcher*.swift`: frontmost scoping, id and
      prefix matching, the pinned error strings (`protocol.target-resolution`)
- [ ] `agtermctl tree` and the global target options
- [ ] write tests: projection for multi-window and split layouts against expected JSON
- [ ] write tests: resolver ambiguity, unknown id, cross-window targets
- [ ] gate: full run; the `test_tree.py`/`test_addressing.py` tests that need only tree and resolution green, the
      rest ⚠️ until Tasks 11–15; ratchet

### Task 11: Session lifecycle, eager deck and `window.list`

**Files:**
- Create: `crates/agterm-core/src/commands/{session,window}.rs`, `crates/agterm-gtk/src/{deck,sidebar}.rs`
- Create: `crates/agterm-ctl/src/{session,window}.rs`

- [ ] session.new, duplicate, close (soft close, recently closed), select, go, rename, reveal, move from
      `ControlDispatcher*.swift` and the `AppStore+*` files they call (`AppStore+Duplicate`, `+PendingClose`,
      `RecentClosed.swift`)
- [ ] window.list; the eager deck of the fork: every session's surface stays mounted, visibility flips on select,
      mount state readable for the harness's `wait_mounted`
- [ ] pane environment wired into primary panes
- [ ] a minimal sidebar of workspaces and sessions (full parity in Task 22)
- [ ] matching `agtermctl` subcommands (`SessionCommands.swift`, `WindowCommands.swift` list)
- [ ] write tests: lifecycle and reselection on the fake surface; duplicate; recently closed and undo
- [ ] write tests: dispatcher errors for these commands from the Swift tests
- [ ] gate: full run; ratchet the green ids of `test_session.py`, `test_tree.py`, `test_addressing.py`,
      `cli/test_cli_output.py`

### Task 12: Event ring and `events.read`

**Files:**
- Create: `crates/agterm-core/src/events.rs`
- Modify: `crates/agterm-server/src/stream.rs`
- Create: `crates/agterm-ctl/src/events.rs`

- [ ] event ring per `ControlEvents.swift` and `AppStore+Events.swift`: the 10 event kinds, cursors, the anchor in
      `result.events` on a cursor failure
- [ ] `events.read`: without a cursor it subscribes from now with no history; stream handoff after the first ok
- [ ] `agtermctl events` subcommands (`EventCommands.swift`)
- [ ] write tests: ring overflow, stale cursor, subscription delivery order
- [ ] write tests: a stream connection survives other requests and closes cleanly on client hang-up
- [ ] gate: full run; ratchet (`test_events.py`)

### Task 13: `workspace.*`

**Files:**
- Create: `crates/agterm-core/src/commands/workspace.rs`
- Modify: `crates/agterm-gtk/src/sidebar.rs`
- Create: `crates/agterm-ctl/src/workspace.rs`

- [ ] the 10 workspace commands (new, rename, delete, select, go, move, focus, filter, collapse, expand) from
      `ControlDispatcher.swift`, `AppStore+Focus.swift`, `AppStore+Sidebar*.swift` and
      `ControlDispatcherWorkspaceTests`
- [ ] `workspace new --collapsed` per upstream (`fork-bugs.md`): it never takes the active workspace and opts out of
      focus membership
- [ ] `agtermctl workspace ...` (`WorkspaceCommands.swift`)
- [ ] write tests: each command's success path and its errors (strings from the Swift tests)
- [ ] write tests: the background create keeps the current workspace
- [ ] gate: full run; ratchet (`test_workspace.py`, including `test_background_create_keeps_the_current_workspace`)

### Task 14: The rest of `window.*`

**Files:**
- Modify: `crates/agterm-core/src/commands/window.rs`, `crates/agterm-ctl/src/window.rs`
- Create: `crates/agterm-gtk/src/windows.rs`

- [ ] window.new, select, go, close, rename, delete, resize, move, zoom, fullscreen, minimize with the fork's Linux
      behavior (`WindowManager.swift`, `ControlActions+AppControllerWindows.swift`); no claims of `window-geometry`,
      `window-minimize`, `select-takes-frontmost`
- [ ] multiple GTK windows, each over its own `AppStore`; close and delete persistence
- [ ] `agtermctl window ...`
- [ ] write tests: window lifecycle and index persistence
- [ ] write tests: error paths (last window, unknown id)
- [ ] gate: full run; ratchet (`test_window.py`)

### Task 15: Terminal I/O and session metadata

**Files:**
- Modify: `crates/agterm-gtk/src/surface.rs`, `crates/agterm-core/src/commands/session.rs`
- Create: `crates/agterm-core/src/commands/surface.rs`

- [ ] session.type (`KeystrokeSegments.swift`), text (`TerminalText.swift`), copy, paste, selectall, search;
      surface.cursor, surface.zoom (`TerminalZoom.swift`)
- [ ] session.status, flag, context, seen (`AppStore+Status.swift`, `SessionContext.swift`, `AgentStatus.swift`)
- [ ] `Surface` over libghostty: `read_text`, inject, selection, foreground pid; clipboard prompt policy
      (`ClipboardPromptPolicy.swift`)
- [ ] matching `agtermctl` subcommands (`SessionMetadataCommands.swift`)
- [ ] write tests: keystroke segmentation, text read-back, status/flag/context read-back on the fake surface
- [ ] write tests: errors (NUL text per the Linux reference, unknown pane, `ControlDispatcherSessionMetadataTests`)
- [ ] gate: full run; ratchet (`test_session_text.py`, `test_surface.py`, `test_session.py`); scratch and split users
      ⚠️ until Task 16, quick, overlay and HUD users ⚠️ until Tasks 17–20

### Task 16: Splits and the scratch terminal

**Files:**
- Create: `crates/agterm-core/src/commands/panes.rs`, `crates/agterm-gtk/src/split.rs`

- [ ] session.split, split.close, swap, focus, resize, lead, scratch from `ControlDispatcher+Panes.swift`,
      `AppStore+Panes.swift`; GTK `SplitPaneLayout.swift`, `SplitRatioRestoreCoordinator.swift`,
      `AppControllerPaneSwap.swift`
- [ ] `hidden-split-holds-focus` as the fork; `pane.split` and `pane.scratch` events
- [ ] pane environment wired into split and scratch panes
- [ ] matching `agtermctl` subcommands (`Session+Swap.swift`, `Session+Lead.swift`)
- [ ] write tests: split tree mutations, ratio restore, swap
- [ ] write tests: focus on a hidden split; close of the last pane
- [ ] gate: full run; ratchet (`test_split.py` and newly green tests elsewhere)

### Task 17: Restart, background, restore, quick terminal, dashboard and auto-follow

**Files:**
- Create: `crates/agterm-core/src/commands/{quick,dashboard,restart}.rs`, `crates/agterm-core/src/auto_follow.rs`
- Modify: `crates/agterm-core/src/commands/session.rs` (background, restore)
- Create: `crates/agterm-gtk/src/{quick,dashboard}.rs`

- [ ] session.restart (`RestartReplay.swift`, `ControlActions+Restart.swift`), session.background
      (`PaneBackgrounds.swift`), session.restore
- [ ] quick, quick.type, quick.text (`QuickTerminalMetrics.swift`, `AppControllerQuickTerminal.swift`); dashboard
      (`Dashboard*.swift`, `ControlDispatcher+Dashboard.swift`)
- [ ] auto-follow (`AppStore+AutoFollow.swift`, `LinuxAutoFollowCoordinator.swift`) behind `autoFollowMs`
- [ ] pane environment wired into the quick terminal (quick variant)
- [ ] matching `agtermctl` subcommands
- [ ] write tests: restart replay decisions, dashboard layout and targets, auto-follow decisions
- [ ] write tests: quick.text `--lines`/`--all` validation as the Linux reference
- [ ] gate: full run; ratchet (`test_dashboard_quick.py` and newly green tests elsewhere)

### Task 18: HUD and notifications

**Files:**
- Create: `crates/agterm-core/src/commands/{hud,notify}.rs`, `crates/agterm-gtk/src/{hud,notifications}.rs`

- [ ] session.hud.open, update, close (`Hud.swift`, `HudMarkdown*.swift`, `ControlDispatcher+Hud.swift`); the GTK
      HUD (`GhosttySurface+Hud.swift`, `LinuxHudAutoHide.swift`, `LinuxHudLinkClick.swift`)
- [ ] notify (`Notifications.swift`, `NotificationManager.swift`, `LinuxNotificationPolicy.swift`) over the
      instance's own dbus
- [ ] matching `agtermctl` subcommands (`SessionHudCommands.swift`)
- [ ] write tests: HUD markdown parsing and state transitions
- [ ] write tests: notify argument validation
- [ ] gate: full run; ratchet (`test_hud.py`, `test_notify.py`); HUD-over-overlay tests ⚠️ until Task 20

### Task 19: ask and pick

**Files:**
- Create: `crates/agterm-core/src/commands/{ask,pick}.rs`, `crates/agterm-gtk/src/{ask_dialog,picker}.rs`

- [ ] ask.open, result, cancel (`Ask.swift`, `ControlAsk.swift`, `ControlDispatcher+Ask.swift`; GTK
      `AskDialog.swift`, `ControlAsk.swift`)
- [ ] pick.open, result, cancel (`Pick.swift`, `ControlPick.swift`, `Fuzzy.swift`; GTK `ControlPicker.swift`)
- [ ] matching `agtermctl` subcommands (`AskCommands.swift`)
- [ ] write tests: pending-ask rules, fuzzy ranking, result/cancel races
- [ ] write tests: errors from `ControlDispatcherAskTests` and `ControlDispatcherPickTests`
- [ ] gate: full run; ratchet (`test_ask_pick.py`)

### Task 20: Program overlays and overlay jobs

**Files:**
- Create: `crates/agterm-core/src/commands/overlay.rs`, `crates/agterm-gtk/src/overlay.rs`
- Modify: `crates/agterm-server/src/stream.rs`

- [ ] session.overlay.open (program), close, resize, result, submit, copy, text (`ControlDispatcher+Overlay.swift`,
      `OverlayCapture.swift`; GTK `ControlActions+AppControllerOverlay.swift`)
- [ ] session.overlay.job.run with the job-frame stream (`OverlayJobs.swift`, `ControlServer+Streams.swift`)
- [ ] matching `agtermctl` subcommands (`OverlayPageCommands.swift`, `OverlayRunJob.swift`)
- [ ] write tests: overlay slot rules, exit status capture, job claiming
- [ ] write tests: size validation per the Linux reference
- [ ] gate: full run; ratchet (program part of `test_overlay.py`, plus the HUD and surface tests waiting on it)

### Task 21: HTML overlays

**Files:**
- Create: `crates/agterm-core/src/html/{mod,bridge,navigation,outcomes}.rs`, `crates/agterm-gtk/src/html_overlay.rs`

- [ ] `--html` overlays on WebKitGTK through the `webkit6` crate behind the default cargo feature `html`; without it
      the build refuses HTML overlays as the fork does when WebKitGTK is missing (no port of the `AgtermWebKit` C
      shim); with it WebKitGTK is a runtime dependency (Technical Details)
- [ ] `HtmlOverlay.swift`, `HtmlNavigationPolicy`, `HtmlBridge.swift`, `HtmlPageOutcomes.swift`; GTK
      `LinuxHtml*.swift`, `LinuxWebKit.swift`; session.overlay.reload, navigate; the `data-agterm` adapter, `--js`
      helper and `--block` outcomes
- [ ] matching `agtermctl` subcommands
- [ ] write tests: bridge message → request mapping with page defaults; navigation policy
- [ ] write tests: outcome bookkeeping by page id
- [ ] gate: full run; ratchet (rest of `test_overlay.py`)

### Task 22: Sidebar

**Files:**
- Create: `crates/agterm-core/src/commands/sidebar.rs`
- Modify: `crates/agterm-gtk/src/sidebar.rs`

- [ ] sidebar, sidebar.mode, flagged-layout, expand, collapse, width (`AppStore+Sidebar*.swift`, `SidebarMode.swift`,
      `SidebarDrop.swift`; GTK `AppControllerSidebar*.swift`, `LinuxSidebarPolicy.swift`,
      `AppControllerFlaggedLayout.swift`)
- [ ] full sidebar rows: status glyphs, git status, context lines, drag-and-drop reorder as the fork
- [ ] matching `agtermctl` subcommands
- [ ] write tests: mode and width rules, drop placement
- [ ] write tests: errors from `ControlDispatcherSidebarTests`
- [ ] gate: full run; ratchet (`test_sidebar.py`)

### Task 23: Fonts and themes

**Files:**
- Create: `crates/agterm-core/src/commands/{font,theme}.rs`, `crates/agterm-gtk/src/theme.rs`

- [ ] font.inc, dec, reset over libghostty
- [ ] theme.set, theme.list (`ThemeCatalog.swift`, `ThemeName.swift`, `ThemeBrightness.swift`; GTK
      `GhosttyConfigTheme.swift`, `LinuxThemePolicy.swift`) over the staged themes
- [ ] matching `agtermctl` subcommands
- [ ] write tests: theme catalog listing and light/dark slots
- [ ] write tests: unknown theme errors
- [ ] gate: full run; ratchet (`test_font_theme.py`)

### Task 24: Keymap, hooks and custom commands

**Files:**
- Create: `crates/agterm-core/src/{keymap,hooks}/mod.rs`, `crates/agterm-core/src/commands/{keymap,hooks}.rs`
- Create: `crates/agterm-gtk/src/{keymap_dispatch,hook_runner}.rs`

- [ ] config dir for `keymap.conf` and `hooks.conf` per `ConfigPaths.swift` (with the `AppSettings.configDirectory`
      override)
- [ ] keymap model and matcher (`Keymap.swift`, `Keybind*.swift`, `BuiltinAction.swift`, `ControlKeymap.swift`);
      keymap.reload, list, run; GTK `KeymapDispatch.swift`, `LinuxKeyboardPolicy.swift`
- [ ] `hooks.conf` and `HookScheduler` (bounded queue, one child per hook, exit releases a slot); hooks.reload,
      list; `ProcessLauncher` impl (`LinuxHookProcessRunner.swift`)
- [ ] custom commands (`CustomCommand*.swift`, `LinuxCustomCommandProcess.swift`)
- [ ] matching `agtermctl` subcommands
- [ ] write tests: keymap parse/match, scheduler ordering and queue limits (port `HookSchedulerTests`, `HooksTests`)
- [ ] write tests: reload with a broken file keeps the previous config and reports the error
- [ ] gate: full run; ratchet (`test_keymap_hooks.py`, `behaviors/test_keymap.py`, `behaviors/test_hooks.py`)

### Task 25: Config reload, browser and debug

**Files:**
- Create: `crates/agterm-core/src/commands/{config,browser,debug}.rs`

- [ ] config.reload on every surface; browser.clear, browser.links (`BrowserProfile.swift`,
      `ControlDispatcher+Browser.swift`); debug.appearance refused outside UI-test mode
- [ ] matching `agtermctl` subcommands
- [ ] write tests: reload propagation through the fake surfaces
- [ ] write tests: browser profile paths and errors
- [ ] gate: full run; ratchet (`test_config_browser.py`)

### Task 26: Restore modes and relaunch

**Files:**
- Create: `crates/agterm-core/src/restore/{mod,mode,capture}.rs`, `crates/agterm-core/src/commands/restore.rs`

- [ ] restore.mode, restore.capture, restore.clear (`RestoreMode.swift`, `CommandRestore.swift`,
      `AppStore+Restore.swift`); the launch mode frozen for the process, quit capture
- [ ] `none` and `rerun` relaunch: layout, pinned commands, captured commands (`LaunchSpawnPlan.swift`,
      `SpawnPacer.swift`); `capture-reads-command-panes` as the fork (`LinuxProcessCapture.swift`)
- [ ] live requested without zmx: fall back with `unavailableReason` as upstream
- [ ] matching `agtermctl` subcommands
- [ ] write tests: capture argv parsing, shell recognition, denylist
- [ ] write tests: relaunch plans for each mode; live fallback status
- [ ] gate: full run; ratchet (`test_restore.py`, `behaviors/test_relaunch.py` non-live cases)

### Task 27: zmx build and the `SessionHost` crate

**Files:**
- Create: `scripts/build-zmx.sh`
- Create: `crates/agterm-zmx/{Cargo.toml,src/lib.rs,src/launch.rs,src/client.rs,src/inventory.rs}`
- Modify: `scripts/stage.sh`

- [ ] `build-zmx.sh`: port `setup-linux-zmx.sh` (Zig 0.16.0, `ZMX_REV` + its 2 patches, a private checkout of the
      ghostty revision zmx's `build.zig.zon` pins + its patch, both digests in the cache key) into the vendored
      prefix; `stage.sh` adds `bin/zmx`
- [ ] `SessionHost` over zmx: socket dir `/tmp/agterm-zmx-<FNV-1a 64>`, daemon names, wrap configuration and replay
      script (`ZmxSupport.swift`, `ZmxReplayScript.swift`), inventory and lifecycle (`ZmxInventory.swift`,
      `ZmxLifecycle.swift`, `LiveReset.swift`)
- [ ] lead protocol (`ZmxLead.swift`), `zmx screen`/`zmx type` replies
- [ ] write tests: FNV-1a dir against values computed by the fork; daemon name round-trip
- [ ] write tests: wrap rejections (non-absolute path, missing binary, non-zsh shell, missing integration)
- [ ] run `cargo test -p agterm-zmx`; gate: full run; ratchet

### Task 28: Live sessions in the app

**Files:**
- Modify: `crates/agterm-gtk/src/surface.rs`
- Create: `crates/agterm-core/src/commands/zmx.rs`, `crates/agterm-gtk/src/{zmx_runtime,pane_lead}.rs`
- Modify: `.github/workflows/rust-linux.yml`

- [ ] live launch of primary and split panes through `SessionHost` (`LinuxZmxLaunch.swift`, `LinuxZmxRuntime.swift`),
      reattach on relaunch, reaping on mode switch
- [ ] zmx.list, prune, kill, screen; zmx.reset refused as unsupported, as the fork; covered panes answered from the
      daemon (`ControlActions+PaneLead.swift`, `LinuxPaneLead.swift`, `LinuxPaneLeadCover.swift`)
- [ ] claim `live-sessions` only for a zsh password-database shell with zmx staged
- [ ] matching `agtermctl zmx ...` subcommands (`ZmxCommands.swift`)
- [ ] CI: a live job whose runner login shell is zsh (`sudo apt-get install -y zsh`, then
      `sudo chsh -s "$(command -v zsh)" "$USER"`; the harness reads the password database), gated by the ratchet
      only: `--must-pass live-sessions` waits for Task 32, since `zmx.present` (Task 29) and `clipboard set`
      (Task 32) tests also require the capability
- [ ] write tests: launch disposition (ordinary, wrapped, fallback) and reaping decisions
- [ ] write tests: the stop leaves no process with the instance's `ZMX_DIR`
- [ ] gate: full run locally (live tests skip here) and the zsh CI job; ratchet (`test_zmx.py` live cases,
      `behaviors/test_relaunch.py` live case)

### Task 29: Remote sessions, origin side

**Files:**
- Create: `crates/agterm-core/src/remote/{mod,tree,frames,hub}.rs`

- [ ] zmx.tree (`RemoteTree.swift`, `Remotes.swift`), the `zmx.present` stream and its origin hub
      (`PresentationFrames.swift`, `PresentationHub.swift`, `PresentationLayout.swift`, `PresenterGrant.swift`)
- [ ] `agtermctl zmx tree` and the `agtermctl zmx present` stdio bridge (`StreamBridge.swift`)
- [ ] write tests: frame codec, hub subscribe-before-snapshot, generation and revision
- [ ] write tests: tree listing errors
- [ ] gate: full run; ratchet (rest of `test_zmx.py`)

### Task 30: Remote sessions, viewer side

**Files:**
- Create: `crates/agterm-core/src/remote/{client,layout,reconnect}.rs`, `crates/agterm-gtk/src/remote.rs`

- [ ] zmx.attach and remote panes (`RemoteSession.swift`, `RemotePresentationClient.swift`, `RemoteReconnect.swift`,
      `AppStore+Remote*.swift`; GTK `LinuxRemote*.swift`, `AppControllerRemoteAttach.swift`)
- [ ] `remote.opened` and `remote.closed` events (exempt in the catalog)
- [ ] write tests: reconnect backoff, layout application to replicas, remote ask and overlay replicas
- [ ] manual check against a second host (the suite exempts this side)
- [ ] gate: full run; ratchet

### Task 31: Live sessions for bash and fish

**Files:**
- Modify: `crates/agterm-zmx/src/launch.rs`
- Create: `crates/agterm-zmx/src/shell_integration.rs`
- Modify: `conformance/agterm_conformance/impls/base.py`, `conformance/docs/rust-build.md`, `conformance/README.md`,
  `conformance/tests/protocol/test_restore.py`, `.github/workflows/rust-linux.yml`
- Create: `conformance/tests/protocol/test_live_any_shell.py`

- [ ] port the injection from ghostty's `src/termio/shell_integration.zig` into the zmx wrap: bash (`ENV`, `--posix`,
      `GHOSTTY_BASH_INJECT`), fish (`XDG_DATA_DIRS` with the integration's `vendor_conf.d`), zsh unchanged
- [ ] per-shell replay script (fish: `set -gx`, `exec`; bash as zsh)
- [ ] capability `live-any-shell` in `CAPABILITIES` (Rust-only; the references never claim it), its meaning text
      carrying the claim rule (login shell zsh, bash or fish, zmx staged); the `rust-build.md` row follows from it
      with both reference columns empty, as `selftests/test_docs.py` generates; `live-sessions` stays as upstream
- [ ] document Rust-only extension tests in the README's Reference discipline and in `rust-build.md`: they skip as
      unsupported on both references and are validated by the Rust CI with `--must-pass live-any-shell`
- [ ] `test_restore.py::test_seeded_live_mode_is_requested_at_launch`: the fallback branch runs only when the build
      has neither `live-sessions` nor `live-any-shell`; the references are unaffected
- [ ] `test_live_any_shell.py` under `requires("live-any-shell")` with `covers` markers
      (`behavior:restore.live-session-survives-relaunch`, the cwd field): a live pane in the host's login shell
      reports its cwd through OSC 7 and survives a relaunch; the host picks the shell
- [ ] CI: install fish and add a fish-login-shell job next to the zsh one; the default job's bash login shell covers
      bash; jobs on bash and fish run with `--must-pass live-any-shell`
- [ ] reference discipline: `run-reference-linux.sh` stays green with the changed `test_restore.py` and the new file
      (report on the PR), and reference-macos CI passes
- [ ] write tests: unit tests for each shell's environment and replay script
- [ ] gate: full run locally on fish (the new tests run here), the CI jobs, selftests and the coverage gate;
      ratchet

### Task 32: CLI completeness

**Files:**
- Modify: `crates/agterm-ctl/src/*`

- [ ] the CLI-only commands (`clipboard set`, `terminfo install`; `ClipboardCommands.swift`, `TerminfoCommands.swift`)
      and any subcommand still missing from the 114 in the catalog
- [ ] output formats and exit codes still red in `tests/cli`
- [ ] write tests: unit tests for formatting helpers and argument validation
- [ ] write tests: cli-only commands against an in-process server
- [ ] CI: the zsh job gains `--must-pass live-sessions`
- [ ] gate: full run; every `tests/cli` test that passes on reference-linux is in the ratchet

### Task 33: Settings dialog and first run

**Files:**
- Create: `crates/agterm-gtk/src/settings/*.rs`, `crates/agterm-gtk/src/welcome.rs`
- Create: `docs/linux-ui-checklist.md`

- [ ] settings dialog with the fork's pages (`Settings*Page.swift`, `SettingsDialog.swift`), persisting through
      `settings.json`
- [ ] first-run welcome (`FirstRunWelcome.swift`, `WelcomeDialog.swift`), quit prompt (`QuitPrompt.swift`)
- [ ] `docs/linux-ui-checklist.md`: every user-visible GTK feature of the fork, each with a manual check
- [ ] write tests: settings model validation and persistence round-trip
- [ ] write tests: first-run decision logic
- [ ] gate: full run; ratchet

### Task 34: Command palette, session switcher and search

**Files:**
- Create: `crates/agterm-gtk/src/{palette,session_switcher,search}.rs`

- [ ] command palette (`Palette.swift`, `PalettePresentation.swift`, `PaletteCatalog.swift`, `PaletteCustomRow.swift`,
      `PaletteSearchKeys.swift`), session switcher (`LinuxSessionSwitcher.swift`), scrollback search UI (`Search.swift`)
- [ ] write tests: palette catalog and search keys
- [ ] write tests: switcher ordering by recency
- [ ] gate: full run; ratchet

### Task 35: Context menus, popovers and decorations

**Files:**
- Create: `crates/agterm-gtk/src/{context_menus,popovers,watermark}.rs`

- [ ] context menus (`AppControllerContextMenus.swift`), primary menu, attention and recent popovers
      (`AppControllerAttention.swift`), watermark (`WatermarkRenderer.swift`, `WatermarkConfig.swift`), status sounds
- [ ] write tests: menu model per selection state
- [ ] write tests: watermark configuration parsing
- [ ] gate: full run; ratchet

### Task 36: Agent integrations and the checklist walk

**Files:**
- Create: `crates/agterm-gtk/src/integrations/*.rs`

- [ ] agent integrations (`LinuxIntegrations/*`, `AgentHooksInstall.swift`, `SkillInstall.swift`, `CLIInstall.swift`,
      `LinuxAgentIntegrations.swift`)
- [ ] walk `docs/linux-ui-checklist.md` against the fork; fix or record every gap with ⚠️
- [ ] write tests: integration plans and status detection on a temp home
- [ ] write tests: managed-marker idempotence
- [ ] gate: full run; ratchet

### Task 37: Verify acceptance criteria

- [ ] run reference-linux fresh (`run-reference-linux.sh`) and `run-rust-linux.sh`: every test passing on the fork
      passes on rust and is in `rust-passing.txt`; the `fork-bugs.md` tests pass on rust
- [ ] CI green: the cargo job, the full-suite job, the zsh job (`--must-pass live-sessions`) and the bash and fish
      jobs (`--must-pass live-any-shell`)
- [ ] the staged build replaces the installed fork on this machine with the existing state dir
      (`~/.local/share/agterm`) and socket: windows, sessions and settings come back
- [ ] coverage gate and selftests pass: `python -m pytest conformance --coverage-gate`, `pytest conformance/selftests`
- [ ] the lint report shows every complexity exception with a reason

### Task 38: [Final] Update documentation

- [ ] README.md at the repo root: what agterm-rs is, build prerequisites (Rust 1.98.1, Zig 0.15.2 and 0.16.0, GTK4,
      libadwaita, WebKitGTK as a runtime dependency, weston for the suite), build, run and test commands
- [ ] `conformance/README.md`: the Rust runner, the node-id report and the ratchet
- [ ] CLAUDE.md with the crate map, the command-group porting loop, the fork-bugs rule and the fork-coexistence rule
- [ ] move this plan to `docs/plans/completed/`

## Post-Completion

*Items requiring manual intervention or external systems - no checkboxes, informational only*

**Manual verification**:
- daily use as a replacement for the fork: input methods, clipboard, HiDPI, multiple monitors, GPU drivers
- live sessions across a real logout and reboot
- remote sessions between two hosts

**External system updates**:
- push to `nickalie/agterm-rs` so the CI jobs run
- bump the libghostty pin to a ghostty that builds with Zig 0.16 (one toolchain for both native builds)
- packaging (Flatpak, distro packages) is a separate plan
- macOS frontend (objc2/AppKit + libghostty) and the Windows decision are separate brainstorms
- offer the bash/fish live-session support upstream
