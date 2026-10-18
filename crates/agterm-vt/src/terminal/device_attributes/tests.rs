use super::*;

#[test]
fn primary_default() {
    let mut buf = String::new();
    Primary::default().encode(&mut buf).unwrap();
    assert_eq!("\x1b[?62;22c", buf);
}

#[test]
fn primary_with_clipboard() {
    let mut buf = String::new();
    Primary {
        features: &[Feature::AnsiColor, Feature::Clipboard],
        ..Primary::default()
    }
    .encode(&mut buf)
    .unwrap();
    assert_eq!("\x1b[?62;22;52c", buf);
}

#[test]
fn primary_with_multiple_features() {
    let mut buf = String::new();
    Primary {
        conformance_level: ConformanceLevel::VT420,
        features: &[Feature::Columns132, Feature::SelectiveErase, Feature::AnsiColor],
    }
    .encode(&mut buf)
    .unwrap();
    assert_eq!("\x1b[?64;1;6;22c", buf);
}

#[test]
fn primary_no_features() {
    let mut buf = String::new();
    Primary {
        conformance_level: ConformanceLevel::Vt100,
        features: &[],
    }
    .encode(&mut buf)
    .unwrap();
    assert_eq!("\x1b[?1c", buf);
}

#[test]
fn secondary_default() {
    let mut buf = String::new();
    Secondary::default().encode(&mut buf).unwrap();
    assert_eq!("\x1b[>1;0;0c", buf);
}

#[test]
fn tertiary_default() {
    let mut buf = String::new();
    Tertiary::default().encode(&mut buf).unwrap();
    assert_eq!("\x1bP!|00000000\x1b\\", buf);
}

#[test]
fn tertiary_custom_unit_id() {
    let mut buf = String::new();
    Tertiary { unit_id: 0xAABB_CCDD }.encode(&mut buf).unwrap();
    assert_eq!("\x1bP!|AABBCCDD\x1b\\", buf);
}

/// Each request encodes its own reply.
#[test]
fn attributes_encode_each_request() {
    let attributes = Attributes::default();
    for (req, want) in [
        (Req::Primary, "\x1b[?62;22c"),
        (Req::Secondary, "\x1b[>1;0;0c"),
        (Req::Tertiary, "\x1bP!|00000000\x1b\\"),
    ] {
        let mut buf = String::new();
        attributes.encode(req, &mut buf).unwrap();
        assert_eq!(want, buf, "{req:?}");
    }
}

/// The conformance levels are the numbers DA1 reports: the VT100 series per model, 60 plus the level from the VT200
/// series on.
#[test]
fn conformance_levels_are_their_wire_numbers() {
    let levels = [
        (ConformanceLevel::Vt100, 1),
        (ConformanceLevel::VT101, 1),
        (ConformanceLevel::Vt132, 4),
        (ConformanceLevel::Vt102, 6),
        (ConformanceLevel::Vt131, 7),
        (ConformanceLevel::Vt125, 12),
        (ConformanceLevel::VT220, 62),
        (ConformanceLevel::VT240, 62),
        (ConformanceLevel::VT320, 63),
        (ConformanceLevel::VT340, 63),
        (ConformanceLevel::VT420, 64),
        (ConformanceLevel::VT510, 65),
        (ConformanceLevel::VT520, 65),
        (ConformanceLevel::VT525, 65),
        (ConformanceLevel::Other(99), 99),
    ];
    for (level, want) in levels {
        assert_eq!(want, level.int(), "{level:?}");
    }
}

/// The device types are xterm's `decTerminalID` numbers, which DA2 reports.
#[test]
fn device_types_are_their_wire_numbers() {
    let types = [
        (DeviceType::Vt100, 0),
        (DeviceType::Vt220, 1),
        (DeviceType::Vt240, 2),
        (DeviceType::Vt330, 18),
        (DeviceType::Vt340, 19),
        (DeviceType::Vt320, 24),
        (DeviceType::Vt382, 32),
        (DeviceType::Vt420, 41),
        (DeviceType::Vt510, 61),
        (DeviceType::Vt520, 64),
        (DeviceType::Vt525, 65),
        (DeviceType::Other(7), 7),
    ];
    for (device_type, want) in types {
        assert_eq!(want, device_type.int(), "{device_type:?}");
    }
}

/// Every DA1 feature is its attribute code.
#[test]
fn features_are_their_wire_numbers() {
    let features = [
        (Feature::Columns132, 1),
        (Feature::Printer, 2),
        (Feature::Regis, 3),
        (Feature::Sixel, 4),
        (Feature::SelectiveErase, 6),
        (Feature::UserDefinedKeys, 8),
        (Feature::NationalReplacement, 9),
        (Feature::TechnicalCharacters, 15),
        (Feature::Locator, 16),
        (Feature::TerminalState, 17),
        (Feature::Windowing, 18),
        (Feature::HorizontalScrolling, 21),
        (Feature::AnsiColor, 22),
        (Feature::RectangularEditing, 28),
        (Feature::AnsiTextLocator, 29),
        (Feature::Clipboard, 52),
        (Feature::Other(70), 70),
    ];
    for (feature, want) in features {
        assert_eq!(want, feature.int(), "{feature:?}");
    }
}
