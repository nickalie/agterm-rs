use super::{
    ColorScheme, MAX_COLOR_SCHEME_REPORT_ENCODE_SIZE, MAX_VISIBILITY_REPORT_ENCODE_SIZE, Request, Visibility,
    encode_color_scheme_report, encode_visibility_report, question, req_from_int,
};

#[test]
fn encode_color_scheme_report_dark() {
    assert_eq!(9, MAX_COLOR_SCHEME_REPORT_ENCODE_SIZE);
    let mut buf = Vec::new();
    encode_color_scheme_report(&mut buf, ColorScheme::Dark);
    assert_eq!(b"\x1B[?997;1n", &buf[..]);
}

#[test]
fn encode_color_scheme_report_light() {
    let mut buf = Vec::new();
    encode_color_scheme_report(&mut buf, ColorScheme::Light);
    assert_eq!(b"\x1B[?997;2n", &buf[..]);
    assert!(buf.len() <= MAX_COLOR_SCHEME_REPORT_ENCODE_SIZE);
}

#[test]
fn encode_visibility_report_test() {
    let mut buf = Vec::new();
    encode_visibility_report(&mut buf, Visibility::PotentiallyVisible);
    assert_eq!(b"\x1B[?999;1n", &buf[..]);
    assert!(buf.len() <= MAX_VISIBILITY_REPORT_ENCODE_SIZE);

    buf.clear();
    encode_visibility_report(&mut buf, Visibility::NotVisible);
    assert_eq!(b"\x1B[?999;2n", &buf[..]);
    assert!(buf.len() <= MAX_VISIBILITY_REPORT_ENCODE_SIZE);
}

/// Zig's `Tag` test "order": the value is the low bits, the question bit is on top.
#[test]
fn order() {
    assert_eq!(Request::OperatingStatus.tag(), 5);
    assert_eq!(Request::CursorPosition.tag(), 6);
    assert_eq!(Request::ColorScheme.tag(), question(996));
}

#[test]
fn requests_from_numbers() {
    assert_eq!(req_from_int(5, false), Some(Request::OperatingStatus));
    assert_eq!(req_from_int(6, false), Some(Request::CursorPosition));
    assert_eq!(req_from_int(996, true), Some(Request::ColorScheme));
    assert_eq!(req_from_int(998, true), Some(Request::Visibility));
    // the wrong prefix, an unknown number, a number past 15 bits
    assert_eq!(req_from_int(5, true), None);
    assert_eq!(req_from_int(996, false), None);
    assert_eq!(req_from_int(7, false), None);
    assert_eq!(req_from_int(question(996), false), None);
}

/// Drift `fbda61b87`: `CSI ? 6 n` is DECXCPR, the extended cursor position report, beside the plain CPR.
#[test]
fn the_question_form_of_6_is_decxcpr() {
    assert_eq!(req_from_int(6, true), Some(Request::CursorPositionExtended));
    assert_eq!(Request::CursorPositionExtended.tag(), question(6));
}
