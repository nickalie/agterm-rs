# Known swift-linux bugs

Each entry is an upstream-documented contract the Linux fork breaks.
Its test asserts upstream's contract on every implementation and is `xfail(strict=True, raises=AssertionError)`
on `swift-linux` only, so a fork fix turns it into a failing XPASS: delete the mark then.
A behavior upstream leaves undocumented and the platforms do differently is a capability instead.

- `workspace new --collapsed` makes the new workspace current and joins it to an applied focus set.
  Upstream (v0.36.0 `site/commands.html`, `.claude/rules/control-api.md`, `.claude/rules/sidebar.md`):
  a background create never takes the active workspace and opts out of focus membership.
  `tests/protocol/test_workspace.py::test_background_create_keeps_the_current_workspace[collapsed]`.
