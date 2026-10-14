# GPUI frontend in place of agterm-gtk

## Overview

- Replace the GTK4/libadwaita frontend (`agterm-gtk`) with one built on GPUI (Zed's UI framework), so a single UI
  codebase can later carry macOS and Windows. This plan targets Linux only; nothing in it may close the other platforms.
- The terminal moves from full libghostty (its own OpenGL renderer inside a `GtkGLArea`) to **libghostty-vt** (Ghostty's
  parser and terminal state, no renderer) with the grid drawn by GPUI, the way Zed draws its terminal. GPUI cannot host
  a foreign GL/Metal view, and full libghostty has no Windows apprt; libghostty-vt keeps upstream's VT semantics.
  libghostty-vt is used through its **official C API** (`include/ghostty/vt.h` at a pinned Ghostty commit) with our own
  checked-in bindgen `-sys` crate and safe wrapper, as `agterm-ghostty-sys`/`agterm-ghostty` do for full libghostty
  (user decision 2026-10-14, Task 4; the third-party `libghostty-vt` crate is not used).
- `agterm-proto`, `agterm-host`, `agterm-server`, `agterm-ctl` and `agterm-zmx` stay as they are, and so does
  `agterm-core` apart from two additions (the ghostty config parser and keybind actions that libghostty used to own):
  the new frontend implements the same `agterm-host` traits. Everything that needs no window is tested without one.
- The GTK frontend stays in the workspace until the GPUI one reaches parity **including HTML overlays**; the switch is
  gated on the conformance ratchet and the Linux UI checklist (Task 34).
- Design decisions come from the brainstorm of 2026-10-14 (user and assistant); the reasons are kept below so later
  tasks do not reopen them.

## Context (from discovery)

- Frontend today: `crates/agterm-gtk` (~38k lines), `crates/agterm-ghostty` (safe wrapper over the libghostty embedding
  API: app, config, surface, input, actions, clipboard, text), `crates/agterm-ghostty-sys` (checked-in bindgen),
  `scripts/build-ghostty.sh`, `scripts/gen-ghostty-bindings.sh`, `scripts/build-layer-shell.sh`, `scripts/stage.sh`.
- libghostty APIs the frontend relies on (all need a replacement): config load/diagnostics/get, surface key/mouse/IME
  /preedit, `read_text`, `read_selection`, `has_selection`, `foreground_pid`, `binding_action`, clipboard requests and
  confirmation, actions `CELL_SIZE`, `COLOR_CHANGE`, `CONFIG_CHANGE`, `DESKTOP_NOTIFICATION`, `START/END_SEARCH`,
  `SEARCH_SELECTED/TOTAL`, `MOUSE_SHAPE/VISIBILITY`, `OPEN_URL`, `PWD`, `RENDER`, `RING_BELL`, `SET_TITLE`,
  `SHOW_CHILD_EXITED`, `NEW_TAB`.
- Host seams: `agterm-host/src/lib.rs` — `MainLoop<S>`, `Surface`, `SessionHost`, `ProcessLauncher`, `ProcessTable`.
- Already host-free in core and reused as is: shell integration injection (`agterm-core/src/zmx/shell_integration.rs`,
  zmx spawns the shell, so libghostty never injected it for live panes), theme catalog and `light:/dark:` resolution
  (`agterm-core/src/theme.rs`), the settings' ghostty lines (`ghostty-settings.conf`), `config.reload`
  (`agterm-core/src/commands/config.rs`, answers with the diagnostic count — a wire contract), keymap (kitty chord
  syntax, `agterm-core/src/keymap`).
- `crates/agterm-gtk/src/ghostty-defaults.conf`: padding, block cursor, `shell-integration-features = no-cursor,no-title`,
  `cursor-click-to-move = false`, physical-key copy/paste/select-all binds.
- HTML overlay today: `crates/agterm-gtk/src/html_overlay*` (~3k lines, feature `html`, WebKitGTK 6): `NetworkSession`
  (persistent profiles = saved logins), `UserContentManager`/`UserScript` (JS bridge), navigation and response policy
  decisions, `register_uri_scheme`, `Settings`, context-menu downloads.
- Kitty graphics are needed for parity: upstream's `show-image.sh` emits chunked kitty APC into a PTY overlay
  (`.upstream/agterm/.claude/rules/control-api.md:80`).
- Conformance: `conformance/tools/run-rust-linux.sh` already takes `--stage DIR` and `--ratchet FILE`; the harness
  (`conformance/agterm_conformance/impls/base.py`) runs a private headless weston (`--renderer=gl`, Mesa EGL vendor)
  and dbus; the adapter is `impls/rust.py`. `conformance/tools/chrome_probe.py` pixel-checks the chrome against the
  terminal. UI the suite cannot see: `docs/linux-ui-checklist.md`.
- The user's machine: niri (Wayland, wlr-layer-shell), NVIDIA.
- `agterm-gtk` module map the port must cover (plan review, 2026-10-14), beyond the terminal and the obvious views:
  `state.rs` (`AppState::settle`, the main loop's heart), `signals.rs` (SIGTERM quit within 10 s — a rust-build.md
  contract), `keymap_dispatch.rs`/`keymap_actions.rs` (window-level capture before the terminal), `overlay.rs` (caller
  programs over a page or pane — `show-image.sh` runs in one), `pane_lead.rs` (lead cover/takeover), `mute.rs`
  (inactive-pane and backdrop wash), `background.rs` + core `watermark` (`background-image*` config lines the terminal
  draws), `remote.rs` (gio `NetworkMonitor`, system-bus resume signal), `remotes_watcher.rs` (gio `FileMonitor`),
  `theme.rs` (follows the desktop's light/dark through `adw::StyleManager`), `zmx_runtime.rs` (reset dialog and
  relaunch), `integrations/` (skill, hooks and CLI installers, opencode; ~3k lines), `dashboard.rs` (a second, view-only
  rendering of a pane's terminal). GTK-free already and shareable: `hook_runner.rs`, `process_table.rs`,
  `quick/placement.rs`, `restore_launch.rs`, `logging.rs`, the integrations' planners. `heap_trim.rs` (uncommitted) is
  GTK-GL specific.
- libghostty also gave the shell its terminal identity: `TERM=xterm-ghostty` with `TERMINFO` pointing at the staged
  terminfo (fallback `term = xterm-256color` when resources are unresolved, `ghostty_app.rs` defaults) and
  `COLORTERM=truecolor`; with libghostty-vt we set these ourselves.
- Probes with no GPUI counterpart yet: `conformance/tools/ghostty_probe.py` (first frame drawn, from `agterm::surface`
  debug logs) and `webkit_probe.py`.

### External pieces (verified 2026-10-14)

- **libghostty-vt** (updated in Task 4, 2026-10-14) — Ghostty's own C library, `ghostty-org/ghostty`
  `include/ghostty/vt.h` + `include/ghostty/vt/*.h`, built by Ghostty's `build.zig` (`zig build -Demit-lib-vt=true`)
  into `lib/libghostty-vt.a` (static, empty `Libs.private` in `libghostty-vt-static.pc`), `libghostty-vt.so`, the
  headers and pkg-config files. At the chosen pin (Technical Details > Pins) the API has: terminal options for every
  effect the frontend needs (`WRITE_PTY`, `BELL`, `TITLE`/`TITLE_CHANGED`, `PWD`/`PWD_CHANGED`,
  `DESKTOP_NOTIFICATION`, `PROGRESS_REPORT`, `PROGRAM_STATUS`, `CLIPBOARD_READ`/`CLIPBOARD_WRITE`, `COLOR_*`,
  `COLOR_SCHEME`, `SELECTION`, `SEMANTIC_PROMPT`, `RENDER_HOLD`, `RESIZE_WINDOW`, `KITTY_IMAGE_*`, scrollback limits),
  `render.h` (render state, dirty rows, two-phase update), `key/` + `mouse/` encoders, `selection.h`, `formatter.h`,
  `paste.h`, `search.h` (scrollback search, ticked off-lock), `snapshot.h`, `kitty_graphics.h`, `osc.h` (standalone
  OSC parser), `sys.h` (process-global PNG decoder, log, random). Verified by building it at the pin with Zig 0.16.0
  (105 s, Task 4).
- **Rejected: the `libghostty-vt` crate** (`Uzaaft/libghostty-rs` 0.2.2, user decision 2026-10-14): it pins an older
  Ghostty (`a887df42…`, Zig 0.15.2, Zig found on PATH only), has no desktop-notification, progress, clipboard-read,
  mouse-shape or color-change effects and no search or snapshot module, and makes every type `!Send`
  (`docs/spikes/20261014-gpui-term.md`). Its source stays a reference for the safe-wrapper shape only.
- **gpui-kit** — `longbridge/gpui-kit` (formerly gpui-component), ~17k stars, daily commits, Apache-2.0-family. Pins
  Zed's GPUI as the crates.io snapshot `gpui-pre`, re-exports it; 75+ components; headless UI integration tests. Its
  optional `gpui-fast` backend is longbridge's own implementation — **not used** here. Confirmed in Tasks 1–3:
  **gpui-kit 0.7.3** pins **`gpui-pre` / `gpui-pre-platform` `=0.3.9`** ("snapshot of zed@93d7456"; platform features
  `font-kit`, `x11`, `wayland`, `runtime_shaders`) with **wgpu / wgpu-hal 29.0.4**; it builds with
  `default-features = false` and with its defaults minus `gpui-fast` (no `gpui-fast*` package in `Cargo.lock`).
- **WPE WebKit** — confirmed in Task 2: **WPE WebKit 2.54.1** (WPEPlatform API, `WPEDisplayHeadless`) from Arch's
  `extra/wpewebkit` 2.54.1-2 binary package, unpacked into a private prefix (signature checked, no root); GStreamer
  1.28.8. Distro builds compile their helper path in (`/usr/lib/wpe-webkit-2.0/WPEWebProcess`, no `WEBKIT_EXEC_PATH`
  in release builds), so a private prefix only runs under a `bwrap --ro-overlay`; a packaged agterm depends on the
  distro's `wpewebkit` instead (Task 30 decides the CI/packaging source).
- GPUI's Linux renderer is wgpu (Zed PR #46758, 2026-02-13, replaced Blade); macOS/Windows keep native renderers.
- `WindowKind::LayerShell` in GPUI (Zed PR #35610, after the revert of #32651): wlroots compositors, KDE, niri; not
  GNOME, not X11. Confirmed in `gpui-pre` 0.3.9 (Task 3): a Wayland compositor without `zwlr_layer_shell_v1` fails
  `open_window` with `LayerShellNotSupportedError`, but GPUI's X11 backend silently opens `LayerShell` as a plain
  window, so the host is chosen by `cx.compositor_name()`, not by the open error.
- `gpui-webview` (gpui-kit `crates/webview`, formerly `gpui-wry`): Wry; on Linux WebKitGTK 4.1/GTK3 in an X11 child
  window only (Wayland unsupported: one client cannot embed another's surface), and requires GPUI Fast. Rejected as a
  dependency; its GPUI-side element and API shape are the model for ours.
- References to port from (adapt, attribute, never depend on): `Xuanwo/gpui-ghostty` (Apache-2.0; GPUI
  `TerminalView`, key mapping to `KeyEncoder`, IME/preedit, selection, dirty-row rendering, procedural box drawing;
  stale pins: Ghostty v1.2.3, Zig 0.14, pre-wgpu GPUI) and `joshka/qwertty-term` (MIT; a subsystem-by-subsystem Rust
  port of Ghostty, differential-tested against it — the first place to look for how Ghostty does anything we now do
  ourselves: `qwertty-term-sprite` (procedural sprites pixel-identical to upstream goldens), kitty graphics rendering,
  `qwertty-term-renderer` (run-based shaping cache, per-row dirty tracking, cursor/decoration drawing),
  `qwertty-term-font` (metrics, nerd-font constraints, emoji), `qwertty-term-input` (keyboard protocols),
  `qwertty-term-termio` (PTY read pipeline)). Zed's own `terminal`/`terminal_view`
  are GPL-3.0 and tied to the editor: read for ideas only, copy nothing.

## Development Approach

- **testing approach**: Regular (code first, then tests, in the same task).
- complete each task fully before moving to the next; small, focused changes.
- **CRITICAL: every task MUST include new/updated tests** for its code changes: unit tests for new and modified
  functions, success and error paths. Spike tasks (1–3) are throwaway code: their deliverable is a measured report, and
  their "tests" are the scripted acceptance checks recorded in the report.
- **CRITICAL: all tests must pass before starting the next task** — no exceptions.
- **CRITICAL: update this plan file when scope changes during implementation.**
- project gates per task: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `scripts/lint-report.sh`, `cargo test --workspace`; from Task 17 on, a full conformance run in the background for
  **both** frontends. A ratchet id never goes red (`rust-passing.txt` for GTK, `rust-gpui-passing.txt` for GPUI).
- capabilities: the GPUI build claims a capability in its `version` result only once its commands implement it,
  starting from an empty set in Task 12 and growing task by task (the GTK set in `agterm-gtk/src/app.rs` `CAPABILITIES`
  + `live_capabilities` is the target); the ratchet and Task 34's superset check depend on it.
- complexity limits (`clippy.toml`) are met by splitting; the only exception is an outer
  `#[expect(clippy::..., reason = "...")]` on one item. Thresholds are never raised.
- dependencies follow the "popular tools only" rule: `gpui-kit`, `portable-pty`, `notify-rust`, `zbus`, `notify`,
  `gir`; libghostty-vt is Ghostty's own C library behind our own bindings (`bindgen`, as for libghostty), not a
  third-party crate.
- isolation (CLAUDE.md > Never touch the user's running agterm) holds everywhere: stripped `AGTERM_*`/`ZMX_*`, explicit
  `--socket`, private state dir, zmx reaped only by its own `ZMX_DIR`.
- GUI runs headless (private weston) everywhere **except** Spikes 2 and 3, which the user allowed (2026-10-14, while
  away from the machine) to run on the real niri session and NVIDIA GPU — with the same isolation, never touching the
  live agterm. That permission covers those spikes only.

## Testing Strategy

- **unit tests**: required for every task. `agterm-term` and the config parser are tested with real libghostty-vt and a
  real PTY, no window (plain `cargo test`).
- **GPUI components**: gpui-kit's headless UI tests (`TestAppContext`-style: real input, assert state/focus/layout) for
  `TerminalElement`, overlays and dialogs.
- **conformance (e2e)**: the protocol/cli/behaviors suite runs against a staged GPUI build with its own ratchet
  `conformance/rust-gpui-passing.txt`; GPUI needs Vulkan, so the harness's private weston gets Mesa's lavapipe
  (software Vulkan) — no dependency on the NVIDIA driver in CI-like runs — and `--fake-seat` (GPUI panics on a
  compositor without `wl_seat`, Task 1). Fallback paths that need X11 (the quick terminal, activation) run under a
  private Xvfb with GPUI's X11 backend (Task 3).
- **pixel checks**: `chrome_probe.py` extended to the GPUI build (chrome vs terminal colors), plus golden PNGs for box
  drawing sprites against upstream's.
- **manual**: `docs/linux-ui-checklist.md` gains a GPUI column; UI the suite cannot see is walked there.

## Progress Tracking

- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- update the plan if implementation deviates from the original scope
- keep the plan in sync with the actual work done

## Solution Overview

```
agterm-core / proto / host / server / ctl / zmx      (unchanged; + ghostty config parser and keybind actions in core)
        ▲ agterm-host traits
agterm-app    (no UI)  frontend-neutral app services moved out of agterm-gtk: hook runner, process table,
                       quick-terminal placement, restore launch, logging, integrations planners
agterm-ghostty-vt-sys  checked-in bindgen of Ghostty's `ghostty/vt.h`, links the vendored static libghostty-vt
agterm-ghostty-vt      safe wrapper: terminal, render state, encoders, selection, formatter, search, kitty, effects
                       (handles Send, not Sync)
        ▲
agterm-term   (no UI)  pane: VT behind a mutex + one capture render state, PTY (portable-pty), IO thread per pane,
                       effects → main-thread channel, frames/kitty views in its own types, encoders,
                       selection/search/read-text, launch (zmx attach or plain) + TERM env;
                       libghostty-vt types never leave its public API
        ▲
agterm-gpui → bin `agterm`   gpui-kit app: AppState, windows, sidebar, deck/splits, TerminalElement (grid, sprites,
                       kitty images, background image), Surface impl, overlays, quick terminal (LayerShell),
                       settings, remotes, integrations UI, HTML overlay element
        ▲
agterm-wpe   (Linux)   WPE WebKit offscreen in its own thread/GMainContext, gir bindings, dmabuf frames
gpui-pre patch         wgpu external-texture element importing dmabuf (zero-copy video)
```

- **Threads** (as in Zed and Ghostty): GPUI owns the main thread; `MainLoop` is GPUI's foreground executor. Each pane's
  IO thread reads the PTY and calls `vt_write` under the pane's mutex; effects go to the main thread through a channel
  and wake the view (`cx.notify()`, coalesced by GPUI to a frame). Painting locks the VT only for `begin_update`. Task 4
  re-checked this against the official C API's contract (the spike's actor model came from the third-party crate's
  `!Send` types, not from Ghostty); the protocol and its evidence are in Technical Details > Threads.
- **Config**: no libghostty config parser any more. Core parses ghostty config itself, same load order; unknown
  ghostty keys → diagnostic (keeps `config.reload`'s count meaningful); known-to-ghostty but unsupported keys → ignored
  and logged. Ghostty `keybind = …=action` runs through our own action table; an unknown action → diagnostic.
- **Box drawing / block / powerline / braille / legacy computing**: drawn procedurally per cell like Ghostty (first
  version — font glyphs leave gaps in TUI borders and prompts, a visible regression against upstream).
- **Kitty graphics**: first version (parity with `show-image.sh`). `custom-shader`: not supported.
- **HTML overlays**: GPUI element and API modeled on `gpui-webview`; the Linux engine is WPE WebKit offscreen (same
  WebKit API as today, so the port is near-mechanical, same engine family as upstream's WKWebView, Wayland-native).
  Frames are imported zero-copy via dmabuf into a wgpu texture (video is a real use case; Task 2: YouTube 1080p60 with
  0 drops, CPU at or below the GTK build). SHM with damage rects is a **last-resort correctness fallback** only, for
  documents when dmabuf import fails on a driver: Task 2 measured it at 80–115 % CPU on 1080p video and 2.5 fps on
  YouTube, so it is never a performance path and never chosen while dmabuf works.
- **Engine boundary**: `agterm-term`'s public API exposes only our own types (`PaneEvent`, render rows/cells, input
  structs). The compiler, not discipline, keeps libghostty-vt swappable (e.g. for `qwertty-term-vt`, a pure-Rust port,
  should the Zig toolchain ever go away for zmx too).
- **Surface**: the `agterm_host::Surface` impl lives in `agterm-gpui` over an `agterm-term` pane, because
  `font_size()`, `cell_size()`, `is_realized()` need GPUI's font metrics and realization; `binding_action` goes through
  core's ghostty action table (Task 8).
- **Single instance and coexistence**: GApplication's forwarding is replaced by speaking GApplication's own D-Bus
  protocol through `zbus`: the GPUI build owns the app id's bus name (`io.github.melonamin.agterm`, or `AGTERM_APP_ID`
  under GApplication's validity rule — `run-dev.sh` and the harness's `check_app_id` rely on it) at GApplication's
  object path, serving `org.gtk.Application` (`Activate` with platform data — the only call a default-flags GTK launch
  makes) and `org.gtk.Actions` (`DescribeAll`, `Activate`; a non-primary GApplication calls `DescribeAll` first and
  fails registration without it). Both directions work: a GTK launch forwards to a running GPUI primary, and a GPUI
  launch finding the name owned calls `Activate` on the primary (GTK or GPUI) and exits before starting GPUI. The two
  builds never share `windows.json`, and no command is added to upstream's wire protocol. The xdg-activation token from
  platform data raises the window (Task 3: works on niri, focused in 10–12 ms). GPUI consumes `XDG_ACTIVATION_TOKEN`
  only for its first toplevel and removes it from the environment at platform creation, and has no API to activate an
  existing window with a foreign token, so: a forwarding GPUI launch reads `XDG_ACTIVATION_TOKEN` and
  `DESKTOP_STARTUP_ID` **before** GPUI starts and sends them as `activation-token` / `desktop-startup-id` (plain
  GApplication does not forward the token; GtkApplication does); the primary binds `xdg_activation_v1` on GPUI's own
  `wl_display` (raw-window-handle) and calls `activate(token, surface)`; on X11 it sends `_NET_ACTIVE_WINDOW` (source 1)
  with the startup id's `_TIME` timestamp, sets `_NET_WM_USER_TIME` and ends the startup sequence (x11rb on the raw
  handle) — GPUI's own X11 `activate_window` sends time 0. Both are GPUI patch candidates (Technical Details > GPUI
  patches).

## Technical Details

- **Pins** (`build/pins.env`, decided in Task 4): **two Ghostty pins while both frontends coexist.** `GHOSTTY_REV`
  stays upstream agterm v0.36.0's pin (`683d8db6…`, 2026-08-25) with its four patches: it moves only with an upstream
  release (CLAUDE.md), and the GTK ratchet rests on it. Its vt API lacks `search.h` and the October fixes, so
  libghostty-vt gets its own pin: `LIBGHOSTTY_VT_REV="81598385d608dda4946093d6d6a234d204a2f225"` (ghostty main,
  2026-10-13, merge #14691: every `build-libghostty-vt*` CI job green, only the snap builds red; includes `search.h`,
  `PROGRAM_STATUS`, `SEMANTIC_PROMPT`/prompt navigation, the scrollback/cursor-page use-after-free fix #14686), with
  its own Zig pin lines `LIBGHOSTTY_VT_ZIG_VERSION="0.16.0"` and sums (`minimum_zig_version` at that commit; the
  same release as `GHOSTTY_`/`ZMX_ZIG_VERSION` today, kept separate by the pins.env rule "each build keeps its own
  Zig pin"), and the source archive's sha256 (codeload tar.gz of the commit:
  `1bb165d1e9458c8694370039cc1a8f2a2958929760bb80f6251499158ad6b694`, re-verified in Task 5). No patches. The prefix
  must not start with `GHOSTTY_`: `pin_lines GHOSTTY` (`scripts/lib/native.sh`) keys the libghostty build on every
  `^GHOSTTY_` line. The two pins become one in Task 34, when libghostty goes away. A vt pin bump regenerates the
  bindings and is a change of its own (the vt API is still moving: names changed on 2026-10-14).
- **Native build** (existing convention, `scripts/lib/native.sh`: Cargo never runs Zig): `scripts/build-ghostty-vt.sh`
  builds `zig build -Demit-lib-vt=true -Doptimize=ReleaseFast` from the verified source archive into
  `${AGTERM_VENDOR}/ghostty-vt-<key>/` (key = `LIBGHOSTTY_VT_` pin lines + arch), with `--print-prefix`/`--verify`
  like `build-ghostty.sh`; `agterm-ghostty-vt-sys/build.rs` asks `--print-prefix`, links `static=ghostty-vt` and fails
  with a pointer to the script when the prefix is incomplete. The static library is linked into the binary, so staging
  gains nothing. The full-libghostty Zig and the vt Zig resolve through `find_zig` (one unpacked 0.16.0 serves both
  while the versions match).
- **Resources while both builds coexist**: terminfo, themes and shell integration keep coming from the libghostty
  prefix at `GHOSTTY_REV`, exactly as `scripts/stage.sh` stages them today (`share/terminfo`, `share/ghostty/*`). Task 34
  moves them to the Ghostty source at `LIBGHOSTTY_VT_REV` (terminfo compiled with `tic`) when libghostty goes away.
- **Cargo**: no third-party libghostty-vt crate (`agterm-ghostty-vt-sys` + `agterm-ghostty-vt` are workspace crates);
  `gpui-kit = "=0.7.3"` with default features minus `gpui-fast` (it pins `gpui-pre`/`gpui-pre-platform` `=0.3.9`, wgpu
  29.0.4); `[patch.crates-io]` for the patched GPUI crates (`gpui-pre`, `gpui-pre-wgpu`, `gpui-pre-linux` as the
  patches need) from the first task that lands a GPUI patch (Technical Details > GPUI patches), as a vendored copy
  under `vendor/` with the patch series checked in (Task 2's `scripts/vendor.sh` shape) or a pinned git fork.
- **GPUI patches** (gaps found by Tasks 2–3; each lands in the task that needs it, each is an upstream candidate):
  1. dmabuf external surface: `PaintSurface` Linux payload + `Window::paint_external_surface`, wgpu-hal Vulkan import
     (multi-plane, non-disjoint), and device creation through `open_with_callback` adding
     `VK_EXT_image_drm_format_modifier` — Task 29;
  2. activate a window with an external xdg-activation token (`xdg_activation_v1.activate`) — Task 12 uses the proven
     raw-window-handle workaround until then;
  3. X11 activation with the startup id's timestamp (`_NET_ACTIVE_WINDOW` source 1, `_NET_WM_USER_TIME`, startup
     notification `remove`) — Task 12, x11rb workaround on the raw handle until then;
  4. Wayland display bounds from `zxdg_output_v1` logical geometry (GPUI uses the integer `wl_output.scale`, so a
     1.75-scaled output reports 1440x900 instead of 1645x1028) — Task 24;
  5. optional: runtime `zwlr_layer_surface_v1` setters (anchor, margin, layer, keyboard interactivity) — Task 24
     reopens the surface instead;
  6. the exit SIGSEGV (wgpu's GLES instance runs `eglTerminate` in a TLS destructor after the Wayland connection is
     gone, on every exit, Task 2 quirk 12) — reported upstream; Task 12 keeps the exit status clean meanwhile.
- **Cell metrics**: advance of `M` for width; ascent + descent + line gap for height; `window-padding-x/y` around the
  grid; resize → `Terminal::resize` + PTY `TIOCSWINSZ` + cell size reported where `CELL_SIZE` was used.
- **Threads** (decided in Task 4 from the C API's contract at `LIBGHOSTTY_VT_REV`, Codex-reviewed). Evidence: the
  library creates no threads and asks callers to serialize access to a terminal, from any thread — `render.h:29-51`
  (render state needs terminal access only during the update, "safely multi-threaded (as long as a lock is held
  during the update call …)"; two-phase update for "a renderer thread sharing a lock with an IO thread"),
  `render.h:645-652` (`end_update` touches
  only render-state memory, "safe to call while the terminal is being modified"), `terminal.h:79-84` (callbacks run
  synchronously inside `vt_write`, must not call `vt_write` re-entrantly, must not block), `terminal.h:1845-1848`
  ("Like every callback, this runs on the thread that called `ghostty_terminal_vt_write()`"), `search.h:92-110`
  (terminal-touching search calls serialized with the terminal; `tick`/`get` safe while another thread modifies it —
  "how Ghostty runs search on a background thread"), `kitty_graphics.h:109-115` (borrowed image handles die on any
  mutating call), `formatter.h:124` (the formatter keeps a borrowed terminal and reads it live), `terminal.h:3284`
  (compression serialized like everything else), `sys.h:21` + `src/terminal/sys.zig:18,65` +
  `src/terminal/c/sys.zig:87` (PNG decoder, log, random are process-global plain variables set at startup — not
  thread-local as the third-party crate made them); `grep threadlocal src/terminal` is empty. Hence wrapper handles are
  **`Send`, not `Sync`**, and the plan's original design stands, with this protocol per pane:
  - `Mutex<Vt>` holds the terminal, key/mouse encoders, selection and the search's terminal-touching side. The IO
    thread reads the PTY and calls `vt_write` under the lock in bounded slices, releasing between slices. Callbacks
    copy their data into the effects channel and return (no re-entrant `vt_write`, no blocking); `WRITE_PTY` replies go
    to a PTY writer queue, never a blocking write under the lock.
  - **One capture render state per pane**, not one per view: dirty flags have a single consumer (`render.zig:768`
    "we're the only consumer of dirty state"; a two-state experiment in Task 4 left the second state clean). Lock order
    VT → capture, never the reverse. Paint: `try_lock` VT, then `try_lock` capture (on either failure release both and
    draw the previous frame, asking for another); while holding both, check the render hold, `begin_update` and copy
    the bytes of any new kitty image `(id, generation)`; release VT; keep capture through `end_update` and the
    conversion into the pane's frame in `agterm-term` types (rows shared as `Arc`, dirty rows only); release capture.
    Every view (the pane, the dashboard) draws that frame. Paint never re-takes VT while holding capture.
  - Synchronized output (`RENDER_HOLD`, mode 2026): the callback (IO thread, VT already held) takes capture and calls
    `ghostty_render_state_update` there, as `terminal.h:1777-1785` asks; while the hold lasts paint does not update but
    **does publish the frame the callback captured**.
  - Clipboard read (OSC 52 / 5522) must be answered before the callback returns (`terminal.h:970-981`; no reply = empty
    clipboard / `EPERM`), so it can never wait for GPUI under the VT lock. Task 9 picks the mechanism with tests:
    answer from a pane-local snapshot the main thread keeps (policy decision + clipboard text), or intercept the read
    with the standalone `ghostty_osc_*` parser before `vt_write` and reply into the PTY after the prompt, or an
    upstream deferred-reply API. Parity with the GTK build's prompt is the requirement.
  - Formatter (read-back, selection text) is created and run entirely under the VT lock; only owned bytes escape.
    Search: `GhosttySearch` in its own mutex, `new`/`feed`/`run`/`set`/`free` also under the VT lock, `tick`/`get` on a
    worker off the VT lock.
  - PNG decoder installed once per process at startup (`ghostty_sys_set(GHOSTTY_SYS_OPT_DECODE_PNG)`), decoding with
    the `png` crate.
  - Fallback: if Task 9's measurements show paint or input waiting longer than a frame on the VT lock (a slice bounds
    bytes, not time: PNG decode, reflow), `agterm-term` switches internally to the spike's actor-per-pane model; its
    public API does not change.
- **Frame**: capture per Threads (`begin_update` under the VT lock, `end_update` after it) → per dirty row: merged
  background quads + text runs shaped with `shape_line` (row cache keyed by row content) → layers: backgrounds,
  selection/search highlight, kitty images below text, text, sprites, decorations (underline styles, strikethrough,
  hovered link), kitty images above text, cursor (block/bar/underline, hollow unfocused), IME preedit. GPUI's
  `RenderImage` is BGRA: kitty RGBA is swapped once per image generation (Task 1).
- **Terminal identity**: the pane's child gets `TERM=xterm-ghostty`, `TERMINFO=<stage>/share/terminfo` (the
  `xterm-ghostty` entry staged as above), `COLORTERM=truecolor`; `TERM=xterm-256color` without
  `TERMINFO` when the terminfo is not resolvable, matching today's fallback; the `term` config key overrides.
- **Config diagnostics**: `gen-ghostty-config-keys.sh` extracts every key **with its value type** from `Config.zig`;
  values of every known key are validated by type (bool, ints, floats, enums, colors, durations; complex structs
  accepted), so malformed values count as libghostty counted them, supported or not. Residual differences are listed
  against the `config-reload-counts` capability tests.
- **Keyboard**: GPUI keystroke → our keymap first (core), then ghostty keybind table, then the C API's key encoder
  (`vt/key/`; legacy + kitty protocol per terminal modes). Physical-key copy/paste/select-all as in
  `ghostty-defaults.conf`.
- **Effects mapping** (replaces `GHOSTTY_ACTION_*`), each from a `GHOSTTY_TERMINAL_OPT_*` callback of the C API: title
  (`TITLE_CHANGED`), pwd (`PWD_CHANGED`), bell (`BELL`), desktop notification (`DESKTOP_NOTIFICATION`, OSC 9/777),
  progress report (`PROGRESS_REPORT`), clipboard read/write (`CLIPBOARD_READ`/`CLIPBOARD_WRITE`, with the existing
  `ClipboardPromptPolicy` confirmation; read per Threads), color changes (`COLOR_*`, OSC 10/11/12/4) for the chrome
  tint, mouse shape (`GHOSTTY_TERMINAL_DATA_MOUSE_SHAPE`, OSC 22, read at capture), child exit (PTY EOF / wait
  status — not a VT effect).
- **HTML frames**: WPE exports dmabuf (fourcc + modifier + planes) → Vulkan import (`VK_EXT_external_memory_dma_buf`,
  `VK_EXT_image_drm_format_modifier`) → `wgpu::Texture` via wgpu-hal → GPUI element. Sync with the buffer's
  sync_file/implicit fence; release the buffer back to WPE after the frame is presented. Proven by Task 2 on Intel and
  NVIDIA, with these conditions: wgpu does not enable `VK_EXT_image_drm_format_modifier` itself, so GPUI's Vulkan
  device is opened through `open_with_callback` + `create_device_from_hal` (a GPUI patch, not only an element); every
  memory plane's layout is passed (Intel CCS buffers have 3 planes in one BO, non-disjoint image); **WPE renders on the
  GPU GPUI's wgpu adapter uses** (cross-GPU dmabufs fail in `vkCreateImage`: WPE gets the adapter's render node, e.g.
  via `VK_EXT_physical_device_drm`, and offers only modifiers that device imports); WebKit's buffer set changes under
  the host (2 → recreated on load → 3), so the host tracks the current set, drops stale acknowledgements and invalidates
  an imported texture only when its buffer leaves the set; acknowledgements go through idle sources (acknowledging in
  `render_buffer` re-enters WebKit); the headless display uses implicit sync (no rendering fence on either driver).

## What Goes Where

- **Implementation Steps** (`[ ]`): code, tests, scripts, conformance harness changes and docs in this repository.
- **Post-Completion** (no checkboxes): upstreaming the GPUI dmabuf element to Zed, manual long-session use, uninstalling
  the GTK build from the machine.

## Implementation Steps

Every non-spike task ends with the gates (Development Approach) and, from Task 17 on, a background conformance run on
both frontends with newly green ids added to `rust-gpui-passing.txt` (`check_ratchet.py --add`). Headless GPUI tests
live next to the module as `<module>/tests.rs` unless a path is given.

### Stage 0 — spikes

### Task 1: Spike — libghostty-vt terminal in a gpui-kit window

Throwaway, on branch `spike/gpui-term`, outside the workspace. Headless weston only, with Mesa's lavapipe as the
Vulkan driver (GPUI needs Vulkan); throughput is compared against the GTK build in the same headless setup and is a
sanity check, not a performance verdict.

**Files:**
- Create: `spikes/gpui-term/` (Cargo project)
- Create: `docs/spikes/20261014-gpui-term.md` (report)

- [x] gpui-kit window with one element drawing a libghostty-vt `RenderState` (bg quads + shaped text runs + cursor),
      PTY via `portable-pty` running `zmx attach` under a private `ZMX_DIR` (fallback: plain shell)
- [x] keyboard through `KeyEncoder`, IME commit/preedit, wheel scroll; resize propagates to VT and PTY
- [x] kitty graphics: draw one placement from the `kitty` API (APC input as `show-image.sh` emits it)
- [x] scripted checks recorded in the report: `vim`, `htop`, a kitty image, IME (an input method in the headless
      session or a synthetic preedit), throughput (`cat` of a 50 MB file: time, CPU) against the GTK build
- [x] record which `gpui-pre` revision gpui-kit pins and that it builds without `gpui-fast`; record how
      `libghostty-vt-sys`'s `build.rs` finds Zig (env var or PATH) for Task 5
- [x] report: pass/fail per criterion, surprises, API gaps in `libghostty-vt` (candidates for upstream PRs)
- ⚠️ result (2026-10-14): pass; for Task 4 to fold in — libghostty-vt types are `!Send` (actor thread per pane, not
  `vt_write` under a mutex), 0.2.2 pins Ghostty `a887df42…` built with Zig 0.15.2 (PATH only), several effects and
  `search`/`snapshot` are missing in 0.2.2, GPUI needs weston's `--fake-seat`; see the report's "Changes for the plan"

### Task 2: Spike — WPE WebKit offscreen frames into GPUI via dmabuf (real screen)

Throwaway, on branch `spike/wpe-dmabuf`, on the real niri session and NVIDIA GPU (user's permission), isolated from
the live agterm.

**Files:**
- Create: `spikes/wpe-dmabuf/` (Cargo project; minimal hand-written FFI or gir output is fine here)
- Create: `docs/spikes/20261014-wpe-dmabuf.md` (report)

- [x] WPE WebKit (WPEPlatform, headless/offscreen display) in its own thread with its own `GMainContext`, loading a URL
- [x] receive dmabuf frames; prototype a GPUI element importing them into a wgpu texture through wgpu-hal Vulkan
      (patched local copy of `gpui-pre`); release buffers after present
- [x] input forwarding: pointer, wheel, keyboard into WPE
- [x] fallback path: SHM frames with damage rects drawn as a GPUI image; measure it too
- [x] measurements: YouTube 1080p60 (dropped frames, CPU, GPU) for dmabuf and SHM, long-page scroll fps, input
      latency; the GTK build's WebKitGTK overlay on the same page as the baseline
- [x] report: pass/fail against "1080p60 without drops, CPU ≈ the GTK build", driver quirks met

### Task 3: Spike — layer-shell quick terminal and window activation under niri (real screen)

Throwaway, on branch `spike/gpui-layer-shell`, on the real niri session (user's permission), isolated from the live
agterm: private state dir and socket, stripped `AGTERM_*`/`ZMX_*`, its own app id.

**Files:**
- Create: `spikes/gpui-layer-shell/`
- Create: `docs/spikes/20261014-gpui-layer-shell.md`

- [x] confirm `WindowKind::LayerShell` in the pinned `gpui-pre`; anchored top layer with keyboard interactivity,
      show/hide, resize
- [x] check what the quick terminal needs (`crates/agterm-gtk/src/quick*`): anchor/margins, exclusive zone, focus on
      show, multi-output placement
- [x] check the fallback path for compositors without layer-shell (the suite's weston, GNOME)
- [x] window activation: a second launch hands an xdg-activation token through platform data; check GPUI can raise
      and focus its window with it
- [x] report: pass/fail per point; gaps that need a GPUI patch

### Task 4: Spike gate

- [x] read the three reports; if Task 2 or Task 3 failed, stop and bring the findings to the user before any
      production code (the HTML engine or the quick-terminal approach is reopened, not worked around)
- [x] fold confirmed versions (gpui-kit, gpui-pre, libghostty-vt, WPE) and design changes into this plan
- [x] spike code stays on its branches; reports stay in `docs/spikes/`
- result (2026-10-14): Tasks 1–3 pass, nothing reopened. Folded in: the official libghostty-vt C API with our own
  `-sys` crate and wrapper instead of the `libghostty-vt` crate (user decision), a second Ghostty pin
  `LIBGHOSTTY_VT_REV=81598385…` with Zig 0.16.0, the thread protocol re-derived from the C API (Technical Details >
  Threads), gpui-kit 0.7.3 / `gpui-pre` 0.3.9 / wgpu 29.0.4, WPE WebKit 2.54.1, the GPUI patch list, SHM as a
  last-resort fallback, the quick-terminal and activation rules incl. X11; ➕ items in Tasks 7, 11, 12, 17, 24, 29–32,
  34. Spike code lives on `spike/gpui-term` (`74d7394`), `spike/wpe-dmabuf` (`9e092fd`), `spike/gpui-layer-shell`
  (`caa56d7`) and is removed from `gpui-frontend`. Codex discussion:
  `~/.local/state/plan-exec-revmux/gpui-frontend/discussions/task-4-vt-pin-and-crates/transcript.md`.

### Stage 1 — no GUI

### Task 5: Build wiring for libghostty-vt (official C API, own bindings)

Rewritten in Task 4: no third-party crate; the pattern of `build-ghostty.sh` + `gen-ghostty-bindings.sh` +
`agterm-ghostty-sys` + `agterm-ghostty`, per Technical Details > Pins and > Native build.

**Files:**
- Modify: `build/pins.env`, `scripts/lib/native.sh` (`ghostty_vt_prefix`, `ghostty_vt_prefix_is_complete`)
- Create: `scripts/build-ghostty-vt.sh`, `scripts/gen-ghostty-vt-bindings.sh`
- Create: `crates/agterm-ghostty-vt-sys/` (`Cargo.toml` with `links = "ghostty-vt"`, `build.rs`,
  `src/{lib,bindings}.rs`)
- Create: `crates/agterm-ghostty-vt/` (safe wrapper skeleton: `src/{lib,terminal,render}.rs`, `tests/link.rs`)
- Modify: `Cargo.toml` (workspace members), `scripts/lint-report.sh` (exempt the bindgen crate, narrowly)

- [x] `pins.env`: `LIBGHOSTTY_VT_REPO`, `LIBGHOSTTY_VT_REV=81598385d608dda4946093d6d6a234d204a2f225`, the source
      archive's sha256, `LIBGHOSTTY_VT_ZIG_VERSION=0.16.0` and its x86_64/aarch64 sums, with a comment block like the
      others (no `GHOSTTY_` prefix; why a second Ghostty pin)
- [x] `build-ghostty-vt.sh`: fetch the archive, verify its sha256, `zig build -Demit-lib-vt=true
      -Doptimize=ReleaseFast -Dtarget=<arch>-linux-gnu.2.39` with `find_zig` on the vt Zig pin, stage
      `lib/libghostty-vt.a`, `include/ghostty/`, the pin manifest; atomic rename into `ghostty-vt-<key>`;
      `--print-prefix`, `--verify`; reuse a complete prefix (conventions of `build-ghostty.sh`, scratch dirs in the
      vendor root)
- [x] `gen-ghostty-vt-bindings.sh`: bindgen over `<prefix>/include/ghostty/vt.h` with an allowlist of the functions and
      types the wrapper binds (terminal, options/effects, render state + row/cell iterators, key/mouse encoders,
      selection, formatter, paste, search, kitty graphics, sys, osc), same flags as `gen-ghostty-bindings.sh`; output
      checked in, regenerated on a pin bump
- [x] `agterm-ghostty-vt-sys/build.rs`: `--print-prefix`, `rustc-link-lib=static=ghostty-vt` plus whatever the static
      archive needs (`libghostty-vt-static.pc` lists none at this pin — confirm on link), rerun-if-changed on the pins,
      the scripts and `AGTERM_VENDOR`; a clear error naming `scripts/build-ghostty-vt.sh` when the prefix is missing;
      not under the workspace lints, like `agterm-ghostty-sys` (`lint-report.sh`'s exemption extended to this one
      bindgen crate only; the wrapper stays under every workspace lint)
- [x] `agterm-ghostty-vt` skeleton: `Terminal` (new/free, `vt_write`, resize) and `RenderState` (update, rows) as owned
      handles that are `Send` and not `Sync` (Technical Details > Threads), each `unsafe` block documented; the rest of
      the API grows in Tasks 9–10
- [x] CI (`.github/workflows/rust-linux.yml`, its vendor cache key) and `README.md` build steps call
      `build-ghostty-vt.sh` next to `build-ghostty.sh`
- [x] test that bumping `LIBGHOSTTY_VT_REV` leaves the libghostty and zmx prefix keys unchanged, and that a
      `GHOSTTY_` pin bump leaves the vt key unchanged
- [x] test (`tests/link.rs`): feed VT bytes and read the screen back through the wrapper (proves the link and the
      bindings), plus `Send` checked at compile time and `!Sync` by a compile-fail doc test
- [x] gates — must pass before Task 6
- result (2026-10-14): archive sha256 re-verified (`1bb165d1…`, github archive = codeload), Zig 0.16.0 sums from
  ziglang.org's index.json; `scripts/build-ghostty-vt.sh` built the prefix at the pin. The static archive needs only
  libc (`nm`: 25 undefined libc symbols, compiler_rt bundled), so `build.rs` links `static=ghostty-vt`
  alone. The prefix also holds `share/doc/ghostty-vt/LICENSE` and `share/agterm/ghostty-vt-PINS` (completeness
  requires it to match the `LIBGHOSTTY_VT_` lines). The bindgen allowlist adds the helpers the listed families need
  (`ghostty_alloc`/`ghostty_free`, raw cell/row reads, grid refs, mode reports, focus encoding, style default, build
  info); `ghostty_mode_new`/`_value` are `static inline` in the header and not bound. The two-phase update is
  `RenderState::begin_update` returning a `PendingUpdate` guard (ends on drop), so the state cannot be read
  mid-update. The prefix-key test is `agterm-ghostty-vt-sys/tests/prefix_keys.rs` (real `--print-prefix` over a
  copied, edited `pins.env`). CI's vendor cache key already hashes `pins.env`, so it needed no change.

### Task 6: Move GTK-free app services into `agterm-app`

Both frontends use them while they coexist, so they must not be copied.

**Files:**
- Create: `crates/agterm-app/` (`hook_runner`, `process_table`, `quick_placement`, `restore_launch`, `logging`,
  `integrations` planners, `remote` presentation)
- Modify: `crates/agterm-gtk/src/{hook_runner,process_table,restore_launch,logging}.rs`, `quick/placement.rs`,
  `integrations/*` (re-export or call into `agterm-app`)

- [x] move each module with its tests; GTK keeps only widget glue
- [x] also move `remote.rs`'s presentation process (GTK-free apart from the network/resume observer, which stays per
      frontend)
- [x] no behavior change: existing tests move along and pass unchanged
- [x] gates, full conformance on GTK (`rust-passing.txt` unchanged) — must pass before Task 7

### Task 7: Ghostty config parser in core

**Files:**
- Create: `crates/agterm-core/src/ghostty_config/{mod,parse,keys,model,validate}.rs`, `ghostty_config/tests.rs`
- Create: `scripts/gen-ghostty-config-keys.sh`
- Modify: `crates/agterm-core/src/commands/config.rs` (diagnostics source abstraction, if needed)

- [x] move `ghostty-defaults.conf` out of `agterm-gtk` into core (`include_str!` there), so both frontends load the
      same defaults (`ghostty_config::{DEFAULTS, defaults_text, config_files}`; GTK calls them)
- [x] parser: `key = value`, comments, repeatable keys, `config-file` with `?` optional includes and cycle detection,
      today's load order (defaults, user files, includes, `ghostty-settings.conf`) — as the GTK build and upstream
      load it: defaults, global/scoped/`ghostty-settings.conf`, then the includes, then a per-surface overlay
- [x] `gen-ghostty-config-keys.sh`: every key with its value type from `src/config/Config.zig` at the pinned rev into
      `keys.rs` (checked in, regenerated on a pin bump)
- [x] ➕ (Task 4) the pinned rev for config keys is `GHOSTTY_REV` (the libghostty whose diagnostic counts upstream and
      the GTK build report), not `LIBGHOSTTY_VT_REV`; Task 34 moves it with the resources
- [x] typed model for the supported subset: font-family*, font-size, font-feature, theme incl. `light:,dark:`, palette,
      background/foreground, cursor-*, selection-*, background-opacity, `background-image*` (watermark), window-padding-*
      incl. `window-padding-balance`, scrollback-limit, clipboard-*/mouse-*, cursor-click-to-move,
      shell-integration-features, `term`, keybind, title; theme files through the same parser (`scrollback-limit` is
      `scrollback-limit-bytes`/`-lines` at `GHOSTTY_REV`, the old name taken by ghostty's compatibility rename)
- [x] diagnostics per Technical Details > Config diagnostics (unknown key, malformed value of any known key; supported
      or not)
- [x] tests: load order and overrides, includes and cycles, themes light/dark, each supported key, diagnostics counts
      (unknown key, malformed known-unsupported value, valid unsupported value = 0), the residual-difference list;
      ➕ `agterm-ghostty/tests/config_parity.rs` checks the counts against the linked libghostty
- [x] gates — must pass before Task 8

### Task 8: Ghostty keybind actions in core

**Files:**
- Create: `crates/agterm-core/src/ghostty_config/binds.rs`, `ghostty_config/binds_tests.rs`
- ➕ Create: `ghostty_config/binds/{action,trigger,set,defaults,keyboard}.rs` (split for the size and complexity
  limits); Modify: `ghostty_config/{mod,model,validate,keys,tests}.rs`, `scripts/gen-ghostty-config-keys.sh`
  (`Keybinds` ported, no longer opaque), `agterm-ghostty/tests/config_parity.rs`

- [x] parse `keybind = trigger=action` (physical `key_*` and unicode triggers, sequences, `unconsumed:`/`performable:`,
      `unbind`), ghostty's built-in Linux defaults merged as ghostty does — also `all:`/`global:`, `chain=`, key
      tables (`name/…`), `clear`, W3C codes and ghostty 1.1 key names; `GhosttyConfig.keybind` is the typed
      `Keybinds`
- [x] action table: `copy_to_clipboard`, `paste_from_clipboard`, `select_all`, `scroll_*`, `jump_to_prompt`,
      `increase/decrease/reset_font_size`, `set_font_size:N`, `clear_screen`, `start_search`/`end_search` and
      navigation, `text:`, `csi:`, `esc:` — every action core sends through `Surface::binding_action` included; unknown
      action → diagnostic (all 87 settable actions of `Binding.Action` at `GHOSTTY_REV`; `keybind` lines now count as
      libghostty counts them, messages included — `config_parity` checks every case and every action)
- [x] resolution order with agterm's keymap (keymap, then ghostty binds, then encoding) documented and tested
      (`binds` module docs, `Keyboard::route`/`handle`: the frontend passes its keymap stage, the binds return where
      the key goes and the queued sequence keys' fate; actions run through the frontend's `Performer`)
- [x] tests: non-Latin layouts (physical keys), performable vs not, overrides and `unbind`, each core-sent action
- [x] gates — must pass before Task 9

### Task 9: agterm-term — pane core (VT + PTY + IO thread)

Built on `agterm-ghostty-vt` (Task 5), with the thread protocol of Technical Details > Threads (updated in Task 4).

**Files:**
- Create: `crates/agterm-term/src/{lib,pane,pty,effects,render}.rs`, `tests/pane.rs`, `tests/render.rs`
- Modify: `crates/agterm-ghostty-vt/src/` (effect callbacks, render-state iterators, as the pane needs them)

- [ ] `Pane`: `Mutex<Vt>` (terminal, encoders, selection), `portable-pty` child, IO thread reading the PTY into
      `vt_write` in bounded slices, `WRITE_PTY` replies into a PTY writer queue (never a blocking write under the
      lock), bounded write queue (backpressure)
- [ ] effects → `PaneEvent` (title, pwd, bell, desktop notification, progress, program status, clipboard write, color
      changes, child exited with status): callbacks copy into a channel the frontend drains on its main thread, never
      block and never re-enter `vt_write`; the wrapper reaches callback userdata without aliased `&mut`
- [ ] clipboard read (OSC 52 / 5522): pick and implement the mechanism of Technical Details > Threads (synchronous
      reply from a main-thread-maintained snapshot, or interception with `ghostty_osc_*` and a PTY reply after the
      prompt, or an upstream deferred reply); never wait for the UI under the VT lock; parity with the GTK prompt
- [ ] resize (cells + pixels) to VT and PTY; scrollback limit from Task 7's config
- [ ] render capture in `agterm-term` types (`render.rs`): **one** capture render state per pane (dirty flags have one
      consumer), lock order VT → capture, `try_lock` on paint, two-phase update with only `begin_update` under the VT
      lock, frame = rows shared as `Arc` with dirty flags, cells (graphemes, width, style, colors, hyperlink id), cursor
      (position, style, visibility), mouse shape, palette and default colors; every view draws that frame
- [ ] synchronized output: the `RENDER_HOLD` callback captures the held frame under the capture lock; paint publishes
      that frame and does not update until the hold ends
- [ ] process-global setup once: PNG decoder (`ghostty_sys_set`, `png` crate), log callback into `tracing`
- [ ] public API exposes only `agterm-term` types (Technical Details > Engine boundary)
- [ ] profile the read path through zmx (Task 1: `cat` of 50 MiB took ~1.45 s behind zmx vs 0.41–0.51 s for the GTK
      build behind the same zmx, while plain-shell runs were within 1.0–1.7×): read sizes, per-read wakes, slicing;
      target within 1.5× of the GTK build through zmx
- [ ] measure the lock: worst-case paint wait and key-to-PTY latency under a 50 MiB `cat`, a kitty PNG burst and a
      resize/reflow of a full scrollback; if either exceeds one frame (16.7 ms), switch `agterm-term` internally to the
      spike's actor-per-pane model (same public API) and record it here
- [ ] tests (real PTY): `printf` output on screen; OSC 0/2, 7, 9/777, 9;4, BEL, 52 write and read produce events or
      replies; exit status; resize reaches the child (`stty size`); frame dirty flags after partial updates, two views
      of one pane drawing one frame, a mode 2026 hold publishing the held frame, styles and wide cells; errors: spawn
      failure, PTY closed mid-write, backpressure limit, `try_lock` contention keeps the previous frame
- [ ] gates — must pass before Task 10

### Task 10: agterm-term — input, selection, search, read-back

**Files:**
- Create: `crates/agterm-term/src/{input,selection,search,text,kitty}.rs`, `tests/input.rs`, `tests/selection.rs`,
  `tests/kitty.rs`
- Modify: `crates/agterm-ghostty-vt/src/` (key/mouse encoders, selection, formatter, paste, search, kitty graphics)

- [ ] toolkit-neutral key struct (physical key, text, modifiers, action) → the C API's key encoder (`vt/key/`) per
      terminal modes (legacy, kitty), encoded under the VT lock (the encoder reads modes); mouse → `vt/mouse/` encoder;
      focus events (`focus.h`); bracketed paste via `paste.h`
- [ ] selection (cell ranges, word/line, select all) via `selection.h`; selected text via `formatter.h`, created and
      run under the VT lock
- [ ] scrollback search via `search.h` (matches, selected index, next/prev) — replaces the search actions;
      `GhosttySearch` in its own mutex, terminal-touching calls under the VT lock, `tick`/`get` on a worker
- [ ] screen/scrollback read-back via `formatter.h` matching what `ghostty_surface_read_text` returned; cursor column
- [ ] kitty view in `agterm-term` types (`kitty.rs`) from `kitty_graphics.h`: images (id, generation, format, pixels —
      copied under the VT lock, handles die on mutation), placements (layer, cell rect, source rect, pixel size),
      deletions
- [ ] the safe API for each of these lands in `agterm-ghostty-vt` with its own unit tests; `agterm-term` wraps it
- [ ] tests: legacy and kitty encodings (arrows, F-keys, ctrl/alt), bracketed paste, selection text, search counts and
      navigation, read-back of wide chars and wrapped lines, kitty APC (chunked) → image and placement in the view
- [ ] gates — must pass before Task 11

### Task 11: agterm-term — launch, terminal identity, foreground process

**Files:**
- Create: `crates/agterm-term/src/launch.rs`, `tests/launch.rs`
- Modify: `crates/agterm-term/Cargo.toml` (dep `agterm-zmx`)

- [ ] live panes: launch from `agterm-zmx` launch inputs (`zmx attach`, shell-integration env from core)
- [ ] ➕ (Task 4) fix the zmx first-paint race (Task 1: `zmx attach` paints nothing until it sees a real size change
      after its attach handshake; a prompt printed between the client's attach and its first snapshot is held by the
      daemon, a resize before the handshake is lost; 1–4 of every 6 launches stayed blank): the real fix goes into
      the zmx patch (`build/patches/zmx`) if the race is in zmx, otherwise the launch code waits for the handshake and
      then resizes once; the spike's timed resize nudge is not the fix. Test: 50 consecutive launches all paint the
      prompt
- [ ] plain panes (no live capability), as libghostty spawned them (`agterm-gtk/src/surface.rs` ~675–710): `command`,
      `initial_input`, `wait_after_command` (the child-exited state that `SHOW_CHILD_EXITED` drove), working
      directory, env merge, shell-integration injection
- [ ] terminal identity per Technical Details (`TERM`, `TERMINFO`, `COLORTERM`, fallback, `term` override), terminfo
      from the existing stage
- [ ] foreground pid exposed by the pane; `agterm-app`'s process table consumes it (no dependency of `agterm-term` on
      `agterm-app`)
- [ ] tests: `TERM`/`TERMINFO`/`COLORTERM` reach the shell (`env` in the pane), fallback without terminfo, plain pane
      `command`/`initial_input`/`wait_after_command`/cwd/env, real zmx under a private `ZMX_DIR` (reaped by that dir
      only); errors: zmx missing, attach failure, session gone, command not found
- [ ] gates — must pass before Task 12

### Stage 2 — GPUI frontend core

### Task 12: agterm-gpui skeleton — app state, main loop, server, signals, single instance

**Files:**
- Create: `crates/agterm-gpui/Cargo.toml` (bin `agterm-gpui` until the switch), `src/{main,app,state,main_loop,
  signals,instance}.rs`, `src/state/tests.rs`
- Modify: `Cargo.toml` (workspace deps `gpui-kit`, `zbus`)

- [ ] gpui-kit app on the main thread; `MainLoop` over the foreground executor; `AppState` and its `settle` ported from
      `agterm-gtk/src/state.rs` as a skeleton here — it grows task by task as deck (Task 18) and windows (Task 26) land
- [ ] `version` capabilities start empty and are claimed task by task (Development Approach)
- [ ] `agterm-server` started as in GTK `main.rs`; state dir, logging (`agterm-app`), app id kept
- [ ] single instance per Solution Overview: `AGTERM_APP_ID` with GApplication's validity rule and object-path
      derivation, bus name, `org.gtk.Application` + `org.gtk.Actions` via `zbus`, both forwarding directions,
      activation token (per Task 3)
- [ ] SIGTERM → orderly quit within 10 s (rust-build.md contract), installed only once this process is the primary
- [ ] ➕ (Task 3) `Application::with_quit_mode(QuitMode::Explicit)`: GPUI's Linux default quits when the last window
      closes, which hiding the quick terminal (a `remove_window`) triggers
- [ ] ➕ (Task 3) activation per Solution Overview > Single instance: read `XDG_ACTIVATION_TOKEN` and
      `DESKTOP_STARTUP_ID` before GPUI's platform is created (GPUI removes the variable); a forwarding launch sends them
      as `activation-token` / `desktop-startup-id`; the primary activates its window with a received token by binding
      `xdg_activation_v1` on GPUI's `wl_display` through raw-window-handle (Task 3's ~20-line workaround, until GPUI
      patch 2), and on X11 by `_NET_ACTIVE_WINDOW` with the startup id's `_TIME`, `_NET_WM_USER_TIME` and the
      startup-notification `remove` through x11rb on the raw handle (until GPUI patch 3)
- [ ] ➕ (Task 2) the GPUI exit SIGSEGV (wgpu's GLES instance tears down EGL in a TLS destructor after the Wayland
      connection is gone): find what creates the GLES instance under a Vulkan renderer and avoid it, or end the
      process after the orderly shutdown before TLS destructors run, so a quit exits 0; file the upstream issue
      (gpui/wgpu) with the stack from `docs/spikes/20261014-wpe-dmabuf.md` quirk 12
- [ ] tests: `MainLoop` hop ordering and shutdown, `settle` cases carried over from GTK, SIGTERM deadline and exit
      status 0, forwarding of a second GPUI launch and of a GTK-build launch to the GPUI instance, and of a GPUI launch
      to a GTK primary (private bus); the activation token reaches the primary's platform data from a GPUI and from a
      GTK launch, under a nested niri where installed (stock config, no `honor-xdg-activation-with-invalid-serial`;
      skipped with a message otherwise) and under Xvfb; invalid
      `AGTERM_APP_ID` falls back; never two writers of `windows.json`
- [ ] gates — must pass before Task 13

### Task 13: TerminalElement — grid rendering and the Surface impl

Port from gpui-ghostty's view (Apache-2.0) and `qwertty-term-renderer`/`qwertty-term-font` (MIT), attribution in file
headers, rebuilt on `agterm-term` and current GPUI.

**Files:**
- Create: `crates/agterm-gpui/src/terminal/{element,metrics,row_cache,colors,surface}.rs`, `terminal/tests.rs`

- [ ] cell metrics from the configured font; padding incl. `window-padding-balance`; resize → pane resize
- [ ] frame per Technical Details > Frame: merged background quads, shaped runs with a row cache over dirty rows,
      decorations, cursor styles, focus state; colors from config/theme and OSC overrides; wide chars and graphemes
- [ ] several views of one pane (the dashboard draws a second, view-only one): render state per view, input only on
      the owning view
- [ ] `agterm_host::Surface` impl over the pane — every method GTK's `surface.rs` implements: `pane_token`,
      `is_realized`, `is_live_session`, `promote_to_primary`, `type_text` (keystroke path with `SUBMIT_GAP`),
      `read_text`, `read_selection`, `cursor_column`, `binding_action` (Task 8), `foreground_pid`, `font_size`,
      `cell_size`, `teardown`
- [ ] "first frame drawn" debug log line under `agterm::surface` (what `ghostty_probe.py` reads)
- [ ] tests (headless): metrics for a known font, dirty-row cache invalidation, run splitting by style, cursor styles,
      wide-char placement, two views of one pane, `Surface` methods against the fakes' contract
- [ ] gates — must pass before Task 14

### Task 14: TerminalElement — keyboard routing, IME, mouse, selection, links, clipboard

**Files:**
- Create: `crates/agterm-gpui/src/terminal/{input,mouse,links}.rs`, `src/keymap_dispatch.rs`, `src/keymap_actions.rs`

- [ ] window-level keymap capture before the terminal (port of `keymap_dispatch.rs`/`keymap_actions.rs`), then ghostty
      binds, then `agterm-term` encoding; physical-key copy/paste/select-all
- [ ] IME via GPUI's input handler (commit, preedit at the cursor, candidate position)
- [ ] mouse: reporting modes, selection (drag, double/triple click, `cursor-click-to-move = false`), wheel scroll,
      mouse shape, hide-while-typing
- [ ] OSC 8 and detected URLs: hover underline, open through the existing open-url path
- [ ] clipboard: system and primary; OSC 52 through `ClipboardPromptPolicy` with a minimal confirm prompt (the full
      dialog comes with Task 21)
- [ ] tests (headless): routing order (keymap capture wins), preedit lifecycle, drag and multi-click selection, link
      hit testing, OSC 52 allow/deny/remember
- [ ] gates — must pass before Task 15

### Task 15: Procedural sprites for box drawing, blocks, powerline, braille

Port from `qwertty-term-sprite` (MIT, attribution), adapted to GPUI paths/quads.

**Files:**
- Create: `crates/agterm-gpui/src/terminal/sprite/` (module per glyph family), `tests/sprite_golden.rs`,
  `tests/golden/`

- [ ] box drawing U+2500–257F, blocks U+2580–259F, powerline U+E0B0–E0BF, braille U+2800–28FF, the legacy computing
      ranges Ghostty draws — to cell bounds without gaps; the element routes these code points away from the font
- [ ] golden tests: each family at two cell sizes against goldens derived from upstream's
- [ ] gates — must pass before Task 16

### Task 16: Kitty graphics and background image

Port the placement rendering from qwertty-term (MIT, attribution).

**Files:**
- Create: `crates/agterm-gpui/src/terminal/{kitty,background}.rs`, `terminal/kitty_tests.rs`

- [ ] image data from `agterm-term`'s kitty view → GPUI images cached by id/generation; placements by z-layer, source
      rects, scaling, scrolling with the viewport, deletion
- [ ] `background-image*` (agterm's watermark lines, `background.rs` + core `watermark`) drawn behind the grid
- [ ] tests: placement geometry, layer ordering, cache eviction on delete, background image fit/position modes
- [ ] gates — must pass before Task 17 (the `show-image.sh` end-to-end check is in Task 23)

### Task 17: Conformance harness for the GPUI build

**Files:**
- Modify: `scripts/stage.sh` (`--frontend gtk|gpui`), `conformance/tools/run-rust-linux.sh` (frontend switch,
  ratchet per frontend), `conformance/agterm_conformance/impls/base.py` (lavapipe `VK_DRIVER_FILES`, weston
  `--fake-seat` for GPUI)
- Modify: `conformance/tools/ghostty_probe.py` (GPUI first-frame line)
- Create: `conformance/rust-gpui-passing.txt`

- [ ] staged GPUI build runs under the harness's weston with lavapipe; isolation unchanged
- [ ] ➕ (Task 1) the harness's weston gets `--fake-seat` for GPUI runs (GPUI panics without `wl_seat`); check the GTK
      ratchet stays green with it, or pass it for the GPUI frontend only
- [ ] ➕ (Task 1) provide lavapipe: it is not installed on the user's machine (the spike unpacked the signed
      `vulkan-swrast` package into the vendor dir); pick a documented source per environment (distro package on CI,
      a verified unpack into `${AGTERM_VENDOR}` otherwise) and point `VK_DRIVER_FILES` at it; a missing lavapipe is a
      clear harness error, never a fallback to a hardware driver
- [ ] first full run on GPUI; seed `rust-gpui-passing.txt` with `check_ratchet.py --add`
- [ ] harness self-tests in `conformance/selftests` (`test_run_rust_linux`, `test_ghostty_probe`, `test_impls`) and
      `test_docs.py` (capability table in rust-build.md vs `CAPABILITIES` in `base.py`) updated and green; both ratchets
      green
- [ ] gates — must pass before Task 18

### Task 18: Deck, panes, splits, pane lead, mute

**Files:**
- Create: `crates/agterm-gpui/src/{window,deck,pane,split,session_host,pane_lead,mute}.rs`

- [ ] window shell (port of `window.rs`): window composition, frontmost tracking and `window.list` reporting, the
      modal layers overlays attach to
- [ ] deck of sessions with panes and splits, wired to `SessionHost`/`ProcessLauncher`/process table like GTK
- [ ] lead cover/takeover (`pane_lead.rs`), inactive-pane and backdrop wash (`mute.rs`)
- [ ] tests (headless): split layout math, focus moves, lead takeover, wash on focus change
- [ ] gates + conformance (both ratchets) — must pass before Task 19

### Stage 3 — UI groups

### Task 19: Sidebar

**Files:**
- Create: `crates/agterm-gpui/src/sidebar/`, `src/{status_glyph,row_icon}.rs`

- [ ] workspaces, sessions, status glyphs, row icons, drag reorder, collapse per upstream `agterm/Views` and the
      checklist; gpui-kit list components
- [ ] tests (headless): selection, reorder, collapse, glyph states
- [ ] gates + conformance — must pass before Task 20

### Task 20: Title bar, chrome, appearance

**Files:**
- Create: `crates/agterm-gpui/src/{titlebar,chrome,appearance}.rs`
- Modify: `conformance/tools/chrome_probe.py` (GPUI build)

- [ ] title bar and chrome painted in the terminal's colors (parity with `fd5349f`)
- [ ] follow the desktop light/dark (`org.freedesktop.appearance color-scheme` through the settings portal, `zbus`),
      replacing `adw::StyleManager`; feeds core's `light:/dark:` resolution
- [ ] tests: appearance switch re-resolves the theme; `chrome_probe.py` pixel check passes on GPUI
- [ ] gates + conformance — must pass before Task 21

### Task 21: HUD, ask dialog, modal, popovers

**Files:**
- Create: `crates/agterm-gpui/src/{hud,ask_dialog,modal,popovers}.rs`

- [ ] HUD/captions, ask dialog, modal, popovers with upstream behavior; accessibility ids as element ids
- [ ] full OSC 52 confirmation dialog replaces Task 14's minimal prompt
- [ ] tests (headless): open/close, keyboard navigation, results returned to core
- [ ] gates + conformance — must pass before Task 22

### Task 22: Picker, palette, session switcher, search bar

**Files:**
- Create: `crates/agterm-gpui/src/{picker,palette,session_switcher,search}.rs`

- [ ] picker, command palette, Ctrl-Tab switcher, search bar over Task 10's search
- [ ] tests (headless): filtering, navigation, selection results, search next/prev and counts
- [ ] gates + conformance — must pass before Task 23

### Task 23: PTY overlays, notifications, welcome

**Files:**
- Create: `crates/agterm-gpui/src/{overlay,notifications,status_sound,welcome}.rs`

- [ ] caller programs over a page or pane (port of `overlay.rs`), exit status read-back
- [ ] `show-image.sh` end to end: chunked kitty APC in a PTY overlay renders the image (Task 16)
- [ ] desktop notifications via `notify-rust` with today's semantics (`notifications.rs`): one banner per identity
      (a repeat replaces it through `replaces_id`), a click reveals its pane (`ActionInvoked` → `notification-reveal`),
      clears withdraw banners (`CloseNotification`); status sound; bell; welcome
- [ ] tests: overlay lifecycle and exit status, show-image end to end (headless); notifications on a private session
      bus: replace per identity, click reveals the pane, clear withdraws
- [ ] gates + conformance — must pass before Task 24

### Task 24: Quick terminal on layer-shell

**Files:**
- Create: `crates/agterm-gpui/src/quick/` (placement from `agterm-app`)

- [ ] quick terminal on `WindowKind::LayerShell` per Task 3; fallback window where the protocol is missing
- [ ] ➕ (Task 3) host chosen by `cx.compositor_name()` ("Wayland" → layer surface, falling back on
      `LayerShellNotSupportedError`; X11 → the window host directly, since GPUI opens `LayerShell` on X11 as a plain
      window): it must work in every environment (user, 2026-10-14) — layer-shell compositors, GNOME/weston (no
      layer-shell) and X11. Fallback window: undecorated (`WindowDecorations::Client`, no titlebar, transparent
      background), zoom = maximize
- [ ] ➕ (Task 3) the GTK build's surface attributes: overlay layer, namespace, keyboard on demand, exclusive zone 0,
      top anchor + centering margin; **name no output on show** (niri gives no keyboard to an on-demand surface on an
      unfocused output), then refit by reopening named on the output it landed on (`observe_window_display`)
- [ ] ➕ (Task 3) hide = `remove_window`, show = a new layer surface (GPUI cannot hide a window on Linux); zoom and the
      refit reopen the surface (no runtime anchor/margin/layer setters — GPUI patch 5 optional); focus loss via
      `observe_window_activation` dismisses a summoned panel; `zwlr_layer_surface_v1.closed` (output gone) maps to the
      GTK build's dismissal
- [ ] ➕ (Task 3) fractional-scale outputs: GPUI's display bounds use the integer `wl_output.scale` (eDP at 1.75 reports
      1440x900 instead of 1645x1028, the panel 64 px off): land GPUI patch 4 (`zxdg_output_v1` logical geometry) or
      compute placement from the output's logical size another way; test the placement math against both sizes
- [ ] toggle via keymap and control commands; focus and hide behavior as upstream
- [ ] tests (headless, fallback path); quick-terminal conformance tests green
- [ ] ➕ (Task 3) fallback tests on both backends: weston (`--fake-seat`, no layer-shell) and **X11 under a private
      Xvfb** (open, size, resize, hide/show, placement; focus and maximize are the window manager's on X11 and are not
      asserted without one); a nested niri run for the layer-shell path where niri is installed
- [ ] gates + conformance — must pass before Task 25

### Task 25: Settings window

**Files:**
- Create: `crates/agterm-gpui/src/settings/` (one module per page)

- [ ] the six settings pages of the GTK build per upstream (theme picker with light/dark, fonts, keymap and hooks
      editors entry points, …), round-trips through core
- [ ] tests (headless): each page's controls write and read back through core
- [ ] gates + conformance — must pass before Task 26

### Task 26: App menu, dashboard, zoom, windows and restore

**Files:**
- Create: `crates/agterm-gpui/src/{app_menu,dashboard,zoom,windows}.rs`

- [ ] in-window app menu (port of `app_menu.rs`), dashboard grid (view-only pane views from Task 13), zoom,
      multi-window registry and restore (`restore_launch` from `agterm-app`)
- [ ] tests (headless): menu actions dispatch, dashboard layout, zoom in/out, restore of a saved window set
- [ ] gates + conformance — must pass before Task 27

### Task 27: Remotes and zmx runtime

**Files:**
- Create: `crates/agterm-gpui/src/{remote,remotes_watcher,zmx_runtime}.rs`
- Modify: `Cargo.toml` (workspace dep `notify`)

- [ ] network reachability and resume from sleep over `zbus` (NetworkManager / portal network monitor, logind
      `PrepareForSleep`), replacing gio `NetworkMonitor` and the system-bus signal
- [ ] remotes file watching with `notify`, replacing gio `FileMonitor`
- [ ] zmx reset dialog and relaunch (port of `zmx_runtime.rs`)
- [ ] tests: reconnect on network/resume events (private bus), watcher debounce, reset flow
- [ ] gates + conformance — must pass before Task 28

### Task 28: Integrations UI

**Files:**
- Create: `crates/agterm-gpui/src/integrations/` (UI over `agterm-app`'s planners)

- [ ] skill, hooks and CLI installers, opencode integration — the GTK `integrations/ui.rs` flows on gpui-kit
- [ ] tests: planner calls from the UI, install/uninstall round-trip in a temp home
- [ ] gates + conformance — must pass before Task 29

### Stage 3b — HTML overlays

### Task 29: GPUI external texture element (dmabuf import)

Productionizes Task 2's prototype.

**Files:**
- Create: `vendor/gpui-pre/` (patched copy) or a pinned git fork; `Cargo.toml` `[patch.crates-io]`
- Create: `crates/agterm-gpui/src/external_texture.rs`

- [ ] wgpu-hal Vulkan import of dmabuf (fourcc, modifier, planes), fence/sync_file handling, buffer release callback
- [ ] ➕ (Task 2) the patch has three parts (GPUI patch 1): the scene primitive + `Window::paint_external_surface`
      (`PaintSurface` Linux payload), the renderer draw, and device creation through wgpu-hal `open_with_callback` +
      `create_device_from_hal` adding `VK_EXT_image_drm_format_modifier` (wgpu enables only the external-memory
      extensions)
- [ ] ➕ (Task 2) every memory plane's layout passed (Intel CCS: 3 planes in one BO, non-disjoint image); the import
      cache keyed per buffer and invalidated only when the buffer leaves the producer's current set
- [ ] ➕ (Task 2) expose the wgpu adapter's DRM render node (`VK_EXT_physical_device_drm`) and the modifiers it imports,
      for Task 31 (cross-GPU dmabufs fail in `vkCreateImage`)
- [ ] GPUI element drawing the imported texture with clipping and content mask, so overlays above it work
- [ ] SHM-with-damage fallback behind the same element when import fails — a **last-resort correctness fallback**
      for documents only (Task 2: 80–115 % CPU on 1080p video, 2.5 fps on YouTube), logged as a warning when chosen,
      never selected while dmabuf import works
- [ ] tests: format/modifier negotiation, fallback selection, lifetime (no use after release) with a synthetic dmabuf
      (GBM/udmabuf) under Mesa where available
- [ ] gates — must pass before Task 30

### Task 30: agterm-wpe-sys — gir bindings

**Files:**
- Create: `crates/agterm-wpe-sys/` (gir output from `WPEWebKit-2.0.gir` and `WPEPlatform`, checked in, not under the
  workspace lints like `agterm-ghostty-sys`)
- Create: `scripts/gen-wpe-bindings.sh`

- [x] generation script pinned to the WPE version from Task 4 (**WPE WebKit 2.54.1**, the version Task 2 ran);
      regenerated output is reproducible
- [x] ➕ (Task 2) decide where WPE comes from per environment and record it in `build/pins.env`/README: the distro's
      `wpewebkit` ≥ 2.54 where packaged (the user's Arch machine; distro builds compile the helper path in, so a
      packaged agterm depends on it), and for CI images without it either a verified binary package unpacked into the
      vendor prefix and run under `bwrap --ro-overlay` as the spike did, or a source build with its helper path
      configured to the prefix; the `.gir` files come from the same source
- [x] test: link and version check against the system WPE (here against the verified vendored package, the machine
      has no system wpewebkit; against the distro package in the CI `wpe` job, run locally under podman)
- [x] gates — must pass before Task 31
- result (2026-10-15): **three gir-native crates, not one** (gir's sys mode is one crate per GObject namespace and
  WPEWebKit includes two of ours): `agterm-wpe-sys` (WPEWebKit-2.0), `agterm-wpe-platform-sys` (WPEPlatform-2.0, with
  the `WPEDisplayClass`/`WPEViewClass` vfuncs Task 31 fills) and `agterm-wpe-javascriptcore-sys` (WPEJavaScriptCore-2.0,
  `JSCValue` of the script bridge, probing `wpe-webkit-2.0` since no WPE build installs the `.pc` its .gir names); no
  Headless crate (Task 31 subclasses the base classes; add one if a task needs it). Soup comes from crates.io
  `soup3-sys` 0.9 (as `soup-sys`), GLib/GObject/Gio from gtk-rs 0.22. `scripts/gen-wpe-bindings.sh [--check]` builds
  gir (the 0.22 branch commit that generated glib-sys 0.22.9, `GIR_*` pins, `build/patches/gir/` for a build without
  git) into the vendor root and generates from pinned, sha256-checked inputs only: gir-files at glib-sys 0.22.9's
  commit, soup3-rs v0.9's `Soup-3.0.gir`, the WPE .gir files of the pinned Arch package; `--check` diffs a regeneration
  against the checked-in crates (identical). Providers (README > WPE WebKit, `WPE_*` pins): the distro's `wpewebkit`
  (system `wpe-webkit-2.0` ≥ `WPE_API_VERSION` wins in `scripts/pkg-config.sh`); rootless Arch-compatible machines:
  `scripts/fetch-wpe.sh` unpacks the Arch Linux Archive package (sha256-pinned, glibc 2.44 guard, `ldd` check) into
  `wpe-<key>` with relocated `.pc` files; pages from it need the `bwrap --ro-overlay` launcher (Task 31). CI: Ubuntu
  packages no wpewebkit and cannot load Arch's (glibc 2.39), so a new `wpe` job runs the three crates' clippy/tests and
  `--check` in a digest-pinned `archlinux:base-devel` container on a date-pinned Arch Linux Archive snapshot
  (2026/10/14, `wpewebkit 2.54.1-2`); the Ubuntu jobs, `run-dev.sh` and `run-rust-linux.sh` build
  `WORKSPACE_WITHOUT_WPE` (`scripts/lib/native.sh`). Tests: ABI (gir's layout/constant checks against the headers,
  all three crates), runtime class/instance sizes of WPEDisplay/WPEView/WebKitWebView and the subclass vfunc
  signatures, version (all three namespaces ≥ `WPE_API_VERSION`, one build), provider selection and prefix keys
  (`agterm-wpe-sys/tests/provider.rs`). Header quirk: `wpe/webkit.h` hides `WebKitPointerLockPermissionRequest` behind
  the build's `ENABLE_POINTER_LOCK` (set in the ABI tests' `manual.h`); `WPEKeymapXKB`'s `xkb_keymap`/`xkb_state` are
  opaque hand-written types (`agterm-wpe-platform-sys/src/xkb.rs`). Codex discussion:
  `~/.local/state/plan-exec-revmux/gpui-frontend/discussions/task-30-wpe-source-and-crates/transcript.md`.
- ⚠️ gate for Task 31/32 (from that discussion): before `agterm-wpe` or the GPUI `html` feature makes WPE a dependency
  of anything the Ubuntu jobs build (`--exclude` does not drop dependencies of selected packages), choose a provider
  for Ubuntu (a source build into the vendor prefix, keyed by the build environment too) or move the GPUI checks and
  their weston/lavapipe harness into the Arch job, keeping both ratchets.

### Task 31: agterm-wpe — WPE WebKit host

**Files:**
- Create: `crates/agterm-wpe/` (thread + `GMainContext`, view, frames, input, profiles), `tests/`

- [x] dedicated WPE thread with its own `GMainContext` (pushed thread-default before any WebKit object exists);
      commands in, frames/events out over channels
- [x] offscreen view producing dmabuf frames (Task 29 consumer) and the SHM last-resort fallback (documents only,
      Technical Details > HTML frames)
- [x] ➕ (Task 2) a proper `WPEDisplay`/`WPEView` subclass instead of the spike's patched `WPEViewHeadless` vfuncs; the
      display's DRM device = the render node of GPUI's wgpu adapter (Task 29) and its preferred formats = the modifiers
      that device imports; buffer-set tracking (the set is recreated and grows under the host) with stale
      acknowledgements dropped; acknowledgements deferred to idle sources (acknowledging in `render_buffer` re-enters
      WebKit); a buffer released one frame after GPUI presents it
- [x] ➕ (Task 2) media: keep `media_playback_requires_user_gesture` as the GTK overlay sets it; progressive media
      download stalls in WPE 2.54.1 (MSE works) — record it in the checklist as an engine limitation, do not work
      around it
- [x] the WebKit surface the overlay uses: `NetworkSession` (persistent dirs), `UserContentManager`/`UserScript`,
      script message handlers, navigation/response policy, `register_uri_scheme`, `Settings`, downloads
- [x] WPE's bubblewrap sandbox: `scripts/install-apparmor-profile.sh` attaches the profile to the `agterm-gpui` binary
      path too (and to `agterm` after Task 34)
- [x] tests: load `data:`/custom-scheme pages headless, JS bridge round-trip, policy callbacks, profile persistence
      across view restarts
- [x] gates — must pass before Task 32
- result (2026-10-15): `crates/agterm-wpe` (Linux; over the Task 30 -sys crates and the gtk-rs 0.22 `glib`/`gio`,
  added as workspace deps). API: `Host::start(HostConfig)` → `(Host, async_channel::Receiver<Event>)`; commands are
  `Host` methods (stores, pages, loads, history, zoom, scripts, background, geometry, visibility, focus, input,
  `frame_presented`, `evaluate`), and every synchronous question WebKit asks — navigation/response policy, script
  message replies, reads of the consumer's URI schemes — is held open and reported with a token the consumer answers
  (`decide_policy`, `reply_script_message`, `finish_scheme_request`); closing a page answers what it left open.
  Engine: one per process, its thread pushes a fresh `GMainContext` thread-default before any WebKit object and
  **never ends** — it is WebKit's main thread, whose thread-local teardown waits forever on the run loop it tears down
  (a joined shutdown hung there), so `shutdown`/drop closes everything, waits a bounded 10 s for "stopped" and leaves
  the thread idle; ➕ (review) the process then ends through `agterm_wpe::exit_process` (`_exit`): WebKit's static
  destructors assert they run on its main thread and aborted under `exit()` (4 of 15 suite runs) — Task 12/32 end the
  GPUI app that way. Platform: `AgtermWpeDisplay`/`AgtermWpeView`/`AgtermWpeToplevel` (gtk-rs `ObjectSubclass` over the
  abstract WPEPlatform classes; the toplevel carries size and scale). The input the GPUI side fills (Task 29/32) is a
  plain struct: `FrameMode::DmaBuf(DmaBufTarget { render_node, formats: Vec<BufferFormat { fourcc, modifiers }> })`
  or `FrameMode::Shm`; the display resolves the render node's DRM device and opens EGL on it through a private
  `WPEDisplayHeadless` on that node (libdrm/GBM stay WebKit's; its constructor is declared by hand, the -sys crates
  bind no Headless namespace) and offers exactly the given formats; SHM mode offers no device and no formats, and
  WebKit then renders on the CPU into SHM buffers by itself (no `WEBKIT_SKIA_ENABLE_CPU_RENDERING` needed). Frames:
  `Event::Frame` with `FrameContent::DmaBuf(Arc<DmaBuf>)` (every plane, fds duplicated once per buffer so the consumer
  may outlive WebKit's) or `Shm(Vec<ShmRegion>)` (damaged rects copied out, buffer released at once);
  `Event::BuffersRemoved` when buffers leave the set; `FrameFlow` (unit-tested) marks a frame rendered on
  presentation and releases a dmabuf one presentation later, dropping acknowledgements for buffers that left the set;
  acknowledgements run from a source of their own on the engine's context — ⚠️ glib's `idle_add_local*` attaches to
  the *global* default context, which nothing runs on the engine thread (frames stalled after the first one until
  this was found). WebKit surface: `SessionSpec::{Ephemeral, Persistent { data_dir, cache_dir }}` with downloads
  cancelled and reported (`Event::DownloadRefused`) and clearing (`Event::SessionCleared`); a persistent store also
  saves cookies (`<data>/cookies.sqlite`): WebKit keeps them in memory otherwise, which the GTK overlay's saved store
  does today (backlog: `docs/backlog/gtk-saved-store-forgets-cookies.md`); `Settings` bitflags with
  `Settings::overlay(js)` = the GTK overlay's (media needs a gesture); user scripts per world; reply-style message
  handlers per world; schemes registered at start (`HostConfig::schemes`, served as file, bytes or error); what a page
  may not do is refused in the engine as the GTK overlay refuses it (no new window, silent script dialogs, no file
  chooser, permission, full screen, notification or context menu); load/page/cursor/close/web-process events.
  `agterm_wpe::launcher` builds the `bwrap --ro-overlay` launch for the vendored package (detected by `fetch-wpe.sh`'s
  manifest; the system's package runs plain). Tests: 15 unit tests (config, frame protocol, SHM copy, input bits,
  launcher) and `tests/headless` (custom harness: each frame mode in its own process, through the launcher, private
  XDG dirs): `shm`, `dmabuf` on the first Mesa-driven render node (Intel iGPU here, LINEAR XR24/AR24 offered, the
  pixel read back from the mapped dmabuf) and `bad-device` (a missing render node fails the start; one start per
  process), 11 cases per frame mode: frames and resize/scale with the buffer set recreated, the fall-back to SHM,
  scheme pages (bytes, file, error), the bridge in the page world and in a script world with page scripts off, policy (a clicked link and a
  real click on a `target=_blank` link held and ignored, a refused response failing as interrupted), pointer/key/wheel
  input, a saved store across view restarts and across a new session on the same dirs (cookie and local storage) then
  cleared, refused downloads, closing with questions open, a page that cannot be built (`Event::ViewFailed`), a second
  start refused. ⚠️ NVIDIA's own driver (renderD129) allocates no LINEAR render target and WebKit falls back to the
  driver's block-linear modifier outside the offered list (logged as a warning): Task 29 must check each buffer's
  modifier before importing. ➕ (review) SHM after the start: the engine's frame mode is fixed per process (WebKit
  reads the display's DRM device once per UI process, and a web process picks dmabuf or SHM when it builds a page's
  surface, never again), so `Host::fall_back_to_shm(view)` is the per-page fall-back Task 29/32 call when an import
  fails: WebKit's `HardwareAcceleration` feature goes off in the page's settings and its web process is replaced (the
  page reloads, history kept, script state lost; other pages untouched), and SHM frames follow. CI: the `wpe`
  job checks and tests `agterm-wpe` too (SHM and `bad-device`; no render node there); `WORKSPACE_WITHOUT_WPE` excludes
  it. AppArmor: the profile file holds `agterm` and `agterm-gpui` profiles, `install-apparmor-profile.sh --gpui PATH`
  (default the stage's `bin/agterm-gpui`). Checklist: the media row records the progressive-download stall. For Task
  32 (not in the WebKit surface the GTK overlay used, all WPE-only gaps): the clipboard (WPE has its own
  `WPEClipboard`, not the system's), `<select>` popups (`show-option-menu`), context menus, IME, accessibility; the
  file-paste refusal holds by construction while the WPE clipboard carries only what the host puts there.

### Task 32: HTML overlay on GPUI + WPE

GPUI-side element and API modeled on gpui-kit `crates/webview` (Apache-2.0, attribution).

**Files:**
- Create: `crates/agterm-gpui/src/html_overlay/` (port of `agterm-gtk/src/html_overlay*`: files, page, panel, policy,
  registry, sandbox)
- Modify: `crates/agterm-gpui/Cargo.toml` (feature `html`, default on)
- Modify: `conformance/tools/webkit_probe.py` (GPUI/WPE counterpart)

- [ ] element: bounds/visibility sync, hitbox, focus, scale factor, click forwarding so GPUI popovers close
- [ ] port the overlay logic from GTK onto `agterm-wpe` (same policies, sessions, scheme, bridge, downloads)
- [ ] tests: registry/policy/sandbox unit tests carried over; page lifecycle (headless)
- [ ] HTML conformance tests green on GPUI
- [ ] manual check on the real screen recorded in the checklist: YouTube 1080p60 via dmabuf
- [ ] ➕ (Task 2) the overlay never selects SHM while dmabuf import works; when it does fall back (import failed), the
      checklist and the log say video is not supported in that mode (Task 2: 2.5 fps YouTube, 80–115 % CPU)
- [ ] gates + conformance — must pass before Task 33

### Stage 4 — switch

### Task 33: Linux UI checklist walk for GPUI

**Files:**
- Modify: `docs/linux-ui-checklist.md` (GPUI form column and walk status)

- [ ] add the GPUI form per row; walk every row against upstream; fix gaps or record them with ⚠️
- [ ] best effort: accessibility ids from `conformance/ui-ids.yaml` as element ids / AccessKit names (no Linux UI
      driver exists, and GPUI's AccessKit on Linux is unverified)
- [ ] tests for every gap fixed during the walk; gates + conformance

### Task 34: Switch to GPUI and remove the GTK frontend

**Files:**
- Modify: `crates/agterm-gpui/Cargo.toml` (bin `agterm`)
- Delete: `crates/agterm-gtk/`, `crates/agterm-ghostty/`, `crates/agterm-ghostty-sys/`
- Delete: `scripts/build-ghostty.sh`, `scripts/gen-ghostty-bindings.sh`, `scripts/build-layer-shell.sh`
- Modify: `build/pins.env`, `scripts/stage.sh`, `scripts/run-dev.sh`, `conformance/tools/run-rust-linux.sh`
- Modify: `conformance/rust-passing.txt` (becomes the GPUI ratchet; `rust-gpui-passing.txt` folded in)

- [ ] precondition: `rust-gpui-passing.txt` ⊇ `rust-passing.txt`, no checklist row the GTK build passes is open on GPUI
- [ ] binary rename; staging without `libghostty.so`/layer-shell libs; terminfo (`tic`), themes and shell integration
      from the Ghostty source at `LIBGHOSTTY_VT_REV` (Technical Details > Resources)
- [ ] remove the GTK-era crates and scripts; `heap_trim.rs` is GTK-GL specific and does not carry over unless GPUI
      memory measurements show the same need
- [ ] ➕ (Task 4) the two Ghostty pins become one: the `GHOSTTY_` libghostty lines go, `LIBGHOSTTY_VT_REV` is the only
      Ghostty pin; `gen-ghostty-config-keys.sh` and the config-diagnostics residual list move to it
- [ ] full conformance on the switched build; ratchet unchanged or grown; gates

### Task 35: Verify acceptance criteria

- [ ] every Overview requirement implemented; GTK removed only after parity including HTML overlays
- [ ] edge cases: kitty keyboard protocol apps, IME on a Cyrillic layout, wide/emoji text, huge output throughput,
      config diagnostics counts, single-instance forwarding (incl. a GTK-build launch), quick terminal on niri and the
      fallback, SIGTERM deadline
- [ ] `cargo fmt --all --check`, clippy `-D warnings`, `scripts/lint-report.sh`, `cargo test --workspace`
- [ ] full `conformance/tools/run-rust-linux.sh` in the background; ratchet green; chrome probe and sprite goldens green

### Task 36: [Final] Update documentation

- [ ] README.md: build prerequisites (Zig for libghostty-vt and zmx, Vulkan, WPE WebKit), run and test commands
- [ ] CLAUDE.md: crate map (`agterm-app`, `agterm-ghostty-vt-sys`, `agterm-ghostty-vt`, `agterm-term`, `agterm-gpui`,
      `agterm-wpe*`), terminal reference rule
      (libghostty-vt + references), fork coexistence via the D-Bus name served by `zbus`, headless Vulkan (lavapipe)
- [ ] `conformance/docs/rust-build.md`: GPUI build, staging, lavapipe
- [ ] move this plan to `docs/plans/completed/`

## Post-Completion

*Items requiring manual intervention or external systems — no checkboxes, informational only*

**Manual verification**:
- a few days of daily use on niri/NVIDIA: long sessions, many panes, memory over time, video in HTML overlays,
  fractional scaling, multi-monitor quick terminal
- latency feel against the GTK build (typing, scrolling in `less`/`vim`)

**External system updates**:
- offer the GPUI dmabuf external-texture element to Zed upstream; drop the `[patch]` once merged
- issues/PRs to `ghostty-org/ghostty` for libghostty-vt C API gaps hit in Tasks 9–11, 16 (e.g. a deferred clipboard
  read reply)
- the GPUI patches (Technical Details > GPUI patches 2–5) offered to Zed; the exit SIGSEGV reported to gpui/wgpu
- replace the installed GTK build on the machine after the switch
