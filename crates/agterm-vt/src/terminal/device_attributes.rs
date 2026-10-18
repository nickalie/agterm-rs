//! Device attributes (DA1, DA2, DA3): the replies to `CSI c`, `CSI > c` and `CSI = c`.
//!
//! Ghostty: `src/terminal/device_attributes.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/device_attributes.rs` at `RUSTTY_REV` (MIT).
//! Changes: the encoders write to a `core::fmt::Write`; Zig's non-exhaustive `enum(u16)`s keep their unknown values
//! in an `Other` variant, and the VT model names are Rust-cased (`Vt220`).

use core::fmt::Write;

/// The device attribute request type (CSI c).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Req {
    Primary,   // Blank
    Secondary, // >
    Tertiary,  // =
}

/// Response data for all device attribute queries.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Attributes {
    /// Reply to CSI c (DA1).
    pub(crate) primary: Primary,

    /// Reply to CSI > c (DA2).
    pub(crate) secondary: Secondary,

    /// Reply to CSI = c (DA3).
    pub(crate) tertiary: Tertiary,
}

impl Attributes {
    /// Encode the response for the given request type into the writer.
    pub(crate) fn encode<W: Write>(&self, req: Req, writer: &mut W) -> core::fmt::Result {
        match req {
            Req::Primary => self.primary.encode(writer),
            Req::Secondary => self.secondary.encode(writer),
            Req::Tertiary => self.tertiary.encode(writer),
        }
    }
}

/// Primary device attributes (DA1).
///
/// Response format: CSI ? Pp ; Ps... c
/// where Pp is the conformance level and Ps are feature flags.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Primary {
    /// Conformance level sent as the first parameter.
    pub(crate) conformance_level: ConformanceLevel,

    /// Optional feature attributes.
    pub(crate) features: &'static [Feature],
}

impl Default for Primary {
    fn default() -> Self {
        Primary {
            conformance_level: ConformanceLevel::VT220,
            features: &[Feature::AnsiColor],
        }
    }
}

/// DA1 feature attribute codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Feature {
    Columns132,
    Printer,
    Regis,
    Sixel,
    SelectiveErase,
    UserDefinedKeys,
    NationalReplacement,
    TechnicalCharacters,
    Locator,
    TerminalState,
    Windowing,
    HorizontalScrolling,
    AnsiColor,
    RectangularEditing,
    AnsiTextLocator,
    Clipboard,
    Other(u16),
}

impl Feature {
    fn int(self) -> u16 {
        match self {
            Feature::Columns132 => 1,
            Feature::Printer => 2,
            Feature::Regis => 3,
            Feature::Sixel => 4,
            Feature::SelectiveErase => 6,
            Feature::UserDefinedKeys => 8,
            Feature::NationalReplacement => 9,
            Feature::TechnicalCharacters => 15,
            Feature::Locator => 16,
            Feature::TerminalState => 17,
            Feature::Windowing => 18,
            Feature::HorizontalScrolling => 21,
            Feature::AnsiColor => 22,
            Feature::RectangularEditing => 28,
            Feature::AnsiTextLocator => 29,
            Feature::Clipboard => 52,
            Feature::Other(v) => v,
        }
    }
}

impl Primary {
    /// Encode the primary DA response into the writer.
    pub(crate) fn encode<W: Write>(&self, writer: &mut W) -> core::fmt::Result {
        write!(writer, "\x1b[?{}", self.conformance_level.int())?;
        for feature in self.features {
            write!(writer, ";{}", feature.int())?;
        }
        writer.write_str("c")
    }
}

/// Secondary device attributes (DA2).
///
/// Response format: CSI > Pp ; Pv ; Pc c
#[derive(Clone, Copy, Debug)]
pub(crate) struct Secondary {
    /// Terminal type identifier (Pp parameter from secondary DA response).
    pub(crate) device_type: DeviceType,

    /// Firmware/patch version number.
    pub(crate) firmware_version: u16,

    /// ROM cartridge registration number. Always 0 for emulators.
    pub(crate) rom_cartridge: u16,
}

impl Default for Secondary {
    fn default() -> Self {
        Secondary {
            device_type: DeviceType::Vt220,
            firmware_version: 0,
            rom_cartridge: 0,
        }
    }
}

impl Secondary {
    /// Encode the secondary DA response into the writer.
    pub(crate) fn encode<W: Write>(self, writer: &mut W) -> core::fmt::Result {
        write!(
            writer,
            "\x1b[>{};{};{}c",
            self.device_type.int(),
            self.firmware_version,
            self.rom_cartridge,
        )
    }
}

/// Tertiary device attributes (DA3).
///
/// Response format: DCS ! | D...D ST
/// where D...D is the unit ID as hex digits (DECRPTUI).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Tertiary {
    /// Unit ID (DECRPTUI). Encoded as 8 uppercase hex digits.
    /// Meaningless for emulators nowadays. The actual DEC manuals
    /// appear to split this into two 16-bit fields but since there
    /// is no practical usage I know if I'm simplifying this.
    pub(crate) unit_id: u32,
}

impl Tertiary {
    /// Encode the tertiary DA response into the writer.
    pub(crate) fn encode<W: Write>(self, writer: &mut W) -> core::fmt::Result {
        write!(writer, "\x1bP!|{:08X}\x1b\\", self.unit_id)
    }
}

/// Conformance level reported as the first parameter (Pp) in the
/// primary device attributes (DA1) response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConformanceLevel {
    // Vt100-series have per-model values.
    Vt100,
    Vt132,
    Vt102,
    Vt131,
    Vt125,

    // Vt200+ use 60 + decTerminalID/100.
    /// Level 2 conformance (Vt200 series, e.g. Vt220, Vt240).
    Level2,
    /// Level 3 conformance (Vt300 series, e.g. Vt320, Vt340).
    Level3,
    /// Level 4 conformance (Vt400 series, e.g. Vt420).
    Level4,
    /// Level 5 conformance (Vt500 series, e.g. Vt510, Vt520, Vt525).
    Level5,

    Other(u16),
}

impl ConformanceLevel {
    pub(crate) const VT101: ConformanceLevel = ConformanceLevel::Vt100;
    pub(crate) const VT220: ConformanceLevel = ConformanceLevel::Level2;
    pub(crate) const VT240: ConformanceLevel = ConformanceLevel::Level2;
    pub(crate) const VT320: ConformanceLevel = ConformanceLevel::Level3;
    pub(crate) const VT340: ConformanceLevel = ConformanceLevel::Level3;
    pub(crate) const VT420: ConformanceLevel = ConformanceLevel::Level4;
    pub(crate) const VT510: ConformanceLevel = ConformanceLevel::Level5;
    pub(crate) const VT520: ConformanceLevel = ConformanceLevel::Level5;
    pub(crate) const VT525: ConformanceLevel = ConformanceLevel::Level5;

    fn int(self) -> u16 {
        match self {
            ConformanceLevel::Vt100 => 1,
            ConformanceLevel::Vt132 => 4,
            ConformanceLevel::Vt102 => 6,
            ConformanceLevel::Vt131 => 7,
            ConformanceLevel::Vt125 => 12,
            ConformanceLevel::Level2 => 62,
            ConformanceLevel::Level3 => 63,
            ConformanceLevel::Level4 => 64,
            ConformanceLevel::Level5 => 65,
            ConformanceLevel::Other(v) => v,
        }
    }
}

/// Terminal type identifier reported as the Pp parameter in the
/// secondary device attributes (DA2) response. Values correspond
/// to the decTerminalID resource in xterm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeviceType {
    Vt100,
    Vt220,
    Vt240,
    Vt330,
    Vt340,
    Vt320,
    Vt382,
    Vt420,
    Vt510,
    Vt520,
    Vt525,
    Other(u16),
}

impl DeviceType {
    fn int(self) -> u16 {
        match self {
            DeviceType::Vt100 => 0,
            DeviceType::Vt220 => 1,
            DeviceType::Vt240 => 2,
            DeviceType::Vt330 => 18,
            DeviceType::Vt340 => 19,
            DeviceType::Vt320 => 24,
            DeviceType::Vt382 => 32,
            DeviceType::Vt420 => 41,
            DeviceType::Vt510 => 61,
            DeviceType::Vt520 => 64,
            DeviceType::Vt525 => 65,
            DeviceType::Other(v) => v,
        }
    }
}

#[cfg(test)]
mod tests;
