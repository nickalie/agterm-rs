//! Colors: RGB values and their xterm specifications, the 256-color palette with its program changes, the named,
//! special and dynamic colors.
//!
//! Ghostty: `src/terminal/color.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/color.rs` at `RUSTTY_REV` (MIT).
//! Changes: the dynamic palette's own copy of a non-default palette is a `Box` (Zig's allocator-owned pointer),
//! allocated fallibly; `std.StaticBitSet(256)` is [`PaletteMask`]. Zig's non-exhaustive `Name` keeps its unnamed values
//! in [`Name::Other`]. Not taken: what only the C API reaches and agterm never calls (`generate256Color` with its
//! CIELAB, `parsePaletteEntry`, `luminance`, `perceivedLuminance`, `contrast`, the `C` and `cval` forms).

use crate::mem::alloc::{OutOfMemory, try_box};
use crate::terminal::x11_color;

/// The default palette.
pub(crate) static DEFAULT: Palette = build_default();

/// The default palette: the 16 named colors, the 6x6x6 cube, then the 24-step gray ramp.
const fn build_default() -> Palette {
    let mut result: Palette = [Rgb { r: 0, g: 0, b: 0 }; 256];

    // Named values
    let mut i: usize = 0;
    while i < 16 {
        result[i] = NAMED_DEFAULTS[i];
        i += 1;
    }

    // Cube
    let mut r: u8 = 0;
    while r < 6 {
        let mut g: u8 = 0;
        while g < 6 {
            let mut b: u8 = 0;
            while b < 6 {
                result[i] = Rgb {
                    r: if r == 0 { 0 } else { r * 40 + 55 },
                    g: if g == 0 { 0 } else { g * 40 + 55 },
                    b: if b == 0 { 0 } else { b * 40 + 55 },
                };
                i += 1;
                b += 1;
            }
            g += 1;
        }
        r += 1;
    }

    // Gray ramp
    let mut value: u8 = 8;
    while i < 256 {
        result[i] = Rgb {
            r: value,
            g: value,
            b: value,
        };
        value += 10;
        i += 1;
    }

    result
}

/// Palette is the 256 color palette.
pub(crate) type Palette = [Rgb; 256];

/// Mask that can be used to set which palette indexes were set (`std.StaticBitSet(256)`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PaletteMask {
    words: [u64; 4],
}

impl PaletteMask {
    /// All bits unset (`initEmpty`).
    pub(crate) const fn init_empty() -> Self {
        Self { words: [0; 4] }
    }

    /// Set the bit of palette index `idx`.
    pub(crate) fn set(&mut self, idx: u8) {
        self.words[usize::from(idx / 64)] |= 1_u64 << (idx % 64);
    }

    /// Clear the bit of palette index `idx`.
    pub(crate) fn unset(&mut self, idx: u8) {
        self.words[usize::from(idx / 64)] &= !(1_u64 << (idx % 64));
    }

    /// Whether the bit of palette index `idx` is set.
    pub(crate) fn is_set(&self, idx: u8) -> bool {
        (self.words[usize::from(idx / 64)] >> (idx % 64)) & 1 == 1
    }

    /// The number of set bits.
    pub(crate) fn count(&self) -> u32 {
        self.words.iter().map(|w| w.count_ones()).sum()
    }

    /// The set palette indexes in ascending order (`iterator(.{})`).
    pub(crate) fn iter_set(&self) -> impl Iterator<Item = u8> + '_ {
        (0..=u8::MAX).filter(move |&i| self.is_set(i))
    }
}

/// A palette that can have its colors changed and reset. Purposely built for terminal color operations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DynamicPalette {
    /// The current palette including any user modifications.
    pub(crate) current: Palette,

    /// The original/default palette values: the shared built-in default palette unless a different default was given
    /// (see [`DynamicPalette::change_default`]), in which case this palette's own copy. Sharing the built-in default
    /// saves a full palette copy for every terminal.
    original: Option<Box<Palette>>,

    /// A bitset where each bit represents whether the corresponding palette index has been modified from its default
    /// value.
    pub(crate) mask: PaletteMask,
}

impl Default for DynamicPalette {
    /// A dynamic palette using the built-in default palette. This owns no memory.
    fn default() -> Self {
        Self {
            current: DEFAULT,
            original: None,
            mask: PaletteMask::init_empty(),
        }
    }
}

impl DynamicPalette {
    /// Initialize a dynamic palette with a default palette. No memory is allocated if `def` is the built-in default
    /// palette.
    pub(crate) fn init(def: &Palette) -> Result<Self, OutOfMemory> {
        let mut result = Self::default();
        result.change_default(def)?;
        Ok(result)
    }

    /// The original/default palette values.
    pub(crate) fn original(&self) -> &Palette {
        self.original.as_deref().unwrap_or(&DEFAULT)
    }

    /// Whether the original palette is the shared built-in default (no copy is owned).
    pub(crate) fn original_is_shared(&self) -> bool {
        self.original.is_none()
    }

    /// Set a custom color at the given palette index.
    pub(crate) fn set(&mut self, idx: u8, color: Rgb) {
        self.current[usize::from(idx)] = color;
        self.mask.set(idx);
    }

    /// Reset the color at the given palette index to its original value.
    pub(crate) fn reset(&mut self, idx: u8) {
        self.current[usize::from(idx)] = self.original()[usize::from(idx)];
        self.mask.unset(idx);
    }

    /// Reset all colors to their original values.
    pub(crate) fn reset_all(&mut self) {
        self.current = *self.original();
        self.mask = PaletteMask::init_empty();
    }

    /// Change the default palette, but preserve the changed values.
    ///
    /// The built-in default palette is shared rather than copied, so changing back to it releases any owned copy (see
    /// [`DynamicPalette::reset_default`]). Any other default is copied into memory owned by this palette, allocated on
    /// the first change and reused after that. On allocation failure nothing changes.
    pub(crate) fn change_default(&mut self, def: &Palette) -> Result<(), OutOfMemory> {
        if *def == DEFAULT {
            self.reset_default();
            return Ok(());
        }
        match &mut self.original {
            Some(owned) => **owned = *def,
            None => self.original = Some(try_box(*def)?),
        }
        self.apply_default(def);
        Ok(())
    }

    /// Change the default palette back to the built-in default palette, preserving the changed values. This releases
    /// any owned copy and never allocates, so it is safe to use as the fallback when
    /// [`DynamicPalette::change_default`] fails.
    pub(crate) fn reset_default(&mut self) {
        self.original = None;
        self.apply_default(&DEFAULT);
    }

    /// Rebuild the current palette from a new default, preserving the changed values.
    fn apply_default(&mut self, def: &Palette) {
        // Fast path, the palette is usually not changed.
        if self.mask.count() == 0 {
            self.current = *def;
            return;
        }

        // There are usually less set than unset, so iterate over the changed values and override them.
        let mut current = *def;
        for idx in self.mask.iter_set() {
            current[usize::from(idx)] = self.current[usize::from(idx)];
        }
        self.current = current;
    }
}

/// RGB value that can be changed and reset. This can also be totally unset in every way, in which case the caller can
/// determine their own ultimate default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DynamicRGB {
    pub(crate) override_: Option<Rgb>,
    pub(crate) default: Option<Rgb>,
}

impl DynamicRGB {
    /// Neither an override nor a default.
    pub(crate) const UNSET: Self = Self {
        override_: None,
        default: None,
    };

    /// No override over the default `def`.
    pub(crate) fn init(def: Rgb) -> Self {
        Self {
            override_: None,
            default: Some(def),
        }
    }

    /// The override, else the default.
    pub(crate) fn get(self) -> Option<Rgb> {
        self.override_.or(self.default)
    }

    /// Override the color.
    pub(crate) fn set(&mut self, color: Rgb) {
        self.override_ = Some(color);
    }

    /// Drop the override.
    pub(crate) fn reset(&mut self) {
        self.override_ = None;
    }
}

/// Color names in the standard 8 or 16 color palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Name {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    BrightBlack,
    BrightRed,
    BrightGreen,
    BrightYellow,
    BrightBlue,
    BrightMagenta,
    BrightCyan,
    BrightWhite,

    /// Remainders are valid unnamed values in the 256 color palette.
    Other(u8),
}

/// The named colors in their palette order.
const NAMES: [Name; 16] = [
    Name::Black,
    Name::Red,
    Name::Green,
    Name::Yellow,
    Name::Blue,
    Name::Magenta,
    Name::Cyan,
    Name::White,
    Name::BrightBlack,
    Name::BrightRed,
    Name::BrightGreen,
    Name::BrightYellow,
    Name::BrightBlue,
    Name::BrightMagenta,
    Name::BrightCyan,
    Name::BrightWhite,
];

/// The default colors of the 16 named colors, in their palette order.
const NAMED_DEFAULTS: [Rgb; 16] = [
    Rgb::new(0x1D, 0x1F, 0x21),
    Rgb::new(0xCC, 0x66, 0x66),
    Rgb::new(0xB5, 0xBD, 0x68),
    Rgb::new(0xF0, 0xC6, 0x74),
    Rgb::new(0x81, 0xA2, 0xBE),
    Rgb::new(0xB2, 0x94, 0xBB),
    Rgb::new(0x8A, 0xBE, 0xB7),
    Rgb::new(0xC5, 0xC8, 0xC6),
    Rgb::new(0x66, 0x66, 0x66),
    Rgb::new(0xD5, 0x4E, 0x53),
    Rgb::new(0xB9, 0xCA, 0x4A),
    Rgb::new(0xE7, 0xC5, 0x47),
    Rgb::new(0x7A, 0xA6, 0xDA),
    Rgb::new(0xC3, 0x97, 0xD8),
    Rgb::new(0x70, 0xC0, 0xB1),
    Rgb::new(0xEA, 0xEA, 0xEA),
];

impl Name {
    /// The name of palette index `v` (`@enumFromInt`).
    pub(crate) fn from_int(v: u8) -> Self {
        NAMES.get(usize::from(v)).copied().unwrap_or(Self::Other(v))
    }

    /// The palette index of this name (`@intFromEnum`).
    pub(crate) fn int(self) -> u8 {
        match self {
            Self::Other(v) => v,
            named => NAMES
                .iter()
                .zip(0_u8..)
                .find_map(|(name, i)| (*name == named).then_some(i))
                .unwrap_or_default(),
        }
    }

    /// Default colors for tagged values.
    pub(crate) fn default(self) -> Result<Rgb, NoDefaultValue> {
        match self {
            Self::Other(_) => Err(NoDefaultValue),
            named => Ok(NAMED_DEFAULTS[usize::from(named.int())]),
        }
    }
}

/// The error of [`Name::default`] for a value without a default (`error.NoDefaultValue`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NoDefaultValue;

/// The "special colors" as denoted by xterm. These can be set via OSC 5 or via OSC 4 by adding the palette length to
/// it.
///
/// <https://invisible-island.net/xterm/ctlseqs/ctlseqs.html>
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Special {
    Bold = 0,
    Underline = 1,
    Blink = 2,
    Reverse = 3,
    Italic = 4,
}

impl Special {
    /// The special color's number in OSC 5 (`@intFromEnum`).
    pub(crate) const fn int(self) -> u16 {
        self as u16
    }

    /// The special color's number in OSC 4.
    pub(crate) const fn osc4(self) -> u16 {
        // "The special colors can also be set by adding the maximum number of colors (e.g., 88 or 256) to these codes
        // in an OSC 4  control" - xterm ctlseqs
        self.int() + 256
    }

    /// The special color of OSC 5 number `v`, if there is one.
    pub(crate) const fn from_int(v: u16) -> Option<Self> {
        Some(match v {
            0 => Self::Bold,
            1 => Self::Underline,
            2 => Self::Blink,
            3 => Self::Reverse,
            4 => Self::Italic,
            _ => return None,
        })
    }
}

/// The "dynamic colors" as denoted by xterm. These can be set via OSC 10 through 19.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dynamic {
    Foreground = 10,
    Background = 11,
    Cursor = 12,
    PointerForeground = 13,
    PointerBackground = 14,
    TektronixForeground = 15,
    TektronixBackground = 16,
    HighlightBackground = 17,
    TektronixCursor = 18,
    HighlightForeground = 19,
}

impl Dynamic {
    /// The dynamic color's OSC number (`@intFromEnum`).
    pub(crate) const fn int(self) -> u8 {
        self as u8
    }

    /// The dynamic color of OSC number `v` (`std.enums.fromInt`).
    pub(crate) const fn from_int(v: u8) -> Option<Self> {
        Some(match v {
            10 => Self::Foreground,
            11 => Self::Background,
            12 => Self::Cursor,
            13 => Self::PointerForeground,
            14 => Self::PointerBackground,
            15 => Self::TektronixForeground,
            16 => Self::TektronixBackground,
            17 => Self::HighlightBackground,
            18 => Self::TektronixCursor,
            19 => Self::HighlightForeground,
            _ => return None,
        })
    }

    /// The field name (`@tagName`), which logs print.
    pub(crate) const fn tag_name(self) -> &'static str {
        match self {
            Self::Foreground => "foreground",
            Self::Background => "background",
            Self::Cursor => "cursor",
            Self::PointerForeground => "pointer_foreground",
            Self::PointerBackground => "pointer_background",
            Self::TektronixForeground => "tektronix_foreground",
            Self::TektronixBackground => "tektronix_background",
            Self::HighlightBackground => "highlight_background",
            Self::TektronixCursor => "tektronix_cursor",
            Self::HighlightForeground => "highlight_foreground",
        }
    }

    /// The next dynamic color sequentially. This is required because specifying colors sequentially without their
    /// index will automatically use the next dynamic color.
    ///
    /// "Each successive parameter changes the next color in the list.  The value of Ps tells the starting point in the
    /// list."
    pub(crate) const fn next(self) -> Option<Self> {
        Self::from_int(self.int() + 1)
    }
}

/// RGB
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct Rgb {
    pub(crate) r: u8,
    pub(crate) g: u8,
    pub(crate) b: u8,
}

impl Rgb {
    /// The color `(r, g, b)`.
    pub(crate) const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Encode as the 8-bit-per-channel `rgb:xx/xx/xx` xterm wire form.
    pub(crate) fn encode_rgb8<W: std::io::Write + ?Sized>(self, out: &mut W) -> std::io::Result<()> {
        write!(out, "rgb:{:02x}/{:02x}/{:02x}", self.r, self.g, self.b)
    }

    /// Encode as the 16-bit-per-channel `rgb:xxxx/xxxx/xxxx` xterm wire form (each 8-bit value times 257).
    pub(crate) fn encode_rgb16<W: std::io::Write + ?Sized>(self, out: &mut W) -> std::io::Result<()> {
        write!(
            out,
            "rgb:{:04x}/{:04x}/{:04x}",
            u16::from(self.r) * 257,
            u16::from(self.g) * 257,
            u16::from(self.b) * 257,
        )
    }

    /// Parse a color from a floating point intensity value.
    ///
    /// The value should be between 0.0 and 1.0, inclusive.
    fn from_intensity(value: &[u8]) -> Result<u8, InvalidFormat> {
        let i = crate::terminal::fraction::parse(value).ok_or(InvalidFormat)?;
        // `fraction::parse` returns only values in [0, 1], so the product is in [0, 255] and the cast truncates as
        // Zig's `@intFromFloat` does.
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "i * 255 is in [0, 255]: fraction::parse refuses anything outside [0, 1]"
        )]
        let component = (i * f64::from(u8::MAX)) as u8;
        Ok(component)
    }

    /// Parse a color from a string of hexadecimal digits
    ///
    /// The string can contain 1, 2, 3, or 4 characters and represents the color value scaled in 4, 8, 12, or 16 bits,
    /// respectively.
    fn from_hex(value: &[u8]) -> Result<u8, InvalidFormat> {
        let divisor: u32 = match value.len() {
            1 => 0xF,
            2 => 0xFF,
            3 => 0xFFF,
            4 => 0xFFFF,
            _ => return Err(InvalidFormat),
        };
        let color = crate::parse_int::parse_int::<u16, 16>(value).map_err(|_| InvalidFormat)?;
        u8::try_from(u32::from(color) * u32::from(u8::MAX) / divisor).map_err(|_| InvalidFormat)
    }

    /// The three components of `#rgb`-style hex digits: `digits` split into three runs of `width` digits.
    fn from_hex_triple(digits: &[u8], width: usize) -> Result<Self, InvalidFormat> {
        Ok(Self {
            r: Self::from_hex(&digits[..width])?,
            g: Self::from_hex(&digits[width..2 * width])?,
            b: Self::from_hex(&digits[2 * width..3 * width])?,
        })
    }

    /// Parse a color specification.
    ///
    /// Leading and trailing spaces and tabs are ignored.
    ///
    /// Any of the following forms are accepted:
    ///
    /// 1. `rgb:<red>/<green>/<blue>`
    ///
    ///    `<red>, <green>, <blue> := h | hh | hhh | hhhh`
    ///
    ///    where `h` is a single hexadecimal digit.
    ///
    /// 2. `rgbi:<red>/<green>/<blue>`
    ///
    ///    where `<red>`, `<green>`, and `<blue>` are floating point values between 0.0 and 1.0 (inclusive).
    ///
    /// 3. `#rgb`, `#rrggbb`, `rgb`, `rrggbb`, `#rrrgggbbb`, `#rrrrggggbbbb`
    ///
    ///    where `r`, `g`, and `b` are hexadecimal digits. The forms with a leading # specify a color with 4, 8, 12,
    ///    and 16 bits of precision per color channel. The forms without a leading # are accepted for compatibility
    ///    with Ghostty config/theme color values.
    ///
    /// 4. X11 color names
    pub(crate) fn parse(value: &[u8]) -> Result<Self, InvalidFormat> {
        let input = trim_space_tab(value);
        if input.is_empty() {
            return Err(InvalidFormat);
        }

        if let Some(digits) = input.strip_prefix(b"#") {
            return match digits.len() {
                3 => Self::from_hex_triple(digits, 1),
                6 => Self::from_hex_triple(digits, 2),
                9 => Self::from_hex_triple(digits, 3),
                12 => Self::from_hex_triple(digits, 4),
                _ => Err(InvalidFormat),
            };
        }

        // Check for X11 named colors. We allow whitespace around the edges.
        if let Some(rgb) = x11_color::get(input) {
            return Ok(rgb);
        }

        match input.len() {
            3 => return Self::from_hex_triple(input, 1),
            6 => return Self::from_hex_triple(input, 2),
            _ => {}
        }

        Self::parse_rgb_spec(input)
    }

    /// The `rgb:` and `rgbi:` forms of [`RGB::parse`].
    fn parse_rgb_spec(input: &[u8]) -> Result<Self, InvalidFormat> {
        if input.len() < "rgb:a/a/a".len() || &input[0..3] != b"rgb" {
            return Err(InvalidFormat);
        }
        let (use_intensity, rest) = match &input[3..] {
            [b'i', b':', rest @ ..] => (true, rest),
            [b':', rest @ ..] => (false, rest),
            _ => return Err(InvalidFormat),
        };
        let component = |slice: &[u8]| {
            if use_intensity {
                Self::from_intensity(slice)
            } else {
                Self::from_hex(slice)
            }
        };
        let mut parts = rest.splitn(3, |&b| b == b'/');
        let (Some(r), Some(g), Some(b)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(InvalidFormat);
        };
        Ok(Self {
            r: component(r)?,
            g: component(g)?,
            b: component(b)?,
        })
    }
}

/// `std.mem.trim(u8, value, " \t")`: the slice without leading and trailing spaces and tabs.
fn trim_space_tab(value: &[u8]) -> &[u8] {
    let is_space = |b: &u8| *b == b' ' || *b == b'\t';
    let start = value.iter().position(|b| !is_space(b)).unwrap_or(value.len());
    let end = value.iter().rposition(|b| !is_space(b)).map_or(start, |i| i + 1);
    &value[start..end]
}

/// The error of the color parsers (`error.InvalidFormat`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InvalidFormat;

#[cfg(test)]
mod tests;
