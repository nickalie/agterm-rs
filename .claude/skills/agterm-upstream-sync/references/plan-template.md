# Plan template

The plan goes to `docs/plans/<YYYYMMDD>-upstream-sync-<new tag>.md` and is run by `/plan-exec-revmux`, which reads
the `## Overview` and `## Context` sections, then executes each `### Task N:` section in a fresh subagent: every
`- [ ]` item under a task is that subagent's work list, it ticks them, runs the validation the task names and
commits. So each task must be self-contained (a subagent sees only the plan file and the repo), name exact files
and commands, and end with its own gate. Write it in English, like the repository's other plans.

Everything in `<...>` is filled in; drop sections that would stay empty except Safety rules and the last task.

````markdown
# Upstream sync <old tag> -> <new tag>

## Overview

- Bring agterm-rs from upstream `umputun/agterm` <old tag> to <new tag> (<N> commits, releases <list>). The
  mechanical part is done on branch `upstream-sync-<new tag>` (commit `<sha>`): catalog and ui-ids regenerated,
  upstream's pins <moved: what | unchanged>, vendored copies <synced | conflicts: which>, parity-target mentions bumped.
- This plan ports the behavior: <one line per task group>.
- Out of scope: <the no-port list, one line, or "upstream CI, packaging and release metadata">.

## Context (from the sync)

- Upstream range: `.upstream/agterm` at <new tag> (`<sha>`); diff `git -C .upstream/agterm diff <old>..<new>`.
  Changelog of the range: <summary, a bullet per feature/fix with its PR number>.
- Reference rule (CLAUDE.md): upstream is the reference for protocol, semantics, error strings, `agtermctl`,
  on-disk formats, model and UI; the fork `../agterm-linux` only for Linux glue upstream lacks.
- Baseline after the mechanical commit:
  - `cargo fmt --check`: <ok | ...>; clippy: <ok | ...>; `scripts/lint-report.sh`: <ok | ...>
  - `cargo test --workspace` (under xvfb): <ok | the failing tests, each with the task that turns it green>
  - coverage gate: <ok | the uncovered keys, each with its task>
  - `run-rust-linux.sh` (full): ratchet <holds | breaks: ids and cause>; newly green: <ids added | none>
- Already in the Rust build (no task): <feature — evidence: file / ratchet id>, or "nothing".
- Not run on this host: <the ratchet's `not run on this host` ids and why (no zsh login shell)>, or "none".
- Upstream's pins (none is a pin here): ZMX_REV and zmx-patches <unchanged | a -> b and the changed patches, a task
  ports them into agterm-session>; GHOSTTY_REV <unchanged | a -> b, a task reviews it against LIBGHOSTTY_VT_REV>.

## Safety rules

<the block between the markers in references/safety.md, verbatim>

## Development Approach

- Each task follows CLAUDE.md > "Porting a command group": read upstream's implementation, its Swift tests (exact
  strings, edge cases, field order), `site/commands.html` and the matching `.claude/rules/*.md`; implement in
  `agterm-core` against the `agterm-host` fakes with unit tests asserting upstream's full error string; wire the
  app side (`agterm-gpui`); add the `agtermctl` part from `agtermctlKit`; claim a capability only once its commands implement it.
- New conformance tests follow conformance/README.md > Reference discipline: a test is accepted only when green on
  its references. reference-macos runs only in CI on macOS (`reference-macos.yml`), so a task that adds a suite test
  marks "green on reference-macos" as a manual check for the user, and runs `run-reference-linux.sh` locally when
  the installed fork has the feature.
- Gate of every task: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`,
  `scripts/lint-report.sh`, `GDK_BACKEND=x11 xvfb-run -a cargo test --workspace`,
  `.venv/bin/python -m pytest conformance --coverage-gate` (until the task that closes the last uncovered key, only
  the keys listed for later tasks may stay uncovered), then a full `conformance/tools/run-rust-linux.sh` in the
  background with the ratchet holding, and the newly green ids added with
  `.venv/bin/python conformance/tools/check_ratchet.py --report conformance/reports/rust-linux/nodeids.jsonl --add`.
- A ratchet id never goes red; no reruns; a function over a `clippy.toml` limit is split, not excused.

## Testing Strategy

- unit: `cargo test --workspace` on the `agterm-host` fakes; the app's tests under `xvfb-run`.
- conformance: `run-rust-linux.sh` (protocol, cli, behaviors); `tests/ui` skips on Linux.

## Progress Tracking

- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- update plan if implementation deviates from original scope

## Implementation Steps

### Task 1: <group or feature> (<upstream PR numbers>)

**Upstream:**
- implementation: `<agtermCore/Sources/agtermCore/ControlDispatcher+X.swift>`, `<agterm/Control/ControlServer+X.swift>`,
  `<agtermCore/Sources/agtermctlKit/XCommands.swift>`, `<core files>`
- tests: `<agtermCore/Tests/agtermCoreTests/XTests.swift>` (`<testFunction>`, ...), `<agtermTests/...>`
- contract: `site/commands.html` (<the command's section>), `.claude/rules/<x>.md`

**Rust side:** `<crates/...>` (create/modify)

**Conformance:**
- existing ids that cover it (from `coverage-map.py`): `<tests/protocol/test_x.py::test_y>` (<listed | not listed>)
- new tests to add: `<tests/protocol/test_x.py::test_new_behavior>` covering `<catalog key>`
- expected newly green: <ids>

- [ ] <behavior 1, with the exact upstream string it must produce>
- [ ] <behavior 2>
- [ ] write tests: Rust unit tests on the fakes asserting upstream's full strings (<cases from the Swift tests>)
- [ ] write tests: conformance <new test ids> with `covers(<keys>)`
- [ ] manual check (user): the new conformance tests are green on reference-macos (`reference-macos.yml` on a pushed
      branch)
- [ ] gate (Development Approach) passes; newly green ids added with `check_ratchet.py --add`

### Task <N-1>: UI-only changes

- [ ] <one item per UI change: the GPUI form, or "no Linux counterpart" with the reason>
- [ ] gate passes

### Task <N>: Claim <new tag> parity and update the docs

- [ ] bump `PARITY_LEVEL` in `crates/agterm-proto/src/version.rs` and its mirrors (`tag-mentions.sh <old>` class
      `claim`: the version.rs test, `agterm-core/src/dispatch.rs` and `agterm-server/tests/server.rs` expectations,
      `conformance/agterm_conformance/impls/rust.py`, `conformance/docs/rust-build.md`), only once every task above is
      done
- [ ] `conformance/docs/fork-bugs.md` citations re-checked against <new tag> (bump the tag only where the cited
      contract still says the same)
- [ ] README.md, CLAUDE.md, conformance/README.md, conformance/docs/rust-build.md describe what changed (new
      capabilities, commands, on-disk files); `tag-mentions.sh <old>` lists no `review` or stale `claim` line
- [ ] gate passes
````

## Writing the tasks

- Order: protocol first when the release adds wire names, because `agterm-proto/tests/catalog.rs` and
  `agterm-ctl`'s `the_command_tree_is_the_catalogs_cli` compare the code with the regenerated catalog: until the
  new names exist those cargo tests are red for every later task. The first task adds every new `Command`
  variant, field, event and CLI subcommand skeleton (answering the same refusal the dispatcher gives any command it
  does not yet implement), so each later task starts from a green `cargo test`; when those tests are already green
  at the baseline, there is no such task. Then one task per command group or
  feature, then vendored-copy conflicts and native pin work, then the UI-only task, then the claim-and-docs
  task, always last.
- One task is one reviewable change: a group with a large diff splits by behavior (validation, read-back, GUI).
- Every task names its upstream files and Swift tests by path, so the task subagent reads the right sources without
  re-running the analysis, and lists the conformance ids it expects to turn green.
- Facts in the plan come from this run's commands (classify report, coverage-map, gate logs), not from memory.
