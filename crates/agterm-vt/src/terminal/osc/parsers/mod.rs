//! The parsers of each OSC's body, which `Parser::end` dispatches to by the OSC number it read.
//!
//! Ghostty: `src/terminal/osc/parsers.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/osc/parsers/mod.rs` at `RUSTTY_REV` (MIT).
//! Changes: none.

pub(crate) mod change_window_icon;
pub(crate) mod change_window_title;
pub(crate) mod clipboard_operation;
pub(crate) mod color;
pub(crate) mod context_signal;
pub(crate) mod hyperlink;
pub(crate) mod iterm2;
pub(crate) mod kitty_clipboard_protocol;
pub(crate) mod kitty_color;
pub(crate) mod kitty_desktop_notification;
pub(crate) mod kitty_dnd_protocol;
pub(crate) mod kitty_text_sizing;
pub(crate) mod mouse_shape;
pub(crate) mod osc9;
pub(crate) mod program_status;
pub(crate) mod report_pwd;
pub(crate) mod rxvt_extension;
pub(crate) mod semantic_prompt;
pub(crate) mod unknown;
