# Rust agterm, Linux first

## Overview

- Implement agterm in Rust, starting with a Linux build. The parity target is upstream macOS agterm
  (`umputun/agterm` at the catalog's tag, v0.36.0): its wire protocol, command semantics, error strings, `agtermctl`,
  on-disk formats, model and UI, the UI adapted to GTK4/libadwaita idioms where AppKit has no direct equivalent.
- The Linux build also keeps the application id, state dir and control socket of the Swift/GTK fork
  (`../agterm-linux`), so it can replace the fork installed on this machine with the existing state. The fork is a
  reference for how Linux specifics were solved, never a port source (see Context > Reference rule).
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
  not the fork's code (today: `workspace new --collapsed`); this is one case of the Reference rule below.
- Reference rule (user decision, 2026-10-12):
  - upstream is the reference for the wire protocol, command semantics, error strings, `agtermctl` behavior and
    output, on-disk formats, model and core behavior, and the UI. Port from upstream's sources and its Swift unit
    tests (the source of exact strings), never from the fork's;
  - the fork is consulted only for Linux platform glue upstream has no counterpart for: GtkGLArea surface hosting and
    the input handoff, XDG paths, the app id and state dir a Linux build needs, the libghostty Linux build and its
    patches, WebKitGTK. Its choices are adopted only where they serve upstream parity; user-visible behavior follows
    upstream wherever upstream defines it;
  - where upstream and the fork differ, upstream wins. A capability the fork claims is not a target by itself
    (Technical Details lists the Rust target profile).
- Catalog `conformance/catalog.yaml` (upstream v0.36.0): 110 commands in 22 groups (`session` alone has 41),
  10 events, 114 CLI subcommands, 14 behaviors. Exempt (no suite test): `remote.opened`, `remote.closed`,
  `zmx.attach`, `zmx.reset`, `debug.appearance`, `session.paste`, `session.overlay.copy`, `cli:session paste`,
  `zmx.tree.remote-host`, presentation fields, `field:result.liveReset`, `field:tree.liveReset`,
  `field:tree.session.remoteHost`.
- Port sources: upstream, cloned read-only at `.upstream/agterm` (git-ignored through `.git/info/exclude`;
  `git clone --depth 1 --branch v0.36.0 https://github.com/umputun/agterm .upstream/agterm`, commit
  `6cef448a5d63e2640c759a07d41c68ece5012b0a`). Paths below are relative to it:
  - `agtermCore/Sources/agtermCore` (host-free): `ControlProtocol.swift`, `ControlResponse.swift`,
    `ControlPayloads.swift`, `ControlProjection.swift`, `ControlResolve.swift`, `ControlDispatcher*.swift`,
    `ControlActionsDefaults.swift`, `ControlEvents.swift`, `AppStore*.swift`, `WindowLibrary*.swift`,
    `Snapshot.swift`, `PersistenceStore.swift`, `SettingsStore.swift`, `AppSettings.swift`,
    `SurfaceEnvironment.swift`, `Keymap.swift`, `Hooks.swift`, `HookScheduler.swift`, `Zmx*.swift`,
    `RestoreMode.swift`, `CommandRestore.swift`, `HtmlBridge.swift` (`ControlWire.invalidRequestMessage`).
  - `agtermCore/Tests/agtermCoreTests`: the `ControlDispatcher*Tests` and `AppStore*Tests` are the source of exact
    error strings and model edge cases; `agtermCore/Tests/agtermctlKitTests` the CLI's.
  - `agtermCore/Sources/agtermctlKit`: the CLI (`Commands.swift`, `SocketClient.swift`, per-group `*Commands.swift`).
  - `agterm/` (the AppKit app target): `Control/ControlServer*.swift` (socket, app-side effects of every command
    group), `Control/ControlStreamOwner.swift`, `Ghostty/*` (libghostty app, surfaces, input, zmx launch, foreground
    process), `Views/*` (sidebar, deck, palette, switcher, HUD/ask/HTML overlays, quick terminal, settings),
    `AppActions*.swift`, `agtermApp+Menus.swift`, `WindowRegistry.swift`, `WelcomeAlert.swift`,
    `*Installer.swift`; `agtermTests/` (hosted tests, e.g. `ControlServer*Tests`).
  - Behavior docs: `site/commands.html` (every command, argument and read-back), `site/docs.html` (the user guide),
    `.claude/rules/*.md` (per-subsystem contracts: `control-api.md`, `sidebar.md`, `windows.md`, `menu-actions.md`,
    `keymap.md`, `settings.md`, `notifications.md`, `libghostty.md`, `theme-picker.md`), `ARCHITECTURE.md`.
  - zmx: `scripts/setup.sh` (`ZMX_REV`, `GHOSTTY_REV`), `scripts/zmx-patches/` (the same 2 + 1 patches the Rust
    build carries).
  - Layout difference from the fork: the fork keeps upstream's `agtermCore`/`agtermctlKit` almost verbatim (Linux
    `#if` shims only) and replaces the `agterm/` app target with `agterm-linux/Sources/AgtermLinux`; upstream's app
    logic lives in `agterm/Control/ControlServer+*.swift`, `agterm/Ghostty/` and `agterm/Views/`.
- Linux glue reference in `../agterm-linux` (consulted, not ported): `agterm-linux/Sources/AgtermLinux`
  (`GhosttyApp.swift`, `GhosttySurface*.swift` for `GtkGLArea` hosting and input, `GhosttyResourceResolver.swift`,
  `LinuxHtml*.swift`/`LinuxWebKit.swift` for WebKitGTK, `LinuxAppMetadata.swift`); build scripts
  `scripts/setup-linux.sh`, `scripts/setup-linux-zmx.sh`, `scripts/verify-linux-resources.sh`,
  `scripts/stage-linux.sh`; pins and patches `linux/ghostty-resources.env`, `linux/zmx.env`,
  `linux/patches/ghostty-*.patch` (3).
- Linux compatibility to keep, so the build can replace the installed fork: application id
  `io.github.melonamin.agterm` (the fork's `LinuxAppMetadata.swift`), overridable by `AGTERM_APP_ID`; state dir
  `PersistenceStore.defaultDirectory` (Foundation's application support dir + `agterm`, on Linux
  `$XDG_DATA_HOME/agterm`, else `~/.local/share/agterm`), overridden by `AGTERM_STATE_DIR`; config dir for
  `keymap.conf`/`hooks.conf` from `ConfigPaths.swift`; control socket `AGTERM_CONTROL_SOCKET`, else
  `<state dir>/agterm.sock` (`ControlServer.defaultSocketPath`, `ControlResolve.socketPath`), owned through `flock`
  on `<socket>.lock`, with `<socket>.unavailable` advertised by a refused second instance (`ControlServer.swift`).
- Harness model for the Rust runner: `conformance/tools/run-reference-linux.sh` (private headless weston + dbus).

## Development Approach

- **testing approach**: Regular (code first, then tests in the same task). The red conformance tests already play the
  role of tests written ahead for each command group.
- Each command-group task follows one loop:
  1. read upstream's implementation of the group in `.upstream/agterm` (`ControlDispatcher*.swift`, the
     `AppStore+*` files it calls, the app-side `agterm/Control/ControlServer+*.swift`), its unit tests (error
     strings, edge cases, field order), `site/commands.html` and the matching `.claude/rules/*.md`; consult the
     fork only for Linux glue upstream lacks, and implement upstream's contract wherever the fork differs
     (`conformance/docs/fork-bugs.md` lists the cases the suite already pins);
  2. implement in `agterm-core` against the `agterm-host` traits, with unit tests on fakes; the full upstream error
     string is asserted on the Rust dispatcher's output, dynamic arguments included (the suite asserts only the
     part both Swift references share);
  3. wire the GTK side in `agterm-gtk`, following upstream's UI (`agterm/Views`, `agterm/Ghostty`, the rules files)
     in GTK4/libadwaita idioms;
  4. add the group's `agtermctl` subcommands, ported from upstream's `agtermctlKit`;
  5. claim the group's capabilities from the Rust target profile (Technical Details) once its commands implement
     them;
  6. run the gate (below) and append newly green ids to `conformance/rust-passing.txt`.
- **Gate of every task from Task 7 on**: a full protocol/cli/behaviors run of `run-rust-linux.sh` in the background
  (an all-green reference run takes about 8 minutes. Measured in Task 7: 48 s with the build cached, since a red
  test fails on its first unsupported command within a second rather than waiting out a timeout; the run grows
  toward the reference's 8 minutes as groups go green, so it stays a background run), with the ratchet passing.
  The ratchet appends the green ids; tests of the task's group that stay red because they use a later command are
  left to the runner's list of red ids.
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
- The Rust build uses the fork's app id, state dir and socket (Linux compatibility, not a behavior reference).
  Never launch it by hand without `scripts/run-dev.sh` while the fork runs: the launch would be forwarded to the
  fork, or both would write one `windows.json`. The socket ownership lock (Task 3) keeps a stray instance from
  unlinking the fork's live socket, but not from sharing its state.

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
  against upstream's behavior (`site/docs.html`, the rules files, `tests/ui` and `ui-ids.yaml` as the list of what
  upstream exposes), in GTK idioms (Tasks 33–36).
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
- On-disk formats match upstream (the fork shares them): `settings.json`, `windows.json` + `windows/<id>.json`,
  keymap, `hooks.conf`, `ghostty.conf`.
- Errors: one `ControlError` enum whose `Display` reproduces the Swift strings verbatim. A panic while handling a
  request becomes `ok:false` plus a log entry. No panic crosses a libghostty callback (`catch_unwind`, then abort with
  a log entry). A surface is freed exactly once, in `Surface::teardown`. Logs go through `tracing` to the state dir,
  with the level from `AGTERM_LOG`.

## Technical Details

- `version` result: `{"app": {"version", "commit"}, "conformance": {"parityLevel": "0.36.0", "capabilities": [...]}}`.
  `tree.app` carries the same `app` object.
- Rust target profile (capabilities `version` claims; settled with Codex, see the progress log). The Rust build is
  held to upstream's branch of every capability whose behavior lives in upstream's own code, and each capability is
  claimed by the task that implements its command, never ahead of it:
  - `posix-shell` (Task 7); `status-shape`, `type-rejects-nul` (Task 15); `drop-placement` (Task 11);
    `select-takes-frontmost` (Task 14: upstream's `windowSelect` calls `takeFrontmost` itself, a model change, not
    a compositor focus); `resize-echoes-ratio`, `hidden-split-holds-focus` (Task 16: upstream's documented model
    contract, `control-api.md` "works shown or hidden; read `splitFocused`", and `AppStorePaneTests`; macOS's
    reversion is an AppKit first-responder effect the suite calls nondeterministic); `quick-validates-input`
    (Task 17); `modal-blocks-covers`, `pick-names-pending-ask` (Task 19); `overlay-size-validated` (Task 20);
    `sidebar-min-width` (Task 22); `theme-slots-by-sync` (Task 23); `keymap-menu` (Task 24, once a GTK menu carries
    the actions' live accelerators; dropping it needs an explicit exception recorded here before Task 37);
    `config-reload-counts` (Task 25); `live-sessions` (Task 28) and `live-any-shell` (Task 31) by host.
  - not claimed: `window-geometry` and `window-minimize` (Wayland gives a client no window position and no
    minimized read-back; their tests branch, and Rust passes the other branch); `capture-reads-command-panes`
    (it contradicts upstream's intent: `ForegroundProcess.command` is leader-only so a `--command` pane's creation
    command keeps the exec path, `--wait` and close-on-exit, see Task 26).
  - `conformance/docs/rust-build.md` gets a "Rust target profile" section with this list (Task 5); its capability
    table stays the references' profiles, as `selftests/test_docs.py` checks.
- Linux keymap defaults (Task 24, settled with Codex: transcript
  `/tmp/plan-exec-revmux-rust-linux-implementation/task-24-linux-chords/transcript.md`). Upstream's
  `BuiltinAction.defaultChord` is built on ⌘, which GNOME/KDE take as Super chords, so the Linux build ships its own
  table (`agterm_core::keymap::linux_defaults`), keeping upstream's grammar and canonical display (`cmd` = Super,
  `opt` = Alt, `keymap.list` prints `ctrl+cmd+opt+shift`): Ctrl+Shift+<letter> base with the replaced fork's letters
  (new_window n, new_workspace w, new_session t, open_directory o, close_session q, toggle_split d,
  toggle_horizontal_split h, toggle_scratch j, toggle_search f, toggle_sidebar s, toggle_flag g, show_attention i,
  dashboard m, attach_remote r, command_palette p); upstream's ctrl-only chords kept (quick_terminal ctrl+`,
  session_palette ctrl+p); reopen_recent ctrl+opt+shift+t, custom_command_palette ctrl+opt+shift+o, undo_close
  ctrl+shift+z, fonts ctrl+= / ctrl+- / ctrl+0, toggle_terminal_zoom ctrl+shift+return, toggle_fullscreen f11, panes
  ctrl+shift+left/right, sessions ctrl+shift+up/down, attention sessions opt+shift+up/down. Upstream's
  vacate-when-used rule applies to the defaults new relative to the fork (reopen_recent, undo_close, the three font
  actions, toggle_terminal_zoom, toggle_fullscreen, custom_command_palette, the six arrow actions) plus attach_remote.
  The chords are dispatched by one capture-phase key controller per window (custom-command matcher first, then the
  menu chords); the GTK menu shows them as display-only accelerators, which `keymap.list` reads back.
  `global-hotkey` lines are parsed and diagnosed as upstream but not registered: Wayland has no client-side global
  hotkey (the GlobalShortcuts portal is a later option).
- Launch contract: bind `AGTERM_CONTROL_SOCKET` (rust-build.md; upstream reads it too, the Linux fork does not), else
  `<state dir>/agterm.sock`. Use `AGTERM_APP_ID`, else `io.github.melonamin.agterm`. Honor `AGTERM_STATE_DIR`, `HOME` and the XDG dirs. Load
  `<state>/config/ghostty.conf`. Quit on SIGTERM without a prompt.
- Pane environment as `SurfaceEnvironment.swift`: session panes get `AGTERM_ENABLED`, `AGTERM_SESSION_ID`,
  `AGTERM_SOCKET`, `AGTERM_WINDOW_ID`, `AGTERM_WORKSPACE_ID`, `AGTERM_PANE`, `AGTERM_PANE_ID`, `TERM_PROGRAM`,
  `TERM_PROGRAM_VERSION`; the quick terminal gets its own variant without session and window ids. `AGTERM_SOCKET` is
  `<path>.unavailable` when the instance was refused the socket. `agtermctl --pane-id`, `--pane` forwarding and the
  agent-status hooks rely on it.
- Toolchains: Tasks 1–4 built libghostty with Zig 0.15.2 at the fork's `GHOSTTY_REV` (11b9a6e, 2026-07-09) and zmx
  with Zig 0.16.0 (zmx's `minimum_zig_version`). Task 4.1 moves libghostty to upstream v0.36.0's `GHOSTTY_REV`
  (683d8db, 2026-08-25, Zig 0.16.0, `.upstream/agterm/scripts/setup.sh`), so the terminal core matches the
  reference (its hidden-surface GPU release, the font-increase scrollback fix upstream re-verified) and one Zig
  builds both. Fallback if the three Linux patches cannot be carried onto it: ⚠️ with the concrete reason, the
  fork's revision stays, and the profile and acceptance criteria record that exception before Task 37.
  `build/pins.env` holds each build's Zig version with sha256 (taken from ziglang.org), `GHOSTTY_REV`, the themes
  URL and sha, `ZMX_REV`.
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
  too; calibrated on the GTK code in Task 6 and frozen at 5: a signal handler inside a method reaches 4 there, the
  control server already uses 5, and a handler handing on an async callback needs 5), `allow-unwrap-in-tests`, `allow-expect-in-tests`,
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
- **Post-Completion**: manual checks against upstream's behavior and of the fork replacement, CI on GitHub, the macOS
  and Windows follow-up brainstorms.

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

- [x] clap command tree skeleton mirroring `agtermctlKit/Commands.swift` (global `--socket`, `--json` where upstream
      has it) (as upstream, both follow the subcommand: `agtermctl --socket S version` is a usage error, exit 64)
- [x] socket resolution: `--socket`, then `AGTERM_STATE_DIR`, then the default, ported from `SocketClient.swift` and
      `ControlResolve.swift`; `agtermctl` reads neither `AGTERM_CONTROL_SOCKET` nor `AGTERM_SOCKET`
      (`test_agterm_socket_variable_is_not_read`), so it shares the state-dir code but not the app's socket env
- [x] `agtermctl version` prints `version (commit)`; exit codes and the error format as upstream
- [x] write tests: socket resolution order; exit code and stderr on a refused connection
- [x] write tests: `version` against an in-process server from `agterm-server`
- [x] run `cargo test`; must pass before Task 5

### Task 4.1: ➕ Realign Tasks 1–4 with upstream

Tasks 1–4 followed the fork where it differs from upstream; paths under `.upstream/agterm` unless noted.

**Files:**
- Modify: `build/pins.env`, `build/patches/ghostty/*.patch`, `scripts/build-ghostty.sh`,
  `scripts/gen-ghostty-bindings.sh`, `crates/agterm-ghostty-sys/src/bindings.rs`, `crates/agterm-ghostty/src/*`,
  `crates/agterm-ghostty/tests/smoke.rs`
- Modify: `crates/agterm-core/src/{lib,error,dispatch,paths}.rs`, `crates/agterm-proto/src/request.rs`
- Modify: `crates/agterm-server/src/{lib,connection}.rs`, `crates/agterm-server/tests/server.rs`
- Modify: `crates/agterm-ctl/src/{main,output,socket}.rs`, `crates/agterm-ctl/tests/cli.rs`

- [x] libghostty at upstream v0.36.0's `GHOSTTY_REV` (683d8db, `scripts/setup.sh`) built with Zig 0.16.0 (the zmx
      one, so `GHOSTTY_ZIG_*` pins follow): carry the three Linux patches onto it, re-checking each is still needed
      and that the input-handoff one still synchronizes with the renderer state; refresh the themes pin and resource
      checks for that revision, regenerate the bindings, rebuild the prefix (background) and rerun the
      `agterm-ghostty` tests; fallback per Technical Details > Toolchains (no fallback needed: all three patches
      still needed and rebased with `--fuzz=0`, the input handoff onto 0.16's `std.Io` futex API, the glad hunk
      onto `root_module`; themes 602 from the 20260810 archive; bindings regenerated, plus
      `ghostty_surface_deny_clipboard_request` that the new clipboard read contract requires)
- [x] an undecodable request answers `invalid request: <context>` (`ControlWire.invalidRequestMessage`,
      `agtermCore/Sources/agtermCore/HtmlBridge.swift`; `control-api.md` > Transport), not the fork's
      `could not decode request`: the `invalid request: ` prefix always (`HtmlBridgeTests`), an unknown command
      named in the context as Foundation's `Cannot initialize Command from invalid String value <cmd>`, the other
      decode failures with the decoder's own context
- [x] a request line that cannot be read (EOF before any byte, a read error or timeout, a line over 1 MiB) is
      answered `{"ok":false,"error":"request too large or read failed"}` before the connection closes, as upstream's
      `handleConnection` (`agterm/Control/ControlServer.swift`); the server now closes without a reply
- [x] upstream's connection deadlines: 10 s overall for the request read on top of the 5 s per-read timeout (a
      byte-a-second client never trips the latter), 5 s per write and 10 s overall for the response write
      (`readDeadlineSeconds`, `writeTimeoutSeconds`, `writeDeadlineSeconds`)
- [x] `agtermctl` usage errors and help in swift-argument-parser's format (upstream's CLI is an `ArgumentParser`
      tree): the usage line spells the options (`Usage: agtermctl version [--socket <socket>] [--json]`, not clap's
      `[OPTIONS]`), messages use its wording (`Unknown option '--bogus'`, `Missing expected argument '<id>'`,
      `Unexpected argument 'x'`, `The value 'x' is invalid for '--mode <mode>'`), and help uses its
      `OVERVIEW:`/`USAGE:`/`OPTIONS:`/`SUBCOMMANDS:` layout with `-h, --help  Show help information.`; one
      formatter later command groups inherit
- [x] doc comments and test names that cite the fork as the source (`messages_match_the_fork`, "verbatim from the
      Swift fork", "as the fork's per-read poll", `paths.rs` "as the Swift fork does") re-pointed at the upstream
      files; the Linux-only parts (XDG data home, `/proc/locks`) keep their fork reference as Linux glue
- [x] write tests: each malformed-request case of `test_addressing.py::test_undecodable_request_is_refused` against
      the dispatcher's full text; the unreadable-line reply (empty connection, oversized line, slow trickle past the
      overall deadline); a stalled reader times the response write out; the CLI usage-error and help texts
- [x] run `cargo test --workspace` and `cargo clippy --workspace --all-targets --all-features -- -D warnings`; both
      must pass before Task 5

### Task 5: Rust runner, ratchet, dev launcher, lint report and the cargo CI job

**Files:**
- Create: `conformance/tools/run-rust-linux.sh`, `conformance/tools/check_ratchet.py`, `conformance/rust-passing.txt`
- Modify: `conformance/agterm_conformance/plugin.py` (`--nodeid-report`), `conformance/selftests/test_plugin_options.py`
- Create: `conformance/selftests/test_ratchet.py`, `conformance/selftests/test_run_rust_linux.py`
- Create: `scripts/run-dev.sh`, `scripts/lint-report.sh`
- Create: `.github/workflows/rust-linux.yml`
- Modify: `conformance/README.md`, `conformance/docs/rust-build.md`

- [x] `--nodeid-report PATH` per Technical Details, as a registered class next to `MustPass`
- [x] `run-rust-linux.sh`: cargo build (release), `stage.sh`, the suite with `--impl rust` under the same
      weston/dbus isolation as `run-reference-linux.sh` (keeping its leftover-instance-root check), report in
      `conformance/reports/rust-linux/`, then `check_ratchet.py`; exit non-zero on a ratchet failure or a leftover
      root, otherwise zero whatever pytest's own status
- [x] `check_ratchet.py` per Testing Strategy; prints newly passing unlisted ids
- [x] `scripts/run-dev.sh`: unique `AGTERM_APP_ID`, temp `AGTERM_STATE_DIR` and socket, then exec the staged build
- [x] `scripts/lint-report.sh`: list complexity `#[expect]`s with file, item, reason and count; fail on inner
      `#![allow`/`#![expect` outside `agterm-ghostty-sys`
- [x] CI job `rust-linux` (ubuntu-24.04), cargo part: both Zig versions by sha256, GTK4/libadwaita/WebKitGTK `-dev`
      packages, vendored prefix cached by the hash of `build/pins.env` and `build/patches/**`, fmt check, strict
      clippy, lint report in the job summary, `cargo test` (written and actionlint-clean; its first GitHub run
      happens after a push, not automatable here)
- [x] `rust-build.md`: a "Rust target profile" section listing the capabilities the Rust build must claim by Task 37
      and the ones it does not, each with its reason (Technical Details); the capability table stays the
      references' profiles. `conformance/README.md` Reference discipline: the Rust build is held to upstream's
      contract (its capability branches included), reference-linux is a diagnostic for it, and the per-platform
      reference rule applies to the two Swift references
- [x] write tests: `test_ratchet.py` (pass; failed, errored, teardown-error and skipped listed ids; host-dependent
      unsupported skip allowed, other unsupported skips not; collection error; not-collected on a partial and a full
      run; must-pass problem; ids containing spaces; unlisted new pass; malformed file)
- [x] write tests: pytester selftests for `--nodeid-report` in `test_plugin_options.py` (phase merge, module
      teardown error, xfail/xpass, collection error, refused under `--coverage-gate`)
- [x] write tests: `test_run_rust_linux.py` with `fake_app`, like `test_run_reference_linux.py` (exit status follows
      the ratchet, leftover root fails); runs before any `agterm` binary exists
- [x] run selftests and `cargo test`; must pass before Task 6

### Task 6: A libghostty surface in a GTK window

**Files:**
- Create: `crates/agterm-gtk/{Cargo.toml,src/main.rs,src/ghostty_app.rs,src/surface.rs,src/input.rs}`
- Modify: `crates/agterm-ghostty/src/` (surface, input, actions)
- ➕ Create: `crates/agterm-gtk/{build.rs,src/clipboard.rs,src/ghostty-defaults.conf}`,
  `crates/agterm-ghostty/src/clipboard.rs`, `crates/agterm-ghostty/tests/app.rs`
- ➕ Modify: `crates/agterm-core/src/paths.rs` (config dir per `ConfigPaths.swift`, the ghostty config paths),
  `crates/agterm-ghostty/build.rs` (`DT_RPATH`), `scripts/gen-ghostty-bindings.sh` (`ghostty_surface_set_occlusion`),
  `scripts/run-dev.sh` (keeps `AGTERM_LOG`)

- [x] `adw::Application` with one window and a `GtkGLArea` hosting a libghostty surface that runs the configured
      command: app, callbacks and surface behavior from upstream's `agterm/Ghostty/GhosttyApp.swift`,
      `GhosttyCallbacks.swift`, `GhosttySurfaceView*.swift` and `.claude/rules/libghostty.md` (wakeup coalescing,
      close callback never frees synchronously, actions handled); the `GtkGLArea` hosting and OpenGL context from
      the fork's `GhosttySurface*.swift` (Linux glue) (config stack as upstream's `loadConfig` minus the
      settings-gated global config; OSC 52/Kitty prompts and link opening are interim, see the ➕ item in Task 15;
      the binaries link libghostty through `DT_RPATH`, since this machine's environment exports the fork's
      `LD_LIBRARY_PATH` and a runpath loses to it)
- [x] keyboard, IME preedit, mouse, scroll, resize, focus and content scale with upstream's semantics
      (`GhosttySurfaceView+Input.swift`: fixed Edit chords, keycode fallbacks for non-Latin layouts, paste never
      prompting), mapped onto GTK events; the input-handoff and OpenGL patches as the fork uses them (GDK mapping
      after ghostty's own GTK runtime; the Edit chords are `ctrl+shift+key_{c,v,a}` physical binds in the defaults)
- [x] calibrate `excessive-nesting-threshold` on this code; record the frozen value in Technical Details (5)
- [x] write tests: unit tests for the key and modifier translation and action decoding (the fork's
      `ModifierKeyMods.swift`/`GhosttyActionDecoder.swift` only as a hint for the GDK side)
- [x] manual check through `run-dev.sh`: the shell renders, takes input, resizes, copy and paste work; a font-size
      increase keeps the scrollback (the regression upstream's pin note names); a surface hidden and shown again
      repaints; input stays responsive under heavy PTY output (the input-handoff patch on the new revision)
      (automated part done under a private headless weston and dbus through `run-dev.sh`: the surface is created,
      runs the configured command and draws its first frame; the command's exit closes the window and the app exits
      0; SIGTERM ends it (the clean-quit handler is Task 7). Input, resize, copy/paste, font-size scrollback,
      hide/show repaint and input under heavy output: skipped - not automatable)
- [x] run `cargo test` and clippy; must pass before Task 7

### Task 7: App shell, server wiring and the GUI CI spike

**Files:**
- Create: `crates/agterm-gtk/src/{app,main_loop,signals}.rs`
- Create: `crates/agterm-core/src/pane_env.rs`
- Modify: `.github/workflows/rust-linux.yml`, `docs/plans/20261012-rust-linux-implementation.md` (measured gate duration)
- ➕ Create: `conformance/tools/webkit_probe.py` (the WebKitGTK spike, runnable locally too)
- ➕ Modify: `crates/agterm-gtk/{build.rs,src/main.rs,src/surface.rs}` (recorded commit, `teardown_all`),
  `Cargo.toml` and the crates' manifests (`uuid`, `async-channel`, `glib-unix`), `conformance/README.md`,
  `conformance/rust-passing.txt`

- [x] `AGTERM_APP_ID` or `io.github.melonamin.agterm`; `MainLoop` over the GLib main context; the control server on
      the resolved socket (`GlibMainLoop` over an `async-channel` drained by a `spawn_local` future; ownership is
      settled before the shell spawns, the server starts once the window exists, as upstream's `agtermApp`)
- [x] SIGTERM via `glib::unix_signal_add_local`: tear down the surfaces and quit without a prompt within 10 s
      (glib 0.22 keeps it in the `glib-unix` crate: `glib_unix::unix_signal_add_local_once`; the main loop closes,
      the server stops and unlinks its socket, every surface is torn down, the shell exits; measured under headless
      weston: exit 0, socket removed, shell gone)
- [x] `version` with `parityLevel: "0.36.0"` and `posix-shell`, the first capability of the Rust target profile
      (Technical Details); one `AppIdentity` feeds `version`, `tree.app` and `TERM_PROGRAM_VERSION`
      (`control-api.md` > Tree and window read-back) (`CARGO_PKG_VERSION` plus the commit `build.rs` records from
      git, or from `AGTERM_BUILD_COMMIT` when set)
- [x] the pure pane-environment builder per `SurfaceEnvironment.swift` (session and quick-terminal variants, full
      variable set, `.unavailable` socket); the app's single surface gets the session variant. Wiring per surface
      kind follows in Tasks 11 (primary), 16 (split, scratch) and 17 (quick) (ids as Foundation's upper-case
      `uuidString`; the interim surface gets a fresh session id, `AGTERM_PANE=left` and a pane token, no window or
      workspace id until the model exists)
- [x] CI: weston + dbus + mesa (`libgl1-mesa-dri`, `libegl-mesa0`); the first step is a spike:
      `run-rust-linux.sh -- -k test_version` on the runner. If headless weston's GL backend cannot host libghostty
      there, stop with ⚠️ and settle the display stack (xvfb + openbox, the fork's CI as the Linux hint) before
      going on. The spike also opens a WebKitGTK view, to learn early whether Ubuntu 24.04's AppArmor userns
      restriction breaks WebKit's sandbox on the runner (written, actionlint-clean, and emulated locally: the same
      `run-rust-linux.sh -- -k test_version` under the private headless weston passes, and
      `conformance/tools/webkit_probe.py` loads a page and runs its script in a WebKitGTK 2.54 view there; the
      runner part is not automatable before a push: the first GitHub run decides both, the WebKit step is
      `continue-on-error` and reports the host's userns sysctls in the job summary)
- [x] CI: a full-suite job (`run-rust-linux.sh`, protocol/cli/behaviors) after the spike; the node-id report is
      uploaded as an artifact and new green ids are printed in the job summary (job `conformance`, after the cargo
      job; the spike steps come first and a failing libghostty spike stops it before the full run; its first
      GitHub run happens after a push, not automatable here)
- [x] write tests: the builder for every variant and optional field; socket path resolution in the app
- [x] measure the full-run duration and restate it in Development Approach (48 s on this machine with the build
      cached, 47 s of it pytest)
- [x] gate: full run in the background; `test_version.py` green except the `tree.app` test (⚠️ Task 10),
      `cli/test_socket_resolution.py` tests that need only `version` green; ratchet updated (14 green and listed:
      `test_version_reports_the_app_version`, the four `test_socket_resolution.py` tests,
      `cli/test_cli_output.py::test_version`, and ids that only needed the wire already in place: six
      `test_undecodable_request_is_refused` cases, a bad-cursor `events` CLI refusal and `zmx.present` without a
      session; `test_tree_app_is_the_version_identity` stays red until `tree`)

### Task 8: Model

**Files:**
- Create: `crates/agterm-core/src/model/{mod,window_library,store,workspace,session,pane,naming}.rs`
- ➕ Create: `crates/agterm-core/src/model/{focus,navigation,recency,outbox}.rs` (focus filter and visible sets,
  navigation and reselection, `RecencyStack`, the store's event/save outbox) and
  `crates/agterm-core/src/model/tests/{mod,support,store,navigation,organization,reselection,library}.rs`
- ➕ Modify: `crates/agterm-core/src/lib.rs`

- [x] `WindowLibrary` → per-window `AppStore` → workspaces → sessions → panes, ported from upstream's
      `WindowLibrary*.swift`, `AppStore.swift`, `AppStore+SessionState.swift`, `Workspace.swift`, `Session.swift`,
      `AppStore+Naming.swift` (the fork's extra `SessionNaming.swift` is not a source), per `sidebar.md` and
      `windows.md` (in-memory part: a store records its events and save requests in an `Outbox` the library
      drains, for the ring (Task 12) and persistence (Task 9); sessions own their surfaces through `SurfaceSlot`
      over the `Surface` trait; split fields grouped in `SplitLayout`, creation commands in `PaneLaunch`; the focus
      filter, sidebar mode/width and multi-selection are in, since navigation and reselection scope by them;
      soft close, recent closed (Task 11), restore from a snapshot (Task 9), status details (Task 15) and the
      overlay/HUD/ask/scratch state come with their tasks; upstream's SwiftUI claim queue is not ported)
- [x] selection, recency (`RecencyStack.swift`, `AppStore+Recency.swift`), new-session placement
      (`AppStore+NewSessionPlacement.swift`), close reselection (`AppStore+CloseReselection.swift`)
- [x] write tests: tree mutations, naming and display names (basename pins), placement (ported from upstream's
      `AppStoreTests`, `AppStoreNameSanitizeTests`, `AppStoreNavigationTests`; also `AppStoreFocusTests`,
      `AppStoreOrganizationTests`, `AppStoreSidebarTests`, `AppStoreCurrentWorkspaceTests`, the store-level
      `AppStoreEventTests` and the in-memory `WindowLibraryTests`: 329 ported tests in `model/tests/`, plus 2 for the
      library's cross-window event drain order)
- [x] write tests: reselection after close, recency order
- [x] gate: full run; ratchet; must pass before Task 9 (ratchet holds, 14 green as before: the model is not
      wired into the app until Tasks 10–11)

### Task 9: Persistence and settings

**Files:**
- Create: `crates/agterm-core/src/persist/{mod,snapshot,settings}.rs`
- Create: `crates/agterm-core/tests/fixtures/` (snapshots and settings in upstream's format, plus files the
  installed fork wrote, for the replacement)
- ➕ Modify: `crates/agterm-gtk/src/main.rs` (the log file under the state dir)
- ➕ Create: `crates/agterm-core/src/persist/{json,library}.rs` (Foundation's pretty, key-sorted JSON and atomic
  write; the library's bootstrap, loads and saves), `crates/agterm-core/src/model/{snapshot,background}.rs` (the
  model ↔ snapshot boundary; `BackgroundWatermark`/`PaneBackgrounds`), `crates/agterm-core/src/model/tests/snapshot.rs`,
  `crates/agterm-core/tests/persistence.rs`, `crates/agterm-gtk/src/logging.rs`
- ➕ Modify: `crates/agterm-host/src/{lib,fakes}.rs` and `crates/agterm-gtk/src/main_loop.rs` (`MainLoop::post_after`,
  the timer the debounced save arms), `crates/agterm-core/src/model/{session,pane,store}.rs` (per-pane
  `PaneRestore`, backgrounds, `validate_context`)

- [x] snapshot JSON identical to `Snapshot.swift`: `windows.json` index + `windows/<id>.json`, migration and recovery
      (legacy, corrupt, missing) as `WindowLibrary` init; debounced save through `MainLoop`; no save on a cwd report
      (written as Foundation's `[.prettyPrinted, .sortedKeys]` output, ids upper case; the model's outbox is drained
      by `LibraryPersistence::commit`, debounced saves armed with `MainLoop::post_after`; wiring it into `Core` and
      the GTK app is Task 11's)
- [x] `settings.json` read/write as `SettingsStore.swift`/`AppSettings.swift`, including seeded settings from the
      harness (accessors whose consumers come later, e.g. the ghostty config lines, join with them)
- [x] write tests: load fixtures in upstream's format (`SnapshotRoundTripTests`, `PersistenceTests`) and ones the
      installed fork wrote; save → load round-trip is lossless (fixtures hand-written from upstream's code; the
      fork keeps upstream's `agtermCore` persistence code verbatim, so its files are the same format plus older
      builds' legacy keys and the fork-only `sessionNameFromTerminalTitle` setting)
- [x] write tests: corrupt index, missing window file, unknown fields handled as upstream does
- [x] ➕ logging to the state dir, as Solution Overview promises: `tracing` today writes to stderr only, at the level
      from `AGTERM_LOG` (`crates/agterm-gtk/src/main.rs`). Add a log file under the resolved state dir (rotation
      and name settled against upstream's and the fork's practice), keep stderr, which the harness captures as the
      instance log `conformance/tools/ghostty_probe.py` reads, and test the file appears under `AGTERM_STATE_DIR`
      (`<state>/agterm.log`, appended, owner-only, no symlink follow, as upstream's `session-host.log`; moved to
      `agterm.log.1` at startup past 8 MiB)
- [x] gate: full run; ratchet; must pass before Task 10 (ratchet holds, 14 green as before: persistence is not
      wired into the app until Task 11)

### Task 10: Tree projection and target resolution

**Files:**
- Create: `crates/agterm-core/src/{projection,resolve}.rs`
- Modify: `crates/agterm-ctl/src/main.rs`
- ➕ Create: `crates/agterm-proto/src/tree.rs` (the wire nodes, shared with `agtermctl`), `crates/agterm-ctl/src/tree.rs`,
  `crates/agterm-gtk/src/pane.rs` (the terminal as the core's `Surface`)
- ➕ Modify: `Cargo.toml` (serde_json `preserve_order`, so a result keeps upstream's field order on the wire),
  `crates/agterm-core/src/{dispatch,error,lib}.rs`, `crates/agterm-ctl/src/output.rs`,
  `crates/agterm-gtk/src/{app,main,surface}.rs`, `crates/agterm-host/src/fakes.rs` (`FakeMainLoop::spawn_with`: the
  core holds its terminals and no longer crosses threads), the server and CLI integration tests

- [x] `tree` projection per `ControlProjection.swift` (field names, order and optionality as the catalog `fields`),
      `tree.app` (fields the model does not hold yet — overlays, HUD, ask, dashboard, zoom, foreground, font sizes,
      live attribution — stay absent until their tasks; minimal wiring so `tree` answers over the socket: `Core`
      owns the bootstrapped library and settings, and the GTK window's one terminal sits in the shown session's
      primary slot with that session's real ids; the eager deck and saves after mutations stay Task 11's)
- [x] target resolution per upstream's `ControlResolve.swift` and `agterm/Control/ControlTargetResolver.swift`
      (`control-api.md` > Transport and addressing): frontmost scoping, id and prefix matching, batch targets, the
      pinned error strings (`protocol.target-resolution`)
- [x] `agtermctl tree` and the global target options (`--window`; `--target` and the repeatable batch `--target`
      are defined and tested, their first commands come in Task 11)
- [x] write tests: projection for multi-window and split layouts against expected JSON
- [x] write tests: resolver ambiguity, unknown id, cross-window targets
- [x] gate: full run; the `test_tree.py`/`test_addressing.py` tests that need only tree and resolution green, the
      rest ⚠️ until Tasks 11–15 (they use `window.list`, `session.*` or `workspace.*`); ratchet (holds; 5 new green
      ids: the four fresh-tree tests of `test_tree.py` and `test_tree_app_is_the_version_identity`, 19 listed)

### Task 11: Session lifecycle, eager deck and `window.list`

**Files:**
- Create: `crates/agterm-core/src/commands/{session,window}.rs`, `crates/agterm-gtk/src/{deck,sidebar}.rs`
- Create: `crates/agterm-ctl/src/{session,window}.rs`
- ➕ Create: `crates/agterm-proto/src/result.rs` (upstream's `ControlResult` subset and `ControlWindowNode`),
  `crates/agterm-core/src/commands/{mod,tests/*}.rs`, `crates/agterm-core/src/sidebar_drop.rs`,
  `crates/agterm-core/src/model/{pending_close,recent_closed,duplicate}.rs` and their `model/tests/`,
  `crates/agterm-core/src/persist/recent_closed.rs`, `crates/agterm-gtk/src/{state,window}.rs`,
  `build/patches/ghostty/0004-embedded-release-gpu-resources.patch`, `crates/agterm-ghostty/tests/gpu_release.rs`
- ➕ Modify: `crates/agterm-core/src/{dispatch,error,lib}.rs`, the model's store/outbox/window library/focus/snapshot,
  `crates/agterm-core/src/persist/{library,mod}.rs`, `crates/agterm-gtk/src/{app,surface,main}.rs`,
  `crates/agterm-ghostty/src/surface.rs`, `crates/agterm-ghostty-sys/src/bindings.rs`,
  `scripts/gen-ghostty-bindings.sh`, `crates/agterm-ctl/src/{main,client,output,usage}.rs`, the server test

- [x] session.new, duplicate, close (soft close, recently closed), select, go, rename, reveal, move from
      `ControlDispatcher*.swift` and the `AppStore+*` files they call (`AppStore+Duplicate`, `+PendingClose`,
      `RecentClosed.swift`) (a batch close soft-closes under `closeGraceUndoEnabled`, a single one closes hard, as
      upstream; Open Recent is recorded at close time into `recent-closed.json`; its reopen path and undo exist in
      the model, tested, with no UI caller until the menus; `session.reveal` opens the directory through
      `gtk::FileLauncher::open_containing_folder`)
- [x] window.list (upstream's cached list, `ControlServer+WindowCommands.swift`); upstream's eager deck
      (`libghostty.md`, `WindowContentView+Detail.swift`): every session's surface stays mounted, visibility flips
      on select, mount state readable for the harness's `wait_mounted` (`window.list` is computed on dispatch, no
      off-thread cache; geometry absent until Task 14; the deck is a `gtk::Overlay` of per-session pages, hidden
      ones at opacity 0 and untargetable but mapped, and a hidden page's surface is created from a frame-clock tick
      once its area is realized and allocated, since a `GtkGLArea` emits `resize` only when snapshotted; every
      mutation is a main-loop task that ends in `settle`: commit, arm debounced saves and grace timers, reconcile
      the deck and sidebar; quit saves everything first; one GTK window shows the frontmost store until Task 14)
- [x] ➕ hidden surfaces release their GPU resources: libghostty releases them on occlusion only where the
      renderer draws on its own thread, and the OpenGL build draws on the app thread, so a hidden deck surface
      needs a release call there, with its area's context current. The embedding API has none at 683d8db: a
      `build/patches/ghostty/` patch exporting `releaseGpuResources` (Task 6 only drives `set_occlusion` on
      map/unmap). Keep a live surface's `GtkGLArea` mounted (hide, never re-parent): unrealizing it frees the surface
      (`TerminalSurface::teardown`), since the embedding API cannot move a renderer to a new GL context
      (patch 0004 exports `ghostty_surface_release_gpu_resources`, upstream's own `releaseGpuResources`; the deck
      calls it with the area's context current when a page hides or is created hidden; an EGL test checks the
      object count drops and the next draw is identical)
- [x] pane environment wired into primary panes
- [x] a minimal sidebar of workspaces and sessions after upstream's `WorkspaceSidebar.swift` (full parity in
      Task 22) (a `navigation-sidebar` list in a `gtk::Paned`, tree mode only; clicks select or expand)
- [x] `session.move --after/--before` places like a sidebar drop (`SidebarDrop.resolveRelative`); claim
      `drop-placement`
- [x] matching `agtermctl` subcommands (`SessionCommands.swift`, `WindowCommands.swift` list)
- [x] write tests: lifecycle and reselection on the fake surface; duplicate; recently closed and undo
- [x] write tests: dispatcher errors for these commands from upstream's Swift tests
- [x] gate: full run; ratchet the green ids of `test_session.py`, `test_tree.py`, `test_addressing.py`,
      `cli/test_cli_output.py` (30 new green ids, 49 listed; the rest of these files stays red on commands of later
      tasks: `workspace.*`, `events.read`, `session.flag/context/seen/status/restore/type`, `window.rename`, so
      `test_move_after_an_anchor_below_it` and the CLI session tests wait for Task 13; `drop-placement` is checked by
      the dispatcher's unit tests until then)

### Task 12: Event ring and `events.read`

**Files:**
- Create: `crates/agterm-core/src/events.rs`
- Modify: `crates/agterm-server/src/stream.rs`
- Create: `crates/agterm-ctl/src/events.rs`

- ➕ Create: `crates/agterm-core/src/events/tests.rs`, `crates/agterm-core/src/commands/tests/events.rs`,
  `crates/agterm-server/tests/stream.rs`; Modify: `crates/agterm-proto/src/{events,result}.rs` (event, payload and
  batch wire types), `crates/agterm-core/src/{dispatch,lib}.rs`, the model's outbox and store (the wire payload),
  `crates/agterm-gtk/src/state.rs`, `crates/agterm-ctl/src/{main,client}.rs`

- [x] event ring per `ControlEvents.swift` and `AppStore+Events.swift`: the 10 event kinds, cursors, the anchor in
      `result.events` on a cursor failure (every commit sequences the model's events into the ring; `tree.changed`
      coalesces per window behind upstream's 0.1 s debounce, armed on the main loop by `settle`)
- [x] `events.read`: without a cursor it subscribes from now with no history; stream handoff after the first ok
      (upstream's `agterm/Control/ControlStreamOwner.swift`) (upstream's `events.read` is a plain read that
      `agtermctl events` polls, so it hands nothing off; `ControlStreamOwner` is ported as `LineStream` in
      `stream.rs` for `zmx.present` and overlay jobs)
- [x] `agtermctl events` subcommands (`EventCommands.swift`)
- [x] write tests: ring overflow, stale cursor, subscription delivery order
- [x] write tests: a stream connection survives other requests and closes cleanly on client hang-up
- [x] gate: full run; ratchet (`test_events.py`) (25 new green ids, 74 listed;
      `test_cursor_expires_past_the_ring_capacity` and the CLI event-printing tests wait for `session.status`,
      `session.split`, `notify` in later tasks)

### Task 13: `workspace.*`

**Files:**
- Create: `crates/agterm-core/src/commands/workspace.rs`
- Modify: `crates/agterm-gtk/src/sidebar.rs`
- Create: `crates/agterm-ctl/src/workspace.rs`
- ➕ Create: `crates/agterm-core/src/commands/tests/workspace.rs`; Modify: `crates/agterm-core/src/commands/mod.rs`
  (`workspace_store`, `ToggleMode`), `crates/agterm-core/src/dispatch.rs`, `crates/agterm-core/src/model/{focus,store}.rs`
  (`FocusMode` wire names and help, the outline's expansion requests), `crates/agterm-ctl/src/main.rs`,
  `crates/agterm-ctl/tests/cli.rs`

- [x] the 10 workspace commands (new, rename, delete, select, go, move, focus, filter, collapse, expand) from
      `ControlDispatcher.swift`, `AppStore+Focus.swift`, `AppStore+Sidebar*.swift` and
      `ControlDispatcherWorkspaceTests` (`workspace.delete` closes hard via `remove_workspace`, as upstream's control
      arm; `collapse`/`expand` also queue a per-row request the GTK outline applies on refresh, upstream's
      `.agtermSetWorkspaceExpanded` notification, so a row a reveal opened folds even when the persisted flag is
      unchanged)
- [x] `workspace new --collapsed` per upstream (`fork-bugs.md`): it never takes the active workspace and opts out of
      focus membership
- [x] `agtermctl workspace ...` (`WorkspaceCommands.swift`)
- [x] write tests: each command's success path and its errors (strings from the Swift tests)
- [x] write tests: the background create keeps the current workspace
- [x] gate: full run; ratchet (`test_workspace.py`, including `test_background_create_keeps_the_current_workspace`)
      (65 new green ids, 139 listed: all 35 of `test_workspace.py`, the CLI workspace and session tests, and the
      `test_session.py`/`test_addressing.py` ids that waited on `workspace.new`, `test_move_after_an_anchor_below_it`
      included)

### Task 14: The rest of `window.*`

**Files:**
- Modify: `crates/agterm-core/src/commands/window.rs`, `crates/agterm-ctl/src/window.rs`
- Create: `crates/agterm-gtk/src/windows.rs`
- ➕ Create: `crates/agterm-core/src/windows.rs` (`WindowState`, `WindowRequest`, `WindowSize`, upstream's
  `WindowGeometry.clampSize`); Modify: `crates/agterm-core/src/{dispatch,lib}.rs`,
  `crates/agterm-core/src/commands/tests/window.rs`, `crates/agterm-proto/src/result.rs` (`width`, `height`),
  `crates/agterm-ctl/src/output.rs`, `crates/agterm-ctl/tests/cli.rs`, `crates/agterm-gtk/src/{app,main,state,window}.rs`

- [x] window.new, select, go, close, rename, delete, resize, move, zoom, fullscreen, minimize per upstream
      (`ControlServer+WindowCommands.swift`, `WindowRegistry.swift`, `windows.md`) on GTK windows; `window.select`
      and `window.go` take frontmost in the model as upstream's `takeFrontmost`, so claim `select-takes-frontmost`;
      Wayland gives no client positioning and no minimized read-back, so `window-geometry` and `window-minimize`
      stay unclaimed and their tests pass the other branch (the core leaves on-screen work to the frontend as
      `WindowRequest`s applied after the commit; `window.move` refuses with the fork's string unless the host claims
      `window-geometry`; minimized is read back on X11 only, else a request is always sent and `window.list` reads
      false; resize clamps between 640x400 and the monitor)
- [x] multiple GTK windows, each over its own `AppStore`; close and delete persistence (`windows.rs` registry driven
      by the open set; each window's deck and sidebar are its own, so a terminal never changes windows; the user's
      close is upstream's `willClose`; the app quits once no window is open; activation reports frontmost)
- [x] `agtermctl window ...`
- [x] write tests: window lifecycle and index persistence
- [x] write tests: error paths (last window, unknown id)
- [x] gate: full run; ratchet (`test_window.py`) (31 new green ids, 170 listed: all of `test_window.py`, the CLI
      `test_window_commands`, and `test_tree.py::test_index_unsaved_while_windows_json_cannot_be_written`)

### Task 15: Terminal I/O and session metadata

**Files:**
- Modify: `crates/agterm-gtk/src/surface.rs`, `crates/agterm-core/src/commands/session.rs`
- Create: `crates/agterm-core/src/commands/surface.rs`
- ➕ Create: `crates/agterm-core/src/{keystrokes,interrupt,zoom,clipboard_policy,link_policy}.rs`,
  `crates/agterm-core/src/model/{search,status}.rs`, `crates/agterm-core/src/commands/{metadata,typing,search,
  surface_target}.rs` (+ `commands/tests/{metadata,surface}.rs`, `model/tests/status.rs`),
  `crates/agterm-ghostty/src/text.rs`, `crates/agterm-ghostty/tests/{common/mod.rs,surface_io.rs}`,
  `crates/agterm-ctl/src/{session_io,session_meta,surface}.rs`; Modify: `crates/agterm-host/src/{lib,fakes}.rs` (the
  `Surface` I/O methods), `crates/agterm-server/src/{stream,connection,lib}.rs` (deferred replies),
  `crates/agterm-core/src/{dispatch,lib,pane_env,projection,resolve}.rs`, `model/{session,store,outbox,mod}.rs`,
  `persist/settings.rs`, `crates/agterm-proto/src/{result,lib,events}.rs`, `crates/agterm-gtk/src/{app,state,deck,
  window,pane,clipboard,ghostty_app,input}.rs`, `crates/agterm-ghostty/src/{surface,lib}.rs`, `crates/agterm-ctl/src/{main,
  session,output}.rs`, `crates/agterm-ctl/tests/cli.rs`, `Cargo.toml` (serde_json `float_roundtrip`)

- [x] session.type (`KeystrokeSegments.swift`), text (`TerminalText.swift`), copy, paste, selectall, search;
      surface.cursor, surface.zoom (`TerminalZoom.swift`) (`session.type` and `session.search` wait on the frontend
      as upstream's main-actor polls do: `Core::begin` answers `Dispatched::Pending` with a step the GTK state runs as
      a delayed main-loop task, and the connection waits on a deferred `Reply`; terminal zoom is a per-window target in
      `Core` the GTK window shows as the zoomed session's page with the sidebar stepped aside and an exit-zoom button,
      no terminal reparented)
- [x] session.status, flag, context, seen (`AppStore+Status.swift`, `SessionContext.swift`, `AgentStatus.swift`)
      (status sounds are queued for the frontend, which sounds the display's alert: Linux has no named system sounds)
- [x] `Surface` over libghostty: `read_text`, inject (key events with the final-Return gap, never
      `ghostty_surface_text`), selection, foreground pid; clipboard prompt policy (`ClipboardPromptPolicy.swift`,
      `Ghostty/ClipboardPromptController.swift`); `control-api.md` > Surface input, output, and search
- [x] ➕ replace Task 6's interim clipboard and link handling: OSC 52/Kitty reads and `ask` writes go through the
      prompt instead of being denied (`crates/agterm-gtk/src/clipboard.rs`), and `GHOSTTY_ACTION_OPEN_URL` through
      upstream's `LinkPolicy.swift`/`LinkOpener.swift` instead of being consumed unopened (`ghostty_app.rs`)
      (automount roots are Linux's `/net` and `/misc`; an overlay route opens in the browser until Task 20)
- [x] `session.type` refuses a NUL (`text must not contain a NUL byte`) and `session.status` validates and reads back
      `shape`; claim `type-rejects-nul` and `status-shape`
- [x] matching `agtermctl` subcommands (`SessionMetadataCommands.swift`)
- [x] write tests: keystroke segmentation, text read-back, status/flag/context read-back on the fake surface
- [x] write tests: errors (NUL text, unknown pane, `ControlDispatcherSessionMetadataTests`, all as upstream)
- [x] gate: full run; ratchet (`test_session_text.py`, `test_surface.py`, `test_session.py`); scratch and split users
      ⚠️ until Task 16, quick, overlay and HUD users ⚠️ until Tasks 17–20 (70 new green ids, 240 listed, identical over two full runs: the reachable `test_session_text.py`, `test_surface.py` and `test_session.py` ids (status, flag, context, type, text, search, cursor refusals, selection), `test_split.py`'s zoom refusals, `test_events.py` and `test_tree.py` ids that waited on `session.status`/`session.type`, and `test_events_json_prints_each_event_object_of_the_asked_kinds` once serde_json parses floats exactly)

### Task 16: Splits and the scratch terminal

**Files:**
- Create: `crates/agterm-core/src/commands/panes.rs`, `crates/agterm-gtk/src/split.rs`

- [x] session.split, split.close, swap, focus, resize, lead, scratch from `ControlDispatcher+Panes.swift`,
      `AppStore+Panes.swift`, `ControlServer+SessionActions.swift` (`control-api.md` > Organization commands); the
      GTK split after upstream's `WindowContentView+Detail.swift`, `Views/SplitRatioAccessor.swift`,
      `AppActions.swift` and `menu-actions.md`
- [x] `session.resize` echoes the clamped ratio; claim `resize-echoes-ratio`
- [x] focus on a hidden split holds, as upstream's model (`AppStorePaneTests`): stale GTK focus callbacks after a
      layout change never undo the chosen pane, and a hidden split with the right pane chosen shows that pane
      maximized and gives it input; claim `hidden-split-holds-focus`; `pane.split` and `pane.scratch` events
- [x] pane environment wired into split and scratch panes
- [x] matching `agtermctl` subcommands (`Session+Swap.swift`, `Session+Lead.swift`)
- [x] write tests: split tree mutations, ratio restore, swap
- [x] write tests: focus on a hidden split (ported from `AppStorePaneTests`); close of the last pane
- [x] manual check through `run-dev.sh`: hide the split, focus right, type a marker, show both: the marker is in the
      right pane and it still has focus
- [x] ➕ per-pane font-size read-back (`fontSize`, `splitFontSize`, `scratchFontSize` through `Surface::font_size`):
      the harness's `wait_mounted` reads a split or scratch pane as up by them, so every split test needs them
- [x] gate: full run; ratchet (`test_split.py` and newly green tests elsewhere)

### Task 17: Restart, background, restore, quick terminal, dashboard and auto-follow

**Files:**
- Create: `crates/agterm-core/src/commands/{quick,dashboard,restart}.rs`, `crates/agterm-core/src/auto_follow.rs`
- Modify: `crates/agterm-core/src/commands/session.rs` (background, restore)
- Create: `crates/agterm-gtk/src/{quick,dashboard}.rs`
- ➕ Create: `crates/agterm-core/src/{dashboard,quick_metrics,restart_replay,command_restore,watermark,
  osc_background}.rs`, `crates/agterm-core/src/{dashboard,watermark}/tests.rs`,
  `crates/agterm-core/src/commands/{background,restore}.rs`,
  `crates/agterm-core/src/model/restore.rs`, `crates/agterm-core/src/commands/tests/{restart,background,quick,
  dashboard,auto_follow}.rs`, `crates/agterm-ctl/src/{quick,dashboard,session_restore,session_background}.rs`,
  `crates/agterm-gtk/src/background.rs`; Modify: `crates/agterm-core/src/{dispatch,lib}.rs`,
  `commands/{mod,surface,surface_target}.rs`, `model/{background,session,store,mod}.rs`,
  `crates/agterm-proto/src/result.rs` (`restart` receipt), `crates/agterm-ctl/src/{main,session,usage}.rs`,
  `crates/agterm-ctl/tests/cli.rs`, `crates/agterm-gtk/src/{app,deck,split,window,windows,state,surface,
  ghostty_app,main}.rs`, `crates/agterm-server/tests/server.rs` (the unimplemented-command test now uses
  `session.overlay.open`)

- [x] session.restart (`RestartReplay.swift`, `ControlServer+Restart.swift`), session.background
      (`PaneBackgrounds.swift`, `BackgroundWatermark.swift`), session.restore (restart: upstream's validation and
      `resolveRestartTarget` refusals through the live check, `RestartReplay` ported; ⚠️ the kill/rebuild of a live
      pane needs zmx and lands with Task 28; backgrounds render as per-surface libghostty config overlays, text
      rasterized to `<state>/watermarks/` as upstream's `WatermarkStorage`; the OSC background policy is ported but
      not wired, translucency and wash regions wait for their tasks)
- [x] quick, quick.type, quick.text (`QuickTerminalMetrics.swift`, `Views/QuickTerminal.swift`, `windows.md`);
      dashboard (`Dashboard*.swift`, `ControlDispatcher+Dashboard.swift`, `Views/DashboardView.swift`); claim
      `quick-validates-input` (the quick panel is one undecorated GTK window per app, sized against the active
      window's monitor since Wayland gives no position; the dashboard lays the members' existing terminals out in
      their grid cells in place, never reparented, view-only under a chrome layer for keys, clicks and captions;
      opening a dashboard hides the quick terminal and pauses auto-follow, as upstream)
- [x] auto-follow (`AppStore+AutoFollow.swift`) behind `autoFollowMs` (user activity from terminal keystrokes and
      sidebar clicks; suppression hooks exist for the rename/palette/pick tasks)
- [x] pane environment wired into the quick terminal (quick variant)
- [x] matching `agtermctl` subcommands
- [x] write tests: restart replay decisions, dashboard layout and targets, auto-follow decisions
- [x] write tests: quick.type NUL and quick.text `--lines`/`--all` validation as upstream
- [x] gate: full run; ratchet (`test_dashboard_quick.py` and newly green tests elsewhere) (56 new green ids, 332
      listed: every `test_dashboard_quick.py` test, the session.restore/session.background tests of
      `test_session.py`, `test_split.py` and `test_restore.py`, `test_zmx.py`'s non-live restart refusals, the quick
      cursor test of `test_surface.py`, and the cli background/restore/quick/dashboard tests)

### Task 18: HUD and notifications

**Files:**
- Create: `crates/agterm-core/src/commands/{hud,notify}.rs`, `crates/agterm-gtk/src/{hud,notifications}.rs`
- ➕ Create: `crates/agterm-core/src/hud/{mod,layout,markdown,tests}.rs`, `crates/agterm-core/src/hud/markdown/tests.rs`,
  `crates/agterm-core/src/model/overlay.rs` (the session-wide overlay slot Task 20 extends),
  `crates/agterm-core/src/commands/tests/hud.rs`, `crates/agterm-ctl/src/{session_hud,notify}.rs`,
  `crates/agterm-gtk/resources/hud/hud.sh` (upstream's painter, embedded and written to `<state>/hud/hud.sh`);
  Modify: `agterm-host` (`Surface::cell_size`), core `dispatch`, `projection`, `model/{session,store,mod}.rs`,
  `commands/panes.rs` (a pane focus clears the badge), GTK `split.rs` (the HUD layer and the page geometry report),
  `deck.rs`, `state.rs`, `surface.rs`, `app.rs`, `ghostty_app.rs` (OSC 9/777); deps `pulldown-cmark`,
  `unicode-normalization`

- [x] session.hud.open, update, close (`Hud.swift`, `HudMarkdown.swift`, `ControlDispatcher+Hud.swift`,
      `ControlServer+Hud.swift`); the panel as upstream's: a pty running `agterm/Resources/hud/hud.sh`, auto-hide,
      ⌘/Ctrl-click links (`Ghostty/HudLinkClick.swift`), per `control-api.md` > Overlay, zoom, dashboard (the panel
      is a passive libghostty terminal in a framed layer added once to the page's `PaneStack`; markdown parsed with
      pulldown-cmark in place of Foundation's parser; cells measured from the live terminal, else pango)
- [x] notify (`Notifications.swift`, `Notifications/NotificationManager.swift`, `notifications.md`) through
      GNotification over the instance's own dbus (Linux glue) (terminal OSC 9/777 too, with upstream's focus
      suppression; banners withdrawn once the badge clears; no notification sound or dock bounce on Linux)
- [x] matching `agtermctl` subcommands (`SessionHudCommands.swift`)
- [x] write tests: HUD markdown parsing and state transitions
- [x] write tests: notify argument validation
- [x] gate: full run; ratchet (`test_hud.py`, `test_notify.py`); HUD-over-overlay tests ⚠️ until Task 20 (60 new
      green ids, 392 listed; red until Task 20: `test_hud_holds_the_slot_without_covering_the_session`,
      `test_hud_and_program_share_one_slot`, `test_page_replaces_a_hud`, `test_hud_refused_over_a_page`,
      `test_resize_sets_the_hud_width`, `test_surface.py::test_cursor_reads_a_program_overlay_but_not_a_hud`, all
      on `session.overlay.*`)

### Task 19: ask and pick

**Files:**
- Create: `crates/agterm-core/src/commands/{ask,pick}.rs`, `crates/agterm-gtk/src/{ask_dialog,picker}.rs`
- ➕ Create: `crates/agterm-core/src/{modal,fuzzy}.rs` (the window modal slot, the ask and pick registries; the
  palettes' fuzzy ranking), `crates/agterm-core/src/model/ask.rs` (`PendingAsk`, the session ask slot,
  `AskNavigation`), `crates/agterm-core/src/commands/tests/{ask,pick}.rs`,
  `crates/agterm-ctl/src/{ask,pick,modal}.rs`; Modify: `agterm-proto` `result.rs` (`pick`, `ask` outcomes), core
  `dispatch`, `projection`, `model/{session,store,panes,pending_close,window_library,mod}.rs` (a leaving session or
  closing pane cancels its ask), `commands/{quick,dashboard,search,surface_target}.rs` (the modal refusals),
  `crates/agterm-ctl/src/{main,usage}.rs`, `crates/agterm-ctl/tests/cli.rs`, `crates/agterm-gtk/src/modal.rs` (the
  shared dim-and-panel frame), GTK `app.rs` (capabilities) and the
  window/deck wiring of the dialog and picker

- [x] ask.open, result, cancel (`Ask.swift`, `ControlAsk.swift`, `ControlDispatcher+Ask.swift`,
      `ControlServer+Ask.swift`); terminal and GUI styles drawn after `Views/AskDialogView.swift`
- [x] pick.open, result, cancel (`Pick.swift`, `ControlPick.swift`, `Fuzzy.swift`, `ControlServer+Pick.swift`); the
      picker after `Views/Palette.swift`
- [x] a pending GUI ask refuses control-driven covers and `pick.open` names it; claim `modal-blocks-covers` and
      `pick-names-pending-ask`
- [x] matching `agtermctl` subcommands (`AskCommands.swift`)
- [x] write tests: pending-ask rules, fuzzy ranking, result/cancel races
- [x] write tests: errors from `ControlDispatcherAskTests` and `ControlDispatcherPickTests`
- [x] gate: full run; ratchet (`test_ask_pick.py`) (48 new green ids, 440 listed: all 44 of
      `test_ask_pick.py` and the four cli ask/pick tests of `test_cli_output.py`)

### Task 20: Program overlays and overlay jobs

**Files:**
- Create: `crates/agterm-core/src/commands/overlay.rs`, `crates/agterm-gtk/src/overlay.rs`
- Modify: `crates/agterm-server/src/stream.rs`
- ➕ Create: `crates/agterm-core/src/{overlay_capture,overlay_jobs}.rs`, `crates/agterm-core/src/model/pane_overlay.rs`
  (the pane-scoped overlays), `crates/agterm-core/src/html/mod.rs` (the page checks the dispatcher runs before any
  web view: `grantError`, `webURL`, `HtmlNavigation`; Task 21 extends it), `crates/agterm-core/src/commands/tests/overlay.rs`,
  `crates/agterm-ctl/src/{session_overlay,overlay_page,overlay_job}.rs`; Modify: `agterm-proto` `result.rs`
  (`exitCode`, `pageID`, `pageOutcome`), core `zoom.rs` (overlay surfaces addressable), `projection.rs`, `dashboard.rs`
  (`DashboardCover`), `model/{overlay,panes,session}.rs`, GTK `split.rs`, `deck.rs`, `dashboard.rs`, `state.rs`; dep `url`

- [x] session.overlay.open (program), close, resize, result, submit, copy, text (`ControlDispatcher+Overlay.swift`,
      `OverlayCapture.swift`, `control-api.md` > Overlay); the overlay panel after upstream's session detail; a
      `sizePercent` outside 1...100 is refused, so claim `overlay-size-validated` (pane overlays with `--pane` too; a
      valid `--html`/`--url` open is refused until Task 21, while every page flag check and the no-page refusals of
      submit/reload/navigate/`result --page` answer as upstream)
- [x] session.overlay.job.run with the job-frame stream (`OverlayJobs.swift`, `ControlServer+OverlayJobs.swift`,
      `ControlStreamOwner.swift`) (the table, frames and the helper connection; nothing registers a job until the
      remote session tasks, so a claim answers `job not claimable`)
- [x] matching `agtermctl` subcommands (`OverlayPageCommands.swift`, `OverlayRunJob.swift`; upstream names the helper
      `session overlay run-job`)
- [x] write tests: overlay slot rules, exit status capture, job claiming
- [x] write tests: size validation as upstream
- [x] gate: full run; ratchet (program part of `test_overlay.py`, plus the HUD and surface tests waiting on it) (63 new
      green ids, 503 listed: every non-page test of `test_overlay.py`, the three HUD tests over a program, the surface
      cursor test, the cli overlay program and tree tests, and `test_zmx.py`'s job.run refusals; red until Task 21: the
      page tests of `test_overlay.py`, `test_hud.py`'s two page tests and the cli page tests)

### Task 21: HTML overlays

**Files:**
- Create: `crates/agterm-core/src/html/{mod,bridge,navigation,outcomes}.rs`, `crates/agterm-gtk/src/html_overlay.rs`
- ➕ Create: `crates/agterm-core/src/html/{profile,theme,page_tests}.rs`, `crates/agterm-core/src/model/html_overlay.rs`
  (the page in the slots, upstream's `AppStore+HtmlOverlay.swift`/`Session+HtmlOverlay.swift`),
  `crates/agterm-core/src/commands/overlay_page.rs` (the page arms, the outcomes and the frontend's hooks; a page's
  outcome is settled at each commit by noticing it gone from every slot, in place of upstream's release singleton),
  `crates/agterm-core/src/commands/tests/overlay_page.rs`; Modify: `model/{overlay,pane_overlay}.rs`, `commands/overlay.rs`,
  `projection.rs`, `zoom.rs`, `dashboard.rs`, GTK `overlay.rs`, `split.rs`, `state.rs`; `conformance/tests/protocol/test_overlay.py`
  (stable ids for `test_page_result_needs_a_known_page`, whose random UUID id never matched across runs)

- [x] `--html` overlays on WebKitGTK through the `webkit6` crate behind the default cargo feature `html`; without it
      the build refuses HTML overlays (the fork's handling of a missing WebKitGTK as the Linux hint; no port of its
      `AgtermWebKit` C shim); with it WebKitGTK is a runtime dependency (Technical Details)
- [x] `HtmlOverlay.swift` (origin pinning, navigation policy), `HtmlBridge.swift`, `HtmlPageOutcomes.swift`,
      `BrowserProfile.swift`; the panel, identity strip and toolbar after `Views/HtmlOverlay*.swift` and
      `control-api.md` > Overlay; the fork's `LinuxHtml*.swift`/`LinuxWebKit.swift` only for the WebKitGTK calls;
      session.overlay.reload, navigate; the `data-agterm` adapter, `--js` helper and `--block` outcomes (Linux
      specifics: a `--cwd` grant is served through an `agterm-file:` scheme confined to the grant, reported as
      `file:`; WebKitGTK names no frame on a script message, so a `--js` page's `agterm.request` reaches the handler
      through a top-window relay in agterm's world; persistent store at `<state>/browser/<profile id>/`; a clicked
      link in overlay link mode opens a browsing page. Left for later tasks: the theme palette variables and theme
      refresh (Task 23), `browser.clear` (Task 25), page zoom from font commands (Task 23))
- [x] matching `agtermctl` subcommands (in since Task 20; the cli page tests now pass)
- [x] write tests: bridge message → request mapping with page defaults; navigation policy
- [x] write tests: outcome bookkeeping by page id
- [x] gate: full run; ratchet (rest of `test_overlay.py`) (13 new green ids, 516 listed: every page test of
      `test_overlay.py`, `test_hud.py`'s two page tests and the two cli page tests)

### Task 22: Sidebar

**Files:**
- Create: `crates/agterm-core/src/commands/sidebar.rs`
- Modify: `crates/agterm-gtk/src/sidebar.rs` (became the `sidebar/` module: rows, row, menu, drag, context, footer)

- [x] sidebar, sidebar.mode, flagged-layout, expand, collapse, width (`AppStore+Sidebar*.swift`, `SidebarMode.swift`,
      `SidebarDrop.swift`, `sidebar.md`); `sidebar.width` clamps to upstream's floor and echoes it, so claim
      `sidebar-min-width` (`commands/sidebar.rs`; `AppStore::renders_workspace_rows`, `expand_all_workspaces`,
      `collapse_other_workspaces` persist first and queue per-row requests, so they hold with the sidebar hidden;
      `Core::update_settings` writes `settings.json`; `sidebar-min-width` claimed)
- [x] full sidebar rows as upstream's (`Views/WorkspaceSidebar*.swift`, `SidebarRowViews.swift`, `StatusGlyph.swift`,
      `SidebarRenameController.swift`): status glyphs, context lines, inline rename, context menu, flagged layout,
      drag-and-drop reorder, in GTK list idioms (GTK `sidebar/`, `status_glyph.rs`, `row_icon.rs`; the gestures go
      through `agterm_core::sidebar_actions` and stand down while the modal slot is held; upstream's rows carry no
      context line: `session.context` shows in the title bar, which Task 35 builds)
- [x] matching `agtermctl` subcommands
- [x] write tests: mode and width rules, drop placement
- [x] write tests: errors from `ControlDispatcherSidebarTests`
- [x] gate: full run; ratchet (`test_sidebar.py`) (20 new green ids, 536 listed: all of `test_sidebar.py` and the cli
      `test_sidebar`)

### Task 23: Fonts and themes

**Files:**
- Create: `crates/agterm-core/src/commands/{font,theme}.rs`, `crates/agterm-gtk/src/theme.rs`

- [x] font.inc, dec, reset over libghostty (`commands/font.rs`: the binding action on the addressed pane, a page over
      the pane steps the HTML page zoom; cell-size reports persist the main-role pane's size through
      `Core::report_font_size`, dropped under a dashboard font override and for its revert; new terminals start at the
      session's `fontSize`)
- [x] theme.set, theme.list (`ThemeCatalog.swift`, `ThemeName.swift`, `ThemeBrightness.swift`, `theme-picker.md`,
      `SystemAppearanceObserver.swift`) over the staged themes, following the system appearance through
      libadwaita's style manager; claim `theme-slots-by-sync` (`agterm_core::theme`: catalog, dual-name parser, slot
      setters, `ghosttyConfigLines`; a change rewrites `ghostty-settings.conf`, written at launch too, and a changed
      text clears the session font sizes and reloads the config into every terminal: background/HUD/overlay
      overlays re-applied, text watermarks re-rendered, ask colors and HTML pages (palette `--agterm-color-N` vars)
      re-themed. The desktop's dark flip re-feeds a dual theme keeping zoom (GTK `theme.rs`). ➕ agterm's own
      `agterm` theme staged from `build/themes/` by `scripts/stage.sh`; `theme-slots-by-sync` claimed)
- [x] matching `agtermctl` subcommands
- [x] write tests: theme catalog listing and light/dark slots
- [x] write tests: unknown theme errors
- [x] gate: full run; ratchet (`test_font_theme.py`) (562 passed, 80 failed, 12 skipped, 17 errors; ratchet 536 → 562:
      all 24 `test_font_theme.py` ids plus cli `test_font`/`test_theme`)

### Task 24: Keymap, hooks and custom commands

**Files:**
- Create: `crates/agterm-core/src/{keymap,hooks}/mod.rs`, `crates/agterm-core/src/commands/{keymap,hooks}.rs`
- Create: `crates/agterm-gtk/src/{keymap_dispatch,hook_runner}.rs`
- ➕ Create: `crates/agterm-core/src/{app_actions,palette_catalog}.rs`, `crates/agterm-core/src/commands/process.rs`,
  `crates/agterm-gtk/src/{app_menu,keymap_actions}.rs`, `crates/agterm-proto/src/keymap.rs`

- [x] config dir for `keymap.conf` and `hooks.conf` per `ConfigPaths.swift` (with the `AppSettings.configDirectory`
      override) (`paths::ConfigLocator`, `Core::config_dir`)
- [x] keymap model and matcher (`Keymap.swift`, `Keybind*.swift`, `BuiltinAction.swift`, `ControlKeymap.swift`,
      `keymap.md`); keymap.reload, list, run; the app's menu actions and their accelerators after
      `agtermApp+Menus.swift` and `Commands/KeyboardLayout.swift`, as a GTK menu whose live accelerators
      `keymap.list` reports in `menu`; claim `keymap-menu` (or record the exception in Technical Details) (Linux
      default table in Technical Details; one capture-phase key monitor per window and on the quick terminal; the
      primary menu's File/View/Navigate items carry display-only accelerators read back by `keymap.list`;
      `PaletteCatalog` ported for the menu/keymap enablement; `keymap-menu` claimed. ➕ the palettes, the theme
      picker and Attach Remote reach their action but log until Tasks 34 and 29/30 build their UI; `global-hotkey`
      parsed, not registered)
- [x] `hooks.conf` and `HookScheduler` (bounded queue, one child per hook, exit releases a slot); hooks.reload,
      list; `ProcessLauncher` impl after `Commands/HookProcessRunner.swift` and `HookController.swift` (the ring's
      appended events feed the scheduler after every commit and `tree.changed` fire; `ProcessLauncher` grew an exit
      handler and stderr capture; GTK `SystemLauncher`)
- [x] custom commands (`CustomCommand*.swift`, `Commands/CustomCommandRunner.swift`) (spawn, failure banner and
      `--error-hud` panel, usage counts, Edit Keymap/Hooks/ghostty.conf editor overlays reloading on exit)
- [x] matching `agtermctl` subcommands
- [x] write tests: keymap parse/match, scheduler ordering and queue limits (port `HookSchedulerTests`, `HooksTests`)
- [x] write tests: reload with a broken file keeps the previous config and reports the error (upstream's
      contract followed: bad lines are reported and the good ones kept; an unreadable file loads nothing and reports
      one line-0 diagnostic, as `KeymapStore.load`/`loadHooks`)
- [x] gate: full run; ratchet (`test_keymap_hooks.py`, `behaviors/test_keymap.py`, `behaviors/test_hooks.py`)
      (588 passed, 54 failed, 12 skipped, 2 errors; ratchet 562 → 588: every keymap/hooks protocol, behaviors and
      cli id)

### Task 25: Config reload, browser and debug

**Files:**
- Create: `crates/agterm-core/src/commands/{config,browser,debug}.rs`
- ➕ Modify: `crates/agterm-gtk/src/main.rs`, `crates/agterm-ghostty/src/app.rs` (argv to `ghostty_init`)

- [x] config.reload on every surface, answering the diagnostic count, so claim `config-reload-counts`;
      browser.clear, browser.links (`BrowserProfile.swift`, `ControlDispatcher+Browser.swift`); debug.appearance
      refused outside UI-test mode
- [x] ➕ `agterm`'s command line as upstream's: upstream has no options of its own and hands the full argv to
      `ghostty_init` (`agterm/Ghostty/GhosttyApp.swift`), while the Rust build passes only argv[0] to both
      GApplication and `ghostty_init` (`crates/agterm-gtk/src/main.rs`, `crates/agterm-ghostty/src/app.rs`). Pass
      the argv on to `ghostty_init` as upstream does, keep GApplication from rejecting arguments it does not own, and
      test that a launch with extra arguments starts and serves
- [x] matching `agtermctl` subcommands
- [x] write tests: reload propagation through the fake surfaces
- [x] write tests: browser profile paths and errors
- [x] gate: full run; ratchet (`test_config_browser.py`)

### Task 26: Restore modes and relaunch

**Files:**
- Create: `crates/agterm-core/src/restore/{mod,mode,capture}.rs`, `crates/agterm-core/src/commands/restore.rs`
- ➕ Create: `crates/agterm-core/src/restore/{pacer,spawn_plan}.rs`, `crates/agterm-core/src/commands/relaunch.rs`,
  `crates/agterm-core/src/commands/tests/restore.rs`, `crates/agterm-ctl/src/restore.rs`,
  `crates/agterm-gtk/src/{process_table,restore_launch}.rs`; Modify: `crates/agterm-host/src/{lib,fakes}.rs`
  (`ProcessTable` seam), `crates/agterm-proto/src/{lib,result}.rs` (`ControlRestoreStatus`),
  `crates/agterm-core/src/{command_restore,dispatch,lib}.rs`, `model/restore.rs`, `commands/{mod,typing}.rs`,
  `crates/agterm-gtk/src/{app,deck,main,state,surface}.rs`, `crates/agterm-ctl/src/{main,output}.rs`,
  `crates/agterm-ctl/tests/cli.rs`

- [x] restore.mode, restore.capture, restore.clear (`RestoreMode.swift`, `CommandRestore.swift`,
      `AppStore+Restore.swift`); the launch mode frozen for the process, quit capture
- [x] `none` and `rerun` relaunch: layout, pinned commands, captured commands (`LaunchSpawnPlan.swift`,
      `SpawnPacer.swift`, `CommandRestore.restorePlan`)
- [x] capture keeps upstream's intent (`Ghostty/ForegroundProcess.swift` `command`): it never turns a pane's own
      creation command into a shell replay, so a `--command` session keeps the exec path, `--wait` and
      close-on-exit, while a foreground job started from a shell (also inside `--command /bin/sh`) still takes
      precedence over the creation command. On Linux the foreground group's leader is the program itself, so a
      leader-only read is not enough: a candidate predicate (leader is the pane's spawned child) is checked against
      a shell wrapper, `exec`, a pipeline and a live pane. `capture-reads-command-panes` is not claimed (predicate:
      the foreground group's leader is the terminal's session leader, `/proc/<pgid>/stat` session == pgid, i.e. the
      process libghostty spawned; refused for a `--command` program, a shell wrapper, an `exec`, a shell at its
      prompt; a job an interactive shell starts, also a pipeline's first element, is captured; ⚠️ a live pane's
      leader is read through the zmx daemon and lands with Task 28)
- [x] live requested without zmx: fall back with `unavailableReason` as upstream
- [x] matching `agtermctl` subcommands
- [x] write tests: capture argv parsing, shell recognition, denylist
- [x] write tests: relaunch plans for each mode; live fallback status
- [x] write tests: the whole chain capture → save → relaunch → surface spawn on the fakes, for a `--command` session
      with and without `--wait` and for a job typed into a shell pane
- [x] gate: full run; ratchet (`test_restore.py`, `behaviors/test_relaunch.py` non-live cases) (12 new green ids,
      617 listed: every non-live `test_restore.py` test, the three non-live relaunch behaviors and cli `test_restore`;
      the 28 still red are zmx, terminfo and live clipboard tests of later tasks)

### Task 26.1: ➕ Quick terminal as a floating layer-shell panel

Task 17 hosts the quick terminal in an ordinary undecorated GTK window. Upstream's `QuickTerminalPanel`
(`agterm/Views/QuickTerminal.swift`, `.claude/rules/windows.md` "Quick terminal") is a borderless, non-activating
`NSPanel` at `.floating` level on every space, shown on the pointer's screen over any application and dismissed on
focus loss when summoned by the user. On Wayland an ordinary window cannot ask to float, so a tiling compositor
(niri here) places it in its layout like any other window. The Linux equivalent of upstream's panel is a
wlr-layer-shell surface.

**Files:**
- Modify: `crates/agterm-gtk/Cargo.toml`, `crates/agterm-gtk/src/quick.rs` (and its callers in `state.rs`/`app.rs`)
- Modify: `.github/workflows/rust-linux.yml` (the `-dev` package), `README.md`/`conformance/README.md` if they list
  runtime dependencies

- [ ] host the quick terminal in a layer-shell surface through `gtk4-layer-shell` (the standard library, used via its
      Rust bindings): layer `top` (or `overlay` if `top` does not cover full-screen windows on the compositor), anchored
      to the top edge on the monitor under the pointer (upstream's pointer-screen rule), sized by
      `QuickTerminalMetrics` as today, keyboard interactivity `on-demand` (exclusive only if on-demand loses keys),
      exclusive zone 0 so it overlays without pushing other windows
- [ ] keep upstream's behavior on top of it: summon/dismiss, dismiss on focus loss for a user summon and the pinned
      control-driven `quick show`, never activating or raising agterm's own windows, one panel per app, the surface kept
      realized while hidden (Task 17), the quick pane environment, `quickVisible`, zoom and `quick.type`/`quick.text`
- [ ] fall back to today's ordinary window when the compositor has no layer-shell (GNOME/Mutter, X11): detect at
      runtime with `gtk4_layer_shell::is_supported()`, log which host is used
- [ ] the build dependency (`libgtk4-layer-shell-dev` / `gtk4-layer-shell`) in CI and the README's build prerequisites;
      the shared library is a runtime dependency only where layer-shell is used — if the crate links it
      unconditionally, record that as a runtime dependency
- [ ] write tests: the placement and sizing decisions (monitor choice, anchors, margins from `QuickTerminalMetrics`)
      as pure functions; the host selection (layer-shell vs fallback)
- [ ] manual check (not automatable here): on niri the panel drops over the current workspace above other windows,
      takes keys, hides on focus loss, and does not tile; on a compositor without layer-shell the fallback window works
- [ ] gate: full run; ratchet (`test_dashboard_quick.py` and every quick test stay green under the private headless
      weston; check whether that weston offers layer-shell — if not, the suite exercises the fallback host and the
      layer-shell host is covered by the unit tests and the manual check)

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
- [ ] write tests: FNV-1a dir against values computed by upstream's `ZmxSupport` algorithm (rust-build.md); daemon
      name round-trip
- [ ] write tests: wrap rejections (non-absolute path, missing binary, non-zsh shell, missing integration)
- [ ] run `cargo test -p agterm-zmx`; gate: full run; ratchet

### Task 28: Live sessions in the app

**Files:**
- Modify: `crates/agterm-gtk/src/surface.rs`
- Create: `crates/agterm-core/src/commands/zmx.rs`, `crates/agterm-gtk/src/{zmx_runtime,pane_lead}.rs`
- Modify: `.github/workflows/rust-linux.yml`

- [ ] live launch of primary and split panes through `SessionHost` (`Ghostty/ZmxLaunch.swift`,
      `Ghostty/ZmxClient.swift`, `ZmxLifecycle.swift`), reattach on relaunch, reaping on mode switch
      (`control-api.md` > Restore mode and the zmx group)
- [ ] zmx.list, prune, kill, screen; zmx.reset as upstream (`LiveReset.swift`, `LiveResetCoordinator.swift`,
      `Ghostty/LiveResetConsumer.swift`, `LiveResetRelauncher.swift`: refusals, marker, relaunch; exempt in the
      suite, so unit tests carry it), not the fork's refusal; covered panes answered from the daemon and the lead
      cover (`ControlServer+PaneLead.swift`, `Ghostty/PaneLead.swift`, `Views/PaneLeadCover.swift`)
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
- [ ] ➕ the origin side of a handed-over ask, left by Task 19 (`AppStore+RemoteAsk.swift`,
      `ControlServer+Ask.swift` `presentAskRemotely`/`takeBackRemoteAsk`/`receivePresenterFrame`; the tree's
      `ask.remote`): `ask.open` on a presented session goes to the presenter
- [ ] `agtermctl zmx tree` and the `agtermctl zmx present` stdio bridge (`StreamBridge.swift`)
- [ ] write tests: frame codec, hub subscribe-before-snapshot, generation and revision
- [ ] write tests: tree listing errors
- [ ] gate: full run; ratchet (rest of `test_zmx.py`)

### Task 30: Remote sessions, viewer side

**Files:**
- Create: `crates/agterm-core/src/remote/{client,layout,reconnect}.rs`, `crates/agterm-gtk/src/remote.rs`

- [ ] zmx.attach and remote panes (`RemoteSession.swift`, `RemotePresentationClient.swift`, `RemoteReconnect.swift`,
      `AppStore+Remote*.swift`; app side `AppActions+RemoteAttach.swift`, `agtermApp+RemoteLayout.swift`,
      `ControlServer+RemotePresentation.swift`, `ControlServer+RemoteReconnect.swift`,
      `RemoteCommandProcessRunner.swift`, `RemotePresentationProcess.swift`)
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
- [ ] gate: full run; every `tests/cli` test that passes in the reference-macos junit is in the ratchet

### Task 33: Settings dialog and first run

**Files:**
- Create: `crates/agterm-gtk/src/settings/*.rs`, `crates/agterm-gtk/src/welcome.rs`
- Create: `docs/linux-ui-checklist.md`

- [ ] settings with upstream's sections and controls (`Views/SettingsView.swift`, `SettingsModel.swift`,
      `SettingsCatalog.swift`, `settings.md`) as libadwaita preference pages, persisting through `settings.json`;
      the fork's `Settings*Page.swift` only as a GTK hint
- [ ] first-run welcome (`FirstRunWelcome.swift`, `WelcomeAlert.swift`), quit prompt (`QuitPrompt.swift`,
      `QuitReason.swift`)
- [ ] `docs/linux-ui-checklist.md`: every user-visible feature of upstream macOS (`site/docs.html`, the rules files,
      `conformance/ui-ids.yaml`), each with its GTK form and a manual check against upstream's behavior; the
      fork's UI only as a hint for the GTK form
- [ ] write tests: settings model validation and persistence round-trip
- [ ] write tests: first-run decision logic
- [ ] gate: full run; ratchet

### Task 34: Command palette, session switcher and search

**Files:**
- Create: `crates/agterm-gtk/src/{palette,session_switcher,search}.rs`
- Create: `crates/agterm-core/src/palette/{mod,feeds,run,search}.rs` (palette state, feeds, row runs, search keys)

- [x] command palette (`Views/Palette.swift`, `AppActions+Palette.swift`, `PaletteCatalog.swift`,
      `PaletteCustomRow.swift`, `PaletteSearchKeys.swift`), session switcher (`Views/SessionSwitcher.swift`),
      scrollback search UI (`Views/TerminalSearchBar.swift`), per `menu-actions.md`
- [x] ➕ left by Task 19: `pick.open` and a GUI `ask.open` on the frontmost window close its palette, and the palette
      does not open while that window's modal slot is held (`Core::modal_pending`); reuse `agterm_core::fuzzy`
- [x] write tests: palette catalog and search keys
- [x] write tests: switcher ordering by recency
- [x] gate: full run; ratchet

### Task 35: Context menus, popovers and decorations

**Files:**
- Create: `crates/agterm-gtk/src/{context_menus,popovers,watermark}.rs`

- [ ] context menus (`Views/WorkspaceSidebar+ContextMenu.swift`), the app menu (`agtermApp+Menus.swift`, as a GTK
      menu), titlebar (`WindowContentView+Titlebar.swift`, `TitlebarComposition.swift`), attention and recent
      sessions (`WindowContentView+RecentSessions.swift`, `WindowLibrary+Attention.swift`), watermark
      (`Ghostty/WatermarkRenderer.swift`, `WatermarkConfig.swift`), status sounds (`StatusSoundPlayer.swift`)
- [ ] write tests: menu model per selection state
- [ ] write tests: watermark configuration parsing
- [ ] gate: full run; ratchet

### Task 36: Agent integrations and the checklist walk

**Files:**
- Create: `crates/agterm-gtk/src/integrations/*.rs`

- [ ] agent integrations as upstream's (`AgentHooksInstall.swift`, `SkillInstall.swift`, `CLIInstall.swift`,
      `AgentHooksInstaller.swift`, `SkillInstaller.swift`, `CLIInstaller.swift`, `control-api.md` > Installation),
      with Linux install paths (the fork's `LinuxIntegrations` as the Linux hint)
- [ ] walk `docs/linux-ui-checklist.md` against upstream's behavior; fix or record every gap with ⚠️
- [ ] write tests: integration plans and status detection on a temp home
- [ ] write tests: managed-marker idempotence
- [ ] gate: full run; ratchet

### Task 37: Verify acceptance criteria

- [ ] `version` claims exactly the Rust target profile (Technical Details), plus the live capabilities the host
      allows; any exception (`keymap-menu`, the libghostty fallback) is recorded there with its reason
- [ ] `run-rust-linux.sh`, protocol/cli/behaviors: no unexpected failed, errored or skipped test (the allowed skips
      are listed: host-dependent live ones and the ui layer); every passing id is in `rust-passing.txt`
- [ ] a reference-macos CI run at the catalog's tag on the same suite commit (layers and options recorded): every id
      green in its junit is green on rust, or is an allowed host-dependent skip from the item above
- [ ] run reference-linux fresh (`run-reference-linux.sh`) as a diagnostic: every id green on the fork and red on
      rust is resolved (upstream's branch or a Rust bug); the `fork-bugs.md` tests pass on rust
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
- [ ] CLAUDE.md with the crate map, the command-group porting loop, the Reference rule (upstream is the reference,
      `.upstream/agterm` the source, the fork only for Linux glue) and the fork-coexistence rule
- [ ] move this plan to `docs/plans/completed/`

## Post-Completion

*Items requiring manual intervention or external systems - no checkboxes, informational only*

**Manual verification**:
- daily use as a replacement for the fork: input methods, clipboard, HiDPI, multiple monitors, GPU drivers
- live sessions across a real logout and reboot
- remote sessions between two hosts

**External system updates**:
- push to `nickalie/agterm-rs` so the CI jobs run
- follow upstream releases: bump the catalog's tag, `.upstream/agterm` and the native pins together
- packaging (Flatpak, distro packages) is a separate plan
- macOS frontend (objc2/AppKit + libghostty) and the Windows decision are separate brainstorms
- offer the bash/fish live-session support upstream
