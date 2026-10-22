---
worth: yes
where: crates/ngterm-gpui/src/terminal/mouse.rs
added: 2026-10-15
---
# cursor-click-to-move needs the prompt-click state libghostty-vt does not expose

`cursor-click-to-move = true` (Ghostty's `Surface.maybePromptClick`) is not carried out by the GPUI build; ngterm's
own default is `false` (`ghostty-defaults.conf`), so only a user who turns it on misses it. The GTK build had it
through full libghostty. Found in Task 14 of `docs/plans/20261014-gpui-frontend.md` (review round 2).

Ghostty decides a prompt click from:

- `screen.semantic_prompt.click`, set from the `click_events=1|2` and `cl=line|m|v|w` options of the latest OSC 133
  prompt-start (`Terminal.zig`, `.prompt_start`): **missing** at `LIBGHOSTTY_VT_REV`. `GhosttyTerminalSemanticPrompt`
  (the `GHOSTTY_TERMINAL_OPT_SEMANTIC_PROMPT` callback) carries kind, prompt kind, exit code, command and error, no
  options; the standalone `osc.h` parser exposes no OSC 133 data; there is no `GHOSTTY_TERMINAL_DATA_*` for it.
- the cursor's own semantic content (`screen.cursor.semantic_content`, the pen): **missing**; only the content of
  the cell under the cursor is readable (`GHOSTTY_CELL_DATA_SEMANTIC_CONTENT` through a grid ref).
- whether the cursor is at a prompt (`GHOSTTY_TERMINAL_DATA_CURSOR_AT_PROMPT`), each row's prompt mark
  (`GHOSTTY_ROW_DATA_SEMANTIC_PROMPT`) and each cell's semantic content: available.

With those, the move itself (`Screen.promptClickMove` → `promptClickLine`: arrow keys counted over input cells and
prompt-continuation rows, or `CSI < 0 ; x ; y M` for `click_events`, absolute or relative to the prompt row) is a
straightforward port over grid refs.

Ways to close it: an upstream libghostty-vt API (a `GHOSTTY_TERMINAL_DATA_SEMANTIC_CLICK` datum, or the options on
the semantic-prompt event, plus the cursor's pen content), then port `promptClickLine` into `ngterm-term`; or, without
waiting, have the stream scanner (`ngterm-ghostty-vt`'s `scan.rs`, which already watches OSC 52 reads) track the OSC 133
prompt-start options per screen as Ghostty does and use the cell under the cursor in place of the pen. Meanwhile a
`true` value is accepted by the config and has no effect.
