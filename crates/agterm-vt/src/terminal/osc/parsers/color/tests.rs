use super::*;

#[test]
fn osc_4_empty_param() {
    let mut p = Parser::new(false);

    let input = b"4;;";
    for &ch in input {
        p.next(ch);
    }

    let cmd = p.end(Some(b'\x1b'));
    assert!(cmd.is_none());
}

const SPECIAL_FIELDS: [SpecialColor; 5] = [
    SpecialColor::Bold,
    SpecialColor::Underline,
    SpecialColor::Blink,
    SpecialColor::Reverse,
    SpecialColor::Italic,
];

const DYNAMIC_FIELDS: [(DynamicColor, u32); 10] = [
    (DynamicColor::Foreground, 10),
    (DynamicColor::Background, 11),
    (DynamicColor::Cursor, 12),
    (DynamicColor::PointerForeground, 13),
    (DynamicColor::PointerBackground, 14),
    (DynamicColor::TektronixForeground, 15),
    (DynamicColor::TektronixBackground, 16),
    (DynamicColor::HighlightBackground, 17),
    (DynamicColor::TektronixCursor, 18),
    (DynamicColor::HighlightForeground, 19),
];

#[test]
fn osc_color_indexes_reject_digit_separators() {
    for op in [Operation::Osc4, Operation::Osc5] {
        let list = parse_color(op, b"1;red;0_3;blue;2;green").unwrap();
        // Invalid indexes stop get/set requests at the valid prefix.
        assert_eq!(1, list.len());
    }

    for op in [Operation::Osc104, Operation::Osc105] {
        let list = parse_color(op, b"1;0_3;2").unwrap();
        // Invalid indexes are skipped in reset requests.
        assert_eq!(2, list.len());
    }
}

#[test]
fn osc_4() {
    // Test every palette index
    for idx in 0..255u8 {
        // Simple color set
        // printf '\e]4;0;red\\'
        {
            let body = format!("{idx};red");

            let list = parse_color(Operation::Osc4, body.as_bytes()).unwrap();
            assert_eq!(1, list.len());
            assert_eq!(
                Request::Set(ColoredTarget {
                    target: Target::Palette(idx),
                    color: Rgb { r: 255, g: 0, b: 0 },
                }),
                list[0],
            );
        }

        // Simple color query
        // printf '\e]4;0;?\\'
        {
            let body = format!("{idx};?");

            let list = parse_color(Operation::Osc4, body.as_bytes()).unwrap();
            assert_eq!(1, list.len());
            assert_eq!(Request::Query(Target::Palette(idx)), list[0],);
        }

        // Trailing invalid data produces results up to that point
        // printf '\e]4;0;red;\e\\'
        {
            let body = format!("{idx};red;");

            let list = parse_color(Operation::Osc4, body.as_bytes()).unwrap();
            assert_eq!(1, list.len());
            assert_eq!(
                Request::Set(ColoredTarget {
                    target: Target::Palette(idx),
                    color: Rgb { r: 255, g: 0, b: 0 },
                }),
                list[0],
            );
        }

        // Whitespace doesn't produce a working value in xterm but we
        // allow it because Kitty does and it seems harmless.
        //
        // printf '\e]4;0;red \e\\'
        {
            let body = format!("{idx};red ");

            let list = parse_color(Operation::Osc4, body.as_bytes()).unwrap();
            assert_eq!(1, list.len());
            assert_eq!(
                Request::Set(ColoredTarget {
                    target: Target::Palette(idx),
                    color: Rgb { r: 255, g: 0, b: 0 },
                }),
                list[0],
            );
        }
    }

    // Test every special color
    for (i, &special) in SPECIAL_FIELDS.iter().enumerate() {
        // Simple color set
        // printf '\e]4;256;red\\'
        {
            let body = format!("{};red", 256 + i);

            let list = parse_color(Operation::Osc4, body.as_bytes()).unwrap();
            assert_eq!(1, list.len());
            assert_eq!(
                Request::Set(ColoredTarget {
                    target: Target::Special(special),
                    color: Rgb { r: 255, g: 0, b: 0 },
                }),
                list[0],
            );
        }
    }
}

#[test]
fn osc_5() {
    // Test every special color
    for (i, &special) in SPECIAL_FIELDS.iter().enumerate() {
        // Simple color set
        // printf '\e]4;256;red\\'
        {
            let body = format!("{i};red");

            let list = parse_color(Operation::Osc5, body.as_bytes()).unwrap();
            assert_eq!(1, list.len());
            assert_eq!(
                Request::Set(ColoredTarget {
                    target: Target::Special(special),
                    color: Rgb { r: 255, g: 0, b: 0 },
                }),
                list[0],
            );
        }
    }
}

// No Ghostty test. The color index is parsed with
// `lib.parseInt(u9, color_str, 10)` (`73768913b`), which rejects `_`
// digit separators and, the type being unsigned, a leading `+` or `-`
// (`std.fmt.parseInt`, used before, accepted `+11`, `-0` and `1_0`), and
// rejects anything past `u9`. A rejected index takes the same failure path
// as any bad index.
#[test]
fn osc_4_104_color_index_follows_zig_parse_int_but_refuses_digit_separators() {
    // A `_` fails the parse, which ends an OSC 4 list.
    assert_eq!(0, parse_color(Operation::Osc4, b"1_0;red;+11;blue").unwrap().len());

    // So does a sign: the list stops at `+11`.
    let list = parse_color(Operation::Osc4, b"10;red;+11;blue").unwrap();
    assert_eq!(1, list.len());
    assert_eq!(
        Request::Set(ColoredTarget {
            target: Target::Palette(10),
            color: Rgb { r: 255, g: 0, b: 0 },
        }),
        list[0]
    );

    // OSC 104 skips a bad index (`_` or a sign) and goes on.
    let list = parse_color(Operation::Osc104, b"1_2;_3;3_;-0;+1;5_12;4;12").unwrap();
    assert_eq!(2, list.len());
    assert_eq!(Request::Reset(Target::Palette(4)), list[0]);
    assert_eq!(Request::Reset(Target::Palette(12)), list[1]);

    // A leading `_` fails the parse, which ends an OSC 4 list.
    assert_eq!(0, parse_color(Operation::Osc4, b"_1;red").unwrap().len());
}

#[test]
fn osc_4_multiple_requests() {
    // printf '\e]4;0;red;1;blue\e\\'
    {
        let list = parse_color(Operation::Osc4, b"0;red;1;blue").unwrap();
        assert_eq!(2, list.len());
        assert_eq!(
            Request::Set(ColoredTarget {
                target: Target::Palette(0),
                color: Rgb { r: 255, g: 0, b: 0 },
            }),
            list[0],
        );
        assert_eq!(
            Request::Set(ColoredTarget {
                target: Target::Palette(1),
                color: Rgb { r: 0, g: 0, b: 255 },
            }),
            list[1],
        );
    }

    // Multiple requests with same index overwrite each other
    // printf '\e]4;0;red;0;blue\e\\'
    {
        let list = parse_color(Operation::Osc4, b"0;red;0;blue").unwrap();
        assert_eq!(2, list.len());
        assert_eq!(
            Request::Set(ColoredTarget {
                target: Target::Palette(0),
                color: Rgb { r: 255, g: 0, b: 0 },
            }),
            list[0],
        );
        assert_eq!(
            Request::Set(ColoredTarget {
                target: Target::Palette(0),
                color: Rgb { r: 0, g: 0, b: 255 },
            }),
            list[1],
        );
    }
}

#[test]
fn osc_104() {
    // Test every palette index
    for idx in 0..255u8 {
        // Simple color set
        // printf '\e]104;0\\'
        {
            let body = format!("{idx}");

            let list = parse_color(Operation::Osc104, body.as_bytes()).unwrap();
            assert_eq!(1, list.len());
            assert_eq!(Request::Reset(Target::Palette(idx)), list[0],);
        }
    }

    // Test every special color
    for (i, &special) in SPECIAL_FIELDS.iter().enumerate() {
        // Simple color set
        // printf '\e]104;256\\'
        {
            let body = format!("{}", 256 + i);

            let list = parse_color(Operation::Osc104, body.as_bytes()).unwrap();
            assert_eq!(1, list.len());
            assert_eq!(Request::Reset(Target::Special(special)), list[0],);
        }
    }
}

#[test]
fn osc_104_empty_index() {
    let list = parse_color(Operation::Osc104, b"0;;1").unwrap();
    assert_eq!(2, list.len());
    assert_eq!(Request::Reset(Target::Palette(0)), list[0],);
    assert_eq!(Request::Reset(Target::Palette(1)), list[1],);
}

#[test]
fn osc_104_invalid_index() {
    let list = parse_color(Operation::Osc104, b"ffff;1").unwrap();
    assert_eq!(1, list.len());
    assert_eq!(Request::Reset(Target::Palette(1)), list[0],);
}

#[test]
fn osc_104_reset_all() {
    let list = parse_color(Operation::Osc104, b"").unwrap();
    assert_eq!(1, list.len());
    assert_eq!(Request::ResetPalette, list[0],);
}

#[test]
fn osc_105_reset_all() {
    let list = parse_color(Operation::Osc105, b"").unwrap();
    assert_eq!(1, list.len());
    assert_eq!(Request::ResetSpecial, list[0],);
}

// OSC 10-19: Get/Set Dynamic Colors
#[test]
fn osc_10_osc_11_osc_12_osc_13_osc_14_osc_15_osc_16_osc_17_osc_18_osc_19_dynamic() {
    for (color, value) in DYNAMIC_FIELDS {
        let op = operation_from_value(value);

        // Example script:
        // printf '\e]10;red\e\\'
        {
            let list = parse_color(op, b"red").unwrap();
            assert_eq!(1, list.len());
            assert_eq!(
                Request::Set(ColoredTarget {
                    target: Target::Dynamic(color),
                    color: Rgb { r: 255, g: 0, b: 0 },
                }),
                list[0],
            );
        }
    }
}

#[test]
fn osc_10_osc_11_osc_12_osc_13_osc_14_osc_15_osc_16_osc_17_osc_18_osc_19_dynamic_multiple() {
    // Example script:
    // printf '\e]11;red;blue\e\\'
    {
        let list = parse_color(Operation::Osc11, b"red;blue").unwrap();
        assert_eq!(2, list.len());
        assert_eq!(
            Request::Set(ColoredTarget {
                target: Target::Dynamic(DynamicColor::Background),
                color: Rgb { r: 255, g: 0, b: 0 },
            }),
            list[0],
        );
        assert_eq!(
            Request::Set(ColoredTarget {
                target: Target::Dynamic(DynamicColor::Cursor),
                color: Rgb { r: 0, g: 0, b: 255 },
            }),
            list[1],
        );
    }
}

// OSC 110-119: Reset Dynamic Colors
#[test]
fn osc_110_osc_111_osc_112_osc_113_osc_114_osc_115_osc_116_osc_117_osc_118_osc_119_reset_dynamic() {
    for (color, value) in DYNAMIC_FIELDS {
        let op = operation_from_reset_value(value);

        // Example script:
        // printf '\e]110\e\\'
        {
            let list = parse_color(op, b"").unwrap();
            assert_eq!(1, list.len());
            assert_eq!(Request::Reset(Target::Dynamic(color)), list[0],);
        }

        // xterm allows a trailing semicolon. script to verify:
        //
        // printf '\e]110;\e\\'
        {
            let list = parse_color(op, b";").unwrap();
            assert_eq!(1, list.len());
            assert_eq!(Request::Reset(Target::Dynamic(color)), list[0],);
        }

        // xterm does NOT allow any whitespace
        //
        // printf '\e]110 \e\\'
        {
            let list = parse_color(op, b" ").unwrap();
            assert_eq!(0, list.len());
        }
    }
}

fn operation_from_value(value: u32) -> Operation {
    match value {
        10 => Operation::Osc10,
        11 => Operation::Osc11,
        12 => Operation::Osc12,
        13 => Operation::Osc13,
        14 => Operation::Osc14,
        15 => Operation::Osc15,
        16 => Operation::Osc16,
        17 => Operation::Osc17,
        18 => Operation::Osc18,
        19 => Operation::Osc19,
        _ => unreachable!(),
    }
}

// REGRESSION (fuzz finding, `cargo fuzz run fuzz_osc`, 2026-08-11): a color
// spec holding a multi-byte UTF-8 scalar used to PANIC inside `RGB::parse`
// (byte ranges applied to a `&str`). Ghostty's
// `RGB.parse(value: []const u8)` (ghostty src/terminal/color.zig:657) byte-
// slices and returns `error.InvalidFormat`, which `parseColor` turns into
// "stop and return the requests accumulated so far" (`catch return result`).
#[test]
fn osc_4_non_ascii_spec_stops_the_list_instead_of_panicking() {
    // The exact crashing payload from
    // `crates/rustty-vt/fuzz/findings/osc4-rgb-parse-char-boundary`
    // (fuzz input `\x00` + `4;1;aebcd` with a real 'e-acute'): 'aébcd' is
    // valid UTF-8, 6 BYTES, so `parse` takes its 6-byte arm and cuts
    // `input[2..4]` inside 'é'.
    let list = parse_color(Operation::Osc4, "1;aébcd".as_bytes()).unwrap();
    assert!(list.is_empty());

    // Raw invalid UTF-8 in the spec is equally just a failed parse.
    let list = parse_color(Operation::Osc4, b"1;U\x8evv").unwrap();
    assert!(list.is_empty());

    // The bail is "return what we have", not "drop everything": a good
    // pair before the bad one survives.
    let list = parse_color(Operation::Osc4, "0;red;1;aébcd".as_bytes()).unwrap();
    assert_eq!(1, list.len());
    assert_eq!(
        Request::Set(ColoredTarget {
            target: Target::Palette(0),
            color: Rgb { r: 255, g: 0, b: 0 },
        }),
        list[0],
    );
}

// Same regression through the DYNAMIC color path (`OSC 10/11/12/...`),
// which has its own `RGB.parse` call site.
#[test]
fn osc_10_non_ascii_spec_stops_the_list_instead_of_panicking() {
    let list = parse_color(Operation::Osc10, "aébcd".as_bytes()).unwrap();
    assert!(list.is_empty());

    let list = parse_color(Operation::Osc11, b"U\x8evv").unwrap();
    assert!(list.is_empty());

    // A query for the foreground, then a bad background spec: the query
    // survives, the bad spec ends the list.
    let list = parse_color(Operation::Osc10, "?;aébcd".as_bytes()).unwrap();
    assert_eq!(1, list.len());
    assert_eq!(Request::Query(Target::Dynamic(DynamicColor::Foreground)), list[0],);
}

// The same input driven through the REAL `osc::Parser` byte loop, i.e. the
// exact path `fuzz_harness::fuzz_osc` (and therefore the live fuzzer) takes.
#[test]
fn osc_parser_non_ascii_color_spec_is_not_a_crash() {
    for input in [
        "4;1;aébcd".as_bytes(),
        "10;aébcd".as_bytes(),
        "11;#aébcd".as_bytes(),
        "104;aébcd".as_bytes(),
        b"4;1;U\x8evv",
    ] {
        for terminator in [Some(b'\x07'), Some(b'\x9c'), None] {
            let mut p = crate::terminal::osc::Parser::new(false);
            for &ch in input {
                p.next(ch);
            }
            // The assertion is that this returns at all: pre-fix it aborted
            // the VT thread with "byte index N is not a char boundary".
            let _ = p.end(terminator);
        }
    }
}

fn operation_from_reset_value(value: u32) -> Operation {
    match value {
        10 => Operation::Osc110,
        11 => Operation::Osc111,
        12 => Operation::Osc112,
        13 => Operation::Osc113,
        14 => Operation::Osc114,
        15 => Operation::Osc115,
        16 => Operation::Osc116,
        17 => Operation::Osc117,
        18 => Operation::Osc118,
        19 => Operation::Osc119,
        _ => unreachable!(),
    }
}
