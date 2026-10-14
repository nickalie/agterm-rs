# GPUI frontend in place of agterm-gtk

## Overview

- Replace the GTK4/libadwaita frontend (`agterm-gtk`) with one built on GPUI (Zed's UI framework), so a single UI
  codebase can later carry macOS and Windows. This plan targets Linux only; nothing in it may close the other platforms.
- The terminal moves from full libghostty (its own OpenGL renderer inside a `GtkGLArea`) to **libghostty-vt** (Ghostty's
  parser and terminal state, no renderer) with the grid drawn by GPUI, the way Zed draws its terminal. GPUI cannot host
  a foreign GL/Metal view, and full libghostty has no Windows apprt; libghostty-vt keeps upstream's VT semantics.
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

- **libghostty-vt** — `Uzaaft/libghostty-rs`, crates `libghostty-vt` / `libghostty-vt-sys` 0.2.2 (2026-09-28), MIT,
  ~165k downloads, active. Safe API: `Terminal` (effects `on_pty_write`, `on_bell`, `on_title_changed`,
  `on_pwd_changed`, `on_desktop_notification`, `on_progress_report`, `on_clipboard_read/write`, `on_color_scheme`,
  `on_size`, …), `RenderState` + row/cell iterators with dirty tracking and two-phase update, `KeyEncoder`,
  `MouseEncoder`, `search`, `selection`, `fmt` (formatter), `paste`, `snapshot`, `kitty`, `osc`. Builds Ghostty with
  Zig 0.16 from the commit pinned in its `build.rs` (`3425025e585a3403340d4dc6d65132f42e05e605` at 0.2.2);
  `GHOSTTY_SOURCE_DIR` points it at a local checkout (no network); links `libghostty-vt.a` statically by default.
- **gpui-kit** — `longbridge/gpui-kit` (formerly gpui-component), ~17k stars, daily commits, Apache-2.0-family. Pins
  Zed's GPUI as the crates.io snapshot `gpui-pre`, re-exports it; 75+ components; headless UI integration tests. Its
  optional `gpui-fast` backend is longbridge's own implementation — **not used** here.
- GPUI's Linux renderer is wgpu (Zed PR #46758, 2026-02-13, replaced Blade); macOS/Windows keep native renderers.
- `WindowKind::LayerShell` in GPUI (Zed PR #35610, after the revert of #32651): wlroots compositors, KDE, niri; not
  GNOME, not X11.
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
- dependencies follow the "popular tools only" rule: `libghostty-vt`, `gpui-kit`, `portable-pty`, `notify-rust`, `zbus`, `notify`, `gir`.
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
  (software Vulkan) — no dependency on the NVIDIA driver in CI-like runs.
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
agterm-term   (no UI)  libghostty-vt Terminal + RenderState, PTY (portable-pty), IO thread per pane,
                       effects → main-thread channel, render/kitty views, encoders, selection/search/read-text,
                       launch (zmx attach or plain) + TERM env;
                       libghostty-vt types never leave its public API
        ▲
agterm-gpui → bin `agterm`   gpui-kit app: AppState, windows, sidebar, deck/splits, TerminalElement (grid, sprites,
                       kitty images, background image), Surface impl, overlays, quick terminal (LayerShell),
                       settings, remotes, integrations UI, HTML overlay element
        ▲
agterm-wpe   (Linux)   WPE WebKit offscreen in its own thread/GMainContext, gir bindings, dmabuf frames
gpui-pre patch         wgpu external-texture element importing dmabuf (zero-copy video)
```

- **Threads** (as in Zed): GPUI owns the main thread; `MainLoop` is GPUI's foreground executor. Each pane's IO thread
  reads the PTY and calls `vt_write` under the pane's mutex; effects go to the main thread through a channel and wake
  the view (`cx.notify()`, coalesced by GPUI to a frame). Painting locks only for `begin_update`.
- **Config**: no libghostty config parser any more. Core parses ghostty config itself, same load order; unknown
  ghostty keys → diagnostic (keeps `config.reload`'s count meaningful); known-to-ghostty but unsupported keys → ignored
  and logged. Ghostty `keybind = …=action` runs through our own action table; an unknown action → diagnostic.
- **Box drawing / block / powerline / braille / legacy computing**: drawn procedurally per cell like Ghostty (first
  version — font glyphs leave gaps in TUI borders and prompts, a visible regression against upstream).
- **Kitty graphics**: first version (parity with `show-image.sh`). `custom-shader`: not supported.
- **HTML overlays**: GPUI element and API modeled on `gpui-webview`; the Linux engine is WPE WebKit offscreen (same
  WebKit API as today, so the port is near-mechanical, same engine family as upstream's WKWebView, Wayland-native).
  Frames are imported zero-copy via dmabuf into a wgpu texture (video is a real use case); SHM with damage rects is the
  fallback path when dmabuf import fails on a driver.
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
  platform data raises the window (Task 3 verifies GPUI can use it).

## Technical Details

- **Pins** (`build/pins.env`): `LIBGHOSTTY_VT_REV` = the commit `libghostty-vt-sys`'s `build.rs` pins (bump together
  with the crate version, never independently). The prefix must not start with `GHOSTTY_`: `pin_lines GHOSTTY`
  (`scripts/lib/native.sh`) keys the libghostty build on every `^GHOSTTY_` line. The source is fetched and verified into
  a hash-keyed prefix and exposed at a fixed symlink (`build/vendor/ghostty-vt-src`) that `.cargo/config.toml`'s `[env]`
  names as `GHOSTTY_SOURCE_DIR` (plus `GHOSTTY_ZIG_SYSTEM_DIR` for offline Zig packages); the pinned Zig 0.16 is linked
  at a fixed path too and reaches the build script through `[env]` (or a PATH wrapper, whichever the crate's `build.rs`
  honors — checked in Task 1), so a plain `cargo build --workspace` works.
- **Resources while both builds coexist**: terminfo, themes and shell integration keep coming from the libghostty
  prefix at `GHOSTTY_REV`, exactly as `scripts/stage.sh` stages them today (`share/terminfo`, `share/ghostty/*`). Task 34
  moves them to the Ghostty source at `LIBGHOSTTY_VT_REV` (terminfo compiled with `tic`) when libghostty goes away.
- **Cargo**: `libghostty-vt = "=0.2.2"`, `gpui-kit` at an exact version with default features minus `gpui-fast`;
  `[patch.crates-io]` for `gpui-pre`/`gpui-pre-platform` only once Task 29 lands (fork under `vendor/` or a git rev).
- **Cell metrics**: advance of `M` for width; ascent + descent + line gap for height; `window-padding-x/y` around the
  grid; resize → `Terminal::resize` + PTY `TIOCSWINSZ` + cell size reported where `CELL_SIZE` was used.
- **Frame**: `begin_update` (locked) → `end_update` → per dirty row: merged background quads + text runs shaped with
  `shape_line` (row cache keyed by row content) → layers: backgrounds, selection/search highlight, kitty images below
  text, text, sprites, decorations (underline styles, strikethrough, hovered link), kitty images above text, cursor
  (block/bar/underline, hollow unfocused), IME preedit.
- **Terminal identity**: the pane's child gets `TERM=xterm-ghostty`, `TERMINFO=<stage>/share/terminfo` (the
  `xterm-ghostty` entry staged as above), `COLORTERM=truecolor`; `TERM=xterm-256color` without
  `TERMINFO` when the terminfo is not resolvable, matching today's fallback; the `term` config key overrides.
- **Config diagnostics**: `gen-ghostty-config-keys.sh` extracts every key **with its value type** from `Config.zig`;
  values of every known key are validated by type (bool, ints, floats, enums, colors, durations; complex structs
  accepted), so malformed values count as libghostty counted them, supported or not. Residual differences are listed
  against the `config-reload-counts` capability tests.
- **Keyboard**: GPUI keystroke → our keymap first (core), then ghostty keybind table, then `KeyEncoder` (legacy + kitty
  protocol per terminal modes). Physical-key copy/paste/select-all as in `ghostty-defaults.conf`.
- **Effects mapping** (replaces `GHOSTTY_ACTION_*`): title, pwd, bell, desktop notification (OSC 9/777), progress
  report, clipboard read/write (with the existing `ClipboardPromptPolicy` confirmation), child exit (PTY EOF / wait
  status), color changes (OSC 10/11/12/4) for the chrome tint.
- **HTML frames**: WPE exports dmabuf (fourcc + modifier + planes) → Vulkan import (`VK_EXT_external_memory_dma_buf`,
  `VK_EXT_image_drm_format_modifier`) → `wgpu::Texture` via wgpu-hal → GPUI element. Sync with the buffer's
  sync_file/implicit fence; release the buffer back to WPE after the frame is presented.

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

- [ ] read the three reports; if Task 2 or Task 3 failed, stop and bring the findings to the user before any
      production code (the HTML engine or the quick-terminal approach is reopened, not worked around)
- [ ] fold confirmed versions (gpui-kit, gpui-pre, libghostty-vt, WPE) and design changes into this plan
- [ ] spike code stays on its branches; reports stay in `docs/spikes/`

### Stage 1 — no GUI

### Task 5: Build wiring for libghostty-vt

**Files:**
- Modify: `build/pins.env`
- Create: `scripts/fetch-ghostty-vt-src.sh`
- Modify: `Cargo.toml` (workspace dependencies), `.cargo/config.toml` (`[env] GHOSTTY_SOURCE_DIR`)
- Create: `crates/agterm-term/tests/link.rs` (with the crate skeleton)

- [ ] add `LIBGHOSTTY_VT_REV` (the wrapper's pinned commit) and its source sum to `pins.env` (Technical Details >
      Pins: no `GHOSTTY_` prefix)
- [ ] `fetch-ghostty-vt-src.sh`: fetch the pinned Ghostty source and its Zig packages into a hash-keyed prefix, verify
      sums, reuse a matching one, point the fixed symlinks (source, Zig) at it (conventions of `build-ghostty.sh`)
- [ ] workspace dependency `libghostty-vt = "=0.2.2"`; plain `cargo build --workspace` builds offline through `[env]`
- [ ] test that bumping `LIBGHOSTTY_VT_REV` leaves the libghostty and zmx prefix keys unchanged
- [ ] test: feed VT bytes and read the screen back through the crate (proves the link)
- [ ] gates — must pass before Task 6

### Task 6: Move GTK-free app services into `agterm-app`

Both frontends use them while they coexist, so they must not be copied.

**Files:**
- Create: `crates/agterm-app/` (`hook_runner`, `process_table`, `quick_placement`, `restore_launch`, `logging`,
  `integrations` planners, `remote` presentation)
- Modify: `crates/agterm-gtk/src/{hook_runner,process_table,restore_launch,logging}.rs`, `quick/placement.rs`,
  `integrations/*` (re-export or call into `agterm-app`)

- [ ] move each module with its tests; GTK keeps only widget glue
- [ ] also move `remote.rs`'s presentation process (GTK-free apart from the network/resume observer, which stays per
      frontend)
- [ ] no behavior change: existing tests move along and pass unchanged
- [ ] gates, full conformance on GTK (`rust-passing.txt` unchanged) — must pass before Task 7

### Task 7: Ghostty config parser in core

**Files:**
- Create: `crates/agterm-core/src/ghostty_config/{mod,parse,keys,model,validate}.rs`, `ghostty_config/tests.rs`
- Create: `scripts/gen-ghostty-config-keys.sh`
- Modify: `crates/agterm-core/src/commands/config.rs` (diagnostics source abstraction, if needed)

- [ ] move `ghostty-defaults.conf` out of `agterm-gtk` into core (`include_str!` there), so both frontends load the
      same defaults
- [ ] parser: `key = value`, comments, repeatable keys, `config-file` with `?` optional includes and cycle detection,
      today's load order (defaults, user files, includes, `ghostty-settings.conf`)
- [ ] `gen-ghostty-config-keys.sh`: every key with its value type from `src/config/Config.zig` at the pinned rev into
      `keys.rs` (checked in, regenerated on a pin bump)
- [ ] typed model for the supported subset: font-family*, font-size, font-feature, theme incl. `light:,dark:`, palette,
      background/foreground, cursor-*, selection-*, background-opacity, `background-image*` (watermark), window-padding-*
      incl. `window-padding-balance`, scrollback-limit, clipboard-*/mouse-*, cursor-click-to-move,
      shell-integration-features, `term`, keybind, title; theme files through the same parser
- [ ] diagnostics per Technical Details > Config diagnostics (unknown key, malformed value of any known key; supported
      or not)
- [ ] tests: load order and overrides, includes and cycles, themes light/dark, each supported key, diagnostics counts
      (unknown key, malformed known-unsupported value, valid unsupported value = 0), the residual-difference list
- [ ] gates — must pass before Task 8

### Task 8: Ghostty keybind actions in core

**Files:**
- Create: `crates/agterm-core/src/ghostty_config/binds.rs`, `ghostty_config/binds_tests.rs`

- [ ] parse `keybind = trigger=action` (physical `key_*` and unicode triggers, sequences, `unconsumed:`/`performable:`,
      `unbind`), ghostty's built-in Linux defaults merged as ghostty does
- [ ] action table: `copy_to_clipboard`, `paste_from_clipboard`, `select_all`, `scroll_*`, `jump_to_prompt`,
      `increase/decrease/reset_font_size`, `set_font_size:N`, `clear_screen`, `start_search`/`end_search` and
      navigation, `text:`, `csi:`, `esc:` — every action core sends through `Surface::binding_action` included; unknown
      action → diagnostic
- [ ] resolution order with agterm's keymap (keymap, then ghostty binds, then encoding) documented and tested
- [ ] tests: non-Latin layouts (physical keys), performable vs not, overrides and `unbind`, each core-sent action
- [ ] gates — must pass before Task 9

### Task 9: agterm-term — pane core (VT + PTY + IO thread)

**Files:**
- Create: `crates/agterm-term/src/{lib,pane,pty,effects,render}.rs`, `tests/pane.rs`, `tests/render.rs`

- [ ] `Pane`: `Terminal` behind a mutex, `portable-pty` child, IO thread reading the PTY into `vt_write`, `on_pty_write`
      back to the PTY, bounded write queue (backpressure)
- [ ] effects → `PaneEvent` (title, pwd, bell, desktop notification, progress, clipboard read/write, color changes,
      child exited with status) over a channel the frontend drains on its main thread
- [ ] resize (cells + pixels) to VT and PTY; scrollback limit from Task 7's config
- [ ] render view in `agterm-term` types (`render.rs`): per-view `RenderState` update (two-phase, lock only for
      `begin_update`), rows with dirty flags, cells (graphemes, width, style, colors, hyperlink id), cursor (position,
      style, visibility), palette and default colors
- [ ] public API exposes only `agterm-term` types (Technical Details > Engine boundary)
- [ ] tests (real PTY): `printf` output on screen; OSC 0/2, 7, 9/777, BEL, 52 produce events; exit status; resize
      reaches the child (`stty size`); render view dirty flags after partial updates, two views of one pane, styles
      and wide cells; errors: spawn failure, PTY closed mid-write, backpressure limit
- [ ] gates — must pass before Task 10

### Task 10: agterm-term — input, selection, search, read-back

**Files:**
- Create: `crates/agterm-term/src/{input,selection,search,text,kitty}.rs`, `tests/input.rs`, `tests/selection.rs`,
  `tests/kitty.rs`

- [ ] toolkit-neutral key struct (physical key, text, modifiers, action) → `KeyEncoder` per terminal modes (legacy,
      kitty); mouse → `MouseEncoder`; focus events; bracketed paste via `paste`
- [ ] selection (cell ranges, word/line, select all) via `selection`; selected text via `fmt`
- [ ] scrollback search via `search` (matches, selected index, next/prev) — replaces the search actions
- [ ] screen/scrollback read-back via `fmt` matching what `ghostty_surface_read_text` returned; cursor column
- [ ] kitty view in `agterm-term` types (`kitty.rs`): images (id, generation, format, pixels), placements (layer, cell
      rect, source rect, pixel size), deletions
- [ ] tests: legacy and kitty encodings (arrows, F-keys, ctrl/alt), bracketed paste, selection text, search counts and
      navigation, read-back of wide chars and wrapped lines, kitty APC (chunked) → image and placement in the view
- [ ] gates — must pass before Task 11

### Task 11: agterm-term — launch, terminal identity, foreground process

**Files:**
- Create: `crates/agterm-term/src/launch.rs`, `tests/launch.rs`
- Modify: `crates/agterm-term/Cargo.toml` (dep `agterm-zmx`)

- [ ] live panes: launch from `agterm-zmx` launch inputs (`zmx attach`, shell-integration env from core)
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
- [ ] tests: `MainLoop` hop ordering and shutdown, `settle` cases carried over from GTK, SIGTERM deadline, forwarding of
      a second GPUI launch and of a GTK-build launch to the GPUI instance, and of a GPUI launch to a GTK primary
      (private bus); invalid `AGTERM_APP_ID` falls back; never two writers of `windows.json`
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
  ratchet per frontend), `conformance/agterm_conformance/impls/base.py` (lavapipe `VK_ICD_FILENAMES` for GPUI)
- Modify: `conformance/tools/ghostty_probe.py` (GPUI first-frame line)
- Create: `conformance/rust-gpui-passing.txt`

- [ ] staged GPUI build runs under the harness's weston with lavapipe; isolation unchanged
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
- [ ] toggle via keymap and control commands; focus and hide behavior as upstream
- [ ] tests (headless, fallback path); quick-terminal conformance tests green
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
- [ ] GPUI element drawing the imported texture with clipping and content mask, so overlays above it work
- [ ] SHM-with-damage fallback behind the same element when import fails
- [ ] tests: format/modifier negotiation, fallback selection, lifetime (no use after release) with a synthetic dmabuf
      (GBM/udmabuf) under Mesa where available
- [ ] gates — must pass before Task 30

### Task 30: agterm-wpe-sys — gir bindings

**Files:**
- Create: `crates/agterm-wpe-sys/` (gir output from `WPEWebKit-2.0.gir` and `WPEPlatform`, checked in, not under the
  workspace lints like `agterm-ghostty-sys`)
- Create: `scripts/gen-wpe-bindings.sh`

- [ ] generation script pinned to the WPE version from Task 4; regenerated output is reproducible
- [ ] test: link and version check against the system WPE
- [ ] gates — must pass before Task 31

### Task 31: agterm-wpe — WPE WebKit host

**Files:**
- Create: `crates/agterm-wpe/` (thread + `GMainContext`, view, frames, input, profiles), `tests/`

- [ ] dedicated WPE thread with its own `GMainContext`; commands in, frames/events out over channels
- [ ] offscreen view producing dmabuf frames (Task 29 consumer) and SHM fallback
- [ ] the WebKit surface the overlay uses: `NetworkSession` (persistent dirs), `UserContentManager`/`UserScript`,
      script message handlers, navigation/response policy, `register_uri_scheme`, `Settings`, downloads
- [ ] WPE's bubblewrap sandbox: `scripts/install-apparmor-profile.sh` attaches the profile to the `agterm-gpui` binary
      path too (and to `agterm` after Task 34)
- [ ] tests: load `data:`/custom-scheme pages headless, JS bridge round-trip, policy callbacks, profile persistence
      across view restarts
- [ ] gates — must pass before Task 32

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
- [ ] CLAUDE.md: crate map (`agterm-app`, `agterm-term`, `agterm-gpui`, `agterm-wpe*`), terminal reference rule
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
- PRs to `libghostty-rs` for any API gaps hit in Tasks 9–11, 16
- replace the installed GTK build on the machine after the switch
