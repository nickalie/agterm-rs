//! The terminal: its parser, stream, pages, screens and the terminal state machine, laid out as Ghostty's
//! `src/terminal` is, one module per Ghostty file, so a Ghostty diff maps onto the modules that name its files.
//!
//! Ghostty: `src/terminal` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src` at `RUSTTY_REV` (MIT).
//! Changes: filled task by task (the zig-free-core plan); each module names its own sources.

pub(crate) mod utf8_decoder;
