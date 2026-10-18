//! ANSI control characters and the enums of the sequences the stream decodes beside CSI's own: cursor styles, the
//! status display, modifyOtherKeys formats, protection modes.
//!
//! Ghostty: `src/terminal/ansi.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/ansi.rs` at `RUSTTY_REV` (MIT).
//! Changes: Zig's non-exhaustive `enum(u7)` keeps its unknown values in an `Other` variant, with `from_int` for
//! `@enumFromInt`; Zig's `LibEnum`s are plain enums. `RenditionAspect` (used nowhere) and `StatusLineType` (C API
//! only) are not here.

/// C0 (7-bit) control characters from ANSI.
///
/// This is not complete, control characters are only added to this
/// as the terminal emulator handles them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum C0 {
    /// Null
    Nul, // = 0x00
    /// Start of heading
    Soh, // = 0x01
    /// Start of text
    Stx, // = 0x02
    /// Enquiry
    Enq, // = 0x05
    /// Bell
    Bel, // = 0x07
    /// Backspace
    Bs, // = 0x08
    // Horizontal tab
    Ht, // = 0x09
    /// Line feed
    Lf, // = 0x0A
    /// Vertical Tab
    Vt, // = 0x0B
    /// Form feed
    Ff, // = 0x0C
    /// Carriage return
    Cr, // = 0x0D
    /// Shift out
    So, // = 0x0E
    /// Shift in
    Si, // = 0x0F

    // Non-exhaustive so that @intToEnum never fails since the inputs are
    // user-generated.
    Other(u8),
}

impl C0 {
    /// Construct from a u7 value (`@intToEnum`, never fails).
    pub(crate) fn from_int(v: u8) -> C0 {
        match v {
            0x00 => C0::Nul,
            0x01 => C0::Soh,
            0x02 => C0::Stx,
            0x05 => C0::Enq,
            0x07 => C0::Bel,
            0x08 => C0::Bs,
            0x09 => C0::Ht,
            0x0A => C0::Lf,
            0x0B => C0::Vt,
            0x0C => C0::Ff,
            0x0D => C0::Cr,
            0x0E => C0::So,
            0x0F => C0::Si,
            _ => C0::Other(v),
        }
    }
}

/// Possible cursor styles (ESC [ q)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CursorStyle {
    Default,
    BlinkingBlock,
    SteadyBlock,
    BlinkingUnderline,
    SteadyUnderline,
    BlinkingBar,
    SteadyBar,
}

/// The display to target for status updates (DECSASD).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StatusDisplay {
    Main,
    StatusLine,
}

/// The possible modify key formats to ESC[>{a};{b}m
/// Note: this is not complete, we should add more as we support more
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModifyKeyFormat {
    Legacy,
    CursorKeys,
    FunctionKeys,
    OtherKeysNone,
    OtherKeysNumericExcept,
    OtherKeysNumeric,
}

/// The protection modes that can be set for the terminal. See DECSCA and
/// ESC V, W.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProtectedMode {
    Off,
    Iso, // ESC V, W
    Dec, // CSI Ps " q
}
