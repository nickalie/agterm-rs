---
worth: yes
where: crates/agterm-session/src/process_table/mod.rs:72
added: 2026-10-20
---
# Windows' console foreground can name a background child, which restore then replays

`console_foreground` takes the shell's newest direct child started after the shell for the console's foreground
program. A program the shell started in the background shares the console the same way (PowerShell's
`Start-Process -NoNewWindow server`, `[Diagnostics.Process]::Start`, a `Start-Job` worker, a prompt helper such as
posh-git's `git status` while the prompt renders, `start /b` in `cmd.exe`), so a background child that is the newest
one is read as the pane's foreground: `tree` reports it, the quit capture records it, and the next launch or a
`session.restart` replays it in the foreground. Replaying a program that was not in the foreground is worse than
restoring a bare prompt.

Found in Task 27's round-2 review (docs/plans/20261018-cross-platform-sessions.md, PowerShell live sessions) while
the Unix PowerShell descent with the same flaw was removed (a Unix pwsh pane now restores to its prompt); the Codex
discussion that settled that call (`plan-exec-revmux/20261018-cross-platform-sessions/discussions/
task-27-pwsh-foreground`) flagged the Windows heuristic as an unresolved safety risk. Not fixed there: it is Task
15's process table, `cmd.exe` panes rely on it too, and Tasks 25 and 26 move live panes onto the session server. A fix
needs a signal that ties the console's foreground to one process — for PowerShell, the integration script publishing
the native command it starts (or its pid) between OSC 133 C and D; otherwise the conservative read (no capture under
an idle shell with more than one candidate child) as on Unix.
