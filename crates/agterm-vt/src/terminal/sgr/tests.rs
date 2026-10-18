use super::*;

fn test_parse(params: &[u16]) -> Attribute<'_> {
    let mut p = Parser {
        params,
        ..Default::default()
    };
    p.next().unwrap()
}

fn test_parse_colon(params: &[u16]) -> Attribute<'_> {
    let mut p = Parser {
        params,
        ..Default::default()
    };
    // Mark all parameters except the last as having a colon after.
    for i in 0..params.len() - 1 {
        p.params_sep.set(i);
    }
    p.next().unwrap()
}

/// An RGB color.
const fn rgb(r: u8, g: u8, b: u8) -> color::Rgb {
    color::Rgb { r, g, b }
}

#[test]
fn sgr_parser() {
    assert_eq!(test_parse(&[]), Attribute::Unset);
    assert_eq!(test_parse(&[0]), Attribute::Unset);

    {
        let v = test_parse(&[38, 2, 40, 44, 52]);
        assert_eq!(Attribute::DirectColorFg(rgb(40, 44, 52)), v);
    }

    assert!(matches!(test_parse(&[38, 2, 44, 52]), Attribute::Unknown(_)));

    {
        let v = test_parse(&[48, 2, 40, 44, 52]);
        assert_eq!(Attribute::DirectColorBg(rgb(40, 44, 52)), v);
    }

    assert!(matches!(test_parse(&[48, 2, 44, 52]), Attribute::Unknown(_)));
}

#[test]
fn sgr_parser_multiple() {
    let mut p = Parser {
        params: &[0, 38, 2, 40, 44, 52],
        ..Default::default()
    };
    assert_eq!(p.next().unwrap(), Attribute::Unset);
    assert!(matches!(p.next().unwrap(), Attribute::DirectColorFg(_)));
    assert!(p.next().is_none());
    assert!(p.next().is_none());
}

#[test]
fn sgr_unsupported_with_colon() {
    let mut p = Parser {
        params: &[0, 4, 1],
        params_sep: {
            let mut list = SepList::init_empty();
            list.set(0);
            list
        },
        ..Default::default()
    };
    assert!(matches!(p.next().unwrap(), Attribute::Unknown(_)));
    assert_eq!(p.next().unwrap(), Attribute::Bold);
    assert!(p.next().is_none());
}

#[test]
fn sgr_unsupported_with_multiple_colon() {
    let mut p = Parser {
        params: &[0, 4, 2, 1],
        params_sep: {
            let mut list = SepList::init_empty();
            list.set(0);
            list.set(1);
            list
        },
        ..Default::default()
    };
    assert!(matches!(p.next().unwrap(), Attribute::Unknown(_)));
    assert_eq!(p.next().unwrap(), Attribute::Bold);
    assert!(p.next().is_none());
}

#[test]
fn sgr_bold() {
    {
        let v = test_parse(&[1]);
        assert_eq!(v, Attribute::Bold);
    }

    {
        let v = test_parse(&[22]);
        assert_eq!(v, Attribute::ResetBold);
    }
}

#[test]
fn sgr_italic() {
    {
        let v = test_parse(&[3]);
        assert_eq!(v, Attribute::Italic);
    }

    {
        let v = test_parse(&[23]);
        assert_eq!(v, Attribute::ResetItalic);
    }
}

#[test]
fn sgr_underline() {
    {
        let v = test_parse(&[4]);
        assert!(matches!(v, Attribute::Underline(_)));
    }

    {
        let v = test_parse(&[24]);
        assert_eq!(Attribute::Underline(Underline::None), v);
    }
}

#[test]
fn sgr_underline_styles() {
    {
        let v = test_parse_colon(&[4, 2]);
        assert_eq!(Attribute::Underline(Underline::Double), v);
    }

    {
        let v = test_parse_colon(&[4, 0]);
        assert_eq!(Attribute::Underline(Underline::None), v);
    }

    {
        let v = test_parse_colon(&[4, 1]);
        assert_eq!(Attribute::Underline(Underline::Single), v);
    }

    {
        let v = test_parse_colon(&[4, 3]);
        assert_eq!(Attribute::Underline(Underline::Curly), v);
    }

    {
        let v = test_parse_colon(&[4, 4]);
        assert_eq!(Attribute::Underline(Underline::Dotted), v);
    }

    {
        let v = test_parse_colon(&[4, 5]);
        assert_eq!(Attribute::Underline(Underline::Dashed), v);
    }
}

#[test]
fn sgr_underline_style_with_more() {
    let mut p = Parser {
        params: &[4, 2, 1],
        params_sep: {
            let mut list = SepList::init_empty();
            list.set(0);
            list
        },
        ..Default::default()
    };

    assert!(matches!(p.next().unwrap(), Attribute::Underline(_)));
    assert_eq!(p.next().unwrap(), Attribute::Bold);
    assert!(p.next().is_none());
}

#[test]
fn sgr_underline_style_with_too_many_colons() {
    let mut p = Parser {
        params: &[4, 2, 3, 1],
        params_sep: {
            let mut list = SepList::init_empty();
            list.set(0);
            list.set(1);
            list
        },
        ..Default::default()
    };

    assert!(matches!(p.next().unwrap(), Attribute::Unknown(_)));
    assert_eq!(p.next().unwrap(), Attribute::Bold);
    assert!(p.next().is_none());
}

#[test]
fn sgr_blink() {
    {
        let v = test_parse(&[5]);
        assert_eq!(v, Attribute::Blink);
    }

    {
        let v = test_parse(&[6]);
        assert_eq!(v, Attribute::Blink);
    }

    {
        let v = test_parse(&[25]);
        assert_eq!(v, Attribute::ResetBlink);
    }
}

#[test]
fn sgr_inverse() {
    {
        let v = test_parse(&[7]);
        assert_eq!(v, Attribute::Inverse);
    }

    {
        let v = test_parse(&[27]);
        assert_eq!(v, Attribute::ResetInverse);
    }
}

#[test]
fn sgr_strikethrough() {
    {
        let v = test_parse(&[9]);
        assert_eq!(v, Attribute::Strikethrough);
    }

    {
        let v = test_parse(&[29]);
        assert_eq!(v, Attribute::ResetStrikethrough);
    }
}

#[test]
fn sgr_8_color() {
    let mut p = Parser {
        params: &[31, 43, 90, 103],
        ..Default::default()
    };

    {
        let v = p.next().unwrap();
        assert_eq!(Attribute::Color8Fg(color::Name::Red), v);
    }

    {
        let v = p.next().unwrap();
        assert_eq!(Attribute::Color8Bg(color::Name::Yellow), v);
    }

    {
        let v = p.next().unwrap();
        assert_eq!(Attribute::Color8BrightFg(color::Name::BrightBlack), v);
    }

    {
        let v = p.next().unwrap();
        assert_eq!(Attribute::Color8BrightBg(color::Name::BrightYellow), v);
    }
}

#[test]
fn sgr_256_color() {
    let mut p = Parser {
        params: &[38, 5, 161, 48, 5, 236],
        ..Default::default()
    };
    assert!(matches!(p.next().unwrap(), Attribute::Color256Fg(_)));
    assert!(matches!(p.next().unwrap(), Attribute::Color256Bg(_)));
    assert!(p.next().is_none());
}

#[test]
fn sgr_256_color_underline() {
    let mut p = Parser {
        params: &[58, 5, 9],
        ..Default::default()
    };
    assert!(matches!(p.next().unwrap(), Attribute::Color256Underline(_)));
    assert!(p.next().is_none());
}

#[test]
fn sgr_24_bit_bg_color() {
    {
        let v = test_parse_colon(&[48, 2, 1, 2, 3]);
        assert_eq!(Attribute::DirectColorBg(rgb(1, 2, 3)), v);
    }
}

#[test]
fn sgr_underline_color() {
    {
        let v = test_parse_colon(&[58, 2, 1, 2, 3]);
        assert_eq!(Attribute::UnderlineColor(rgb(1, 2, 3)), v);
    }

    {
        let v = test_parse_colon(&[58, 2, 0, 1, 2, 3]);
        assert_eq!(Attribute::UnderlineColor(rgb(1, 2, 3)), v);
    }
}

#[test]
fn sgr_reset_underline_color() {
    let mut p = Parser {
        params: &[59],
        ..Default::default()
    };
    assert_eq!(p.next().unwrap(), Attribute::ResetUnderlineColor);
}

#[test]
fn sgr_invisible() {
    let mut p = Parser {
        params: &[8, 28],
        ..Default::default()
    };
    assert_eq!(p.next().unwrap(), Attribute::Invisible);
    assert_eq!(p.next().unwrap(), Attribute::ResetInvisible);
}

#[test]
fn sgr_underline_bg_and_fg() {
    let mut p = Parser {
        params: &[4, 38, 2, 255, 247, 219, 48, 2, 242, 93, 147, 4],
        ..Default::default()
    };
    {
        let v = p.next().unwrap();
        assert_eq!(Attribute::Underline(Underline::Single), v);
    }
    {
        let v = p.next().unwrap();
        assert_eq!(Attribute::DirectColorFg(rgb(255, 247, 219)), v);
    }
    {
        let v = p.next().unwrap();
        assert_eq!(Attribute::DirectColorBg(rgb(242, 93, 147)), v);
    }
    {
        let v = p.next().unwrap();
        assert_eq!(Attribute::Underline(Underline::Single), v);
    }
}

#[test]
fn sgr_direct_color_fg_missing_color() {
    // This used to crash
    let mut p = Parser {
        params: &[38, 5],
        ..Default::default()
    };
    while p.next().is_some() {}
}

#[test]
fn sgr_direct_color_bg_missing_color() {
    // This used to crash
    let mut p = Parser {
        params: &[48, 5],
        ..Default::default()
    };
    while p.next().is_some() {}
}

#[test]
fn sgr_direct_fg_bg_underline_ignore_optional_color_space() {
    // These behaviors have been verified against xterm.

    // Colon version should skip the optional color space identifier
    {
        // 3 8 : 2 : Pi : Pr : Pg : Pb
        let v = test_parse_colon(&[38, 2, 0, 1, 2, 3]);
        assert_eq!(Attribute::DirectColorFg(rgb(1, 2, 3)), v);
    }
    {
        // 4 8 : 2 : Pi : Pr : Pg : Pb
        let v = test_parse_colon(&[48, 2, 0, 1, 2, 3]);
        assert_eq!(Attribute::DirectColorBg(rgb(1, 2, 3)), v);
    }
    {
        // 5 8 : 2 : Pi : Pr : Pg : Pb
        let v = test_parse_colon(&[58, 2, 0, 1, 2, 3]);
        assert_eq!(Attribute::UnderlineColor(rgb(1, 2, 3)), v);
    }

    // Semicolon version should not parse optional color space identifier
    {
        // 3 8 ; 2 ; Pr ; Pg ; Pb
        let v = test_parse(&[38, 2, 0, 1, 2, 3]);
        assert_eq!(Attribute::DirectColorFg(rgb(0, 1, 2)), v);
    }
    {
        // 4 8 ; 2 ; Pr ; Pg ; Pb
        let v = test_parse(&[48, 2, 0, 1, 2, 3]);
        assert_eq!(Attribute::DirectColorBg(rgb(0, 1, 2)), v);
    }
    {
        // 5 8 ; 2 ; Pr ; Pg ; Pb
        let v = test_parse(&[58, 2, 0, 1, 2, 3]);
        assert_eq!(Attribute::UnderlineColor(rgb(0, 1, 2)), v);
    }
}

#[test]
fn sgr_direct_fg_colon_with_too_many_colons() {
    let mut p = Parser {
        params: &[38, 2, 0, 1, 2, 3, 4, 1],
        params_sep: {
            let mut list = SepList::init_empty();
            for idx in 0..6 {
                list.set(idx);
            }
            list
        },
        ..Default::default()
    };

    assert!(matches!(p.next().unwrap(), Attribute::Unknown(_)));
    assert_eq!(p.next().unwrap(), Attribute::Bold);
    assert!(p.next().is_none());
}

#[test]
fn sgr_direct_fg_colon_with_colorspace_and_extra_param() {
    let mut p = Parser {
        params: &[38, 2, 0, 1, 2, 3, 1],
        params_sep: {
            let mut list = SepList::init_empty();
            for idx in 0..5 {
                list.set(idx);
            }
            list
        },
        ..Default::default()
    };

    {
        let v = p.next().unwrap();
        assert_eq!(Attribute::DirectColorFg(rgb(1, 2, 3)), v);
    }

    assert_eq!(p.next().unwrap(), Attribute::Bold);
    assert!(p.next().is_none());
}

#[test]
fn sgr_direct_fg_colon_no_colorspace_and_extra_param() {
    let mut p = Parser {
        params: &[38, 2, 1, 2, 3, 1],
        params_sep: {
            let mut list = SepList::init_empty();
            for idx in 0..4 {
                list.set(idx);
            }
            list
        },
        ..Default::default()
    };

    {
        let v = p.next().unwrap();
        assert_eq!(Attribute::DirectColorFg(rgb(1, 2, 3)), v);
    }

    assert_eq!(p.next().unwrap(), Attribute::Bold);
    assert!(p.next().is_none());
}

// Kakoune sent this complex SGR sequence that caused invalid behavior.
#[test]
fn sgr_kakoune_input() {
    // This used to crash
    let mut p = Parser {
        params: &[0, 4, 3, 38, 2, 175, 175, 215, 58, 2, 0, 190, 80, 70],
        params_sep: {
            let mut list = SepList::init_empty();
            list.set(1);
            list.set(8);
            list.set(9);
            list.set(10);
            list.set(11);
            list.set(12);
            list
        },
        ..Default::default()
    };

    {
        let v = p.next().unwrap();
        assert_eq!(v, Attribute::Unset);
    }
    {
        let v = p.next().unwrap();
        assert_eq!(Attribute::Underline(Underline::Curly), v);
    }
    {
        let v = p.next().unwrap();
        assert_eq!(Attribute::DirectColorFg(rgb(175, 175, 215)), v);
    }
    {
        let v = p.next().unwrap();
        assert_eq!(Attribute::UnderlineColor(rgb(190, 80, 70)), v);
    }
}

// Discussion #5930, another input sent by kakoune
#[test]
fn sgr_kakoune_input_issue_underline_fg_and_bg() {
    // echo -e "\033[4:3;38;2;51;51;51;48;2;170;170;170;58;2;255;97;136mset everything in one sequence, broken\033[m"

    // This used to crash
    let mut p = Parser {
        params: &[4, 3, 38, 2, 51, 51, 51, 48, 2, 170, 170, 170, 58, 2, 255, 97, 136],
        params_sep: {
            let mut list = SepList::init_empty();
            list.set(0);
            list
        },
        ..Default::default()
    };

    {
        let v = p.next().unwrap();
        assert_eq!(Attribute::Underline(Underline::Curly), v);
    }

    {
        let v = p.next().unwrap();
        assert_eq!(Attribute::DirectColorFg(rgb(51, 51, 51)), v);
    }

    {
        let v = p.next().unwrap();
        assert_eq!(Attribute::DirectColorBg(rgb(170, 170, 170)), v);
    }

    {
        let v = p.next().unwrap();
        assert_eq!(Attribute::UnderlineColor(rgb(255, 97, 136)), v);
    }

    assert!(p.next().is_none());
}

// Fuzz crash: afl-out/stream/default/crashes/id:000021
// Input "ESC [ 5 8 : 4 : m" produces params [58, 4] with colon
// separator bits set at indices 0 and 1. The trailing colon causes
// the second iteration to see param 4 (underline) with a colon,
// triggering assert(slice.len >= 2) with slice.len == 1.
#[test]
fn sgr_underline_colon_with_trailing_separator_and_short_slice() {
    let mut p = Parser {
        params: &[58, 4],
        params_sep: {
            let mut list = SepList::init_empty();
            list.set(0);
            list.set(1);
            list
        },
        ..Default::default()
    };

    // 58:4 is not a valid underline color (sub-param 4 is not 2 or 5),
    // so it falls through as unknown.
    assert!(matches!(p.next().unwrap(), Attribute::Unknown(_)));

    // Param 4 with a trailing colon but no sub-param is malformed,
    // so it also falls through as unknown rather than panicking.
    assert!(matches!(p.next().unwrap(), Attribute::Unknown(_)));

    assert!(p.next().is_none());
}
