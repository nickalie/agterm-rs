# What a Rust build must implement for the suite

## `version` extension

`version` is the harness's readiness probe: it is polled until it answers, and an error reply fails the launch.
Its result is the references' `app` block plus a `conformance` block only the Rust build reports:

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

SIGTERM must quit without a prompt within 10 seconds.
Live sessions keep their zmx sockets in `/tmp/agterm-zmx-<FNV-1a 64 of the resolved state dir>`, and every zmx or
session-host process carries `ZMX_DIR=<that dir>`: one left running after the stop fails it.
`agtermctl` is taken from `--ctl`, else from beside `--bin`.

## Capability vocabulary

Source of truth: `CAPABILITIES` in `agterm_conformance/impls/base.py`; `selftests/test_docs.py` keeps this table equal
to it and to the references' profiles. `host`: claimed only where the account's login shell is zsh (on Linux, also with zmx staged).

| capability | swift-macos | swift-linux | meaning |
|---|---|---|---|
| `posix-shell` | yes | yes | panes run /bin/sh, so `session type` + `session.text` round-trips are deterministic |
| `live-sessions` | host | host | restoreMode live keeps panes in zmx daemons that outlive the app |
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
