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
```

`--macos-launch-mode plain` replaces the default `uitest` launch (XCUITest sentinel with zmx on).
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
- `spike-macos` (`macos-26`): the launch and ax-driver spikes, when their tools change.
