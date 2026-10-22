//! Value checks by kind, ported from ghostty's `src/cli/args.zig` (`parse`, `parseIntoField`, `parseBool`,
//! `parsePackedStruct`) and each config type's `parseCLI` at `LIBGHOSTTY_VT_REV`, so a value counts as malformed
//! exactly when libghostty would count it: the same accepted spellings of booleans, Zig integers and floats, colors, durations,
//! paddings, palette entries and themes. Kinds whose `parseCLI` is not ported ([`Kind::Opaque`]) accept any value;
//! they are the residual differences listed in the module docs.

use std::path::PathBuf;

use super::keys::{COMPATIBILITY, KEYS, X11_COLORS};
use crate::chrome::Rgb;

/// How a key's values are checked: the Zig type of its `Config.zig` field, resolved by `gen-ghostty-config-keys.sh`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `bool`: `1 t T true` / `0 f F false`; a bare key means true.
    Bool,
    /// A Zig integer of `bits` width, `parseInt` base 0 (`0x`/`0o`/`0b` prefixes, `_` separators, a sign).
    Int { signed: bool, bits: u8 },
    /// `f32`/`f64` as Zig's `parseFloat` reads them (`_` separators, hex floats, `inf`, `nan`).
    Float,
    /// A string; anything but a bare key.
    String,
    /// A Zig enum: one of its tag names, exactly.
    Enum(&'static [&'static str]),
    /// A packed struct of bools: a boolean for all of them, or a comma list of names, each optionally `no-`.
    Flags(&'static [&'static str]),
    /// `Color`: `#rgb`, `#rrggbb` (12 and 16-bit too), bare hex, `rgb:`/`rgbi:`, an X11 name.
    Color,
    /// `TerminalColor`: a [`Kind::Color`] or `cell-foreground`/`cell-background`.
    TerminalColor,
    /// `BoldColor`: a [`Kind::Color`] or `bright`.
    BoldColor,
    /// `Duration`: numbers with units (`y w d h m s ms µs us ns`), or a bare `0`.
    Duration,
    /// `WindowPadding`: `N` or `N,M`, unsigned 32-bit decimals.
    WindowPadding,
    /// `WindowDecoration`: a boolean or a tag; a bare key means `auto`.
    WindowDecoration,
    /// `Theme`: one name, or `light:NAME,dark:NAME`.
    Theme,
    /// `Palette`: `N=COLOR` with `N` a `u8`.
    Palette,
    /// `Limit(usize, ...)`: `unlimited` or an integer.
    Limit,
    /// `Path`: optional `?` prefix, optional quotes.
    Path,
    /// `RepeatablePath`: as [`Kind::Path`], one more entry per line.
    RepeatablePath,
    /// `RepeatableString`: one more entry per line.
    RepeatableString,
    /// `Keybinds`: a `keybind` line ([`super::binds::check`]).
    Keybind,
    /// `SelectionWordChars`: a Zig string literal's characters ([`decode_codepoints`]).
    SelectionWordChars,
    /// `RepeatableStringMap`: `KEY=VALUE` (`env`); a value without `=` is `error.ValueRequired`.
    StringMap,
    /// `RepeatableReadableIO` (`input`): a Zig string literal's body ([`decode_zig_string`]), tagged `raw:`/`path:`
    /// or not.
    ReadableIo,
    /// A type with a `parseCLI` ngterm does not port: every value is accepted, a bare key too unless `needs_value`.
    Opaque { needs_value: bool },
    /// A type whose `parseCLI` refuses every value (`error.NotImplemented`).
    NotImplemented,
}

/// One config key ghostty knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeySpec {
    /// The key as a config file spells it.
    pub name: &'static str,
    /// The field's Zig type in `Config.zig`.
    pub zig: &'static str,
    /// How its values are checked.
    pub kind: Kind,
}

impl KeySpec {
    /// Whether the field is optional in Zig (`?T`), which keeps the valid values out of its error message.
    fn optional(&self) -> bool {
        self.zig.starts_with('?')
    }
}

/// A compatibility handler of `Config.zig`, consulted only when the key itself fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compat {
    /// The key was renamed: the value is parsed as the new key's.
    Renamed(&'static str),
    /// Old values of the key are still taken.
    Values(&'static [&'static str]),
    /// A removed boolean key is still taken when its value is a boolean.
    Bool,
}

/// Why a value was refused: the error `parseIntoField` returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueError {
    /// `error.ValueRequired`: a bare key whose type needs a value.
    Required,
    /// `error.InvalidValue`.
    Invalid,
    /// Another error, reported by its Zig name (`error.Overflow` from a palette index).
    Other(&'static str),
}

/// The key of `name`, when ghostty knows it.
#[must_use]
pub fn key_spec(name: &str) -> Option<&'static KeySpec> {
    KEYS.binary_search_by(|spec| spec.name.cmp(name))
        .ok()
        .and_then(|index| KEYS.get(index))
}

/// The compatibility handler of `name`, when it has one.
#[must_use]
pub fn compat(name: &str) -> Option<Compat> {
    COMPATIBILITY
        .binary_search_by(|(key, _)| (*key).cmp(name))
        .ok()
        .and_then(|index| COMPATIBILITY.get(index))
        .map(|(_, handler)| *handler)
}

/// The key a line sets once it is accepted: itself, or the key it was renamed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accepted {
    /// The key took the value.
    Field(&'static KeySpec),
    /// The key failed and its compatibility handler took the value.
    Compat(Compat),
}

/// Checks one `key = value` line as `cli.args.parse` does: the key's own parse first, then its compatibility handler
/// on any failure. `Err` carries the diagnostic message libghostty would record.
///
/// # Errors
///
/// The message of the diagnostic the line adds.
pub fn check(key: &str, value: Option<&str>) -> Result<Accepted, String> {
    let spec = key_spec(key);
    let error = match spec {
        Some(spec) => match check_kind(spec.kind, value) {
            Ok(()) => return Ok(Accepted::Field(spec)),
            Err(error) => error,
        },
        None => ValueError::Invalid,
    };
    if let Some(handler) = compat(key).filter(|handler| compat_takes(*handler, value)) {
        return Ok(Accepted::Compat(handler));
    }
    Err(match spec {
        None => "unknown field".to_owned(),
        Some(spec) => message(spec, value, error),
    })
}

fn compat_takes(handler: Compat, value: Option<&str>) -> bool {
    match handler {
        Compat::Renamed(to) => key_spec(to).is_some_and(|spec| check_kind(spec.kind, value).is_ok()),
        Compat::Values(taken) => taken.contains(&value.unwrap_or("")),
        Compat::Bool => parse_bool(value.unwrap_or("t")).is_ok(),
    }
}

/// The diagnostic message of a refused value (`formatValueRequired`, `formatInvalidValue`).
fn message(spec: &KeySpec, value: Option<&str>, error: ValueError) -> String {
    let valid = match spec.kind {
        Kind::Enum(names) if !spec.optional() => format!(", valid values are: {}", names.join(", ")),
        _ => String::new(),
    };
    match error {
        ValueError::Required => format!("value required{valid}"),
        ValueError::Invalid => format!("invalid value \"{}\"{valid}", value.unwrap_or("null")),
        ValueError::Other(name) => format!("unknown error {name}"),
    }
}

/// Checks `value` against `kind`. An empty value resets the key to its default and is always taken.
///
/// # Errors
///
/// The error `parseIntoField` returns for it.
pub fn check_kind(kind: Kind, value: Option<&str>) -> Result<(), ValueError> {
    if value == Some("") {
        return Ok(());
    }
    match (kind, value) {
        (Kind::Bool, value) => parse_bool(value.unwrap_or("t")).map(drop),
        (Kind::NotImplemented, _) => Err(ValueError::Other("error.NotImplemented")),
        (Kind::WindowDecoration | Kind::Opaque { needs_value: false }, None) => Ok(()),
        (_, None) => Err(ValueError::Required),
        (kind, Some(value)) => check_value(kind, value),
    }
}

fn check_value(kind: Kind, value: &str) -> Result<(), ValueError> {
    match kind {
        Kind::Int { signed, bits } => parse_int(value, 0, signed, bits)
            .map(drop)
            .map_err(|_| ValueError::Invalid),
        Kind::Float => parse_float(value).map(drop),
        Kind::Enum(names) => names.contains(&value).then_some(()).ok_or(ValueError::Invalid),
        Kind::Flags(names) => parse_flags(value, names).map(drop),
        Kind::Color => parse_color(value).map(drop),
        Kind::TerminalColor => parse_terminal_color(value).map(drop),
        Kind::BoldColor => match value {
            "bright" => Ok(()),
            color => parse_color(color).map(drop),
        },
        Kind::Duration => parse_duration(value).map(drop),
        Kind::WindowPadding => parse_padding(value).map(drop),
        Kind::WindowDecoration => {
            let tag = ["auto", "client", "server", "none"].contains(&value);
            (tag || parse_bool(value).is_ok())
                .then_some(())
                .ok_or(ValueError::Invalid)
        }
        Kind::Theme => parse_theme(value).map(drop),
        Kind::Palette => parse_palette_entry(value).map(drop),
        Kind::Limit => parse_limit(value).map(drop),
        Kind::Keybind => super::binds::check(value).map_err(|error| ValueError::Other(error.zig_name())),
        Kind::SelectionWordChars => decode_codepoints(value).map(drop).ok_or(ValueError::Invalid),
        Kind::StringMap => value.contains('=').then_some(()).ok_or(ValueError::Required),
        Kind::ReadableIo => decode_zig_string(value).map(drop),
        Kind::Bool
        | Kind::String
        | Kind::Path
        | Kind::RepeatablePath
        | Kind::RepeatableString
        | Kind::Opaque { .. }
        | Kind::NotImplemented => Ok(()),
    }
}

/// `cli.args.parseBool`.
///
/// # Errors
///
/// [`ValueError::Invalid`] for anything else.
pub fn parse_bool(value: &str) -> Result<bool, ValueError> {
    match value {
        "1" | "t" | "T" | "true" => Ok(true),
        "0" | "f" | "F" | "false" => Ok(false),
        _ => Err(ValueError::Invalid),
    }
}

/// Why Zig's integer parse failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntError {
    /// `error.InvalidCharacter`.
    Invalid,
    /// `error.Overflow`.
    Overflow,
}

/// Zig's `std.fmt.parseInt` into an integer of `bits` width: an optional sign, then [`parse_digits`].
///
/// # Errors
///
/// [`IntError`] as Zig's.
pub fn parse_int(value: &str, base: u32, signed: bool, bits: u8) -> Result<i128, IntError> {
    let (negative, digits) = match value.as_bytes().first() {
        Some(b'+') => (false, &value[1..]),
        Some(b'-') => (true, &value[1..]),
        _ => (false, value),
    };
    let magnitude = i128::from(parse_digits(digits, base)?);
    let number = if negative { -magnitude } else { magnitude };
    let (min, max) = if signed {
        (-(1_i128 << (bits - 1)), (1_i128 << (bits - 1)) - 1)
    } else {
        (0, (1_i128 << bits) - 1)
    };
    if number < min || number > max {
        return Err(IntError::Overflow);
    }
    Ok(number)
}

/// Zig's `parseUnsigned` digits: base 0 detects a `0b`/`0o`/`0x` prefix (else decimal); `_` may separate digits but
/// not start or end them.
///
/// # Errors
///
/// [`IntError::Invalid`] for a stray character, [`IntError::Overflow`] past `u64`.
pub fn parse_digits(value: &str, base: u32) -> Result<u64, IntError> {
    if value.is_empty() {
        return Err(IntError::Invalid);
    }
    let (base, digits) = if base == 0 { detect_base(value) } else { (base, value) };
    if digits.starts_with('_') || digits.ends_with('_') {
        return Err(IntError::Invalid);
    }
    let mut number: u64 = 0;
    for digit in digits.chars().filter(|character| *character != '_') {
        let digit = digit.to_digit(base).ok_or(IntError::Invalid)?;
        number = number
            .checked_mul(u64::from(base))
            .and_then(|number| number.checked_add(u64::from(digit)))
            .ok_or(IntError::Overflow)?;
    }
    Ok(number)
}

fn detect_base(value: &str) -> (u32, &str) {
    let bytes = value.as_bytes();
    if bytes.len() > 2 && bytes[0] == b'0' {
        let base = match bytes[1].to_ascii_lowercase() {
            b'b' => Some(2),
            b'o' => Some(8),
            b'x' => Some(16),
            _ => None,
        };
        if let Some(base) = base {
            return (base, &value[2..]);
        }
    }
    (10, value)
}

/// Zig's `std.fmt.parseFloat`: an optional sign, then a decimal or `0x` hex number (`_` between digits, an `e`/`p`
/// exponent), or `inf`, `infinity`, `nan` in any case.
///
/// # Errors
///
/// [`ValueError::Invalid`] for anything else.
pub fn parse_float(value: &str) -> Result<f64, ValueError> {
    let (negative, body) = match value.as_bytes().first() {
        Some(b'-') => (true, &value[1..]),
        Some(b'+') => (false, &value[1..]),
        _ => (false, value),
    };
    if body.is_empty() {
        return Err(ValueError::Invalid);
    }
    let magnitude = match body.get(..2) {
        Some(prefix) if prefix.eq_ignore_ascii_case("0x") => float_number(&body[2..], 16),
        _ => float_number(body, 10),
    }
    .or_else(|| float_special(body))
    .ok_or(ValueError::Invalid)?;
    Ok(if negative { -magnitude } else { magnitude })
}

fn float_special(body: &str) -> Option<f64> {
    if body.eq_ignore_ascii_case("inf") || body.eq_ignore_ascii_case("infinity") {
        Some(f64::INFINITY)
    } else if body.eq_ignore_ascii_case("nan") {
        Some(f64::NAN)
    } else {
        None
    }
}

/// The number of a float body in `base` 10 or 16: digits, an optional `.` and digits, an optional exponent; at least
/// one digit, nothing left over, every `_` between two digits.
fn float_number(body: &str, base: u32) -> Option<f64> {
    let bytes = body.as_bytes();
    let digit = |byte: u8| char::from(byte).is_digit(base);
    let scan = |mut at: usize, digits: &mut Vec<u8>| {
        while at < bytes.len() && (digit(bytes[at]) || bytes[at] == b'_') {
            if bytes[at] != b'_' {
                digits.push(bytes[at]);
            }
            at += 1;
        }
        at
    };
    let (mut integer, mut fraction) = (Vec::new(), Vec::new());
    let mut at = scan(0, &mut integer);
    if bytes.get(at) == Some(&b'.') {
        at = scan(at + 1, &mut fraction);
    }
    if integer.is_empty() && fraction.is_empty() {
        return None;
    }
    let marker = if base == 16 { b'p' } else { b'e' };
    let mut exponent: i64 = 0;
    if bytes.get(at).map(u8::to_ascii_lowercase) == Some(marker) {
        let (value, end) = float_exponent(bytes, at + 1)?;
        exponent = value;
        at = end;
    }
    if at != bytes.len() || !valid_underscores(bytes, base) {
        return None;
    }
    Some(float_value(&integer, &fraction, exponent, base))
}

/// The exponent after `e`/`p` from `at`: an optional sign and at least one decimal digit (`_` between them).
fn float_exponent(bytes: &[u8], mut at: usize) -> Option<(i64, usize)> {
    let negative = bytes.get(at) == Some(&b'-');
    if matches!(bytes.get(at), Some(b'-' | b'+')) {
        at += 1;
    }
    if !bytes.get(at).is_some_and(u8::is_ascii_digit) {
        return None;
    }
    let mut exponent: i64 = 0;
    while at < bytes.len() && (bytes[at].is_ascii_digit() || bytes[at] == b'_') {
        if bytes[at] != b'_' && exponent < 0x1000_0000 {
            exponent = exponent * 10 + i64::from(bytes[at] - b'0');
        }
        at += 1;
    }
    Some((if negative { -exponent } else { exponent }, at))
}

/// `validUnderscores`: every `_` has a digit of `base` on both sides.
fn valid_underscores(bytes: &[u8], base: u32) -> bool {
    let digit = |at: usize| bytes.get(at).is_some_and(|byte| char::from(*byte).is_digit(base));
    bytes
        .iter()
        .enumerate()
        .filter(|(_, byte)| **byte == b'_')
        .all(|(at, _)| at > 0 && digit(at - 1) && digit(at + 1))
}

fn float_value(integer: &[u8], fraction: &[u8], exponent: i64, base: u32) -> f64 {
    if base == 10 {
        let text = format!(
            "{}.{}e{exponent}",
            String::from_utf8_lossy(integer),
            String::from_utf8_lossy(fraction)
        );
        let text = text.replace(".e", ".0e");
        let text = if text.starts_with('.') {
            format!("0{text}")
        } else {
            text
        };
        return text.parse().unwrap_or(f64::NAN);
    }
    hex_float_value(integer, fraction, exponent)
}

/// A hex float's value: at most 13 significant digits (52 bits, exact in an `f64`), the dropped ones folded into a
/// clamped binary exponent applied in steps whose powers of two stay finite and normal.
fn hex_float_value(integer: &[u8], fraction: &[u8], exponent: i64) -> f64 {
    const DIGITS: usize = 13;
    const STEP: i32 = 1000;
    let digits: Vec<u32> = integer
        .iter()
        .chain(fraction)
        .filter_map(|byte| char::from(*byte).to_digit(16))
        .skip_while(|digit| *digit == 0)
        .collect();
    if digits.is_empty() {
        return 0.0;
    }
    let kept = digits.len().min(DIGITS);
    let mantissa = digits[..kept]
        .iter()
        .fold(0.0, |value: f64, digit| value * 16.0 + f64::from(*digit));
    let dropped = i64::try_from(digits.len() - kept).unwrap_or(i64::MAX);
    let fraction = i64::try_from(fraction.len()).unwrap_or(i64::MAX);
    let shift = exponent
        .saturating_add(dropped.saturating_mul(4))
        .saturating_sub(fraction.saturating_mul(4));
    // past ±10 000 a 52-bit mantissa is infinite or zero anyway
    let mut shift = i32::try_from(shift.clamp(-10_000, 10_000)).unwrap_or(0);
    let mut value = mantissa;
    while shift != 0 {
        let step = shift.clamp(-STEP, STEP);
        value *= 2_f64.powi(step);
        shift -= step;
    }
    value
}

/// A packed struct of `names`: a boolean sets all of them, else a comma list of names (each trimmed, `no-` turning
/// it off) over every flag's default. Returns the flags the value names, `true` or `false`, in `names` order (`None`
/// for a flag left at its default), or all of them for a boolean.
///
/// # Errors
///
/// [`ValueError::Invalid`] for a name not in `names`.
pub fn parse_flags(value: &str, names: &[&str]) -> Result<Vec<Option<bool>>, ValueError> {
    if let Ok(all) = parse_bool(value) {
        return Ok(vec![Some(all); names.len()]);
    }
    let mut flags = vec![None; names.len()];
    for part in value.split(',') {
        let part = part.trim_matches([' ', '\t']);
        let (name, on) = part.strip_prefix("no-").map_or((part, true), |name| (name, false));
        let index = names
            .iter()
            .position(|candidate| *candidate == name)
            .ok_or(ValueError::Invalid)?;
        flags[index] = Some(on);
    }
    Ok(flags)
}

/// `terminal.color.RGB.parse`: trimmed of spaces and tabs, then `#` hex of 4, 8, 12 or 16 bits a channel, an X11
/// color name (any case), 3 or 6 bare hex digits, `rgb:h/h/h` (1–4 hex digits each) or `rgbi:f/f/f` (0–1).
///
/// # Errors
///
/// [`ValueError::Invalid`] for anything else.
pub fn parse_color(value: &str) -> Result<Rgb, ValueError> {
    let input = value.trim_matches([' ', '\t']);
    // every form, X11 names included, is ASCII; the widths below slice by bytes
    if input.is_empty() || !input.is_ascii() {
        return Err(ValueError::Invalid);
    }
    if let Some(hex) = input.strip_prefix('#') {
        let width = match hex.len() {
            3 => 1,
            6 => 2,
            9 => 3,
            12 => 4,
            _ => return Err(ValueError::Invalid),
        };
        return rgb_of([&hex[..width], &hex[width..2 * width], &hex[2 * width..]], channel_hex);
    }
    if let Some([r, g, b]) = x11_color(input) {
        return Ok(Rgb::new(r, g, b));
    }
    match input.len() {
        3 => return rgb_of([&input[..1], &input[1..2], &input[2..]], channel_hex),
        6 => return rgb_of([&input[..2], &input[2..4], &input[4..]], channel_hex),
        _ => {}
    }
    parse_rgb_spec(input)
}

/// Parses one channel of a color.
type Channel = fn(&str) -> Result<u8, ValueError>;

/// The `rgb:` and `rgbi:` forms.
fn parse_rgb_spec(input: &str) -> Result<Rgb, ValueError> {
    if input.len() < "rgb:a/a/a".len() || !input.starts_with("rgb") {
        return Err(ValueError::Invalid);
    }
    let (rest, channel): (&str, Channel) = match input.strip_prefix("rgbi") {
        Some(rest) => (rest, channel_intensity),
        None => (&input[3..], channel_hex),
    };
    let rest = rest.strip_prefix(':').ok_or(ValueError::Invalid)?;
    let (r, rest) = rest.split_once('/').ok_or(ValueError::Invalid)?;
    let (g, b) = rest.split_once('/').ok_or(ValueError::Invalid)?;
    rgb_of([r, g, b], channel)
}

fn rgb_of(parts: [&str; 3], channel: Channel) -> Result<Rgb, ValueError> {
    Ok(Rgb::new(channel(parts[0])?, channel(parts[1])?, channel(parts[2])?))
}

/// `RGB.fromHex`: 1–4 hex digits scaled to 8 bits.
fn channel_hex(digits: &str) -> Result<u8, ValueError> {
    if digits.is_empty() || digits.len() > 4 {
        return Err(ValueError::Invalid);
    }
    let value = parse_digits(digits, 16).map_err(|_| ValueError::Invalid)?;
    let value = u16::try_from(value).map_err(|_| ValueError::Invalid)?;
    let max: u32 = (1 << (4 * digits.len())) - 1;
    u8::try_from(u32::from(value) * 255 / max).map_err(|_| ValueError::Invalid)
}

/// `RGB.fromIntensity`: a decimal fraction in 0...1 (`terminal/fraction.zig`), scaled to 8 bits.
fn channel_intensity(value: &str) -> Result<u8, ValueError> {
    let (negative, body) = match value.as_bytes().first() {
        Some(b'-') => (true, &value[1..]),
        Some(b'+') => (false, &value[1..]),
        _ => (false, value),
    };
    let (integer, fraction) = body.split_once('.').unwrap_or((body, ""));
    let digits = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
    if integer.len() + fraction.len() == 0 || !digits(integer) || !digits(fraction) {
        return Err(ValueError::Invalid);
    }
    let magnitude: f64 = format!("0{integer}.{fraction}0")
        .parse()
        .map_err(|_| ValueError::Invalid)?;
    let fraction = if negative { -magnitude } else { magnitude };
    if !(0.0..=1.0).contains(&fraction) {
        return Err(ValueError::Invalid);
    }
    // truncated, as Zig's @intFromFloat
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "0...255 by the range check"
    )]
    let channel = (fraction * 255.0) as u8;
    Ok(channel)
}

/// An X11 color by name, ignoring ASCII case.
fn x11_color(name: &str) -> Option<[u8; 3]> {
    let lower = name.to_ascii_lowercase();
    X11_COLORS
        .binary_search_by(|(candidate, _)| (*candidate).cmp(lower.as_str()))
        .ok()
        .and_then(|index| X11_COLORS.get(index))
        .map(|(_, rgb)| *rgb)
}

/// A `TerminalColor`: a color, or the cell's own foreground or background.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalColor {
    /// A fixed color.
    Color(Rgb),
    /// The cell's foreground.
    CellForeground,
    /// The cell's background.
    CellBackground,
}

/// `TerminalColor.parseCLI`.
///
/// # Errors
///
/// As [`parse_color`].
pub fn parse_terminal_color(value: &str) -> Result<TerminalColor, ValueError> {
    match value {
        "cell-foreground" => Ok(TerminalColor::CellForeground),
        "cell-background" => Ok(TerminalColor::CellBackground),
        color => parse_color(color).map(TerminalColor::Color),
    }
}

/// `Duration.parseCLI`, in nanoseconds: whitespace-separated numbers each with a unit, saturating; a lone `0` needs
/// none.
///
/// # Errors
///
/// [`ValueError::Invalid`] for a number without a unit or an unknown unit, [`ValueError::Required`] for blanks only.
pub fn parse_duration(value: &str) -> Result<u64, ValueError> {
    const UNITS: [(&str, u64); 10] = [
        ("y", 365 * 86_400_000_000_000),
        ("w", 7 * 86_400_000_000_000),
        ("d", 86_400_000_000_000),
        ("h", 3_600_000_000_000),
        ("m", 60_000_000_000),
        ("s", 1_000_000_000),
        ("ms", 1_000_000),
        ("µs", 1_000),
        ("us", 1_000),
        ("ns", 1),
    ];
    let mut remaining = value;
    let mut total: Option<u64> = None;
    loop {
        remaining = remaining.trim_start_matches(|character: char| character.is_ascii_whitespace());
        if remaining.is_empty() {
            break;
        }
        // the longest number prefix, stopping at the first prefix that does not parse
        let mut number = None;
        let mut taken = 0;
        for end in 1..=remaining.len() {
            let Some(prefix) = remaining.get(..end) else { break };
            match parse_digits(prefix, 10) {
                Ok(parsed) => (number, taken) = (Some(parsed), end),
                Err(_) => break,
            }
        }
        let number = number.ok_or(ValueError::Invalid)?;
        remaining = &remaining[taken..];
        if remaining.is_empty() {
            if number == 0 {
                total = Some(0);
                break;
            }
            return Err(ValueError::Invalid);
        }
        let (factor, length) = UNITS
            .iter()
            .filter(|(name, _)| remaining.starts_with(name))
            .map(|(name, factor)| (*factor, name.len()))
            .max_by_key(|(_, length)| *length)
            .ok_or(ValueError::Invalid)?;
        remaining = &remaining[length..];
        total = Some(total.unwrap_or(0).saturating_add(number.saturating_mul(factor)));
    }
    total.ok_or(ValueError::Required)
}

/// One axis of `window-padding-*`: the start (top or left) and end (bottom or right) padding in points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Padding {
    pub start: u32,
    pub end: u32,
}

/// `WindowPadding.parseCLI`: `N` for both sides, `N,M` for each; decimal `u32`s.
///
/// # Errors
///
/// [`ValueError::Invalid`] when a side does not parse.
pub fn parse_padding(value: &str) -> Result<Padding, ValueError> {
    let side = |text: &str| {
        let number = parse_int(text.trim_matches([' ', '\t']), 10, false, 32).map_err(|_| ValueError::Invalid)?;
        u32::try_from(number).map_err(|_| ValueError::Invalid)
    };
    if let Some((start, end)) = value.split_once(',') {
        return Ok(Padding {
            start: side(start)?,
            end: side(end)?,
        });
    }
    let both = side(value)?;
    Ok(Padding { start: both, end: both })
}

/// The `theme` key: one theme for both color schemes, or a light and a dark one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeName {
    pub light: String,
    pub dark: String,
}

impl ThemeName {
    /// The side the color scheme picks.
    #[must_use]
    pub fn side(&self, dark: bool) -> &str {
        if dark { &self.dark } else { &self.light }
    }
}

/// `Theme.parseCLI`: with a `,`, `=` or `:` in it, `light:NAME,dark:NAME` (both required, either order, values may be
/// quoted); else one trimmed name.
///
/// # Errors
///
/// [`ValueError::Invalid`] for a malformed pair.
pub fn parse_theme(value: &str) -> Result<ThemeName, ValueError> {
    if !value.contains([',', '=', ':']) {
        let name = value.trim_matches([' ', '\t']).to_owned();
        return Ok(ThemeName {
            light: name.clone(),
            dark: name,
        });
    }
    let (mut light, mut dark) = (None, None);
    for entry in split_commas(value)? {
        let (key, side) = entry.split_once(':').ok_or(ValueError::Invalid)?;
        let side = unquote(side.trim_matches([' ', '\t']))?;
        match key.trim_matches([' ', '\t']) {
            "light" => light = Some(side),
            "dark" => dark = Some(side),
            _ => return Err(ValueError::Invalid),
        }
    }
    match (light, dark) {
        (Some(light), Some(dark)) => Ok(ThemeName { light, dark }),
        _ => Err(ValueError::Invalid),
    }
}

/// `CommaSplitter`: fields split at commas outside double quotes; a trailing comma ends the list. A backslash starts
/// an escape that must be well formed (`\n \r \t \\ \' \"`, `\x` and two hex digits, `\u{` hex digits `}`), inside
/// quotes or not.
fn split_commas(value: &str) -> Result<Vec<&str>, ValueError> {
    let bytes = value.as_bytes();
    let (mut fields, mut start, mut at, mut quoted) = (Vec::new(), 0, 0, false);
    while at < bytes.len() {
        match bytes[at] {
            b'\\' => {
                at = skip_escape(bytes, at + 1)?;
                continue;
            }
            b'"' => quoted = !quoted,
            b',' if !quoted => {
                fields.push(&value[start..at]);
                start = at + 1;
            }
            _ => {}
        }
        at += 1;
    }
    if quoted {
        return Err(ValueError::Other("error.UnclosedQuote"));
    }
    if start < bytes.len() {
        fields.push(&value[start..]);
    }
    Ok(fields)
}

/// The index past the escape whose character is at `at` (just after the backslash), as `CommaSplitter` checks it.
fn skip_escape(bytes: &[u8], at: usize) -> Result<usize, ValueError> {
    const UNFINISHED: ValueError = ValueError::Other("error.UnfinishedEscape");
    const ILLEGAL: ValueError = ValueError::Other("error.IllegalEscape");
    match bytes.get(at).ok_or(UNFINISHED)? {
        b'n' | b'r' | b't' | b'\\' | b'\'' | b'"' => Ok(at + 1),
        b'x' => {
            for offset in 1..=2 {
                if !bytes.get(at + offset).ok_or(UNFINISHED)?.is_ascii_hexdigit() {
                    return Err(ILLEGAL);
                }
            }
            Ok(at + 3)
        }
        b'u' => {
            if *bytes.get(at + 1).ok_or(UNFINISHED)? != b'{' {
                return Err(ILLEGAL);
            }
            skip_unicode_digits(bytes, at + 2)
        }
        _ => Err(ILLEGAL),
    }
}

/// `CommaSplitter`'s `\u{...}` digits from `at`: at least one, then `}`. Its running value is its own (a letter digit
/// adds its offset from `a`/`A`, not 10 more), checked against U+10FFFF after each digit.
fn skip_unicode_digits(bytes: &[u8], mut at: usize) -> Result<usize, ValueError> {
    const UNFINISHED: ValueError = ValueError::Other("error.UnfinishedEscape");
    const ILLEGAL: ValueError = ValueError::Other("error.IllegalEscape");
    let (mut digits, mut value) = (0, 0_u32);
    loop {
        let byte = *bytes.get(at).ok_or(UNFINISHED)?;
        at += 1;
        let digit = match byte {
            b'}' if digits == 0 => return Err(ILLEGAL),
            b'}' => return Ok(at),
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a',
            b'A'..=b'F' => byte - b'A',
            _ => return Err(ILLEGAL),
        };
        digits += 1;
        value = (value << 4) + u32::from(digit);
        if value > 0x10_ffff {
            return Err(ILLEGAL);
        }
    }
}

/// A field value of `parseAutoStruct`: a double-quoted one decoded as a Zig string literal
/// (`std.zig.string_literal.parseWrite`), up to its first unescaped quote. A `\x` byte that leaves invalid UTF-8 is
/// replaced (U+FFFD): libghostty looks such a theme up by its raw bytes, which a `String` cannot hold, and not finding
/// either name counts the same.
fn unquote(value: &str) -> Result<String, ValueError> {
    if value.len() < 2 || !value.starts_with('"') || !value.ends_with('"') {
        return Ok(value.to_owned());
    }
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut at = 1;
    loop {
        match bytes.get(at).ok_or(ValueError::Invalid)? {
            b'"' => break,
            b'\\' => at = decode_escape(bytes, at + 1, &mut decoded)?,
            byte => {
                decoded.push(*byte);
                at += 1;
            }
        }
    }
    Ok(String::from_utf8_lossy(&decoded).into_owned())
}

/// `config/string.zig`'s `parse`: a Zig string literal's body, its escapes decoded (`\xNN` one byte, any other
/// escape a character's UTF-8); `None` for a malformed escape.
#[must_use]
pub fn decode_string(text: &str) -> Option<Vec<u8>> {
    decode_zig_string(text).ok()
}

/// [`decode_string`] with the error `string.parse` returns.
///
/// # Errors
///
/// [`ValueError::Other`]: `error.InvalidString` for a malformed escape, `error.Utf8CannotEncodeSurrogateHalf` for a
/// `\u{...}` of a surrogate (Zig's escape parser takes it, the UTF-8 encoder refuses it).
pub fn decode_zig_string(text: &str) -> Result<Vec<u8>, ValueError> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while let Some(byte) = bytes.get(at) {
        if *byte == b'\\' {
            at = decode_escape(bytes, at + 1, &mut decoded).map_err(|error| match error {
                ValueError::Other(name) => ValueError::Other(name),
                ValueError::Required | ValueError::Invalid => ValueError::Other("error.InvalidString"),
            })?;
        } else {
            decoded.push(*byte);
            at += 1;
        }
    }
    Ok(decoded)
}

/// `config/string.zig`'s `codepointIterator`: a Zig string literal's characters, its escapes decoded (`\xNN` the
/// character U+00NN here, not a byte; any other escape its character); `None` for a malformed escape or invalid UTF-8
/// (`selection-word-chars`, `SelectionWordChars.parseCLI`).
#[must_use]
pub fn decode_codepoints(text: &str) -> Option<Vec<char>> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(text.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'\\' {
            if bytes.get(at + 1) == Some(&b'x') {
                let hex = |index: usize| bytes.get(index).and_then(|byte| char::from(*byte).to_digit(16));
                let value = hex(at + 2)? * 16 + hex(at + 3)?;
                decoded.push(char::from_u32(value)?);
                at += 4;
                continue;
            }
            let mut out = Vec::new();
            at = decode_escape(bytes, at + 1, &mut out).ok()?;
            decoded.extend(std::str::from_utf8(&out).ok()?.chars());
        } else {
            let character = text.get(at..)?.chars().next()?;
            decoded.push(character);
            at += character.len_utf8();
        }
    }
    Some(decoded)
}

/// `parseEscapeSequence` for the escape whose character is at `at`: its bytes appended to `out`, the index past it
/// returned. `\u{...}` takes any number of hex digits up to U+10FFFF and must name a scalar value.
fn decode_escape(bytes: &[u8], at: usize, out: &mut Vec<u8>) -> Result<usize, ValueError> {
    let hex = |index: usize| {
        bytes
            .get(index)
            .and_then(|byte| char::from(*byte).to_digit(16))
            .ok_or(ValueError::Invalid)
    };
    let simple = match bytes.get(at).ok_or(ValueError::Invalid)? {
        b'n' => b'\n',
        b'r' => b'\r',
        b't' => b'\t',
        byte @ (b'\\' | b'\'' | b'"') => *byte,
        b'x' => {
            let byte = hex(at + 1)? * 16 + hex(at + 2)?;
            out.push(u8::try_from(byte).map_err(|_| ValueError::Invalid)?);
            return Ok(at + 3);
        }
        b'u' => {
            if bytes.get(at + 1) != Some(&b'{') || bytes.get(at + 2) == Some(&b'}') {
                return Err(ValueError::Invalid);
            }
            let (mut index, mut value) = (at + 2, 0_u32);
            while bytes.get(index) != Some(&b'}') {
                value = value * 16 + hex(index)?;
                if value > 0x10_ffff {
                    return Err(ValueError::Invalid);
                }
                index += 1;
            }
            if (0xd800..=0xdfff).contains(&value) {
                return Err(ValueError::Other("error.Utf8CannotEncodeSurrogateHalf"));
            }
            let character = char::from_u32(value).ok_or(ValueError::Invalid)?;
            out.extend_from_slice(character.encode_utf8(&mut [0; 4]).as_bytes());
            return Ok(index + 1);
        }
        _ => return Err(ValueError::Invalid),
    };
    out.push(simple);
    Ok(at + 1)
}

/// `terminal.color.parsePaletteEntry`: `N=COLOR`, `N` a `u8` in Zig's base-0 syntax, spaces and tabs around both.
///
/// # Errors
///
/// [`ValueError::Other`] (`error.Overflow`) for an index past 255, else [`ValueError::Invalid`].
pub fn parse_palette_entry(value: &str) -> Result<(u8, Rgb), ValueError> {
    let (index, color) = value.split_once('=').ok_or(ValueError::Invalid)?;
    let index = match parse_int(index.trim_matches([' ', '\t']), 0, false, 8) {
        Ok(index) => u8::try_from(index).map_err(|_| ValueError::Other("error.Overflow"))?,
        Err(IntError::Overflow) => return Err(ValueError::Other("error.Overflow")),
        Err(IntError::Invalid) => return Err(ValueError::Invalid),
    };
    Ok((index, parse_color(color)?))
}

/// `Limit.parseCLI`: `unlimited` (`None`) or a `usize` in Zig's base-0 syntax.
///
/// # Errors
///
/// [`ValueError::Invalid`] when the number does not parse.
pub fn parse_limit(value: &str) -> Result<Option<u64>, ValueError> {
    if value == "unlimited" {
        return Ok(None);
    }
    let number = parse_int(value, 0, false, 64).map_err(|_| ValueError::Invalid)?;
    u64::try_from(number).map(Some).map_err(|_| ValueError::Invalid)
}

/// A path value (`config/path.zig`): `?` marks it optional, surrounding double quotes are dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPath {
    pub path: PathBuf,
    pub optional: bool,
}

/// `Path.parse`: `None` for an empty path (after `?` and quotes).
#[must_use]
pub fn parse_path(value: &str) -> Option<ConfigPath> {
    let (optional, rest) = value.strip_prefix('?').map_or((false, value), |rest| (true, rest));
    let rest = if rest.len() >= 2 && rest.starts_with('"') && rest.ends_with('"') {
        &rest[1..rest.len() - 1]
    } else {
        rest
    };
    (!rest.is_empty()).then(|| ConfigPath {
        path: PathBuf::from(rest),
        optional,
    })
}
