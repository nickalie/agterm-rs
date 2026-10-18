//! Kitty's color protocol (OSC 21): its requests and the keys they name.
//!
//! Ghostty: `src/terminal/kitty/color.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/kitty/color.rs` at `RUSTTY_REV` (MIT).
//! Changes: the request list is a `Vec` grown fallibly ([`Osc::append`]), which frees itself (Zig's `deinit`); the
//! C form (`C`, `cval`) belongs to the C API, which is not here.

use crate::mem::alloc::{OutOfMemory, TryVecExt};
use crate::terminal::color::Rgb;
use crate::terminal::osc::Terminator;

/// A parsed OSC 21 sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Osc {
    /// list of requests
    pub(crate) list: Vec<Request>,

    /// We must reply with the same string terminator (ST) as used in the request.
    pub(crate) terminator: Terminator,
}

/// One request of an OSC 21 sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Request {
    Query(Kind),
    Set { key: Kind, color: Rgb },
    Reset(Kind),
}

impl Osc {
    /// No requests yet, answered with `terminator`.
    pub(crate) const fn new(terminator: Terminator) -> Self {
        Self {
            list: Vec::new(),
            terminator,
        }
    }

    /// Append a request (`try self.list.append(alloc, request)`).
    pub(crate) fn append(&mut self, request: Request) -> Result<(), OutOfMemory> {
        self.list.try_push(request)
    }
}

/// The special (non-palette) colors the protocol names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Special {
    Foreground,
    Background,
    SelectionForeground,
    SelectionBackground,
    Cursor,
    CursorText,
    VisualBell,
    SecondTransparentBackground,
}

impl Special {
    /// The number of special colors (`@typeInfo(Special).@"enum".fields.len`).
    const COUNT: usize = 8;

    /// The special color of the field name `key` (`std.meta.stringToEnum`).
    fn from_name(key: &[u8]) -> Option<Self> {
        Some(match key {
            b"foreground" => Self::Foreground,
            b"background" => Self::Background,
            b"selection_foreground" => Self::SelectionForeground,
            b"selection_background" => Self::SelectionBackground,
            b"cursor" => Self::Cursor,
            b"cursor_text" => Self::CursorText,
            b"visual_bell" => Self::VisualBell,
            b"second_transparent_background" => Self::SecondTransparentBackground,
            _ => return None,
        })
    }

    /// The field name (`@tagName`).
    const fn name(self) -> &'static str {
        match self {
            Self::Foreground => "foreground",
            Self::Background => "background",
            Self::SelectionForeground => "selection_foreground",
            Self::SelectionBackground => "selection_background",
            Self::Cursor => "cursor",
            Self::CursorText => "cursor_text",
            Self::VisualBell => "visual_bell",
            Self::SecondTransparentBackground => "second_transparent_background",
        }
    }
}

/// The color a request names: a palette index or a special color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Palette(u8),
    Special(Special),
}

impl Kind {
    /// The number of keys (`maxInt(u8)` plus the special colors), the bound on a sequence's key count.
    pub(crate) const MAX: usize = u8::MAX as usize + Special::COUNT;

    /// The key `key` names: a special color's field name, or a decimal palette index.
    pub(crate) fn parse(key: &[u8]) -> Option<Self> {
        if let Some(s) = Special::from_name(key) {
            return Some(Self::Special(s));
        }
        crate::parse_int::parse_int::<u8, 10>(key).ok().map(Self::Palette)
    }

    /// Returns true when a terminal has built-in state for this key.
    ///
    /// Unsupported special colors may still be valid Kitty protocol keys, but libghostty-vt cannot report them because
    /// Terminal does not store them.
    pub(crate) fn has_terminal_query_color(self) -> bool {
        match self {
            Self::Palette(_) => true,
            Self::Special(special) => matches!(special, Special::Foreground | Special::Background | Special::Cursor),
        }
    }
}

impl core::fmt::Display for Kind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Palette(p) => write!(f, "{p}"),
            Self::Special(s) => f.write_str(s.name()),
        }
    }
}

#[cfg(test)]
mod tests;
