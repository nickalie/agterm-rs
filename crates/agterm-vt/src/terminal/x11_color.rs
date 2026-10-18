//! The X11 color names (`rgb.txt`), which color specifications may use (`RGB::parse`).
//!
//! Ghostty: `src/terminal/x11_color.zig` and `src/terminal/res/rgb.txt` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/x11_color.rs` at `RUSTTY_REV` (MIT).
//! Changes: Zig parses `rgb.txt` at comptime into `entries` and a case-insensitive `StaticStringMap`; here the
//! entries are parsed once, on first use, and [`get`] compares names ASCII-case-insensitively over them, so a lookup
//! allocates nothing. The entries keep `rgb.txt`'s order, as Zig's do. `rgb.txt` itself is X.Org's, byte for byte as
//! Ghostty ships it; its license is in the repository's `NOTICE`.

use std::sync::OnceLock;

use crate::terminal::color::Rgb;

/// A single X11 color entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    /// Color name, as `rgb.txt` writes it.
    pub(crate) name: &'static str,
    pub(crate) color: Rgb,
}

/// All X11 colors in rgb.txt file order.
pub(crate) fn entries() -> &'static [Entry] {
    static ENTRIES: OnceLock<Vec<Entry>> = OnceLock::new();
    ENTRIES.get_or_init(|| DATA.lines().filter_map(parse_line).collect())
}

/// The color of the X11 color `name`, its case ignored, or `None` for an unknown name.
pub(crate) fn get(name: &[u8]) -> Option<Rgb> {
    entries()
        .iter()
        .find(|entry| entry.name.as_bytes().eq_ignore_ascii_case(name))
        .map(|entry| entry.color)
}

/// One `rgb.txt` line: three right-aligned decimal components in columns 0-2, 4-6 and 8-10, then the name from
/// column 12. This is not very robust parsing, because we expect a very exact format for rgb.txt; the unit tests
/// check that every line of it parses. A carriage return is trimmed, since git may convert rgb.txt to CRLF on Windows
/// checkouts.
fn parse_line(raw_line: &'static str) -> Option<Entry> {
    let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
    let component = |range: core::ops::Range<usize>| line.get(range)?.trim_matches(' ').parse::<u8>().ok();
    let color = Rgb {
        r: component(0..3)?,
        g: component(4..7)?,
        b: component(8..11)?,
    };
    let name = line.get(12..)?.trim_matches([' ', '\t']);
    (!name.is_empty()).then_some(Entry { name, color })
}

/// This is the rgb.txt file from the X11 project. This was last sourced from this location:
/// <https://gitlab.freedesktop.org/xorg/app/rgb>. This data is licensed under the MIT/X11 license while this file is
/// licensed under the same license as Ghostty.
const DATA: &str = include_str!("res/rgb.txt");

#[cfg(test)]
mod tests;
