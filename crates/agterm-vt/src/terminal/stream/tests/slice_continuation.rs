//! The stream's `next_slice_until_ground`, its ground detection and its continuation tracking.

use super::*;
use crate::mem::testing::with_failing;
use crate::terminal::stream_continuation::FixedIoWriter;

/// Counts the actions that commit terminal effects, and tracks the open APC and DCS strings.
struct ContinuationTestHandler {
    committed: usize,
    apc_active: bool,
    apc_buf: [u8; 256],
    apc_len: usize,
    dcs_active: bool,
}

impl Default for ContinuationTestHandler {
    fn default() -> ContinuationTestHandler {
        ContinuationTestHandler {
            committed: 0,
            apc_active: false,
            apc_buf: [0; 256],
            apc_len: 0,
            dcs_active: false,
        }
    }
}

impl Handler for ContinuationTestHandler {
    fn apc_start(&mut self) {
        self.apc_active = true;
    }
    fn apc_put(&mut self, code: u8) {
        self.apc_buf[self.apc_len] = code;
        self.apc_len += 1;
    }
    fn apc_put_slice(&mut self, bytes: &[u8]) {
        self.apc_buf[self.apc_len..self.apc_len + bytes.len()].copy_from_slice(bytes);
        self.apc_len += bytes.len();
    }
    fn dcs_hook(&mut self, _dcs: &parser::Dcs) {
        self.dcs_active = true;
    }
    fn dcs_put(&mut self, _code: u8) {}
    fn apc_end(&mut self, _v: ApcEnd) {
        self.apc_active = false;
        self.apc_len = 0;
        self.committed += 1;
    }
    fn dcs_unhook(&mut self) {
        self.dcs_active = false;
        self.committed += 1;
    }
    fn print(&mut self, _cp: u32) {
        self.committed += 1;
    }
    fn print_slice(&mut self, cps: &[u32]) {
        self.committed += cps.len();
    }
    fn print_repeat(&mut self, count: usize) {
        self.committed += count;
    }
    fn bell(&mut self) {
        self.committed += 1;
    }
    fn set_attribute(&mut self, _attr: sgr::Attribute<'_>) {
        self.committed += 1;
    }
    fn window_title(&mut self, _v: WindowTitle) {
        self.committed += 1;
    }
}

#[derive(Default)]
struct ContinuationNullHandler;
impl Handler for ContinuationNullHandler {}

#[test]
fn stream_next_slice_until_ground_stops_at_the_earliest_boundary() {
    let mut stream: Stream<ContinuationTestHandler> =
        Stream::new(Options::with_handler(ContinuationTestHandler::default()));

    stream.next_slice(b"\x1b[31");
    assert!(!stream.ground());
    stream.handler.committed = 0;

    let input = b"mABC\x1b[";
    let consumed = stream.next_slice_until_ground(input).unwrap();
    assert_eq!(1, consumed);
    assert!(stream.ground());
    assert_eq!(1, stream.handler.committed);

    // The suffix was not inspected by the handler and can be processed after
    // the caller performs work at the boundary.
    stream.next_slice(&input[consumed..]);
    assert!(!stream.ground());
    assert_eq!(parser::State::CsiEntry, stream.parser.state);
    assert_eq!(4, stream.handler.committed);
}

#[test]
fn stream_next_slice_until_ground_consumes_all_input_without_a_boundary() {
    let mut stream: Stream<ContinuationNullHandler> = Stream::new(Options::with_handler(ContinuationNullHandler));

    assert_eq!(Some(0), stream.next_slice_until_ground(b"unprocessed"));
    assert!(stream.ground());

    stream.next_slice(b"\x1b[");
    assert_eq!(None, stream.next_slice_until_ground(b"123"));
    assert!(!stream.ground());

    // A boundary on the final byte is distinguishable from exhausting the
    // input while still pending.
    assert_eq!(Some(1), stream.next_slice_until_ground(b"m"));
    assert!(stream.ground());
}

#[test]
fn stream_next_slice_until_ground_handles_utf8_boundaries() {
    // A completed codepoint is committed synchronously, and the printable
    // suffix remains untouched.
    let mut valid: Stream<ContinuationTestHandler> =
        Stream::new(Options::with_handler(ContinuationTestHandler::default()));
    valid.next_slice(&[0xF0]);
    let valid_input = [0x9Fu8, 0x98, 0x84, b'X'];
    assert_eq!(Some(3), valid.next_slice_until_ground(&valid_input));
    assert!(valid.ground());
    assert_eq!(1, valid.handler.committed);

    // A malformed continuation emits the replacement codepoint and retries
    // the same byte. Ground is observed after that complete byte operation.
    let mut malformed: Stream<ContinuationTestHandler> =
        Stream::new(Options::with_handler(ContinuationTestHandler::default()));
    malformed.next_slice(&[0xE0, 0xA0]);
    assert_eq!(Some(1), malformed.next_slice_until_ground(b"A!"));
    assert!(malformed.ground());
    assert_eq!(2, malformed.handler.committed);

    // If the retried byte is ESC, the stream has begun VT state at the end of
    // that byte and must continue to the following ground boundary.
    let mut retry_escape: Stream<ContinuationTestHandler> =
        Stream::new(Options::with_handler(ContinuationTestHandler::default()));
    retry_escape.next_slice(&[0xE0, 0xA0]);
    assert_eq!(Some(3), retry_escape.next_slice_until_ground(b"\x1b[mX"));
    assert!(retry_escape.ground());
}

#[test]
fn stream_next_slice_until_ground_handles_aborts_and_bulk_strings() {
    let mut aborted: Stream<ContinuationTestHandler> =
        Stream::new(Options::with_handler(ContinuationTestHandler::default()));
    aborted.next_slice(b"\x1b[123");
    assert_eq!(Some(1), aborted.next_slice_until_ground(&[0x18, b'X']));
    assert!(aborted.ground());

    let mut apc: Stream<ContinuationTestHandler> =
        Stream::new(Options::with_handler(ContinuationTestHandler::default()));
    apc.next_slice(b"\x1b_Gseed");
    let mut input = [0u8; 131];
    input[0..128].fill(b'a');
    input[128..130].copy_from_slice(b"\x1b\\");
    input[130] = b'X';
    assert_eq!(Some(130), apc.next_slice_until_ground(&input));
    assert!(apc.ground());
}

#[test]
fn stream_next_slice_until_ground_tracks_only_the_consumed_prefix() {
    let mut stream: Stream<ContinuationNullHandler> = Stream::new(Options {
        alloc: true,
        handler: ContinuationNullHandler,
        continuation_max_bytes: Some(64),
        osc_unknown_max_bytes: 0,
    });

    stream.next_slice(b"\x1b[31");
    assert_eq!(Some(1), stream.next_slice_until_ground(b"mX\x1b["));

    let mut buf = [0u8; 64];
    let mut writer = FixedIoWriter::new(&mut buf);
    stream.write_continuation(&mut writer).unwrap();
    assert_eq!(0, writer.buffered().len());

    stream.next_slice(b"\x1b[");
    assert_eq!(None, stream.next_slice_until_ground(b"123"));
    let mut pending_buf = [0u8; 64];
    let mut pending_writer = FixedIoWriter::new(&mut pending_buf);
    stream.write_continuation(&mut pending_writer).unwrap();
    assert_eq!(b"\x1b[123".as_slice(), pending_writer.buffered());
}

#[test]
fn stream_continuation_lifecycle() {
    let disabled: Stream<ContinuationTestHandler> =
        Stream::new(Options::with_handler(ContinuationTestHandler::default()));
    let mut disabled_buf = [0u8; 1];
    let mut disabled_writer = FixedIoWriter::new(&mut disabled_buf);
    assert!(matches!(
        disabled.write_continuation(&mut disabled_writer),
        Err(WriteContinuationError::ContinuationDisabled)
    ));

    let zero_capacity: Stream<ContinuationTestHandler> = Stream::new(Options {
        handler: ContinuationTestHandler::default(),
        alloc: false,
        continuation_max_bytes: Some(0),
        osc_unknown_max_bytes: 0,
    });
    let mut zero_capacity_buf = [0u8; 1];
    let mut zero_capacity_writer = FixedIoWriter::new(&mut zero_capacity_buf);
    assert!(matches!(
        zero_capacity.write_continuation(&mut zero_capacity_writer),
        Err(WriteContinuationError::ContinuationDisabled)
    ));
    let no_allocator: Stream<ContinuationTestHandler> = Stream::new(Options {
        handler: ContinuationTestHandler::default(),
        alloc: false,
        continuation_max_bytes: Some(64),
        osc_unknown_max_bytes: 0,
    });
    let mut no_allocator_buf = [0u8; 1];
    let mut no_allocator_writer = FixedIoWriter::new(&mut no_allocator_buf);
    assert!(matches!(
        no_allocator.write_continuation(&mut no_allocator_writer),
        Err(WriteContinuationError::ContinuationDisabled)
    ));

    let mut tracked: Stream<ContinuationTestHandler> = Stream::new(Options {
        alloc: true,
        handler: ContinuationTestHandler::default(),
        continuation_max_bytes: Some(64),
        osc_unknown_max_bytes: 0,
    });

    tracked.next_slice(b"complete input");
    let mut empty_buf = [0u8; 1];
    let mut empty_writer = FixedIoWriter::new(&mut empty_buf);
    tracked.write_continuation(&mut empty_writer).unwrap();
    assert_eq!(0, empty_writer.end);

    tracked.next_slice(b"\x1b[");
    let mut short_buf = [0u8; 1];
    let mut short_writer = FixedIoWriter::new(&mut short_buf);
    let Err(WriteContinuationError::WriteFailed(err)) = tracked.write_continuation(&mut short_writer) else {
        panic!("a writer too short for the suffix fails the write");
    };
    assert_eq!(std::io::ErrorKind::WriteZero, err.kind());

    // A tracker whose every allocation fails is unavailable once it has a suffix to keep.
    let (failing_stream, _) = with_failing(0, || {
        let mut s: Stream<ContinuationNullHandler> = Stream::new(Options {
            alloc: true,
            handler: ContinuationNullHandler,
            continuation_max_bytes: Some(64),
            osc_unknown_max_bytes: 0,
        });
        s.next_slice(b"\x1b[");
        s
    });
    let mut unavailable_buf = [0u8; 1];
    let mut unavailable_writer = FixedIoWriter::new(&mut unavailable_buf);
    assert!(matches!(
        failing_stream.write_continuation(&mut unavailable_writer),
        Err(WriteContinuationError::ContinuationUnavailable)
    ));
}

#[test]
fn stream_continuation_suffixes_are_replay_safe() {
    struct Case {
        input: &'static [u8],
        expected: &'static [u8],
    }
    let cases = [
        Case {
            input: b"text\x1b",
            expected: b"\x1b",
        },
        Case {
            input: b"text\x1b[12;",
            expected: b"\x1b[12;",
        },
        Case {
            input: b"text\x1b[1\x07;2",
            expected: b"\x1b[1;2",
        },
        Case {
            input: b"text\x1b]2;hello",
            expected: b"\x1b]2;hello",
        },
        Case {
            input: b"text\x1b_Gabc",
            expected: b"\x1b_Gabc",
        },
        Case {
            input: b"text\x1bP+qabc",
            expected: b"\x1bP+qabc",
        },
        Case {
            input: b"text\xE0\xA0\xF0",
            expected: b"\xF0",
        },
        Case {
            input: b"text\x1b[12\x1b",
            expected: b"\x1b",
        },
        Case {
            input: b"text\x1b[12\x9D2;title",
            expected: b"\x1b[12\x9D2;title",
        },
    ];

    for case in &cases {
        let mut stream: Stream<ContinuationTestHandler> = Stream::new(Options {
            alloc: true,
            handler: ContinuationTestHandler::default(),
            continuation_max_bytes: Some(1024),
            osc_unknown_max_bytes: 0,
        });
        stream.next_slice(case.input);

        let mut buf = [0u8; 1024];
        let mut writer = FixedIoWriter::new(&mut buf);
        stream.write_continuation(&mut writer).unwrap();
        assert_eq!(case.expected, writer.buffered());
    }
}

#[test]
fn stream_continuation_reconstructs_every_unfinished_vt_state() {
    let cases: [(&[u8], parser::State); 13] = [
        (b"\x1b", parser::State::Escape),
        (b"\x1b(", parser::State::EscapeIntermediate),
        (b"\x1b[", parser::State::CsiEntry),
        (b"\x1b[1", parser::State::CsiParam),
        (b"\x1b[1$", parser::State::CsiIntermediate),
        (b"\x1b[:", parser::State::CsiIgnore),
        (b"\x1bP", parser::State::DcsEntry),
        (b"\x1bP1", parser::State::DcsParam),
        (b"\x1bP1$", parser::State::DcsIntermediate),
        (b"\x1bP1q", parser::State::DcsPassthrough),
        (b"\x1bP:", parser::State::DcsIgnore),
        (b"\x1b]2;title", parser::State::OscString),
        (b"\x1b_Gpayload", parser::State::SosPmApcString),
    ];

    for (input, state) in cases {
        let mut source: Stream<ContinuationTestHandler> = Stream::new(Options {
            alloc: true,
            handler: ContinuationTestHandler::default(),
            continuation_max_bytes: Some(1024),
            osc_unknown_max_bytes: 0,
        });
        source.next_slice(input);
        assert_eq!(state, source.parser.state);

        let mut buf = [0u8; 1024];
        let mut writer = FixedIoWriter::new(&mut buf);
        source.write_continuation(&mut writer).unwrap();

        let mut restored: Stream<ContinuationTestHandler> = Stream::new(Options {
            alloc: true,
            handler: ContinuationTestHandler::default(),
            continuation_max_bytes: Some(1024),
            osc_unknown_max_bytes: 0,
        });
        restored.next_slice(writer.buffered());
        assert_eq!(0, restored.handler.committed);
        assert_eq!(source.parser.state, restored.parser.state);
        assert_eq!(source.utf8decoder.state, restored.utf8decoder.state);
    }

    // Parser ground is still unfinished while the UTF-8 decoder is waiting
    // for the remaining bytes of a codepoint.
    let mut utf8: Stream<ContinuationTestHandler> = Stream::new(Options {
        alloc: true,
        handler: ContinuationTestHandler::default(),
        continuation_max_bytes: Some(4),
        osc_unknown_max_bytes: 0,
    });
    utf8.next(0xF0);
    assert_eq!(parser::State::Ground, utf8.parser.state);
    assert!(utf8.utf8decoder.state != 0);
    let mut utf8_buf = [0u8; 4];
    let mut utf8_writer = FixedIoWriter::new(&mut utf8_buf);
    utf8.write_continuation(&mut utf8_writer).unwrap();
    assert_eq!(&[0xF0u8], utf8_writer.buffered());
}

#[test]
fn stream_continuation_is_chunking_independent_and_idempotent() {
    let input = b"committed\x1b[1\x07;2";

    let mut bulk: Stream<ContinuationTestHandler> = Stream::new(Options {
        alloc: true,
        handler: ContinuationTestHandler::default(),
        continuation_max_bytes: Some(1024),
        osc_unknown_max_bytes: 0,
    });
    bulk.next_slice(input);

    let mut scalar: Stream<ContinuationTestHandler> = Stream::new(Options {
        alloc: true,
        handler: ContinuationTestHandler::default(),
        continuation_max_bytes: Some(1024),
        osc_unknown_max_bytes: 0,
    });
    for &c in input {
        scalar.next(c);
    }

    let mut bulk_buf = [0u8; 1024];
    let mut bulk_writer = FixedIoWriter::new(&mut bulk_buf);
    bulk.write_continuation(&mut bulk_writer).unwrap();
    let mut scalar_buf = [0u8; 1024];
    let mut scalar_writer = FixedIoWriter::new(&mut scalar_buf);
    scalar.write_continuation(&mut scalar_writer).unwrap();
    assert_eq!(bulk_writer.buffered(), scalar_writer.buffered());

    let mut restored: Stream<ContinuationTestHandler> = Stream::new(Options {
        alloc: true,
        handler: ContinuationTestHandler::default(),
        continuation_max_bytes: Some(1024),
        osc_unknown_max_bytes: 0,
    });
    restored.next_slice(bulk_writer.buffered());
    assert_eq!(0, restored.handler.committed);

    let mut restored_buf = [0u8; 1024];
    let mut restored_writer = FixedIoWriter::new(&mut restored_buf);
    restored.write_continuation(&mut restored_writer).unwrap();
    assert_eq!(bulk_writer.buffered(), restored_writer.buffered());

    bulk.handler.committed = 0;
    restored.handler.committed = 0;
    bulk.next_slice(b"mZ");
    restored.next(b'm');
    restored.next(b'Z');
    assert_eq!(bulk.handler.committed, restored.handler.committed);
}

#[test]
fn stream_continuation_rebuilds_apc_handler_input() {
    let mut source: Stream<ContinuationTestHandler> = Stream::new(Options {
        alloc: true,
        handler: ContinuationTestHandler::default(),
        continuation_max_bytes: Some(1024),
        osc_unknown_max_bytes: 0,
    });
    source.next_slice(b"committed\x1b_Gabc");

    let mut continuation = [0u8; 1024];
    let mut writer = FixedIoWriter::new(&mut continuation);
    source.write_continuation(&mut writer).unwrap();

    let mut restored: Stream<ContinuationTestHandler> = Stream::new(Options {
        alloc: true,
        handler: ContinuationTestHandler::default(),
        continuation_max_bytes: Some(1024),
        osc_unknown_max_bytes: 0,
    });
    restored.next_slice(writer.buffered());
    assert_eq!(0, restored.handler.committed);
    assert!(restored.handler.apc_active);
    assert_eq!(
        &source.handler.apc_buf[0..source.handler.apc_len],
        &restored.handler.apc_buf[0..restored.handler.apc_len],
    );

    source.handler.committed = 0;
    restored.handler.committed = 0;
    source.next_slice(b"\x1b\\");
    restored.next_slice(b"\x1b\\");
    assert_eq!(source.handler.committed, restored.handler.committed);
    assert!(!source.handler.apc_active);
    assert!(!restored.handler.apc_active);
}

#[test]
fn stream_continuation_cap_and_recovery() {
    // The raw feed exceeds the cap, but only the unfinished three-byte
    // CSI suffix is retained.
    let mut seeded: Stream<ContinuationTestHandler> = Stream::new(Options {
        alloc: true,
        handler: ContinuationTestHandler::default(),
        continuation_max_bytes: Some(4),
        osc_unknown_max_bytes: 0,
    });
    seeded.next_slice(b"committed text\x1b[1");
    let mut seeded_buf = [0u8; 4];
    let mut seeded_writer = FixedIoWriter::new(&mut seeded_buf);
    seeded.write_continuation(&mut seeded_writer).unwrap();
    assert_eq!(b"\x1b[1".as_slice(), seeded_writer.buffered());

    let mut exceeded: Stream<ContinuationTestHandler> = Stream::new(Options {
        alloc: true,
        handler: ContinuationTestHandler::default(),
        continuation_max_bytes: Some(4),
        osc_unknown_max_bytes: 0,
    });
    exceeded.next_slice(b"\x1b[123");
    let mut unavailable_buf = [0u8; 1];
    let mut unavailable_writer = FixedIoWriter::new(&mut unavailable_buf);
    assert!(matches!(
        exceeded.write_continuation(&mut unavailable_writer),
        Err(WriteContinuationError::ContinuationUnavailable)
    ));

    // Completing the CSI reaches ground and recovers without rebuilding the
    // Stream. A later unfinished sequence is tracked normally.
    exceeded.next_slice(b"mtext\x1b[");
    let mut recovered_buf = [0u8; 4];
    let mut recovered_writer = FixedIoWriter::new(&mut recovered_buf);
    exceeded.write_continuation(&mut recovered_writer).unwrap();
    assert_eq!(b"\x1b[".as_slice(), recovered_writer.buffered());

    // A fresh ESC seed also recovers broken tracking even when the stream
    // never reaches ground: the ESC abandons the previous unfinished
    // state and everything after it is retained.
    exceeded.next_slice(b"\x1b[123");
    let mut reexceeded_buf = [0u8; 1];
    let mut reexceeded_writer = FixedIoWriter::new(&mut reexceeded_buf);
    assert!(matches!(
        exceeded.write_continuation(&mut reexceeded_writer),
        Err(WriteContinuationError::ContinuationUnavailable)
    ));
    exceeded.next_slice(b"\x1b]0;");
    let mut seed_buf = [0u8; 4];
    let mut seed_writer = FixedIoWriter::new(&mut seed_buf);
    exceeded.write_continuation(&mut seed_writer).unwrap();
    assert_eq!(b"\x1b]0;".as_slice(), seed_writer.buffered());
}

#[test]
fn stream_continuation_spans_multiple_bulk_feeds() {
    // An unfinished APC grows across feeds that contain no new seed.
    let mut apc: Stream<ContinuationTestHandler> = Stream::new(Options {
        alloc: true,
        handler: ContinuationTestHandler::default(),
        continuation_max_bytes: Some(1024),
        osc_unknown_max_bytes: 0,
    });
    apc.next_slice(b"text\x1b_Gab");
    apc.next_slice(b"cd");
    apc.next_slice(b"ef");
    let mut apc_buf = [0u8; 16];
    let mut apc_writer = FixedIoWriter::new(&mut apc_buf);
    apc.write_continuation(&mut apc_writer).unwrap();
    assert_eq!(b"\x1b_Gabcdef".as_slice(), apc_writer.buffered());

    // An incomplete UTF-8 sequence grows across feeds of its
    // continuation bytes.
    let mut utf8: Stream<ContinuationTestHandler> = Stream::new(Options {
        alloc: true,
        handler: ContinuationTestHandler::default(),
        continuation_max_bytes: Some(1024),
        osc_unknown_max_bytes: 0,
    });
    utf8.next_slice(b"text\xF0");
    utf8.next_slice(b"\x9F");
    let mut utf8_buf = [0u8; 4];
    let mut utf8_writer = FixedIoWriter::new(&mut utf8_buf);
    utf8.write_continuation(&mut utf8_writer).unwrap();
    assert_eq!(b"\xF0\x9F".as_slice(), utf8_writer.buffered());

    // A later feed with its own seed drops everything retained earlier.
    utf8.next_slice(b"\x98\x84 done \x1b[38;5");
    let mut seed_buf = [0u8; 8];
    let mut seed_writer = FixedIoWriter::new(&mut seed_buf);
    utf8.write_continuation(&mut seed_writer).unwrap();
    assert_eq!(b"\x1b[38;5".as_slice(), seed_writer.buffered());
}

#[test]
fn stream_continuation_exact_cap_and_large_unfinished_string() {
    let mut exact: Stream<ContinuationNullHandler> = Stream::new(Options {
        alloc: true,
        handler: ContinuationNullHandler,
        continuation_max_bytes: Some(5),
        osc_unknown_max_bytes: 0,
    });
    exact.next_slice(b"\x1b[123");
    let mut exact_buf = [0u8; 5];
    let mut exact_writer = FixedIoWriter::new(&mut exact_buf);
    exact.write_continuation(&mut exact_writer).unwrap();
    assert_eq!(b"\x1b[123".as_slice(), exact_writer.buffered());

    let payload_len = 12 * 1024;
    let mut input = vec![0u8; payload_len];
    input[0..3].copy_from_slice(b"\x1b_G");
    input[3..].fill(b'A');

    let mut large: Stream<ContinuationNullHandler> = Stream::new(Options {
        alloc: true,
        handler: ContinuationNullHandler,
        continuation_max_bytes: Some(payload_len),
        osc_unknown_max_bytes: 0,
    });
    large.next_slice(&input);
    let mut large_buf = vec![0u8; payload_len];
    let mut large_writer = FixedIoWriter::new(&mut large_buf);
    large.write_continuation(&mut large_writer).unwrap();
    assert_eq!(input.as_slice(), large_writer.buffered());
}

#[test]
fn stream_continuation_allocation_failure_recovers() {
    let mut stream: Stream<ContinuationNullHandler> = Stream::new(Options {
        alloc: true,
        handler: ContinuationNullHandler,
        continuation_max_bytes: Some(16 * 1024),
        osc_unknown_max_bytes: 0,
    });

    let mut input = vec![0u8; 12 * 1024];
    input[0..2].copy_from_slice(b"\x1b[");
    input[2..].fill(b'1');

    // The suffix outgrows the tracker's initial capacity, and its growth fails.
    let ((), failing) = with_failing(0, || stream.next_slice(&input));
    assert!(failing.has_induced_failure);
    let mut unavailable_buf = [0u8; 1];
    let mut unavailable_writer = FixedIoWriter::new(&mut unavailable_buf);
    assert!(matches!(
        stream.write_continuation(&mut unavailable_writer),
        Err(WriteContinuationError::ContinuationUnavailable)
    ));

    stream.next(b'm');
    let mut recovered_buf = [0u8; 1];
    let mut recovered_writer = FixedIoWriter::new(&mut recovered_buf);
    stream.write_continuation(&mut recovered_writer).unwrap();
    assert_eq!(0, recovered_writer.end);
}

#[test]
fn stream_continuation_every_byte_cuts_preserve_future_behavior() {
    let corpora: [&[u8]; 10] = [
        b"plain \xF0\x9F\x98\x84 utf8",
        b"bad \xE0\xA0\xF0\x9F\x98\x84 utf8",
        b"\x1b[1\x07;2mstyled\x1b[0m",
        b"\x1b]2;window title\x1b\\text",
        b"\x1bP$qm\x1b\\text",
        b"\x1b_Ga=q;payload\x1b\\text",
        b"\x1b_25a1;s\x1b\\text",
        b"\x1b]2;first\x1b\\\x1b_Gsecond",
        b"\x1b[12\x9D2;title\x1b\\text",
        b"\x1b[12\x18text\x1b[1\x1Atext",
    ];

    for corpus in &corpora {
        for cut in 0..=corpus.len() {
            let mut source: Stream<ContinuationTestHandler> = Stream::new(Options {
                alloc: true,
                handler: ContinuationTestHandler::default(),
                continuation_max_bytes: Some(64 * 1024),
                osc_unknown_max_bytes: 0,
            });
            source.next_slice(&corpus[0..cut]);

            let mut continuation = vec![0u8; 64 * 1024];
            let mut continuation_writer = FixedIoWriter::new(&mut continuation);
            source.write_continuation(&mut continuation_writer).unwrap();

            let mut restored: Stream<ContinuationTestHandler> = Stream::new(Options {
                alloc: true,
                handler: ContinuationTestHandler::default(),
                continuation_max_bytes: Some(64 * 1024),
                osc_unknown_max_bytes: 0,
            });
            restored.next_slice(continuation_writer.buffered());
            assert_eq!(0, restored.handler.committed);
            assert_eq!(source.handler.apc_active, restored.handler.apc_active);
            assert_eq!(source.handler.dcs_active, restored.handler.dcs_active);
            if source.handler.apc_active {
                assert_eq!(
                    &source.handler.apc_buf[0..source.handler.apc_len],
                    &restored.handler.apc_buf[0..restored.handler.apc_len],
                );
            }

            let mut reexport = vec![0u8; 64 * 1024];
            let mut reexport_writer = FixedIoWriter::new(&mut reexport);
            restored.write_continuation(&mut reexport_writer).unwrap();
            assert_eq!(continuation_writer.buffered(), reexport_writer.buffered());

            source.handler.committed = 0;
            restored.handler.committed = 0;
            source.next_slice(&corpus[cut..]);
            let mut offset = cut;
            let mut partition = cut.wrapping_add(corpus.len()).wrapping_add(1);
            while offset < corpus.len() {
                partition = partition.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let len = core::cmp::min(1 + partition % 7, corpus.len() - offset);
                restored.next_slice(&corpus[offset..offset + len]);
                offset += len;
            }
            assert_eq!(source.handler.committed, restored.handler.committed);
            assert_eq!(source.handler.apc_active, restored.handler.apc_active);
            assert_eq!(source.handler.dcs_active, restored.handler.dcs_active);

            let mut source_final = [0u8; 64];
            let mut source_final_writer = FixedIoWriter::new(&mut source_final);
            source.write_continuation(&mut source_final_writer).unwrap();
            let mut restored_final = [0u8; 64];
            let mut restored_final_writer = FixedIoWriter::new(&mut restored_final);
            restored.write_continuation(&mut restored_final_writer).unwrap();
            assert_eq!(source_final_writer.buffered(), restored_final_writer.buffered());
        }
    }
}
