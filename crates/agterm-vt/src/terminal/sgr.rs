//! SGR (Select Graphic Rendition) attribute parsing and types.
//!
//! Ghostty: `src/terminal/sgr.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/sgr.rs` at `RUSTTY_REV` (MIT).
//! Changes: `Parser::next` is split into the plain attributes ([`plain`]), the underline and the extended colors;
//! Zig's `@truncate` of a color parameter to its low byte is [`low_byte`]. The `C` forms are not here.

use crate::terminal::color;

pub(crate) use crate::terminal::parser::SepList;

/// The underline styles (`SGR 4:n`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Underline {
    None = 0,
    Single = 1,
    Double = 2,
    Curly = 3,
    Dotted = 4,
    Dashed = 5,
}

/// The parameters of an attribute the parser does not know.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Unknown<'a> {
    /// Full is the full SGR input.
    pub(crate) full: &'a [u16],

    /// Partial is the remaining, where we got hung up.
    pub(crate) partial: &'a [u16],
}

/// Attribute type for SGR
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Attribute<'a> {
    /// Unset all attributes
    Unset,

    /// Unknown attribute, the raw CSI command parameters are here.
    Unknown(Unknown<'a>),

    /// Bold the text.
    Bold,
    ResetBold,

    /// Italic text.
    Italic,
    ResetItalic,

    /// Faint/dim text. Note: reset faint is the same SGR code as reset bold
    Faint,

    /// Underline the text
    Underline(Underline),
    UnderlineColor(color::Rgb),
    /// `256_underline_color`
    Color256Underline(u8),
    ResetUnderlineColor,

    /// Overline the text
    Overline,
    ResetOverline,

    /// Blink the text
    Blink,
    ResetBlink,

    /// Invert fg/bg colors.
    Inverse,
    ResetInverse,

    /// Invisible
    Invisible,
    ResetInvisible,

    /// Strikethrough the text.
    Strikethrough,
    ResetStrikethrough,

    /// Set foreground color as RGB values.
    DirectColorFg(color::Rgb),

    /// Set background color as RGB values.
    DirectColorBg(color::Rgb),

    /// Set the background as a named color attribute (`8_bg`).
    Color8Bg(color::Name),
    /// Set the foreground as a named color attribute (`8_fg`).
    Color8Fg(color::Name),

    /// Reset the fg/bg to their default values.
    ResetFg,
    ResetBg,

    /// Set the background as a named bright color attribute (`8_bright_bg`).
    Color8BrightBg(color::Name),
    /// Set the foreground as a named bright color attribute (`8_bright_fg`).
    Color8BrightFg(color::Name),

    /// Set background color as 256-color palette (`256_bg`).
    Color256Bg(u8),

    /// Set foreground color as 256-color palette (`256_fg`).
    Color256Fg(u8),
}

/// What an extended color (38, 48 or 58) sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ColorTarget {
    Fg,
    Bg,
    Underline,
}

impl ColorTarget {
    /// The direct-color attribute of this target.
    const fn direct(self, rgb: color::Rgb) -> Attribute<'static> {
        match self {
            Self::Fg => Attribute::DirectColorFg(rgb),
            Self::Bg => Attribute::DirectColorBg(rgb),
            Self::Underline => Attribute::UnderlineColor(rgb),
        }
    }

    /// The 256-color attribute of this target.
    const fn indexed(self, idx: u8) -> Attribute<'static> {
        match self {
            Self::Fg => Attribute::Color256Fg(idx),
            Self::Bg => Attribute::Color256Bg(idx),
            Self::Underline => Attribute::Color256Underline(idx),
        }
    }
}

/// Zig's `@truncate` of a parameter to a color byte: the value should be 0 to 255, and if it isn't, the behavior is
/// undefined so we just... truncate it.
const fn low_byte(v: u16) -> u8 {
    v.to_le_bytes()[0]
}

/// The named color `n` (a parameter minus its range's base, so below 16).
fn name(n: u16) -> color::Name {
    color::Name::from_int(low_byte(n))
}

/// The attributes a single parameter sets, with no sub-parameters: everything but the underline and the extended
/// colors, which [`Parser::next`] handles.
fn plain(code: u16) -> Option<Attribute<'static>> {
    Some(match code {
        0 => Attribute::Unset,
        1 => Attribute::Bold,
        2 => Attribute::Faint,
        3 => Attribute::Italic,
        5 | 6 => Attribute::Blink,
        7 => Attribute::Inverse,
        8 => Attribute::Invisible,
        9 => Attribute::Strikethrough,
        21 => Attribute::Underline(Underline::Double),
        22 => Attribute::ResetBold,
        23 => Attribute::ResetItalic,
        24 => Attribute::Underline(Underline::None),
        25 => Attribute::ResetBlink,
        27 => Attribute::ResetInverse,
        28 => Attribute::ResetInvisible,
        29 => Attribute::ResetStrikethrough,
        30..=37 => Attribute::Color8Fg(name(code - 30)),
        39 => Attribute::ResetFg,
        40..=47 => Attribute::Color8Bg(name(code - 40)),
        49 => Attribute::ResetBg,
        53 => Attribute::Overline,
        55 => Attribute::ResetOverline,
        59 => Attribute::ResetUnderlineColor,
        // 82 instead of 90 to offset to "bright" colors
        90..=97 => Attribute::Color8BrightFg(name(code - 82)),
        100..=107 => Attribute::Color8BrightBg(name(code - 92)),
        _ => return None,
    })
}

/// Parser parses the attributes from a list of SGR parameters.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Parser<'a> {
    pub(crate) params: &'a [u16],
    pub(crate) params_sep: SepList,
    pub(crate) idx: usize,
}

impl<'a> Parser<'a> {
    /// Next returns the next attribute or null if there are no more attributes.
    pub(crate) fn next(&mut self) -> Option<Attribute<'a>> {
        if self.idx >= self.params.len() {
            // If we're at index zero it means we must have an empty list and an empty list implicitly means unset,
            // otherwise we're done and return null. Add one to ensure we don't loop on unset.
            let at_zero = self.idx == 0;
            self.idx += 1;
            return at_zero.then_some(Attribute::Unset);
        }

        let slice = &self.params[self.idx..];
        let colon = self.params_sep.is_set(self.idx);
        self.idx += 1;

        // If we have a colon separator then we need to ensure we're parsing a value that allows it.
        if colon && !matches!(slice[0], 4 | 38 | 48 | 58) {
            // In real world use it's very rare that we receive an invalid sequence. Consume all the colon separated
            // values and return them as unknown.
            let start = self.idx;
            while self.params_sep.is_set(self.idx) {
                self.idx += 1;
            }
            self.idx += 1;
            return Some(Attribute::Unknown(Unknown {
                full: self.params,
                partial: &slice[..(self.idx - start + 1).min(slice.len())],
            }));
        }

        let attr = match slice[0] {
            4 => self.underline(slice, colon),
            38 => self.extended_color(ColorTarget::Fg, slice, colon),
            48 => self.extended_color(ColorTarget::Bg, slice, colon),
            58 => self.extended_color(ColorTarget::Underline, slice, colon),
            code => plain(code),
        };
        Some(attr.unwrap_or(Attribute::Unknown(Unknown {
            full: self.params,
            partial: slice,
        })))
    }

    /// SGR 4, and 4:n for a style. `None` (an unknown attribute) for a colon with nothing after it, or a style that has
    /// more colon sub-parameters.
    fn underline(&mut self, slice: &[u16], colon: bool) -> Option<Attribute<'a>> {
        if !colon {
            return Some(Attribute::Underline(Underline::Single));
        }

        // A trailing colon with no following sub-param (e.g. "ESC[58:4:m") leaves the colon separator bit set on the
        // last param without adding another entry, so we can see param 4 with a colon but nothing after it.
        let &style = slice.get(1)?;

        if self.is_colon() {
            // Invalid/unknown SGRs are just not very likely.
            self.consume_unknown_colon();
            return None;
        }

        self.idx += 1;
        Some(Attribute::Underline(match style {
            0 => Underline::None,
            2 => Underline::Double,
            3 => Underline::Curly,
            4 => Underline::Dotted,
            5 => Underline::Dashed,
            // For unknown underline styles, just render a single underline.
            _ => Underline::Single,
        }))
    }

    /// SGR 38, 48 and 58: `2` for a direct color (r, g, b), `5` for an indexed one.
    fn extended_color(&mut self, target: ColorTarget, slice: &[u16], colon: bool) -> Option<Attribute<'a>> {
        match slice.get(1)? {
            // We need at least 3 more params for this to make sense.
            2 => self.parse_direct_color(target, slice, colon),
            5 => {
                let &idx = slice.get(2)?;
                self.idx += 2;
                Some(target.indexed(low_byte(idx)))
            }
            _ => None,
        }
    }

    /// A direct color (`slice[1] == 2`): `;r;g;b`, or with colons `:r:g:b` or `:colorspace:r:g:b`.
    fn parse_direct_color(&mut self, target: ColorTarget, slice: &[u16], colon: bool) -> Option<Attribute<'a>> {
        // Any direct color style must have at least 5 values.
        if slice.len() < 5 {
            return None;
        }

        // Only used for direct color sets (38, 48, 58) and subparam 2.
        debug_assert_eq!(slice[1], 2);

        let rgb = |at: usize| color::Rgb {
            r: low_byte(slice[at]),
            g: low_byte(slice[at + 1]),
            b: low_byte(slice[at + 2]),
        };

        // If we don't have a colon, then we expect exactly 3 semicolon separated values.
        if !colon {
            self.idx += 4;
            return Some(target.direct(rgb(2)));
        }

        // We have a colon, we might have either 5 or 6 values depending on if the colorspace is present.
        match self.count_colon() {
            // This is the much more common case in the wild.
            3 => {
                self.idx += 4;
                Some(target.direct(rgb(2)))
            }
            4 => {
                self.idx += 5;
                Some(target.direct(rgb(3)))
            }
            _ => {
                // Invalid/unknown SGRs just don't happen very often at all.
                self.consume_unknown_colon();
                None
            }
        }
    }

    /// Returns true if the present position has a colon separator. This always returns false for the last value since
    /// it has no separator.
    #[inline]
    fn is_colon(&self) -> bool {
        self.params_sep.is_set(self.idx)
    }

    /// The number of colon separators from the present position on.
    fn count_colon(&self) -> usize {
        let mut count: usize = 0;
        let mut idx = self.idx;
        while idx + 1 < self.params.len() && self.params_sep.is_set(idx) {
            count += 1;
            idx += 1;
        }
        count
    }

    /// Consumes all the remaining parameters separated by a colon (the caller returns an unknown attribute).
    fn consume_unknown_colon(&mut self) {
        let count = self.count_colon();
        self.idx += count + 1;
    }
}

#[cfg(test)]
mod tests;
