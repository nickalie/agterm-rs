//! Mouse reporting modes and the pointer shapes a program can ask for (OSC 22).
//!
//! Ghostty: `src/terminal/mouse.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/mouse.rs` at `RUSTTY_REV` (MIT).
//! Changes: Zig's `LibEnum`s are plain enums; the comptime string map is a `match`. The `GObject` type and the
//! `ghostty.h` check belong to the app and the C API, which are not here. `Format` (the mouse report format) comes
//! with the terminal's mouse state, its only user.

/// The event types that can be reported for mouse-related activities. These are all mutually exclusive (hence in a
/// single enum).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    None,
    /// 9
    X10,
    /// 1000
    Normal,
    /// 1002
    Button,
    /// 1003
    Any,
}

/// Returns true if this event sends motion events.
pub(crate) fn event_sends_motion(event: Event) -> bool {
    event == Event::Button || event == Event::Any
}

/// The possible cursor shapes. Not all app runtimes support these shapes. The shapes are always based on the W3C
/// supported cursor styles so we can have a cross platform list.
//
// Must be kept in sync with ghostty_cursor_shape_e
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shape {
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

impl Shape {
    /// Build cursor shape from string or null if its unknown.
    pub(crate) fn from_string(v: &[u8]) -> Option<Self> {
        Some(match v {
            // W3C
            b"default" => Self::Default,
            b"context-menu" => Self::ContextMenu,
            b"help" => Self::Help,
            b"pointer" => Self::Pointer,
            b"progress" => Self::Progress,
            b"wait" => Self::Wait,
            b"cell" => Self::Cell,
            b"crosshair" => Self::Crosshair,
            b"text" => Self::Text,
            b"vertical-text" => Self::VerticalText,
            b"alias" => Self::Alias,
            b"copy" => Self::Copy,
            b"move" => Self::Move,
            b"no-drop" => Self::NoDrop,
            b"not-allowed" => Self::NotAllowed,
            b"grab" => Self::Grab,
            b"grabbing" => Self::Grabbing,
            b"all-scroll" => Self::AllScroll,
            b"col-resize" => Self::ColResize,
            b"row-resize" => Self::RowResize,
            b"n-resize" => Self::NResize,
            b"e-resize" => Self::EResize,
            b"s-resize" => Self::SResize,
            b"w-resize" => Self::WResize,
            b"ne-resize" => Self::NeResize,
            b"nw-resize" => Self::NwResize,
            b"se-resize" => Self::SeResize,
            b"sw-resize" => Self::SwResize,
            b"ew-resize" => Self::EwResize,
            b"ns-resize" => Self::NsResize,
            b"nesw-resize" => Self::NeswResize,
            b"nwse-resize" => Self::NwseResize,
            b"zoom-in" => Self::ZoomIn,
            b"zoom-out" => Self::ZoomOut,
            // xterm/foot
            _ => return Self::from_xcursor_name(v),
        })
    }

    /// The xterm and foot (X cursor font) names of the shapes.
    fn from_xcursor_name(v: &[u8]) -> Option<Self> {
        Some(match v {
            b"left_ptr" => Self::Default,
            b"question_arrow" => Self::Help,
            b"hand" => Self::Pointer,
            b"left_ptr_watch" => Self::Progress,
            b"watch" => Self::Wait,
            b"cross" => Self::Crosshair,
            b"xterm" => Self::Text,
            b"dnd-link" => Self::Alias,
            b"dnd-copy" => Self::Copy,
            b"dnd-move" => Self::Move,
            b"dnd-no-drop" => Self::NoDrop,
            b"crossed_circle" => Self::NotAllowed,
            b"hand1" => Self::Grab,
            b"right_side" => Self::EResize,
            b"top_side" => Self::NResize,
            b"top_right_corner" => Self::NeResize,
            b"top_left_corner" => Self::NwResize,
            b"bottom_side" => Self::SResize,
            b"bottom_right_corner" => Self::SeResize,
            b"bottom_left_corner" => Self::SwResize,
            b"left_side" => Self::WResize,
            b"fleur" => Self::AllScroll,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests;
