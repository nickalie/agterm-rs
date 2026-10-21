---
worth: yes
where: crates/agterm-session/src/server/pane.rs:949
added: 2026-10-21
---
# A Windows resize repaints over text `print` wrote into a session

`Pane::print` writes its bytes into the server's terminal of the session alone, never through the pseudoconsole, so
conhost's own screen buffer never holds them. Every `ResizePseudoConsole`, to the size the session has too, makes
conhost repaint its viewport from that buffer through the output pipe, and the repaint paints over the printed text
the viewport shows: `screen`, every client and every later attach's snapshot lose it. On Windows a session resizes
whenever its leader's grid changes (a window resize, an attach of another size taking the lead), so printed text
is gone after the first one. Unix PTYs keep no screen of their own and are not affected.

Measured on `windows-2025` (Windows Server 2025, conhost build 26100), CI run 39525073518, a cmd.exe session at
80x24: a print, then a same-size resize, a 100x30 resize and a resize back each lost the printed text, while a print
followed by output only kept it. Linux and macOS kept it in every case. `tests/pane.rs`
(`a_resize_on_windows_repaints_over_printed_text`) pins the Windows loss and the Unix survival.

The update handoff hit it too and no longer does: the replacement server resized the pseudoconsole it took over (to
the size it had) to check it could drive it, which erased printed text across every handoff, and in the old server's
terminal on a rollback. That check is gone on Windows (`server/pane/handoff.rs`, round 1 review of Task 24 in
`docs/plans/20261018-cross-platform-sessions.md`; the Codex discussion is
`plan-exec-revmux/20261018-cross-platform-sessions/discussions/fixer-t24r1-conhost-repaint`).

Not fixed with it: a fix changes what `print` is on Windows. Writing the bytes into conhost's buffer (the server
attaching to the session's console and writing them there, so the repaint carries them and the session's terminal
reads them back from the output pipe) is process-wide (`AttachConsole`), makes `print` asynchronous and passes the
bytes through conhost's VT renderer; replaying printed bytes after a resize does not merge with the cursor, modes
and erasures the program's output made meanwhile. Tasks 25 and 26 switch live sessions onto this server, so a
`print` into a Windows pane inherits this.
