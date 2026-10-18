//! Types and functions related to Kitty protocols.
//!
//! Ghostty: `src/terminal/kitty.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/kitty/mod.rs` at `RUSTTY_REV` (MIT).
//! Changes: filled task by task (the zig-free-core plan). The graphics command parser is its own module here, which
//! the APC handler imports directly; Ghostty's `graphics` aggregate, which also re-exports the command's execution,
//! comes with that execution.

pub(crate) mod color;
pub(crate) mod graphics_command;
pub(crate) mod key;
