use super::*;

fn input(bytes: &[u8]) -> Token {
    Token::Input(bytes.to_vec())
}

fn answer(answer: Answer) -> Token {
    Token::Answer(answer)
}

#[test]
fn keys_pass_and_answers_are_picked_out() {
    let mut parser = InputParser::new(true);
    let tokens = parser.feed(
        b"ls\x1b[A\x1b]11;rgb:1111/2222/3333\x1b\\\x1bOP\x1b[?62;22c\x1bx\x1b[?997;1n\x1b[6;20;10t\x1b[4;600;800t\r",
    );
    assert_eq!(
        tokens,
        [
            input(b"ls\x1b[A"),
            answer(Answer::Dynamic {
                which: Dynamic::Background,
                color: Some(Rgb {
                    r: 0x11,
                    g: 0x22,
                    b: 0x33
                })
            }),
            input(b"\x1bOP"),
            answer(Answer::PrimaryAttributes),
            input(b"\x1bx"),
            answer(Answer::ColorScheme(ColorScheme::Dark)),
            answer(Answer::CellSize { width: 10, height: 20 }),
            input(b"\r"),
        ]
    );
}

#[test]
fn other_answers_and_reports_are_dropped() {
    let mut parser = InputParser::new(true);
    let tokens = parser.feed(
        b"a\x1b[>1;10;0c\x1b[0n\x1b[?2026;2$y\x1b[?1u\x1b[48;24;80;480;800t\x1bP>|ghostty\x1b\\\x1b_Xother\x1b\\b",
    );
    assert_eq!(tokens, [input(b"ab")]);
}

#[test]
fn a_cpr_shaped_key_is_a_key() {
    let mut parser = InputParser::new(true);
    assert_eq!(parser.feed(b"\x1b[1;5R"), [input(b"\x1b[1;5R")]);
}

#[test]
fn palette_clipboard_and_kitty_answers_are_read() {
    let mut parser = InputParser::new(true);
    let tokens = parser.feed(b"\x1b]4;1;rgb:ff/00/00\x07\x1b]52;c;aGVsbG8=\x1b\\\x1b]52;c;!!\x07\x1b_Gi=31;OK\x1b\\");
    assert_eq!(
        tokens,
        [
            answer(Answer::Palette {
                index: 1,
                color: Some(Rgb { r: 0xff, g: 0, b: 0 })
            }),
            answer(Answer::Clipboard(Some(b"hello".to_vec()))),
            answer(Answer::Clipboard(None)),
            answer(Answer::Kitty(b"\x1b_Gi=31;OK\x1b\\".to_vec())),
        ]
    );
}

#[test]
fn an_answer_cut_across_reads_is_held_for_the_next() {
    let whole: &[u8] = b"x\x1b]10;rgb:ffff/0000/8080\x1b\\y";
    for cut in 1..whole.len() {
        let mut parser = InputParser::new(true);
        let mut tokens = parser.feed(&whole[..cut]);
        tokens.extend(parser.feed(&whole[cut..]));
        assert!(!parser.holding());
        let mut merged = Vec::new();
        for token in tokens {
            match token {
                Token::Input(bytes) => push_input(&mut merged, &bytes),
                other => merged.push(other),
            }
        }
        assert_eq!(
            merged,
            [
                input(b"x"),
                answer(Answer::Dynamic {
                    which: Dynamic::Foreground,
                    color: Some(Rgb { r: 0xff, g: 0, b: 0x80 })
                }),
                input(b"y"),
            ],
            "cut at {cut}"
        );
    }
}

#[test]
fn a_lone_escape_waits_for_the_quiet_then_is_a_key() {
    let mut parser = InputParser::new(true);
    assert!(parser.feed(b"\x1b").is_empty());
    assert!(parser.holding());
    assert_eq!(parser.flush(), [input(b"\x1b")]);
    assert!(!parser.holding());
}

#[test]
fn a_cut_sequence_is_taken_as_typed_once_flushed() {
    let mut parser = InputParser::new(true);
    assert_eq!(parser.feed(b"a\x1b]"), [input(b"a")]);
    assert_eq!(parser.held(), Some(Held::Key), "Alt+]");
    assert_eq!(parser.flush(), [input(b"\x1b]")]);
    assert_eq!(parser.feed(b"b"), [input(b"b")]);
    assert_eq!(parser.feed(b"\x1b[1;5"), []);
    assert_eq!(parser.held(), Some(Held::Csi));
    assert_eq!(parser.flush(), [input(b"\x1b[1;5")]);
}

#[test]
fn a_cut_answer_waits_longer_and_is_never_typed() {
    for cut in [
        b"\x1b]52;c;aGVs".as_slice(),
        b"\x1b]4;17;rgb:",
        b"\x1b[?62;2",
        b"\x1b_Gi=1;O",
        b"\x1bP>|gh",
    ] {
        let mut parser = InputParser::new(true);
        assert_eq!(parser.feed(cut), []);
        assert_eq!(parser.held(), Some(Held::Answer), "{:?}", String::from_utf8_lossy(cut));
        assert_eq!(parser.flush(), [], "{:?}", String::from_utf8_lossy(cut));
        assert!(!parser.holding());
    }
    // held until its end comes, the answer is read whole
    let mut parser = InputParser::new(true);
    assert_eq!(parser.feed(b"\x1b]52;c;aGVs"), []);
    assert_eq!(
        parser.feed(b"bG8=\x1b\\x"),
        [answer(Answer::Clipboard(Some(b"hello".to_vec()))), input(b"x")]
    );
}

#[test]
fn a_cut_paste_end_is_held_until_the_rest_comes() {
    let mut parser = InputParser::new(true);
    assert_eq!(parser.feed(b"\x1b[200~abc\x1b[20"), [input(b"\x1b[200~abc")]);
    assert!(parser.holding());
    assert_eq!(parser.held(), None, "no escape time inside a paste");
    assert_eq!(parser.flush(), []);
    assert_eq!(parser.feed(b"1~\x1c"), [input(b"\x1b[201~"), Token::Detach]);
}

#[test]
fn a_paste_passes_whole_with_what_it_holds() {
    let mut parser = InputParser::new(true);
    let paste: &[u8] = b"\x1b[200~\x1c\x1b]11;rgb:0/0/0\x07\x1b[?62c\x1b[201~";
    for cut in 1..paste.len() {
        let mut parser = InputParser::new(true);
        let mut got = Vec::new();
        for token in parser.feed(&paste[..cut]).into_iter().chain(parser.feed(&paste[cut..])) {
            let Token::Input(bytes) = token else {
                panic!("not input: {token:?} (cut at {cut})");
            };
            got.extend(bytes);
        }
        assert_eq!(got, paste, "cut at {cut}");
    }
    assert_eq!(parser.feed(paste), [input(paste)]);
    assert_eq!(parser.feed(b"\x1c"), [Token::Detach]);
}

#[test]
fn the_detach_key_detaches_unless_turned_off() {
    let mut parser = InputParser::new(true);
    assert_eq!(parser.feed(b"\x1cmore"), [Token::Detach]);
    assert_eq!(parser.feed(b"a\x1b[92;5u"), [input(b"a"), Token::Detach]);
    assert_eq!(parser.feed(b"\x1b[92;69:1u"), [Token::Detach]);
    assert_eq!(parser.feed(b"\x1b[27;5;92~"), [Token::Detach]);
    // a release and Ctrl+Shift+\ are no detach; Ctrl+\ inside a read is, after what came before it
    assert_eq!(parser.feed(b"\x1b[92;5:3u"), [input(b"\x1b[92;5:3u")]);
    assert_eq!(parser.feed(b"\x1b[92;6u"), [input(b"\x1b[92;6u")]);
    assert_eq!(parser.feed(b"ab\x1ccd"), [input(b"ab"), Token::Detach]);
    let mut off = InputParser::new(false);
    assert_eq!(off.feed(b"\x1c"), [input(b"\x1c")]);
    assert_eq!(off.feed(b"\x1b[92;5u"), [input(b"\x1b[92;5u")]);
}

#[test]
fn colours_parse_in_every_width() {
    let rgb = |r, g, b| Some(Rgb { r, g, b });
    assert_eq!(parse_color(b"rgb:ffff/8080/0000"), rgb(0xff, 0x80, 0));
    assert_eq!(parse_color(b"rgb:f/8/0"), rgb(0xff, 0x88, 0));
    assert_eq!(parse_color(b"rgb:12/34/56"), rgb(0x12, 0x34, 0x56));
    assert_eq!(parse_color(b"#123456"), rgb(0x12, 0x34, 0x56));
    assert_eq!(parse_color(b"#fff"), rgb(0xff, 0xff, 0xff));
    assert_eq!(parse_color(b"rgb:12/34"), None);
    assert_eq!(parse_color(b"red"), None);
    assert_eq!(parse_color(b"rgb:12345/0/0"), None);
}
