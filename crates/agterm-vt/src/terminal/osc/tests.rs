//! Ghostty's `osc.zig` tests of the parser and its captures, and the tests of what this port changes: the commands
//! borrow the parser's storage, the lists are the parser's, allocation failures take Zig's paths.

use super::{Capture, CaptureBacking, Command, MAX_BUF, Parser, State, Terminator, color, parsers};
use crate::log_capture;
use crate::mem::testing::{with_counting, with_failing};

fn feed(p: &mut Parser, input: &[u8]) {
    for &ch in input {
        p.next(ch);
    }
}

/// The allocating capture's backing buffer length (Zig's `cap.writer.buffer.len`): its capacity, which the bounded
/// growth keeps exact.
fn allocating_capacity(cap: &Capture) -> usize {
    match &cap.backing {
        CaptureBacking::Allocating(w) => w.capacity(),
        CaptureBacking::Fixed(_) => panic!("expected an allocating capture"),
    }
}

/// `ensureAllocator` warns, naming the state with `{t}`, before it invalidates the parser.
#[test]
fn ensure_allocator_warns_when_no_allocator_was_provided() {
    assert_eq!("start", State::Start.tag_name());
    assert_eq!("invalid", State::Invalid.tag_name());
    assert_eq!("unknown_truncated", State::UnknownTruncated.tag_name());
    assert_eq!("5522", State::S5522.tag_name());
    assert_eq!(b"133", State::S133.prefix());
    assert_eq!(b"", State::Start.prefix());

    let mut p = Parser::new(false);
    let cap = log_capture::start();
    feed(&mut p, b"10;");
    let messages = cap.messages();
    drop(cap);
    assert_eq!(State::Invalid, p.state);
    assert_eq!(
        vec![(
            tracing::Level::WARN,
            "osc".to_owned(),
            "An allocator is required to process OSC 10 but none was provided.".to_owned(),
        )],
        messages
    );
}

#[test]
fn parser_end_with_can_or_sub_cancels_the_command() {
    let mut p = Parser::new(true);

    // A fixed capture (OSC 2) cancelled by CAN.
    feed(&mut p, b"2;title");
    assert!(p.end(Some(0x18)).is_none());

    // An allocating capture (OSC 52) cancelled by SUB.
    p.reset();
    feed(&mut p, b"52;c;Zm9v");
    assert!(p.end(Some(0x1A)).is_none());

    // The parser still works normally after a cancel.
    p.reset();
    feed(&mut p, b"2;title");
    assert_eq!(Some(Command::ChangeWindowTitle(b"title")), p.end(Some(0x07)));
}

#[test]
fn parser_allocating_captures_have_a_hard_limit() {
    let limit = MAX_BUF + 1;
    for prefix in [&b"52;"[..], b"66;", b"72;", b"99;", b"5522;"] {
        let mut p = Parser::new(true);
        p.max_allocating_bytes = limit;

        feed(&mut p, prefix);
        for _ in 0..limit {
            p.next(b'a');
        }
        let cap = p.capture.as_ref().unwrap();
        assert_eq!(limit, cap.trailing(&p.buffer).len());
        assert_eq!(limit, allocating_capacity(cap));

        p.next(b'a');
        assert_eq!(State::Invalid, p.state);
        let cap = p.capture.as_ref().unwrap();
        assert_eq!(limit, cap.trailing(&p.buffer).len());
        assert_eq!(limit, allocating_capacity(cap));
    }
}

#[test]
fn parser_next_slice_allocating_captures_have_a_hard_limit() {
    let limit = MAX_BUF + 1;
    let mut p = Parser::new(true);
    p.max_allocating_bytes = limit;
    let data = vec![b'a'; limit];

    // Exactly at the limit stays valid and bounded.
    p.next_slice(b"52;");
    p.next_slice(&data);
    let cap = p.capture.as_ref().unwrap();
    assert_ne!(p.state, State::Invalid);
    assert_eq!(limit, cap.trailing(&p.buffer).len());
    assert_eq!(limit, allocating_capacity(cap));

    // One more byte overflows: the state becomes invalid and the retained bytes and allocation stay bounded.
    p.next_slice(b"a");
    assert_eq!(State::Invalid, p.state);
    let cap = p.capture.as_ref().unwrap();
    assert_eq!(limit, cap.trailing(&p.buffer).len());
    assert_eq!(limit, allocating_capacity(cap));
    assert!(p.end(None).is_none());
}

#[test]
fn parser_next_slice_overflowing_slice_is_truncated_at_the_limit() {
    let mut p = Parser::new(true);
    p.max_allocating_bytes = 4;

    p.next_slice(b"52;abcdef");
    assert_eq!(State::Invalid, p.state);
    assert!(p.end(None).is_none());

    let cap = p.capture.as_ref().unwrap();
    assert_eq!(b"abcd", cap.trailing(&p.buffer));
    assert_eq!(4, allocating_capacity(cap));
}

/// The allocating arm of `Capture::write_slice`: an append that fits in the spare capacity, an append that grows the
/// buffer (doubling, then clamped to `max_bytes`), and an append truncated at `max_bytes`. Distinct bytes per
/// position catch a wrong offset or a stale length.
#[test]
fn capture_write_slice_allocating_arm_appends_exact_bytes() {
    let mut buffer = [0_u8; MAX_BUF];
    let mut w = Vec::new();
    w.try_reserve_exact(4).unwrap();
    let mut cap = Capture {
        backing: CaptureBacking::Allocating(w),
        max_bytes: 10,
    };
    let initial = allocating_capacity(&cap);
    assert!(initial >= 4);

    // Fits in the reserved capacity: no growth.
    assert!(cap.write_slice(&mut buffer, b"ab"));
    assert_eq!(b"ab", cap.trailing(&buffer));
    assert_eq!(initial, allocating_capacity(&cap));

    // Needs growth: needed = 2 + 5 = 7.
    assert!(cap.write_slice(&mut buffer, b"cdefg"));
    assert_eq!(b"abcdefg", cap.trailing(&buffer));
    assert!((7..=10).contains(&allocating_capacity(&cap)));

    // Truncated at max_bytes: the first 3 bytes land, the write fails.
    assert!(!cap.write_slice(&mut buffer, b"hijkl"));
    assert_eq!(b"abcdefghij", cap.trailing(&buffer));
    assert_eq!(10, allocating_capacity(&cap));

    // Full: nothing more lands.
    assert!(!cap.write_slice(&mut buffer, b"z"));
    assert_eq!(b"abcdefghij", cap.trailing(&buffer));
    // The fixed buffer was never touched by the allocating arm.
    assert!(buffer.iter().all(|&b| b == 0));
}

/// An empty append to the allocating arm is a no-op.
#[test]
fn capture_write_slice_allocating_arm_ignores_an_empty_append() {
    let mut buffer = [0_u8; MAX_BUF];
    let mut cap = Capture {
        backing: CaptureBacking::Allocating(Vec::new()),
        max_bytes: 10,
    };
    assert!(cap.write_slice(&mut buffer, b"ab"));
    let capacity = allocating_capacity(&cap);
    assert!(cap.write_slice(&mut buffer, b""));
    assert_eq!(b"ab", cap.trailing(&buffer));
    assert_eq!(capacity, allocating_capacity(&cap));
}

/// Every two-way split of the input parses identically to the byte-at-a-time path.
#[test]
fn parser_next_slice_matches_per_byte_parsing() {
    let input = b"52;c;aGVsbG8=";
    for split in 0..=input.len() {
        let mut p = Parser::new(true);
        p.next_slice(&input[..split]);
        p.next_slice(&input[split..]);
        let Some(Command::ClipboardContents { kind, data, .. }) = p.end(None) else {
            panic!("expected clipboard_contents at split {split}");
        };
        assert_eq!(b'c', kind);
        assert_eq!(b"aGVsbG8=", data);
    }
}

#[test]
fn parser_allocating_capture_limit_includes_parser_added_bytes() {
    let mut p = Parser::new(true);
    p.max_allocating_bytes = 4;

    feed(&mut p, b"52;abcd");
    assert!(p.end(None).is_none());
    assert_eq!(State::Invalid, p.state);

    let cap = p.capture.as_ref().unwrap();
    assert_eq!(4, cap.trailing(&p.buffer).len());
    assert_eq!(4, allocating_capacity(cap));
}

/// The parser keeps a color operation's list (Zig's `parser.command` owns it) until `reset`, which frees it.
#[test]
fn color_requests_are_the_parsers_until_reset() {
    let ((), counts) = with_counting(|| {
        let mut p = Parser::new(true);
        feed(&mut p, b"4;1;?;2;?;3;?");
        let Some(Command::ColorOperation { requests, .. }) = p.end(Some(0x07)) else {
            panic!("expected color_operation");
        };
        assert_eq!(3, requests.len());
        assert_eq!(color::Request::Query(color::Target::Palette(3)), requests[2]);
        assert!(p.color_requests.is_some(), "the parser keeps the list");
        p.reset();
        assert!(p.color_requests.is_none(), "`reset` frees it");
    });
    assert_eq!(0, counts.live_bytes);
}

/// The parser keeps a Kitty color protocol command's list until `reset`.
#[test]
fn kitty_color_list_is_the_parsers_until_reset() {
    let mut p = Parser::new(true);
    feed(&mut p, b"21;foreground=?;background=#ffffff;cursor=");
    let Some(Command::KittyColorProtocol(osc)) = p.end(Some(0x1b)) else {
        panic!("expected kitty_color_protocol");
    };
    assert_eq!(3, osc.list.len());
    assert!(p.kitty_color.is_some());
    p.reset();
    assert!(p.kitty_color.is_none());
}

/// A refused list allocation takes Zig's `catch`: the command is still produced, with an empty list, and `log.info`
/// names the OSC, the error and the data.
#[test]
fn color_request_allocation_failure_logs_and_yields_an_empty_list() {
    let mut p = Parser::new(true);
    feed(&mut p, b"4;1;?;2;?;3;?");
    let cap = log_capture::start();
    let (len, stats) = with_failing(0, || match p.end(Some(0x07)) {
        Some(Command::ColorOperation { op, requests, .. }) => Some((op, requests.len())),
        _ => None,
    });
    let messages = cap.messages();
    drop(cap);
    assert_eq!(Some((color::Operation::Osc4, 0)), len);
    assert!(stats.has_induced_failure);
    assert!(
        messages
            .iter()
            .any(|(level, target, msg)| *level == tracing::Level::INFO
                && target == "osc_color"
                && msg == "failed to parse OSC 4 color request err=error.OutOfMemory data=1;?;2;?;3;?"),
        "expected the parseColor catch diagnostic, got: {messages:?}"
    );
}

/// A refused Kitty list append logs Zig's warning and skips that key: the loop reaches the next key, whose append is
/// refused as well (the failing allocator's refusal is sticky, as Zig's is), so both keys log and the command is still
/// produced.
#[test]
fn kitty_color_append_failure_logs_and_continues() {
    let mut p = Parser::new(true);
    feed(&mut p, b"21;foreground=?;background=?");
    let cap = log_capture::start();
    let (len, _) = with_failing(0, || match p.end(Some(0x1b)) {
        Some(Command::KittyColorProtocol(osc)) => Some(osc.list.len()),
        _ => None,
    });
    let messages = cap.messages();
    drop(cap);
    assert_eq!(Some(0), len);
    let warnings = messages
        .iter()
        .filter(|(level, target, msg)| {
            *level == tracing::Level::WARN
                && target == "osc_kitty_color"
                && msg == "unable to append kitty color protocol option: error.OutOfMemory"
        })
        .count();
    assert_eq!(2, warnings, "got: {messages:?}");
}

/// Text payloads are borrowed from the parser's storage, not copied: a title from the fixed capture points into
/// `Parser::buffer`, and parsing it allocates nothing.
#[test]
fn fixed_capture_payloads_borrow_the_parser_buffer() {
    let ((), counts) = with_counting(|| {
        let mut p = Parser::new(true);
        let buffer = p.buffer.as_ptr_range();
        feed(&mut p, b"0;a title");
        let Some(Command::ChangeWindowTitle(title)) = p.end(None) else {
            panic!("expected change_window_title");
        };
        assert_eq!(b"a title", title);
        assert!(buffer.contains(&title.as_ptr()), "the title is the buffer's bytes");

        // The same parser, fed again (the borrow has ended).
        p.reset();
        feed(&mut p, b"8;id=x;https://example.com");
        let Some(Command::HyperlinkStart { id: Some(id), uri }) = p.end(None) else {
            panic!("expected hyperlink_start with an id");
        };
        assert_eq!((&b"x"[..], &b"https://example.com"[..]), (id, uri));
        assert!(buffer.contains(&uri.as_ptr()));
        assert!(buffer.contains(&id.as_ptr()));
    });
    assert_eq!(0, counts.alloc_calls, "no payload copy, no list");
}

/// A payload from an allocating capture points into the capture's own allocation; the only allocation is the
/// capture's (`initCapacity`, `MAX_BUF` bytes), none for the payload.
#[test]
fn allocating_capture_payloads_borrow_the_capture() {
    let ((), counts) = with_counting(|| {
        let mut p = Parser::new(true);
        let buffer = p.buffer.as_ptr_range();
        feed(&mut p, b"52;c;aGVsbG8=");
        let Some(CaptureBacking::Allocating(w)) = p.capture.as_ref().map(|cap| &cap.backing) else {
            panic!("OSC 52 captures with an allocator");
        };
        let capture = w.as_ptr_range();
        let Some(Command::ClipboardContents { kind, data, terminator }) = p.end(Some(0x07)) else {
            panic!("expected clipboard_contents");
        };
        assert_eq!((b'c', Terminator::Bel), (kind, terminator));
        assert_eq!(b"aGVsbG8=", data);
        assert!(capture.contains(&data.as_ptr()), "the data is the capture's bytes");
        assert!(!buffer.contains(&data.as_ptr()));
    });
    assert_eq!(1, counts.alloc_calls, "the capture only");
    assert_eq!(0, counts.live_bytes);
}

/// The nested payloads borrow too: a Kitty clipboard OSC's metadata and payload, and an option value read from the
/// metadata, are slices of the capture.
#[test]
fn nested_payloads_and_option_values_borrow_the_capture() {
    let mut p = Parser::new(true);
    feed(&mut p, b"5522;type=read:mime=dGV4dA==;cGF5bG9hZA==");
    let Some(CaptureBacking::Allocating(w)) = p.capture.as_ref().map(|cap| &cap.backing) else {
        panic!("OSC 5522 captures with an allocator");
    };
    let capture = w.as_ptr_range();
    let Some(Command::KittyClipboardProtocol(osc)) = p.end(Some(0x1b)) else {
        panic!("expected kitty_clipboard_protocol");
    };
    assert_eq!(b"type=read:mime=dGV4dA==", osc.metadata);
    assert_eq!(Some(&b"cGF5bG9hZA=="[..]), osc.payload);
    assert!(capture.contains(&osc.metadata.as_ptr()));
    let mime = osc
        .read_option(parsers::kitty_clipboard_protocol::OptionKey::Mime)
        .as_bytes()
        .unwrap();
    assert_eq!(b"dGV4dA==", mime);
    assert!(capture.contains(&mime.as_ptr()));
}

#[test]
fn terminator_strings() {
    assert_eq!(Terminator::Bel, Terminator::init(Some(0x07)));
    assert_eq!(Terminator::St, Terminator::init(Some(b'\\')));
    assert_eq!(Terminator::St, Terminator::init(None));
    assert_eq!(b"\x1b\\", Terminator::St.string());
    assert_eq!("\x07", Terminator::Bel.to_string());
}

#[test]
fn command_keys_follow_the_variants() {
    assert_eq!(super::CommandKey::Invalid, Command::Invalid.key());
    assert_eq!(super::CommandKey::HyperlinkEnd, Command::HyperlinkEnd.key());
    assert_eq!(
        super::CommandKey::ChangeWindowTitle,
        Command::ChangeWindowTitle(b"").key()
    );
}
