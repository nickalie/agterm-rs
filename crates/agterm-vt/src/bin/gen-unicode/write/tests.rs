//! The writer's layout: arrays packed as rustfmt packs them, the struct literals, the case folding lines and the
//! headers naming the inputs.

use super::{array, case_folding_data, grapheme_data, header, props_data, symbols_data};
use crate::Inputs;
use crate::lut::Tables;
use crate::props::{GraphemeBreakNoControl, Properties};

fn inputs() -> Inputs {
    Inputs {
        uucode_version: "0.2.0".to_owned(),
        uucode_rev_short: "9d555245".to_owned(),
        ucd_version: "18.0.0".to_owned(),
    }
}

fn items(values: &[u32]) -> Vec<String> {
    values.iter().map(ToString::to_string).collect()
}

/// A header's text: its doc lines without the `//!`, joined by spaces, an empty doc line as `|`.
fn prose(out: &str) -> String {
    out.lines()
        .take_while(|line| line.starts_with("//!"))
        .map(|line| line.strip_prefix("//! ").unwrap_or("|"))
        .collect::<Vec<&str>>()
        .join(" ")
}

#[test]
fn a_short_array_stays_on_one_line() {
    let mut out = String::new();
    array(
        &mut out,
        "pub(crate) static STAGE3",
        "bool",
        &["false".to_owned(), "true".to_owned()],
    );
    assert_eq!(out, "pub(crate) static STAGE3: [bool; 2] = [false, true];\n");
}

/// Lines stay below 120 columns: 38 one-digit items to a line (117 columns), not 39 (120).
#[test]
fn a_long_array_is_packed_below_the_maximum_width() {
    let mut out = String::new();
    array(&mut out, "pub(crate) const DATA", "u8", &items(&[0; 80]));
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "pub(crate) const DATA: [u8; 80] = [");
    assert_eq!(lines[1], format!("    {},", ["0"; 38].join(", ")));
    assert_eq!(lines[1].len(), 117);
    assert_eq!(lines[2], lines[1]);
    assert_eq!(lines[3], "    0, 0, 0, 0,");
    assert_eq!(lines[4], "];");
    assert_eq!(lines.len(), 5);
}

/// An array short enough for one line but wider than rustfmt's `array_width` (72) is packed.
#[test]
fn an_array_wider_than_the_array_width_is_packed() {
    let mut out = String::new();
    array(&mut out, "static A", "u16", &items(&[10_000; 11]));
    assert_eq!(out.lines().count(), 3, "{out}");
    let mut out = String::new();
    array(&mut out, "static A", "u16", &items(&[10_000; 10]));
    assert_eq!(out.lines().count(), 1, "{out}");
}

#[test]
fn a_header_wraps_its_paragraphs_within_the_width() {
    let mut out = String::new();
    let long = ["word"; 60].join(" ");
    header(&mut out, &[long.clone(), "second".to_owned()]);
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines.iter().all(|line| line.len() <= 120), "{out}");
    // 23 words of four letters and their spaces after `//!`
    assert_eq!(lines[0].len(), 118, "{out}");
    assert_eq!(&lines[lines.len() - 3..], ["//!", "//! second", ""]);
    assert_eq!(prose(&out), format!("{long} | second"));
}

/// rustty's `write_zig` test stages, written as Rust.
#[test]
fn symbols_data_writes_the_three_stages() {
    let tables = Tables {
        stage1: vec![0, 256],
        stage2: vec![0, 1, 1],
        stage3: vec![false, true],
    };
    let out = symbols_data(&inputs(), &tables);
    let body = out.split("\n\n").nth(1).unwrap();
    assert_eq!(
        body,
        "pub(crate) static STAGE1: [u16; 2] = [0, 256];\npub(crate) static STAGE2: [u16; 3] = [0, 1, 1];\n\
         pub(crate) static STAGE3: [bool; 2] = [false, true];\n"
    );
    let prose = prose(&out);
    assert!(
        prose.contains("from uucode 0.2.0 @ `9d555245` and the UCD 18.0.0 it ships."),
        "{prose}"
    );
}

#[test]
fn props_data_writes_each_properties_one_field_to_a_line() {
    let tables = Tables {
        stage1: vec![0],
        stage2: vec![0],
        stage3: vec![Properties {
            width: 2,
            width_zero_in_grapheme: false,
            grapheme_break: GraphemeBreakNoControl::IndicConjunctBreakLinkerOther,
            emoji_vs_base: true,
        }],
    };
    let out = props_data(&inputs(), &tables);
    assert!(out.contains("\n\nuse crate::unicode::props::{GraphemeBreakNoControl, Properties};\n\n"));
    assert!(out.ends_with(
        "pub(crate) static STAGE3: [Properties; 1] = [\n    Properties {\n        width: 2,\n        \
         width_zero_in_grapheme: false,\n        \
         grapheme_break: GraphemeBreakNoControl::IndicConjunctBreakLinkerOther,\n        \
         emoji_vs_base: true,\n    },\n];\n"
    ));
    let prose = prose(&out);
    assert!(
        prose.contains("over the fields of uucode 0.2.0 @ `9d555245`"),
        "{prose}"
    );
    assert!(prose.contains("from the UCD 18.0.0 that uucode ships"), "{prose}");
}

#[test]
fn grapheme_data_writes_the_bytes() {
    let mut data = [0_u8; 8192];
    data[0] = 1;
    data[1] = 9;
    let out = grapheme_data(&inputs(), &data);
    assert!(out.contains("pub(crate) const DATA: [u8; 8192] = [\n    1, 9, 0, 0,"));
    assert!(out.ends_with("0, 0,\n];\n"));
    assert!(prose(&out).contains("over the rules of uucode 0.2.0 @ `9d555245`,"));
}

#[test]
fn case_folding_data_writes_one_padded_entry_a_line_with_its_counts() {
    let folds = [
        (0x41, vec![0x61]),
        (0x390, vec![0x3B9, 0x308, 0x301]),
        (0x1E9E, vec![0x73, 0x73]),
    ];
    let out = case_folding_data(&inputs(), &folds, 2);
    let prose = prose(&out);
    assert!(prose.contains("1 C entries and 2 F entries, 3 code points."), "{prose}");
    assert!(
        prose.contains("from the UCD 18.0.0 that uucode 0.2.0 @ `9d555245` ships."),
        "{prose}"
    );
    assert!(out.ends_with(
        "#[rustfmt::skip]\npub(crate) const CASE_FOLDING_FULL: &[(u32, [u32; 3])] = &[\n    \
         (0x0041, [0x0061, 0x0000, 0x0000]),\n    (0x0390, [0x03B9, 0x0308, 0x0301]),\n    \
         (0x1E9E, [0x0073, 0x0073, 0x0000]),\n];\n"
    ));
}
