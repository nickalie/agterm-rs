---
worth: yes
where: crates/ngterm-gpui/tests/quick_terminal.rs:126
added: 2026-10-21
---
# The quick terminal's weston test misses the unzoomed size under load

`the_panel_falls_back_to_a_window_on_weston` waits for a second `quick terminal: sized size=1100x700` after the unzoom
and times out when the machine is loaded: the log shows the zoom's `sized size=1280x800` and then nothing, so either the
window never shrinks back on weston or its configure arrives after the wait's deadline. Seen twice in the
cross-platform-sessions plan (Task 27 at a load average of 40–70, Task 28 at about 14 while the conformance selftests
ran beside it); both times the test passed in a quiet run. Task 28 left it as it was (unrelated to removing zmx). A fix
starts from telling the two apart: log weston's configure for the unzoom, then either re-request the restored size when
weston keeps the maximized one, or bound the wait by the configure rather than by time.
