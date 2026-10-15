---
worth: later
where: crates/agterm-gpui/src/terminal/kitty/placeholder.rs:1
added: 2026-10-15
---
# close the Kitty virtual-placement gaps libghostty-vt's C API leaves: native patch, upstream API, or a scope exception

The GPUI build draws Kitty Unicode-placeholder (virtual, `U=1`) placements itself (Task 16 review fix:
`agterm-term/src/placeholder.rs` scans the frame's U+10EEEE cells, `agterm-gpui/src/terminal/kitty/placeholder.rs`
ports Ghostty's `renderPlacement`), but two things the GTK build's libghostty does cannot be reproduced, because
libghostty-vt's C API at `LIBGHOSTTY_VT_REV` (and at ghostty main on 2026-10-15: the header is byte-identical) does
not expose the data:

1. **Placement-id namespace.** Storage keys placements by `(image, internal|external, id)`; the C API's
   `PLACEMENT_DATA_PLACEMENT_ID` reports the number only (`src/terminal/c/kitty_graphics.zig:401`). Ghostty's
   `placeholderTarget` (`graphics_storage.zig:882`) looks a named id up among client ids only, and with none named
   prefers a client's virtual placement over a terminal-assigned one (`PlacementId.preferredOver`). The GPUI build
   resolves by number: a named id can hit a terminal-assigned placement, and the lowest number wins the fallback.
2. **Relative placements rooted at a virtual placement** (`P=`/`Q=`/`H=`/`V=`). Ghostty positions them at the root's
   minimum placeholder cell plus the accumulated chain offsets (`src/renderer/image.zig` `pending_relative`). The C
   API returns them not visible (`kitty_graphics.zig:629`) with no root key or offsets, so the GPUI build does not
   draw them.

Recovering either from the APC stream ahead of `vt_write` was ruled out (Codex): internal ids and `P=` parents are
decided inside storage after validation, replacement and eviction, so a pre-scan cannot know the committed state.

Options: (a) an upstream additive API (a namespace field per placement; per relative placement its resolution status,
root key with namespace and accumulated signed offsets), then a pin bump to that commit; (b) a narrowly scoped local
patch under `build/patches/ghostty-vt` — the first libghostty-vt patch, so `build-ghostty-vt.sh` applies patches,
`ghostty_vt_prefix` keys over them and the bindings are regenerated; (c) the user accepts the gap as out of scope.

Decide before Task 34 switches frontends: only working support or the user's explicit scope exception closes it,
"waiting for upstream" does not. Meanwhile the gaps are documented in the code and the plan (Task 16 result) and
nothing else depends on them. Discussion: `~/.local/state/plan-exec-revmux/gpui-frontend/discussions/fixer-task-16-kitty-c-api-gaps/transcript.md`.
