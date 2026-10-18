//! The terminal: its parser, stream, pages, screens and the terminal state machine, laid out as Ghostty's
//! `src/terminal` is, one module per Ghostty file, so a Ghostty diff maps onto the modules that name its files.
//!
//! Ghostty: `src/terminal` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src` at `RUSTTY_REV` (MIT).
//! Changes: filled task by task (the zig-free-core plan); each module names its own sources.

pub(crate) mod ansi;
pub(crate) mod apc;
pub(crate) mod charsets;
pub(crate) mod color;
pub(crate) mod csi;
pub(crate) mod device_attributes;
pub(crate) mod device_status;
pub(crate) mod fraction;
pub(crate) mod kitty;
pub(crate) mod modes;
pub(crate) mod mouse;
pub(crate) mod osc;
pub(crate) mod parse_table;
pub(crate) mod parser;
pub(crate) mod sgr;
pub(crate) mod size_report;
pub(crate) mod stream;
pub(crate) mod stream_continuation;
pub(crate) mod utf8_decoder;
pub(crate) mod x11_color;
