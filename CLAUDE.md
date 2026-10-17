# agterm-rs

agterm in Rust for macOS, Linux and Windows as equal targets: upstream macOS agterm (`umputun/agterm` v0.36.0) is the
parity target. The one frontend is GPUI's, which renders libghostty-vt itself and replaced the GTK4/libadwaita one;
today it is built and tested on Linux only, so its macOS and Windows forms are open work under the platform rules
below, not a later phase. Build, run and test commands are in [README.md](README.md); the suite and the ratchet in
[conformance/README.md](conformance/README.md) and [conformance/docs/rust-build.md](conformance/docs/rust-build.md).

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
| `agterm-core` | model, target resolver, command dispatcher (`commands/`, one module per group), events, keymap and Ghostty's keybind actions, the Ghostty config parser and its diagnostics, hooks, persistence, settings, pane environment, zmx/restore logic | any |
| `agterm-server` | AF_UNIX control server: socket ownership (`flock`), one thread per connection, main-loop hop, stream handoff | any |
| `agterm-ctl` → `agtermctl` | clap CLI ported from upstream's `agtermctlKit`, socket resolution, output formatting | any |
| `agterm-ghostty-vt-sys` | checked-in bindgen output over libghostty-vt's official C API (`scripts/gen-ghostty-vt-bindings.sh`), links the vendored static library; not under the workspace lints | any |
| `agterm-ghostty-vt` | safe wrapper over the sys crate: terminal, render state, effects, key and mouse encoders, selection and its gesture, formatter, search, Kitty graphics | any |
| `agterm-term` | the toolkit-neutral pane core: a libghostty-vt terminal fed from a PTY by an IO thread per pane, input, selection, search, read-back, launch and terminal identity, frames and events in its own types | Linux |
| `agterm-zmx` | the `SessionHost` over the patched zmx: launch inputs, client, inventory, reaps | Linux, macOS |
| `agterm-app` | frontend-neutral app services: the hooks' `ProcessLauncher`, the `/proc` `ProcessTable`, logging, the restore launch decision, quick-terminal placement, the agent integrations' installers, the remote presentation process | Linux |
| `agterm-wpe-sys`, `agterm-wpe-platform-sys`, `agterm-wpe-javascriptcore-sys` | checked-in gir output over WPE WebKit, one crate per GObject namespace (`scripts/gen-wpe-bindings.sh`) | Linux |
| `agterm-wpe` | WPE WebKit host for the HTML overlays: offscreen pages on an engine thread of their own (or in a worker process), frames out as dmabufs or SHM | Linux |
| `agterm-gpui` → `agterm` | GPUI/gpui-kit frontend: windows, sidebar, deck, the terminal element (grid, sprites, Kitty graphics), overlays, quick terminal (layer-shell, a window where there is none), HTML overlays over `agterm-wpe` (feature `html`), single instance, agent integrations | Linux |

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
- The Linux fork (`../agterm-linux`) is consulted only for Linux glue upstream has no counterpart for: XDG paths, and
  what its GTK build expects of another instance (single-instance forwarding). Never port from it.
- Where upstream and the fork differ, upstream wins (`conformance/docs/fork-bugs.md` lists the cases the suite pins).
  A capability the fork claims is not a target by itself; the build claims the list in rust-build.md > Rust target
  profile.

## Porting a command group

1. Read upstream's implementation (`agtermCore/Sources/agtermCore/ControlDispatcher*.swift`, the `AppStore+*` files it
   calls, the app side in `agterm/Control/ControlServer+*.swift`), its unit tests (error strings, edge cases, field
   order), `site/commands.html` and the matching `.claude/rules/*.md`.
2. Implement in `agterm-core` against the `agterm-host` traits, with unit tests on the fakes; assert the full upstream
   error string on the dispatcher's output (the suite asserts only the part both Swift references share).
3. Wire the frontend side in `agterm-gpui` (terminal behavior in `agterm-term`), following upstream's UI
   (`agterm/Views`, `agterm/Ghostty`, the rules files) in GPUI/gpui-kit idioms; UI the suite cannot see is checked
   against `docs/linux-ui-checklist.md`.
4. Add the `agtermctl` subcommands, ported from upstream's `agtermctlKit`.
5. Claim a capability (`version` result, rust-build.md > Rust target profile) only once its commands implement it.
6. Gate: every check README > Test lists (format, locked metadata, clippy with `-D warnings` on the workspace and on
   the frontend without its default features, the lint report, the tracing-features check, the vendored-source
   checks, the workspace tests headless, the suite's selftests), then a full `conformance/tools/run-rust-linux.sh` in
   the background; add the newly green ids to the ratchet (`check_ratchet.py`).

A new upstream release moves the catalog's tag (`conformance/tools/gen_catalog.py`), `.upstream/agterm` and the zmx
pin in `build/pins.env` together. The Ghostty pin follows ghostty main, not upstream's libghostty: its bump is a change
of its own, with what it regenerates (`build/pins.env` names it).

## Gates

- A ratchet id (`conformance/rust-passing.txt`) never goes red: a change that breaks one fixes it in the same change.
- No reruns: protocol, cli and behaviors tests wait on read-back, so a flaky one is a bug in the build or the test.
- A function over a complexity limit (`clippy.toml`) is split, not excused. The only exception is an outer
  `#[expect(clippy::..., reason = "...")]` on that one item; `#[allow]` and inner `#![allow]`/`#![expect]` are refused.
- Thresholds in `clippy.toml` are lowered, never raised. A `rust-toolchain.toml` bump is a change of its own.
- Generated bindings change only through their generator scripts. The patched third-party crates under `vendor/` are
  their sources plus `build/patches/`: an edit there goes back into its patch (`scripts/vendor-gpui.sh`,
  `scripts/vendor-calloop.sh`), and each script's check holds.

## Never touch the user's running agterm

Agents here usually run inside a pane of the user's live agterm, whose `AGTERM_*` and `ZMX_*` variables point at it.
Every dev instance, probe or `agtermctl` call stays off it:

- Launch a checkout's build only through `scripts/run-gpui-dev.sh` (or the suite): the build shares the installed
  agterm's application id and state (README holds them), so a plain launch is forwarded to the running instance
  through the D-Bus name it serves (`zbus`), or shares its state.
- Strip `AGTERM_*` and `ZMX_*` from the environment before launching anything (`run-gpui-dev.sh` and the harness do).
- Give every `agtermctl` call an explicit `--socket` (a per-subcommand option, after the subcommand name). Without it
  the CLI resolves the live instance's socket.
- Use a private state dir, so the instance's live-session namespace is private too. Live sessions outlive the app by
  design: reap only the session processes of that instance's namespace, never by name (`pkill zmx`), and never run
  `zmx` outside that namespace.
- Run the GUI headless (the suite's private compositor and dbus, or `xvfb-run` with a private bus for `cargo test`),
  never on the user's desktop or GPU. GPUI renders through Vulkan, so a headless run renders on Mesa's lavapipe alone
  (`scripts/fetch-lavapipe.sh`), and on Wayland it needs a seat, which only a recent headless weston gives
  (`scripts/build-weston.sh` builds one where the system's is too old). A compositor started by hand copies the setup in
  `impls/base.py`; without a seat GPUI panics, and without lavapipe it would reach the machine's GPU.
