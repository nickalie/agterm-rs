//! The CSI command parameters the stream decodes: erase modes, tab clear, size reports.
//!
//! Ghostty: `src/terminal/csi.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/csi.rs` at `RUSTTY_REV` (MIT).
//! Changes: Zig's non-exhaustive `enum(u8)`s keep their unknown values in an `Other` variant, with `from_int`;
//! `TitlePushPop`, which nothing uses (the stream hands the title stack index over directly), is not here. Drift
//! absorbed: `6104c78f1` (`csi_19_t`, CSI 19 t).

/// Modes for the ED CSI command.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EraseDisplay {
    Below = 0,
    Above = 1,
    Complete = 2,
    Scrollback = 3,

    /// This is an extension added by Kitty to move the viewport into the
    /// scrollback and then erase the display.
    ScrollComplete = 22,
}

impl EraseDisplay {
    /// The mode of a parameter (`std.meta.intToEnum`), `None` for an unknown one.
    pub(crate) const fn from_int(v: u16) -> Option<Self> {
        Some(match v {
            0 => Self::Below,
            1 => Self::Above,
            2 => Self::Complete,
            3 => Self::Scrollback,
            22 => Self::ScrollComplete,
            _ => return None,
        })
    }
}

/// Modes for the EL CSI command.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EraseLine {
    Right = 0,
    Left = 1,
    Complete = 2,
    RightUnlessPendingWrap = 4,

    // Non-exhaustive so that @intToEnum never fails since the inputs are
    // user-generated.
    Other(u8),
}

impl EraseLine {
    /// The mode of a byte (`@enumFromInt`).
    pub(crate) const fn from_int(v: u8) -> Self {
        match v {
            0 => Self::Right,
            1 => Self::Left,
            2 => Self::Complete,
            4 => Self::RightUnlessPendingWrap,
            _ => Self::Other(v),
        }
    }
}

/// Modes for the TBC (tab clear) command.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TabClear {
    Current = 0,
    All = 3,

    // Non-exhaustive so that @intToEnum never fails since the inputs are
    // user-generated.
    Other(u8),
}

impl TabClear {
    /// The mode of a byte (`std.enums.fromInt`).
    pub(crate) const fn from_int(v: u8) -> Self {
        match v {
            0 => Self::Current,
            3 => Self::All,
            _ => Self::Other(v),
        }
    }
}

/// Style formats for terminal size reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SizeReportStyle {
    // XTWINOPS
    Csi14T,
    Csi16T,
    Csi18T,
    Csi19T,
    Csi21T,
}
