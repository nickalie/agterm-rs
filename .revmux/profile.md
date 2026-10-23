# ngterm — review profile

Source of truth: `CLAUDE.md` (rules), `README.md` (commands, gates), `conformance/README.md`,
`conformance/docs/rust-build.md`. Where this file and those disagree, they win.

## What the software is

- ngterm: a Rust port of upstream macOS agterm (`umputun/agterm` v0.36.0), a terminal for running coding agents
  in sessions/workspaces/windows, driven by the `ngtermctl` CLI over a control socket.
- One frontend, GPUI (`ngterm-gpui`), rendering libghostty-vt itself. Toolkit-neutral logic lives in
  `ngterm-core` (model, dispatcher, palette, keymap, persistence) and `ngterm-term` (pane core over a PTY).
- macOS, Linux and Windows are equal targets; today the frontend builds and is tested on Linux only.

## What a real failure looks like

- **critical**: loss or corruption of on-disk state (`windows.json`, settings, keymap, hooks), killing or
  orphaning a user's live sessions (zmx), a dev/test run touching the user's running ngterm (its socket, bus,
  display, state dir or session namespace), a crash on a path users reach.
- **major**: a deviation from upstream's wire protocol, command semantics, error strings, `ngtermctl` output or
  on-disk formats; a ratchet id in `conformance/rust-passing.txt` going red; a platform-specific mechanism with no
  counterpart or `cfg` fallback for the other OSes; a compositor-specific path with no fallback; anything that
  reads, runs or falls back to an agterm/agtermctl name or path (the build has its own identity).
- **minor**: a doc or comment that misstates what the code does; a missing test for a case the change introduces.

## Blast radius

- Agents and users run this inside their live terminal; a wrong default socket, env leak (`NGTERM_*`, `ZMX_*`)
  or a reap by process name hits real work.
- `ngterm-core` changes reach every frontend and the control protocol; the frontend's UI changes reach only GPUI.

## Reporting bar

- Report runtime behavior, broken contracts, parity breaks and gate breaks. Style the toolchain already
  enforces (rustfmt, clippy pedantic with `-D warnings`, `clippy.toml` thresholds) is not a finding.
- Finding nothing is a valid answer.

## Deliberate conventions — not findings

- Upstream is the reference for behavior; a deviation is a finding unless the code or docs record it as the
  build's own choice (Linux glue upstream has no counterpart for, or a user override such as Settings being an
  in-window popup).
- Doc comments cite the upstream Swift (or Ghostty) symbol they port; long, dense module docs are the house
  style, not verbosity.
- Complexity over a `clippy.toml` limit is split, or carries one outer `#[expect(..., reason = "...")]`;
  `#[allow]` and inner `#![allow]`/`#![expect]` are refused.
- Generated bindings change only through their generator scripts; `vendor/` changes go through `build/patches/`.
- Plans in `docs/plans/` are historical records: do not flag them as out of date with the code.
- Tests run headless (xvfb/private weston, private bus, lavapipe); `#[expect]`/`unwrap` in tests are allowed.
