use crate::mem::testing::{with_counting, with_failing};
use crate::terminal::osc::{CaptureBacking, Command, MAX_BUF, Parser, State, Terminator, Unknown};

/// Every `Parser.State`, in declaration order (Ghostty iterates
/// `@typeInfo(Parser.State).@"enum".fields`).
const ALL_STATES: &[State] = &[
    State::Start,
    State::Invalid,
    State::Unknown,
    State::UnknownTruncated,
    State::S0,
    State::S1,
    State::S2,
    State::S3,
    State::S4,
    State::S5,
    State::S6,
    State::S7,
    State::S8,
    State::S9,
    State::S30,
    State::S300,
    State::S3008,
    State::S10,
    State::S11,
    State::S12,
    State::S13,
    State::S14,
    State::S15,
    State::S16,
    State::S17,
    State::S18,
    State::S19,
    State::S21,
    State::S22,
    State::S52,
    State::S55,
    State::S66,
    State::S72,
    State::S75,
    State::S750,
    State::S7501,
    State::S77,
    State::S99,
    State::S104,
    State::S105,
    State::S110,
    State::S111,
    State::S112,
    State::S113,
    State::S114,
    State::S115,
    State::S116,
    State::S117,
    State::S118,
    State::S119,
    State::S133,
    State::S552,
    State::S777,
    State::S1337,
    State::S5522,
];

fn unknown(cmd: Command<'_>) -> Unknown<'_> {
    match cmd {
        Command::Unknown(v) => v,
        other => panic!("expected unknown, got {other:?}"),
    }
}

fn is_fixed(p: &Parser) -> bool {
    matches!(p.capture.as_ref().unwrap().backing, CaptureBacking::Fixed(_))
}

fn is_allocating(p: &Parser) -> bool {
    matches!(p.capture.as_ref().unwrap().backing, CaptureBacking::Allocating(_))
}

/// `p.capture.?.writer.buffer.len`: the allocating writer's backing
/// buffer length, which is the Vec's capacity here.
fn allocating_capacity(p: &Parser) -> usize {
    match &p.capture.as_ref().unwrap().backing {
        CaptureBacking::Allocating(w) => w.capacity(),
        CaptureBacking::Fixed(_) => panic!("expected an allocating capture"),
    }
}

fn trailing_len(p: &Parser) -> usize {
    p.capture.as_ref().unwrap().trailing(&p.buffer).len()
}

#[test]
fn osc_unknown_prefix_state_names_are_the_bytes_consumed() {
    // beginUnknown recovers the consumed identifier from the prefix state's
    // name, so every prefix state must be reachable by feeding its name.
    for &state in ALL_STATES {
        match state {
            State::Start | State::Invalid | State::Unknown | State::UnknownTruncated => {}
            _ => {
                let mut p = Parser::new(false);
                for &ch in state.prefix() {
                    p.next(ch);
                }
                assert_eq!(state, p.state, "prefix {:?}", state.prefix());
            }
        }
    }

    let mut frontier: Vec<Vec<u8>> = vec![Vec::new()];
    let mut reached = 0usize;
    while let Some(path) = frontier.pop() {
        for d in b'0'..=b'9' {
            let mut next = path.clone();
            next.push(d);
            let mut p = Parser::new(false);
            for &ch in &next {
                p.next(ch);
            }
            if p.state != State::Invalid {
                assert!(p.capture.is_none());
                assert!(ALL_STATES.contains(&p.state), "{:?} is not listed", p.state);
                assert_eq!(next, p.state.prefix());
                reached += 1;
                frontier.push(next);
            }
        }
    }
    // start, invalid, unknown and unknown_truncated are not prefixes.
    assert_eq!(ALL_STATES.len() - 4, reached);
}

#[test]
fn osc_unknown_disabled_by_default() {
    let mut p = Parser::new(true);

    assert_eq!(0usize, p.unknown_max_bytes);
    p.next_slice(b"7400;status=busy");
    assert_eq!(State::Invalid, p.state);
    assert!(p.capture.is_none());
    assert!(p.end(Some(0x07)).is_none());

    // A sequence ending on a partial number is dropped too.
    p.reset();
    p.next_slice(b"77");
    assert!(p.end(Some(0x07)).is_none());
}

#[test]
fn osc_unknown_reported() {
    struct Case {
        input: &'static [u8],
        terminator_ch: Option<u8>,
        terminator: Terminator,
    }
    // terminator_ch: ?u8 = 0x1B, terminator: osc.Terminator = .st,
    const fn c(input: &'static [u8]) -> Case {
        Case {
            input,
            terminator_ch: Some(0x1B),
            terminator: Terminator::St,
        }
    }
    let cases = [
        // Unsupported number with a body.
        c(b"7400;status=busy"),
        // Unsupported number with no body, BEL terminated.
        Case {
            input: b"7400",
            terminator_ch: Some(0x07),
            terminator: Terminator::Bel,
        },
        // Bridge states that only lead to longer identifiers.
        c(b"3;x"),
        c(b"30;x"),
        c(b"300;x"),
        c(b"55;x"),
        c(b"552;x"),
        c(b"6;x"),
        c(b"77;x"),
        // Bridge states with no body end on the prefix itself.
        c(b"3"),
        c(b"300"),
        Case {
            input: b"77",
            terminator_ch: Some(0x07),
            terminator: Terminator::Bel,
        },
        // Known number extended by a digit.
        c(b"1338;x"),
        c(b"1339"),
        // Leading zero is not a known number.
        c(b"0133;x"),
        // Non-numeric identifier.
        c(b"I;name"),
        // Known number followed by junk before ';'.
        c(b"133 x"),
        // Empty identifier.
        c(b";x"),
        // A null terminator is treated as ST.
        Case {
            input: b"7400;x",
            terminator_ch: None,
            terminator: Terminator::St,
        },
    ];

    for case in &cases {
        let mut p = Parser::new(true);
        p.unknown_max_bytes = 64;

        p.next_slice(case.input);
        let cmd = unknown(p.end(case.terminator_ch).unwrap());
        assert_eq!(case.input, cmd.content);
        assert!(!cmd.truncated);
        assert_eq!(case.terminator, cmd.terminator);
    }
}

#[test]
fn osc_unknown_supported_commands_are_unchanged() {
    let mut p = Parser::new(true);
    p.unknown_max_bytes = 64;

    // A supported command still parses as itself.
    p.next_slice(b"0;title");
    match p.end(Some(0x07)).unwrap() {
        Command::ChangeWindowTitle(title) => assert_eq!(b"title", title),
        other => panic!("expected change_window_title, got {other:?}"),
    }

    // A known number with a malformed body is not reported as unknown.
    p.reset();
    p.next_slice(b"9;4;9");
    if let Some(c) = p.end(Some(0x07)) {
        assert!(!matches!(c, Command::Unknown(_)));
    }

    // A known number with an unsupported sub-command is not reported.
    p.reset();
    p.next_slice(b"1337;SetUserVar=foo=YmFy");
    if let Some(c) = p.end(Some(0x07)) {
        assert!(!matches!(c, Command::Unknown(_)));
    }

    // A known number without a body is not reported.
    p.reset();
    p.next_slice(b"7");
    if let Some(c) = p.end(Some(0x07)) {
        assert!(!matches!(c, Command::Unknown(_)));
    }

    // An empty OSC has no identifier and is not reported.
    p.reset();
    assert!(p.end(Some(0x07)).is_none());
}

#[test]
fn osc_unknown_aborted_sequences_are_dropped() {
    const CAN: u8 = 0x18;
    const SUB: u8 = 0x1A;

    let mut p = Parser::new(true);
    p.unknown_max_bytes = 64;

    p.next_slice(b"7400;x");
    assert!(p.end(Some(CAN)).is_none());

    p.reset();
    p.next_slice(b"7400;x");
    assert!(p.end(Some(SUB)).is_none());

    // Bridge prefixes are dropped on abort too.
    p.reset();
    p.next_slice(b"77");
    assert!(p.end(Some(CAN)).is_none());
}

#[test]
fn osc_unknown_truncated_at_the_limit() {
    let mut p = Parser::new(true);
    p.unknown_max_bytes = 8;

    // Exactly at the limit is not truncated.
    p.next_slice(b"7400;abc");
    let cmd = unknown(p.end(Some(0x07)).unwrap());
    assert_eq!(b"7400;abc", cmd.content);
    assert!(!cmd.truncated);

    // One byte over is truncated but still reported.
    p.reset();
    p.next_slice(b"7400;abcd");
    let cmd = unknown(p.end(Some(0x07)).unwrap());
    assert_eq!(b"7400;abc", cmd.content);
    assert!(cmd.truncated);

    // Later input is discarded, both by slice and by byte.
    p.reset();
    p.next_slice(b"7400;abcdef");
    p.next_slice(b"ghi");
    p.next(b'j');
    let cmd = unknown(p.end(Some(0x07)).unwrap());
    assert_eq!(b"7400;abc", cmd.content);
    assert!(cmd.truncated);
}

#[test]
fn osc_unknown_limit_smaller_than_the_identifier() {
    let mut p = Parser::new(true);
    p.unknown_max_bytes = 2;

    // The prefix itself overflows while beginning the capture.
    p.next_slice(b"7400;x");
    let cmd = unknown(p.end(Some(0x07)).unwrap());
    assert_eq!(b"74", cmd.content);
    assert!(cmd.truncated);

    // The prefix fits but the unrecognized byte does not.
    p.reset();
    p.next_slice(b"77x");
    let cmd = unknown(p.end(Some(0x07)).unwrap());
    assert_eq!(b"77", cmd.content);
    assert!(cmd.truncated);

    // A bridge prefix longer than the limit at end.
    p.reset();
    p.unknown_max_bytes = 1;
    p.next_slice(b"77");
    let cmd = unknown(p.end(Some(0x07)).unwrap());
    assert_eq!(b"7", cmd.content);
    assert!(cmd.truncated);
}

#[test]
fn osc_unknown_fixed_tier_does_not_allocate() {
    let ((), failing) = with_failing(0, || {
        let mut p = Parser::new(true);
        p.unknown_max_bytes = MAX_BUF;

        p.next_slice(b"7400;");
        assert!(is_fixed(&p));
        assert_eq!(MAX_BUF, p.capture.as_ref().unwrap().max_bytes);
        p.next_slice(b"x");
        let cmd = unknown(p.end(Some(0x07)).unwrap());
        assert_eq!(b"7400;x", cmd.content);
    });
    assert!(!failing.has_induced_failure);
}

#[test]
fn osc_unknown_allocating_tier() {
    let limit = MAX_BUF * 2;

    let ((), counting) = with_counting(|| {
        let mut p = Parser::new(true);
        p.unknown_max_bytes = limit;

        p.next_slice(b"7400;");
        assert!(is_allocating(&p));

        let body = vec![b'a'; limit];

        // Fill to exactly the limit, including the 5 identifier bytes.
        p.next_slice(&body[..limit - 5]);
        assert_eq!(State::Unknown, p.state);
        assert_eq!(limit, allocating_capacity(&p));

        // One more byte truncates without growing the allocation.
        p.next_slice(b"a");
        assert_eq!(State::UnknownTruncated, p.state);
        assert_eq!(limit, allocating_capacity(&p));

        let cmd = unknown(p.end(Some(0x07)).unwrap());
        assert!(cmd.truncated);
        assert_eq!(limit, cmd.content.len());
        assert!(cmd.content.starts_with(b"7400;aaa"));
    });
    assert!(counting.alloc_calls > 0);
    assert_eq!(0, counting.live_bytes);
}

#[test]
fn osc_unknown_large_limit_without_allocator_uses_fixed_buffer() {
    let mut p = Parser::new(false);
    p.unknown_max_bytes = MAX_BUF * 2;

    p.next_slice(b"7400;");
    assert!(is_fixed(&p));

    let body = [b'a'; MAX_BUF];
    p.next_slice(&body);

    let cmd = unknown(p.end(Some(0x07)).unwrap());
    assert!(cmd.truncated);
    assert_eq!(MAX_BUF, cmd.content.len());
}

#[test]
fn osc_unknown_allocation_failure_falls_back_to_fixed_buffer() {
    let ((), failing) = with_failing(0, || {
        let mut p = Parser::new(true);
        p.unknown_max_bytes = MAX_BUF * 2;

        p.next_slice(b"7400;x");
        assert!(is_fixed(&p));
        let cmd = unknown(p.end(Some(0x07)).unwrap());
        assert_eq!(b"7400;x", cmd.content);
    });
    assert!(failing.has_induced_failure);
}

#[test]
fn osc_unknown_allocation_failure_while_growing_truncates() {
    let ((), counting) = with_counting(|| {
        let mut p = Parser::new(true);
        p.unknown_max_bytes = MAX_BUF * 2;

        // Allow the initial capacity allocation and fail the first growth.
        let ((), failing) = with_failing(1, || {
            p.next_slice(b"7400;");
            assert!(is_allocating(&p));

            let body = [b'a'; MAX_BUF];
            p.next_slice(&body);
        });
        assert!(failing.has_induced_failure);
        assert_eq!(State::UnknownTruncated, p.state);
        let retained = trailing_len(&p);
        assert!(retained <= MAX_BUF);

        // Later bytes must not be appended after the gap, even though an allocation would now succeed.
        p.next_slice(b"zz");
        p.next(b'z');

        let cmd = unknown(p.end(Some(0x07)).unwrap());
        assert!(cmd.truncated);
        assert_eq!(retained, cmd.content.len());
        assert!(!cmd.content.contains(&b'z'));
    });
    assert_eq!(0, counting.live_bytes);
}

#[test]
fn osc_unknown_next_slice_matches_per_byte_parsing() {
    let inputs: [&[u8]; 5] = [
        b"7400;status=busy:jobs=4",
        b"I;name",
        b"1338;x",
        b"77",
        b"7400;abcdefghijklmnop",
    ];

    for input in inputs {
        // Per-byte reference.
        let mut r = Parser::new(true);
        r.unknown_max_bytes = 12;
        for &ch in input {
            r.next(ch);
        }
        let expected = unknown(r.end(Some(0x07)).unwrap());
        let (expected_content, expected_truncated) = (expected.content.to_vec(), expected.truncated);

        // Every two-way split of the input must parse identically.
        for split in 0..=input.len() {
            let mut p = Parser::new(true);
            p.unknown_max_bytes = 12;
            p.next_slice(&input[..split]);
            p.next_slice(&input[split..]);

            let cmd = p.end(Some(0x07)).unwrap();
            assert!(matches!(cmd, Command::Unknown(_)));
            let cmd = unknown(cmd);
            assert_eq!(expected_content, cmd.content);
            assert_eq!(expected_truncated, cmd.truncated);
        }
    }
}

// No Ghostty test: a huge unknown OSC. A 1 MiB payload fed
// in 4 KiB slices with capture at 4 KiB (the allocating tier) truncates
// at the limit, never grows the capture past it, and leaves the parser
// ready for the next OSC. With capture disabled it costs nothing.
#[test]
fn osc_unknown_huge_sequence_is_bounded_at_the_limit() {
    const LIMIT: usize = 4096;
    const SLICE: usize = 4096;
    const TOTAL: usize = 1024 * 1024;

    // Feed `prefix` + 1 MiB of 'A' + BEL, then `0;title` + BEL. Returns
    // the unknown command's (content length, truncated, all 'A' after
    // the prefix), or None if the sequence was not reported.
    fn run(p: &mut Parser, prefix: &[u8], slice: &[u8], check: &dyn Fn(&Parser)) -> Option<(usize, bool, bool)> {
        p.next_slice(prefix);
        for _ in 0..TOTAL / SLICE {
            p.next_slice(slice);
            check(p);
        }
        let got = match p.end(Some(0x07)) {
            Some(Command::Unknown(u)) => Some((
                u.content.len(),
                u.truncated,
                u.content[prefix.len()..].iter().all(|&b| b == b'A'),
            )),
            Some(other) => panic!("expected unknown or nothing, got {other:?}"),
            None => None,
        };

        // The next supported OSC parses normally.
        p.reset();
        p.next_slice(b"0;title");
        match p.end(Some(0x07)) {
            Some(Command::ChangeWindowTitle(t)) => assert_eq!(b"title", t),
            other => panic!("expected change_window_title, got {other:?}"),
        }
        p.reset();
        got
    }

    let slice = [b'A'; SLICE];
    let prefix: &[u8] = b"7400;File=";
    let ((), counting) = with_counting(|| {
        let mut p = Parser::new(true);
        p.unknown_max_bytes = LIMIT;
        let got = run(&mut p, prefix, &slice, &|p: &Parser| {
            if let Some(cap) = p.capture.as_ref()
                && let CaptureBacking::Allocating(w) = &cap.backing
            {
                assert!(w.capacity() <= LIMIT, "capacity {}", w.capacity());
            }
        });
        // content holds the prefix and then `LIMIT - prefix.len()` bytes
        // of the body.
        assert_eq!(Some((LIMIT, true, true)), got);
    });
    assert!(
        counting.peak_bytes <= isize::try_from(LIMIT).unwrap(),
        "{}",
        counting.peak_bytes
    );
    assert_eq!(0, counting.live_bytes);

    // Capture disabled: the same input allocates nothing and reports
    // nothing.
    let ((), counting) = with_counting(|| {
        let mut p = Parser::new(true);
        assert_eq!(
            None,
            run(&mut p, prefix, &slice, &|p: &Parser| {
                assert_eq!(State::Invalid, p.state);
            })
        );
    });
    assert_eq!(0, counting.alloc_calls);

    // OSC 1337 is supported: with capture enabled its 1 MiB body still
    // overflows the fixed buffer, is dropped, and allocates nothing.
    let ((), counting) = with_counting(|| {
        let mut p = Parser::new(true);
        p.unknown_max_bytes = LIMIT;
        assert_eq!(None, run(&mut p, b"1337;File=", &slice, &|_: &Parser| {}));
    });
    assert_eq!(0, counting.alloc_calls);
}
