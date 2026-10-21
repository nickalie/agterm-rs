# agterm-rs

agterm in Rust for macOS, Linux and Windows as equal targets. The one frontend is GPUI's, which renders libghostty-vt
itself; today it is built and tested on Linux only, so its macOS and Windows forms are open work under the platform
rules below, not a later phase. Build, run and test commands are in [README.md](README.md); the suite and the ratchet
in [conformance/README.md](conformance/README.md) and [conformance/docs/rust-build.md](conformance/docs/rust-build.md).

## Platforms

- macOS, Linux and Windows are equal targets. A feature is done when it works on all three, not when it works on the
  dev box; nothing ships as "Linux now, the others later".
- A platform-specific mechanism (PTY, IPC, process tables, daemon spawn, paths, notifications) comes with its
  counterparts for the other two in the same change or the same plan. POSIX-only code sits behind `cfg` with its
  Windows form beside it, never as the only path.
- Prefer cross-platform crates over per-OS code (`portable-pty` with ConPTY, `interprocess`, `windows-sys` for what
  remains). A crate marked `any` in the map below must build on all three.
- Code for an OS the dev box is not runs on CI for that OS (macOS and Windows runners), not only through `cargo check`.
- Within Linux, any desktop counts: wlroots, KDE, GNOME, weston, X11. Every compositor-specific path has a fallback.

## Crate map

| crate | contents | platform |
|---|---|---|
| `agterm-proto` | wire types: two-level `Command` (group → command, strum wire names), request/response, read-back results, events, `tree` projection | any |
| `agterm-host` | the seams a frontend implements: `MainLoop`, `Surface`, `SessionHost`, `ProcessLauncher`, `ProcessTable`; in-memory fakes behind feature `fakes` | any |
| `agterm-core` | model, target resolver, command dispatcher (`commands/`, one module per group), events, keymap and Ghostty's keybind actions, the Ghostty config parser and its diagnostics, hooks, persistence, settings, pane environment, live-session/restore logic | any |
| `agterm-server` | AF_UNIX control server: socket ownership (`flock`), one thread per connection, main-loop hop, stream handoff | Linux, macOS |
| `agterm-ctl` → `ngtermctl` | clap CLI ported from upstream's `agtermctlKit`, socket resolution, output formatting | Linux |
| `agterm-ghostty-vt-sys` | checked-in bindgen output over libghostty-vt's official C API (`scripts/gen-ghostty-vt-bindings.sh`), links the vendored static library; not under the workspace lints | any |
| `agterm-ghostty-vt` | safe wrapper over the sys crate: terminal, render state, effects, key and mouse encoders, selection and its gesture, formatter with its extras, search, Kitty graphics, stream continuation, snapshot codec, clipboard-read scan | any |
| `agterm-pty` | the PTY layer of plain and live panes: a child on a PTY (openpty, ConPTY; its process group or kill-on-close job), herdr's PTY actor (poll and wake pipe on Unix, reader and writer threads on Windows) with the handoff's quiesce, duplicate, rollback and release, over the vendored portable-pty | any |
| `agterm-term` | the toolkit-neutral pane core: a libghostty-vt terminal fed by an IO thread per pane from a PTY, or for a live pane from its session's attach stream in process, input, selection, search, read-back, launch and terminal identity, frames and events in its own types | any |
| `agterm-session` | the live-session server on herdr's architecture, the app's session host: wire, endpoint, lifecycle and detached spawn per OS; the pane runtime (a session's program on a PTY, its terminal state, the fan-out to attached clients); the update handoff; the client library and the `SessionHost` over it; the binary's server and its attach-contract CLI; the foreground job and its sweeper and the process table, on every OS; the live-launch inputs (session host executable, build id, login shell, resources, namespace); the `/proc` stat and cmdline parsers | any |
| `agterm-app` | frontend-neutral app services: the hooks' `ProcessLauncher`, the `ProcessTable` (`agterm-session`'s, re-exported), logging, the restore launch decision, quick-terminal placement, the agent integrations' installers, the remote presentation process | Linux |
| `agterm-wpe-sys`, `agterm-wpe-platform-sys`, `agterm-wpe-javascriptcore-sys` | checked-in gir output over WPE WebKit, one crate per GObject namespace (`scripts/gen-wpe-bindings.sh`) | Linux |
| `agterm-wpe` | WPE WebKit host for the HTML overlays: offscreen pages on an engine thread of their own (or in a worker process), frames out as dmabufs or SHM | Linux |
| `agterm-gpui` → `agterm` | GPUI/gpui-kit frontend: windows, sidebar, deck, the terminal element (grid, sprites, Kitty graphics), overlays, quick terminal (layer-shell, a window where there is none), HTML overlays over `agterm-wpe` (feature `html`), single instance, agent integrations | Linux |

The platform column is where a crate builds and works today. `any` is a crate CI builds and tests on Linux, macOS and
Windows: the cross-platform workflow's package list is the record (README > CI), and a crate joins it in the change
that makes it build on all three. A crate that names its OSes has its other forms open under the platform rules; the
control server and its CLI are meant to be `any`: the server waits for its Windows form, the CLI for its macOS and
Windows forms. The libghostty-vt crates hold no OS-specific Rust, and the native library behind them is built for
cargo's target by the sys crate's build script, with the Zig `build/pins.env` pins for the host's OS and arch, over
the source prefix `scripts/build-ghostty-vt.sh` prepares (README > Native dependencies), so they are `any`. An `any`
crate may depend on another `any` crate, the libghostty-vt ones included; a crate built on fewer OSes enters it only
behind the `cfg` of those OSes.

GPUI owns the main thread and the whole state; control connections, the single-instance bus and the signal thread hop
onto it through `MainLoop`. Work that needs no live terminal or widget belongs in `agterm-core`, unit-tested against
the `agterm-host` fakes; terminal work that needs no window belongs in `agterm-term`, tested on real PTYs. Both stay
toolkit-neutral, so another frontend can drive them.

## Reference rule

- Upstream is the reference for the wire protocol, command semantics, error strings, `agtermctl` behavior and output,
  on-disk formats, model, core behavior and the UI. Port from its sources and Swift unit tests (the source of exact
  strings) in `.upstream/agterm`, a read-only clone git-ignored through `.git/info/exclude`; when it is missing:
  `git clone --depth 1 --branch v0.36.0 https://github.com/umputun/agterm .upstream/agterm`.
- The terminal's reference is Ghostty at the libghostty-vt pin (`build/pins.env`), not upstream's libghostty pin:
  libghostty-vt's C API is the oracle for what the terminal does, and Ghostty's own source at that revision for what
  the library leaves to its embedder (rendering, sprites, fonts, the shell integration, config keys, the PTY
  pipeline). Ported code cites that revision. Other terminals are read-only references, adapted with attribution in
  the module docs, never dependencies: qwertty-term (a Rust port of Ghostty's subsystems, the first place to look),
  gpui-ghostty (a GPUI terminal element) and rustty (Kitty placement chains). Zed's terminal is GPL: ideas only, copy
  nothing.
- The session server's reference is herdr at the commit `build/pins.env` pins (`HERDR_*`): `agterm-session` and
  `agterm-pty` take its background session server, PTY actor and `portable-pty` fixes as code (Apache-2.0), and its
  libghostty-vt fixes are carried as recorded patches (Gates). Each derived file names the herdr file it comes from,
  its license and that it was modified; `NOTICE` carries herdr's license. Read it in `.upstream/herdr`, a read-only
  clone at that commit, git-ignored like upstream's (when it is missing, clone `HERDR_REPO` and check out
  `HERDR_REV`). herdr is the source of the server's design and code, not of what the app shows: the `live.*` group,
  its error strings and the attach contract follow upstream, and none of herdr's names, paths or sockets reach the
  build.
- The Linux fork (`../agterm-linux`) is consulted only for Linux glue upstream has no counterpart for: XDG paths, and
  what its GTK build expects of another instance (single-instance forwarding). Never port from it.
- Where upstream and the fork differ, upstream wins (`conformance/docs/fork-bugs.md` lists the cases the suite pins).
  A capability the fork claims is not a target by itself; the build claims the list in rust-build.md > Rust target
  profile.
- Upstream and the fork are references for behavior, not for identity. The build is ngterm and shares nothing with
  them at runtime: its application id, state and config dirs, sockets, CLI, skill and install paths are its own
  (README holds them), and nothing falls back to, runs or reads an agterm or `agtermctl` path or name. Those names
  appear only where code and docs cite upstream or the fork, and in the harness adapters that drive the reference
  builds. A rename or a new path covers every place at once; keeping one on the old name "for compatibility" is not a
  choice to make alone.

## Plans

- `docs/plans/completed/` holds historical artifacts: what was decided and done at the time, not the product's
  current state. The code, README and CLAUDE.md are the current state; a completed plan is never read as a
  description of the product nor updated to match a later change.

## Porting a command group

1. Read upstream's implementation (`agtermCore/Sources/agtermCore/ControlDispatcher*.swift`, the `AppStore+*` files it
   calls, the app side in `agterm/Control/ControlServer+*.swift`), its unit tests (error strings, edge cases, field
   order), `site/commands.html` and the matching `.claude/rules/*.md`.
2. Implement in `agterm-core` against the `agterm-host` traits, with unit tests on the fakes; assert the full upstream
   error string on the dispatcher's output (the suite asserts only the part both Swift references share).
3. Wire the frontend side in `agterm-gpui` (terminal behavior in `agterm-term`), following upstream's UI
   (`agterm/Views`, `agterm/Ghostty`, the rules files) in GPUI/gpui-kit idioms.
4. Add the `ngtermctl` subcommands, ported from upstream's `agtermctlKit`.
5. Claim a capability (`version` result, rust-build.md > Rust target profile) only once its commands implement it.
6. Gate: every check README > Test lists (format, locked metadata, clippy with `-D warnings` on the workspace and on
   the frontend without its default features, the lint report, the tracing-features check, the vendored-source
   checks, the workspace tests headless, the suite's selftests), then a full `conformance/tools/run-rust-linux.sh` in
   the background; add the newly green ids to the ratchet (`check_ratchet.py`).

A new upstream release moves the catalog's tag (`conformance/tools/gen_catalog.py`) and `.upstream/agterm` together. The
build runs no zmx: what an upstream release changes in its zmx (the pin, its patches) is ported into `agterm-session` as
behavior, under `live.*` and the attach contract. A herdr bump is a change of its own, never part of an upstream
sync: its session-server changes are ported into `agterm-session`, its libghostty-vt patches through the Ghostty patch
record. The Ghostty pin follows ghostty main, not upstream's libghostty: its bump is a change of its own, with what it
regenerates (`build/pins.env` names it).

## Gates

- A ratchet id (`conformance/rust-passing.txt`) never goes red: a change that breaks one fixes it in the same change.
- No reruns: protocol, cli and behaviors tests wait on read-back, so a flaky one is a bug in the build or the test.
- A function over a complexity limit (`clippy.toml`) is split, not excused. The only exception is an outer
  `#[expect(clippy::..., reason = "...")]` on that one item; `#[allow]` and inner `#![allow]`/`#![expect]` are refused.
- Thresholds in `clippy.toml` are lowered, never raised. A `rust-toolchain.toml` bump is a change of its own.
- A Ghostty patch (`build/patches/ghostty-vt/`) exposes data the library already stores, as C API. A patch that
  changes the library's logic is taken only for a fix or a parity behavior Rust cannot reach from outside, and only
  recorded in that directory's README, with its reason, upstream status, a remove-when condition and its
  verification. A gap Rust can fill is filled in Rust. A pin bump rebases every patch and drops each one whose
  remove-when condition the new pin meets.
- Generated bindings change only through their generator scripts. The patched third-party crates under `vendor/` are
  their sources plus `build/patches/`: an edit there goes back into its patch (`scripts/vendor-gpui.sh`,
  `scripts/vendor-calloop.sh`, `scripts/vendor-portable-pty.sh`), and each script's check holds.

## Never touch the user's running agterm

Agents here usually run inside a pane of the user's live agterm, whose `AGTERM_*` variables (and `ZMX_*`, where that
agterm runs its sessions on zmx) point at it. Every dev instance, probe or `ngtermctl` call stays off it:

- Launch a checkout's build only through `scripts/run-gpui-dev.sh` (or the suite): the build shares the installed
  ngterm's application id and state (README holds them), so a plain launch is forwarded to the running instance
  through the D-Bus name it serves (`zbus`), or shares its state.
- Strip `AGTERM_*` and `ZMX_*` from the environment before launching anything (`run-gpui-dev.sh` and the harness do).
- Give every `ngtermctl` call an explicit `--socket` (a per-subcommand option, after the subcommand name). Without it
  the CLI resolves the live instance's socket.
- Use a private state dir, so the instance's live-session namespace, and with it the one session server serving it,
  is private too: its socket directory on Unix, its namespace directory and the named pipe named after it on Windows.
  Live sessions outlive the app by design: reap only through that namespace's own server, or the processes the
  namespace names (its server's command line, its sessions' attach-contract environment), never by name
  (`pkill agterm-session`, `taskkill /im`), and never run `agterm-session` against, or connect to the pipe of, any
  other namespace than that instance's.
- Run the GUI headless (the suite's private compositor and dbus, or `xvfb-run` with a private bus for `cargo test`),
  never on the user's desktop: nothing a run starts draws on the user's display or joins its compositor or bus. GPUI
  renders through Vulkan, so a headless app instance (the suite's, the window tests') renders on Mesa's lavapipe alone
  (`scripts/fetch-lavapipe.sh`), and on Wayland it needs a seat, which only a recent headless weston gives
  (`scripts/build-weston.sh` builds one where the system's is too old). A compositor started by hand copies the setup in
  `impls/base.py`; without a seat GPUI panics, and without lavapipe it would reach the machine's GPU. The render-path
  harnesses (README names them) also render offscreen on a hardware render node, each GPU in a child process of its
  own: that headless use of the GPU is part of the gates.
