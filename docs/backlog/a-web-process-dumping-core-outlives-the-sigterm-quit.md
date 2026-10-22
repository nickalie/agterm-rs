---
worth: yes
where: crates/ngterm-gpui/tests/quit.rs:463
added: 2026-10-21
---
# A web process dumping core outlives the SIGTERM quit

`sigterm_saves_and_quits_within_its_deadline_with_live_sessions_busy_and_pages_open` failed once in the bash
conformance leg of `rust-linux.yml` (run 39574804550, the Arch container with the system's wpewebkit, HTML overlays
on): the app quit in 0.13 s with status 0 and its state saved, but a `WPEWebProcess` (and the two `bwrap` processes
above it) was still there 30 s later, its main thread in `do_coredump` and every other thread in `do_exit`. So the web
process crashed when the app's quit cut its connection, and the kernel was still writing its core when the test's wait
ran out. Seen in the cross-platform-sessions plan's Task 29 on a tree whose earlier runs passed the same test (Task 28's
39567082606), so it is intermittent; nothing in Task 29 touched the HTML overlays.

The defect is the crash, not the wait: a web process should end on its own when the UI process goes, as WebKit's does
on a closed connection. A fix starts from the crash's stack (the container's core, or `WEBKIT_DEBUG`/a `catchsegv`-style
handler in the web process's environment): either the engine crashes on a connection lost mid-frame (the spinning page
and the threaded compositor were busy), which the quit could avoid by closing the pages first, or it is a wpewebkit bug
at the pinned version to report upstream.
