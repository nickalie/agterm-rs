# Performance pass: a responsive UI at minimal resource cost

## Overview

- Goal: make the Linux build's UI as responsive as possible while spending as little CPU, as few wakeups and as
  little memory as it can.
- Source: a read-only audit of 2026-10-14 with six parallel reviews (render path, widget layer, core and control
  path, timers and I/O, input and overlays, build and memory). The key findings were re-checked against the code and
  the plan went through a review. Line numbers are as of `2d4badd`.
- What the audit found:
  - At true idle the app arms no repeating sources, libghostty wakeups are coalesced, and CSS providers reload only
    on change.
  - Responsiveness is lost in three places:
    - a full UI re-sync (`AppState::settle`) on every small event;
    - blocking work on the GTK main thread: zmx subprocesses, fsync, `glFinish`, `/proc` walks;
    - per-frame tick callbacks in a few long-lived states.
- Every change keeps upstream parity: the wire protocol, read-backs, error strings, `agtermctl` output, on-disk formats
  and the order in which a reply implies the UI applied it.
- Deliberate non-changes:
  - The 50 MB default scrollback stays, because it is upstream's default.
  - `sidebar.width` keeps saving at once, as upstream does; only the divider drag changes.
  - libghostty behavior changes only through patches whose effect is measured.
- Scope decision (user, 2026-10-14): the whole audit, GTK rework included.
  - The open plan `20261014-gpui-frontend.md` will later replace `agterm-gtk`; the GTK tasks here pay off until then.
  - The core, zmx, persistence, server and libghostty tasks carry over to the GPUI frontend.
- The first task builds a probe and records a baseline. Every later task names the probe numbers it should move, and
  the acceptance task compares them.

## Context (from discovery)

- **`settle()`** (`crates/agterm-gtk/src/state.rs:257`) runs these steps:
  - `core.commit()` and `arm_commit`;
  - `html_overlay::settle`;
  - config reload and `take_debug_appearance`;
  - `chrome::sync`, sounds and banners, `keymap_dispatch::sync`, `take_live_quit`;
  - `windows.replace_panes` and `windows.sync` (per window: deck reconcile, sidebar, titlebar, palette, search,
    switcher);
  - the menu sync, `quick.sync`, notifications;
  - `clear_pane_focus_requests`, `remotes_watcher.follow`, `settings::sync`;
  - `spawn_paced_panes` and the auto-follow, HUD, job and remote-tick timers.
- **Who calls settle:**
  - every main-loop task that changes something;
  - every control request, read-only ones included (`state.rs:567`);
  - every OSC title or cwd report, through `deck.rs:889-904` → `state.rs:845` → `Core::report_pane`
    (`commands/panes.rs:110-124`, which returns `true` unconditionally).
  - Today a `Done` reply is sent only after its settle, so a reply implies that the deck, focus, window requests and
    paced spawns are applied.
- **Main loop:** `crates/agterm-gtk/src/main_loop.rs:26` is an `async_channel` drain that runs the whole queue in one
  dispatch at DEFAULT priority. GTK layout and paint run at `GDK_PRIORITY_REDRAW` (120), so a burst starves frames.
- **zmx subprocesses:** `crates/agterm-zmx/src/client.rs:298` `run()` spawns `zmx`, starts 2–3 drain threads and
  polls `try_wait` every 5 ms. The timeout is 3 s plus a 250 ms grace. Callers on the main thread:
  - `tree`: `commands/zmx/launch.rs:162` `project_live` does a fresh `zmx list` per call, upstream's `buildTree`.
    `live_foreground_group` (`launch.rs:61-79`) and `project_foreground` (`commands/relaunch.rs:404`) can list again
    through the leader cascade.
  - Closing a pane: `commands/zmx.rs:213` `finalize_panes` → `agterm-zmx/src/inventory.rs:22` `kill_names`, which
    does `list`, a `/proc` walk, `kill`, then another `/proc` walk. Upstream also kills synchronously
    (`agtermApp.swift:367`).
  - `send.type`: `commands/typing.rs:160-180` makes two `zmx type` calls with a 10 ms `thread::sleep` between them.
  - The rest: cursor reads (`commands/surface_target.rs`), `zmx.list`/`zmx.kill`, restart polling
    (`commands/restart_live.rs`), the launch reap, and `remote/mod.rs:245` `local_tree`, a second synchronous
    `tree_of` caller.
- **Deferred replies:**
  - `Dispatched::Pending` resumes by polling with `post_after`; there is no completion hook.
  - Off-thread work posts back through `CorePoster`/`CoreTask` (`commands/process.rs:15-24`).
- **/proc walks:**
  - `crates/agterm-gtk/src/process_table.rs:58` `ProcTable::group` and `crates/agterm-zmx/src/sweep.rs:81` read every
    `/proc/*/stat`.
  - `sweep.rs:101` collects a `Vec<&str>` per record.
  - `launch.rs:71,78` read one stat file twice.
- **Persistence:**
  - `crates/agterm-core/src/persist/json.rs:176` `write_atomic` calls `sync_all`. Upstream's
    `Data.write(options: .atomic)` (`PersistenceStore.swift:53`, `SettingsStore.swift:40`) does not fsync.
  - Ratchet id `tests/protocol/test_tree.py::test_index_unsaved_while_windows_json_cannot_be_written` needs
    `tree.indexUnsaved` to be true right after a failed write, so index writes stay synchronous.
  - The reset marker (`zmx/reset.rs:496`) is read by the relaunched instance.
  - `model/store.rs:1180` `set_sidebar_width` saves at once. That is right for the `sidebar.width` command (upstream
    `AppStore+Sidebar.swift:15`) and wrong for the divider drag: `state.rs:910` `sidebar_resized` saves on every
    coalesced notify, while upstream saves once on release (`WindowContentView.swift:375-390`).
  - `persist/library.rs:541` arms a timeout per request and `state.rs:143` one per `tree.changed`; stale ones fire and
    are dropped. The debounce lives in core behind `MainLoop`, which cannot cancel a timer.
- **Frame-clock ticks:**
  - `crates/agterm-gtk/src/status_glyph.rs:228` blinks through `add_tick_callback`;
  - `crates/agterm-gtk/src/modal.rs:338` re-checks the ask anchor every frame, and `WindowAsk::sync` re-installs it
    on every settle.
- **Rendering:**
  - At `GHOSTTY_REV` 683d8db the ghostty renderer calls `gl.finish()` per frame on the GTK thread
    (`src/renderer/opengl/Frame.zig:57`).
  - Occlusion is `deck_visible && area.is_mapped()` (`surface.rs:~539-593`). A minimized or suspended window is not
    occluded, while upstream checks `window.isVisible` (`GhosttySurfaceView+Visibility.swift`).
  - `window.rs:250-400` already watches the toplevel `MINIMIZED` state (`window_state_changed`).
  - On Wayland, GDK reports `SUSPENDED` (from compositors that support it) and never `MINIMIZED`.
  - `set_deck_visible(false)` releases GPU resources at once, while upstream waits a 1 s grace.
- **Input:** `crates/agterm-gtk/src/keymap_dispatch.rs:427-460`:
  - `key_layout` and `ascii_capable` call `display.map_keycode` for each Latin-table position on every key press;
    a non-Latin group (Russian) walks all 47;
  - `function_key` lowercases `keyval.name()` for every letter.
- **libghostty:**
  - Every Zig 0.16 build has a TLS memsz of 0x411c0; the oldest vendored build has 0x11e0.
  - The cause is `std.options.signal_stack_size = 1 << 18` (zig `lib/std/Thread.zig:1734`). The library root is
    `src/main_c.zig:32` → `main.zig:19` → `main_ghostty.zig:201`.
  - `agterm-ghostty/src/surface.rs:372` `font_size()` reads `ghostty_surface_inherited_config`. Per the audit:
    - the `working_directory` in it is allocated and never freed;
    - the call takes the terminal lock.
  - Patches `build/patches/ghostty/0001`–`0004` are applied by `scripts/build-ghostty.sh:60`, and the prefix key
    hashes them.
- **heap trim:** `crates/agterm-gtk/src/heap_trim.rs` was committed in `2d4badd`. It runs `malloc_trim(0)` 2 s after a
  burst of terminal creations, first frames or teardowns.
- **Tooling:**
  - `conformance/tools/chrome_probe.py` launches the staged build under the suite's isolation (private headless
    weston and session bus, fresh root) through `agterm_conformance/impls/rust.py`. Its unit tests live in
    `conformance/selftests/` (pytest `testpaths`).
  - `window.minimize` exists in the protocol (`agterm-proto` command.rs:196).

## Development Approach

- **Testing approach**: Regular (code first, then tests), as in the rest of the project.
  - Logic that needs no widget goes into `agterm-core` and is unit-tested against the `agterm-host` fakes.
  - GTK tests run headless: `GDK_BACKEND=x11 xvfb-run cargo test -p agterm-gtk`.
- Complete each task fully before moving to the next, and make small, focused changes. Each task is sized for one
  isolated subagent run.
- **CRITICAL: every task MUST include new/updated tests** for the code it changes:
  - unit tests for new and modified functions;
  - test cases for new code paths, and updated cases where behavior changes;
  - both success and error scenarios.
- **CRITICAL: all tests must pass before starting the next task** - no exceptions.
- **CRITICAL: update this plan file when scope changes during implementation.**
- **Gate of every task** (CLAUDE.md > Gates):
  - `cargo fmt --all --check`;
  - clippy with `-D warnings`;
  - `scripts/lint-report.sh`;
  - `cargo test --workspace`;
  - then a full `conformance/tools/run-rust-linux.sh`, run in the background;
  - a ratchet id never goes red; add newly green ids with `check_ratchet.py --add`;
  - a function over a clippy complexity limit is split, not excused.
- **Probe** (Task 1) after every task that names probe numbers. Record before/after in the task's notes. A change
  that does not move its numbers is reconsidered.
- **Never touch the user's running agterm** (CLAUDE.md):
  - strip `AGTERM_*`/`ZMX_*`;
  - give every `agtermctl` call an explicit `--socket`;
  - use a private state dir;
  - run the GUI headless only;
  - reap only the instance's own `ZMX_DIR`.
- **Resources:**
  - all parallel agents and builds together use at most 10 of the 20 cores (`taskset`);
  - anything longer than ~30 s (builds, the libghostty rebuild, suite runs, probe runs) runs in the background.

## Testing Strategy

- **Unit tests**: required in every task.
  - Core-side changes are tested on the fakes: the report dedupe, debounce generations, the zmx worker's FIFO order,
    the /proc parsing, the read-only allowlist.
  - GTK-side changes get headless tests where a widget property or signal can be asserted: the blink timer state,
    occlusion through a state-injection seam, the picker model's filtering, paint interleaving under a task flood.
- **Conformance**: the protocol, cli and behaviors tests are the e2e layer, and every task ends with a full run.
  - Tasks 4, 5, 8, 9 and 12 change ordering on the main thread or on disk; their read-after-write behavior is decided
    by that run.
- **Probe** (`conformance/tools/perf_probe.py`, Task 1): a measurement, not pass/fail; outside the catalog and the
  ratchet. Its helpers are unit-tested in `conformance/selftests/test_perf_probe.py`.

## Progress Tracking

- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- update plan if implementation deviates from original scope
- fill in the Baseline table in Task 1 and the Result column in the acceptance task

## Solution Overview

1. **Settle less, settle once.**
   - Pane title and cwd reports that change nothing settle nothing.
   - `settle()` splits into a commit half that runs per task and a render half flushed once per drained batch.
   - The `Done` replies of a batch are held until that batch's flush, so "a reply implies rendered" holds as today.
   - The drain yields after a time budget through a source below redraw priority, so frames interleave with bursts.
2. **Nothing blocking on the main thread.**
   - Every zmx invocation goes through one FIFO worker thread, so their relative order stays as it is today. A close's
     kill becomes fire-and-forget.
   - `tree` with live panes answers through a deferred reply completed by `CorePoster`.
   - fsync is dropped, as upstream does. A writer thread is added only if the probe still shows a stall, and index
     writes stay synchronous.
   - The `/proc` work shrinks to the leader's children.
3. **No frame-clock ticks while idle.**
   - The blink and the ask anchor move to timers or events.
   - Terminals in minimized, suspended or hidden windows are occluded.
4. **Cheaper hot paths.**
   - A cached per-group layout table replaces the per-key-press keymap walks.
   - The picker and the palette become virtualized `ListView`s.
   - A config reload builds the base config once.
5. **Memory.**
   - A libghostty patch drops the 256 KB per-thread signal stack.
   - `mallopt` caps glibc arenas.
   - WebKit's cache model shrinks.
6. **Build.** A release profile with thin LTO, and `agtermctl` builds its clap tree once.

## Technical Details

- **Settle halves.** `settle()` keeps its name and callers.
  - **Commit half (per task):**
    - `core.commit()` + `arm_commit`;
    - `html_overlay::settle`;
    - config reload;
    - `take_debug_appearance`;
    - `take_live_quit`;
    - the sounds and banners taken from the core.
  - **Render half (per batch):**
    - `chrome::sync`, `keymap_dispatch::sync`;
    - `replace_panes`, `windows.sync`, the menu sync, `quick.sync`;
    - delivering the sounds and banners;
    - `clear_pane_focus_requests`, `remotes_watcher.follow`, `settings::sync`;
    - `spawn_paced_panes`;
    - the auto-follow, HUD, job and remote-tick timers.

    The timer and spawn steps follow the render half because they read what the windows sync applied. Sounds and
    banners queue in `AppState` until the flush. Task 3 checks each step's dependencies and records any step that
    must move.
  - `render_pending` is set by the commit half unless the request is on the read-only allowlist.
  - Allowlist: `version`, `tree`, `events.read`, and the `*.list`/`*.get`/read-back commands, named explicitly in
    `agterm-core` next to the dispatcher. A read-only request with nothing pending sends its reply at once.
- **Held replies.**
  - `handle_request` returns `Done` replies into a per-batch `Vec`.
  - After the batch's render flush, the drain sends them in order.
  - Job and presentation streams and deferred replies keep their current paths. A deferred step that completes in a
    batch is sent after that batch's flush, like `Done`.
- **Drain.**
  - Run tasks while `try_recv` yields them and less than ~4 ms has elapsed.
  - Then flush the render half, send the held replies, and yield through a one-shot idle at
    `glib::Priority::DEFAULT_IDLE` (below `GDK_PRIORITY_REDRAW`), before returning to `recv().await`.
  - The flush hook is a closure handed to `attach`, so `agterm-host` stays GTK-free.
- **Report dedupe.**
  - `Session::report_title`/`report_pwd` return `bool` (changed).
  - `Core::report_pane` returns found && changed.
  - No surface-side cache: posting a task is cheap. A surface cache would also hide a title that becomes valid after a
    `cd`, and the `LeadNotice`/`RemoteLinkNotice` titles that travel the title path.
- **zmx worker.**
  - `agterm-zmx` owns one thread and a FIFO of jobs.
  - **Invariant:** every `SessionHost` call that runs zmx goes through it. That covers `list`, `kill`, `type`,
    `screen`, restart and reset kills, the launch reap, and the `refresh_if_needed` inside `live_foreground_group`.
    So a synchronous caller (it enqueues and blocks on its reply) never overtakes an earlier fire-and-forget kill.
  - On quit and before a live-reset relaunch, the worker is drained with a bound.
  - `run()` keeps its `try_wait` poll; it no longer runs on the main thread.
- **Deferred `tree`.**
  - `begin` enqueues the listing on the worker and returns `Pending` with a result slot.
  - The worker posts completion through `CorePoster`/`CoreTask`. The projection is built at completion from the
    store as it is then, and the leader cascade reuses that listing, so there are no further lists.
  - `local_tree` (`remote/mod.rs:245`) uses the gated `LeaderCache` without a fresh listing. It serves remote
    viewers, not the wire `tree`. Task 9 confirms this against upstream and records the choice.
- **Debounce in core.**
  - Each debounced key (a window's save, a window's `tree.changed`) keeps `armed: bool` and `due: Instant`.
  - A request while armed only moves `due` forward.
  - When the timer fires before `due`, it re-arms for the remainder. A burst therefore costs one or two wakeups
    instead of K.
- **Sidebar drag.**
  - `sidebar_resized` uses a setter that updates the width without saving.
  - The drag's end (the paned's button release, or the coalesced notify settling for ~300 ms with no further change)
    saves once.
  - `set_sidebar_width` and the `sidebar.width` command are unchanged.
- **Blink.**
  - A `glib::timeout_add_local` at ~66 ms steps the opacity through a quantized curve: `blink_opacity`'s period and
    endpoints, ~12 levels. `set_opacity` with an equal value is a no-op.
  - It runs while the glyph is mapped and in a blink status, and pauses only while the toplevel is minimized or
    suspended. Inactive but visible windows keep blinking, as upstream does.
- **Occlusion.**
  - Extend `window_state_changed` to `SUSPENDED` and push `window_on_screen` to the window's deck.
  - A terminal's occlusion becomes `deck_visible && mapped && window_on_screen`.
  - Tests inject the state through a seam on `WindowUi`, because under xvfb with no window manager minimize does
    nothing.
  - Hide → release becomes a 1 s grace timer, cancelled when the terminal is shown again.
- **Layout table.**
  - It is built once per keymap state and rebuilt on the keyboard `gdk::Device`'s `changed` signal.
  - Per group it holds:
    - the level-0 chars indexed by keycode over the Latin-table range and the keypad;
    - `ascii_capable: bool`.
  - F-keys are matched by keyval range.
  - The router keeps `needs_unmodified_chords`, computed at `sync()`. While it is false, no modifier is held and the
    engine is idle, `press` skips chord resolution.
- **ListView.**
  - `gio::ListStore` + `gtk::FilterListModel` (+ `SortListModel`) + `SignalListItemFactory`.
  - Filtering goes through a `CustomFilter` over the current query.
  - Height comes from `propagate_natural_height` + `max_content_height`.
- **libghostty patches.**
  - `0005-std-options-signal-stack.patch`: `.signal_stack_size = null` in `main_ghostty.zig`'s `std_options`.
  - `0006-embedded-opengl-flush.patch`: `gl.flush()` instead of `gl.finish()` in the embedded platform's
    `Frame.complete`. Headless llvmpipe cannot show a sync bug, so the patch ships only after the real-GPU check
    (see Task 21).
  - `0007-surface-font-size.patch`, only if the leak is confirmed: a `ghostty_surface_font_size` getter, with the
    bindings regenerated by `scripts/gen-ghostty-bindings.sh`.
- **mallopt.** At the top of `main()`, before GTK or any thread starts: `M_ARENA_MAX` 4 and `M_MMAP_THRESHOLD` 512 KiB.
  The probe's RSS scenario decides which settings to keep.

## Baseline

Task 1 fills in the Baseline column and the acceptance task fills in the Result column. Probe defaults: 20 sessions,
1 window, 1280×800 headless weston.

| Scenario | Metric | Baseline | Result |
|---|---|---|---|
| idle 10 s | CPU ms, voluntary context switches/s | | |
| idle 10 s, one session in a blink status | CPU ms, ctx switches/s | | |
| idle 10 s, one GUI ask open | CPU ms, ctx switches/s | | |
| title flood (one pane, 50 titles/s) | settles/s, CPU ms, `version` p50/p99 ms | | |
| `tree` at 10/s | `tree` p50/p99 ms, `version` p99 ms | | |
| control burst (200 `session.status` sets) | frames painted during the burst, total ms | | |
| close 10 sessions | main-thread stall ms per close (`version` p99 during closes) | | |
| minimized window, output flood | CPU ms (skipped if `window.minimize` is a no-op under weston) | | |
| immediate saves (20 `session.rename`) | ms per request | | |
| startup | ms to first terminal frame | | |
| steady state | RSS MB, threads, TLS per thread | | |
| palette Themes, type 5 chars | render-flush ms per keystroke | | |

## What Goes Where

- **Implementation Steps** (`[ ]` checkboxes): code, patches, tests, the probe and docs in this repository.
- **Post-Completion** (no checkboxes):
  - measurements on real hardware (NVIDIA, a desktop session, a high-refresh monitor);
  - the go/no-go of the `glFlush` patch;
  - carrying the libghostty findings into the GPUI plan.

## Implementation Steps

### Task 1: Perf probe and baseline

**Files:**
- Create: `conformance/tools/perf_probe.py`
- Create: `conformance/selftests/test_perf_probe.py`
- Modify: `crates/agterm-gtk/src/state.rs` (timing log of `settle`)
- Modify: `docs/plans/20261014-performance-pass.md` (Baseline table)

- [ ] log a `tracing::debug!` with the elapsed time at the end of `settle()` under target `agterm::perf`. It is off at
      the default `info`; the probe sets `AGTERM_LOG=info,agterm::perf=debug`.
- [ ] write `perf_probe.py` on the `chrome_probe.py` model. Launch the staged build under the suite's isolation
      through `RustAdapter`, with an explicit `--socket` and a private state dir. Collect:
  - CPU from `/proc/<pid>/stat`;
  - wakeups from `voluntary_ctxt_switches` summed over `/proc/<pid>/task/*`;
  - RSS from `smaps_rollup`;
  - the thread count;
  - round-trip percentiles;
  - settle counts and durations from the log;
  - painted frames from a frame-clock debug log line.
- [ ] drive the Baseline table's scenarios with `agtermctl` only:
  - the title flood: `send.type` of a shell loop printing OSC 0;
  - the blink: the status command with blink;
  - the ask: `agtermctl ask` in the background;
  - the minimize: `window.minimize`.
- [ ] print a JSON report and a markdown table, with `--scenario NAME` and `--sessions N`.
- [ ] write `conformance/selftests/test_perf_probe.py`: the stat and status parsers, the percentile helper, the log
      parser.
- [ ] run the probe in the background, fill in the Baseline column, run the gate.

### Task 2: Pane title and cwd reports settle only on change

**Files:**
- Modify: `crates/agterm-core/src/model/session.rs`
- Modify: `crates/agterm-core/src/commands/panes.rs`
- Modify: `crates/agterm-gtk/src/state.rs` (only if `report_pane`'s caller changes shape)

- [ ] `report_title`/`report_pwd` return whether the stored value changed. Sanitizing and the drop of a title equal
      to the cwd come first, as today.
- [ ] `Core::report_pane` returns `false` when nothing changed, so `AppState::report_pane` skips `settle()`.
- [ ] write core tests:
  - a repeated title or pwd;
  - a title equal to the cwd, then valid after a `cd`;
  - the split pane;
  - a held session;
  - a lead or remote-link notice title still settles.
- [ ] probe the title flood: settles/s should fall to the rate of distinct titles. Run the gate.

### Task 3: Split settle into a commit half and a render half

**Files:**
- Modify: `crates/agterm-gtk/src/state.rs`

- [ ] split `settle()` into `commit_half()` and `render_half()` as in Technical Details > Settle halves.
      `settle()` = commit + render for now, so behavior does not change.
- [ ] check each moved step's dependencies:
  - sounds and banners queue until render;
  - `take_live_quit` stays in commit;
  - the timers follow the windows sync.

  Record any step that has to change halves.
- [ ] write tests: the order of steps inside one settle is unchanged (spy on the hooks); sounds and banners taken in
      commit are delivered by render.
- [ ] full conformance run (no behavior change expected), then the gate.

### Task 4: Batch flush with held replies and a read-only allowlist

**Files:**
- Modify: `crates/agterm-gtk/src/main_loop.rs`
- Modify: `crates/agterm-gtk/src/state.rs`
- Modify: `crates/agterm-core/src/dispatch.rs` (the read-only allowlist)

- [ ] add `render_pending`. Each task runs the commit half only; the drain runs the render half once when the queue is
      empty.
- [ ] hold the `Done` replies of a batch and send them in order after the batch's flush. A deferred step completing
      in the batch is held the same way.
- [ ] add the read-only allowlist. A request on it does not set `render_pending`, and with nothing pending its reply
      goes out at once.
- [ ] write tests:
  - N posted tasks give one render flush;
  - replies are sent after the flush and in order;
  - a request after a mutation in the same batch sees the rendered state;
  - an allowlisted read does not render;
  - a mutation that emits no event (split focus, zoom, sidebar state) still renders.
- [ ] run the full conformance run (read-after-write across a batch). ⚠️ any red ratchet id is fixed here.
- [ ] probe the title flood and `tree` at 10/s, then run the gate.

### Task 5: A bounded drain that lets frames through

**Files:**
- Modify: `crates/agterm-gtk/src/main_loop.rs`

- [ ] the drain stops after ~4 ms of tasks: it flushes the render half, sends the held replies, and yields through a
      one-shot idle at `Priority::DEFAULT_IDLE` before returning to `recv().await`.
- [ ] write tests:
  - a budget overrun yields and then resumes the rest of the queue in order;
  - under a flood of posted tasks the frame clock paints at least once (headless GTK test);
  - an empty queue costs no wakeups.
- [ ] probe the control burst (frames painted during it), then run the gate.

### Task 6: Blink and ask anchor off the frame clock

**Files:**
- Modify: `crates/agterm-gtk/src/status_glyph.rs`
- Modify: `crates/agterm-gtk/src/modal.rs`
- Modify: `crates/agterm-gtk/src/ask_dialog.rs`

- [ ] the blink becomes a quantized `timeout_add_local` stepper that runs while the glyph is mapped and blinking, and
      pauses only while the toplevel is minimized or suspended.
- [ ] the ask anchor is re-checked:
  - on the frame's `size_allocate`;
  - on the anchor's and the window's size notifications;
  - on the paned `position` notify;
  - after a deck reconcile.

  The per-frame tick is removed. `WindowAsk::sync` calls `set_anchor` only when the target changes.
- [ ] write tests: the timer is armed only while mapped and blinking and removed on stop; opacities come from the
      quantized set; the anchor follows a resize with no tick callback; a repeated sync does not re-install anything.
- [ ] probe idle-with-blink and idle-with-ask, then run the gate.

### Task 7: Occlusion follows window visibility; GPU release after a grace

**Files:**
- Modify: `crates/agterm-gtk/src/window.rs`
- Modify: `crates/agterm-gtk/src/deck.rs`
- Modify: `crates/agterm-gtk/src/surface.rs`

- [ ] extend `window_state_changed` to `SUSPENDED` and push `window_on_screen` to the deck. Occlusion becomes
      `deck_visible && mapped && window_on_screen` (upstream's `showsOnScreen`).
- [ ] add a state-injection seam on `WindowUi` for tests.
- [ ] `set_deck_visible(false)` arms a 1 s grace before occlusion and `release_gpu_resources`; showing the terminal
      again cancels it.
- [ ] write tests: an injected minimize or suspend occludes and a restore shows; hide then show within the grace keeps
      the resources; past the grace they are released.
- [ ] probe the minimized output flood (or record that weston ignores `window.minimize`), then run the gate.

### Task 8: One ordered zmx worker; close kills off the main thread

**Files:**
- Create: `crates/agterm-zmx/src/worker.rs`
- Modify: `crates/agterm-zmx/src/lib.rs`, `crates/agterm-zmx/src/inventory.rs`
- Modify: `crates/agterm-core/src/commands/zmx.rs` (`finalize_panes`)
- Modify: `crates/agterm-host/src/lib.rs` and its fakes (only if `SessionHost` needs an enqueue variant)
- Modify: `crates/agterm-gtk/src/state.rs` (drain on quit and before a live-reset relaunch)

- [ ] add a worker with one thread and a FIFO. Every zmx-running `SessionHost` call goes through it (the invariant
      in Technical Details). Synchronous callers enqueue and wait for their result.
- [ ] `finalize_panes` enqueues the kill batch fire-and-forget; the result is logged as today.
- [ ] drain the worker, with a bound, on quit and before a live-reset relaunch.
- [ ] write tests on the fakes:
  - FIFO order across fire-and-forget and synchronous calls;
  - a `list` after a close sees the kill applied;
  - the commit does not block on the kill;
  - the quit drain waits and its bound holds.
- [ ] probe "close 10 sessions", run the full conformance run, then the gate.

### Task 9: `tree` with live panes through the worker

**Files:**
- Modify: `crates/agterm-core/src/commands/zmx/launch.rs` (`project_live`, `live_foreground_group`)
- Modify: `crates/agterm-core/src/commands/relaunch.rs` (`project_foreground` reuses the listing)
- Modify: `crates/agterm-core/src/dispatch.rs` (`tree` begin → Pending)
- Modify: `crates/agterm-core/src/remote/mod.rs` (`local_tree`)

- [ ] `tree` with a wrapped local pane enqueues the listing and returns `Pending` with a result slot. Completion
      arrives through `CorePoster`/`CoreTask`, and the projection is built at completion.
- [ ] the leader cascade and `project_foreground` reuse that listing, with zero further lists.
- [ ] decide what `local_tree` does: the gated cache without a fresh listing. Check this against upstream and record
      the decision here.
- [ ] write tests:
  - the read-back equals today's synchronous projection on the fakes;
  - one listing per `tree`;
  - a store change between begin and completion is reflected;
  - a listing failure gives today's read-back.
- [ ] probe `tree` at 10/s, run the full conformance run, then the gate.

### Task 10: The `send.type` submit gap without sleeping on the main thread

**Files:**
- Modify: `crates/agterm-core/src/commands/typing.rs`
- Modify: `crates/agterm-gtk/src/surface.rs` (`type_text`'s sleep)

- [ ] the gap before Return becomes a `post_after` step, and the reply waits for the final Return step. The existing
      "final Return could not be confirmed" error is kept.
- [ ] `type_text`'s 10 ms sleep becomes the same kind of step.
- [ ] write tests: text then Return order, the gap length, the error path, and no `thread::sleep` on the dispatcher
      path.
- [ ] run the gate.

### Task 11: Cheaper /proc work

**Files:**
- Modify: `crates/agterm-gtk/src/process_table.rs`
- Modify: `crates/agterm-zmx/src/sweep.rs`, `crates/agterm-zmx/src/inventory.rs`
- Modify: `crates/agterm-core/src/commands/zmx/launch.rs`

- [ ] descend from a leader through `/proc/<pid>/task/<pid>/children`, falling back to the full walk when the file is
      missing.
- [ ] the kill path reuses one table snapshot between `capture` and `send`.
- [ ] `parse_record` parses without collecting.
- [ ] read the stat file once per live pane.
- [ ] write tests: parsing a comm with spaces and parentheses, the children descent, the fallback, snapshot reuse.
- [ ] run the gate.

### Task 12: Saves: no fsync, sidebar drag saves on release, coalesced debounce

**Files:**
- Modify: `crates/agterm-core/src/persist/json.rs`
- Modify: `crates/agterm-core/src/persist/library.rs`
- Modify: `crates/agterm-core/src/model/store.rs` (a no-save width setter for the drag)
- Modify: `crates/agterm-gtk/src/state.rs` (`sidebar_resized`, the `tree.changed` debounce), `crates/agterm-gtk/src/window.rs`
  (drag end)

- [ ] drop `sync_all` from `write_atomic` and keep temp-then-rename (upstream's `.atomic`). All callers stay
      synchronous, the index and the reset marker included.
- [ ] the divider drag updates the width without saving and saves once at the drag's end. `sidebar.width` and
      `set_sidebar_width` are unchanged.
- [ ] debounced saves and `tree.changed` use core's `armed` + `due` (Technical Details) instead of a timeout per
      request.
- [ ] `foundation_key_order` compares ASCII bytes case-insensitively without allocating, and keeps the Unicode
      fallback.
- [ ] probe immediate saves. ⚠️ Decision gate: a writer thread for non-index files is added (as ➕ task) only if
      saves still cost more than ~1 ms each.
- [ ] write tests:
  - `indexUnsaved` right after a failed write;
  - `sidebar.width` saves at once;
  - a drag saves once at its end;
  - a debounce burst fires once and on time;
  - the key order is unchanged on fixtures.
- [ ] full conformance run (on-disk formats), then the gate.

### Task 13: Settle tail: window, deck and sidebar

**Files:**
- Modify: `crates/agterm-gtk/src/window.rs`, `crates/agterm-gtk/src/sidebar/mod.rs`,
  `crates/agterm-gtk/src/sidebar/rows.rs`, `crates/agterm-gtk/src/sidebar/context.rs`,
  `crates/agterm-gtk/src/sidebar/footer.rs`
- Modify: `crates/agterm-gtk/src/split.rs`, `crates/agterm-gtk/src/background.rs`, `crates/agterm-gtk/src/dashboard.rs`

- [ ] `Sidebar::refresh` takes the covered (zoom/dashboard) flag and sets visibility once.
- [ ] `PaneView::apply` and `sync_lead_covers` queue an allocation only on a change. `background::sync_page` reads the
      foreground color once per settle.
- [ ] `StackLayout::report` skips hidden pages, which report when shown, and coalesces the HUD frame reports through
      one idle.
- [ ] `session_row` gets its workspace. `menu_context`/`drop_context` are built lazily. `core.settings()` is borrowed.
- [ ] guard `set_title`, the footer tooltip and the accessible value against equal values. `ChromeWidget::update`
      queues an allocation only on change.
- [ ] write tests: zoom keeps the sidebar unmapped across settles; an unchanged plan queues no allocation; a hidden
      page reports no frames.
- [ ] run the gate.

### Task 14: Settle tail: menus, popovers and the switcher

**Files:**
- Modify: `crates/agterm-gtk/src/app_menu.rs`, `crates/agterm-core/src/menu_lists.rs`
- Modify: `crates/agterm-gtk/src/titlebar.rs`, `crates/agterm-gtk/src/popovers.rs`
- Modify: `crates/agterm-gtk/src/session_switcher.rs`

- [ ] `can_reveal_active_session` no longer calls `is_dir()` on every settle; it is cached per cwd string or
      evaluated when the menu opens.
- [ ] `AppMenu::sync` memoizes titles on (context, toggles, keymap generation) and replaces only the changed items.
- [ ] the recent and attention popovers keep their rows as data and build widgets only while shown (the
      `CommandPopover::wants_rows` pattern).
- [ ] switcher candidates are computed when the Ctrl-Tab walk begins.
- [ ] write tests: an unchanged context leaves the model untouched; a toggle replaces one item; a closed popover builds
      no rows.
- [ ] run the gate.

### Task 15: Key press layout table and fast path

**Files:**
- Modify: `crates/agterm-gtk/src/keymap_dispatch.rs`

- [ ] add the cached per-group layout table, rebuilt on the keyboard device's `changed` signal. `key_layout` and
      `ascii_capable` read it.
- [ ] `function_key` matches F1–F20 by keyval range.
- [ ] add `needs_unmodified_chords` and the fast path.
- [ ] write tests:
  - the table gives the same results as today's per-key reads for US, Dvorak and Russian fixtures (extend the
    `chord_key` tests);
  - the fast path skips resolution;
  - an unmodified chord binding still matches;
  - the layout changes the table.
- [ ] run the gate.

### Task 16: Matcher, IME and keystroke activity

**Files:**
- Modify: `crates/agterm-core/src/keymap/matcher.rs`, `crates/agterm-core/src/keymap/keybind.rs`
- Modify: `crates/agterm-gtk/src/input.rs`, `crates/agterm-gtk/src/surface.rs`
- Modify: `crates/agterm-gtk/src/deck.rs`, `crates/agterm-core/src/auto_follow.rs`

- [ ] `KeybindMatcher::advance` no longer clones the pending Vec on every key. `remapped_keycode` uses a `match` or a
      sorted table. `unshifted_codepoint` reads Task 15's table.
- [ ] `filtered_by_input_method` calls `ime_point`/`set_cursor_location` only on a press, and only when the rect
      changed.
- [ ] keystroke activity: stamp a `Cell` and post at most once per ~50–100 ms, unless the pane has a status to clear.
      `rearm_on_attention_change` compares an indicator generation.
- [ ] write tests:
  - the matcher sequences (existing tests plus multi-chord);
  - IME on press only;
  - the throttle still clears a status on the first key;
  - auto-follow re-arms on an indicator change.
- [ ] run the gate.

### Task 17: Search, HUD pointer and clipboard copies

**Files:**
- Modify: `crates/agterm-gtk/src/search.rs`, `crates/agterm-gtk/src/hud.rs`, `crates/agterm-gtk/src/clipboard.rs`

- [ ] push the search needle on `search-changed` (the entry's built-in delay) or after a ~60 ms debounce.
- [ ] the HUD motion handler checks the link modifier or a held cursor before building a `LayerView`.
- [ ] paste passes the `GString` bytes without the extra copies; copy uses `glib::Bytes::from_owned`.
- [ ] write tests: a burst of typing pushes one needle; paste bytes are unchanged (multi-byte UTF-8, bracketed paste).
- [ ] run the gate.

### Task 18: Picker and palette on a virtualized ListView

**Files:**
- Modify: `crates/agterm-gtk/src/picker.rs`
- Modify: `crates/agterm-gtk/src/palette.rs`

- [ ] move both to `ListStore` + `FilterListModel` (+ `SortListModel`) + `SignalListItemFactory`. Typing updates the
      filter instead of rebuilding rows. No `measure` per keystroke.
- [ ] keep these unchanged:
  - selection and keyboard navigation;
  - activation;
  - accessible labels;
  - the palette's status glyphs (their blink follows Task 6);
  - the theme preview debounce.
- [ ] check against `docs/linux-ui-checklist.md` (the picker and palette items).
- [ ] write tests:
  - filtering and order match today's results on fixtures;
  - selection survives a refilter;
  - 1000 items build only the visible rows;
  - Enter activates the selection.
- [ ] probe the palette typing, then run the gate.

### Task 19: Config reload with one base config

**Files:**
- Modify: `crates/agterm-gtk/src/ghostty_app.rs`
- Modify: `crates/agterm-gtk/src/surface.rs`
- Modify: `crates/agterm-ghostty/src/action.rs`

- [ ] a reload reads `settings.json`, the config files and the theme once. Each terminal clones the base, loads its
      overlay and finalizes, cached by overlay text.
- [ ] one `update_config` per terminal per reload: the overlay and the font restore are merged.
- [ ] `CONFIG_CHANGE` is deep-cloned only for the app target.
- [ ] write tests: the effective config per terminal is unchanged (overlay, font override, `light:`/`dark:` theme);
      `config.reload`'s diagnostic count is unchanged.
- [ ] probe a theme switch with 20 terminals, then run the gate.

### Task 20: libghostty patch 0005: no per-thread signal stack

**Files:**
- Create: `build/patches/ghostty/0005-std-options-signal-stack.patch`

- [ ] set `.signal_stack_size = null` in `main_ghostty.zig`'s `std_options`.
- [ ] check in the ghostty source at the pin that nothing (a crash handler or segfault stack traces) relies on the
      alternate stack.
- [ ] rebuild with `scripts/build-ghostty.sh` in the background. `readelf -lW` must show a TLS memsz of ~0x11e0.
- [ ] tests: the existing surface and conformance suites stay green, plus a selftest or script check that asserts the
      staged `libghostty.so`'s TLS size stays under a threshold.
- [ ] probe the steady state (RSS, threads), then run the gate.

### Task 21: libghostty patch 0006: glFlush instead of glFinish

**Files:**
- Create: `build/patches/ghostty/0006-embedded-opengl-flush.patch`

- [ ] in the embedded OpenGL platform's `Frame.complete`, use `gl.flush()` instead of `gl.finish()`.
- [ ] check that GTK 4.14+ fences the GLArea texture it composites (`gdk_gl_texture` sync), and record the source
      reference.
- [ ] run `chrome_probe.py` and the conformance pixel checks under an output flood.
- [ ] ⚠️ go/no-go: the patch is merged only after the real-GPU check in Post-Completion (NVIDIA: no tearing or stale
      frames, CPU under the flood). Until then it stays on a branch or behind the patch-set selection, and this task
      records the outcome.
- [ ] run the gate.

### Task 22: Surface font size without a leak

**Files:**
- Create: `build/patches/ghostty/0007-surface-font-size.patch` (if the leak is confirmed)
- Modify: `crates/agterm-ghostty-sys` (regenerated), `crates/agterm-ghostty/src/surface.rs`

- [ ] confirm in the source at the pin whether `ghostty_surface_inherited_config` allocates `working_directory` and
      takes the terminal lock. If it does not, record that and stop.
- [ ] add a `ghostty_surface_font_size` getter, regenerate the bindings, and switch `Surface::font_size` to it.
- [ ] write tests: `font_size` returns the configured size, and the new size after a change.
- [ ] run the gate.

### Task 23: Eager deck creation order

**Files:**
- Modify: `crates/agterm-gtk/src/deck.rs`, `crates/agterm-gtk/src/surface.rs`

- [ ] create the shown page's terminal first, then the hidden ones one per idle or frame, keeping the existing zmx
      launch pacer.
- [ ] the zero-size creation tick is bounded or replaced by a size-notify trigger, so a 0×0 area cannot tick forever.
- [ ] write tests: the shown page is created first; a 0×0 area does not keep a tick.
- [ ] probe startup, then run the gate.

### Task 24: Allocator settings

**Files:**
- Modify: `crates/agterm-gtk/src/main.rs`
- Modify: `crates/agterm-gtk/src/heap_trim.rs`, `crates/agterm-gtk/src/surface.rs`

- [ ] call `mallopt(M_ARENA_MAX)` and `mallopt(M_MMAP_THRESHOLD)` at the top of `main()`, before GTK or any thread
      starts. Use `mallopt` rather than environment variables, which shells would inherit.
- [ ] heap trim:
  - use `timeout_add_seconds_local_once`;
  - drop the trim from `teardown_all` at quit;
  - add a trim after a GPU release (Task 7).
- [ ] measure RSS with and without each setting (probe steady state), keep only those that move it, and record the
      numbers here.
- [ ] write tests: trim requests coalesce, and a request while one is pending restarts the wait.
- [ ] run the gate.

### Task 25: HTML overlays: WebKit memory

**Files:**
- Modify: `crates/agterm-gtk/src/html_overlay/registry.rs`, `crates/agterm-gtk/src/html_overlay/panel.rs`,
  `crates/agterm-gtk/src/html_overlay/sandbox.rs`

- [ ] call `WebContext::default().set_cache_model(CacheModel::DocumentViewer)` once at scheme registration.
- [ ] replace the per-panel display providers with one shared `CssProvider` holding every panel's classes.
- [ ] move the first page's `bwrap` probe off the main thread (`gio::spawn_blocking`).
- [ ] record WebKit's load-time cost (startup ms, RSS) with and without the `html` feature. Lazy loading is a
      follow-up plan if the numbers justify it.
- [ ] write tests: the shared provider holds two panels' classes and drops one on close; the cache model is set once.
- [ ] run the gate.

### Task 26: Remote tick driven by results and due times

**Files:**
- Modify: `crates/agterm-gtk/src/state.rs`, `crates/agterm-core/src/remote/viewer.rs`,
  `crates/agterm-core/src/remote/attach.rs`

- [ ] off-thread probes post their results through the poster instead of being polled every 100 ms.
- [ ] arm the next tick at the earliest due time (`reconnects.next_due()`, `retry_at`, `STALE_AFTER`).
- [ ] write tests: the next due time is chosen; a posted result lands without a tick; backoff timing is unchanged.
- [ ] run the gate.

### Task 27: Build profile and agtermctl startup

**Files:**
- Modify: `Cargo.toml`
- Modify: `crates/agterm-ctl/src/main.rs`

- [ ] add `[profile.release]` with `lto = "thin"` and `codegen-units = 1`. Keep `panic = "unwind"`. If the release
      build time of `run-rust-linux.sh` grows by more than ~30%, move LTO into a `[profile.dist]` used for staging.
- [ ] `agtermctl` builds its clap `Command` once (`try_get_matches_from_mut` + `FromArgMatches`); help and errors stay
      byte-identical.
- [ ] write tests: the existing cli tests, plus a `--help` snapshot of two groups.
- [ ] record binary sizes before and after, then run the gate.

### Task 28: Verify acceptance criteria

- [ ] run the probe in the background and fill in the Result column. Every scenario moved, or its task records why.
- [ ] at idle (no blink, ask, remote or events subscriber) there are no periodic wakeups from the app.
- [ ] grep that no `thread::sleep`, `sync_all` or direct zmx `run()` is reachable from a main-loop task; document the
      exceptions.
- [ ] full gate: fmt, clippy `-D warnings`, `scripts/lint-report.sh`, `cargo test --workspace`, a full
      `run-rust-linux.sh` (in the background), and a ratchet that is unchanged or grown.
- [ ] re-check the `docs/linux-ui-checklist.md` items for the sidebar, picker, palette, ask, blink and search,
      headless.

### Task 29: [Final] Update documentation

- [ ] update CLAUDE.md:
  - the settle halves and held replies;
  - mutations report whether they changed anything;
  - every zmx call goes through the worker;
  - no frame-clock ticks for long-lived states;
  - the probe's command line.
- [ ] README.md: add the probe to the testing commands.
- [ ] move this plan to `docs/plans/completed/`.

## Post-Completion

*Items requiring manual intervention or external systems - informational only*

**Manual verification:**
- Run the probe on real hardware: NVIDIA, a desktop session, a 120/144 Hz monitor. Headless weston on Mesa hides
  driver costs, in particular:
  - `glFinish` versus `glFlush` (this is the go/no-go of Task 21);
  - the per-context driver threads;
  - the per-terminal RSS behind `heap_trim`.
- On Wayland, check that a minimized window reaches `SUSPENDED` in the user's compositor, so Task 7 occludes there.
- Use the app for a day with agent sessions: watch for stalls on close, title spinners and blink states (`perf top`
  on the agterm pid).

**External system updates:**
- The GPUI plan (`20261014-gpui-frontend.md`) builds libghostty-vt with the same Zig. Carry patch 0005 or the
  equivalent `std_options` into it, and keep this plan's core, zmx and persistence changes as its baseline.
