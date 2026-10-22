---
worth: yes
where: crates/ngterm-core/src/ghostty_config/model.rs:540
added: 2026-10-17
---
# implement the deferred Ghostty config keys the GTK build honored

The GPUI build accepts every key Ghostty knows (a malformed value counts as libghostty counts it), but its typed
model carries only a subset; the rest reach the "ghostty config key not supported; ignored" arms of
`GhosttyConfig::apply` and change nothing. The keys libghostty's core, renderer and termio honored for the removed
GTK build (font metrics and styles, `minimum-contrast`, `bold-color`, `palette-generate`, `faint-opacity`,
`alpha-blending`, `enquiry-response`, `osc-color-report-format`, `title-report`, `vt-kam-allowed`,
`scroll-to-bottom`, the bell, the clipboard and mouse extras, and more) are lost to a user who sets them.

The user deferred them explicitly on 2026-10-17 («Давай отложим эти ключи», Task 35 review of
`docs/plans/20261014-gpui-frontend.md`), so this is scheduled work, not a value question. The full list, each key's
Ghostty meaning and whether the GTK build honored it, is [docs/ghostty-config-parity.md](../ghostty-config-parity.md);
its "honored by the GTK build" rows are the work, the other-platform and never-honored rows are not.
