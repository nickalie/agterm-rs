---
worth: yes
where: crates/agterm-ctl/tests/cli.rs:288
added: 2026-10-15
---
# Two connect-failure cases of the `agtermctl` cli tests flake under load

Seen while stressing the `cli` test binary (8 copies at once, each `--test-threads=16`): 4 failures in 400 binary
runs, then 1 in 1200.

- `a_refused_connection_exits_1_with_the_error` (4 of the 5): `Error: no response from <dir>/agterm.sock` where the
  connect should be refused. The test binds a `UnixListener` and drops it to leave a stale socket file; lead,
  unproven: a sibling test's `Command` forks between the bind and the drop, so the child holds the listening socket
  until its exec closes it (`SOCK_CLOEXEC`), `agtermctl`'s connect lands in that window's backlog and the close
  resets it. If so the fix is in the test (make the stale socket in a way no fork can keep listening, e.g. bind in a
  child process that exits first), not in `agtermctl`.
- `a_present_owner_that_does_not_accept_is_named` (1): the hint said `agterm may be stopped …` where the held
  ownership lock should name `the socket owner is present …`, so `socket::ownership_lock_held` read the lock as not
  held (or `None`) while the test's `ControlServer::new` held it. Not investigated.

Next: put a loop around each case under the same parallel load to reproduce in isolation, then confirm the lead.
