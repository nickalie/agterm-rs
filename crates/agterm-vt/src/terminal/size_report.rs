//! Terminal size reports written to the PTY: in-band reports (mode 2048) and the XTWINOPS replies.
//!
//! Ghostty: `src/terminal/size_report.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/size_report.rs` at `RUSTTY_REV` (MIT).
//! Changes: the encoder writes to a `core::fmt::Write`; the cell counts are `u16`, Ghostty's `size.CellCountInt`.
//! Drift absorbed: `6104c78f1` (`csi_19_t`, the screen size in characters, answered with the text area's).

use core::fmt::Write;

/// Output formats for terminal size reports written to the PTY.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Style {
    /// In-band size reports (mode 2048)
    Mode2048,
    /// XTWINOPS: report text area size in pixels
    Csi14T,
    /// XTWINOPS: report cell size in pixels
    Csi16T,
    /// XTWINOPS: report text area size in characters
    Csi18T,
    /// XTWINOPS: report screen size in characters
    Csi19T,
}

/// Runtime size values used to encode terminal size reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Size {
    /// Terminal row count in cells.
    pub(crate) rows: u16,

    /// Terminal column count in cells.
    pub(crate) columns: u16,

    /// Width of a single terminal cell in pixels.
    pub(crate) cell_width: u32,

    /// Height of a single terminal cell in pixels.
    pub(crate) cell_height: u32,
}

impl Size {
    /// The text area's width in pixels.
    fn width_pixels(self) -> u64 {
        u64::from(self.columns) * u64::from(self.cell_width)
    }

    /// The text area's height in pixels.
    fn height_pixels(self) -> u64 {
        u64::from(self.rows) * u64::from(self.cell_height)
    }
}

/// Encode a terminal size report sequence.
pub(crate) fn encode<W: Write>(writer: &mut W, style: Style, size: Size) -> core::fmt::Result {
    match style {
        Style::Mode2048 => write!(
            writer,
            "\x1B[48;{};{};{};{}t",
            size.rows,
            size.columns,
            size.height_pixels(),
            size.width_pixels(),
        ),
        Style::Csi14T => write!(writer, "\x1b[4;{};{}t", size.height_pixels(), size.width_pixels()),
        Style::Csi16T => write!(writer, "\x1b[6;{};{}t", size.cell_height, size.cell_width),
        Style::Csi18T => write!(writer, "\x1b[8;{};{}t", size.rows, size.columns),
        // We have no knowledge of the physical screen size so, like VTE, we report the size of the text area in
        // characters.
        Style::Csi19T => write!(writer, "\x1b[9;{};{}t", size.rows, size.columns),
    }
}

#[cfg(test)]
mod tests;
