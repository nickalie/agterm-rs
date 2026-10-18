use super::{Size, Style, encode};

fn test_size() -> Size {
    Size {
        rows: 24,
        columns: 80,
        cell_width: 9,
        cell_height: 18,
    }
}

fn encoded(style: Style, size: Size) -> String {
    let mut buf = String::new();
    encode(&mut buf, style, size).unwrap();
    buf
}

#[test]
fn encode_mode_2048() {
    assert_eq!("\x1B[48;24;80;432;720t", encoded(Style::Mode2048, test_size()));
}

#[test]
fn encode_csi_14_t() {
    assert_eq!("\x1b[4;432;720t", encoded(Style::Csi14T, test_size()));
}

#[test]
fn encode_csi_16_t() {
    assert_eq!("\x1b[6;18;9t", encoded(Style::Csi16T, test_size()));
}

#[test]
fn encode_csi_18_t() {
    assert_eq!("\x1b[8;24;80t", encoded(Style::Csi18T, test_size()));
}

/// Drift `6104c78f1`: CSI 19 t reports the text area in characters, as CSI 18 t does, under its own code.
#[test]
fn encode_csi_19_t() {
    assert_eq!("\x1b[9;24;80t", encoded(Style::Csi19T, test_size()));
}

#[test]
fn encode_max_values_for_all_fields() {
    let max_size = Size {
        rows: u16::MAX,
        columns: u16::MAX,
        cell_width: u32::MAX,
        cell_height: u32::MAX,
    };
    let cases = [
        (Style::Mode2048, "\x1B[48;65535;65535;281470681677825;281470681677825t"),
        (Style::Csi14T, "\x1b[4;281470681677825;281470681677825t"),
        (Style::Csi16T, "\x1b[6;4294967295;4294967295t"),
        (Style::Csi18T, "\x1b[8;65535;65535t"),
        (Style::Csi19T, "\x1b[9;65535;65535t"),
    ];
    for (style, expected) in cases {
        assert_eq!(expected, encoded(style, max_size));
    }
}
