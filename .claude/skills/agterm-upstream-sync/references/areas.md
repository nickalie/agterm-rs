# Areas of an upstream diff and their Rust side

`classify-diff.py` sorts each changed upstream file into one of these areas. This file says what the area means for
the port, where its Rust counterpart lives, and how it turns into plan work. Paths on the left are upstream's (in
`.upstream/agterm`), on the right this repository's. The crate map is in `CLAUDE.md`; when this table and the code
disagree, the code wins: check with `ls`/`grep` before naming a Rust file in the plan.

## Contents

- Behavior areas: protocol, dispatcher, app-control, cli, core
- Evidence areas: Swift tests, contract docs, upstream design notes
- UI
- Vendored copies and native pins
- No-port and review
- Telling ported from not ported

## Behavior areas

| area | upstream | Rust side | what to look for |
|---|---|---|---|
| protocol | `agtermCore/Sources/agtermCore/Control*.swift`, `ControlDispatcher.swift` | `agterm-proto` (`command.rs`, `request.rs`, `result.rs`, `events.rs`, `tree.rs`), `agterm-core/src/dispatch.rs`, `projection.rs` | new or renamed wire commands, args, result fields, tree fields, events, mode changes (`ControlModes.swift`: request vs stream), options parsing (`ControlDispatcherOptions.swift`) |
| dispatcher | `ControlDispatcher+<Group>.swift` | `agterm-core/src/commands/<group>.rs` (+ `commands/tests/`) | validation order, refusal strings, defaults, read-back |
| app-control | `agterm/Control/ControlServer+<Group>.swift`, `ControlServer.swift` | `agterm-gtk` (the GTK side of the group) and `agterm-server` (socket: listen backlog, connection handling) | what the app does once the dispatcher accepted a command; socket-level changes |
| cli | `agtermCore/Sources/agtermctlKit/*` | `agterm-ctl/src/<group>.rs`, `usage.rs`, `output.rs`, `socket.rs`, `client.rs` | subcommands, flags, help text, output format, exit codes, retry/backoff, local (no-socket) commands |
| core | other `agtermCore/Sources/agtermCore/*` | `agterm-core/src/*` (model, persist, keymap, hooks, settings_model, remote, hud, palette, ...) | model and store behavior, and every on-disk format (`settings.json`, `windows.json`, keymap/hooks files, `remotes.conf`, `restore-denylist.conf`, ...): the format is part of the contract, the suite reads it back |

A behavior change usually spans several of these at once (a feature commit touches protocol + dispatcher + cli +
core + ui). The plan groups it by **command group or feature**, not by area: one task per group, holding every area
that group's change touches. The per-commit block in the classify report ("Commits by area") is the seam to cut along.

## Evidence areas

- **Swift tests** (`tests-core`, `tests-cli`, `tests-app`, `tests-ui`): the source of exact strings, edge cases,
  validation order and field order. Every plan task names the Swift test files (and the test functions the classify
  report lists) its port asserts against; the Rust unit tests assert the full upstream string on the dispatcher's
  output. `tests-ui` adds or changes accessibility-driven checks: they map to `conformance/tests/ui` and
  `ui-ids.yaml`, which the Linux build cannot run (no ui driver), so they become checklist rows.
- **Contract docs** (`site/commands.html`, `site/docs.html`, `.claude/rules/*.md`, `docs/troubleshooting.md`): the
  documented contract. `commands.html` feeds the catalog's `cli` section; a rules file states the subsystem's
  invariants. Where a doc and the Swift code disagree, read both and record the conflict in the task.
- **Upstream design notes** (`docs/plans/`, `docs/backlog/` upstream): why a feature is shaped the way it is, and
  known upstream bugs. Read them for intent; they are never ported, and an upstream backlog item is not a target.

## UI

`agterm/Views/*`, `agterm/Ghostty/*`, `agterm/*.swift` (menus, app actions, settings model), `agterm/Commands/*`,
`agterm/Notifications/*` map to `agterm-gtk` in GTK4/libadwaita idioms. The suite cannot see this UI on Linux, so
each UI change becomes a row (new or updated) in `docs/linux-ui-checklist.md` with its GTK form and manual check,
plus Rust unit tests where the logic lives in `agterm-core` (palette, keymap, settings model, link policy...).
Platform-only changes (an AppKit workaround, Finder reveal, a macOS keyboard-layout API) still need a decision:
the Linux equivalent, or a recorded "no Linux counterpart" in the checklist.

## Vendored copies and native pins

`sync-resources.sh` handles these mechanically; only its `conflict` lines become plan tasks.

| upstream | copy here | notes |
|---|---|---|
| `agterm/Resources/agent-status/` | `resources/agent-status/` | carries Linux glue (the Codex adapter reads JSON with python3/jq); a conflict there is a manual merge keeping it |
| `plugins/agterm/skills/agterm/` | `resources/skills/agterm/` | the agent skill the app installs |
| `agterm/Resources/hud/` | `crates/agterm-gtk/resources/hud/` | `hud.sh`, included with `include_str!` |
| `agterm/Resources/custom-themes/` | `build/themes/` | agterm's own themes |
| `agterm/Resources/ghostty-defaults.conf` | `crates/agterm-gtk/src/ghostty-defaults.conf` | adapted (Linux chords, comments): usually a conflict, merge the setting, keep the Linux binds |
| `scripts/zmx-patches/*.patch` (+ `ghostty/`) | `build/patches/zmx/` | upstream's own zmx patches; a change rebuilds zmx |
| `LICENSE` | `resources/LICENSE.upstream` | |
| `scripts/setup.sh` (`GHOSTTY_REV`, `ZMX_REV`) | `build/pins.env` | via `pins-delta.sh`; Zig version from each rev's `build.zig.zon`, sums from ziglang.org |

`build/patches/ghostty/*.patch` are this repository's own Linux patches (embedded OpenGL, input handoff,
resources, GPU release), not upstream's: a `GHOSTTY_REV` move must re-apply them (`scripts/build-ghostty.sh` does,
with `--fuzz=0`), and `scripts/gen-ghostty-bindings.sh` regenerates the checked-in bindings afterwards.

A new entry under `agterm/Resources/` (reported `unmapped`) has no copy yet: deciding where it goes is a plan task.

## No-port and review

`no-port` (upstream CI, packaging, release metadata, the plugin manifests' version bumps, site pages other than
commands/docs, upstream's own CLAUDE.md, other scripts) goes to the report's "no port needed" list with one line
each, so the user can see nothing was dropped silently. `review` is a path the table does not know: decide its
area by reading it, and add a rule to `classify-diff.py` when it will recur.

## Telling ported from not ported

The Rust build can already hold part of a release (a feature ported early, or a sync rerun after partial work), so
before writing a task check the Rust side:

- the new wire names, fields and CLI subcommands: `grep -rn '<name>' crates/` and `coverage-map.py <key>`;
- the new strings from the classify report: `grep -rnF '<string>' crates/` (drop the Swift interpolation);
- the Swift test's behavior against the Rust unit tests of that module.

Then apply SKILL.md > Step 3 "What gets a task": absent → port; present without the tests upstream's Swift tests
imply (Rust unit twins, a conformance id where the suite can see it) → a test-port task, which may still find the
code wrong; present and tested → no task, listed under "already in the Rust build" with file and test id. A Rust
behavior that comes from a dependency rather than our code (std's `UnixListener` already listens with the system
maximum backlog on Linux) is "already in the build" too, with that evidence.
