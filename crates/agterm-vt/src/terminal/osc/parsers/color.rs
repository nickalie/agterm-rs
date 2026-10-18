//! OSC color operations: set, query and reset the palette (OSC 4, 104), the special colors (OSC 5, 105) and the
//! dynamic colors (OSC 10-19, 110-119).
//!
//! Ghostty: `src/terminal/osc/parsers/color.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/osc/parsers/color.rs` at `RUSTTY_REV` (MIT).
//! Changes: the request list is a `Vec`, not Zig's `SegmentedList(Request, 2)` (datastruct `segmented_list` is not
//! ported), grown fallibly; it is the parser's (`Parser::color_requests`), which the command borrows until the next
//! reset, so no element needs a stable address while the list grows. Zig keeps the first two requests inline: here
//! even a one-request OSC allocates, so under an allocation failure it is dropped where Zig's would still parse.
//! Zig's `std.mem.tokenizeScalar` is [`tokenize`].

use crate::fmt::IntType;
use crate::mem::alloc::{OutOfMemory, TryVecExt as _};
use crate::terminal::color::{Dynamic as DynamicColor, Rgb, Special as SpecialColor};
use crate::terminal::osc::{Command, Parser, State, Terminator};

/// The log target of this parser (Ghostty's `.osc_color` scope).
const LOG_SCOPE: &str = "osc_color";

/// The possible operations we support for colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Operation {
    Osc4,
    Osc5,
    Osc10,
    Osc11,
    Osc12,
    Osc13,
    Osc14,
    Osc15,
    Osc16,
    Osc17,
    Osc18,
    Osc19,
    Osc104,
    Osc105,
    Osc110,
    Osc111,
    Osc112,
    Osc113,
    Osc114,
    Osc115,
    Osc116,
    Osc117,
    Osc118,
    Osc119,
}

impl Operation {
    /// The operation of the OSC number the parser read.
    fn of_state(state: State) -> Option<Self> {
        Some(match state {
            State::S4 => Self::Osc4,
            State::S5 => Self::Osc5,
            State::S10 => Self::Osc10,
            State::S11 => Self::Osc11,
            State::S12 => Self::Osc12,
            State::S13 => Self::Osc13,
            State::S14 => Self::Osc14,
            State::S15 => Self::Osc15,
            State::S16 => Self::Osc16,
            State::S17 => Self::Osc17,
            State::S18 => Self::Osc18,
            State::S19 => Self::Osc19,
            State::S104 => Self::Osc104,
            State::S105 => Self::Osc105,
            State::S110 => Self::Osc110,
            State::S111 => Self::Osc111,
            State::S112 => Self::Osc112,
            State::S113 => Self::Osc113,
            State::S114 => Self::Osc114,
            State::S115 => Self::Osc115,
            State::S116 => Self::Osc116,
            State::S117 => Self::Osc117,
            State::S118 => Self::Osc118,
            State::S119 => Self::Osc119,
            _ => return None,
        })
    }

    /// Whether this is a reset of dynamic colors (OSC 110-119).
    fn is_reset(self) -> bool {
        matches!(
            self,
            Self::Osc110
                | Self::Osc111
                | Self::Osc112
                | Self::Osc113
                | Self::Osc114
                | Self::Osc115
                | Self::Osc116
                | Self::Osc117
                | Self::Osc118
                | Self::Osc119
        )
    }

    /// The dynamic color of OSC 10-19 or 110-119.
    fn dynamic(self) -> Option<DynamicColor> {
        Some(match self {
            Self::Osc10 | Self::Osc110 => DynamicColor::Foreground,
            Self::Osc11 | Self::Osc111 => DynamicColor::Background,
            Self::Osc12 | Self::Osc112 => DynamicColor::Cursor,
            Self::Osc13 | Self::Osc113 => DynamicColor::PointerForeground,
            Self::Osc14 | Self::Osc114 => DynamicColor::PointerBackground,
            Self::Osc15 | Self::Osc115 => DynamicColor::TektronixForeground,
            Self::Osc16 | Self::Osc116 => DynamicColor::TektronixBackground,
            Self::Osc17 | Self::Osc117 => DynamicColor::HighlightBackground,
            Self::Osc18 | Self::Osc118 => DynamicColor::TektronixCursor,
            Self::Osc19 | Self::Osc119 => DynamicColor::HighlightForeground,
            Self::Osc4 | Self::Osc5 | Self::Osc104 | Self::Osc105 => return None,
        })
    }
}

/// Parse OSCs 4, 5, 10-19, 104, 105, 110-119
pub(crate) fn parse(parser: &mut Parser, terminator_ch: Option<u8>) -> Option<Command<'_>> {
    if !parser.alloc {
        parser.state = State::Invalid;
        return None;
    }
    // If we've collected any extra data parse that, otherwise use an empty string.
    let data: &[u8] = parser.capture.as_ref().map_or(b"", |c| c.trailing(&parser.buffer));
    // Check and make sure that we're parsing the correct OSCs
    let Some(op) = Operation::of_state(parser.state) else {
        parser.state = State::Invalid;
        return None;
    };
    let requests = parse_color(op, data).unwrap_or_else(|_| {
        tracing::info!(
            target: LOG_SCOPE,
            "failed to parse OSC {} color request err=error.OutOfMemory data={}",
            parser.state.tag_name(),
            String::from_utf8_lossy(data)
        );
        List::new()
    });
    let requests = parser.color_requests.insert(requests);
    Some(Command::ColorOperation {
        op,
        requests,
        terminator: Terminator::init(terminator_ch),
    })
}

/// Parse any color operation string. This should NOT include the operation itself, but only the body of the
/// operation. e.g. for "4;a;b;c" the body should be "a;b;c" and the operation should be set accordingly.
///
/// Color parsing is fairly complicated so we pull this out to a specialized function rather than go through our OSC
/// parsing state machine. This is much slower and requires more memory (since we need to buffer the full request) but
/// grants us an easier to understand and testable implementation.
///
/// If color changing ends up being a bottleneck we can optimize this later.
pub(crate) fn parse_color(op: Operation, buf: &[u8]) -> Result<List, OutOfMemory> {
    let mut it = tokenize(buf);
    match (op, op.dynamic()) {
        (Operation::Osc4 | Operation::Osc5, _) => parse_get_set_ansi_color(op, &mut it),
        (Operation::Osc104 | Operation::Osc105, _) => parse_reset_ansi_color(op, &mut it),
        (_, Some(color)) if op.is_reset() => parse_reset_dynamic_color(color, &mut it),
        (_, Some(color)) => parse_get_set_dynamic_color(color, &mut it),
        (_, None) => Ok(List::new()),
    }
}

/// The color number of a request: a palette index, or a special color (OSC 5's number, or OSC 4's offset by the
/// palette size). `None` for a number that names neither.
fn target_of(op: Operation, color: u16) -> Option<Target> {
    let special = |v: u16| SpecialColor::from_int(v).map(Target::Special);
    match op {
        // OSC5 and OSC105 map directly to the Special enum.
        Operation::Osc5 | Operation::Osc105 => special(color),
        // OSC4 and OSC104 map 0-255 to palette, 256-259 to special offset by the palette count.
        _ => u8::try_from(color).map_or_else(|_| special(color - 256), |idx| Some(Target::Palette(idx))),
    }
}

/// OSC 4/5
fn parse_get_set_ansi_color<'a>(op: Operation, it: &mut impl Iterator<Item = &'a [u8]>) -> Result<List, OutOfMemory> {
    // Note: in ANY error scenario below we return the accumulated results. This matches the xterm behavior (see
    // misc.c ChangeAnsiColorRequest)
    let mut result = List::new();
    // We expect a `c; spec` pair. If either doesn't exist then we return the results up to this point.
    while let (Some(color_str), Some(spec_str)) = (it.next(), it.next()) {
        // Color must be numeric. u9 because that'll fit our palette + special
        let Some(target) = parse_int_u9(color_str).and_then(|color| target_of(op, color)) else {
            break;
        };

        // "?" always results in a query.
        if spec_str == b"?" {
            result.try_push(Request::Query(target))?;
            continue;
        }

        let Ok(color) = Rgb::parse(spec_str) else {
            break;
        };
        result.try_push(Request::Set(ColoredTarget { target, color }))?;
    }
    Ok(result)
}

/// OSC 104/105: Reset ANSI Colors
fn parse_reset_ansi_color<'a>(op: Operation, it: &mut impl Iterator<Item = &'a [u8]>) -> Result<List, OutOfMemory> {
    // Note: xterm stops parsing the reset list on any error, but we're more flexible and try the next value. This
    // matches the behavior of Kitty and I don't see a downside to being more flexible here. Hopefully no one depends
    // on the exact behavior of xterm.
    let mut result = List::new();
    for color_str in it {
        // Empty color strings are ignored, not treated as an error. Color must be numeric. u9 because that'll fit our
        // palette + special
        if let Some(target) = parse_int_u9(color_str).and_then(|color| target_of(op, color)) {
            result.try_push(Request::Reset(target))?;
        }
    }

    // If no parameters are given, we reset the full table.
    if result.is_empty() {
        let req = if op == Operation::Osc105 {
            Request::ResetSpecial
        } else {
            Request::ResetPalette
        };
        result.try_push(req)?;
    }
    Ok(result)
}

/// OSC 10-19: Get/Set Dynamic Colors
fn parse_get_set_dynamic_color<'a>(
    start: DynamicColor,
    it: &mut impl Iterator<Item = &'a [u8]>,
) -> Result<List, OutOfMemory> {
    // Note: in ANY error scenario below we return the accumulated results. This matches the xterm behavior (see
    // misc.c ChangeColorsRequest)
    let mut result = List::new();
    let mut color = Some(start);
    while let (Some(current), Some(spec_str)) = (color, it.next()) {
        let target = Target::Dynamic(current);
        if spec_str == b"?" {
            result.try_push(Request::Query(target))?;
        } else {
            let Ok(rgb) = Rgb::parse(spec_str) else {
                break;
            };
            result.try_push(Request::Set(ColoredTarget { target, color: rgb }))?;
        }

        // Each successive value uses the next color so long as it exists.
        color = current.next();
    }
    Ok(result)
}

/// OSC 110-119: Reset Dynamic Colors
fn parse_reset_dynamic_color<'a>(
    color: DynamicColor,
    it: &mut impl Iterator<Item = &'a [u8]>,
) -> Result<List, OutOfMemory> {
    let mut result = List::new();
    if it.next().is_none() {
        result.try_push(Request::Reset(Target::Dynamic(color)))?;
    }
    Ok(result)
}

/// The requests of one color operation.
pub(crate) type List = Vec<Request>;

/// A single operation related to the terminal color palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Request {
    Set(ColoredTarget),
    Query(Target),
    Reset(Target),
    ResetPalette,
    ResetSpecial,
}

/// The color a request names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    Palette(u8),
    Special(SpecialColor),
    Dynamic(DynamicColor),
}

/// A color set on a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ColoredTarget {
    pub(crate) target: Target,
    pub(crate) color: Rgb,
}

/// `std.mem.tokenizeScalar(u8, buf, ';')`: the non-empty runs between semicolons.
fn tokenize(buf: &[u8]) -> impl Iterator<Item = &[u8]> {
    buf.split(|&b| b == b';').filter(|token| !token.is_empty())
}

/// `lib.parseInt(u9, s, 10)`.
fn parse_int_u9(s: &[u8]) -> Option<u16> {
    crate::parse_int::parse_int_type::<10>(IntType::unsigned(9), s)
        .ok()
        .and_then(|v| u16::try_from(v).ok())
}

#[cfg(test)]
mod tests;
