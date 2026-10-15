# What a Rust build must implement for the suite

## `version` extension

`version` is the harness's readiness probe: it is polled until it answers, and an error reply fails the launch.
Its result is the references' `app` block plus a `conformance` block only the Rust build reports, and no other key
(`assert_version_result` in `agterm_conformance/match.py`):

```json
{"app": {"version": "0.1.0", "commit": "abc1234"},
 "conformance": {"parityLevel": "0.36.0", "capabilities": ["posix-shell", "live-sessions"]}}
```

- `app.version`: a non-empty string; `app.commit` is optional, and `agtermctl version` prints `version (commit)`.
  `tree` carries the same object as `tree.app`.
- `conformance.parityLevel`: the upstream version whose behavior the build claims, a dotted version with an optional
  `v` (no suffix such as `-beta`).
  A test marked `since(V)` above it skips as `not-yet-at-level`.
- `conformance.capabilities`: names from the vocabulary below; a test whose `requires` names one missing skips as
  `unsupported`.
  Names outside the vocabulary match nothing.
- A missing `conformance` block, or a run without `--bin`, puts the build below every level: every test skips and
  parity reads 0%.
  A malformed block fails the launch.
- The profile is read once per run, from the first instance launched, or from a probe launch.

## Launch contract

The harness execs the binary with:

- `HOME`, `XDG_{CONFIG,DATA,STATE,CACHE}_HOME`, `AGTERM_STATE_DIR` and `AGTERM_CONTROL_SOCKET` under a fresh
  `/tmp/agt-*` root; the build binds the control socket at `AGTERM_CONTROL_SOCKET` (under 104 bytes);
- on Linux, a unique `AGTERM_APP_ID` it must use as its application id, a private `XDG_RUNTIME_DIR`, and
  `WAYLAND_DISPLAY`/`DBUS_SESSION_BUS_ADDRESS` naming the instance's own headless weston and bus;
- `SHELL=/bin/sh`, `PATH=/usr/bin:/bin`, and `<state>/config/ghostty.conf` holding `command = /bin/sh`,
  `copy-on-select = false` and `clipboard-write = deny`;
- `<state>/windows` present, which skips the first-run welcome, and `<state>/settings.json` when a test seeds settings.

The GPUI frontend (`--rust-frontend gpui`) gets, on top of that:

- `VK_DRIVER_FILES` (and `VK_ICD_FILENAMES`) naming lavapipe's ICD manifest alone, `LIBGL_ALWAYS_SOFTWARE=1`,
  Mesa's EGL vendor, and no inherited `VK_*`, `WGPU_*`, `__NV_*`, `__GLX_*`, `DRI_PRIME` or `MESA_LOADER_*`: it renders
  on lavapipe and llvmpipe only, never on the machine's GPU driver, and a launch without lavapipe fails;
- a headless weston started with `--fake-seat`, since GPUI needs a `wl_seat`.

SIGTERM must quit without a prompt within 10 seconds.
Live sessions keep their zmx sockets in `/tmp/agterm-zmx-%016x` (lowercase hex) of the FNV-1a 64 hash over the UTF-8
bytes of the state dir's realpath, with a leading `/private` dropped on macOS when the shorter path exists
(`zmx_socket_directory` in `impls/base.py`).
Every zmx or session-host process carries `ZMX_DIR=<that dir>`: one left running after the stop fails it.
`agtermctl` is taken from `--ctl`, else from beside `--bin`.

## Capability vocabulary

Source of truth: `CAPABILITIES` in `agterm_conformance/impls/base.py`; `selftests/test_docs.py` keeps this table equal
to it and to the references' profiles. `host`: claimed only where the account's login shell is zsh (on Linux, also with zmx staged).
A row with both reference columns empty and a meaning starting `Rust-only` is an extension of the Rust build, see
Rust-only extensions below.

| capability | swift-macos | swift-linux | meaning |
|---|---|---|---|
| `posix-shell` | yes | yes | panes run /bin/sh, so `session type` + `session.text` round-trips are deterministic |
| `live-sessions` | host | host | restoreMode live keeps panes in zmx daemons that outlive the app |
| `live-any-shell` |  |  | Rust-only: Live sessions in a zsh, bash or fish login shell, each started with Ghostty's shell integration; claimed where the password-database login shell is one of the three and zmx is staged |
| `window-geometry` | yes |  | window x/y position is settable through `window.move`, read back as `geometry` and restored |
| `window-minimize` | yes |  | `window.minimize` and `window.new --minimized` take effect and read back as `minimized` |
| `select-takes-frontmost` | yes |  | `window.select`/`window.go` make their window `active` without waiting on window focus |
| `status-shape` | yes |  | `session.status` validates `shape` and reads it back as `statusShape` |
| `type-rejects-nul` | yes |  | `session.type` refuses text carrying a NUL rather than typing the run before it |
| `resize-echoes-ratio` | yes |  | `session.resize` answers with the applied ratio in `result.ratio` |
| `drop-placement` | yes |  | `session.move --after/--before` places like a sidebar drop, also above its own anchor |
| `overlay-size-validated` | yes |  | `session.overlay.open` refuses a `sizePercent` outside 1...100 rather than clamping it |
| `modal-blocks-covers` | yes |  | a pending GUI ask refuses control-driven covers such as `session.search` with `ask pending` |
| `pick-names-pending-ask` | yes |  | `pick.open` under a pending GUI ask answers `ask already pending`, naming the ask |
| `quick-validates-input` | yes |  | `quick.type` refuses a NUL, `quick.text` `--all` with `--lines` and a `--lines` below 1 |
| `sidebar-min-width` | yes |  | `sidebar.width` narrows the sidebar down to the 160-point floor and reads that back |
| `theme-slots-by-sync` | yes |  | `theme.set`/`theme.list` report `theme` only while not following the system appearance, `light`/`dark` only while following |
| `keymap-menu` | yes |  | `keymap.list` reports the menu bar's live key equivalents in `menu` |
| `config-reload-counts` | yes |  | `config.reload` answers with the ghostty config's diagnostic count in `result.count` |
| `hidden-split-holds-focus` |  | yes | `session.focus` on a hidden split reads back as `splitFocused`; upstream's shown left pane takes the focus back while its window is key |
| `capture-reads-command-panes` |  | yes | `restore.capture` records a `--command` session's program; upstream reads only the foreground group's leader, there the setuid `login` wrapper whose argv a user cannot read |

## Rust target profile

The capabilities the Rust build claims are held to upstream's contract: the build takes upstream's branch of every
capability whose behavior lives in upstream's own code, and claims each one in the task that implements its command,
never ahead of it. By the end of the implementation plan (`docs/plans/20261012-rust-linux-implementation.md`) it
claims:

- `posix-shell`: panes run `/bin/sh` from the harness's ghostty config, as both references;
- `status-shape`, `type-rejects-nul`: upstream validates both in `session.status` and `session.type`;
- `drop-placement`: upstream's `session.move` places like a sidebar drop;
- `select-takes-frontmost`: upstream's `windowSelect` calls `takeFrontmost` itself, a model change, not a compositor
  focus;
- `resize-echoes-ratio`, `hidden-split-holds-focus`: upstream's documented model contract (`control-api.md`, "works
  shown or hidden; read `splitFocused`", and `AppStorePaneTests`); macOS's reversion is an AppKit first-responder
  effect;
- `quick-validates-input`, `modal-blocks-covers`, `pick-names-pending-ask`, `overlay-size-validated`,
  `sidebar-min-width`, `theme-slots-by-sync`, `config-reload-counts`: upstream's validation and read-back;
- `keymap-menu`: once a GTK menu carries the actions' live accelerators;
- `live-sessions`: by host, where the login shell is zsh and zmx is staged;
- `live-any-shell`: by host, live sessions for bash and fish login shells too, the one Rust-only extension (below):
  claimed together with `live-sessions` for a zsh login shell, alone for bash or fish.

It does not claim:

- `window-geometry`, `window-minimize`: Wayland gives a client no window position and no minimized read-back; their
  tests branch, and the Rust build passes the other branch;
- `capture-reads-command-panes`: it contradicts upstream's intent, `ForegroundProcess.command` reads only the
  foreground group's leader, so a `--command` pane's creation command keeps the exec path, `--wait` and
  close-on-exit.

The table above stays the references' profiles; this list is the Rust build's.

## Rust-only extensions

A Rust-only extension is a capability no reference claims, so its tests (`requires` on it) skip as `unsupported` on
both references and the reference discipline holds them to nothing there: the Rust CI validates them instead, with
`--must-pass` on the capability in every job whose host can claim it.
Today there is one, `live-any-shell`: a Live pane runs the password database's login shell in its zmx daemon, zsh as
upstream, bash and fish as well, each started with Ghostty's shell integration the way libghostty starts a shell it
spawns itself (`ZDOTDIR` for zsh, `ENV` in POSIX mode for bash, `XDG_DATA_DIRS` for fish).
Its tests are in `tests/protocol/test_live_any_shell.py`; a test that branches on Live being active
(`test_restore.py::test_seeded_live_mode_is_requested_at_launch`) takes the fallback branch only when the build has
neither `live-sessions` nor `live-any-shell`.
The jobs of `.github/workflows/rust-linux.yml` cover the three shells, each with `--must-pass live-any-shell`: the
default job's login shell is bash, the `conformance-fish` job's fish, the `conformance-zsh` job's zsh. The zsh job,
the one host where the build claims `live-sessions`, also runs with `--must-pass live-sessions`, as reference-macos.

## Ratchet

`conformance/tools/run-rust-linux.sh` builds the workspace (release), stages it with `scripts/stage.sh` and runs the
protocol, cli and behaviors layers with `--impl rust` under run-reference-linux.sh's isolation.
pytest writes each test's merged outcome with `--nodeid-report` next to the report in
`conformance/reports/rust-linux/`, and `conformance/tools/check_ratchet.py` holds `conformance/rust-passing.txt`
against it: one pytest node id per line, verbatim and sorted.
A listed id that fails, errors (a teardown error included) or skips breaks the run, except an `unsupported` skip
for a host-dependent capability (`live-sessions`, `live-any-shell`), which counts as not run.
A collection error or a `--must-pass` problem breaks it too.
When pytest refuses its arguments (a mistyped path after `--`) no report is written and the checker exits 2.
The checker prints the newly passing ids not yet listed; `--add` writes them into the list.
The script exits with the ratchet's status, or 1 when an instance root was left behind, whatever pytest's own status.

## GPUI build

While the GTK and the GPUI frontends coexist, each is held by a ratchet of its own: `rust-passing.txt` for the GTK
build, `rust-gpui-passing.txt` for the GPUI one, which `run-rust-linux.sh --frontend gpui` builds, stages
(`scripts/stage.sh --frontend gpui`: `bin/agterm-gpui`, `agtermctl`, `zmx` and the same `share/`, no `lib/`) and holds.
The GPUI build claims its capabilities task by task from none; its ratchet grows with `check_ratchet.py --add` and must
cover the GTK one before the switch replaces the GTK build.

GPUI renders through Vulkan, so its runs need Mesa's lavapipe, from `scripts/fetch-lavapipe.sh`: `AGTERM_LAVAPIPE_ICD`
when set; the distribution's package where one is installed (Ubuntu/Debian/Fedora `mesa-vulkan-drivers`, Arch
`vulkan-swrast`; the CI's source); else, on a rootless host with an Arch-compatible userland, the pinned Arch
`vulkan-swrast` package (`LAVAPIPE_*` in `build/pins.env`), checked against its sha256 and unpacked into
`${AGTERM_VENDOR}/lavapipe-<key>/`, refused where its libraries (libLLVM, glibc) do not resolve. No lavapipe is an
error, never a fallback to a hardware driver.
