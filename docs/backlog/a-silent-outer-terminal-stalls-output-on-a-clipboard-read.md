---
worth: later
where: crates/agterm-session/src/attach_tty/owner.rs:19
added: 2026-10-21
---
# A silent outer terminal stalls a session's output for 30 s on its first clipboard read

The attach CLI relays a program's OSC 52 read to its outer terminal and, since the Task 22 round 1 review fix,
waits for the answer up to `DELEGATION_WAIT` (30 s) instead of taking the DA1 that follows as "no answer": Ghostty
(`clipboard-read = ask`, its default) and kitty (`read-clipboard-ask`) answer DA1 at once and the read only after the
user agreed, so a DA1 fence denied every read there. The cost lands on terminals that never answer a read (VTE,
Alacritty's default, WezTerm, Windows Terminal): the server holds the program's output behind a delegated read
(`server/replies.rs`, the read's dispatch byte and everything after it), so the session's first clipboard read per
attach freezes its output for the whole wait. Later reads are denied at once, without asking, while that read stays
unresolved (one OSC 52 answer carries no request of its own, so one read at most is asked at a time).

Options: keep it (bounded to one stall per attach); or make the server's clipboard path asynchronous, so the
program's output goes on while a read waits and the answer is formatted and delivered on its own when it comes,
as zmx effectively did (the outer terminal's late answer arrived as input). Codex's position: the asynchronous path
is the full fix, at the price of the synchronous read callback and the armed-answer path Task 19 built
(`replies.rs` `Outlet::answer`), and of strict reply ordering. Claude's: out of scope for a client review fix, and
the stall is bounded.

What would settle it: how often programs in live sessions read the clipboard over OSC 52 (Neovim's OSC 52 provider
on paste, tmux) from terminals that never answer, against the cost of the asynchronous server path. Meanwhile the
client waits and the stall stays; nothing to undo. Transcript:
`/home/nick/.local/state/plan-exec-revmux/20261018-cross-platform-sessions/discussions/fixer-t22r1-clipboard-fence/transcript.md`
(plan `docs/plans/20261018-cross-platform-sessions.md`, Task 22, review round 1).
