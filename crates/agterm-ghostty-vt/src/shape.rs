//! Small value types of the terminal API: modes, scrollback limits, colors, mouse shapes.

use crate::{Rgb, ffi};

/// The scrollback limits (`None` removes a limit); the first reached wins, both at page granularity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScrollbackLimits {
    pub max_bytes: Option<usize>,
    pub max_lines: Option<usize>,
}

/// A terminal mode (`modes.h`): an ANSI mode or a DEC private mode number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Mode(pub(crate) ffi::GhosttyMode);

impl Mode {
    /// Synchronized output (DEC private mode 2026).
    pub const SYNC_OUTPUT: Self = Self::dec(2026);
    /// Color scheme reports (DEC private mode 2031): the program wants a report on every light/dark change.
    pub const COLOR_SCHEME_REPORT: Self = Self::dec(2031);

    /// A DEC private mode (`CSI ? n h`).
    #[must_use]
    pub const fn dec(value: u16) -> Self {
        // `ghostty_mode_new` is `static inline` in the header: the value in the low 15 bits, the ANSI flag on top.
        Self(value & 0x7FFF)
    }

    /// An ANSI mode (`CSI n h`).
    #[must_use]
    pub const fn ansi(value: u16) -> Self {
        Self((value & 0x7FFF) | 0x8000)
    }
}

/// The terminal's effective colors (OSC 4/10/11/12 overrides applied over the defaults).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalColors {
    pub foreground: Option<Rgb>,
    pub background: Option<Rgb>,
    pub cursor: Option<Rgb>,
    pub palette: Box<[Rgb; 256]>,
}

/// The mouse pointer shape a program asked for (OSC 22), as CSS cursor names.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum MouseShape {
    #[default]
    Default,
    ContextMenu,
    Help,
    Pointer,
    Progress,
    Wait,
    Cell,
    Crosshair,
    Text,
    VerticalText,
    Alias,
    Copy,
    Move,
    NoDrop,
    NotAllowed,
    Grab,
    Grabbing,
    AllScroll,
    ColResize,
    RowResize,
    NResize,
    EResize,
    SResize,
    WResize,
    NeResize,
    NwResize,
    SeResize,
    SwResize,
    EwResize,
    NsResize,
    NeswResize,
    NwseResize,
    ZoomIn,
    ZoomOut,
}

impl MouseShape {
    /// In `GhosttyMouseShape` order (`GHOSTTY_MOUSE_SHAPE_DEFAULT` = 0 … `ZOOM_OUT` = 33).
    const ALL: [Self; 34] = [
        Self::Default,
        Self::ContextMenu,
        Self::Help,
        Self::Pointer,
        Self::Progress,
        Self::Wait,
        Self::Cell,
        Self::Crosshair,
        Self::Text,
        Self::VerticalText,
        Self::Alias,
        Self::Copy,
        Self::Move,
        Self::NoDrop,
        Self::NotAllowed,
        Self::Grab,
        Self::Grabbing,
        Self::AllScroll,
        Self::ColResize,
        Self::RowResize,
        Self::NResize,
        Self::EResize,
        Self::SResize,
        Self::WResize,
        Self::NeResize,
        Self::NwResize,
        Self::SeResize,
        Self::SwResize,
        Self::EwResize,
        Self::NsResize,
        Self::NeswResize,
        Self::NwseResize,
        Self::ZoomIn,
        Self::ZoomOut,
    ];

    pub(crate) fn from_raw(raw: ffi::GhosttyMouseShape) -> Self {
        usize::try_from(raw)
            .ok()
            .and_then(|index| Self::ALL.get(index).copied())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_shapes_follow_the_c_enum() {
        assert_eq!(
            MouseShape::from_raw(ffi::GHOSTTY_MOUSE_SHAPE_DEFAULT),
            MouseShape::Default
        );
        assert_eq!(
            MouseShape::from_raw(ffi::GHOSTTY_MOUSE_SHAPE_POINTER),
            MouseShape::Pointer
        );
        assert_eq!(MouseShape::from_raw(ffi::GHOSTTY_MOUSE_SHAPE_TEXT), MouseShape::Text);
        assert_eq!(
            MouseShape::from_raw(ffi::GHOSTTY_MOUSE_SHAPE_ZOOM_OUT),
            MouseShape::ZoomOut
        );
        assert_eq!(MouseShape::from_raw(-1), MouseShape::Default);
        assert_eq!(MouseShape::from_raw(34), MouseShape::Default);
    }

    #[test]
    fn modes_pack_like_ghostty_mode_new() {
        assert_eq!(Mode::SYNC_OUTPUT.0, 2026);
        assert_eq!(Mode::ansi(4).0, 0x8004);
    }
}
