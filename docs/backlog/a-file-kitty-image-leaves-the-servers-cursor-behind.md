---
worth: yes
where: crates/agterm-session/src/server/pane.rs:1194
added: 2026-10-20
---
# A file, temp-file or shared-memory Kitty image moves the clients' cursor but not the server's

The session server's terminal keeps Kitty graphics on with its image media off (`server_terminal`:
`set_kitty_image_media(false, None, false)`), so an image sent with `t=f`, `t=t` or `t=s` fails to load on it
(`graphics_exec.zig` `transmit`: unsupported medium, before `display`): nothing is placed and the cursor movement
never runs, even when the placement gives its cells. The reply owner's terminal loads the same image and, with the
default `C=0`, moves its cursor below or past it. From then on, until the program moves the cursor itself, the server
answers CPR (`CSI 6n`) and DSR from a position the clients do not show, programs that lay out around the image draw in
the wrong place, and the next attach or resync snapshot puts the cursor somewhere other than where the owner has it.
`kitty +kitten icat` sends files or shared memory by default when it runs locally. Direct (`t=d`) images are placed on
the server and move its cursor as on the owner's screen.

Reviewer's suggested fix: have the server place such images without reading their media — a placeholder placement
sized from the command's `c=`/`r=` (or `s`/`v` and the owner's cell size), or sized from rows and columns the owner
reports back with its Kitty completion.

Not fixed in the run's side fixes: libghostty-vt's C API places no image it could not load, so either a Zig patch
exposing a placeholder placement or a wire change (the owner's completion carrying the cells its image took, which
Task 23's client then has to measure) is needed, and both need a design of their own; reading the file on the server
too would race the client for a temporary file (`t=t` is deleted by its first reader) and cannot read the client's
shared memory. The divergence is recorded in `server::replies`' module docs. Reported as pre-existing in the revmux
review of Task 20, round 1 (`docs/plans/20261018-cross-platform-sessions.md`).
