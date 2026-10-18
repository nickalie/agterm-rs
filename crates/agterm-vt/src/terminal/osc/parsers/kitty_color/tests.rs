use crate::terminal::color::Rgb;
use crate::terminal::kitty::color::{Kind, Request, Special};
use crate::terminal::osc::{Command, Parser};

// REGRESSION (fuzz finding, `cargo fuzz run fuzz_osc`, 2026-08-11): the
// kitty color value used to be decoded with `String::from_utf8_lossy`
// before `RGB::parse`. That is NOT what Ghostty does — it hands the
// raw `[]const u8` to `RGB.parse` (ghostty src/terminal/color.zig:657) —
// and it was doubly wrong: each invalid byte became a 3-byte U+FFFD, which
// both changed the byte length `parse` switches on AND planted a multi-byte
// scalar for `parse`'s byte indices to land inside, i.e. a PANIC. An
// unparseable value must simply be skipped (`continue`), leaving the other
// requests in the list.
#[test]
fn osc_21_invalid_utf8_value_is_skipped_not_a_crash() {
    let mut p: Parser = Parser::new(true);

    // The exact minimized fuzz input from
    // `crates/rustty-vt/fuzz/findings/osc21-kitty-color-lossy-char-boundary`
    // is `01 32 31 3b 31 3d 55 8e 76 76` = selector 1 (ST) + `21;1=U\x8evv`.
    // `U\x8evv` is 4 raw bytes; lossy-decoding widened it to 6 and selected
    // `parse`'s 6-byte arm, whose `input[1..3]` cut split the U+FFFD.
    let input: &[u8] = b"21;1=U\x8evv";
    for &ch in input {
        p.next(ch);
    }

    let cmd = p.end(Some(0x9c)).unwrap();
    let Command::KittyColorProtocol(osc) = cmd else {
        panic!("expected a kitty color protocol command")
    };
    // The one and only request had an unparseable value, so it is dropped.
    assert_eq!(0usize, osc.list.len());
}

// Valid UTF-8 with a multi-byte scalar reaches the same byte indices; the
// lossy decode was not required for the crash, only for the second route.
#[test]
fn osc_21_non_ascii_value_is_skipped_not_a_crash() {
    let mut p: Parser = Parser::new(true);

    // `1=aébcd` is 6 BYTES of value, hitting the 6-byte arm whose
    // `input[2..4]` cut lands inside 'é'; `2=?` after it must still land.
    let input = "21;1=aébcd;2=?";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    let Command::KittyColorProtocol(osc) = cmd else {
        panic!("expected a kitty color protocol command")
    };
    assert_eq!(1usize, osc.list.len());
    assert_eq!(Request::Query(Kind::Palette(2)), osc.list[0]);
}

#[test]
fn osc_21_kitty_color_protocol() {
    let mut p: Parser = Parser::new(true);

    let input = "21;foreground=?;background=rgb:f0/f8/ff;cursor=aliceblue;cursor_text;visual_bell=;selection_foreground=#xxxyyzz;selection_background=?;selection_background=#aabbcc;2=?;3=rgbi:1.0/1.0/1.0";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    let Command::KittyColorProtocol(osc) = cmd else {
        unreachable!()
    };
    let set = |key, r, g, b| Request::Set {
        key,
        color: Rgb { r, g, b },
    };
    let want = vec![
        Request::Query(Kind::Special(Special::Foreground)),
        set(Kind::Special(Special::Background), 0xf0, 0xf8, 0xff),
        set(Kind::Special(Special::Cursor), 0xf0, 0xf8, 0xff),
        Request::Reset(Kind::Special(Special::CursorText)),
        Request::Reset(Kind::Special(Special::VisualBell)),
        // selection_foreground's value does not parse: it is skipped.
        Request::Query(Kind::Special(Special::SelectionBackground)),
        set(Kind::Special(Special::SelectionBackground), 0xaa, 0xbb, 0xcc),
        Request::Query(Kind::Palette(2)),
        set(Kind::Palette(3), 0xff, 0xff, 0xff),
    ];
    assert_eq!(want, osc.list);
}

#[test]
fn osc_21_kitty_color_protocol_without_allocator() {
    let mut p: Parser = Parser::new(false);

    let input = "21;foreground=?";
    for ch in input.bytes() {
        p.next(ch);
    }
    assert!(p.end(Some(0x1b)).is_none());
}

#[test]
fn osc_21_kitty_color_protocol_double_reset() {
    let mut p: Parser = Parser::new(true);

    let input = "21;foreground=?;background=rgb:f0/f8/ff;cursor=aliceblue;cursor_text;visual_bell=;selection_foreground=#xxxyyzz;selection_background=?;selection_background=#aabbcc;2=?;3=rgbi:1.0/1.0/1.0";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::KittyColorProtocol(_)));

    p.reset();
    p.reset();
}

#[test]
fn osc_21_kitty_color_protocol_reset_after_invalid() {
    let mut p: Parser = Parser::new(true);

    let input = "21;foreground=?;background=rgb:f0/f8/ff;cursor=aliceblue;cursor_text;visual_bell=;selection_foreground=#xxxyyzz;selection_background=?;selection_background=#aabbcc;2=?;3=rgbi:1.0/1.0/1.0";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::KittyColorProtocol(_)));

    p.reset();

    assert_eq!(crate::terminal::osc::State::Start, p.state);
    p.next(b'X');
    assert_eq!(crate::terminal::osc::State::Invalid, p.state);

    p.reset();
}

#[test]
fn osc_21_kitty_color_protocol_no_key() {
    let mut p: Parser = Parser::new(true);

    let input = "21;";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::KittyColorProtocol(_)));
    match cmd {
        Command::KittyColorProtocol(osc) => assert_eq!(0, osc.list.len()),
        _ => unreachable!(),
    }
}

// No Ghostty test: the four live `log.warn` sites of
// `parse` (kitty_color.zig:35, :42, :46, :66) emit, with Ghostty's
// scope and text, on the input that reaches each.
fn capture(input: &[u8]) -> Vec<crate::log_capture::CapturedRecord> {
    let mut p: Parser = Parser::new(true);
    let cap = crate::log_capture::start();
    for &ch in input {
        p.next(ch);
    }
    let _ = p.end(Some(0x1b));
    let messages = cap.messages();
    drop(cap);
    messages
}

fn warn(msg: &str) -> crate::log_capture::CapturedRecord {
    (tracing::Level::WARN, "osc_kitty_color".to_string(), msg.to_string())
}

#[test]
fn osc_21_logs_zero_length_unknown_key_and_invalid_color() {
    assert_eq!(
        vec![
            warn("zero length key in kitty color protocol"),
            warn("unknown key in kitty color protocol: nope"),
            warn("invalid color format in kitty color protocol: #xxxyyzz"),
        ],
        capture(b"21;=?;nope=?;selection_foreground=#xxxyyzz;2=?")
    );
}

#[test]
fn osc_21_logs_when_the_key_limit_is_exceeded() {
    let mut input = b"21".to_vec();
    for _ in 0..=(Kind::MAX * 2) {
        // `;2` (a reset) keeps the payload inside the 2048-byte capture.
        input.extend_from_slice(b";2");
    }
    assert_eq!(
        vec![warn(
            "exceeded limit for number of keys in kitty color protocol, ignoring"
        )],
        capture(&input)
    );
}
