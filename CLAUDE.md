# agterm-rs

agterm in Rust for macOS, Linux and Windows as equal targets: upstream macOS agterm (`umputun/agterm` v0.36.0) is the
parity target. The GTK4/libadwaita frontend (Linux only) runs libghostty in a `GtkGLArea`; the GPUI frontend, which
renders libghostty-vt itself, is the cross-platform one that replaces it. Build, run and test commands are in
[README.md](README.md); the suite and the ratchet in [conformance/README.md](conformance/README.md) and
[conformance/docs/rust-build.md](conformance/docs/rust-build.md).

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
| `agterm-core` | model, target resolver, command dispatcher (`commands/`, one module per group), events, keymap, hooks, persistence, settings, pane environment, zmx/restore logic | any |
| `agterm-server` | AF_UNIX control server: socket ownership (`flock`), one thread per connection, main-loop hop, stream handoff | any |
| `agterm-ctl` → `agtermctl` | clap CLI ported from upstream's `agtermctlKit`, socket resolution, output formatting | any |
| `agterm-ghostty-sys` | checked-in bindgen output (`scripts/gen-ghostty-bindings.sh`), links the vendored `libghostty.so`; not under the workspace lints | Linux, macOS |
| `agterm-ghostty` | safe wrapper over the sys crate: app, config, surface, input, actions, clipboard, text | Linux, macOS |
| `agterm-zmx` | the `SessionHost` over the patched zmx: launch inputs, client, inventory, reaps | Linux, macOS |
| `agterm-gtk` → `agterm` | GTK4/libadwaita frontend: windows, sidebar, deck, overlays, quick terminal (layer-shell), WebKitGTK HTML overlays (feature `html`), agent integrations | Linux |

The GTK main loop owns the main thread and the whole state; control connections hop onto it through `MainLoop`.
Work that needs no live terminal or widget belongs in `agterm-core`, unit-tested against the `agterm-host` fakes.

## Reference rule

- Upstream is the reference for the wire protocol, command semantics, error strings, `agtermctl` behavior and output,
  on-disk formats, model, core behavior and the UI. Port from its sources and Swift unit tests (the source of exact
  strings) in `.upstream/agterm`, a read-only clone git-ignored through `.git/info/exclude`; when it is missing:
  `git clone --depth 1 --branch v0.36.0 https://github.com/umputun/agterm .upstream/agterm`.
- The Linux fork (`../agterm-linux`) is consulted only for Linux glue upstream has no counterpart for: `GtkGLArea`
  hosting and the input handoff, XDG paths, the app id and state dir, the libghostty Linux build and its patches,
  WebKitGTK. Never port from it.
- Where upstream and the fork differ, upstream wins (`conformance/docs/fork-bugs.md` lists the cases the suite pins).
  A capability the fork claims is not a target by itself; the build claims the list in rust-build.md > Rust target
  profile.

## Porting a command group

1. Read upstream's implementation (`agtermCore/Sources/agtermCore/ControlDispatcher*.swift`, the `AppStore+*` files it
   calls, the app side in `agterm/Control/ControlServer+*.swift`), its unit tests (error strings, edge cases, field
   order), `site/commands.html` and the matching `.claude/rules/*.md`.
2. Implement in `agterm-core` against the `agterm-host` traits, with unit tests on the fakes; assert the full upstream
   error string on the dispatcher's output (the suite asserts only the part both Swift references share).
3. Wire the GTK side in `agterm-gtk`, following upstream's UI (`agterm/Views`, `agterm/Ghostty`, the rules files) in
   GTK4/libadwaita idioms; UI the suite cannot see is checked against `docs/linux-ui-checklist.md`.
4. Add the `agtermctl` subcommands, ported from upstream's `agtermctlKit`.
5. Claim a capability (`version` result, rust-build.md > Rust target profile) only once its commands implement it.
6. Gate: `cargo fmt --all --check`, clippy with `-D warnings`, `scripts/lint-report.sh`, `cargo test --workspace`, then
   a full `conformance/tools/run-rust-linux.sh` in the background; add the newly green ids with
   `check_ratchet.py --add`.

A new upstream release moves the catalog's tag (`conformance/tools/gen_catalog.py --tag`), `.upstream/agterm` and the
native pins in `build/pins.env` together.

## Gates

- A ratchet id (`conformance/rust-passing.txt`) never goes red: a change that breaks one fixes it in the same change.
- No reruns: protocol, cli and behaviors tests wait on read-back, so a flaky one is a bug in the build or the test.
- A function over a complexity limit (`clippy.toml`) is split, not excused. The only exception is an outer
  `#[expect(clippy::..., reason = "...")]` on that one item; `#[allow]` and inner `#![allow]`/`#![expect]` are refused.
- Thresholds in `clippy.toml` are lowered, never raised. A `rust-toolchain.toml` bump is a change of its own.

## Fork coexistence

On Linux, the build shares the installed fork's application id, state dir and control socket, so it can replace the
fork on this machine with its state. While an installed agterm runs, launch a checkout's build only through
`scripts/run-dev.sh` (or the suite): a plain launch is forwarded to the running instance, or both write the same state.
The socket lock keeps a second instance from taking over the live socket, not from sharing its state.

## Never touch the user's running agterm

Agents here usually run inside a pane of the user's live agterm, whose `AGTERM_*` and `ZMX_*` variables point at it.
Every dev instance, probe or `agtermctl` call stays off it:

- Strip `AGTERM_*` and `ZMX_*` from the environment before launching anything (`run-dev.sh` and the harness do).
- Give every `agtermctl` call an explicit `--socket` (a per-subcommand option, after the subcommand name). Without it
  the CLI resolves the live instance's socket.
- Use a private state dir, so the instance's live-session namespace is private too. Live sessions outlive the app by
  design: reap only the session processes of that instance's namespace, never by name (`pkill zmx`), and never run
  `zmx` outside that namespace.
- Run the GUI headless (the suite's private compositor and dbus, or `xvfb-run` for `cargo test`), never on the user's
  desktop. A compositor started by hand copies the setup in `impls/base.py`; without it GTK crashes realizing the
  window.
