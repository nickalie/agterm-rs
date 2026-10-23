---
worth: yes
where: crates/ngterm-session/tests/replies.rs:554
added: 2026-10-23
---
# The two-client replies test sometimes sees different streams under load

`every_query_gets_one_answer_with_two_clients_attached` failed once with `both clients see the same stream` during a
full `cargo test --workspace` on 2026-10-23, run inside the 4-core agent slice while other sessions' builds pushed the
load average to 27–65. Three reruns each on that branch and on unmodified main passed, and the branch touched no
`ngterm-session` code, so this is pre-existing. Under the Gates a flaky test is a bug in the build or the test: find
whether the second client's view can miss or reorder a reply (an attach racing the first query) or whether the test
reads its output before the stream settles, and make it wait on read-back rather than timing.
