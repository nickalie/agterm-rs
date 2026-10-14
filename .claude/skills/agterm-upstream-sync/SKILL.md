---
name: agterm-upstream-sync
description: "Bring agterm-rs up to a new release of upstream umputun/agterm, its parity target: resolve the current and target tags, sort the upstream diff by area (protocol and dispatcher by command group, ControlServer, agtermctlKit, Swift tests, UI, contract docs, vendored resources, native pins, no-port), do the mechanical bump on branch upstream-sync-TAG (catalog, pins.env with verified Zig sums, patches, vendored copies, tag mentions), run the gates, and write a docs/plans/ plan for /plan-exec-revmux with one task per affected command group. It ports no features itself. Use it in agterm-rs whenever the user mentions a new upstream agterm release or tag, syncing or catching up with upstream, bumping the parity target, or what changed upstream since the current tag: 'sync with upstream', 'upstream released v0.37', 'синхронизируй с апстримом', 'вышел новый релиз agterm', 'перенеси изменения нового релиза upstream', 'что изменилось в upstream'."
allowed-tools: Read, Write, Edit, Glob, Grep, Bash, Agent
---

# agterm-upstream-sync

Moves agterm-rs from the upstream release it targets to a newer one, in three parts: the **analysis** (what changed
upstream, by area, and what of it the Rust build already has), the **mechanical bump** on a branch (everything that
needs no porting judgment), and a **plan** for the porting, which `/plan-exec-revmux` executes. The skill ports no
behavior itself: a feature port needs the per-task implement → test → revmux review loop that plan-exec-revmux
runs, and doing it here would skip that review.

`SKILL_DIR` is this skill's directory (usually `<repo>/.claude/skills/agterm-upstream-sync`). Scripts are in
`SKILL_DIR/scripts/`, references in `SKILL_DIR/references/`. Read a reference when its step says so. The scripts act
on the repository of the current directory (`git rev-parse --show-toplevel`), not on the one holding `SKILL_DIR`, so
run them from the checkout being synced; in a git worktree that is the worktree.

## Hard rules

`references/safety.md` holds them between `BEGIN HARD RULES` / `END HARD RULES`. Read the file now and keep to it for
every command in this run. In short: never touch the user's running agterm (strip `AGTERM_*`/`ZMX_*`, explicit
`--socket` on every `agtermctl`, private state dir and `ZMX_DIR`, GUI only headless, never `pkill zmx`);
`.upstream/agterm` is read-only except fetching and checking out the new tag; never push, tag or release; never edit
`docs/plans/completed/`; anything over ~30 s runs in the background.

**Paste the block verbatim into every subagent prompt you send and into the plan's "Safety rules" section.**
Subagents and the plan's task subagents do not see this skill or the user's memory; a rule that is not in their
prompt does not exist for them, and a subagent that reached the user's live agterm is how these rules came about.

## Arguments

- `$ARGUMENTS`: optional target tag (`v0.37.0`); default the latest published release.
- `--upstream DIR`: an upstream clone other than `.upstream/agterm` (an evaluation copy). The scripts that read
  the clone take it: `prepare-upstream.sh`, `classify-diff.py`, `pins-delta.sh`, `sync-resources.sh`.

`WORK` is a scratch dir for the classify report, analyses, gate logs and saved patches: `<session scratchpad>/
upstream-sync-<new tag>` when the session has a scratchpad directory, else `/tmp/agterm-upstream-sync-<new tag>`.

## Step 0. Preflight

From the repository root (`git rev-parse --show-toplevel`; it must hold `conformance/catalog.yaml` and
`crates/agterm-proto`):

- `gh`, `git`, `curl`, `python3`, `patch`, `xvfb-run`, `weston`, `dbus-daemon` on `PATH`; `gh auth status` ok.
- `.venv/bin/python` exists, else create it in the background: `python3 -m venv .venv && .venv/bin/pip install -e conformance`.
  In a worktree, a symlink to the main checkout's `.venv` works, but its editable install points at the main
  checkout's `conformance/`: run every pytest command with `PYTHONPATH=conformance` first (Step 5 does), so it
  imports this checkout's harness.
- No tracked file is modified (`git status --porcelain --untracked-files=no` empty): the branch and its commits
  must hold only this sync's changes. Untracked files (`.claude/`) are fine. Dirty → report and stop.

Missing tools or a dirty tree are outcomes to report, not questions to ask.

## Step 1. Tags

```bash
bash SKILL_DIR/scripts/resolve-tags.sh [TARGET]
```

Prints `CURRENT` (the catalog's `upstream:`), `TARGET`, `STATUS`, and `RELEASES` (every release in between).
Exit 3 (`up-to-date` or `older`): tell the user "agterm-rs targets CURRENT, the latest upstream release is TARGET:
nothing to sync" and stop. A release that skips versions is synced in one go; `RELEASES` says which changelogs to
read.

## Step 2. Upstream clone

```bash
bash SKILL_DIR/scripts/prepare-upstream.sh [--upstream DIR] CURRENT TARGET
```

Creates the clone when missing (blobless, full history), unshallows a `--depth 1` clone, fetches tags, refuses a
clone with local changes, and checks out TARGET. github.com drops long fetches now and then; the script retries,
and a final failure is reported, not worked around.

## Step 3. Analysis

```bash
python3 SKILL_DIR/scripts/classify-diff.py --upstream UPSTREAM CURRENT TARGET > WORK/classify.md
```

Read the whole report. It has the changelog sections, each commit with the areas its files fall in, the command
groups touched, and per area the files, the Swift test functions added and the user-visible strings added
(`(moved)` marks a string that was only moved, a refactor rather than new behavior). Read `references/areas.md`
for what each area means on the Rust side.

**Cluster** the change into units of porting work: start from the commits ("Commits by area"), one cluster per
feature or fix (a PR number, or the commit hash a changelog line cites, is a good seam), merge clusters that touch
the same command group, and fold refactor, docs and test-only commits into the cluster whose files they touch.
"Only moved code" groups in the report are usually a file split: confirm in the diff and leave them out. Commits
that touch only `no-port` and `upstream-design` paths go to the "no port needed" list. `review` paths: open them and
assign an area.

**What gets a task**, per behavior of a cluster:

- absent in the Rust code → a port task;
- present, but no Rust unit test asserts what upstream's Swift tests assert, or a suite-testable behavior that no
  conformance id covers → `partial`: a task that ports only the tests (it may find the code wrong; then it fixes it);
- present, with Rust unit tests twinning the Swift cases and, where the suite can test it, a listed ratchet id → no
  task, "already in the Rust build" with that evidence.

Small test-only clusters touching the same crate can share one task; a task never mixes a port with unrelated
groups.

**Analyse each cluster** by `references/prompts/area-analysis.md`: what changed upstream (Swift code, Swift tests for
the exact strings, commands.html and rules for the contract), what the Rust build already has, which conformance ids
cover it (`coverage-map.py`), and the draft task. With up to three clusters, do it yourself; with more, spawn one
`general-purpose` subagent per cluster in a single message (at most six at a time; more clusters go in waves) with
that prompt filled in, the hard-rules block pasted in. Check each answer: a STATUS of `already-ported` must cite a
file and a test, or it is treated as `partial`; a load-bearing `absent`/`partial` claim (a whole feature missing, a
behavior that differs) is confirmed with one `grep`/read of your own before it becomes a task, because a subagent
that missed a file reports a gap that is not there.

The analysis subagents run no cargo: the `target/` they would share is the gates'. While they read, warm it in the
background (`cargo test --workspace --no-run`, then clippy), so Step 5 starts from a built tree.

Facts in the plan come from what these reads and commands showed in this run, never from memory of an earlier
release: upstream changes its own conventions too.

## Step 4. Mechanical bump

```bash
git switch -c upstream-sync-TARGET
```

An existing branch of that name is an earlier run: report it and stop rather than overwrite it.

Do these in order; each says what to do when it does not go through cleanly. The rule throughout: **keep the branch
building and the ratchet green**. A step that would break that is left out of the bump and becomes a plan task,
because every plan task's subagent starts from this branch and must be able to run the gate.

1. **Catalog**: `.venv/bin/python conformance/tools/gen_catalog.py --tag TARGET --repo UPSTREAM`. It prints per
   section `+new` and `removed-upstream`; an item that comes back after an earlier `removed-upstream` is not
   counted as new, so take the full list from `coverage-map.py` after the commit (item 5). gen_catalog may reflow long
   hand-written exemption lines: that is formatting, keep it. A parse error writes nothing; report it and leave
   the catalog to a plan task (the generator itself needs work, `conformance/tools/gen_catalog.py`).
2. **Native pins**: `bash SKILL_DIR/scripts/pins-delta.sh [--upstream DIR] TARGET`. `PINS=unchanged` → the revisions are TARGET's too: bump the tag in the
   comment above each pin and keep its date. Otherwise put the printed values into `build/pins.env`, and rewrite the comment above each moved pin
   (`# libghostty at upstream agterm TARGET's GHOSTTY_REV (.upstream/agterm scripts/setup.sh, <date>)`, the zmx one
   alike). Then, in the background: `scripts/build-ghostty.sh` when a `GHOSTTY_*` pin moved, `scripts/build-zmx.sh`
   when a `ZMX_*` pin or `build/patches/zmx/` moved. They download, check every sha256, and apply the patches with
   `--fuzz=0`; after a `GHOSTTY_REV` move also run `scripts/gen-ghostty-bindings.sh`. A sum mismatch, a patch that
   no longer applies, a failed build or bindings that no longer compile → `git checkout -- build/pins.env` (and the
   bindings), save the log to `WORK/`, and record a plan task "move <pin> to <rev>: rebase <patch> / fix <error>"
   with the log's key lines.
3. **Vendored copies**: `bash SKILL_DIR/scripts/sync-resources.sh [--upstream DIR] --patches WORK/patches CURRENT TARGET`.
   It applies upstream's own diff to each copy (`resources/agent-status`, `resources/skills/agterm`, `hud.sh`,
   themes, `ghostty-defaults.conf`, zmx patches, `LICENSE.upstream`) only where it applies cleanly, so the Linux glue
   in the copies survives. `conflict` → a plan task to merge it by hand, keeping the Linux glue; the task names the
   upstream diff command (`git -C .upstream/agterm diff CURRENT TARGET -- <path>`), not the temp patch.
   `unmapped` → a plan task deciding where the new resource goes. When nothing conflicted, bump the release named in
   `crates/agterm-gtk/src/integrations/mod.rs`'s header. A zmx patch change also needs `scripts/build-zmx.sh` (as in 2).
4. **Tag mentions**: `bash SKILL_DIR/scripts/tag-mentions.sh CURRENT TARGET --apply` rewrites the `bump` class
   (README.md, CLAUDE.md, conformance/README.md, spike-macos.yml) and lists the rest with its class. Then by hand:
   `reference` (swift_macos.py's `PARITY_LEVEL` with its assert in `conformance/selftests/test_impls.py`); `pins` and
   `resources` were handled above. Leave `claim` (the Rust build's `PARITY_LEVEL` and its mirrors: it states a parity
   not reached yet and unlocks `since(V)` tests, so it moves in the plan's last task), `cite` (fork-bugs.md citations,
   re-checked in the last task) and `keep`. A `review` line has no rule: decide it, and name it in the report so the
   rule table in `tag-mentions.sh` gets a line.
5. **Commit** the files explicitly (never `git add -A`: `.claude/`, a `.venv` symlink and scratch stay out):
   `git add -- <files> && git commit -m "chore: mechanical sync to upstream TARGET" -- <files>` (`git add` first:
   a pathspec commit refuses a file git does not track yet, such as a new vendored resource).
   Then `.venv/bin/python SKILL_DIR/scripts/coverage-map.py --since HEAD~1 > WORK/catalog-delta.md`: every catalog
   item added, re-added, changed or removed, with the tests that cover it.

## Step 5. Baseline gates

Run CLAUDE.md's gate on the branch, all in the background, with `AGTERM_*`/`ZMX_*` stripped, each command's
output in `WORK/<name>.log`:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
scripts/lint-report.sh
GDK_BACKEND=x11 xvfb-run -a cargo test --workspace
PYTHONPATH=conformance .venv/bin/python -m pytest conformance --coverage-gate
conformance/tools/run-rust-linux.sh          # full run, ~10 min: protocol, cli, behaviors, then the ratchet
```

The cargo commands share `target/`, so chain them in one background script (`; ` between them, recording each exit
status), then start `run-rust-linux.sh` after it; the suite builds the release workspace and needs the vendored
prefixes (`scripts/build-*.sh --verify`; build what is missing first).

Read the outcome as the plan's starting point:

- **Expected reds**: `agterm-proto/tests/catalog.rs` and agterm-ctl's `the_command_tree_is_the_catalogs_cli` fail
  while the catalog lists wire names or CLI subcommands the code lacks; the coverage gate fails on the new items no
  test covers. Each becomes part of a plan task (the wire names in the first task). Record the exact test names
  and keys. When they are green (the code already has the names), the plan has no skeleton task.
- **Ratchet break** (`check_ratchet.py` lists a listed id red): the bump caused it, since `main` held the ratchet.
  Find which part (a pin, a resource) by its log, take that part out of the bump (revert it in a new commit),
  rerun, and make it a plan task. The branch never carries a red ratchet id.
- **Not run on this host**: check_ratchet prints `not run on this host:` for listed Live ids when the host cannot
  claim `live-sessions`/`live-any-shell` (no zsh, no zmx). They hold the ratchet without running; name them in the
  plan's Context, since a task touching Live cannot see them green locally (CI's zsh job runs them).
- **Newly green ids**: check_ratchet prints `newly passing, not in the list (N):` with the ids (no such line: none).
  Add them with
  `.venv/bin/python conformance/tools/check_ratchet.py --report conformance/reports/rust-linux/nodeids.jsonl --add`
  and commit `test: ratchet after the TARGET sync`.
- fmt/clippy/lint-report red after a mechanical bump means a regenerated file or the bindings: fix it in the bump.

## Step 6. Plan

Read `references/plan-template.md` and write `docs/plans/<YYYYMMDD>-upstream-sync-TARGET.md` (today's date) from
the analysis, the bump and the baseline:

- one `### Task N:` per cluster with STATUS `absent` or `partial` (partial: only the gap), in the order the template
  gives: new wire names and CLI skeletons first when the catalog gained any, then the groups, then vendored-copy
  conflicts and pin work, then the UI checklist task, then "Claim TARGET parity and update the docs" last;
- every task names its upstream implementation files, Swift tests (with the functions), contract sections, the Rust
  files, the existing and new conformance ids, and ends with the gate and "newly green ids added with
  `check_ratchet.py --add`";
- `already-ported` clusters go to Context with their evidence, so nobody ports them twice;
- the Safety rules section holds the hard-rules block verbatim.

Commit the plan on the branch:
`git add -- docs/plans/<file> && git commit -m "docs: plan the upstream TARGET sync" -- docs/plans/<file>`.

## Step 7. Report

End with this report, in the user's language:

```
## Upstream CURRENT → TARGET (<N> commits, releases ...)

**What changed upstream** — one line per feature/fix, with its PR and the areas it touches.
**Done mechanically** (branch `upstream-sync-TARGET`, commits <sha> ...) — catalog (+x/-y items), pins, vendored
copies, tag mentions; each thing left out of the bump and why.
**Baseline gates** — each gate's result; the expected reds and which task turns each green; the ratchet.
**Left for the plan** — `docs/plans/<file>`: the task list, one line each.
**Already in the Rust build** — clusters with no task, with the evidence.
**No port needed** — the no-port list, one line each.
**Next** — `/plan-exec-revmux docs/plans/<file>` from branch `upstream-sync-TARGET`; then (the user) push the branch
and run `reference-macos.yml` for the new suite tests. Nothing was pushed.
```

## Key rules

- The skill ports nothing; behavior changes go to the plan.
- The branch always builds and holds the ratchet; anything that would break either is a plan task, not a commit.
- `PARITY_LEVEL` moves only in the plan's last task.
- Every subagent prompt and the plan carry the hard-rules block verbatim.
- Commits stay local; nothing is pushed, tagged or released.
