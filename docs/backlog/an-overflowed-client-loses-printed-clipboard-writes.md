---
worth: yes
where: crates/agterm-session/src/server/pane.rs:837
added: 2026-10-20
---
# An overflowed client loses a printed clipboard write while the API answers Done

`Pane::print` (the api's `print`, what `clipboard set` will use) puts its bytes into every client's output queue at
the next offset, the same lane as the program's output. A client whose queue has overflowed is sent none of that
until its resync (`Clients::output` skips it), and a later overflow drops what a queue still holds unsent
(`Queue::overflow` keeps only role reports, `Overflowed` and `Exit`). The resync's snapshot restores the
terminal's state but carries no completed OSC 52 write, so that client never gets the clipboard update, while the
api has already answered `Done`. It happens under ordinary slow-client backpressure (a client more than the queue
limit behind). The same loss applies to the program's own one-shot side effects in the dropped output (its OSC 52
writes, notifications, bells), not only to prints.

Reviewer's suggested fix: keep injected writes as ordered events that survive an overflow and its resync (queued
after the resync's snapshot), and report backpressure to the api caller when they cannot be kept.

Not fixed in the run's side fixes: it changes the overflow and resync contract Task 18 settled (the queue's lossless
lane, what an overflow drops, the epoch order), and deciding which side effects outlast a resync — prints alone or the
program's too — is a design change across `clients.rs`, `pane.rs` and the attach wire, larger than a side fix.
`clipboard set` reaches the server's print only at the switch (Task 25), where the fix belongs at the latest.
Reported as pre-existing in the revmux review of Task 19, round 1 (`docs/plans/20261018-cross-platform-sessions.md`).
