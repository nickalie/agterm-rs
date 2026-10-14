---
worth: yes
where: crates/agterm-ctl/tests/cli.rs:882
added: 2026-10-15
---
# `zmx present` in the cli test can die by a signal under load

`zmx_tree_and_present_send_their_args_and_print_the_sessions` failed once in a full
`GDK_BACKEND=x11 xvfb-run -a cargo test --workspace` (Task 31's fix round, the machine shared with other agents):
`["zmx", "present", "S1"]` ended with no exit code (`(None, "", "")`, killed by a signal) where `(Some(0), "", "")`
was expected; it happened twice in two full workspace runs, while the `cli` binary alone passed 55 of 55 runs (plain,
workspace-built, under `xvfb-run`) and earlier full runs passed it. The case's scripted app "answers and hangs up",
and `agtermctl`'s `main` restores SIGPIPE's default action (`client.rs:189`): a lead, unproven, is a write to the
hung-up socket on the `zmx present` stream path that does not go through `MSG_NOSIGNAL`/`SigpipeIgnored`, so a
hang-up that wins the race kills the client. Not investigated (that round touched only `agterm-wpe`). Next: put the
signal (`ExitStatusExt::signal`) in the assertion message and reproduce in a loop under load.
