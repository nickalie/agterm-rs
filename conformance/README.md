# agterm conformance suite

Black-box tests that define feature parity with upstream agterm.
The same tests run against the released macOS app, the Linux fork and the Rust build, over the control socket,
`agtermctl` and the accessibility tree.

## Install

```sh
python -m venv .venv && .venv/bin/pip install -e conformance          # Linux, CI gate
python -m venv .venv && .venv/bin/pip install -e "conformance[ax]"    # macOS: adds the ax ui driver's pyobjc
```

## Running

`--impl` picks the adapter; `--bin` is the app to launch; `--ctl` overrides the `agtermctl` found beside it.

```sh
# upstream macOS release (a bundle or its executable); fetch_reference.py downloads and caches it by tag
bundle=$(.venv/bin/python conformance/tools/fetch_reference.py --tag v0.36.0 | sed -n 's/^bundle=//p')
.venv/bin/python -m pytest conformance/tests --impl swift-macos --bin "$bundle"

# the installed Linux fork: protocol, cli and behaviors, with a report in conformance/reports/reference-linux/
conformance/tools/run-reference-linux.sh [--bin PATH] [-- PYTEST_ARGS...]

# the Rust build; without --bin every test skips and parity reads 0%
.venv/bin/python -m pytest conformance/tests --impl rust --bin <rust-binary>

# the Rust build of this checkout: build, stage, protocol, cli and behaviors, then the ratchet;
# report in conformance/reports/rust-linux/ (GTK) or conformance/reports/rust-gpui-linux/ (--frontend gpui)
conformance/tools/run-rust-linux.sh [--frontend gtk|gpui] [--stage DIR] [-- PYTEST_ARGS...]

# whether a WebKitGTK view's web process starts under the same headless isolation (a diagnosis; the view runs in
# python, which agterm's AppArmor profile does not cover, so Ubuntu's userns restriction stops its sandbox)
.venv/bin/python conformance/tools/webkit_probe.py

# whether the staged Rust build's terminal surface is created and draws a frame under the same isolation (a CI
# spike; `version` alone answers without a GL context); defaults to run-rust-linux.sh's stage of the frontend
.venv/bin/python conformance/tools/ghostty_probe.py [--frontend gtk|gpui] [--bin PATH]

# whether the staged Rust build's title bar, sidebar and terminal draw one color, the terminal theme's background, and
# a sidebar tint washes the sidebar alone: screenshots of the maximized window from the same isolation, its weston
# started with --debug for weston-screenshooter (a Rust-build check outside the catalog); defaults to the same stage,
# --frontend gpui to the GPUI build's (on lavapipe, through the harness's GPUI frontend)
.venv/bin/python conformance/tools/chrome_probe.py [--frontend gtk|gpui] [--bin PATH] [--keep DIR]
```

`--macos-launch-mode plain` replaces the default `uitest` launch (XCUITest sentinel with zmx on).
`--rust-frontend gpui` launches the Rust build's GPUI frontend instead of its GTK one (see Rust runner and ratchet).
The Rust build's side of the contract (the `version` extension, the launch environment, the capability
vocabulary) is in [docs/rust-build.md](docs/rust-build.md).

## Layers

- `tests/protocol`: one JSON line per request over the socket, through `ctl`.
- `tests/cli`: `agtermctl` output, exit codes and socket resolution, through `cli`.
- `tests/behaviors`: relaunch, keymap and hooks, across instances started by `launch`.
- `tests/ui`: accessibility ids from `ui-ids.yaml`, through `ui`.
  Only the macOS `ax` driver exists; elsewhere the layer skips as unsupported.
- `selftests`: the harness itself, host-free; they run without `--impl`.

## Markers

- `since("0.36.0")`: skipped as `not-yet-at-level` when the build's parity level is lower.
- `requires("capability", reason=...)`: skipped as `unsupported` when the build lacks it.
  A test that branches instead asks `adapter.has("capability")`.
  Both refuse names outside the vocabulary in `impls/base.py`.
- `covers("item", ..., streams=(...))`: the catalog keys a test covers; a stream-mode command counts only when
  `streams` names it too, from a test that drives the stream after the ok.

The run summary prints pass, fail, both skip kinds and reruns, and parity: passed over passed, failed and
not-yet-at-level, so an unsupported capability is a recorded divergence, not a gap.

- `--must-pass CAPABILITY` fails the run unless every collected test requiring it ran and passed, and at least one
  did (reference-macos uses it for `live-sessions`).
- `--nodeid-report PATH` writes one JSON line per run test with its outcome merged over setup, call and teardown,
  plus the run's collection errors and `--must-pass` problems; `tools/check_ratchet.py` reads it.
  A run pytest refuses (a usage error, a path or node id that matches nothing) or that breaks internally writes none.
- `--reruns N` (pytest-rerunfailures) is for the ui layer only, where accessibility reads race the app.
  Protocol, cli and behaviors tests wait on read-back and never rerun.

## Reference discipline

A test is accepted only when it is green on its references: reference-macos in CI and reference-linux through
`run-reference-linux.sh`, whose report goes on the PR.
Red on a reference means the test is wrong.

When the references disagree on observable state or effects:

- a legitimate platform divergence becomes a capability with a reason, and the test asserts each branch under
  `requires` or `adapter.has`, holding each build to its platform's reference;
- a fork bug against upstream's documented contract keeps upstream's assertion everywhere, with
  `xfail(strict=True, raises=AssertionError)` on `swift-linux` only, listed in
  [docs/fork-bugs.md](docs/fork-bugs.md).

Error text that differs only in wording is asserted on its shared part.

The Rust build is held to upstream's contract, its capability branches included: it claims the capabilities listed
in [docs/rust-build.md](docs/rust-build.md) > Rust target profile, and where a test branches on a capability it
takes upstream's branch unless that list says otherwise.
reference-linux is a diagnostic for it, not a reference: a Rust test that differs from the fork is judged against
upstream.
The per-platform rule above applies to the two Swift references.
The Rust build's progress is held by a ratchet: `conformance/rust-passing.txt` lists the ids that went green, and
`run-rust-linux.sh` fails when one of them goes red again (see rust-build.md > Ratchet).
A Rust-only extension (today `live-any-shell`, Live sessions in bash and fish login shells) is a capability neither
reference claims: its tests skip as unsupported on both references and are validated by the Rust CI with
`--must-pass` on the capability (see rust-build.md > Rust-only extensions).

## Rust runner and ratchet

`tools/run-rust-linux.sh` builds the workspace (release), stages it with `scripts/stage.sh` into
`target/conformance-stage` (or runs an existing stage given with `--stage DIR`) and runs the protocol, cli and
behaviors layers with `--impl rust` under run-reference-linux.sh's isolation.
It runs pytest with `$PYTHON`, else `.venv/bin/python`, else `python3`, with this checkout's harness first on
`PYTHONPATH`.
Into `--out DIR` (default `reports/rust-linux/`) it writes `report.md`, `junit.xml`, `pytest.log` and `nodeids.jsonl`,
the `--nodeid-report`.
It exits with the ratchet's status, not pytest's, since red tests are expected until parity: 0 holds, 1 broken (or an
instance root left behind), 2 no usable report.
One suite run at a time per user: concurrent runs, from any checkout, queue on `$CONFORMANCE_LOCK` (default
`$XDG_RUNTIME_DIR/agterm-conformance.lock`, else `/tmp/agterm-conformance-<uid>/`), and a waiting run says so; the
deadlines assume one suite's load, and the lock does not hold back a stand-alone `cargo build` or `cargo test`.

```sh
conformance/tools/run-rust-linux.sh                          # full run
conformance/tools/run-rust-linux.sh -- -k test_version       # partial run: listed ids not selected are not judged
.venv/bin/python conformance/tools/check_ratchet.py \
  --report conformance/reports/rust-linux/nodeids.jsonl --add  # merge the newly green ids into rust-passing.txt
conformance/tools/run-rust-linux.sh --ratchet-only \
  --also tests/protocol/test_split.py --also-k sidebar       # the listed ids plus what a task works on
conformance/tools/run-rust-linux.sh --frontend gpui          # the GPUI build, held by rust-gpui-passing.txt
.venv/bin/python conformance/tools/check_ratchet.py --report conformance/reports/rust-gpui-linux/nodeids.jsonl \
  --list conformance/rust-gpui-passing.txt --add
```

While both frontends coexist each has its own ratchet: `--frontend gpui` builds the same workspace, stages it with
`scripts/stage.sh --frontend gpui` into `target/conformance-stage-gpui` (`bin/agterm-gpui`), runs pytest with
`--rust-frontend gpui` and holds `rust-gpui-passing.txt`, reporting into `reports/rust-gpui-linux/`.
The GPUI build renders through Vulkan, here on Mesa's lavapipe and nothing else: the runner takes its ICD manifest
from `scripts/fetch-lavapipe.sh` (`AGTERM_LAVAPIPE_ICD`, else the distribution's package, else the pinned Arch package
verified and unpacked into the vendor dir) and hands it to the harness as `CONFORMANCE_LAVAPIPE_ICD`; without one the
run exits 2.
The adapter makes it the instance's only Vulkan driver (`VK_DRIVER_FILES`), forces GL onto llvmpipe, drops every
inherited device selection, refuses a manifest that does not load lavapipe, and starts the instance's weston with
`--fake-seat` (GPUI panics on a compositor without a `wl_seat`); the GTK frontend keeps its weston and environment.
`--fake-seat` needs weston 15.0 or later: where the weston on PATH lacks it (Ubuntu 24.04 ships 13), the runner takes
the one `scripts/build-weston.sh` vendors, else exits 2 naming that script, and the adapter refuses such a weston with
the same pointer. The ICD manifest must name a driver that is there.

`rust-passing.txt` holds one pytest node id per line, verbatim (an id may end in spaces) and sorted, after its `#`
header.
`check_ratchet.py` prints the breaks and the newly passing ids (into the job summary in CI); `--add` writes only when
the ratchet holds, and never removes an id.
What breaks it is listed in [docs/rust-build.md](docs/rust-build.md) > Ratchet.

A full run waits out the read-back of every red test (a GPUI run short of parity: ~47 min, almost all of it in tests
that cannot pass yet), so `--ratchet-only` (either frontend) runs just the ids the frontend's list holds, plus each
`--also` directory, file or node-id prefix (a path outside the protocol, cli and behaviors layers is collected too)
and each `--also-k` `-k` expression, repeatable: a task names what it is working on.
An `--also` or `--also-k` that matches no collected test is a usage error (exit 2), unless `-k`, `-m` or node ids
narrowed the run. The plugin options behind it are `--ratchet-only LIST`, `--also` and `--also-k`. The nodeid report's run header then says `"mode": "ratchet-only"`, and
`check_ratchet.py` judges it as a full run: every listed id must pass, a listed id that was not collected is warned
about, and `--add` adds only ids that ran and passed. A narrower selection (`-k`, `-m` or node ids after `--`) makes
the mode `partial`: the listed ids that did not run are not judged, so a partial run can neither break nor shrink
the list through what it left out. The report names the mode and the suite time.
A full run stays the default and is required at milestones: the GPUI switch (the plan's Task 34), and whenever a
change touches the harness or shared crates broadly (agterm-core, -proto, -host, -server, -ctl, -zmx, -app or
-term). Between them a task's routine run is ratchet-only; it finds newly passing ids only among its `--also`
selections, so a task that turns on a broad area (the terminal, say) grows the list with a full run.
`--nodeid-report` cannot be combined with `--coverage-gate`.

## Catalog and coverage gate

`catalog.yaml` (commands, fields, events, CLI subcommands, behaviors, exemptions) and `ui-ids.yaml` are generated
from an upstream tag:

```sh
.venv/bin/python conformance/tools/gen_catalog.py --tag v0.37.0 --repo ../agterm   # or --source <extracted tree>
```

Generated sections are replaced and statuses survive; `behaviors` and `exemptions` are hand-maintained.
Retire an item with `status: removed-upstream`.

```sh
.venv/bin/python -m pytest conformance --coverage-gate
```

The gate launches nothing.
It fails on any active item that no `covers` names and no exemption with a reason excuses, and on a `covers` of an
unknown or removed item.
`covered`/`uncovered` statuses are informational.

## Safety guards

These runs share a machine with a live agterm, so the harness refuses a launch that could reach it:

- `AGTERM_*` and `ZMX_*` are scrubbed from the environment at plugin import, and the launch env may carry only the
  harness's own.
- Each instance gets a fresh root (`/tmp/agt-*`, or under `CONFORMANCE_TMP`) holding its `HOME`, XDG homes, state
  dir and socket; the guards refuse any of them outside it.
  Its zmx directory derives from that state dir.
- Linux instances get a unique `AGTERM_APP_ID`, never the installed one, and a private `XDG_RUNTIME_DIR`.
- Linux instances draw into their own headless weston with their own dbus-daemon, so no window, notification or
  a11y object reaches the desktop.
  `run-reference-linux.sh` requires both programs.
  `CONFORMANCE_LINUX_DISPLAY=session` uses the desktop's display and bus instead, for debugging only.
- A GPUI instance renders on lavapipe only, in either display mode: never on the machine's GPU driver.
- URI and file launches go to a no-op handler through the isolated `mimeapps.list` and `BROWSER=true`.
  The ghostty config turns off copy-on-select and clipboard writes.
- `cli` gives every `agtermctl` call the instance's `--socket`; a call without one must resolve under the root.
- Stopping reaps Live daemons over the protocol, SIGTERMs only the launched pid, and fails on any zmx or
  session-host process left with the instance's `ZMX_DIR`.
  The macOS session host outlives the app by design: the harness ends only the one started from the instance's own
  bundle copy, by exact pid, re-checking ownership before each signal.
- A macOS instance runs from its own copy of the bundle, because a second launch of the same bundle path binds no
  socket ([docs/spike-macos.md](docs/spike-macos.md)).
- `CFFIXED_USER_HOME` does not isolate the `com.umputun.agterm` defaults domain, which every macOS instance shares
  with the account.
  Run reference-macos only on an ephemeral runner, never on a Mac with a live agterm.
- A Linux pane starts in the real home (the fork ignores `HOME`): keep what a test types side-effect free or `cd` into
  the instance root first.

## CI

- `conformance-gate` (ubuntu): self-tests and the coverage gate on every PR and on `main`.
- `reference-macos` (`macos-26`): protocol, cli and behaviors with `--must-pass live-sessions`, then the ui layer
  with `--reruns 1`, against the release named by the catalog's `upstream`.
- `rust-linux` (`ubuntu-24.04`): the Rust workspace's format check, strict clippy, lint report (in the job summary)
  and `cargo test`, over the vendored libghostty and gtk4-layer-shell prefixes; then the staged build under the suite,
  held by the ratchet, once per login shell, each with `--must-pass live-any-shell`: bash (`conformance`), fish
  (`conformance-fish`) and zsh (`conformance-zsh`, with `--must-pass live-sessions` too); and the GPUI build
  (`conformance-gpui`, `--frontend gpui` on the distribution's lavapipe), held by `rust-gpui-passing.txt`.
- `spike-macos` (`macos-26`): the launch and ax-driver spikes, when their tools change.
