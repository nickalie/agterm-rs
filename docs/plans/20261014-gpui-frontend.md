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
  7. ➕ `TextSystem::font_metrics` (the line gap and underline of a face) — Task 15, `gpui-pre` 0002;
  8. ➕ `Window::paint_alpha_mask` (a caller-rasterized coverage mask from the sprite atlas, tinted like a glyph) —
     Task 15, `gpui-pre` 0003.
  9. ➕ (Task 14) the native key behind a key event on Linux (`gpui-pre-linux`: XKB keycode, layout character,
     modifiers with Shift, for physical-key binds and the Kitty protocol), installed for one dispatch, and a
     `text/html` clipboard offer (Wayland and X11) — Task 14, `gpui-pre-linux` 0001.
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
- ➕ Create: `crates/agterm-term/src/{scan,png,bench}.rs`, `tests/support/mod.rs`; `crates/agterm-ghostty-vt/src/
  {effects,cell,shape,sys}.rs`, `tests/{effects,sys}.rs`; Modify: `Cargo.toml` (workspace deps `portable-pty` 0.9,
  `png` 0.18)

- [x] `Pane`: `Mutex<Vt>` (terminal, encoders, selection), `portable-pty` child, IO thread reading the PTY into
      `vt_write` in bounded slices, `WRITE_PTY` replies into a PTY writer queue (never a blocking write under the
      lock), bounded write queue (backpressure)
- [x] effects → `PaneEvent` (title, pwd, bell, desktop notification, progress, program status, clipboard write, color
      changes, child exited with status): callbacks copy into a channel the frontend drains on its main thread, never
      block and never re-enter `vt_write`; the wrapper reaches callback userdata without aliased `&mut`
- [x] clipboard read (OSC 52 / 5522): pick and implement the mechanism of Technical Details > Threads (synchronous
      reply from a main-thread-maintained snapshot, or interception with `ghostty_osc_*` and a PTY reply after the
      prompt, or an upstream deferred reply); never wait for the UI under the VT lock; parity with the GTK prompt
- [x] resize (cells + pixels) to VT and PTY; scrollback limit from Task 7's config
- [x] render capture in `agterm-term` types (`render.rs`): **one** capture render state per pane (dirty flags have one
      consumer), lock order VT → capture, `try_lock` on paint, two-phase update with only `begin_update` under the VT
      lock, frame = rows shared as `Arc` with dirty flags, cells (graphemes, width, style, colors, hyperlink id), cursor
      (position, style, visibility), mouse shape, palette and default colors; every view draws that frame
- [x] synchronized output: the `RENDER_HOLD` callback captures the held frame under the capture lock; paint publishes
      that frame and does not update until the hold ends
- [x] process-global setup once: PNG decoder (`ghostty_sys_set`, `png` crate), log callback into `tracing`
- [x] public API exposes only `agterm-term` types (Technical Details > Engine boundary)
- [x] profile the read path through zmx (Task 1: `cat` of 50 MiB took ~1.45 s behind zmx vs 0.41–0.51 s for the GTK
      build behind the same zmx, while plain-shell runs were within 1.0–1.7×): read sizes, per-read wakes, slicing;
      target within 1.5× of the GTK build through zmx
- [x] measure the lock: worst-case paint wait and key-to-PTY latency under a 50 MiB `cat`, a kitty PNG burst and a
      resize/reflow of a full scrollback; if either exceeds one frame (16.7 ms), switch `agterm-term` internally to the
      spike's actor-per-pane model (same public API) and record it here
- [x] tests (real PTY): `printf` output on screen; OSC 0/2, 7, 9/777, 9;4, BEL, 52 write and read produce events or
      replies; exit status; resize reaches the child (`stty size`); frame dirty flags after partial updates, two views
      of one pane drawing one frame, a mode 2026 hold publishing the held frame, styles and wide cells; errors: spawn
      failure, PTY closed mid-write, backpressure limit, `try_lock` contention keeps the previous frame
- [x] gates — must pass before Task 10
- result (2026-10-15):
  - **Clipboard read** (Codex-reviewed, transcript
    `~/.local/state/plan-exec-revmux/gpui-frontend/discussions/task-9-osc52-read/transcript.md`): "park before the
    dispatch byte". The IO thread runs `scan.rs`, a mirror of Ghostty's parse table (`parse_table.zig`: anywhere/C1
    transitions, escape intermediates, DCS/APC/SOS strings ended by ESC or CAN/SUB) with the OSC 52 (`52;<0–1 byte
    selector>;?`) and OSC 5522 (`type=read`, last duplicate wins, padded base64, invalid metadata values dropped)
    classifiers; it feeds everything before the byte Ghostty dispatches the OSC on (BEL, or the **ESC** of ST — Ghostty
    dispatches there, not on `\`), releases the VT lock, emits `PaneEvent::ClipboardRead { id, location, scope }`
    and waits for `Pane::answer_clipboard_read` (or shutdown) with no lock held; then arms the answer, feeds the
    dispatch byte alone, so libghostty's own callback validates and serializes the reply (OSC 52 and 5522 statuses,
    ids, terminators, ordered with its other replies), and disarms what was not taken. The callback denies when
    nothing is armed, the clipboard differs, or a list-only answer meets a data request. The frontend applies
    `clipboard-read` and the app-wide `ClipboardPromptPolicy` as GTK does (a listing needs no consent). ⚠️ Difference
    from GTK: while a read waits for its answer the pane reads no output (libghostty-vt's synchronous model,
    `terminal.h:1043-1048`); full libghostty keeps streaming. A deferred-read API upstream would lift it. Kitty
    session grants (`remember`) are never set; OSC 5522 writes are acknowledged before the frontend's `clipboard-write`
    decision.
  - **Thread model**: the mutex protocol stands; no actor fallback. Release build, 141×41 cells, a UI stand-in
    painting every 16 ms (`try_lock`, never waits) and taking the VT lock every 4 ms as key encoding will
    (`src/bench.rs`, `cargo test --release -p agterm-term --lib bench -- --ignored`): longest IO-thread hold of the VT
    lock 0.5–0.8 ms during a 50 MiB `cat` (64 KiB slices), **7.1 ms** during 10 Kitty PNG transmissions of 1080p
    (decode under the lock); a resize reflowing a full 50 MB scrollback takes 7–10 ms on the caller; key-path lock
    wait max 1.5 ms (`cat`), 6.4 ms (PNG burst), 7.8 ms (reflow) — all under 16.7 ms. Paint is stale (previous frame
    drawn) in 3–15 % of frames under the `cat`, never more than two in a row.
  - **zmx read path**: the IO thread `poll`s, reads, and drains whatever is ready right away into one batch (up to
    256 KiB), feeds the VT in 64 KiB slices, then wakes the frontend once (coalesced until it paints). Through zmx the
    client writes ~3.6 KiB at a time, so batches stay ~9 KiB (≈6 000 per 50 MiB vs ≈550 on a plain shell). `cat` of
    50 MiB: **0.73–1.0 s through zmx** (spike: 1.45–1.55 s), 0.49–0.53 s plain; a reader doing no VT work at all needs
    0.69–0.83 s through the same zmx, so the pane is at zmx's own ceiling. ⚠️ The spike's GTK baseline "behind the same
    zmx" was not behind zmx: the conformance harness launches without `restoreMode: live` (its pane's parent is
    `agterm`, no `ZMX_*`). Re-measured back to back at the harness's 71×27 grid: GTK through live zmx 2.8–3.6 s (17 s
    in a run under load 35–96), GTK plain 0.63–0.92 s, the pane through zmx 0.73–0.76 s and plain 0.49–0.51 s — within
    the target by a wide margin.
  - Also in this task: theme colors (`ThemeColors` from the config's `background`/`foreground`/`palette`/
    `cursor-color`) are set on the terminal, since a render state reports default colors only when both are set;
    `Pane::set_colors`/`set_scrollback`; DA1/DA2 answered as Ghostty does (`62;22;52`, `1;10;0`), XTVERSION
    `ghostty <libghostty-vt version>`; a full reset reports an empty title and pwd. Cells carry a hyperlink flag (the
    C API has no per-cell link id; the URI is read through grid refs with links in Task 14). The IO thread never
    stops reading output for queued replies (a child blocked on output while we wait for it to read would deadlock);
    replies past 16 MiB queued are dropped instead. The PTY end closes the write queue (`PaneError::Closed`); the
    child gets `SIGHUP` through a pidfd on drop. 50 repeated runs of the two crates' tests: no failures.

### Task 10: agterm-term — input, selection, search, read-back

**Files:**
- Create: `crates/agterm-term/src/{input,selection,search,text,kitty}.rs`, `tests/input.rs`, `tests/selection.rs`,
  `tests/kitty.rs`
- Modify: `crates/agterm-ghostty-vt/src/` (key/mouse encoders, selection, formatter, paste, search, kitty graphics)

- [x] toolkit-neutral key struct (physical key, text, modifiers, action) → the C API's key encoder (`vt/key/`) per
      terminal modes (legacy, kitty), encoded under the VT lock (the encoder reads modes); mouse → `vt/mouse/` encoder;
      focus events (`focus.h`); bracketed paste via `paste.h`
- [x] selection (cell ranges, word/line, select all) via `selection.h`; selected text via `formatter.h`, created and
      run under the VT lock
- [x] scrollback search via `search.h` (matches, selected index, next/prev) — replaces the search actions;
      `GhosttySearch` in its own mutex, terminal-touching calls under the VT lock, `tick`/`get` on a worker
- [x] screen/scrollback read-back via `formatter.h` matching what `ghostty_surface_read_text` returned; cursor column
- [x] kitty view in `agterm-term` types (`kitty.rs`) from `kitty_graphics.h`: images (id, generation, format, pixels —
      copied under the VT lock, handles die on mutation), placements (layer, cell rect, source rect, pixel size),
      deletions
- [x] the safe API for each of these lands in `agterm-ghostty-vt` with its own unit tests; `agterm-term` wraps it
- [x] tests: legacy and kitty encodings (arrows, F-keys, ctrl/alt), bracketed paste, selection text, search counts and
      navigation, read-back of wide chars and wrapped lines, kitty APC (chunked) → image and placement in the view
- [x] gates — must pass before Task 11
- result (2026-10-15):
  - `agterm-ghostty-vt` gains `key` (`KeyEncoder`, `encode_focus`), `mouse` (`MouseEncoder`), `paste`
    (`Terminal::paste`), `selection` (`Point`, set/clear/word/line/all, read back in any space), `format`
    (`Terminal::format` of a `Region`: viewport, screen, range, selection), `search` and `kitty` (`KittyGraphics`
    borrows the terminal mutably, so no lent handle outlives a mutation), plus `Terminal::{scroll_viewport,
    total_rows, mouse_tracking, set_kitty_image_media}` and `RowRef::selection`. The library's match snapshots die on
    the next mutation, so `Search::{feed, run, select}` return owned `SearchResults` read inside the call; `tick` and
    the counters need no terminal; `Search::close(&mut Terminal)` frees, and a search dropped unclosed is leaked
    rather than race a terminal another thread writes.
  - Keys: `KeyInput` carries core's `PhysicalKey`/`Mods`/`KeyAction`, so one event serves the binds stage
    (`binds_event`) and the encoder; ghostty's key names and the C key enum list the keys in one order, so the engine
    key is the name's index (tested). Mouse: `setopt_from_terminal` and a size change reset motion de-duplication, so
    the encoder re-reads the terminal only when one of the eight mouse-mode bits flips (⚠️ residual: re-setting a
    mode that is already set does not switch Ghostty's "last one set wins"). Focus reports only in mode 1004.
  - Paste: `ghostty_terminal_paste` (mode 2004 framing, unsafe text → `Paste::Unsafe` for confirm-and-retry, a Kitty
    paste event under mode 5522) with its PTY writes diverted into one input chunk, so a paste is queued whole on the
    bounded input queue instead of the capped reply queue.
  - Selection: the terminal owns and tracks it; frame rows carry their selected columns (`Row::selection`), and every
    change marks the capture so the next frame re-checks each row. The selection gesture state machine
    (`ghostty_selection_gesture_*`, drag and multi-click timing) is bound but not wrapped: Task 14's.
  - Read-back: plain, unwrapped, untrimmed, viewport or screen top-left to bottom-right, as `dumpTextLocked` read it;
    neither formatter emits trailing blank rows. ⚠️ Residual: the libghostty formatter moves a range ending on a
    wide character's spacer head to the next row, the one at `LIBGHOSTTY_VT_REV` does not. `SelectionText::Raw` is
    `read_selection`, `SelectionText::Copy` trims as `copy_to_clipboard` does.
  - Search: `Pane::search` (`search:NEEDLE`, empty = `end_search`), `navigate_search` (`next` toward older output),
    `search_state` (counts, highlights in viewport cells); a worker feeds every 24 ms (Ghostty's `REFRESH_INTERVAL`)
    under the VT lock and ticks under the search mutex alone. `PaneEvent::SearchTotal`/`SearchSelected` carry what
    libghostty reported: the selected index is 0-based newest-first, as `SEARCH_SELECTED` delivered it (upstream and
    core pass it through as is), so the control API's output stays the GTK build's.
  - Kitty: 320 MB storage and file, temp-file and shared-memory media, as Ghostty's termio sets them; the view is
    captured with the frame under the VT lock, pixels copied only for a new `(id, generation)` and shared by `Arc`
    otherwise; only images a placement shows are listed; a pending payload waits; `removed` is against the previous
    frame.
  - 40 consecutive runs of both crates' tests: no failures.

### Task 11: agterm-term — launch, terminal identity, foreground process

**Files:**
- Create: `crates/agterm-term/src/launch.rs`, `tests/launch.rs`
- Modify: `crates/agterm-term/Cargo.toml` (dep `agterm-zmx`)
- ➕ Create: `build/patches/zmx/0003-first-attach-snapshot.patch`, `crates/agterm-term/src/exit.rs`,
  `crates/agterm-app/tests/pane_foreground.rs`; Modify: `crates/agterm-term/src/{pane,effects,lib,bench}.rs`,
  `crates/agterm-core/src/zmx/shell_integration.rs` (`setup`), `crates/agterm-core/src/ghostty_config/model.rs`
  (`ShellIntegrationFeatures::{UNSUPPORTED, env_value}`), `crates/agterm-app/Cargo.toml` (dev-dep `agterm-term`),
  `scripts/build-zmx.sh` (header), `.claude/skills/agterm-upstream-sync/references/areas.md`

- [x] live panes: launch from `agterm-zmx` launch inputs (`zmx attach`, shell-integration env from core)
- [x] ➕ (Task 4) fix the zmx first-paint race (Task 1: `zmx attach` paints nothing until it sees a real size change
      after its attach handshake; a prompt printed between the client's attach and its first snapshot is held by the
      daemon, a resize before the handshake is lost; 1–4 of every 6 launches stayed blank): the real fix goes into
      the zmx patch (`build/patches/zmx`) if the race is in zmx, otherwise the launch code waits for the handshake and
      then resizes once; the spike's timed resize nudge is not the fix. Test: 50 consecutive launches all paint the
      prompt
- [x] plain panes (no live capability), as libghostty spawned them (`agterm-gtk/src/surface.rs` ~675–710): `command`,
      `initial_input`, `wait_after_command` (the child-exited state that `SHOW_CHILD_EXITED` drove), working
      directory, env merge, shell-integration injection
- [x] terminal identity per Technical Details (`TERM`, `TERMINFO`, `COLORTERM`, fallback, `term` override), terminfo
      from the existing stage
- [x] foreground pid exposed by the pane; `agterm-app`'s process table consumes it (no dependency of `agterm-term` on
      `agterm-app`)
- [x] tests: `TERM`/`TERMINFO`/`COLORTERM` reach the shell (`env` in the pane), fallback without terminfo, plain pane
      `command`/`initial_input`/`wait_after_command`/cwd/env, real zmx under a private `ZMX_DIR` (reaped by that dir
      only); errors: zmx missing, attach failure, session gone, command not found
- [x] gates — must pass before Task 12
- result (2026-10-15):
  - **zmx first-paint race — in zmx** (Codex-reviewed, transcript
    `~/.local/state/plan-exec-revmux/gpui-frontend/discussions/task-11-zmx-first-paint/transcript.md`): `zmx attach`
    of a new session forks the daemon, which starts the program at once, and connects 10+ ms later; output read
    before the daemon accepts the client reaches no client (it is broadcast to connected clients only), the first
    `Init` sends no snapshot (`handleInit` serialized on re-attach only) and its `TIOCSWINSZ` at an unchanged size
    sends no SIGWINCH, so nothing repaints. Without any frontend, `zmx attach s /bin/sh -c "printf READY; exec sleep
    30"` in an 80x24 pty stayed blank 20/20 times. A launch-side wait and resize cannot fix it (only programs that
    repaint on SIGWINCH would recover). Fix: `build/patches/zmx/0003-first-attach-snapshot.patch` (this repository's
    own): the first attach is sent the snapshot when the program already wrote; every snapshot (first and re-attach)
    is followed by the daemon parser's unfinished sequence (Ghostty's stream continuation tracking, limit as
    libghostty's snapshots), so a sequence cut at the attach continues identically in the client; and a client the
    accept already put among the broadcast's receivers (output sent between its connect and its Init, first attach or
    re-attach) gets a reset ahead of the snapshot that repeats that output: CAN, SGR 0, OSC 8 closed, G0–G3 ASCII
    and SI, DECSCA 1 then 0 (the 1 leaves ISO protection, which SPA sets and every erase honors), IRM off, `?69l`,
    `?6l`, `ESC[r`, `ESC[H`, ED 0 — everything the cursor carries into the snapshot's text (an erase paints with the
    pen's background; the snapshot writes default-style text without an SGR and restores the cursor's style, link
    and charsets only after its text). Review fixes: the first version reset with `ESC[H ESC[2J` on first attach
    only (homed into an origin-mode margin, left re-attach unprotected), the second left the pen, link and charsets,
    the third left SPA-protected cells standing (DECSCA 0 alone keeps ISO protection on). Not taken: holding output from a client until its Init (no reset needed at all)
    — `zmx tail` connects and only reads, never sending an Init or anything else, so that needs a client protocol
    change or a timer. zig tests: 110/110, convergence tests feeding a client terminal across CSI/OSC/DCS/APC/UTF-8
    cuts before connect, and in the connect→Init broadcast with 20 lines of scrollback, origin mode, the alternate
    screen, a colored pen, an open hyperlink, DEC graphics in G0 and shifted G1, cells written under DECSCA and under
    SPA, insert mode and left/right margins, first and re-attach (a broadcast client must end as one that connected
    after it; without any reset 12 cases differ, with the second reset 8, with the third the 4 SPA ones). Re-attach scrollback replay is unchanged (a snapshot's history rows that
    never scrolled off are erased by its own `ESC[2J`: the serializer's, before and after). Residues: a string
    sequence (OSC/DCS/APC) cut in that broadcast window is dispatched early by a Ghostty client (any exit from a
    string state dispatches), with whatever external effect it has; tab stops, Kitty images and a saved cursor the
    broadcast set stay; a broadcast that scrolled is in the client's scrollback and comes again with the snapshot's
    history (the screen and title still match; ED 3 would also erase a plain `zmx attach`'s outer history). Results: repro 60/60 painted (was 20/20 blank); `tests/launch.rs` through the
    real Pane: 50 sequential + 54 parallel (6 at a time) fresh launches all painted, prompt within ≤ 263 ms (old zmx:
    the first launch already blank); 30 consecutive runs of agterm-term's tests (each 20 + 24 fresh live launches,
    1 320 in all, alongside a full GTK conformance run) all painted and passed. The bench's resize nudges are gone. The zmx build id changes, so
    daemons of the previous build read as outdated (the existing reset path).
  - **Launch** (`launch.rs`): `Pane::launch(&Launch, &Host, &PaneConfig)`. `Launch` = a session pane's seed
    (`Launch::from_seed` of core's `LaunchSeed`, the seed's variables over the pane's, the GTK `merged_env`) plus
    `env_remove`; `Host` = login shell, resolved resources (`resolve_resources`/`resource_candidates`: staged
    `share/ghostty` with `../terminfo/x/xterm-ghostty` beside it, then `/usr/share/ghostty`), config `term`,
    `shell-integration-features` minus the ssh wrappers, `cursor-style-blink`, bin dir; `Host::from_runtime` takes an
    `agterm_zmx::Runtime`. Everything runs as `/bin/sh -c <line>` (core's foreground reader looks past exactly that).
    Environment in libghostty's order: `GHOSTTY_LOG`/`VTE_VERSION` removed; identity (`TERM` = `term` + `TERMINFO` +
    `GHOSTTY_RESOURCES_DIR` when resolved, else `xterm-256color` and both removed, `COLORTERM=truecolor`,
    `TERM_PROGRAM=ghostty`, `TERM_PROGRAM_VERSION` = libghostty-vt's, `GHOSTTY_BIN_DIR` appended to `PATH`);
    `GHOSTTY_SHELL_FEATURES`; the shell integration of a zsh/bash/fish line (core `shell_integration::setup`, a port
    of `setup`/`setupBash`: bash → `--posix`, `GHOSTTY_BASH_INJECT` flags, `GHOSTTY_BASH_RCFILE`); the launch's
    variables; `PWD`. A working directory that is not one is ignored. `initial_input` is queued at spawn.
  - **Exit** (`exit.rs`, `ExitPolicy`, `ExitStatus::held`): a command not waited for ends its pane; a nonzero exit
    within 250 ms holds on libghostty's failure report ("Ghostty failed to launch the requested command", rule,
    runtime, exit code); a waited command holds on "Process exited. Press any key to close the terminal."; otherwise
    the pane ends. The message goes through the parser, so it starts with CAN (a program that died mid-sequence
    would otherwise eat it; libghostty wrote it on the terminal directly). A held terminal hides the cursor and turns
    KAM and Kitty keyboard flags off, so the frontend closes
    on any key that encodes (Task 14). `Pane::spawn` alone keeps `ExitPolicy::Report`.
  - **Foreground**: `Pane::foreground_pid` = `tcgetpgrp` on the PTY; `agterm-app/tests/pane_foreground.rs` reads it
    through `ProcTable` + core's `ForegroundReader` (a command pane's program past `/bin/sh -c`, a job typed into the
    shell).
  - ⚠️ Not carried over: the config keys `command`, `env`, `input`, `working-directory`, `shell-integration`,
    `wait-after-command`, `abnormal-command-exit-runtime` (outside Task 7's model; the launch's values and Ghostty's
    defaults stand), the elvish/nushell integrations, `GHOSTTY_SURFACE_ID`. A live seed's `settled` prefix (a GTK
    adaptation: it waits up to 200 ms for a resize libghostty made after spawn) still runs; a GPUI pane spawned at its
    final size can drop it (Tasks 18/27).

### Stage 2 — GPUI frontend core

### Task 12: agterm-gpui skeleton — app state, main loop, server, signals, single instance

**Files:**
- Create: `crates/agterm-gpui/Cargo.toml` (bin `agterm-gpui` until the switch), `src/{main,app,state,main_loop,
  signals,instance}.rs`, `src/state/tests.rs`
- Modify: `Cargo.toml` (workspace deps `gpui-kit`, `zbus`)
- ➕ Create: `crates/agterm-gpui/src/{lib,activation}.rs`, `src/main_loop/tests.rs`, `tests/instance.rs`,
  `tests/support/mod.rs`; `vendor/calloop/`, `build/patches/calloop/0001-tracing-without-log.patch`,
  `scripts/vendor-calloop.sh`, `scripts/check-tracing-features.sh`,
  `docs/upstream-issues/gpui-wgpu-gles-exit-segfault.md`; Modify: `Cargo.toml` (also `raw-window-handle`,
  `wayland-*`, `x11rb`, `signal-hook`, `exclude = ["vendor"]`, `[patch.crates-io] calloop`), `Cargo.lock`,
  `.github/workflows/rust-linux.yml`, `README.md`

- [x] gpui-kit app on the main thread; `MainLoop` over the foreground executor; `AppState` and its `settle` ported from
      `agterm-gtk/src/state.rs` as a skeleton here — it grows task by task as deck (Task 18) and windows (Task 26) land
- [x] `version` capabilities start empty and are claimed task by task (Development Approach)
- [x] `agterm-server` started as in GTK `main.rs`; state dir, logging (`agterm-app`), app id kept
- [x] single instance per Solution Overview: `AGTERM_APP_ID` with GApplication's validity rule and object-path
      derivation, bus name, `org.gtk.Application` + `org.gtk.Actions` via `zbus`, both forwarding directions,
      activation token (per Task 3)
- [x] SIGTERM → orderly quit within 10 s (rust-build.md contract), installed only once this process is the primary
- [x] ➕ (Task 3) `Application::with_quit_mode(QuitMode::Explicit)`: GPUI's Linux default quits when the last window
      closes, which hiding the quick terminal (a `remove_window`) triggers
- [x] ➕ (Task 3) activation per Solution Overview > Single instance: read `XDG_ACTIVATION_TOKEN` and
      `DESKTOP_STARTUP_ID` before GPUI's platform is created (GPUI removes the variable); a forwarding launch sends them
      as `activation-token` / `desktop-startup-id`; the primary activates its window with a received token by binding
      `xdg_activation_v1` on GPUI's `wl_display` through raw-window-handle (Task 3's ~20-line workaround, until GPUI
      patch 2), and on X11 by `_NET_ACTIVE_WINDOW` with the startup id's `_TIME`, `_NET_WM_USER_TIME` and the
      startup-notification `remove` through x11rb on the raw handle (until GPUI patch 3)
- [x] ➕ (Task 2) the GPUI exit SIGSEGV (wgpu's GLES instance tears down EGL in a TLS destructor after the Wayland
      connection is gone): find what creates the GLES instance under a Vulkan renderer and avoid it, or end the
      process after the orderly shutdown before TLS destructors run, so a quit exits 0; file the upstream issue
      (gpui/wgpu) with the stack from `docs/spikes/20261014-wpe-dmabuf.md` quirk 12 (drafted in
      `docs/upstream-issues/gpui-wgpu-gles-exit-segfault.md`, not posted: the run never posts externally)
- [x] tests: `MainLoop` hop ordering and shutdown, `settle` cases carried over from GTK, SIGTERM deadline and exit
      status 0, forwarding of a second GPUI launch and of a GTK-build launch to the GPUI instance, and of a GPUI launch
      to a GTK primary (private bus); the activation token reaches the primary's platform data from a GPUI and from a
      GTK launch, under a nested niri where installed (stock config, no `honor-xdg-activation-with-invalid-serial`;
      skipped with a message otherwise) and under Xvfb; invalid
      `AGTERM_APP_ID` falls back; never two writers of `windows.json`
- [x] ➕ calloop's `tracing/log` (see the result): vendored calloop without it, a guard script before clippy
- [x] gates — must pass before Task 13
- result (2026-10-15):
  - **Shape** (Codex-reviewed, transcript
    `~/.local/state/plan-exec-revmux/gpui-frontend/discussions/task-12-app-state-loop/transcript.md`): `AppState` is a
    GPUI `Entity` in the app and a plain struct in its tests; `settle` stays context-free and records `FrontendWork`
    (activations, quit) that the loop's consumer lands with `&mut App` after each task, the state released.
    `GpuiMainLoop` = an `async_channel` queue + GPUI's background executor for `post_after` timers; delayed tasks wait
    in a registry, so `close` drops them (and their reply senders) at once, not at their deadline; one foreground task
    (`main_loop::drive`) runs them in order. The bus and SIGTERM enter as ordinary main-loop tasks (the bus handlers
    buffer in a channel created before GPUI exists, drained into the loop at launch). The crate is a library plus the
    thin binary, so its integration tests and later tasks reach the modules (a binary-only crate would flag every
    not-yet-wired method as dead code).
  - **Single instance**: interfaces exported before `RequestName(DO_NOT_QUEUE)` (zbus reports `Exists` as
    `Error::NameTaken`); a forwarding launch sends `Activate` with no reply expected (GApplication's fire-and-forget)
    carrying `cwd` (`ay`), `activation-token` and `desktop-startup-id`, and exits before GPUI or the state dir; no
    session bus = a primary of its own, as GApplication. Activation: Wayland per Task 3 (xdg-activation on GPUI's
    `wl_display`), X11 on a connection of its own to the same display with the raw handle's window id
    (`_NET_ACTIVE_WINDOW` source 1 + the `_TIME`, `_NET_WM_USER_TIME`, startup `remove` from an InputOnly messenger
    window). A placeholder window (the app id as its Wayland app id / X11 class) stands for the window shell of
    Tasks 18/26; the last window closing quits.
  - **Exit**: GPUI creates its wgpu instance with `Backends::VULKAN | Backends::GL` (`gpui-pre-wgpu`
    `wgpu_context.rs:473`), which no app setting removes, so `main` ends with `libc::_exit(status)` after the orderly
    shutdown (`std::process::exit` would not help: glibc's `exit` runs `__call_tls_dtors` first). The crash did not
    reproduce on Mesa (a windowed primary on headless weston exited 0 with plain `exit`, on Intel GL and on
    lavapipe): it may need NVIDIA's EGL (quirk 12's machine); the `_exit` stays until a fixed GPUI is vendored.
  - ⚠️ **calloop's `tracing/log`** (Codex-reviewed, transcript
    `~/.local/state/plan-exec-revmux/gpui-frontend/discussions/task-12-tracing-log-complexity/transcript.md`): calloop
    (GPUI's Linux event loop) requests tracing's `log` feature unconditionally; workspace feature unification spread it
    to every crate, whose tracing macros then expand with the `log` fallback that clippy's `cognitive_complexity`
    counts (~10 points per call site): 29 existing functions in core, server, app, term and gtk went over 15 (e.g.
    `ControlServer::new` 28, a three-arm match with two warns). Splitting would only have wrapped log calls in
    helpers, so `vendor/calloop` is the verified crates.io 0.14.5 source plus one manifest patch dropping the feature
    (`[patch.crates-io]`, `scripts/vendor-calloop.sh [--check]`); `scripts/check-tracing-features.sh` (CI, before
    clippy) fails if any crate asks for `log`/`log-always` again, naming it. LogTracer still bridges `log` records
    into agterm's subscriber.
  - Tests: 41 unit tests (main loop on GPUI's test executor, the GTK settle cases, instance, activation helpers) and
    9 process tests in private sandboxes (`tests/support`: fresh root, private dbus-daemon, env built from scratch,
    an isolation check on every command; displays only private: headless weston + lavapipe, Xvfb, niri nested in
    headless weston, all rendering through Mesa software): forwarding with token and startup id and no state written
    by the forwarding launch, GTK build → GPUI primary and GPUI → GTK primary (the GTK build's `agterm` beside the
    binary, else skipped), X11 activation with the startup time for a GPUI and a GTK launch, nested-niri focus by a
    launcher's token for a GPUI and a GTK launch, SIGTERM headless and windowed (status 0, < 10 s), invalid app id,
    no bus. CI installs the GPUI build's xkbcommon/xcb dev packages and dbus-daemon. 20 consecutive runs of the
    crate's tests: no failures. Gates green; full GTK conformance: 653 pass, 0 fail, ratchet holds (665 ids).
  - Deferred: the CLAUDE.md crate-map row for `agterm-gpui` (Task 36); `AppState::set_key_window` is wired by the
    window shell (Task 18); the live sessions reset's relaunch (Task 27) only logs here.

### Task 13: TerminalElement — grid rendering and the Surface impl

Port from gpui-ghostty's view (Apache-2.0) and `qwertty-term-renderer`/`qwertty-term-font` (MIT), attribution in file
headers, rebuilt on `agterm-term` and current GPUI.

**Files:**
- Create: `crates/agterm-gpui/src/terminal/{element,metrics,row_cache,colors,surface}.rs`, `terminal/tests.rs`
- ➕ Create: `crates/agterm-gpui/src/terminal.rs` (module root: `Terminal`, `Shared`, `TerminalView`),
  `terminal/font.rs`; Modify: `crates/agterm-gpui/{Cargo.toml,src/lib.rs}`, `Cargo.toml` (dep `agterm-term`, dev-dep
  `gpui-pre-wgpu =0.3.9`), `crates/agterm-term/src/{render,pane,lib}.rs` + `tests/render.rs` (`CursorDefaults`),
  `crates/agterm-ghostty-vt/src/terminal.rs` + `tests/link.rs` (`set_default_cursor`), `.github/workflows/rust-linux.yml`
  (`fontconfig`, `fonts-dejavu-core`); review fixes: Create `crates/agterm-term/src/actions.rs`,
  `crates/agterm-term/tests/actions.rs`; Modify `crates/agterm-term/src/input.rs`, `crates/agterm-ghostty-vt/src/
  selection.rs` (terminal-scoped actions), `crates/agterm-gpui/Cargo.toml` (`tempfile` a dependency)

- [x] cell metrics from the configured font; padding incl. `window-padding-balance`; resize → pane resize
- [x] frame per Technical Details > Frame: merged background quads, shaped runs with a row cache over dirty rows,
      decorations, cursor styles, focus state; colors from config/theme and OSC overrides; wide chars and graphemes
- [x] several views of one pane (the dashboard draws a second, view-only one): render state per view, input only on
      the owning view
- [x] `agterm_host::Surface` impl over the pane — every method GTK's `surface.rs` implements: `pane_token`,
      `is_realized`, `is_live_session`, `promote_to_primary`, `type_text` (keystroke path with `SUBMIT_GAP`),
      `read_text`, `read_selection`, `cursor_column`, `binding_action` (Task 8), `foreground_pid`, `font_size`,
      `cell_size`, `teardown`
- [x] "first frame drawn" debug log line under `agterm::surface` (what `ghostty_probe.py` reads)
- [x] tests (headless): metrics for a known font, dirty-row cache invalidation, run splitting by style, cursor styles,
      wide-char placement, two views of one pane, `Surface` methods against the fakes' contract
- [x] gates — must pass before Task 14
- result (2026-10-15):
  - **Shape** (Codex-reviewed, transcript
    `~/.local/state/plan-exec-revmux/gpui-frontend/discussions/task-13-terminal-design/transcript.md`): `Terminal` (a
    GPUI entity, `EventEmitter<TerminalEvent>`) owns one foreground pump per pane, woken through a one-token channel by
    the pane's `PaneConfig::wake` and by the Surface; each wake drains the pane's events (`TerminalEvent::Pane`), runs
    the queued app work (clipboard read/write, events) and notifies the views. `Shared` (`Rc`, main thread) holds the
    pane, the live font size, the cell metrics (computed synchronously through the app's `Arc<TextSystem>` at the
    owner's last scale, so `font_size`/`cell_size` are right right after a font action), the owner's grid and the
    queue. `TerminalSurface` implements `agterm_host::Surface` over `Shared` with no GPUI context: binding actions are
    accepted for a realized terminal (as the GTK build's were) and parsed through Task 8's `Action`; the performer
    (`TerminalSurface::perform`, `binds::Performer`) does `csi:/esc:/text:`, copy/paste (standard and primary),
    `select_all`, font size (Ghostty's clamps; reset to the spec's or config's start size), `search:`/`start_search`/
    `end_search`/`navigate_search`, the `scroll_*` actions and `jump_to_prompt`; other actions return false (Task 14
    routes app, window and split actions). `TerminalView` (one entity per view, `ViewRole::Owner | Viewer`) keeps its
    own row cache over the pane's shared frame; only the owner lays the grid out, starts the pane (`Spawn`, a boxed
    `FnOnce(PaneConfig) -> Result<Pane, PaneError>`, deferred out of prepaint with `cx.defer`) at its first nonzero size
    and resizes it; a viewer draws at its own font size and never spawns, resizes or takes focus. Teardown drops the
    pane (SIGHUP to the child) and closes the pump's channel. Events: `Spawned`, `SpawnFailed`, `FontSize`,
    `StartSearch`, `EndSearch`, `UnsafePaste` (a refused paste; its confirmation prompt is Task 14's/21's).
  - **Metrics**: Ghostty's `Metrics.calc` (via qwertty-term-font) in device pixels (a point is 96/72 px, as Ghostty's
    Linux default DPI), the cell width the widest printable-ASCII advance (Ghostty's rule, which equals the
    Technical Details' "advance of M" for monospace fonts), reported in logical pixels; geometry and
    `window-padding-balance` (`false`/`true`/`equal`) from Ghostty's `size.zig`. ⚠️ Residual (correctness gap until the
    Task 15 ➕ item): GPUI's public text system exposes neither `line_gap` nor the underline metrics, so the line gap
    counts 0 (cells are shorter than libghostty's for a font with a nonzero `hhea` line gap; DejaVu Sans Mono, JetBrains
    Mono and most monospace fonts have none: DejaVu measures 10x19 at 12 pt as in Ghostty) and underline/strikethrough
    use Ghostty's own estimates.
  - **Frame**: rows planned per view and cached by the frame's `Arc<Row>` identity, the colors' `Arc`, the palette and
    the row's marks (selection, search highlights incl. the selected match, hovered link); shaping redone only for
    rebuilt rows or a font/size/scale change. Cell colors follow Ghostty's `rebuildRow` (inverse, covering full block,
    selection/search colors incl. `cell-foreground`/`cell-background`, faint at Ghostty's 0.5); runs break at style
    changes and blank cells only and are shaped without forced widths, each glyph placed at its cluster's column with
    its offset from the cluster's first glyph (across fallback runs), emoji through `paint_emoji`. Layers: default
    background (`background-opacity` alpha), cell backgrounds with `window-padding-color = extend/extend-always`
    (Ghostty's `neverExtendBg` without the prompt-row check: frames do not mark prompt rows), underlines (single,
    double, curly, dotted, dashed as Ghostty's sprites, hovered links underlined, double over an underline) and
    overlines, text, strikethroughs, cursor (block/bar/underline, hollow when unfocused or in a viewer, Ghostty's 600 ms
    blink while focused, the text under a block in `cursor-text`), the IME preedit at the cursor (`Shared::set_preedit`,
    driven by Task 14). Fonts: `font-family*` (first installed family primary, the others fallbacks; unset or none
    installed → `JetBrains Mono`, `DejaVu Sans Mono`, `Liberation Mono`, …), style families, `font-feature`.
  - **Cursor defaults** (shared crates): `agterm-ghostty-vt` `Terminal::set_default_cursor` (the C API's
    `DEFAULT_CURSOR_STYLE/BLINK`), `agterm-term` `CursorDefaults` in `PaneConfig::cursor` and
    `Pane::set_cursor_defaults`: the terminal starts with and returns (`CSI 0 q`, RIS) to `cursor-style` /
    `cursor-style-blink` (unset blinks, as Ghostty's termio sets it; libghostty-vt's own default is a steady block).
  - "first frame drawn": `terminal surface created` / `terminal drew its first frame` / `could not create the terminal
    surface` at target `agterm::surface` with the surface key, as `ghostty_probe.py` reads them.
  - Tests: 42 in `terminal/tests.rs` (GPUI's test app: its fixed fake face, and GPUI's Linux `CosmicTextSystem` with
    DejaVu Sans Mono found through fontconfig for the known-font metrics; real panes under `/bin/sh` with `AGTERM_*`/
    `ZMX_*` removed; `allow_parking` because the panes' IO threads wake the pump from outside the test scheduler) plus
    the cursor-default and action tests in `agterm-ghostty-vt` and `agterm-term` (`tests/actions.rs`).
  - Review fixes (round 1; Codex-reviewed, transcript
    `~/.local/state/plan-exec-revmux/gpui-frontend/discussions/fixer-task-13-terminal-actions/transcript.md`): every
    terminal-scoped action of Ghostty's `performBindingAction` at `GHOSTTY_REV` is performed with libghostty's
    performed/not-performed answer: `end_search` and an empty `search:` only with a search open (so the default
    `performable:escape=end_search` lets Escape through), `search_selection` (the selection untrimmed, an empty one
    counting), `reset`, `clear_screen` (not performed on the alternate screen), `select_all` (always performed; the
    selection copied per `copy-on-select`, now in core's config model, through `Shared::copy_on_select`, Ghostty's
    `setSelectionAndCopy` with `clipboard-trim-trailing-spaces`),
    `adjust_selection` and `scroll_to_selection` (not performed without a selection), `scroll_to_row`,
    `toggle_mouse_reporting`, `toggle_readonly` (`Shared::is_readonly`; typing, binding text and pastes are dropped,
    the terminal's replies are not), `write_screen_file`/`write_scrollback_file`/`write_selection_file` (a 0600 file
    in a temporary directory of its own, then copy, paste the path, or open through `TerminalEvent::OpenUrl`; performed
    without a region too, writing nothing), `copy_to_clipboard:{plain,vt,html,mixed}` (performed whenever a selection
    exists), `copy_url_to_clipboard` (the URL `Shared::set_hovered_url` holds, not performed without one),
    `copy_title_to_clipboard` not performed (neither the GTK build's nor upstream's embedded apprt handles it).
    `agterm-term` gained `Pane::{reset, clear_screen, adjust_selection, scroll_to_selection, format, mouse_reporting}`,
    `Scroll::Row`, `FormatRegion`, `TextFormat`; `agterm-ghostty-vt` `vt_write_until_ground`, `at_ground`,
    `cursor_at_prompt`, `viewport_top`, `adjust_selection`, `Scroll::Row`. Also: the cell-size report on a config font
    change and a scale change, the blink timer synced after prepaint installs the frame, the cursor colors from a blank
    cell's erased background, an unfocused (hollow) cursor drawn opaque whatever `cursor-opacity` (Ghostty's
    `addCursor`), one glyph-painting helper.
  - ⚠️ `clear_screen` is a documented approximation, not exact parity: libghostty-vt has no erase API (and the vt pin
    takes no patches), so the clear is VT sequences written into the terminal at a parser ground point (at once, or by
    the IO thread at the next ground point of the program's output through `vt_write_until_ground`): at a prompt
    (`OSC 133`) `CSI 3 J` + `CSI 2 J` (Ghostty's own complete erase) and a form feed to the shell; else `CSI n S`,
    `CSI 3 J`, `CSI n A` and a Kitty delete-all, which equals Ghostty's `eraseActive` + `cursorReload` except that a
    pending wrap ends, a scroll region the program set limits the scroll, and the new rows take the pen's background.
  - ⚠️ For Task 14 (it owns the clipboard, keys and links): GPUI's Linux clipboard offers text only, so
    `copy_to_clipboard:html` leaves the clipboard unchanged (logged) and `mixed` writes only its plain text, until a
    text/html offer is added to the clipboard transport (a vendored `gpui-pre-linux` patch); the key and mouse paths
    must honor `Shared::is_readonly`; the link hover sets `Shared::set_hovered_url`; a mouse selection made copies
    through `Shared::copy_on_select`.

### Task 14: TerminalElement — keyboard routing, IME, mouse, selection, links, clipboard

**Files:**
- Create: `crates/agterm-gpui/src/terminal/{input,mouse,links}.rs`, `src/keymap_dispatch.rs`, `src/keymap_actions.rs`
- ➕ Create: `crates/agterm-gpui/src/terminal/{keyboard,keycodes,clipboard,input_tests}.rs`,
  `src/keymap_dispatch/tests.rs`, `crates/agterm-ghostty-vt/src/gesture.rs`, `crates/agterm-term/src/gesture.rs`,
  `crates/agterm-term/tests/pointer.rs`, `crates/agterm-gpui/tests/clipboard_html.rs`,
  `build/patches/gpui-pre-linux/0001-native-key-events.patch`, `vendor/gpui-pre-linux/`; Modify:
  `crates/agterm-gpui/src/{terminal,terminal/element,terminal/surface,terminal/tests,state,state/tests,app,lib}.rs`,
  `crates/agterm-gpui/Cargo.toml`, `crates/agterm-ghostty-vt/src/{lib,render,selection}.rs`,
  `crates/agterm-term/src/{lib,pane,input,render}.rs`, `Cargo.toml` (`regex`, `gpui-pre-linux` dep and
  `[patch.crates-io]`), `Cargo.lock`, `scripts/vendor-gpui.sh`

- [x] window-level keymap capture before the terminal (port of `keymap_dispatch.rs`/`keymap_actions.rs`), then ghostty
      binds, then `agterm-term` encoding; physical-key copy/paste/select-all
- [x] IME via GPUI's input handler (commit, preedit at the cursor, candidate position)
- [x] mouse: reporting modes, selection (drag, double/triple click, `cursor-click-to-move = false`), wheel scroll,
      mouse shape, hide-while-typing
- [x] OSC 8 and detected URLs: hover underline, open through the existing open-url path
- [x] clipboard: system and primary; OSC 52 through `ClipboardPromptPolicy` with a minimal confirm prompt (the full
      dialog comes with Task 21)
- [x] tests (headless): routing order (keymap capture wins), preedit lifecycle, drag and multi-click selection, link
      hit testing, OSC 52 allow/deny/remember
- [x] ➕ GPUI patch 9 (Technical Details > GPUI patches): the native key behind a key event on Linux (GPUI's
      `Keystroke` keeps no position, no layout character, and drops Shift from symbols)
- [x] gates — must pass before Task 15
- result (2026-10-15):
  - **Keys** (Codex-reviewed, transcript
    `~/.local/state/plan-exec-revmux/gpui-frontend/discussions/task-14-physical-keys/transcript.md`): one GPUI
    keystroke interceptor for the app (`keymap_dispatch::init`) runs agterm's order before GPUI's binding matcher and
    pending-keystroke buffering: the keymap router (the GTK `Router` port: custom commands, `map` alternatives, leaders
    and `--repeat` windows as deadlines checked at the next key, the menu chords, consumed keys' autorepeats and
    releases) in a window registered with `register_window(handle, KeyOrigin)`, then, when a terminal owner view holds
    the keyboard, ghostty's binds (`binds::Keyboard`, actions through `TerminalSurface::perform`, `all:` on every
    terminal, sequences queued and flushed) and the encoder; whatever it handles stops there (GPUI's input handler does
    not type it again; an `ignore`d key is the terminal's too). A text field (`Input` key context) or a prompt keeps
    every key. Releases come through the owner view's key-up listener (`keymap_dispatch::key_up`, the binds' consumed
    release, Kitty release events). Ghostty's app/window/tab/split binding actions stay unperformed, as libghostty's
    apprt callbacks left them in the GTK build.
  - **Native keys** (GPUI patch 9, `vendor/gpui-pre-linux` + `build/patches/gpui-pre-linux`, `scripts/vendor-gpui.sh`):
    the Wayland and X11 clients install a `NativeInput` for exactly one key dispatch (an RAII scope: a press, each
    Wayland autorepeat with the press's own record, the release; text-input-v3's one-character commit as `Text`), read
    with `gpui_linux::current_native_input()` and checked against the event's keystroke: XKB keycode (→ the physical
    key through Ghostty's keycode table, `terminal/keycodes.rs`), modifiers with Shift, consumed modifiers, the layout's
    level-0 character and whether it types ASCII (the keymap's `chordKey`), the text before Ctrl transformations,
    composition and repeat. Linux-only and additive (no GPUI type changes: gpui-kit/gpui-component and the other
    platforms build unchanged). Without a record (GPUI's test platform, a replayed keystroke, other platforms) the key
    is read off a US keyboard with Shift restored for its symbols; committed input-method text is a key with no
    position. ⚠️ Residual: the native path is unit-tested through its translation and compiled; a real key on a real
    seat is exercised at the checklist walk (Task 33) and by the window shell's own tests (Task 18).
  - **IME**: the owner view is GPUI's `EntityInputHandler` while focused: a mark is the terminal's preedit (drawn at
    the cursor, never sent), a commit is sent as text (Ghostty's `textCallback`: `PasteSource::Text`), the candidate
    window sits at the cursor's cell; a key while composing goes back to the platform.
  - **Mouse** (Ghostty's `Surface` callbacks): reporting (Shift keeps the mouse for selecting unless
    `mouse-shift-capture`), Ghostty's own selection gesture (`selection.h`, wrapped in `agterm-ghostty-vt`
    `SelectionGesture` and `Pane::gesture`: click counts by 500 ms and a cell's width, word/line/output clicks,
    Ctrl+Alt blocks, half-cell drags, 15 ms autoscroll past an edge), Shift-click extension, copy-on-select to the
    primary selection, middle-click primary paste, right-click word/link selection (no menu, as in the GTK build),
    `cursor-click-to-move = false` (no prompt click), the wheel (`Pane::wheel`, precise deltas per cell,
    `mouse-scroll-multiplier`, GPUI's ×3 undone), the pointer shape from OSC 22 (a hand over a live link), and
    `mouse-hide-while-typing` as GPUI's app-wide cursor hide mode. Positions are device pixels.
  - **Links**: OSC 8 (`Pane::hyperlink`, `Terminal::hyperlink_uri`) and Ghostty's scheme URL matcher over the
    soft-wrapped line (`Row::wrapped`), live with Ctrl or Super alone; hovered cells underlined across rows
    (`TerminalView::set_hovered_links`); a click without a drag emits `TerminalEvent::OpenUrl`, and
    `terminal::follow_link` is the default route through the link policy (`cx.open_url` / `cx.reveal_path`; the
    frontend's session-overlay route is Task 18's). A double click on a URL selects the URL.
  - **Clipboard**: OSC 52/Kitty reads and writes are answered in the pump under `clipboard-read`/`clipboard-write`
    (`ask` → the GTK build's `Prompts` port as a GPUI `Global`: one prompt per terminal and direction, a session choice
    remembered for every terminal); the minimal prompt is GPUI's window prompt with Allow, Deny, Always Allow This
    Session, Always Deny This Session. **Unsafe pastes are pasted as confirmed** (`TerminalEvent::UnsafePaste` is
    gone): upstream (`confirmReadClipboard`) and the GTK build complete libghostty's paste confirmation unasked.
  - Task 13's follow-ups (merged its review fixes): read-only mode (`Shared::is_readonly`) drops keys, sequence
    flushes, input-method text, mouse reports (the mouse selects instead), the middle-click paste and the wheel's
    program routes (the viewport still scrolls); terminal replies still go. The link hover sets
    `Shared::set_hovered_url` (`copy_url_to_clipboard`). `copy_to_clipboard:html` offers `text/html` alone and
    `:mixed` text plus `text/html`, through a second part of GPUI patch 9 (`gpui_linux::with_clipboard_html`, the
    Wayland data source's and the X11 selection owner's offers); a process test reads both selections' targets and
    data from another client on a private Xvfb (`tests/clipboard_html.rs`). ⚠️ Residual: the Wayland side is not
    exercised by a test (a Wayland client owns a selection only with an input serial, which a headless compositor
    never delivers).
  - Focus reports (mode 1004) on each change of view focus and window activation, the start state reported as no
    change. `Shared::{owner, set_owner}` carry a custom command's typing surface (`terminal::typing_surface`).
  - `keymap_actions`: Toggle Full Screen and the Delete Window confirmation on the registered window,
    `AppState::{keymap_fired, window_command}`, `FrontendWork::actions`; the window close, the session-close
    confirmation, the renames, Delete Workspace and Open Directory wait for the window shell, sidebar and dialogs
    (Tasks 18, 19, 21, 26) and are logged until then.
  - Tests: 14 in `terminal/input_tests.rs` (GPUI's test app, real panes under `/bin/sh`: key translation and chords,
    the routing order with the keymap first, the preedit lifecycle, drag/double/triple/Shift selection, mouse reports
    and Shift, the wheel, Ctrl-click links and the default route, focus reports, OSC 52 writes and reads under the
    prompt with a session choice, read-only mode), 8 router tests (`keymap_dispatch/tests.rs`), link and
    `keymap_actions` unit tests, `agterm-term` `tests/pointer.rs` (gesture, cell, OSC 8, soft wraps),
    `agterm-ghostty-vt` gesture tests, and the X11 `text/html` process test. Gates: fmt, clippy (workspace, all
    targets and features), lint-report, check-tracing-features, `vendor-gpui.sh --check`, `cargo test --workspace`
    under Xvfb (3746 passed, 0 failed); the new tests and the terminal module 20 times in a row, no failure; full GTK
    conformance: 653 passed, 14 skipped, ratchet holds (665 ids).
  - Review round 1 (fixes): the pointer's Ghostty keys are typed config now (`Mouse::{right_click_action,
    middle_click_action, click_repeat_interval}` and `Link::url` in core, ghostty's names, values and defaults at
    `GHOSTTY_REV`): right clicks follow `right-click-action` (agterm's settings write `paste`/`ignore`), middle clicks
    `middle-click-action` (reading the clipboard under `copy-on-select = clipboard`, as Ghostty does), clicks count
    within `click-repeat-interval`, URLs only with `link-url`; selections are copied by Task 13's
    `Shared::copy_on_select`. Links drop Shift while the program has the mouse (`mouseModsWithCapture`); OSC 8 runs
    stop at a different link; GPUI's synthetic lone-modifier keystroke goes on to the modifier listeners (a lit link
    goes dark) and a click refreshes the link with its own modifiers. `toggle_mouse_reporting` lasts (the config is
    applied when the pane starts or the config changes, not on each event). The release of a press the keymap took
    never reaches the program. `all:` key-table and `end_key_sequence` actions reach every terminal's keyboard. Native
    keys carry Caps Lock and Num Lock, and X11 autorepeats are repeats. The unsafe-paste decision was re-checked: the
    GTK build's `clipboard.rs` completes libghostty's paste confirmation unasked (`clipboard-paste-protection` on by
    default only raises that request), so no prompt here either. The X11 autorepeat flag has no end-to-end test.
  - Review follow-ups: `selection-word-chars` is typed (`Selection::word_chars`, decoded like Ghostty's
    `codepointIterator`, `\xNN` the character U+00NN) and reaches every word selection: the gesture's word clicks
    and drags (`SelectionGesture::apply_with`, `GHOSTTY_SELECTION_GESTURE_EVENT_OPT_WORD_BOUNDARY_CODEPOINTS`) and
    `Pane::select_word` (`Terminal::select_word_with`), through `Pane::set_word_boundaries`, U+0000 always a boundary.
    `focus-follows-mouse` is typed (`Mouse::focus_follows_mouse`): the owner view emits
    `TerminalEvent::FocusFollowsMouse` when the pointer moves over it while it does not hold the keyboard, its window
    is active and no button is held, once until it leaves or the focus changes. ➕ Task 18 follow-up: the deck
    subscribes to it and focuses that pane as a click would (`window.focus(&view.focus_handle(cx), cx)` on the pane's
    owner `TerminalView`, then the core's pane focus as the GTK build's focus-in path reports it). ⚠️
    `cursor-click-to-move = true` stays a residual: libghostty-vt at `LIBGHOSTTY_VT_REV` exposes no OSC 133
    prompt-click mode (`cl`/`click_events`) and no cursor pen content, so Ghostty's `promptClickMove` has nothing
    to read (`docs/backlog/cursor-click-to-move-needs-prompt-click-state.md`); agterm's default is `false`.
    Gates of round 1, on the tree merged with gpui-frontend
    (Tasks 13 and 15): `cargo test --workspace` under Xvfb 3781 passed, 0 failed; the terminal, router and pointer
    tests 20 times in a row, no failure; GTK conformance not needed (the core change only adds typed-model fields
    the GTK build never reads: it takes ghostty_config's defaults text and file list only).
  - Later tasks build on: `keymap_dispatch::{register_window, unregister_window, keyboard_left}` and a window-root
    `capture_key_up` calling `keymap_dispatch::key_up` (Task 18/24/26), `Shared::set_owner`, `TerminalEvent::OpenUrl`
    → the link-open route (Task 18), the full OSC 52 dialog replacing `terminal/clipboard.rs`'s prompt (Task 21).

### Task 15: Procedural sprites for box drawing, blocks, powerline, braille

Port from `qwertty-term-sprite` (MIT, attribution), adapted to GPUI paths/quads.

**Files:**
- Create: `crates/agterm-gpui/src/terminal/sprite/` (module per glyph family), `tests/sprite_golden.rs`,
  `tests/golden/`
- ➕ Create: `terminal/sprite.rs` (module root), `terminal/font/tests.rs`, `crates/agterm-gpui/build.rs`,
  `scripts/fetch-fonts.sh`, `build/patches/gpui-pre/{0002-font-metrics,0003-alpha-mask}.patch`; Modify:
  `terminal/{element,metrics,font,row_cache,tests}.rs`, `terminal.rs`, `app.rs`, `tests/external_texture/{main,
  cases}.rs`, `crates/agterm-term/src/pane.rs` + `tests/pane.rs`, `vendor/gpui-pre/src/{platform,window,
  text_system}.rs`, `scripts/{vendor-gpui.sh,lib/native.sh}`, `build/pins.env`, `Cargo.toml` (`tiny-skia`),
  `.github/workflows/rust-linux.yml`, `README.md`

- [x] box drawing U+2500–257F, blocks U+2580–259F, powerline U+E0B0–E0BF, braille U+2800–28FF, the legacy computing
      ranges Ghostty draws — to cell bounds without gaps; the element routes these code points away from the font
- [x] ➕ (Task 13) Ghostty's embedded default fonts, deterministically: `JetBrains Mono` (Ghostty's variable faces) as
      the default face when `font-family` is unset (a configured family keeps system selection, even one named
      JetBrains Mono), and `Symbols Nerd Font Mono` as a symbol fallback for every family; from the pinned Ghostty's
      package deps (`build.zig.zon`: `JetBrainsMono-2.304.tar.gz`, `NerdFontsSymbolsOnly-3.4.0.tar.gz`) with their
      licenses, fetched and verified like the other native inputs (`scripts/lib/native.sh`), registered through
      `TextSystem::add_fonts`; Task 13's `terminal/font.rs` default list stands until then (Codex-reviewed, transcript
      `~/.local/state/plan-exec-revmux/gpui-frontend/discussions/task-13-terminal-design/transcript.md`)
- [x] ➕ (Task 13) a public font-metrics accessor in the vendored GPUI patch series (Task 29's tree): `line_gap` and
      the underline position/thickness of the resolved face feed `terminal/metrics.rs` (`FaceMetrics`), closing Task
      13's line-gap residual; `box_thickness` (= the underline thickness) is what the sprites draw with. Strikethrough
      keeps Ghostty's estimate unless the face's strike metrics are exposed too
- [x] ➕ (Task 13) the lock cursor Ghostty draws at a password prompt (`Cursor::password_input`)
- [x] golden tests: each family at two cell sizes against goldens derived from upstream's
- [x] gates — must pass before Task 16
- result (2026-10-15):
  - **Sprites** (`terminal/sprite.rs` + `sprite/{canvas,common,box_drawing,block,braille,geometric,powerline,branch,
    legacy,legacy_supplement}.rs`, `octants.txt`): a port of `qwertty-term-sprite` (MIT, attribution in the headers)
    of Ghostty's sprite font, every code point Ghostty's sprite face draws at `LIBGHOSTTY_VT_REV` (a unit test holds
    the set to Ghostty's `draw*` function names: also geometric shapes U+25E2–25FF, branch U+F5D0–F60D, legacy
    computing U+1FB00–1FBEF and the supplement's subset), brought up to that revision (the outlined half circles
    U+E0B5/E0B7). Each family is a table, not a match per code point. ⚠️ Deviation from "adapted to GPUI
    paths/quads": a sprite is rasterized on the CPU into a coverage mask exactly as Ghostty's canvas does (rectangles
    and pixels straight into the mask, paths with `tiny-skia` 0.12 where Ghostty uses z2d, the quarter-cell padding,
    the trim) and painted from GPUI's sprite atlas through a new GPUI patch, `build/patches/gpui-pre/0003-alpha-mask
    .patch` (`Window::paint_alpha_mask`, `AtlasKey::AlphaMask`): pixel-for-pixel Ghostty geometry (quads at device
    pixels cannot express the coverage compositing, inversion and inner strokes), testable against Ghostty's goldens,
    and painted like a glyph. Masks are cached per thread by (code point, cells, `CellMetrics`) with unique atlas ids.
  - **Routing**: `row_cache::layout_row` takes a cell whose text is one sprite code point out of the text runs into
    `RowLayout::sprites` (`SpriteCell`), ending the run around it; the element paints sprites with the text, in the
    cell's foreground (faint, selection and search colors included), a wide character's at two cells, and repaints
    the one under a block cursor in the cursor-text color. U+E0C0 and the other stylized powerline glyphs stay with
    the font, as in Ghostty.
  - **Goldens** (`tests/sprite_golden.rs`, `tests/golden/`: Ghostty's own sheets at `LIBGHOSTTY_VT_REV` with its
    license, all four sizes 18x36+4, 12x24+3, 11x21+2, 9x17+1): box drawing lines, blocks, braille and octants match
    exactly; arcs and diagonals, geometric shapes, powerline, branch, legacy computing and the supplement within
    budgets of mean coverage difference (worst measured 0.67 %, arcs and diagonals at 9x17+1) and stray pixels (off by
    more than 96: none except branch hollow circles 0.24 %, the legacy hatch fills 0.03 % and the supplement's ellipses
    0.01 %, all at 9x17+1), anti-aliasing differences between tiny-skia and z2d. `sprites_are_deterministic`, and a
    real-render case in `tests/external_texture` (`alpha_masks_cover_whole_device_pixels`, lavapipe and Intel): two
    masks meet on whole device pixels without a gap and a hole stays a hole.
  - **Fonts** (`scripts/fetch-fonts.sh`, `FONTS_*` pins with each archive's sha256, whose Zig package hashes equal
    the ones the pinned `build.zig.zon` lists; `scripts/lib/native.sh` `fonts_prefix`; `agterm-gpui/build.rs` points
    `include_bytes!` at the prefix and fails with "run scripts/fetch-fonts.sh"; CI fetches it in every job that
    builds the workspace; README > Native dependencies names the licenses: JetBrains Mono OFL 1.1, Nerd Fonts MIT,
    kept in the prefix's `share/doc/fonts/`). `terminal::register_embedded_fonts` at app start: `JetBrains Mono`
    (`DEFAULT_FAMILIES` is now just it, Task 13's fallback list is gone) and `Symbols Nerd Font Mono`, the last
    fallback of every family. ⚠️ Deviations: the static Regular/Bold/Italic/BoldItalic faces of the same release
    instead of the variable ones (GPUI loads every face at its default instance, `get_font(id, Weight::NORMAL)`, so a
    variable bold would draw regular); the one-cell `Mono` variant of the symbols font (Ghostty embeds the wide one and
    scales it with Nerd Font constraints GPUI lacks); the embedded JetBrains Mono is registered only where no
    `JetBrains Mono` is installed, so a configured `font-family = JetBrains Mono` keeps the system's as the item asks,
    at the cost of an unset `font-family` also taking an installed JetBrains Mono (same family; determinism holds on
    every machine without one). GPUI accepts no face without an `m` as a primary font, so the symbols font is only
    ever a fallback.
  - **Metrics**: `build/patches/gpui-pre/0002-font-metrics.patch` (`TextSystem::font_metrics`, the platform's
    `FontMetrics` in font units); `metrics::measure` takes the line gap and the `post` underline (Ghostty's broken-table
    rule: a zero thickness is no thickness, a zero position with it no position) from it, so `box_thickness` is the
    face's underline thickness; strikethrough keeps Ghostty's estimate (GPUI reads no OS/2 strikeout). JetBrains Mono
    at 12 pt measures 10x21, baseline 5, underline at 18, box lines 1 px (2 px at 2x). `scripts/vendor-gpui.sh
    --diff` now regenerates the last patch of each crate against the earlier ones.
  - **Lock cursor**: libghostty-vt leaves the terminal's `password_input` flag to its embedder (no option sets it), so
    `agterm-term` gains `Pane::password_input` — Ghostty termio's heuristic, canonical input without echo, read with
    `tcgetattr` on the PTY master (Ghostty polls it every 200 ms; here at every paint: a prompt turns echo off before it
    prints, so its output repaints with the lock) — and the element ORs it into the frame's flag. At a password prompt
    the cursor is U+F023 (`nf-fa-lock`, from the symbols fallback) in the cursor color, centered in the cell, whatever
    the style, visibility, blink or focus (Ghostty's `cursor.style` order). Behind zmx the PTY is the zmx client's (raw,
    no echo), so a live session shows no lock, as upstream's libghostty surface over zmx does not either.
  - Tests: `terminal/sprite/tests.rs` (7: Ghostty's code point set, disjoint ranges, routing of a cell's text, every
    sprite covers pixels at three sizes, line placement, wide cells, the cache), `terminal/font/tests.rs` (4, GPUI's
    Linux text system with only the embedded fonts), `terminal/tests.rs` (+2: sprite cells leave the runs in a real
    pane's view; the lock whatever the cursor), `tests/sprite_golden.rs` (2), `agterm-term/tests/pane.rs` (+2:
    `password_input` at `stty -echo; read`, not in raw mode), the external texture harness's mask case.

### Task 16: Kitty graphics and background image

Port the placement rendering from qwertty-term (MIT, attribution).

**Files:**
- Create: `crates/agterm-gpui/src/terminal/{kitty,background}.rs`, `terminal/kitty_tests.rs`
- ➕ Create: `terminal/background/tests.rs`, `terminal/kitty/{textures,placeholder}.rs`,
  `crates/agterm-term/src/placeholder.rs`, `docs/backlog/kitty-virtual-placement-c-api-gaps.md`; Modify:
  `terminal/{element.rs,terminal.rs}` (the module root), `tests/external_texture/cases.rs`,
  `crates/agterm-gpui/Cargo.toml`, `Cargo.toml` (`image`), `crates/agterm-term/src/{render,kitty,lib}.rs`,
  `crates/agterm-ghostty-vt/src/kitty.rs`

- [x] image data from `agterm-term`'s kitty view → GPUI images cached by id/generation; placements by z-layer, source
      rects, scaling, scrolling with the viewport, deletion
- [x] `background-image*` (agterm's watermark lines, `background.rs` + core `watermark`) drawn behind the grid
- [x] tests: placement geometry, layer ordering, cache eviction on delete, background image fit/position modes
- [x] gates — must pass before Task 17 (the `show-image.sh` end-to-end check is in Task 23)
- result (2026-10-15):
  - **Kitty** (`terminal/kitty.rs`, from Ghostty's `image.zig`/`image.v.glsl` by way of qwertty-term's renderer, MIT,
    attribution in the header): a placement lands at its viewport cell times the cell plus its offset inside the cell
    (clamped to the cell, Ghostty's `cellOffset`), at the pixel size and source rectangle libghostty-vt resolved; a
    negative row (scrolled partly off the top) is clipped by the element; a viewer at its own font size scales the
    terminal's pixels by its cell over the owner's (`Cells { engine, view }`). Sorted by `z`, then image id
    (stable), drawn in Ghostty's three layers: below the cell backgrounds (after the default background and the
    background image), between the cell backgrounds and the underlines/text, above the text (the cursor and the
    preedit stay on top, as Technical Details > Frame orders them; Ghostty draws its cursor with the text, so an
    above-text image covers it there). No GPUI patch: GPUI's own `paint_image` draws a source rectangle when the
    whole image is painted scaled so the rectangle covers the placement, clipped to it (`kitty::image_bounds`; GPUI
    then samples only that part of the tile) — checked on real renderers by the external texture harness's
    `image_source_rects_fill_their_placement` (lavapipe and Intel). Images become BGRA straight-alpha `RenderImage`s
    once per `(id, generation)` (RGB opaque, gray to gray RGB, Ghostty's `convertReplace`), scaled down past 8192 px
    a side (wgpu's default texture limit; Ghostty allows 10000); per view, because the sprite atlas is per window.
    `ViewImages` keeps what a view painted into its window's atlas and `drop_image`s what it stops drawing (deleted,
    new generation, scrolled out, background changed), and everything on teardown and on the view's release
    (`on_release_in`, registered at its first paint). The view compares against its own cache, not
    `KittyView::removed`, so skipped frames cannot leak a texture.
  - **Background image** (`terminal/background.rs`, Ghostty's `prepBackgroundImage`/`bg_image.{v,f}.glsl`):
    `TerminalConfig::background_image` from the config (so the watermark overlay lines `watermark::overlay_text`
    writes reach it: tested end to end through `load_app_config`), drawn over the whole surface after the default
    background: `contain`/`cover`/`stretch`/`none`, the nine positions (`center` = `center-center`), `repeat` tiling
    from the offset both ways (a repeated image under 256 px a side is repeated into one larger texture first; at most
    4096 tiles). Opacity: Ghostty's `min(opacity, 1 / bg alpha) * bg alpha`, baked into the texture's alpha (no GPUI
    patch for an image opacity); exact at `background-opacity = 1`, which the watermark overlay sets. PNG or JPEG by
    content, then extension (the `image` crate, `png`+`jpeg` features only, the version GPUI links), decoded on the
    background executor; the old image stays up while its replacement loads and when a new file fails, as in Ghostty.
    Loads are keyed by path, mtime, length, baked alpha and repeat, shared (loaded or in flight) by every terminal on
    the main thread, and the file is looked at again on each `Terminal::set_config`, never per frame: a text watermark
    re-rendered into the same file reloads without the GTK build's two-overlay trick.
  - **Review round 1** (supersedes the above where they differ): (1) Kitty textures are converted on the background
    executor (`kitty/textures.rs` `KittyStore` in `Shared`, shared by the terminal's views; a placement draws nothing
    until its texture arrives, then the pump redraws), bounded to 8192 px a side and 16 Mpx (64 MB of BGRA) and
    sampled straight from the stored pixels into one buffer of that size — no `image::imageops::resize` and its f32
    intermediate (the background image's downscale uses the same sampler); the store follows only newer frames and
    drops results for images it no longer follows. (2) Unicode placeholders (virtual placements, `U=1`) are drawn, as
    the GTK build's libghostty draws them: `agterm-term` keeps each U+10EEEE cell's ids from its raw style colors (a
    palette index or 24-bit color, which the frame's resolved colors lose) and its row/column/high-byte diacritics
    (`Row::placeholders`, `KittyPlaceholder`) and joins them into runs as Ghostty's `PlacementIterator` does
    (`virtual_placements`, Kitty's diacritics table); `agterm-ghostty-vt`/`agterm-term` expose a placement's
    `columns`/`rows`; the element ports `renderPlacement` and `placeholderTarget` (`kitty/placeholder.rs`) and draws
    each run at `z = -1`, below the text. (3) Under a translucent `background-opacity` the background texture is
    Ghostty's finished blend (the background color baked in, every texel at the background alpha) and the
    background color is painted only around the image (snapped to device pixels), so the surface is exactly as
    translucent as Ghostty's; over an opaque background GPUI's own blend already equals it. (4) A config change to a
    file that cannot be looked at drops the association with a load still running for an earlier spec, so that load
    cannot replace the image on screen. (5) The element records the layers it paints (`PaintLayer`,
    `TerminalView::painted_layers`), and a test over a real pane checks each Kitty layer's stage.
  - **Review round 2** (supersedes the above where they differ): (1) Ghostty's translucent blend with an image
    opacity over 1: `k = max(1, a * opacity)`, color `(img * a * opacity + bg * max(0, 1 - a * opacity)) / k`,
    alpha `background alpha * k` (Ghostty's 0.5 x 1.5 = 0.75), where round 1 pinned every texel to the background
    alpha. (2) Placeholder cells shape no glyph (Ghostty shapes a space), so nothing is drawn over their image; their
    backgrounds stay. (3) The background loads in two steps (`terminal/background/load.rs`): the file decoded once
    into unbaked pixels (keyed by the file), then baked off the main thread per opacity and translucent background
    color, so an OSC 11 color change re-bakes without reading the file, at most one bake in flight per terminal (a
    newer one waits); paint pairs the background color with the texture's own bake, so no seam shows while a bake
    runs. (4) A config change forgets the decode and bake in flight at once (not at the next paint), and a file that
    cannot be looked at forgets them too; both tested through `Shared::background_image`. (5) The element paints from
    one table, `PAINT_ORDER`, each stage dispatched and recorded by the same call, Kitty stages with the placements
    `paint_image` actually drew. (6) `agterm-term` copies a new Kitty image's pixels on the pane's IO thread after the
    batch that completed it (`kitty::Prefetch`, kept with the terminal); the paint's capture shares that copy instead of
    copying under the VT lock on the main thread. Every new test was run against its fix reverted and fails there.
  - ⚠️ Residuals: two parity gaps libghostty-vt's C API leaves (it reports a placement id without its namespace, and
    gives relative placements rooted at a virtual placement no position, root or offsets): ids resolve by number, and
    such relative placements are not drawn — `docs/backlog/kitty-virtual-placement-c-api-gaps.md`, a Task 34
    precondition (Codex-reviewed: an APC pre-scan cannot recover the committed state; upstream API or a local patch;
    transcript `~/.local/state/plan-exec-revmux/gpui-frontend/discussions/fixer-task-16-kitty-c-api-gaps/transcript.md`).
    Kitty animation frames are not drawn (not in the view).
  - Tests: `terminal/kitty_tests.rs` (11: cell + offset and its clamp, a negative row, a viewer's scale, what is not
    drawn, z/id order and the three layers, the source-rect bounds, BGRA conversion per format, the atlas limit, the
    cache converting once and evicting; over a real pane in GPUI's test app, a chunked `a=T` image drawn from the
    window's atlas and dropped from it on `a=d`, and on teardown), `terminal/background/tests.rs` (10: fit, position,
    tiling, the tile bound, small-image repetition, the capped and baked opacity, decoding by content and refusing
    other types, the watermark overlay lines into `TerminalConfig`; in GPUI's test app, one load shared by two
    terminals, a re-rendered file reloaded on a config change, the image dropped from the atlas with the config, a
    missing file), and the harness case above. Round 1 adds: Ghostty's placeholder tests (`agterm-term`
    `placeholder.rs`: decode by palette index, 24-bit color and high byte, continuation, breaks, runs per row; a real
    terminal's frame in `render.rs`), its `renderPlacement` tests (dog 4x2, 2x2 with blank cells, 1x1) and target
    lookup, the texture budget and sampler, the store's async lifecycle, placeholder cells drawing two images (RGB
    and palette ids) over a real pane, the layer stages, Ghostty's translucent blend and the bands around the image,
    the stale-load case, and the vt wrapper's virtual placement `columns`/`rows`.

### Task 17: Conformance harness for the GPUI build

**Files:**
- Modify: `scripts/stage.sh` (`--frontend gtk|gpui`), `conformance/tools/run-rust-linux.sh` (frontend switch,
  ratchet per frontend), `conformance/agterm_conformance/impls/base.py` (lavapipe `VK_DRIVER_FILES`, weston
  `--fake-seat` for GPUI)
- Modify: `conformance/tools/ghostty_probe.py` (GPUI first-frame line)
- Create: `conformance/rust-gpui-passing.txt`

- [x] staged GPUI build runs under the harness's weston with lavapipe; isolation unchanged
- [x] ➕ (Task 1) the harness's weston gets `--fake-seat` for GPUI runs (GPUI panics without `wl_seat`); check the GTK
      ratchet stays green with it, or pass it for the GPUI frontend only
- [x] ➕ (Task 1) provide lavapipe: it is not installed on the user's machine (the spike unpacked the signed
      `vulkan-swrast` package into the vendor dir); pick a documented source per environment (distro package on CI,
      a verified unpack into `${AGTERM_VENDOR}` otherwise) and point `VK_DRIVER_FILES` at it; a missing lavapipe is a
      clear harness error, never a fallback to a hardware driver
- [x] first full run on GPUI; seed `rust-gpui-passing.txt` with `check_ratchet.py --add`
- [x] harness self-tests in `conformance/selftests` (`test_run_rust_linux`, `test_ghostty_probe`, `test_impls`) and
      `test_docs.py` (capability table in rust-build.md vs `CAPABILITIES` in `base.py`) updated and green; both ratchets
      green
- [x] gates — must pass before Task 18
- ➕ also touched: `scripts/fetch-lavapipe.sh`, `scripts/lib/native.sh`, `build/pins.env` (`LAVAPIPE_*`),
  `conformance/agterm_conformance/{plugin.py,impls/rust.py,impls/__init__.py}`, `conformance/selftests/
  {test_fetch_lavapipe,test_plugin_options}.py`, `scripts/install-apparmor-profile.sh` (GPUI default path),
  `.github/workflows/rust-linux.yml` (`conformance-gpui` job), README.md, conformance/README.md, rust-build.md
- result (2026-10-15):
  - **Frontend switch**: `stage.sh --frontend gpui` stages `bin/agterm-gpui`, `agtermctl`, `zmx` and the same `share/`
    (no `lib/`: libghostty-vt is static, GPUI loads neither libghostty nor gtk4-layer-shell) into
    `target/conformance-stage-gpui`; `run-rust-linux.sh --frontend gpui` builds the same workspace, stages it, runs
    pytest with the new plugin option `--rust-frontend gpui` and holds `rust-gpui-passing.txt`, reporting into
    `conformance/reports/rust-gpui-linux/`. Separate stage dir (not one stage holding both binaries), so the GTK probes'
    default `target/conformance-stage/bin/agterm` never vanishes under a GPUI run; the AppArmor installer's GPUI default
    follows it.
  - **Seat and lavapipe for GPUI only**: `RustAdapter(frontend=gpui)` appends `--fake-seat` to its weston and makes
    lavapipe the instance's only Vulkan driver (`VK_DRIVER_FILES` + `VK_ICD_FILENAMES`), `LIBGL_ALWAYS_SOFTWARE=1`
    (wgpu's GL backend, as Task 12 found), Mesa's EGL vendor, and drops inherited `VK_*`, `WGPU_*`, `__NV_*`,
    `__GLX_*`, `DRI_PRIME`, `MESA_LOADER_*`, `MESA_VK_DEVICE_SELECT`; it refuses a launch (before anything spawns)
    without `CONFORMANCE_LAVAPIPE_ICD` or with a manifest that does not load `libvulkan_lvp`. The GTK frontend's
    weston and environment are untouched, so its ratchet carries no new risk. A launched GPUI instance mapped only
    `libvulkan_lvp.so` + Mesa's EGL/gallium (no NVIDIA library), checked through `/proc/<pid>/maps`.
  - **lavapipe source**: `scripts/fetch-lavapipe.sh` — `AGTERM_LAVAPIPE_ICD`, else the distribution's
    (`lvp_icd.<arch>.json`/`lvp_icd.json`; CI: Ubuntu's `mesa-vulkan-drivers`), else the pinned Arch package
    `vulkan-swrast 1:26.2.4-1` (`LAVAPIPE_*` pins, sha256; its pacman signature checked good when pinned, Arch's mesa
    packager's key) unpacked into `${AGTERM_VENDOR}/lavapipe-<key>/` with an absolute-path ICD manifest at the prefix
    top (where Task 12's `tests/support` already looks); glibc 2.38 checked before download, every library checked by
    `ldd` after unpack and on each later use (a host whose libLLVM moved on gets an error naming the library, never a
    fallback). `--print-icd` resolves without fetching. The runner resolves it before taking the lock (exit 2 without
    one) and hands it over as `CONFORMANCE_LAVAPIPE_ICD` (not `AGTERM_`-prefixed: the plugin scrubs those).
  - **ghostty_probe.py**: `--frontend gpui` (default bin from the GPUI stage, lavapipe from the env or
    `fetch-lavapipe.sh --print-icd`); Task 13's lines (`terminal surface created` / `terminal drew its first frame` /
    `could not create the terminal surface` under target `agterm::surface`) are read as the GTK build's, with tests.
  - **CI**: a `conformance-gpui` job (Ubuntu, `mesa-vulkan-drivers`) runs `run-rust-linux.sh --frontend gpui`.
  - **Runs**: self-tests 763 pass; GTK full run 653 pass, 0 fail, 14 skip, ratchet holds (665 ids); GPUI first full
    run 339 pass, 292 fail (262 failed + 30 setup errors waiting for a pane mount: no terminal before Task 13), 36 skip
    (unsupported), 47 min, no instance root or process left; 339 ids seeded. A second full GPUI run against the seeded list gave the same 339/292/36 (47 min), ratchet holds.
    Later tasks grow it with `check_ratchet.py --report conformance/reports/rust-gpui-linux/nodeids.jsonl --list
    conformance/rust-gpui-passing.txt --add` after their GPUI run.
  - **Ratchet-only mode** (2026-10-15): `run-rust-linux.sh --ratchet-only [--also SELECTOR]... [--also-k EXPR]...`
    runs a frontend's ratchet ids plus a task's extras and is judged like a full run (every listed id must pass;
    `--add` adds only ids that ran and passed; a `partial` report neither breaks nor removes the ids it left out).
    A full run is still required for the GPUI switch (Task 34) and for harness or broad shared-crate changes. Self-tests
    803 pass. On the tree with Tasks 13–16 merged: GPUI ratchet-only 339/339 pass, suite time 27 s (vs 47 min for a full run),
    638 s wall time including a fresh release build. A ratchet-only run with the terminal tests
    (`--also` test_session_text, test_surface, test_font_theme, test_events; `--also-k "clipboard or terminfo or font
    or type_text or quick_type or title or selectall or paste or cwd or notify"`) gave 339 pass, 67 fail, 17 skip,
    620 s, and no new ids: 58 of the failures wait for a mounted `left` pane, which the GPUI window gets with Task 18;
    7 are `unknown theme` (no bundled themes in the GPUI stage). Growing the ratchet after Tasks 13–16 waits for the
    first full GPUI run after Task 18. That run also catches unrelated ids that started passing as a side effect.
  - Deferred: the CLAUDE.md note on the GPUI conformance run (Task 36); the GPUI ghostty-probe CI step comes with
    Task 13's terminal; HTML/AppArmor for the GPUI stage with Task 32.
  - ⚠️ review round 1: `--fake-seat` is new in weston 15.0; Ubuntu 24.04's weston 13 exits on it (and has no seat
    without it), so CI could launch no GPUI instance, nor could Task 12's process tests. `scripts/build-weston.sh`
    builds the pinned weston 15.0.1 (`WESTON_*`; headless + GL + desktop shell only; wayland-protocols 1.49 and
    libdisplay-info 0.3.0 as pinned subprojects, no download during the build) into the vendored prefix, checked in
    an `ubuntu:24.04` container (rootless podman: builds, takes `--fake-seat`, the harness's real-weston GPUI test
    passes with it). CI puts its `bin/` first on PATH in `rust-linux` and `conformance-gpui`; `run-rust-linux.sh
    --frontend gpui` takes it when the weston on PATH lacks the option, the adapter refuses such a weston naming the
    script, and `tests/support` uses the first weston on PATH that takes it. Also: a lavapipe manifest whose driver is
    missing is refused (harness) or skipped (script, system manifests); `tests/support/lavapipe.rs` asks
    `fetch-lavapipe.sh --print-icd`, so the process tests and the external-texture test share the harness's rule;
    `ghostty_probe.py` finds lavapipe before the harness import scrubs `AGTERM_*` and reports the script's reason.

### Task 18: Deck, panes, splits, pane lead, mute

**Files:**
- Create: `crates/agterm-gpui/src/{window,deck,pane,split,session_host,pane_lead,mute}.rs`
- ➕ Create: `crates/agterm-gpui/src/{windows,watermark}.rs`, `src/{deck,pane,split,mute,window,watermark}/tests.rs`,
  `crates/agterm-term/src/pwd.rs`; Modify: `crates/agterm-gpui/src/{app,lib,state,state/tests,terminal,
  terminal/element,terminal/surface}.rs`, `crates/agterm-gpui/Cargo.toml`, `crates/agterm-term/src/lib.rs`,
  `Cargo.toml` (`cosmic-text`), `Cargo.lock`

- [x] window shell (port of `window.rs`): window composition, frontmost tracking and `window.list` reporting, the
      modal layers overlays attach to
- [x] deck of sessions with panes and splits, wired to `SessionHost`/`ProcessLauncher`/process table like GTK
- [x] lead cover/takeover (`pane_lead.rs`), inactive-pane and backdrop wash (`mute.rs`)
- [x] ➕ (Task 16) session and pane backgrounds on the terminals (port of GTK `background.rs` `sync_page`): each
      pane's effective `BackgroundWatermark` through `watermark::overlay_text` into its terminal's config overlay
      (`load_app_config(.., Some(overlay))` → `Terminal::set_config`; the terminal draws `background-image*` since Task
      16 and re-reads a file re-rendered at the same path on every `set_config`), a color watermark's `background`, and
      the text watermark rasterized to its PNG under `<state dir>/watermarks` in place of GTK's pango (GPUI exposes no
      offscreen text rasterizer: `cosmic-text`, which GPUI's Linux text system already links, or GPUI's headless
      `render_to_image`); stale text files removed as GTK does
- [x] tests (headless): split layout math, focus moves, lead takeover, wash on focus change
- [x] gates + conformance (both ratchets) — must pass before Task 19
- result (2026-10-15, first part; Tasks 14, 16 and 17 not merged yet):
  - **Shape**: the windows follow the model after every main-loop task: `app.rs` `land` runs `Windows::sync` inside
    the `AppState` update (the GTK build's settle-time windows sync; `settle` stays context-free), which reloads the
    config when asked, puts replacement panes in, opens a window per newly open store and closes the gone ones
    (`windows.rs`, the GTK `windows.rs` registry pulled forward from Task 26: the shell needs a window per store),
    syncs each window, applies the window commands' requests, reports each window's state for `window.list`, lands
    the pane focus requests and hands the granted panes their seeds (`AppState::take_spawns`, the GTK
    `spawn_paced_panes`); no open window quits. `window.rs` `WindowView`: the deck under the places of the title bar
    and the sidebar, the modal layers (`Layer`: search, ask, picker, palette, switcher, back to front) later tasks
    attach views to, the title, frontmost/key-window tracking through `observe_window_activation`
    (`AppState::set_key_window`, `key_window_lost`), the close request, full screen/maximized reported once a frame in
    that state is drawn (`Painted`, `on_next_frame`), minimized never read (GPUI has no read-back), raise/resize/zoom/
    full screen/minimize requests; a new window is focused but not activated (the window manager's, as the GTK build).
  - **Deck** (`deck.rs`, `pane.rs`, `split.rs`): every session's page mounted at once; a hidden page and a pane the
    layout does not show are laid out at the page size but not drawn (`TerminalView::set_shown`: no frame capture, no
    redraw for its output, the wrapper `invisible`), so their shells start and follow the size. Session panes are
    gated (`Terminal::gated`, `Shared::launch_with`): the first layout asks the core (`pane_spawn_ready`), the seed
    arrives through the sync; the scratch and replacements start from their spec. `PageElement` lays the children out
    at the GTK `PaneStack`'s rectangles (`Place`), reports the page geometry (`report_hud_frames`), and follows a
    divider drag (window mouse listeners while it runs; a double click on an unmoved divider resets). Focus: the
    deck focuses the model's pane (`window.focus`), reports given focus, and the user's focus comes back through
    `on_focus`. Terminal events are routed as the GTK build's `connect_*`: title, pwd (OSC 7 decoded as Ghostty's
    `reportPwd`, `agterm-term` `pwd.rs`), search reports, font size, notifications (focus sampled), links
    (`AppState::follow_link`: the session-overlay route, else the desktop through `FrontendWork`), exits (held ones
    close on the next key, a held command reports `remote_pane_held`). Panes launch through `agterm-term`'s
    `Pane::launch` under a `Host` from Ghostty's resources (`session_host.rs`): the config's `command` in place of the
    login shell when set (libghostty's rule, which the harness's `command = /bin/sh` relies on), live sessions installed
    over the staged zmx (`session_host::install`, the GTK `zmx_runtime::install`), `live-sessions`/`live-any-shell`
    claimed by host. The config (theme, settings lines written first) loads at launch on the light side (Task 20 brings
    the desktop's), and `config.reload` rebuilds it into every terminal with its diagnostic count.
  - **Lead and mute**: `pane_lead.rs` (upstream's cover: terminal colors, the drawn `rectangle.on.rectangle.slash`,
    title, hint, the yellow `ssh:` note; element ids `pane-lead-cover-left|right`, `remote-reconnect-note-*`), a
    covered or reconnect-waiting pane holds its keys (`Shared::hold_keys`/`held_key`: a press asks the core to take
    the lead); `mute.rs` (GTK's `muted`/`backdrop`, the backdrop's regions flattened into disjoint rectangles because
    GPUI fades each primitive, not a group).
  - **Watermarks** (`watermark.rs`, the Task 16 ➕ item): each pane's effective background into its terminal's config
    overlay (`DeckEnv::config_for`: `load_app_config(.., Some(overlay))`), text rendered bold sans-serif at 256 px with
    `cosmic-text` 0.19 (GPUI's own) into a PNG, stale files removed; applied on change only, kept over a config reload.
  - Tests: 71 in `deck/`, `window/`, `split/`, `mute/`, `pane/`, `watermark/` tests and the module tests (the GTK
    plan, geometry, mute and spec tests ported; in GPUI's test window over real panes: every session mounted and
    started with only the selected one drawn, the split at its ratio and focus following `session.focus`, the wash
    following the focus, a covered live pane taken over by a key press into a fresh terminal, a held exit closed by a
    key, backgrounds reaching the terminals), `agterm-term` `pwd` tests. Gates: fmt, clippy (workspace, all targets
    and features), lint-report, check-tracing-features, `vendor-gpui.sh --check`, `vendor-calloop.sh --check`,
    `cargo test --workspace` under Xvfb (3810 passed, 0 failed), the new tests 10 times in a row. A smoke run of the
    real binary on a private headless weston with lavapipe: sessions, splits, typing and read-back, shells exiting,
    window commands, SIGTERM.
  - Remaining before the gates box: wire Task 14 (window registration with `keymap_dispatch`, `capture_key_up`,
    `Shared::set_owner`, the key holds and the status keystroke inside its interceptor path), Task 16 (a test of
    `background_image` from the overlays), Task 17 (the GPUI conformance run, the ratchet, the capability claims).
    The full GTK run is not needed: the GTK binary links none of the changed crates.
- result (2026-10-15, second part, on the tree merged with gpui-frontend at Tasks 13–17):
  - Review round 1 (fixes): the focus the deck gives is reported to the core inside its reconcile
    (`Deck::reconcile` returns whether the core needs a settle; `WindowView::sync` and `Windows::sync` pass it on to
    `land`'s resettle), no longer as a queued task a later `session.focus` could be undone by. A user focus
    (`on_focus`) is still posted, counted by `FocusMoves`: a report a later focus move overtook is dropped when it
    runs, and while the user's last move awaits its report the deck keeps it over the model's older focus (a sync in
    between does not take the click back). The deck tests now run in an active test window, so the focus listeners
    fire as on a desktop; `queued_focus_moves_settle_on_the_last_one` and
    `a_users_focus_move_outlasts_a_sync_before_its_report` fail without the fix. Docs corrected
    (`Report::Notification`, `watermark.rs`'s `DeckEnv::config_for`, `watermark::change`), the unused
    `Windows::activate` removed, OSC 7 schemes compared case-sensitively as Ghostty's `reportPwd`.
  - Task 14 wired: each `WindowView` registers with `keymap_dispatch` as `KeyOrigin::Window` (unregistered on close);
    the window root's `capture_key_up` hands releases to `keymap_dispatch::key_up` and stops a keymap press's release
    there; a deactivation checks after 150 ms (the GTK `LEAVE_CHECK_DELAY`) whether an agterm window holds the
    keyboard (`keymap_dispatch::holds_keyboard`), else `keyboard_left`. Pane terminals carry `SurfaceOwner::Pane`. The
    terminal key path (`TerminalView::key_down`, which the interceptor calls) emits `TerminalEvent::Keystroke` with the
    press classified for the status glyph (`input::status_keystroke`, the GTK `status_keystroke` over the physical key,
    the text before Ctrl and the held modifiers), then runs `Shared::held_key` before ghostty's binds; the deck's
    wrapper hold is gone. The deck posts the keystroke as `AppState::pane_keystroke` (user activity, auto-follow,
    `clear_status_on_keystroke`). `TerminalEvent::FocusFollowsMouse` focuses the pane as a click would.
    `TerminalEvent::OpenUrl` already went through `AppState::follow_link` (no second open on the default route).
  - Task 16: the background test asserts `background_image` for a text background (its rendered file) and an image
    one.
  - Tests: + 2 focus regression tests, + owner and status keystroke, + focus-follows-mouse (deck), the window's key
    monitor registration (window), the status classification (input). Gates: fmt, clippy (workspace, all targets and
    features), lint-report, check-tracing-features, `vendor-gpui.sh --check`; `cargo test --workspace` under Xvfb
    3911 passed, 0 failed, 6 ignored; the deck, window, input, router and pwd tests 5 times in a row, no failure.
  - Conformance (GPUI, ratchet-only, per the run budget): `--also` test_window, test_session, test_split,
    test_addressing, test_tree, test_session_text, test_surface, test_font_theme, test_events and `--also-k
    "clipboard or terminfo or font or type_text or quick_type or title or selectall or paste or cwd or notify or focus
    or zoom or watermark or background or lead or pane or split or resize or reload"`: 467 pass, 22 fail, 6 skip,
    suite 415 s (996 s wall with the release build). The run claimed posix-shell and config-reload-counts as well:
    under posix-shell `tests/cli/test_cli_output.py::test_quick_terminal` types into the quick terminal, which does
    not exist yet, and broke the ratchet; config-reload-counts' only test fails on the font size a reload should
    apply to open terminals. Both claims dropped; the 16 passing ids that need posix-shell (session text and cursor
    round-trips) were left out of the add. Ratchet: 339 → 452 ids (+113). Claimed: drop-placement,
    select-takes-frontmost, status-shape, type-rejects-nul, resize-echoes-ratio, hidden-split-holds-focus.
    Remaining failures: themes (no bundled themes in the GPUI stage), HUD and overlays (later tasks), the quick
    terminal, the zoom under a running overlay, and `config.reload` not applying a changed `font-size` to open
    terminals (`Terminal::set_config` keeps the live points; Ghostty moves an unzoomed surface to the new size).
  - Later tasks build on: `Deck::reconcile`'s synchronous focus report, `FocusMoves`, `AppState::pane_keystroke`,
    `keymap_dispatch::holds_keyboard`, the window root's key-up, the posix-shell claim once the quick terminal
    lands (with its 16 ids), and the font-size-on-reload gap.

### Stage 3 — UI groups

### Task 19: Sidebar

**Files:**
- Create: `crates/agterm-gpui/src/sidebar/`, `src/{status_glyph,row_icon}.rs`
- ➕ Create: `crates/agterm-gpui/src/sidebar.rs` (module root), `sidebar/{rows,context,drag,menu,look,view,tests}.rs`,
  `src/row_icon/{ink,tests}.rs`, `src/status_glyph/tests.rs`; Modify: `crates/agterm-gpui/src/{lib,window,deck,state,
  window/tests}.rs`, `crates/agterm-gpui/Cargo.toml`, `Cargo.toml` (`ashpd`, `futures-util`), `Cargo.lock`

- [x] workspaces, sessions, status glyphs, row icons, drag reorder, collapse per upstream `agterm/Views` and the
      checklist; gpui-kit list components
- [x] tests (headless): selection, reorder, collapse, glyph states
- [x] gates + conformance — must pass before Task 20
- result (2026-10-15, Task 17 not merged yet: the GPUI conformance run and ratchet wait for it):
  - **Shape**: `sidebar.rs` `Sidebar`, a GPUI entity per window mounted in `WindowView`'s row before the deck (the
    window's only change: the entity, its `sync` after the deck's, `WindowView::sidebar()`; `Deck::env()` hands it the
    terminals' config). As in the GTK build the handlers never touch the model: `sync` (from the window's sync after
    every main-loop task) takes the rows (`rows.rs`, ported unchanged), the handlers' snapshot (`context.rs`) and the
    look, and every gesture posts a `SidebarAction`. The GTK pure modules (`rows`, `context`, `drag` math, the `menu`
    model) are ported with their tests; `view.rs` draws: rows in a scroll container tracked by a `ScrollHandle` (its
    child bounds are the drop math's row spans; it scrolls a newly selected row into view), the selection pill
    (upstream's 8/1.5 inset, radius 7, the theme's selection colors, dimmed outside the key window, else the white
    wash), the disclosure, the leading glyph, the name (tooltip only while truncated, measured with the text system),
    the rename field, the hover "+", the status glyph and the red unseen badge (upstream's `BadgeView` metrics), the
    drop line/frame, the empty flagged hint, the footer (gpui-kit ghost buttons that never take the keyboard, the
    add-session `DropdownMenu` opening upward) and the 1-point divider with a 12-point grab handle `deferred` over the
    terminal's edge. Element ids are upstream's accessibility ids (`session-row`, `workspace-row`, `session-icon`,
    `workspace-icon`, `edit-field`, `workspace-add-session`, `notify-badge`, `agent-status`, `add-session`,
    `focus-filter-toggle`, `flagged-view-toggle`). Colors: `agterm_core::chrome::ChromeColors` from the terminals'
    config (`sidebar/look.rs`, the theme's background, foreground and selection colors and the Settings tint), the
    rows' text size from `sidebarFontSize` (9...20, upstream's 13 points by default).
  - **Behavior** (GTK parity): a click selects; Ctrl toggles and Shift extends from the anchor (GTK's multiple
    selection; the clicked row, else the still-selected active session, else the last becomes active); a right-click
    outside the selection narrows it; row menus are gpui-kit `context_menu`s built from the ported model (submenu Move
    to, disabled Reveal for a missing directory, Copy Name to the clipboard, Reveal through GPUI's `reveal_path`); a
    workspace-row click toggles after 400 ms (GTK's double-click time) unless a double-click renames, re-reading
    `workspaceRowClickExpands` when it fires; the disclosure toggles at once; rename is a gpui-kit `Input` (Enter or
    losing the keyboard commits, Escape cancels, a reshape of the list cancels; auto-follow held meanwhile); drags of
    rows (`on_drag`/`on_drag_move`/`on_drop`, a session drag carries the selection when the row is in it) and of the
    file manager's folders (`ExternalPaths`, "Too Many Folders" past the cap) use the ported drop math, spring-load a
    folded workspace after 650 ms and fold it back unless the drop lands; Close and Delete confirm through GPUI's
    prompt; Open Directory… uses the file chooser portal (`ashpd`, titled, starting in the active session's directory);
    the divider follows the pointer's absolute position, clamped, coalesced into `AppState::sidebar_resized`; a hidden
    sidebar, a zoom or the dashboard hide it. `AppState::sidebar_toggle_workspace` now also queues the store's outline
    request (`set_workspace_expanded_in_outline`), which is how a fold reaches the sidebar's expansion (the GTK build
    set it on its window directly).
  - **Glyphs**: `row_icon.rs` traces the GTK build's cairo glyphs with a small cairo-model painter over tiny-skia
    (`row_icon/ink.rs`: current point, `arc`, `new_sub_path`, knock-outs as `BlendMode::Clear`) into a coverage mask
    painted through `Window::paint_alpha_mask` (GPUI patch 0003) in the row's color, cached in the sprite atlas per
    glyph and device size; added the disclosure chevron, the "+" and the footer's `rectangle.stack.badge.plus` and
    `plus.rectangle`, which GTK took from its icon theme. `status_glyph.rs` ports the resolver and draws the six
    silhouettes the same way; the blink is a 1 s autoreversing opacity animation, off while the desktop's
    `enable-animations` (settings portal, followed live; on when unknown) is off.
  - Tests: 55 new (the GTK `rows`, `drag`, `menu`, `context`, `row_icon` and `status_glyph` tests ported onto the
    GPUI code, the pure click-selection, confirmation and look tests, and 10 in GPUI's test window over a real core:
    clicks with Ctrl/Shift and the right-click narrowing, the fold after the double-click interval and through the
    disclosure with the whole-row setting off, rename committed by Enter and cancelled by Escape, session and
    workspace drag reorder, spring-loading folded back unlanded, the hover "+", the flagged-view toggle and its hint,
    the confirmed and cancelled close, the divider resize clamped at the floor and the hidden sidebar, the status
    glyph's states, the row menu's and the footer menu's items). Gates: fmt, clippy (workspace, all targets and
    features), lint-report, check-tracing-features, `vendor-gpui.sh --check`, `vendor-calloop.sh --check`,
    `cargo test --workspace` under Xvfb (3865 passed, 0 failed), the new tests 10 times in a row. A smoke run of the
    real binary on a private Xvfb with lavapipe (sessions, workspaces, a blocked star status and a truncated name made
    over the control socket, screenshot checked).
  - Remaining before the gates box: the GPUI conformance run and its ratchet (Task 17); for Task 14: the keymap's
    Rename Session/Workspace, Close Session, Delete Workspace and Open Directory… reach `WindowView::sidebar()`
    (`begin_rename`, `close_sessions`, `delete_workspace`, `open_directory`), and the window's key capture must let
    the rename field's keys (Enter, Escape, editing) through while it holds the keyboard. The GTK conformance run is not
    needed: the GTK binary links none of the changed crates.
  - Review round 1 (merged with gpui-frontend at Tasks 13–18): a right-click's row menu holds the keyboard
    (`Sidebar::menu_open`, set as the menu builds, cleared on its `DismissEvent`, which posts a sync): meanwhile the
    deck moves no focus (`Deck::hold_keyboard`, from `WindowView::sync`), then gives the shown terminal the keyboard the
    menu handed back, or leaves it with a field the menu's item focused (Rename); a press outside the rename field
    commits it (`on_mouse_down_out`: the disclosure, the hover "+" and the rows take no focus); a selection made
    elsewhere that leaves the Shift anchor out moves it to the selected session; Open Directory…'s chooser is parented
    to the window (`ashpd::WindowIdentifier::from_raw_handle` over GPUI's raw window and display handles, read from the
    live window as the chooser opens; ashpd `raw_handle` + `wayland`); `add_session` reuses `frame`; the divider test
    covers the dashboard, the glyph tests check the blink (`agent-status-blinking`/`-still`) and the reduced-motion
    switch through the sidebar. The duplicated GTK-free sidebar logic (`rows`, `context`, the `drag` math, the `menu`
    model, the glyph resolver) stays duplicated on purpose: Task 34 deletes the GTK frontend and its copy.
  - Task 14 wired: the keymap's Close Window posts the close; Close Session's confirmation, Rename Session/Workspace,
    Delete Workspace and Open Directory… run on `WindowView::sidebar()` (`keymap_actions::with_sidebar`); Rename
    Window waits for its dialog. The key monitor already leaves every key to a focused `Input` (the rename field):
    tested through the window with the monitor installed (editing keys, Enter, Escape). Gates: fmt, clippy, lint-report,
    `cargo test --workspace` under Xvfb 3981 passed, 0 failed, 6 ignored; GPUI conformance ratchet-only with `--also-k
    "sidebar or workspace or rename or reorder or collapse or flag"`: 455 pass, 1 fail (an HTML overlay test, not
    listed), ratchet held (453 ids) and grew by 2 (relaunch keeps workspaces and sessions, `agtermctl tree`). No
    capability claimed; `sidebar-min-width` is the next candidate (the GPUI sidebar takes the model's width with no
    floor of its own), unverified under the claim.

### Task 20: Title bar, chrome, appearance

**Files:**
- Create: `crates/agterm-gpui/src/{titlebar,chrome,appearance}.rs`
- Modify: `conformance/tools/chrome_probe.py` (GPUI build)
- ➕ Create: `crates/agterm-gpui/src/{titlebar,chrome,appearance}/tests.rs`, `crates/agterm-gpui/tests/appearance.rs`;
  Modify: `crates/agterm-gpui/src/{app,lib,state,session_host,terminal,window,windows}.rs`,
  `crates/agterm-term/src/{effects,lib,pane}.rs`, `crates/agterm-term/tests/pane.rs`,
  `crates/agterm-ghostty-vt/src/{effects,lib,shape}.rs`, `crates/agterm-ghostty-vt/tests/effects.rs`,
  `conformance/selftests/test_chrome_probe.py`, `conformance/README.md`, `docs/linux-ui-checklist.md`

- [x] title bar and chrome painted in the terminal's colors (parity with `fd5349f`)
- [x] follow the desktop light/dark (`org.freedesktop.appearance color-scheme` through the settings portal, `zbus`),
      replacing `adw::StyleManager`; feeds core's `light:/dark:` resolution
- [x] tests: appearance switch re-resolves the theme; `chrome_probe.py` pixel check passes on GPUI — the appearance
      tests pass; the pixel check passes on GPUI (theme, theme-change, tint)
- [x] gates + conformance — must pass before Task 21
- result (2026-10-15; Tasks 17 and 19 not merged yet):
  - **Chrome** (`chrome.rs`): `agterm_core::chrome`'s colors resolved from the terminals' config and the sidebar
    tint into one `Chrome` GPUI global, re-synced in `Windows::sync` after any config reload and replaced only when a
    color changed; the window body and the title bar paint its background, the bar its foreground (0.6 for the
    subtitle and the host) and the hairline its separator; the sidebar color, divider and selected-row pill wait for
    Task 19's sidebar. Client-side decorations (`WindowDecorations::Client`; the window manager's where the
    compositor insists) with resize handles on the free edges (`resize_edges`).
  - **Title bar** (`titlebar.rs`, port of the GTK `titlebar.rs` over `agterm_core::titlebar::compose`): the sidebar
    toggle at the sidebar's edge, the title's lines, the trailing cluster per Interface toggle with dividers between
    groups, Lucide glyphs (gpui-kit's catalog served as the app's assets) with the split and scratch glyphs traced,
    hover texts naming the chords, upstream's ids as element ids, the zoom/dashboard cover with its exit button,
    window controls per the desktop's button layout and what the window manager supports, drag to move, double click
    to maximize, secondary click for the window menu. The recent, attention and custom-commands buttons emit
    `TitlebarEvent::Popover` for Task 21's popovers.
  - **Appearance** (`appearance.rs`): the portal read before the first config load (no bus or portal reads as
    light; `ReadOne`, `Read` for a portal before version 2), `SettingChanged` followed on a thread of its own and
    debounced 0.05 s onto the main loop; a flip reloads only while following with both theme slots set and on a new
    side; every reload (a flip, `config.reload`, a settings change) loads `Appearance::side`, overlays included;
    `debug.appearance` forces a side. Each terminal takes the side as its color scheme: `Pane::set_color_scheme`
    answers queries with it and sends a mode 2031 report on a change (`vt::ColorScheme`, `Mode::COLOR_SCHEME_REPORT`
    in `agterm-ghostty-vt`).
  - **Probe**: `chrome_probe.py --frontend gtk|gpui` (Task 17's stage and lavapipe lookup, `RustAdapter(frontend=)`);
    on a harness without the GPUI frontend it says so instead of launching. Its selftests cover the stage paths, the
    lavapipe lookup and that refusal.
  - **Gates**: fmt; `cargo clippy --workspace --all-targets --all-features -D warnings`; `lint-report.sh`;
    `check-tracing-features.sh`; `vendor-gpui.sh --check`; `xvfb-run cargo test --workspace` 3841 passed, 0 failed,
    5 ignored (78 binaries); the new tests 10 rounds in a row without a failure or skip (the gpui `appearance`,
    `chrome` and `titlebar` units, `tests/appearance.rs` with its weston/lavapipe flip, `agterm-term`'s pane tests
    and effects units, `agterm-ghostty-vt`'s effects tests); `chrome_probe` selftests 13 passed. Not run: the
    conformance suite and the GPUI pixel check (Task 17's harness).
  - Merged with gpui-frontend at Tasks 13–19 (2026-10-15): `window.rs` keeps the title bar and the sidebar side by
    side (the bar over the whole window, the sidebar and the deck in the row under it) and Task 18's focus settle
    (`sync` returns it; the bar's cover and colors sync after the sidebar); `terminal.rs` keeps both the color scheme
    and `selection-word-chars`. One source for the chrome colors: `chrome.rs` resolves them into the `Chrome` global
    (`chrome::colors`, `default_colors` before the first sync); the sidebar's `Look::of` now takes those colors
    (`chrome::current`) and only adds the font size, its divider, selected-row pill and selected text go through
    `chrome::separator` and `chrome::selected_row` (the duplicated resolution in `sidebar/look.rs` is gone, and with
    it the sidebar's `DeckEnv`: `Sidebar::new` takes the poster). The title-bar hairline is the bar's bottom border,
    spanning the window over the sidebar's column (tested through the window: `debug_selector`s `titlebar` and
    `sidebar`), as the GTK header bar's inset shadow does. Tests: the look paints the chrome's colors, the sidebar
    follows the `Chrome` global, the hairline spans the sidebar's column.
  - Gates: fmt; clippy (workspace, all targets and features, `-D warnings`); `lint-report.sh`;
    `check-tracing-features.sh`; `vendor-gpui.sh --check`; `xvfb-run cargo test --workspace` 4014 passed, 0 failed,
    6 ignored (80 binaries). `chrome_probe.py --frontend gpui` (the probe unchanged): theme, theme-change and tint
    hold (#282a36 / #ffffff everywhere, the tinted sidebar #1c1d26). GPUI conformance ratchet-only with `--also-k
    "chrome or titlebar or appearance or theme or toolbar or dark or light"`: 467 passed, 0 failed, ratchet held (455
    ids) and grew by 12 (the `theme.set`/`theme` cli tests, the chromeless overlay refusals). Claimed
    `theme-slots-by-sync` (its branching theme tests pass on upstream's side). The themes need no stage change: the
    GPUI stage already installs `share/ghostty/themes` with `build/themes` like the GTK one, and the binary finds them
    through `resource_candidates`; the theme tests pass on it.

### Task 21: HUD, ask dialog, modal, popovers

**Files:**
- Create: `crates/agterm-gpui/src/{hud,ask_dialog,modal,popovers}.rs`
- ➕ Create: `crates/agterm-gpui/src/{hud,ask_dialog,modal,popovers}/tests.rs`, `crates/agterm-gpui/resources/hud/hud.sh`
  (a copy of the GTK build's painter; Task 34 deletes the GTK one); Modify: `crates/agterm-gpui/src/{app,deck,lib,
  keymap_actions,keymap_dispatch,sidebar,split,terminal,titlebar,window}.rs`, `terminal/{clipboard,element,mouse}.rs`,
  their tests, `docs/linux-ui-checklist.md`

- [x] HUD/captions, ask dialog, modal, popovers with upstream behavior; accessibility ids as element ids
- [x] full OSC 52 confirmation dialog replaces Task 14's minimal prompt
- [x] tests (headless): open/close, keyboard navigation, results returned to core
- [x] gates + conformance — must pass before Task 22
- result (2026-10-16):
  - **Modal** (`modal.rs`): the GTK build's panel geometry (ask cap/width/rect, palette width/offset/rect, for Task 22
    too), the dim layer and the native panel's look in the chrome's colors, the dialog keys (`nav_key`, upstream's
    `AskKeyCatcher`), and the window's **alerts**: heading, body, an optional check or text field, buttons with a
    default (Return) and a cancel (Escape), Tab/arrows moving a focus ring; modal over the whole window (dimmed,
    occluding, the key monitor leaves an `AgtermAlert` context alone); queued per window in an `Alerts` global the
    window observes. Every confirmation moved off GPUI's window prompt onto it: Close Session(s), Delete Workspace,
    Too Many Folders, Delete Window, and Rename Window (a field prefilled and selected, OK commits `window.rename`;
    it was logged as waiting for its dialog). Confirmations follow the GTK build: Cancel is the default, the verb
    destructive.
  - **Ask dialog** (`ask_dialog.rs`): one `AskDialog` view for both owners. A terminal ask is drawn on its page above
    the HUD (`Place::Full` or `Place::Over(pane)`, `shown_scope`), in the terminal's font and colors with block
    buttons; a GUI ask is the window's (`Layer::Ask`), placed over its anchor (the deck's bounds recorded each frame,
    the page or its pane's rectangle) with the rest of the window click-blocked. Equal-width buttons in a row, a column
    when the row does not fit the cap (labels shaped with GPUI's text system), aligned as asked; hotkeys underlined (or
    ` (X)` appended), a terminal destructive button `! ` and bold; Tab/Right/Down, Shift+Tab/Left/Up, Return, the
    hotkey letters, Escape (`escaped`); modified keys go on to the window. Answers post `answer_window_ask` /
    `answer_session_ask`; a click on a terminal ask selects its pane. Ids `ask-dialog`, `ask-title`, `ask-message`,
    `ask-button-<id>`.
  - **HUD** (`hud.rs`): the painter written under the state dir and the cell estimator (the terminal's own face
    metrics through GPUI's text system) installed at launch (`session.hud.open` was refused before); the panel is a
    passive terminal view (`ViewRole::Passive`: lays out and starts its pane, no focus, no pointer) the deck mounts
    per slot generation, installs in the slot and places through `Place::Hud` (`PanelStyle`, scoped to its pane while
    that is laid out); frame hairline and radius unless frameless; its start and font changes post the remeasure, its
    exit closes the slot, a reload keeps its measured size. Ctrl-click over it opens its link (`ClickOrigin::Hud`)
    and goes no further; a Ctrl-hover underlines the link with the pointing hand; plain clicks pass through to the
    pane; an ask above it occludes both gestures.
  - **Popovers** (`popovers.rs`): recent sessions, attention (status glyph, remote cloud) and custom commands (most-run
    above a separator) under the clicked title-bar button (`TitlebarEvent::Popover` now carries the anchor); one open
    per window as `Layer::Popover`, holding the keyboard (Up/Down, Return, Escape), a press outside closes it and goes
    no further, an emptied list or a button turned off closes it; `PopoverOpened`/`PopoverClosed` reach the core. Ids
    `recent-session-row`, `attention-session-row`, `custom-command-top-row`, `custom-command-row`.
  - **OSC 52** (`terminal/clipboard.rs`): upstream's alert (`prompt_alert`): the direction's heading and message,
    Deny (Escape) / Allow (Return), the "Don't ask again this session" check remembering the answer per direction;
    the one-prompt-per-terminal-and-direction queue is unchanged.
  - **Focus return** (review finding at `deck.rs:582`): an alert, the GUI ask, a popover or the model's picker/palette
    are modals the deck moves no keyboard under; an alert or popover closing posts a sync, and a released row-menu hold
    always counts as a pending move, so after any dialog, popover or prompt the shown terminal (or the terminal ask that
    wants the keyboard) takes it back. A sync re-focuses the front alert if a closing menu took the keyboard back. The
    cancelled Close Session from a row menu is tested end to end through the window (right-click, alert, Return/Escape).
  - Tests: modal (geometry, keys, alert ring/Return/Escape/check/field/queue/observers), ask dialog (labels, looks,
    strip, answers, scopes, focus rules), HUD (placement incl. `Place::Hud`, config, helper, link modifier, cell
    estimate), popovers (keyboard run, Escape, outside press, second click, button turned off), deck (terminal ask
    keys → `ask.result` and the keyboard back, hidden-session ask, HUD mount/replace/close/hide, HUD Ctrl-click and a
    plain click through it), window (row-menu confirmation cancelled/confirmed, GUI ask answered by keys, popover and
    Rename Window give the keyboard back), clipboard tests on the alert. Gates: fmt, clippy (workspace, all targets
    and features), lint-report (9 expects, unchanged), check-tracing-features, `vendor-gpui.sh --check`,
    `cargo test --workspace` under Xvfb 4063 passed, 0 failed, 6 ignored; the new and changed tests 5 times in a row.
    GPUI conformance ratchet-only with `--also-k "hud or caption or ask or modal or popover or clipboard or osc52 or
    confirm or pick"`: 544 passed, 3 failed (two HUD-over-HTML-page tests: Task 32; a program overlay's cursor: Task
    23), 1 skipped; ratchet held (467) and grew by 77 (49 `test_hud`, 23 `test_ask_pick`, 5 cli `hud`/`caption`/`ask`)
    to 544. Claimed `modal-blocks-covers` and `pick-names-pending-ask` (their tests are among the new ids). No GTK
    conformance run: no crate the GTK binary links changed.
  - Deviations: a Ctrl-press opens the HUD's link on press (GTK forwarded press and release to libghostty, which
    opened on release); a modifier change with the pointer still updates the HUD's hover on the next pointer move;
    a GUI ask whose anchor is not on screen keeps the keyboard but draws nothing until it is.

### Task 22: Picker, palette, session switcher, search bar

**Files:**
- Create: `crates/agterm-gpui/src/{picker,palette,session_switcher,search}.rs`

- [x] picker, command palette, Ctrl-Tab switcher, search bar over Task 10's search
- [x] tests (headless): filtering, navigation, selection results, search next/prev and counts
- [x] gates + conformance — must pass before Task 23
- result (2026-10-16):
  - ➕ Files: `src/{picker,palette,session_switcher,search}/tests.rs`; Modify: `src/{keymap_dispatch,lib,modal,state,
    titlebar,window,window/tests}.rs`, `docs/linux-ui-checklist.md`.
  - **Picker** (`picker.rs`, the GTK `picker.rs` port): `Picker` view per pick id in `Layer::Picker`, scrim `pick-scrim`
    (a press cancels), panel `pick-palette` near the top over the terminal area, a gpui-kit `Input` field (the prompt
    as placeholder, the query prefilled), rows `palette-item-<id>`; `filter_rows`/`initial_selection`/`moved`/`outcome`
    as GTK, `pick_key` read off the key monitor's `KeyStroke` (Up/Down, Ctrl+J/K through `terminal::input::chord_key`,
    Return, Escape, Tab kept), one answer posted as `resolve_pick`. The panel's frame, field and rows (`frame`,
    `header`, `row`, `text_column`, `Placement`) are shared with the palettes.
  - **Palettes** (`palette.rs`): `PaletteView::of` (rows, enablement, live chords as GTK accelerator labels through
    `titlebar::chord_label`, attention glyphs), a `Palette` per generation in `Layer::Palette` (`command-palette`,
    `palette-scrim`, `palette-subtitle`, `palette-badge`), refreshed in place while unchanged (`reselect`), the theme
    preview debounced 70 ms by a GPUI timer that any other request drops; `AppState::palette_request` (stale
    generations ignored; a run's frontend half through the new `AppState::frontend_action`, landed after the windows'
    sync as GTK's settle-then-apply); a launcher's menu chord typed in the field reaches its action
    (`keymap_dispatch::menu_action`).
  - **Switcher** (`session_switcher.rs`): GTK's `Switcher` walk, a click-through `session-switcher` panel (no listener,
    no focus) always attached in `Layer::Switcher`; Ctrl+Tab/Ctrl+Shift+Tab always consumed (only ending a walk while
    the window takes no UI actions), Escape during a walk; Control's release through the window root's
    `on_modifiers_changed`; a deactivation resets the walk; rows lead with the cloud for a remote session (upstream's
    `SessionSwitcherRow`, which the GTK build lacked).
  - **Search bar** (`search.rs`): always attached in `Layer::Search`, drawn at the terminal area's top right (the deck's
    bounds); `search-field`, `search-counter` (`SearchState::display_text`), previous/next/close buttons with hover
    texts; Return/Shift+Return step, Escape or Find's chord closes; the GTK sent/landed counter keeps a settle from
    rewinding what is typed; input is dropped when another search shows by the time it lands (`shown_key`).
  - **Keys**: the window's own keys run first in the key monitor (`window::intercept_key`, before the keymap, `keeps_keys`
    and the terminal; the GTK capture-phase controllers): the switcher's, then the focused picker/palette/search field's
    navigation keys; everything else types into the field (the `Input` context keeps it from the keymap).
  - **Focus**: a new picker or palette takes the keyboard; an alert closing over a picker, palette or GUI ask gives it
    back to the front one (palette above the slot); `focus_terminal`/`present` prefer picker, ask, palette, popover;
    the search bar takes it when it comes up unless a modal holds it (synced after the deck); closing a picker or
    palette hands it back through the deck's modal reconcile, the search's end through the core's `request_pane_focus`
    on the `Ended` report. `WindowView::sync` split into `sync_modals` and `sync_title`.
  - **Interface font size** (upstream's `InterfaceMetrics`, which the GTK build did not render): `modal::Metrics`
    (base, secondary, shortcut, scale) from `effective_interface_font_size` scales the palettes' and switcher's text,
    paddings, panel widths (520/460) and results height (320); `modal::{fitted_width, palette_cap, palette_top}`.
  - Tests: picker (rows, keys incl. upstream's layout cases, placement and metrics, the view: prefill, caller's row,
    typing, one answer), palette (reselect, labels, A→Z/filter/inert/run/close/launcher, theme open row and debounced
    preview), switcher (walk, keys, frozen rows, the panel), search (keys, shown search, input never lands on another
    search), window (picker answered by Down/Return and cancelled by Escape with Ctrl+J/K and typing; palette row run by
    typing and Return, closed by its launcher's chord and Escape; Ctrl+Tab walk, Escape, Control release selecting;
    search bar focus, typed needle kept, counter, end gives the terminal the keyboard back). Gates: fmt, clippy
    (workspace, all targets and features), lint-report (9 expects, unchanged), check-tracing-features,
    `vendor-gpui.sh --check`, `cargo test --workspace` under Xvfb 4095 passed, 0 failed, 6 ignored; the new tests 5
    times in a row. GPUI conformance ratchet-only with `--also-k "picker or palette or switcher or search or find or
    pick"`: 545 passed, 0 failed, 2 skipped (`test_search_counts_the_matches`, `test_session_type_text_search_and_copy`:
    posix-shell, unclaimed until the quick terminal, Task 24); ratchet held (544) and grew by 1
    (`test_zmx.py::test_prune_finds_no_orphans`) to 545. No capability claimed (the UI tests need a UI driver; the
    search counts wait on posix-shell). No GTK conformance run: no crate the GTK binary links changed.
  - Deviations: the palettes and switcher apply the Interface font size (upstream's; the GTK build kept GTK's size);
    the switcher shows a remote session's cloud (upstream's); the search bar is placed over the deck's bounds rather
    than the GTK content overlay; a real non-Latin layout for Ctrl+J/K is only unit-tested (`chord_key`), checked at
    the Task 33 walk.
  - Later tasks build on: `window::intercept_key` (Task 24's quick terminal is no `WindowView` and takes none),
    `AppState::frontend_action`, `keymap_dispatch::menu_action`, `picker::{frame, header, row, Placement}` and
    `modal::Metrics` for any other list panel, the posix-shell claim (Task 24) bringing the two search conformance
    tests in.

### Task 23: PTY overlays, notifications, welcome

**Files:**
- Create: `crates/agterm-gpui/src/{overlay,notifications,status_sound,welcome}.rs`

- [x] caller programs over a page or pane (port of `overlay.rs`), exit status read-back
- [x] `show-image.sh` end to end: chunked kitty APC in a PTY overlay renders the image (Task 16)
- [x] desktop notifications via `notify-rust` with today's semantics (`notifications.rs`): one banner per identity
      (a repeat replaces it through `replaces_id`), a click reveals its pane (`ActionInvoked` → `notification-reveal`),
      clears withdraw banners (`CloseNotification`); status sound; bell; welcome
- [x] tests: overlay lifecycle and exit status, show-image end to end (headless); notifications on a private session
      bus: replace per identity, click reveals the pane, clear withdraws
- [x] gates + conformance — must pass before Task 24
- result (2026-10-16):
  - ➕ Files: `src/integrations.rs` (the welcome's installers; Task 28 extends it), `src/{overlay,notifications,welcome}/
    tests.rs`, `src/deck/tests/overlay.rs`, `tests/notifications.rs`; Modify: `src/{app,deck,deck/tests,lib,modal,
    modal/tests,split,state,state/tests,windows,keymap_actions,terminal/clipboard}.rs`, `Cargo.toml` (`notify-rust`
    4.18.2, the version GPUI's Linux platform already links), `docs/linux-ui-checklist.md`.
  - **Overlays** (`overlay.rs`, the GTK port): each page keeps an `OverlayLayer` (the program's terminal and the pane
    overlays' by generation), synced after the HUD: a program or pane overlay without a terminal gets one (installed
    in its slot; a pane overlay once its pane is laid out), a closed or replaced one goes. The terminal runs upstream's
    wrapper (`overlay_capture::surface_command`) in the caller's directory (else the session's) with the session's
    environment, the command and the status file, at the session's font size, on the overlay's own background
    (`config_overlay`), owned as `SurfaceOwner::Overlay`. Laid out by `Place::Program` (the page, or centered at its
    share: 12 px radius, white 0.18 edge, 24 px shadow, over the existing backdrop wash) and `Place::Over(slot)` for a
    pane overlay (after the divider, under the overlay washes and the scratch). Its exit closes the slot by generation
    (`overlay_exited` / `pane_overlay_exited`, the core reads the status file); a held (`--wait`) end reports
    `replica_overlay_held` and closes on the next key; keystrokes are user activity (`AppState::overlay_keystroke`);
    links route as `ClickOrigin::ProgramOverlay`; a click into a pane overlay posts `pane_overlay_focused`.
  - **Focus**: `Page::keyboard` picks the terminal ask, else the overlay the plan focuses (`OverlayFocus`), else the
    pane; `Deck::show`/`focus_target` use it, an overlay key (`overlay#g`, `overlay-pane#g`) is never reported as a
    pane focus, and once the overlay goes the changed target gives the pane the keyboard back. Config reloads and the
    quit's teardown include the overlay terminals.
  - **Notifications** (`notifications.rs`): the core's banners and status sounds land through `FrontendWork`
    (`banners`, `sounds`, taken in `settle`); `deliver`/`banner_sound` as GTK; the `Desktop` sink queues them for one
    task on GPUI's background executor (`run`): `notify-rust` banners (`appname` agterm, `desktop-entry` hint = the app
    id, a `default` action), a repeat per identity sent with the previous id (`replaces_id`), each banner's
    `ActionInvoked`/`NotificationClosed` waited for on its own connection (aborted on replace/withdraw), a click posting
    `AppState::reveal_notification`, a withdraw sending `CloseNotification`, a server-closed banner forgotten.
  - **Status sound and bell** (`status_sound.rs`): one throttle over every name as GTK; the bell rung as GDK's
    `Display::beep` was: Wayland `xdg_system_bell_v1` on GPUI's connection for the frontmost window, X11 `Bell`;
    headless nothing.
  - **Welcome** (`welcome.rs`): due as GTK computes it (prior state read before the launch writes), `welcomeShown`
    set before it opens, suppressed on a UI-test launch; an alert with both checks checked and upstream's ids as
    element ids; Install runs the skill then the hooks installer (`integrations.rs`, the OpenCode version chooser and
    result alerts included). The alert gained several checks with initial state and element ids (`AlertCheck`,
    `AlertAnswer::checks`).
  - Tests: overlay (rect, config, keys); deck over real panes (program covering, keyboard, exit 7 read back, pane gets
    the keyboard back; floating at 50% over the backdrop, hidden with its page and refocused; pane overlay over its
    pane with its owner, closed with the keyboard back; held end closing on a key; `show-image.sh --emit` drawing a
    4x2 PNG from the window's atlas through chunked APC in the PTY overlay, Enter closing it with exit 0);
    notifications (order, sound, sink queue; a `dbus-daemon` of the test's own with a fake
    `org.freedesktop.Notifications` server, the scenario in a child whose bus is only that one: replace per identity,
    a second pane's own banner, `ActionInvoked` revealing the identity, a clear withdrawing only the banner still up);
    state (a `notify` lands its banner and sound once); status sound (throttle, burst); welcome (spec, keys dropping a
    check, Later, marked shown); integrations (alerts). Gates: fmt, clippy (workspace, all targets and features),
    lint-report (9 expects, unchanged), check-tracing-features, `vendor-gpui.sh --check`, `cargo test --workspace`
    under Xvfb 4118 passed, 0 failed, 7 ignored (the private-bus child is one); the new tests 5 times in a row.
    GPUI conformance ratchet-only with `--also tests/protocol/test_overlay.py` and `--also-k "overlay or notify or
    notification or bell or welcome or image or exit_status or sound"`: 571 passed, 17 failed (15 HTML page tests,
    Task 32; `test_overlay_open_size_outside_its_range[0-1|101-100]`, which expect the fork's clamp without the
    capability), 1 skipped; ratchet held (545) and grew by 26 (the program/pane overlay tests, `test_open_refusals`,
    `test_overlay_program`, `test_zoom_the_pane_under_a_running_overlay`,
    `test_cursor_reads_a_program_overlay_but_not_a_hud`) to 571. Claimed `overlay-size-validated` (the core refuses
    the range exactly as the claimed branch asserts, seen in that run's failure); its two ids were not added (not seen
    passing). No GTK conformance run: no crate the GTK binary links changed.
  - Decisions: a terminal BEL rings nothing (upstream's rule: `RING_BELL` is no notification source; neither upstream
    nor the GTK build sounds it); the welcome's Install runs real installers here rather than waiting for Task 28;
    the quit confirmation of the GTK `welcome.rs` waits for the app menu's Quit (Task 26).
  - Deviations: banners go over `zbus` directly rather than `notify-rust` (review round 1: `notify-rust` neither
    exposes the server's `ActivationToken` nor sends on a connection the app owns, which KDE targets that signal at),
    so `notify-rust` is no dependency; one task-owned connection carries every banner and its signals, and a click
    raises the window with the server's token through `activation::activate`. Review round 1 also saw the two
    `test_overlay_open_size_outside_its_range` ids pass with the claim and added them (ratchet 573).
  - Later tasks build on: `OverlayLayer` and `Place::Program` (Task 32 mounts HTML pages in the same places),
    `FrontendWork::{banners, sounds, welcome}`, `status_sound::ring`, `integrations.rs` (Task 28), `AlertCheck`.

### Task 24: Quick terminal on layer-shell

**Files:**
- Create: `crates/agterm-gpui/src/quick/` (placement from `agterm-app`)

- [x] quick terminal on `WindowKind::LayerShell` per Task 3; fallback window where the protocol is missing
- [x] ➕ (Task 3) host chosen by `cx.compositor_name()` ("Wayland" → layer surface, falling back on
      `LayerShellNotSupportedError`; X11 → the window host directly, since GPUI opens `LayerShell` on X11 as a plain
      window): it must work in every environment (user, 2026-10-14) — layer-shell compositors, GNOME/weston (no
      layer-shell) and X11. Fallback window: undecorated (`WindowDecorations::Client`, no titlebar, transparent
      background), zoom = maximize
- [x] ➕ (Task 3) the GTK build's surface attributes: overlay layer, namespace, keyboard on demand, exclusive zone 0,
      top anchor + centering margin; **name no output on show** (niri gives no keyboard to an on-demand surface on an
      unfocused output), then refit by reopening named on the output it landed on (`observe_window_display`)
- [x] ➕ (Task 3) hide = `remove_window`, show = a new layer surface (GPUI cannot hide a window on Linux); zoom and the
      refit reopen the surface (no runtime anchor/margin/layer setters — GPUI patch 5 optional); focus loss via
      `observe_window_activation` dismisses a summoned panel; `zwlr_layer_surface_v1.closed` (output gone) maps to the
      GTK build's dismissal
- [x] ➕ (Task 3) fractional-scale outputs: GPUI's display bounds use the integer `wl_output.scale` (eDP at 1.75 reports
      1440x900 instead of 1645x1028, the panel 64 px off): land GPUI patch 4 (`zxdg_output_v1` logical geometry) or
      compute placement from the output's logical size another way; test the placement math against both sizes
- [x] toggle via keymap and control commands; focus and hide behavior as upstream
- [x] tests (headless, fallback path); quick-terminal conformance tests green
- [x] ➕ (Task 3) fallback tests on both backends: weston (`--fake-seat`, no layer-shell) and **X11 under a private
      Xvfb** (open, size, resize, hide/show, placement; focus and maximize are the window manager's on X11 and are not
      asserted without one); a nested niri run for the layer-shell path where niri is installed
- [x] gates + conformance — must pass before Task 25
- result (2026-10-16):
  - ➕ Files: `src/quick.rs` (the panel, its sync and terminal), `src/quick/{host,view,tests}.rs`, `tests/quick_terminal.rs`,
    `build/patches/gpui-pre-linux/0002-xdg-output-logical-geometry.patch` (vendored into `vendor/gpui-pre-linux`);
    Modify: `src/{windows,deck,state,window,app,lib}.rs`, `agterm-core/src/commands/quick.rs` (+ tests),
    `agterm-app/src/quick_placement.rs` (tests), `scripts/vendor-gpui.sh` (header), `docs/linux-ui-checklist.md`.
  - **Panel** (`quick.rs`, owned by `Windows`, synced after the windows each turn): the terminal is created on the
    first show (login shell in `Core::quick_terminal_cwd`, `pane_env::quick_terminal`, the app's config) and installed
    in the core; it lives app-wide, so a hide removes only the window (`remove_window`) and the shell stays typable and
    readable; a show opens a new window with a fresh owner view of the same terminal. Its exit (or a failed spawn)
    posts `quick_shell_exited`; keystrokes are user activity (`AppState::quick_keystroke`); links route as
    `ClickOrigin::Quick`. Config reloads and the quit's teardown include it.
  - **Host** (`quick/host.rs`): `first_host(cx.compositor_name())`: "Wayland" → `WindowKind::LayerShell` (overlay
    layer, namespace `agterm-quick-terminal`, keyboard on demand, exclusive zone 0, top anchor + centering margin,
    `display_id: None` on show), a `LayerShellNotSupportedError` moving the run to the window host for good; X11 and
    every other platform → the window host directly (undecorated `WindowDecorations::Client`, transparent background,
    title "Quick Terminal", centered on the frontmost agterm window's display, `Maximized` for the zoom, maximized in
    place via `zoom_window` when the zoom changes while shown). The Wayland/layer-shell code is behind
    `cfg(target_os = "linux")`. `step` decides keep / open / reopen / zoom: the layer surface reopens for a zoom
    (anchored to every edge at size 0) and once for the refit to the display it landed on (`observe_window_display`,
    `refits_on_enter`), named on that output.
  - **View** (`quick/view.rs`): rounded 12 px frame with upstream's white 0.18 edge on the terminal's background,
    element id `quick-terminal`; `observe_window_activation` → holding the keyboard (a GPUI global the decks read in
    place of `quick_visible`, upstream's `holdsKey`) and a focus loss → `quick_lost_focus`; the window manager's close
    request and the window going without the panel closing it (`zwlr_layer_surface_v1.closed`) → the GTK build's
    dismissal; a window the panel itself closes reports nothing (a reopen's new window takes the keyboard). A removed
    window reports no blur or deactivation, so the panel's own removal settles both (review fix): a hide sends the
    terminal's program its focus-out report and the next window's view continues the reports from there
    (`TerminalView::{release,continue}_focus_reports`), and every removal runs the key monitor's delayed
    keyboard-left check, so a key the keymap consumed in the panel (Ctrl+` hiding it) is forgotten.
  - **Fractional scale**: GPUI patch 4 landed as `gpui-pre-linux` 0002: `zxdg_output_manager_v1` (v1–3) bound, each
    output's xdg-output logical position/size complete it, `cx.displays()` and a window's display report them (the old
    mode-over-scale bounds where the compositor sends none); unit-tested in the vendored crate, and the placement math
    tested at 1645x1028 (margin 164) and 1440x900 (margin 100) in `agterm-app` and the GPUI host.
  - **Re-show suppression** (core, both frontends): upstream's `reshowSuppression` — a user toggle within 0.3 s of a
    focus-loss dismissal only ends that window (`RESHOW_SUPPRESSION`, `toggle_quick_terminal_at`,
    `quick_lost_focus_at`), so a click on the toggle that blurred the panel does not show it again.
  - Tests: host/step/options decisions (layer attributes, zoom fill, fractional placement, window host centering and
    maximize, the layer-shell refusal), the launch; the panel in GPUI's test platform over a real pane (shown and
    focused, hidden with the shell typable, shown again in a new window over the same shell, gone with its shell (EOT
    to `cat`) and a fresh one next, focus loss dismissing a summoned panel but not a pinned one, the window manager's
    close, a hide forgetting the keymap's consumed keys, a hide's focus-out and the next show's focus-in reports, a
    window being closed losing the keyboard without a dismissal); core re-show suppression; `tests/quick_terminal.rs` on real displays in the sandbox: weston (`--fake-seat`:
    the layer surface refused, window 1100x700, zoom 1280x800 and back, hide/show with the shell kept, quick.type and
    quick.text round trips), a private Xvfb (window mapped 1100x700 at (90..92, 50) — GPUI's X11 `CreateWindow` adds 2
    px — gone on hide, back on show) and a niri nested in weston (`#[ignore]`d, run with `--ignored`: an `Overlay`,
    `OnDemand` layer surface 1100x700 holding the keyboard, the zoom's reopen 1280x800 taking it with no focus loss
    logged, gone on hide; the panel there is pinned by the control show, so a dismissal could not show a focus loss
    and the run never checked one: corrected in review to the log's count, the closing guard unit-tested). Gates: fmt, clippy (workspace, all targets and features), lint-report (10 expects, none added),
    check-tracing-features, `vendor-gpui.sh --check`, `cargo test --workspace` under Xvfb 4139 passed, 0 failed, 8
    ignored; the three display tests incl. nested niri pass. GPUI conformance ratchet-only with `--also` the
    dashboard/quick, session-text, surface, restore and relaunch files and `--also-k quick`: 608 passed, 0 failed, 1
    skipped (`test_live_session_survives_the_relaunch`, live-sessions); ratchet held (573) and grew by 35 to 608.
    Claimed `posix-shell` and `quick-validates-input`. GTK conformance ratchet-only with `--also-k quick` for the core
    change: 653 passed, 0 failed, 12 skipped (not run on this host); ratchet held (665), nothing new.
  - Decisions: the re-show suppression lives in core (GTK gains it too); the decks gate on the panel holding the
    keyboard rather than on its visibility (upstream's `holdsKey`: a pinned panel the user clicked away from leaves
    the windows their terminals); a fresh owner view per panel window (a view keeps its window's atlas images).
  - Later tasks build on: `QuickPanel` in `Windows`, `quick::holds_keyboard`, the xdg-output display bounds.

### Task 25: Settings window

**Files:**
- Create: `crates/agterm-gpui/src/settings/` (one module per page)

- [x] the six settings pages of the GTK build per upstream (theme picker with light/dark, fonts, keymap and hooks
      editors entry points, …), round-trips through core
- [x] tests (headless): each page's controls write and read back through core
- [x] gates + conformance — must pass before Task 26
- result (2026-10-16):
  - ➕ Files: `src/settings.rs` (the window, its view, snapshot, edits), `src/settings/{form,draw,general,appearance,
    interface,notifications,agent_status,key_mapping,tests}.rs`, `src/folder_chooser.rs` (the sidebar's portal chooser
    moved out and shared; GPUI's platform prompt off Linux); Modify: `src/{app,lib,sidebar,state,titlebar,windows}.rs`,
    `docs/linux-ui-checklist.md`. No core change: every setting already goes through `Core::change_settings`.
  - **Window** (`settings.rs`): upstream's Settings scene — a window of its own (`SettingsWindow`, owned by `Windows`
    beside the quick terminal), 540x680, not resizable, titled Settings, client-decorated on Linux with gpui-kit's
    `TitleBar` (the platform's title bar elsewhere), opening on General; a second open raises it. Opened through
    `AppState::open_settings` → `FrontendWork::settings` → `Windows::present_settings` (Task 26's app menu item calls
    it; there is no keymap action, as in the GTK build and upstream).
  - **Pages** (one module each, upstream's order and labels): a page describes its sections and rows from a `Snapshot`
    of the core (`form.rs`: `Section`, `Item`, `Control` — toggle, picker, stepper, slider, path, glyph, diagnostics,
    button, hint), so a test reads the page as the window draws it; `draw.rs` turns them into elements in the chrome's
    colors: a glyph-over-label tab strip, scrolling grouped boxes, gpui-kit switches, sliders, color pickers and popup
    menus (the stored row checked; a menu past 12 rows scrolls), the app's native buttons and steppers (− / + greyed at
    the bounds), upstream's ids as element ids. Interface lays its element toggles two per row as upstream; hints are
    caption rows (`settings-restore-restart-hint` is now named, which the GTK build could not); the shape picker shows
    each status's tinted silhouette in upstream's 80 px column; the font picker lists the system's monospaced families
    (fontdb, read once per run off the main thread); a picked sound previews through the throttled bell.
  - **Round trip**: an edit posts one `SettingsChange` (or a Reload, or a directory chooser whose pick is one) through
    `Edits` to the core and settles; `Windows::sync` hands the open window a fresh snapshot after every task, so every
    control reads back what the core holds and a change made elsewhere (a command, a config reload, a light/dark flip)
    shows at once; the stateful controls (sliders, color wells) are reset from the snapshot after every edit, so one the
    core leaves unchanged snaps back (the GTK build's `invalidate`).
  - Tests (`settings/tests.rs`, 26): the tabs and every upstream control id; rows appearing with the settings that need
    them; pickers, diagnostics, sliders, colors, chooser starts; **every editable row of every page, under default and
    under everything-shown settings, edited through the core and read back** (and `settings.json` equal to the core's
    settings); the buttons (Reset, Use Default, both Reloads over a broken file); in GPUI's test window over a real core
    with its own state dir: tabs, a switch click, steppers and their bound, a popup menu pick by keyboard (and a sound
    pick), a button, a slider moved and moved back by a change elsewhere, a slider snapping back after an unchanged
    edit, a picked color and Reset, `browser.links` and a dark flip showing in the open window, the window opened once,
    raised, followed and forgotten once closed. No test presses Choose… (the desktop's chooser).
  - Gates: fmt, clippy (workspace, all targets and features), lint-report (10 expects, none added),
    check-tracing-features, `vendor-gpui.sh --check`, `cargo test --workspace` under Xvfb 4168 passed, 0 failed, 8
    ignored; the settings tests 5 times in a row. GPUI conformance ratchet-only with `--also
    tests/protocol/test_font_theme.py --also-k "settings or theme or font or flagged or links or keymap or hooks"`: 610
    passed, 0 failed, 0 skipped; ratchet held (608) and grew by 2 (`test_cli_output.py::test_keymap`,
    `test_keymap_hooks.py::test_run_starts_the_command_for_the_addressed_session`, which the selector brought in) to
    610. The Settings UI tests (`tests/ui/test_settings.py`) need a UI driver, which Linux has none of; no capability
    claimed. No GTK conformance run: no crate the GTK binary links changed.
  - Decisions: a window of its own rather than the GTK build's dialog over the frontmost window (upstream's scene); the
    plan's "keymap and hooks editors entry points" read as upstream's Key Mapping page (directory, diagnostics,
    Reloads) — upstream's page has no Edit buttons, Edit Keymap/Hooks stay the palette's (and Task 26's menu's) rows; no
    search field (upstream's tabs and pickers have none; the GTK dialog's was libadwaita's); fixed text sizes as
    upstream's form (the Interface font size is the palettes' only).
  - Deviations: paths are not selectable text (GPUI text has no selection); the theme and font menus scroll rather
    than search (the GTK combo rows searched past ten entries).
  - Later tasks build on: `AppState::open_settings` (Task 26's Settings… item), `folder_chooser::choose`,
    `settings::SettingsIcons` in `titlebar::Icons`.

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

- [x] wgpu-hal Vulkan import of dmabuf (fourcc, modifier, planes), fence/sync_file handling, buffer release callback
- [x] ➕ (Task 2) the patch has three parts (GPUI patch 1): the scene primitive + `Window::paint_external_surface`
      (`PaintSurface` Linux payload), the renderer draw, and device creation through wgpu-hal `open_with_callback` +
      `create_device_from_hal` adding `VK_EXT_image_drm_format_modifier` (wgpu enables only the external-memory
      extensions)
- [x] ➕ (Task 2) every memory plane's layout passed (Intel CCS: 3 planes in one BO, non-disjoint image); the import
      cache keyed per buffer and invalidated only when the buffer leaves the producer's current set
- [x] ➕ (Task 2) expose the wgpu adapter's DRM render node (`VK_EXT_physical_device_drm`) and the modifiers it imports,
      for Task 31 (cross-GPU dmabufs fail in `vkCreateImage`)
- [x] GPUI element drawing the imported texture with clipping and content mask, so overlays above it work
- [x] SHM-with-damage fallback behind the same element when import fails — a **last-resort correctness fallback**
      for documents only (Task 2: 80–115 % CPU on 1080p video, 2.5 fps on YouTube), logged as a warning when chosen,
      never selected while dmabuf import works
- [x] tests: format/modifier negotiation, fallback selection, lifetime (no use after release) with a synthetic dmabuf
      (GBM/udmabuf) under Mesa where available
- [x] gates — must pass before Task 30
- result (2026-10-15):
  - **Vendoring**: `vendor/gpui-pre` and `vendor/gpui-pre-wgpu` are the sha256-verified crates.io 0.3.9 sources (minus
    gpui-pre's `examples/` and `Cargo.lock`) plus `build/patches/gpui-pre/0001-external-surfaces.patch` and
    `build/patches/gpui-pre-wgpu/0001-dmabuf-external-surfaces.patch`, wired by `[patch.crates-io]`;
    `scripts/vendor-gpui.sh [--check|--diff]` (the calloop pattern of Task 12; `--diff` turns edits under `vendor/`
    back into the patches), checked in CI. No pinned git fork: offline, reviewable as two patch files.
  - **gpui-pre** (scene/window half): `PaintSurface.source: ExternalSurface` on Linux — `DmaBuf(Arc<DmaBufFrame>)`
    (an `Arc<DmaBuffer>`: fourcc, modifier, every plane's fd/offset/stride) or `Pixels(Arc<PixelFrame>)` (premultiplied
    BGRA, a serial, damage rects, one renderer texture per `PixelSurface`); every frame carries a `FrameCompletion`
    reporting once `Presented` / `Discarded` (dropped undrawn) / `Failed(reason)`.
    `Window::paint_external_surface(bounds, source)`; `dmabuf_import_caps()` (adapter, render node, formats with
    modifiers and plane counts, or the reason it imports none) and `external_surface_stats()`.
  - **gpui-pre-wgpu** (renderer half): `create_device` opens a Vulkan adapter through `open_with_callback` adding
    `VK_EXT_image_drm_format_modifier` (+ the external-memory extensions if wgpu left them out) when the adapter has
    them and Vulkan 1.2 timeline semaphores, else as before (caps then say why); caps from
    `VK_EXT_physical_device_drm` (the `/dev/dri` node with that rdev) and the AR24/XR24/AB24/XB24 modifiers whose
    tiling features sample linearly and whose image format properties import a dmabuf. Import: checked against the caps
    first (format, modifier, its plane count, one buffer object for all planes), then `VkImage` with
    DRM-modifier tiling and every plane's layout, a dedicated allocation importing a dup of plane 0's fd,
    `texture_from_raw` (wgpu-hal frees image and memory when wgpu drops the texture after the GPU is done) →
    `create_texture_from_hal`; cached per `Arc<DmaBuffer>` with a `Weak` (dropped at the first frame after the producer
    and every scene let the buffer go), failures cached the same way so a failed buffer is not re-imported each frame.
    A pipeline of its own (group 1 bindings 4–6) draws within the content mask, padding alpha opaque, premultiplied
    texels for either target alpha mode. **Release**: every submission drawing a frame for the first time signals a
    timeline semaphore of the renderer's (`Queue::add_signal_semaphore`) and a thread reports those frames `Presented`
    after `vkWaitSemaphores` on it — outside wgpu's locks, since `Device::poll(Wait)` holds the fence lock that
    `Queue::submit` needs.
  - ➕ (review) **acquire from the foreign queue family — a GPU copy, not direct sampling**: wgpu-core 29 enters
    every texture from hal as UNINITIALIZED, so its first use was an UNDEFINED → SHADER_READ_ONLY transition (Mesa ANV
    gen12 re-initialises fast-clear state there, and CC modifiers keep the clear colour in the dmabuf), and wgpu never
    acquires from `VK_QUEUE_FAMILY_FOREIGN_EXT`; no wgpu 29 API seeds a tracked state, and raw and wgpu commands may
    not share an encoder. So the import stays a raw `VkImage` wgpu never sees (`VK_EXT_queue_family_foreign` now
    required, modifiers filtered for TRANSFER_SRC): per new frame a wgpu command buffer clears the buffer's snapshot
    texture once and moves it to COPY_DST (`transition_resources`), a raw-only one acquires the image (FOREIGN →
    queue family, GENERAL → TRANSFER_SRC), `vkCmdCopyImage`s it into the snapshot and releases it, then the frame
    samples the snapshot; redraws copy nothing. `Presented` means the copy finished; imported images are destroyed
    once their buffer left and the timeline passed their last copy. Deviation from "zero-copy into a wgpu texture":
    no CPU copy, one GPU copy per new frame (1080p60 ≈ 1 GB/s of read+write bandwidth, nominal). Codex-reviewed
    (agreed), transcript
    `~/.local/state/plan-exec-revmux/gpui-frontend/discussions/fixer-task-29-foreign-acquire/transcript.md`. Also:
    `FrameOutcome::Lost` (device lost, renderer shut down; a slow GPU is waited for, logged after 10 s) never requests
    the SHM fall-back, only `Failed` (import) does; frames of an abandoned submission report `Discarded`; no timeline
    on GL renderers (CPU pixels only there); the SHM canvas alternates two copies, catching up on the previous
    frame's damage, so a frame no longer clones the whole canvas. Tests: the hardware case renders through EGL/GLES
    on GBM buffers of each non-linear XR24 modifier (Intel X/Y tiled, CCS 2-plane, CCS_CC 3-plane), GL clears red then
    blue in the same buffer, both read back, then a redraw after GL cleared green without handing over still shows
    blue with no new copy; under `AGTERM_REQUIRE_DISPLAY_TESTS` a skipped case fails (CI loads `udmabuf`).
  - **agterm-gpui `external_texture`**: `negotiate(caps) -> FrameSource::{DmaBuf { render_node, formats }, Shm {
    reason }}` (the drawn formats preferred AR24, XR24, AB24, XB24, `DRM_FORMAT_MOD_INVALID` dropped, LINEAR last; SHM
    only without a device, without dmabuf import, without a render node or drawn format — logged as a warning);
    `import_refusal(caps, buffer)` (the NVIDIA case: a modifier outside the device's set); `ExternalTexture`:
    `push_dmabuf(buffer, Acquire::{Implicit, SyncFile(fd)}, on_done)` shows a frame once its fence polls readable (the
    dmabuf's implicit fence or a sync_file; `prepare(window)` asks for another frame while one waits),
    `push_shm(width, height, regions, on_done)` composes damaged regions into a CPU copy (copy-on-write while the
    renderer still reads the last one), `take_fallback()` once when a buffer was refused or failed in the renderer
    (warning logged; dmabuf frames are then discarded); `external_texture(surface)` is the element (a canvas painting
    `paint_external_surface` over its bounds).
  - Tests: 21 unit tests (negotiation, the per-buffer check, sync_file gating, discard on replace/clear, outcomes and
    the fall-back, the SHM canvas) and `tests/external_texture` (custom harness, a child per Vulkan driver, no display:
    GPUI's `HeadlessAppContext` + its headless wgpu renderer read back): 11 cases on lavapipe and on the Intel iGPU
    (ANV, `renderD128`) — caps (render node and non-linear modifiers on Intel), udmabuf dmabufs drawn zero-copy with
    padding alpha opaque, one import per buffer (new pixels in the same buffer show without a re-import), release (a
    reused buffer never reaches the screen, the import goes once the buffer left the set), sync_file gating, AR24
    alpha blending, a refused modifier (never reaches the renderer), a failed import (outcome, fall-back, not
    retried), an Intel 3-plane CCS buffer from GBM (modifier `0x0100000000000008`) imported and presented, SHM damage
    uploads (only the damaged bytes), clipping by an `overflow_hidden` parent and an overlay painted above.
  - ⚠️ Not tested headlessly: NVIDIA's own driver (no Mesa, no headless path here) — Task 33 walks it on the real
    screen; the element's `Acquire::Implicit` there relies on NVIDIA attaching implicit fences to its dmabufs.
  - For Task 32: map `agterm_wpe::DmaBuf` to one `gpui::DmaBuffer` per `BufferId` (dup the fds once), drop it on
    `BuffersRemoved`; `Presented`/`Discarded`/`Failed` all answer `Host::frame_presented`, and `take_fallback()` →
    `Host::fall_back_to_shm(view)`; `negotiate(dmabuf_import_caps())` → `FrameMode` (caps exist only once a window drew
    its first frame); WPE hands no rendering fence, so `Acquire::Implicit`.

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
- [ ] ➕ (Task 16 review) precondition: `docs/backlog/kitty-virtual-placement-c-api-gaps.md` closed — the Kitty
      placement-id namespace and relative placements rooted at virtual ones drawn as libghostty draws them (upstream
      API + pin bump, or a local libghostty-vt patch), or the user's explicit scope exception
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
