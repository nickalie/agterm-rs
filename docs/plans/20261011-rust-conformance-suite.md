# Conformance suite for a cross-platform Rust agterm

## Overview

- Bootstrap a black-box conformance suite that defines "full feature parity" for a future Rust
  rewrite of agterm (new repo `agterm-rs`, a local git repo at `../agterm-rs` with no GitHub remote yet).
- The Rust version shares no code with `agtermCore`, so parity can only be proven behaviorally.
  The suite runs the same tests against the Swift references and the Rust build.
- Scope is the suite only: harness, catalog, coverage gate, reference-validated tests, CI.
  The Rust stack choice (terminal engine for Windows, AccessKit GUI, a zmx replacement on Windows)
  is a separate brainstorm. The suite constrains it to two things: the same wire protocol and the same
  accessibility identifiers.
- End state: the suite is green on both Swift references and reports 0% on the empty `rust` adapter.
  That is the starting line of the Rust project.

## Context (from discovery)

- Wire protocol: one newline-terminated JSON line per request, `{cmd, target?, args?}`, answered by
  `{ok, result?, error?}` (`agtermCore/Sources/agtermCore/ControlProtocol.swift`, `ControlResponse.swift`).
  `ok:false` can still carry `result` (an `events.read` cursor failure returns the anchor in `result.events`).
- 110 commands in the `ControlProtocol.swift` enum; wire args in `ControlArgs`; read-back fields are the Codable
  properties of `ControlResult` and the tree projection (`ControlProjection.swift`); events in `ControlEvents.swift`.
- `events.read` without a cursor subscribes from now and returns no history.
- Not one-request/one-reply: `zmx.present` streams, `session.overlay.job.run` switches the connection to job frames,
  `zmx.tree`/`zmx.attach` run ssh, `debug.appearance` is refused outside UI-test mode.
  `clipboard set` and `terminfo install` are CLI-only and absent from the enum.
- `site/commands.html` carries CLI synopses and wire names per card, not wire args; read-back appears only in prose.
- UI ids: 95 SwiftUI `accessibilityIdentifier(...)` calls in 15 files plus 13 AppKit
  `setAccessibilityIdentifier(...)` calls (`session-row`, `workspace-row`, `agent-status`, ...), and
  `agterm/WelcomeAlert.swift` outside `agterm/Views`. Some are variables or two-literal ternaries.
- Scenario sources to mine: 53 upstream UI test files / 547 methods (`ControlAPIUITests.swift` alone has 91,
  plus `ControlAsk/Hud/Pick/Window/Surface*UITests`), `agterm-linux/tests/atspi_smoke.py`, and the 15 dispatcher test
  files in `agtermCore/Tests` as the source of exact error strings.
- Launch isolation:
  - macOS honours `AGTERM_CONTROL_SOCKET` (`agterm/Control/ControlServer.swift:256`).
  - The upstream XCUITest harness also sets the `AGTERM_UITEST_FORCE_SIDEBAR_VISIBLE` sentinel, which turns on the
    FB11763863 window-present workaround, welcome suppression, a quit with no prompt, and zmx disabled unless
    `AGTERM_UITEST_ENABLE_ZMX=1`.
  - The Linux fork forwards a launch to the running instance unless `AGTERM_APP_ID` is unique
    (`agterm-linux/Sources/AgtermLinux/App.swift`).
  - `atspi_smoke.py` also isolates `HOME` and `PATH`.
- `agtermctl` resolves `--socket`, then `AGTERM_STATE_DIR`, then the default (`agtermctlKit/Commands.swift`).
  A pytest run started inside a live agterm shell inherits the live `AGTERM_*`/`ZMX_*` env.
- Decode-error text differs between references: macOS `invalid request: <DecodingError context>`,
  Linux `could not decode request`.

## Development Approach

- **testing approach**: Regular. The harness tooling (client, matchers, guards, catalog generator, coverage gate)
  gets ordinary pytest self-tests under `conformance/selftests/`. Conformance tests under `conformance/tests/` are
  validated by passing on the references, not by self-tests.
- Complete each task fully before moving to the next; small focused changes.
- **CRITICAL: every task MUST include new/updated tests** — self-tests for tooling, reference runs for
  conformance tests.
- **CRITICAL: all tests must pass before starting next task.**
- **CRITICAL: a conformance test is accepted only when green on its references.** Red on a reference means
  the test is wrong. Reference disagreement becomes a capability with a reason.
- **CRITICAL: update this plan file when scope changes during implementation.**

## Testing Strategy

- **self-tests**: `pytest conformance/selftests` — host-free, every PR (ubuntu).
- **coverage gate**: `pytest conformance --coverage-gate` — no app launch, every PR.
- **reference-linux**: locally against the installed Swift/GTK fork; protocol and cli layers.
  It is the acceptance gate for every task up to Task 18.
- **reference-macos**: CI only (a local run would share the live app's `com.umputun.agterm` defaults domain),
  on a GitHub macOS runner against a released upstream build.
  Verified last (Tasks 19–21): it needs the `agterm-rs` GitHub repo and a push, which the user decides on.
  Earlier tasks write the macOS tooling and workflows with offline self-tests but never wait on a macOS run.
- Modules use the `shared_app` fixture unless a test needs a pristine instance, to keep macOS runner minutes down.
- No `sleep` anywhere: waits go through `wait_tree` / `wait_event` with timeouts.
  `--reruns 1` (`pytest-rerunfailures`) only for the ui layer, surfaced in the report.

## Progress Tracking

- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- keep plan in sync with actual work done

## Solution Overview

- A pytest plugin package `conformance/` inside the Rust repo, self-contained, with no dependency on Rust code.
- An implementation adapter launches an isolated instance and reports capabilities and parity level.
- `catalog.yaml` and `ui-ids.yaml` are generated from an upstream tag and define what "full" means.
  The coverage gate makes completeness a number.
- Behavior divergence between macOS and the Linux fork: Rust follows the platform's best reference
  (upstream on macOS, the fork on Linux, decided per case on Windows), expressed as a capability.
- Safety is an allowlist, not a denylist: the harness only ever talks to sockets under its own temp root.

## Technical Details

### Layout

```
conformance/
  pyproject.toml
  catalog.yaml
  ui-ids.yaml
  tools/gen_catalog.py
  tools/fetch_reference.py
  tools/run-reference-linux.sh
  agterm_conformance/
    plugin.py        # options, markers, env scrub, report summary, --coverage-gate
    client.py        # JSON-line socket client, temp-root allowlist
    match.py         # assert_subset, assert_absent, assert_error, ids, float tolerance
    wait.py          # wait_tree, events_baseline, wait_event
    impls/{base,swift_macos,swift_linux,rust}.py
    drivers/{base,ax}.py
    coverage.py
  selftests/
  tests/{protocol,cli,behaviors,ui}/
```

### Markers

- `since("0.36.0")`: upstream version that introduced the behavior; skipped when above the adapter's
  `parity_level`, reported as `skip(not-yet-at-level)`.
- `requires("window-geometry", reason="GTK4 cannot restore x/y")`: capability; skipped when the adapter lacks it,
  `skip(unsupported)`. The reason documents reference divergence.
- `covers("session.resize", "field:tree.session.splitRatio", "event:pane.split", "cli:session resize",
  "ui:ask-dialog", "behavior:keymap.leader-chord")`: catalog links for the coverage gate.

### Adapter contract

- `launch() -> Instance` (pid, socket path, env) under a fresh `mkdtemp` root, waits for the socket.
- `stop(instance)`: reap live-session daemons if any, SIGTERM that pid only, then check for leftover
  `zmx` / `agterm-session-host` processes under the instance's `ZMX_DIR`.
- `ctl_path`, `capabilities: set[str]`, `parity_level: str`.
  For the Rust build both come from a new optional field on the `version` response; for the Swift references
  they are static in the adapter.
- Launch env: `AGTERM_*` and `ZMX_*` scrubbed from the inherited env; then `AGTERM_STATE_DIR` and
  `AGTERM_CONTROL_SOCKET` under the temp root, isolated `HOME` (empty `.claude`, `.codex`), minimal `PATH`,
  `SHELL=/bin/sh` plus `command = /bin/sh` in `<state>/config/ghostty.conf` (mechanism confirmed in spike A),
  `<state>/windows/` to skip the welcome. Linux also gets a unique `AGTERM_APP_ID`.
- Guards (refuse to launch / connect): a socket outside the temp root; a socket path over the platform cap;
  on Linux a missing, non-unique, or default (`io.github.melonamin.agterm`) `AGTERM_APP_ID`.

### Catalog shape

```yaml
upstream: v0.36.0
commands:
  session.resize: {status: covered}
fields:
  tree.session.splitRatio: {status: covered}
events:
  pane.split: {status: covered}
cli:
  "session resize": {status: covered}
behaviors:
  restore.live-session-survives-relaunch: {requires: [live-sessions]}
exemptions:
  debug.appearance: "refused outside UI-test mode"
```

Sources:
- commands come from the enum;
- fields come from the Codable properties of `ControlResult` and the tree/window/session nodes;
- events come from `ControlEvents.swift`;
- CLI subcommands come from the `commands.html` synopsis lines.

Per-command wire args are not cataloged.
Merge rules: generated sections are replaced. `behaviors` and `exemptions` are preserved.
New items are `uncovered`, and vanished items are `removed-upstream`.
`debug.appearance` is auto-exempted. Streaming, ssh and job-frame commands are decided in Task 7.

### Comparison rules

- ids are opaque: captured from responses and reused, never format-checked.
- `assert_subset` ignores extra fields; `assert_absent` pins nil omission.
- floats with tolerance; list order only where semantic.
- dispatcher errors: `ok == false`, exact `error` text, `result` allowed. Decode errors: `ok == false` only, with a
  `requires` reason documenting the divergent text.
- CLI human output matches exactly after normalizing ids and paths; event arguments must appear in it.
- terminal content via `session.text` after `session type`, waiting for a marker; such tests require `posix-shell`.

## What Goes Where

- **Implementation Steps**: everything inside the local `agterm-rs` repo (`../agterm-rs`): `conformance/` and `.github/`.
  The plan's run copy lives in `agterm-rs/docs/plans/` and returns to agterm-linux in the final task.
- **Post-Completion**: the `parity-catch-up` skill, the Rust stack brainstorm, `atspi`/`uia` drivers and Rust CI
  jobs once a Rust build exists.

## Implementation Steps

### Task 1: Create the repo and the plugin skeleton

**Files:**
- Create: `conformance/pyproject.toml`
- Create: `conformance/agterm_conformance/plugin.py`
- Create: `conformance/selftests/test_plugin_options.py`

- [x] in the existing local `agterm-rs` repo (no remote; a GitHub repo is the user's call), make `conformance/` a pip-installable package registering a pytest plugin entry point; declare `pytest-rerunfailures`
- [x] add options `--impl {swift-macos,swift-linux,rust}`, `--bin`, `--ctl`, `--coverage-gate`
- [x] register markers `since`, `requires`, `covers`
- [x] scrub `AGTERM_*` and `ZMX_*` from `os.environ` at plugin load so no subprocess inherits a live session
- [x] write self-tests for option parsing, marker registration and the env scrub (pytester)
- [x] write self-tests for missing/invalid `--impl` and missing `--bin`
- [x] run `pytest conformance/selftests` — must pass before task 2

### Task 2: Socket client and matchers

**Files:**
- Create: `conformance/agterm_conformance/client.py`
- Create: `conformance/agterm_conformance/match.py`
- Create: `conformance/selftests/test_client.py`
- Create: `conformance/selftests/test_match.py`

- [x] JSON-line client (`request(cmd, target=None, **args) -> Response`) with timeouts and the shared line cap; refuses sockets outside the session temp root
- [x] `assert_subset`, `assert_absent`, `assert_error(resp, text=None)` allowing `result` on `ok:false`, float tolerance, opt-in order-insensitive lists
- [x] id capture helpers pulling session/window/workspace ids out of `tree`
- [x] write self-tests against an in-process fake Unix-socket server (success, `ok:false` with result, oversized line, timeout, socket outside the root refused)
- [x] write self-tests for matchers (match, extra fields, missing field, present-but-should-be-absent, float edge)
- [x] run self-tests — must pass before task 3

### Task 3: Implementation adapters and launch guards

**Files:**
- Create: `conformance/agterm_conformance/impls/base.py`
- Create: `conformance/agterm_conformance/impls/swift_macos.py`
- Create: `conformance/agterm_conformance/impls/swift_linux.py`
- Create: `conformance/agterm_conformance/impls/rust.py`
- Create: `conformance/selftests/test_impls.py`

- [x] base adapter: temp root, launch env per the Adapter contract, socket wait, SIGTERM own pid, leftover-process check
- [x] `swift_macos`: direct exec of `agterm.app/Contents/MacOS/agterm`; the sentinel choice stays open until spike A
- [x] `swift_linux`: launch env mirrored from `atspi_smoke.py` (`AGTERM_RESOURCE_ROOT`, unique `AGTERM_APP_ID`, isolated `HOME`, `PATH`)
- [x] `rust`: reads capabilities/parity level from `version`; with no binary it reports nothing and every test skips as not-yet-at-level
- [x] static capability sets and `parity_level` for both Swift references
- [x] write self-tests for the guards: socket outside the root, overlong path, missing/default/duplicate `AGTERM_APP_ID`, inherited `AGTERM_*` absent from the launch env
- [x] write self-tests for capability/level resolution
- [x] run self-tests — must pass before task 4

### Task 4: Fixtures, waits and the report

**Files:**
- Create: `conformance/agterm_conformance/wait.py`
- Modify: `conformance/agterm_conformance/plugin.py`
- Create: `conformance/selftests/test_fixtures.py`

- [ ] fixtures `app` (per test), `shared_app` (per module), `ctl`, `cli` (always passes `--socket` of its instance)
- [ ] `wait_tree(predicate, timeout)`; `events_baseline()` returning a cursor and `wait_event(cursor, kind, match, timeout)` so an event emitted before the wait starts is not missed
- [ ] skip logic for `since` / `requires` with distinct reasons; terminal summary: pass, fail, skip(unsupported), skip(not-yet-at-level), reruns
- [ ] write self-tests with pytester and a fake adapter (skip reasons, summary counts, wait timeout message)
- [ ] write a self-test for the baseline race: event emitted between action and wait is still observed
- [ ] run self-tests — must pass before task 5

### Task 5: Spike A tooling and the Linux launch confirmation

**Files:**
- Create: `conformance/tools/fetch_reference.py`
- Create: `.github/workflows/spike-macos.yml`
- Create: `conformance/docs/spike-macos.md`

- [ ] `fetch_reference.py --tag vX.Y.Z`: download the signed upstream release, mount/extract, cache by tag, print bundle and CLI paths
- [ ] spike workflow (written now, run in Task 19): fetch, launch via the adapter, `version` and `tree` over the socket, `session type` + `session.text` round-trip in `/bin/sh`
- [ ] interim macOS adapter choice: the `AGTERM_UITEST_FORCE_SIDEBAR_VISIBLE` sentinel (window present, no welcome, no quit prompt), zmx via `AGTERM_UITEST_ENABLE_ZMX=1`, plain exec selectable by an adapter option; Task 19 confirms or flips it
- [ ] confirm the `/bin/sh` shell mechanism on reference-linux (macOS in Task 19)
- [ ] record the interim choice and the open macOS questions in `spike-macos.md`
- [ ] write self-tests for `fetch_reference.py` asset selection and cache logic (offline, fixture release JSON)
- [ ] run self-tests; Linux `/bin/sh` round-trip green — must pass before task 6

### Task 6: CI workflows and the local reference-linux runner

**Files:**
- Create: `.github/workflows/reference-macos.yml`
- Create: `.github/workflows/conformance-gate.yml`
- Create: `conformance/tools/run-reference-linux.sh`

- [ ] macOS job when `conformance/**` changes: fetch the reference at `catalog.upstream`, run `tests/protocol` and `tests/cli`, upload the report
- [ ] ubuntu job on every PR: self-tests plus coverage gate (gate starts enforcing after Task 9)
- [ ] local `run-reference-linux.sh` for the installed fork, producing a report to attach to PRs
- [ ] one trivial protocol test (`version`) green via `run-reference-linux.sh`; the workflows' first run on a PR happens in Task 19
- [ ] run self-tests — must pass before task 7

### Task 7: Catalog generator — commands, fields, events, CLI

**Files:**
- Create: `conformance/tools/gen_catalog.py`
- Create: `conformance/catalog.yaml`
- Create: `conformance/selftests/test_gen_catalog.py`
- Create: `conformance/selftests/fixtures/upstream-mini/`

- [ ] commands from the enum (all 110, including implicit raw values); fields from the Codable properties of `ControlResult` and the tree/window/session nodes; events from `ControlEvents.swift`; CLI subcommands from `commands.html` synopses
- [ ] auto-exempt `debug.appearance`; decide and record exemption vs a stream mode for `zmx.present`, `session.overlay.job.run`, `zmx.tree`, `zmx.attach`
- [ ] merge into the existing catalog: replace generated sections, keep `behaviors` / `exemptions`, mark `uncovered` / `removed-upstream`
- [ ] generate the initial catalog for the current upstream tag and review the diff by hand
- [ ] write self-tests against a trimmed upstream fixture tree (parse, implicit raw values, merge preserving manual sections, removed items, auto-exemption, unparseable input reported instead of dropped)
- [ ] run self-tests — must pass before task 8

### Task 8: UI identifier extraction

**Files:**
- Modify: `conformance/tools/gen_catalog.py`
- Create: `conformance/ui-ids.yaml`
- Modify: `conformance/selftests/test_gen_catalog.py`

- [ ] scan all of upstream `agterm/**/*.swift` for `accessibilityIdentifier(...)` and `setAccessibilityIdentifier(...)`
- [ ] interpolations become templates (`ask-button-{choice}`); two-literal ternaries expand to both ids; bare variables are reported for manual entry
- [ ] generate `ui-ids.yaml`, add the manual entries, review by hand
- [ ] write self-tests for literal, interpolated, ternary, AppKit-setter and unparseable identifiers
- [ ] run self-tests — must pass before task 9

### Task 9: Coverage gate

**Files:**
- Create: `conformance/agterm_conformance/coverage.py`
- Modify: `conformance/agterm_conformance/plugin.py`
- Create: `conformance/selftests/test_coverage.py`

- [ ] collect `covers(...)` from all tests without launching anything; compare with commands, fields, events, CLI subcommands, behaviors and ui ids
- [ ] fail listing uncovered items; flag `covers` naming items absent from the catalog; print coverage percentage per section
- [ ] exemptions require a non-empty reason
- [ ] write self-tests (full coverage, uncovered item, unknown covers target, exemption without reason)
- [ ] run self-tests — must pass before task 10

### Task 10: Protocol foundation — version, tree, addressing, events

**Files:**
- Create: `conformance/tests/protocol/test_version.py`
- Create: `conformance/tests/protocol/test_tree.py`
- Create: `conformance/tests/protocol/test_addressing.py`
- Create: `conformance/tests/protocol/test_events.py`

- [ ] `version`, `tree` shape (top level, window, workspace, session nodes), nil omission on a fresh instance
- [ ] target resolution (explicit id, current, invalid target error text)
- [ ] `events.read` cursor semantics, the failure anchor in `result`, core lifecycle events
- [ ] unknown command and malformed request: `ok:false` only, divergence reason recorded
- [ ] run on reference-linux — green before task 11 (reference-macos in Task 19)

### Task 11: Protocol — workspace commands

**Files:**
- Create: `conformance/tests/protocol/test_workspace.py`

- [ ] one test per workspace command: effect, read-back in `tree`, emitted events
- [ ] error cases with exact dispatcher error text (from the `agtermCore/Tests` dispatcher tests)
- [ ] run on reference-linux — green before task 12 (reference-macos in Task 19)

### Task 12: Protocol — session and pane commands

**Files:**
- Create: `conformance/tests/protocol/test_session.py`
- Create: `conformance/tests/protocol/test_split.py`
- Create: `conformance/tests/protocol/test_session_text.py`

- [ ] session lifecycle, rename, select, reorder, status/flag/unseen, restore pins
- [ ] split, focus, `session.resize` (`result.ratio`, `splitRatio`), zoom with read-back
- [ ] `session type` / `session.text` round-trip with marker waits (`requires("posix-shell")`)
- [ ] error cases with exact error text
- [ ] run on reference-linux — green before task 13 (reference-macos in Task 19)

### Task 13: Protocol — overlay, HUD, ask, pick

**Files:**
- Create: `conformance/tests/protocol/test_overlay.py`
- Create: `conformance/tests/protocol/test_hud.py`
- Create: `conformance/tests/protocol/test_ask_pick.py`

- [ ] overlay program/HTML/HUD occupancy and read-back (`overlayActive`, `coverOverlayActive`, `programOverlayActive`)
- [ ] HUD sticky/frameless/size and self-clearing; ask and pick answered over the protocol
- [ ] error cases with exact error text
- [ ] run on reference-linux — green before task 14 (reference-macos in Task 19)

### Task 14: Protocol — window, dashboard, quick terminal

**Files:**
- Create: `conformance/tests/protocol/test_window.py`
- Create: `conformance/tests/protocol/test_dashboard_quick.py`

- [ ] window list/new/close/geometry/fullscreen/zoom/minimize with read-back; `window-geometry` capability with reason on Linux
- [ ] dashboard and quick-terminal commands with read-back
- [ ] run on reference-linux — green before task 15 (reference-macos in Task 19)

### Task 15: Protocol — sidebar, notify, font, theme

**Files:**
- Create: `conformance/tests/protocol/test_sidebar.py`
- Create: `conformance/tests/protocol/test_notify.py`
- Create: `conformance/tests/protocol/test_font_theme.py`

- [ ] sidebar state/mode, notifications and status, `font.inc`/`font.dec`/`font.reset`, theme commands, each with read-back
- [ ] run on reference-linux — green before task 16 (reference-macos in Task 19)

### Task 16: Protocol — keymap, hooks, config, browser, surface

**Files:**
- Create: `conformance/tests/protocol/test_keymap_hooks.py`
- Create: `conformance/tests/protocol/test_config_browser.py`
- Create: `conformance/tests/protocol/test_surface.py`

- [ ] keymap reload/edit, event hooks, agterm-scoped config, browser/HTML pages, surface commands
- [ ] items not black-box testable move to `exemptions` with a reason
- [ ] run on reference-linux — green before task 17 (reference-macos in Task 19)

### Task 17: Protocol — restore and live sessions

**Files:**
- Create: `conformance/tests/protocol/test_restore.py`
- Create: `conformance/tests/protocol/test_zmx.py`

- [ ] seed `restoreMode: live` in `settings.json` for `requires("live-sessions")` tests; on macOS via the interim zmx flag from Task 5
- [ ] restore pins and live-session lifecycle; reap with `zmx.kill`/`zmx.reset` before stop
- [ ] leftover-process check finds no `zmx` / `agterm-session-host` under the instance's `ZMX_DIR`
- [ ] coverage gate reports no uncovered command, field or event
- [ ] run on reference-linux — green before task 18 (reference-macos in Task 19)

### Task 18: Behaviors and CLI tests

**Files:**
- Modify: `conformance/catalog.yaml`
- Create: `conformance/tests/behaviors/test_relaunch.py`
- Create: `conformance/tests/behaviors/test_keymap.py`
- Create: `conformance/tests/cli/test_cli_output.py`
- Create: `conformance/tests/cli/test_socket_resolution.py`
- Create: `conformance/tests/cli/test_cli_only.py`

- [ ] seed `behaviors` with what has no own command (relaunch restore, keymap reload semantics, hook firing) and cover each
- [ ] CLI: exit codes, `--json` shape, exact human output after id/path normalization, event arguments visible in human output
- [ ] CLI-only `clipboard set` and `terminfo install` inside the isolated `HOME`
- [ ] socket resolution: `--socket` and `AGTERM_STATE_DIR` on temp dirs; the default branch only with `HOME`/XDG (`CFFIXED_USER_HOME` on macOS) overridden and after asserting the resolved default lies under the temp root, otherwise skip
- [ ] run on reference-linux — green before task 19 (reference-macos in Task 19)

### Task 19: macOS verification — spike A and protocol/cli on reference-macos

⚠️ Needs the `agterm-rs` GitHub repo and a push; the user decides when. Everything before this task is Linux-only.

**Files:**
- Modify: `conformance/docs/spike-macos.md`
- Modify: `conformance/agterm_conformance/impls/swift_macos.py`
- Modify: `.github/workflows/spike-macos.yml`, `.github/workflows/reference-macos.yml`

- [ ] run spike A on the hosted runner: fetch, launch, `version`/`tree`, `/bin/sh` round-trip
- [ ] confirm or flip the interim sentinel choice from Task 5; confirm the `/bin/sh` mechanism on macOS; record outcomes in `spike-macos.md`
- [ ] both workflows green on a PR (`conformance-gate` on ubuntu, `reference-macos` on macOS)
- [ ] protocol and cli layers (Tasks 10–18) green on reference-macos; a red test is fixed or becomes a capability with a reason
- [ ] run self-tests — must pass before task 20

### Task 20: Spike B and the macOS AX driver

**Files:**
- Create: `conformance/agterm_conformance/drivers/base.py`
- Create: `conformance/agterm_conformance/drivers/ax.py`
- Modify: `.github/workflows/spike-macos.yml`
- Modify: `conformance/docs/spike-macos.md`

- [ ] spike B on the hosted runner: read `AXIdentifier` of `add-session`, `AXPress` it, post one synthesized keystroke (CGEvent, PostEvent TCC); try granting accessibility in the workflow if denied
- [ ] record the outcome; if pyobjc AX is not viable, switch the plan to a thin XCUITest bridge before task 21
- [ ] driver interface `find(id, **attrs)`, `click`, `press(keys)`, `text`; `ax` implementation
- [ ] write self-tests for driver selection and id template expansion
- [ ] run self-tests; spike B green — must pass before task 21

### Task 21: UI tests on reference-macos

**Files:**
- Create: `conformance/tests/ui/`
- Modify: `.github/workflows/reference-macos.yml`

- [ ] UI tests addressed only by `ui-ids.yaml` ids: role, name, state; no tree structure assertions
- [ ] cover the ids or exempt them with a reason; coverage gate clean for `ui:` items
- [ ] add the ui layer to the macOS workflow with `--reruns 1`
- [ ] run the ui layer on reference-macos — green before task 22

### Task 22: Verify acceptance criteria

- [ ] suite green on reference-macos (protocol, cli, ui) and reference-linux (protocol, cli)
- [ ] coverage gate clean: every command, field, event, CLI subcommand, behavior and ui id covered or exempted with a reason
- [ ] `--impl=rust` with no binary: everything skipped as not-yet-at-level, parity reported as 0%
- [ ] no test uses `sleep`; no connection outside the temp root is possible; no leftover processes after a full run
- [ ] run `pytest conformance/selftests` and `pytest conformance --coverage-gate`

### Task 23: [Final] Update documentation

- [ ] `conformance/README.md`: running against each implementation, markers, catalog regeneration, the reference discipline, safety guards
- [ ] record the capability vocabulary and the `version` response extension the Rust build must implement
- [ ] move this plan's run copy from `agterm-rs/docs/plans/` to `docs/plans/completed/` in agterm-linux, replacing the original

## Post-Completion

**Follow-up work:**
- `parity-catch-up` skill for the Rust repo: regenerate the catalog at a new upstream tag → write tests green on
  the references → implement in Rust → confirmation gate for any capability-skip of macOS-specific behavior →
  raise `parity_level` and release.
- Separate brainstorm for the Rust stack: terminal engine that runs on Windows, a GUI toolkit exposing
  accessibility ids on all three OSes (verify AccessKit `author_id` → AXIdentifier / AutomationId / AT-SPI
  accessible-id), and the Windows replacement for zmx live sessions.
- `atspi` and `uia` drivers against a real Rust build; `rust-linux`, `rust-macos`, `rust-windows` CI jobs.

**Manual verification:**
- reference-linux runs happen on the maintainer's machine against the locally installed fork; review the report
  before trusting a new test.
