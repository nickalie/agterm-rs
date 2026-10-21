# What a Rust build must implement for the suite

## `version` extension

`version` is the harness's readiness probe: it is polled until it answers, and an error reply fails the launch.
Its result is the references' `app` block plus a `conformance` block only the Rust build reports, and no other key
(`assert_version_result` in `agterm_conformance/match.py`):

```json
{"app": {"version": "0.1.0", "commit": "abc1234"},
 "conformance": {"parityLevel": "0.36.0", "capabilities": ["posix-shell", "live-sessions"]}}
```

- `app.version`: a non-empty string; `app.commit` is optional, and the CLI's `version` prints `version (commit)`.
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

The Rust build (GPUI) gets, on top of that:

- `VK_DRIVER_FILES` (and `VK_ICD_FILENAMES`) naming lavapipe's ICD manifest alone, `LIBGL_ALWAYS_SOFTWARE=1`,
  Mesa's EGL vendor, and no inherited `VK_*`, `WGPU_*`, `__NV_*`, `__GLX_*`, `DRI_PRIME` or `MESA_LOADER_*`: it renders
  on lavapipe and llvmpipe only, never on the machine's GPU driver, and a launch without lavapipe fails;
- a headless weston started with `--fake-seat`, since GPUI needs a `wl_seat`: weston 15.0 or later (the system's, or
  the one `scripts/build-weston.sh` vendors where the system's is older).

SIGTERM must quit without a prompt within 10 seconds.
Live sessions keep their sockets in a namespace directory named by the FNV-1a 64 hash (`%016x`, lowercase hex) over
the UTF-8 bytes of the state dir's realpath, with a leading `/private` dropped on macOS when the shorter path exists:
upstream's and the fork's zmx sockets in `/tmp/agterm-zmx-<hash>` (`upstream_socket_directory` in
`agterm_conformance/vocabulary.py`), the Rust build's session server in `/tmp/ngterm-live-<hash>`
(`live_socket_directory`); the adapter's vocabulary names it, prefixed with the adapter's `app_dir_name` (the
instance's `live_dir`). Every zmx or session-host process of a reference carries `ZMX_DIR=<that dir>`; the Rust
build's one server per instance runs as `agterm-session server <that dir>`, and its sessions' programs and their
attach clients carry `AGTERM_LIVE_DIR=<that dir>`. One left running after the stop fails it (the server gets its idle
grace to stop by itself once its sessions are reaped).
The CLI (`agtermctl` for the Swift builds, `ngtermctl` for the Rust one: `Adapter.ctl_name`) is taken from `--ctl`,
else from beside `--bin`.

## Capability vocabulary

Source of truth: `CAPABILITIES` in `agterm_conformance/impls/base.py`; `selftests/test_docs.py` keeps this table equal
to it and to the references' profiles. `host`: claimed only where the account's login shell is zsh (on Linux, also with zmx staged).
A row with both reference columns empty and a meaning starting `Rust-only` is an extension of the Rust build, see
Rust-only extensions below.

| capability | swift-macos | swift-linux | meaning |
|---|---|---|---|
| `posix-shell` | yes | yes | panes run /bin/sh, so `session type` + `session.text` round-trips are deterministic |
| `live-sessions` | host | host | restoreMode live keeps panes in live sessions that outlive the app (upstream's zmx daemons; the Rust build's one session server per instance, agterm-session) |
| `live-any-shell` |  |  | Rust-only: Live sessions in a zsh, bash, fish or PowerShell login shell, each started with its shell integration (Ghostty's, ngterm's own for PowerShell); claimed where the password-database login shell is one of them and agterm-session is staged |
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
never ahead of it. Since the implementation plan (`docs/plans/completed/20261012-rust-linux-implementation.md`) it
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
- `keymap-menu`: the app menu (`agterm-gpui`'s `app_menu.rs`) carries the actions' live accelerators;
- `live-sessions`: by host, where the login shell is zsh and `agterm-session` is staged;
- `live-any-shell`: by host, live sessions for bash, fish and PowerShell login shells too, the one Rust-only
  extension (below): claimed together with `live-sessions` for a zsh login shell, alone for the others.

It does not claim:

- `window-geometry`, `window-minimize`: Wayland gives a client no window position and no minimized read-back; their
  tests branch, and the Rust build passes the other branch;
- `capture-reads-command-panes`: it contradicts upstream's intent, `ForegroundProcess.command` reads only the
  foreground group's leader, so a `--command` pane's creation command keeps the exec path, `--wait` and
  close-on-exit.

The table above stays the references' profiles; this list is the Rust build's.

Restore is lazy, as in the GTK fork and unlike upstream's eager deck (the user's call, 2026-10-19): only each
window's selected restored session spawns at launch, the others once shown or once a command needs their terminal.
No capability marks it: the suite already waits for a restored session only once it is selected, since the fork
mounts a pane only on screen.

## Rust-only extensions

A Rust-only extension is a capability no reference claims, so its tests (`requires` on it) skip as `unsupported` on
both references and the reference discipline holds them to nothing there: the Rust CI validates them instead, with
`--must-pass` on the capability in every job whose host can claim it.
Today there is one, `live-any-shell`: a Live pane runs the password database's login shell in its live session, zsh
as upstream, bash, fish and PowerShell as well, each started with its shell integration the way libghostty starts a
shell it spawns itself (`ZDOTDIR` for zsh, `ENV` in POSIX mode for bash, `XDG_DATA_DIRS` for fish; ngterm's own
script for PowerShell, which Ghostty does not integrate).
Its tests are in `tests/protocol/test_live_any_shell.py`; a test that branches on Live being active
(`test_restore.py::test_seeded_live_mode_is_requested_at_launch`) takes the fallback branch only when the build has
neither `live-sessions` nor `live-any-shell`.
The `conformance` job of `.github/workflows/rust-linux.yml` is a matrix over the three login shells, each leg with
`--must-pass live-any-shell`: bash, fish and zsh. The zsh leg, the one host where the build claims `live-sessions`,
also runs with `--must-pass live-sessions`, as reference-macos.

## The `live` group

The Rust build answers upstream's control-protocol group `zmx.*` as `live.*`, a deliberate deviation from upstream: no
`zmx` in its code, its control protocol or its CLI, and the session server (`agterm-session`) in place of zmx itself. An upstream
Mac and this build can therefore no longer view each other's sessions (`live.tree HOST` runs `ngtermctl live tree` on
the far side, an attached row's stream `ngtermctl live present`), and neither can this build and a Rust build from
before the rename.

| upstream | Rust build |
|---|---|
| `zmx.list`, `zmx.prune`, `zmx.kill`, `zmx.reset`, `zmx.tree`, `zmx.attach`, `zmx.present`, `zmx.screen` | `live.list`, `live.prune`, `live.kill`, `live.reset`, `live.tree`, `live.attach`, `live.present`, `live.screen` |
| `agtermctl zmx <subcommand>` | `ngtermctl live <subcommand>` |
| `result.zmx` (the inventory) | `result.live` |
| `tree.session.backedByZmx`, `surfaces[].backedByZmx` | `backedByLive` |
| `AGTERM_UITEST_ENABLE_ZMX` | `AGTERM_UITEST_ENABLE_LIVE` |

Every other field, every semantic and every error string that names no zmx is upstream's. The strings that named zmx:

| upstream | Rust build |
|---|---|
| `zmx.kill requires an explicit --target`, `… requires --pane left\|right`, `… requires --force`, `… needs a session id; 'active' is not accepted`, `… needs a window id; 'active' is not accepted` | the same under `live.kill` |
| `zmx.screen requires a daemon name` | `live.screen requires a daemon name` |
| `zmx.reset requires --force`, `… requires Live sessions mode both configured and active for this launch`, `… could not read the live session list`, `… refused: the pane inventory is incomplete`, `… found no live session to reset` | the same under `live.reset` (and `live.reset is not supported on this platform`, the build's own) |
| `zmx.present requires a session` | `live.present requires a session` |
| `zmx.attach requires a host`, `zmx.attach requires a remote session` | the same under `live.attach` |
| `zmx is unavailable in this instance` | `live sessions are unavailable in this instance` |
| `could not read the zmx session list` | `could not read the live session list` |
| `could not re-read the zmx session list before pruning` | `could not re-read the live session list before pruning` |
| `could not read the zmx screen of NAME` | `could not read the live session screen of NAME` |
| `NAME did not confirm the kill; zmx cleaned up a stale socket and the daemon may still be running` | `… the session host cleaned up a stale socket …` |
| `the pane's zmx daemon did not accept the input` | `the pane's live session did not accept the input` |
| `the remote agterm is too old to report its zmx endpoint` | `… its live-session endpoint` |
| `the zmx executable path is not absolute`, `the zmx executable is unavailable` (`restore.mode`'s `unavailableReason`) | `the live-session executable path is not absolute`, `the live-session executable is unavailable` |
| `this pane has no zmx daemon; run it in a main or split pane started under Live sessions` (`clipboard set`) | `this pane has no live session; run it in a main or split pane started under Live sessions` |
| `no zmx next to this agtermctl[; looked for PATH]`, `zmx print exited N: …` (`clipboard set`) | `no live-session executable next to this ngtermctl[; looked for PATH]`, `the live-session print exited N: …` |

The help texts of `ngtermctl live` and `restore mode` speak of live-session daemons and the session host the same way.

The session server (`agterm-session`) takes zmx's place with names of its own, which no control-protocol field or
error string carries: `bin/agterm-session` and `share/ngterm/session-BUILD` in the stage (upstream's bundled zmx and
its build stamp), `AGTERM_SESSION_PATH` (upstream's `AGTERM_ZMX_PATH`), the `session-build.json` record in the state
dir (`zmx-build.json`), the namespace `/tmp/ngterm-live-<hash>` (`/tmp/agterm-zmx-<hash>`), its attach contract's
`agterm-session attach` line and `AGTERM_LIVE_*` variables (zmx's `ZMX_*`), and the role title `ngterm-live-role;`
(the patched zmx's `zmx-role;`). ngterm's own panes attach to their sessions in process through the server's client
library, so no attach client runs under the app (upstream's live panes run `zmx attach` in their PTYs); the remote
path and a terminal outside the app run the attach line. `clipboard set` prints through `agterm-session print`, so its texts are the table's
above with the print's own complaint after `the live-session print exited N: ` (the session gone, or no server:
`agterm-session print: no live session named NAME`) where upstream shows zmx's.

The suite's tests, `catalog.yaml` and their `covers(...)` markers keep upstream's names, since the Swift references
answer to them; no test id changes, so the ratchet keeps its ids. Each adapter carries a vocabulary object
(`agterm_conformance/vocabulary.py`) for every value that depends on it: command names, the result and tree keys, the
CLI argv, the error-text templates, the environment names, the socket directory and the leading client. The Swift
adapters' is upstream's own (`UPSTREAM`), the Rust adapter's renames the group (`LIVE`); `base.py`, `cli_format.py`
and the shared tests read every such value through `ctl.vocabulary`, `cli.vocabulary` or the instance's. `LIVE`
refuses an upstream text that names zmx and that it does not list, and `selftests/test_vocabulary.py` checks each
listed text against the Rust sources.

The bundled agent skill (`resources/skills/agterm`, installed as `ngterm`) is upstream's with
`build/patches/skill-agterm/` applied (the `live` names, this build's `ngterm` and `ngtermctl`, then the attach
contract's `AGTERM_LIVE_*`):
`.claude/skills/agterm-upstream-sync/scripts/sync-resources.sh` rebuilds it as the new upstream tree plus those
patches on every sync, and `sync-resources.sh --check TAG TAG` with the pinned tag verifies the copy still equals it.

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
The report's run header names its mode: `full`; `ratchet-only` (`run-rust-linux.sh --ratchet-only`, the listed ids
plus a task's `--also`/`--also-k` selections), judged like a full run, a listed id not collected warned about; or
`partial` (any narrower selection), where only the listed ids that ran are judged. No mode removes an id.
A ratchet-only run is a task's routine check; a full run is required at milestones: the GPUI switch, and any change to
the harness or, broadly, to the shared crates (conformance/README.md > Rust runner and ratchet).
The script exits with the ratchet's status, or 1 when an instance root was left behind, whatever pytest's own status.

## GPUI build

The Rust build is the GPUI one since the switch (the GPUI plan's Task 34), which removed the GTK frontend: the binary
is `agterm-gpui`'s `agterm`, and `rust-passing.txt` holds it (the GPUI build's own ratchet, kept beside the GTK one
until the switch, was folded into it).

Build. The native inputs come first, each into its vendored prefix (README > Native dependencies):
`scripts/build-ghostty-vt.sh` (the patched libghostty-vt source, which `agterm-ghostty-vt-sys`'s build script builds
with Zig, offline, and Ghostty's resources at `LIBGHOSTTY_VT_REV`) and `scripts/fetch-fonts.sh` (the fonts the
terminal embeds); the live sessions' server, `agterm-session`, is a workspace binary. `run-rust-linux.sh` then builds
the release workspace without what links WPE WebKit and `agterm-gpui` with its default `html` feature, so the staged
app shows HTML overlays and needs a WPE WebKit (README > WPE WebKit; the CI's is Arch's `wpewebkit`). `--stage DIR`
runs a tree staged elsewhere instead.

Staging. `scripts/stage.sh` assembles the tree the harness launches, `target/conformance-stage` by default:
`bin/agterm`, `bin/ngtermctl` (the harness takes it from beside `--bin`; upstream's and the fork's CLI is `agtermctl`,
and the CLI tests expect the adapter's name in usage lines) and `bin/agterm-session` with
`share/ngterm/session-BUILD` (the staged server's build id), Ghostty's `share/terminfo` (`xterm-ghostty`) and
`share/ghostty` (themes, shell integration) from the libghostty-vt prefix, agterm's own themes beside Ghostty's, and
the agent integrations' payloads under `share/ngterm` (not upstream's `share/agterm`: installed at prefix `~/.local`,
that would be the fork's state dir). There is no `lib/`: libghostty-vt is linked statically, and nothing of GTK,
libghostty or layer-shell is linked at all.

Lavapipe. GPUI renders through Vulkan (wgpu), so every run renders on Mesa's lavapipe, from
`scripts/fetch-lavapipe.sh`: `AGTERM_LAVAPIPE_ICD` when set; the distribution's package where one is installed
(Ubuntu/Debian/Fedora `mesa-vulkan-drivers`, Arch `vulkan-swrast`; the CI's source); else, on a rootless host with an
Arch-compatible userland, the pinned Arch `vulkan-swrast` package (`LAVAPIPE_*` in `build/pins.env`), checked against
its sha256 and unpacked into `${AGTERM_VENDOR}/lavapipe-<key>/`, refused where its libraries (libLLVM, glibc) do not
resolve. `run-rust-linux.sh` exits 2 without one and hands its manifest to the harness, which makes it the only Vulkan
driver of every launch (Launch contract above). No lavapipe is an error, never a fallback to a hardware driver.
`agterm-gpui`'s process tests take the same manifest from the same script (`tests/support/lavapipe.rs`).

Seat. GPUI needs a `wl_seat`, which a headless weston offers only with `--fake-seat` (weston 15.0 and later):
`run-rust-linux.sh` takes the system's weston when it has the flag, else the one `scripts/build-weston.sh` built into
its vendored prefix (`WESTON_*` in `build/pins.env`; Ubuntu 26.04's is 14), and stops naming that script when there is
neither.

Probes. Three scripts beside the runner check what the suite cannot see, on the stage a run left (their `--bin`
default; CI runs them after the bash leg): `ghostty_probe.py` (a terminal draws its first frame), `webkit_probe.py`
(a page loads in WPE WebKit's sandbox) and `chrome_probe.py` (the title bar, the sidebar and the terminal draw the
theme's background, the sidebar tint washing the sidebar alone).
