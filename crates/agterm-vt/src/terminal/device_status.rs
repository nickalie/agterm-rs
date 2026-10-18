//! Device status reports (DSR, `CSI n` and `CSI ? n`): the requests the terminal answers, and the encoders of its
//! color scheme and visibility reports.
//!
//! Ghostty: `src/terminal/device_status.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/device_status.rs` at `RUSTTY_REV` (MIT).
//! Changes: Zig builds `Request` at comptime from the entry list, its values the packed `Tag` (a `u15` value and the
//! question bit on top); here it is a plain enum with those values as discriminants, and [`req_from_int`] matches the
//! same list. The encoders append to a byte buffer. Drift absorbed: `fbda61b87` (DECXCPR, `CSI ? 6 n`, is
//! `cursor_position_extended`).

/// The color scheme reported in response to a CSI ? 996 n query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ColorScheme {
    Light,
    Dark,
}

/// The visibility state reported in response to a CSI ? 998 n query or when DEC mode 2033 is enabled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Visibility {
    PotentiallyVisible,
    NotVisible,
}

/// Maximum number of bytes that [`encode_color_scheme_report`] will write.
pub(crate) const MAX_COLOR_SCHEME_REPORT_ENCODE_SIZE: usize = "\x1B[?997;1n".len();

/// Encode a color scheme report response for CSI ? 996 n queries.
pub(crate) fn encode_color_scheme_report(writer: &mut Vec<u8>, scheme: ColorScheme) {
    writer.extend_from_slice(match scheme {
        ColorScheme::Dark => b"\x1B[?997;1n",
        ColorScheme::Light => b"\x1B[?997;2n",
    });
}

/// Maximum number of bytes that [`encode_visibility_report`] will write.
pub(crate) const MAX_VISIBILITY_REPORT_ENCODE_SIZE: usize = "\x1B[?999;2n".len();

/// Encode a visibility report response for CSI ? 998 n queries and DEC mode 2033 notifications.
pub(crate) fn encode_visibility_report(writer: &mut Vec<u8>, visibility: Visibility) {
    writer.extend_from_slice(match visibility {
        Visibility::PotentiallyVisible => b"\x1B[?999;1n",
        Visibility::NotVisible => b"\x1B[?999;2n",
    });
}

/// The question bit of a request's tag: the request was `CSI ? n`.
const QUESTION: u16 = 1 << 15;

/// The tag of request `n` in its `CSI ? n` form.
const fn question(n: u16) -> u16 {
    n | QUESTION
}

/// The device status requests the terminal answers. Each value is Zig's packed `Tag`: the request number in the low
/// 15 bits and the question bit on top.
#[repr(u16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Request {
    OperatingStatus = 5,
    CursorPosition = 6,
    /// DECXCPR: the "?" form of value 6 reports the extended cursor position (adds a page number) rather than the
    /// plain CPR.
    CursorPositionExtended = question(6),
    ColorScheme = question(996),
    Visibility = question(998),
}

impl Request {
    /// The full list of device status requests: the "question" bit determines if each is valid with or without the
    /// "?" prefix.
    const ALL: [Self; 5] = [
        Self::OperatingStatus,
        Self::CursorPosition,
        Self::CursorPositionExtended,
        Self::ColorScheme,
        Self::Visibility,
    ];

    /// The request's tag (`@intFromEnum`): its number, with the question bit for a "?" request.
    pub(crate) const fn tag(self) -> u16 {
        self as u16
    }
}

/// The request for the number `v` with or without the "?" prefix, if the terminal answers it.
pub(crate) fn req_from_int(v: u16, question: bool) -> Option<Request> {
    // A number with the top bit set is not a 15-bit request number.
    if v & QUESTION != 0 {
        return None;
    }
    let tag = if question { v | QUESTION } else { v };
    Request::ALL.into_iter().find(|req| req.tag() == tag)
}

#[cfg(test)]
mod tests;
