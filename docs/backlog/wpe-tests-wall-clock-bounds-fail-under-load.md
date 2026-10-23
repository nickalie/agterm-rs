---
worth: yes
where: crates/ngterm-wpe/src/worker/client.rs:1251
added: 2026-10-23
---
# ngterm-wpe tests fail on wall-clock bounds when the machine is loaded

Seen 2026-10-23 in full workspace runs inside the 4-core agent slice, load average 27–65 from other sessions, on
branches that changed nothing in `ngterm-wpe`:

- `worker::client::tests::a_backlog_a_stalled_worker_never_drains_breaks_the_transport`: `a send took 277ms` against
  the 250 ms bound here.
- `worker::client::tests::a_large_paste_and_the_input_after_it_reach_a_worker_that_reads`: `Option::unwrap()` on
  `None` at `client.rs:1061` (`wire::recv` returned nothing).
- `--test headless` `pointer_keyboard_and_wheel_input_reach_the_page` (`tests/headless/cases.rs:30`): timed out
  waiting for the page title (`down:0:2`, later `wheel`), in shm direct and shm worker modes; it failed the same way on
  unmodified main.

Each passed when rerun alone at lower load. Under the Gates these are bugs: the bounds measure the scheduler, not the
code. Bound what the test means (a send never blocks on the stalled peer) by event or read-back, or derive the limit
from a measured baseline, rather than raising it.
