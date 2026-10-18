//! The stream's logging, its fast paths (print-slice batching, the APC payload scan, the CSI parameters) against the
//! state machine and the per-byte path, printing and the bulk UTF-8 decode, and the ground-state C0 and C1 handling.

use super::*;

/// A handler that ignores every action.
struct NullHandler;

impl Handler for NullHandler {}

/// Collects every printed code point, whether it arrives as `print` or `print_slice`.
#[derive(Default)]
struct PrintCollector {
    cps: Vec<u32>,
}

impl Handler for PrintCollector {
    fn print(&mut self, cp: u32) {
        self.cps.push(cp);
    }
    fn print_slice(&mut self, cps: &[u32]) {
        self.cps.extend_from_slice(cps);
    }
}

/// What `input` prints fed one byte at a time.
fn printed_per_byte(input: &[u8]) -> Vec<u32> {
    drive_per_byte::<PrintCollector>(input).cps
}

/// What `input` prints fed as one slice.
fn printed_slice(input: &[u8]) -> Vec<u32> {
    drive_slice::<PrintCollector>(input).cps
}

/// The code points of `s`.
fn cps(s: &str) -> Vec<u32> {
    s.chars().map(u32::from).collect()
}

/// The dedup mechanism (Ghostty `b5053153f` has no test: its observable is the log): the first sighting of a key
/// logs, every repeat is suppressed, a distinct value logs again, and a full table suppresses new values too.
#[test]
fn stream_logs_unsupported_once_per_value() {
    let table = UnsupportedOnceTable::new();

    assert!(log_unsupported_once(&table, 34));
    assert!(!log_unsupported_once(&table, 34));
    assert!(!log_unsupported_once(&table, 34));
    assert_eq!(1, table.claimed());

    // "Once per distinct value", not "once".
    assert!(log_unsupported_once(&table, 35));
    assert!(!log_unsupported_once(&table, 35));
    assert_eq!(2, table.claimed());

    // Key 0 is a real key, not the empty sentinel (`u32::MAX`, which no `u16` key reaches).
    assert!(log_unsupported_once(&table, 0));
    assert!(!log_unsupported_once(&table, 0));

    // Fill the remaining slots: a full table suppresses new values too, and still finds a claimed key.
    for k in 100u16..(100 + 13) {
        assert!(log_unsupported_once(&table, k), "slot {k} should be free");
    }
    assert_eq!(16, table.claimed());
    assert!(
        !log_unsupported_once(&table, 9999),
        "table full: new values must be suppressed too"
    );
    assert!(!log_unsupported_once(&table, 34));
}

/// The dispatch sites call the dedup: OSC 1, whose key is a literal, so its table holds at most one claimed slot
/// whatever other tests do.
#[test]
fn stream_osc_1_logs_once() {
    let mut s = Stream::new(Options::with_handler(NullHandler));
    s.next_slice(b"\x1b]1;icon\x07");
    assert_eq!(
        1,
        SEEN_OSC1_CHANGE_ICON.claimed(),
        "the OSC 1 dispatch arm must claim its dedup slot"
    );

    // The same unsupported input a second time claims nothing further.
    s.next_slice(b"\x1b]1;other-icon\x07");
    assert_eq!(1, SEEN_OSC1_CHANGE_ICON.claimed());
    assert!(
        !log_unsupported_once(&SEEN_OSC1_CHANGE_ICON, 0),
        "key 0 was already claimed by the dispatch arm"
    );
}

/// The warnings `input` logs on a fresh stream.
fn warnings(input: &[u8]) -> Vec<crate::log_capture::CapturedRecord> {
    let mut s = Stream::new(Options::with_handler(NullHandler));
    let cap = crate::log_capture::start();
    s.next_slice(input);
    let messages = cap.messages();
    drop(cap);
    messages
}

/// A stream warning.
fn warn(msg: &str) -> crate::log_capture::CapturedRecord {
    (tracing::Level::WARN, "stream".to_owned(), msg.to_owned())
}

#[test]
fn csi_fast_path_logs_on_colon_separator_mismatch_for_non_m_command() {
    // A cursor position with a colon separator, which only 'm' takes.
    assert_eq!(
        vec![warn(
            "CSI colon or mixed separators only allowed for 'm' command, got: ESC [  { 1, 2 } H"
        )],
        warnings(b"\x1b[1:2H")
    );
}

/// A CSI argument is logged as Zig's `{f}` prints it.
#[test]
fn stream_logs_invalid_cursor_up_command() {
    assert_eq!(
        vec![warn("invalid cursor up command: ESC [  { 1, 2, 3 } A")],
        warnings(b"\x1b[1;2;3A")
    );
}

/// The intermediates warning carries the intermediates alone, as Ghostty's `{s}` of `input.intermediates` does.
#[test]
fn stream_logs_unimplemented_csi_a_with_intermediates() {
    assert_eq!(
        vec![warn("ignoring unimplemented CSI A with intermediates: ?")],
        warnings(b"\x1b[?1A")
    );
}

/// A once-logged site emits on the first sighting of a key, not on a repeat, and again for a different key. The
/// mode numbers are absurd so no other test claims their slots in the process-wide table first.
#[test]
fn stream_unsupported_once_emits_once_per_key() {
    assert_eq!(
        vec![warn("unimplemented mode: 60001"), warn("unimplemented mode: 60002")],
        warnings(b"\x1b[?60001h\x1b[?60001h\x1b[?60002h")
    );
}

/// The two UTF-8-decoded C1 sites are two Zig `logUnsupportedOnce` instantiations (a `u32` and a `u21` argument),
/// so each has its own table. Both key on the literal 0x80, so each table holds at most one claimed slot whatever
/// other tests do.
#[test]
fn utf8_decoded_c1_sites_have_separate_once_tables() {
    // The slice path.
    let mut s = Stream::new(Options::with_handler(NullHandler));
    s.next_slice("a\u{85}b".as_bytes());
    assert_eq!(
        1,
        SEEN_UTF8_DECODED_C1_SLICE.claimed(),
        "the slice path's C1 arm must claim its own dedup slot"
    );

    // The per-byte path.
    let mut s = Stream::new(Options::with_handler(NullHandler));
    for &b in "a\u{85}b".as_bytes() {
        s.next(b);
    }
    assert_eq!(
        1,
        SEEN_UTF8_DECODED_C1_CODEPOINT.claimed(),
        "the per-codepoint path's C1 arm must claim its own dedup slot"
    );
    assert!(!log_unsupported_once(&SEEN_UTF8_DECODED_C1_SLICE, 0x80));
    assert!(!log_unsupported_once(&SEEN_UTF8_DECODED_C1_CODEPOINT, 0x80));
}

/// Drive the input through the ground-state loop of `next_slice_capped`, where the printable runs are batched.
fn drive_ground<H: Handler + Default>(input: &[u8]) -> H {
    let mut s = stream::<H>();
    let mut cp_buf = [0_u32; CP_BUF_LEN];
    assert!(input.len() <= cp_buf.len());
    s.next_slice_capped(input, &mut cp_buf);
    s.handler
}

/// Records the `print_slice` runs as they come (rather than fanning them out to `print` the way the trait default
/// does), so a test can assert on the run structure and not just the resulting code points.
#[derive(Default)]
struct PrintSliceRecorder {
    runs: Vec<Vec<u32>>,
    singles: Vec<u32>,
    bells: usize,
}

impl Handler for PrintSliceRecorder {
    fn print(&mut self, cp: u32) {
        self.singles.push(cp);
    }
    fn print_slice(&mut self, cps: &[u32]) {
        self.runs.push(cps.to_vec());
    }
    fn bell(&mut self) {
        // BEL is the control the tests below use as a run separator.
        self.bells += 1;
    }
}

/// The printable-run batching is observationally identical to the per-byte `print` stream it replaces:
/// `RecordingHandler` keeps the `print_slice` default, which fans each run back out to `print`.
#[test]
#[cfg_attr(miri, ignore)]
fn stream_print_slice_matches_per_byte() {
    const CORPUS: &[&[u8]] = &[
        b"hello world",
        b"",
        b"a",
        b"\x07",
        b"hello\nworld\r\n",
        b"one\x07two\x08three",
        // Multi-byte UTF-8 mixed with ASCII and controls.
        "caf\u{e9} \u{4e00}\u{4e01} \u{1f600}!".as_bytes(),
        // Every C0 in range, interleaved with printables.
        b"a\x00b\x01c\x02d\x03e\x04f\x05g\x06h\x07i\x08j\x09k\x0al\x0bm\x0cn\x0do\x0ep\x0fq",
        // A run longer than any plausible vector width, then a control.
        b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\x07b",
        // Control first.
        b"\x0aabc",
        // Escape sequences bracketing printable runs.
        b"abc\x1b[31mdef\x1b[0mghi",
    ];

    for input in CORPUS {
        let per_byte = drive_per_byte::<RecordingHandler>(input);
        let ground = drive_ground::<RecordingHandler>(input);
        assert_eq!(
            per_byte.log,
            ground.log,
            "print_slice batching diverged from per-byte path on {:?}",
            String::from_utf8_lossy(input)
        );
    }
}

/// The run scan batches: a contiguous printable run arrives as one `print_slice` carrying every code point, and a C0
/// splits it into two runs with the control executed in between. A `print_slice` per code point (what an off-by-one
/// in the vectorized end scan produces) passes the previous test but not this one.
#[test]
#[cfg_attr(miri, ignore)]
fn stream_print_slice_batches_printable_runs() {
    // 70 'a's is longer than twice any native vector width for u32, so the vector loop runs to completion and hands
    // off to the scalar tail.
    let mut input = vec![b'a'; 70];
    input.push(0x07);
    input.extend_from_slice(b"bcd");

    let h = drive_ground::<PrintSliceRecorder>(&input);
    assert_eq!(vec![cps(&"a".repeat(70)), cps("bcd")], h.runs);
    assert_eq!(1, h.bells);
    assert!(
        h.singles.is_empty(),
        "the ground loop must emit print_slice, never bare print"
    );
}

/// A control at every offset across a full vector's worth of positions splits the run exactly there.
#[test]
#[cfg_attr(miri, ignore)]
fn stream_print_slice_splits_at_every_lane() {
    for n in 0..40usize {
        let mut input: Vec<u8> = vec![b'x'; n];
        input.push(0x07);
        input.extend_from_slice(b"yz");
        let h = drive_ground::<PrintSliceRecorder>(&input);
        let mut want = Vec::new();
        if n > 0 {
            want.push(vec![u32::from(b'x'); n]);
        }
        want.push(cps("yz"));
        assert_eq!(want, h.runs, "n={n}");
    }
}

/// Accumulates APC bytes whether they arrive per byte (`apc_put`) or in bulk (`apc_put_slice`).
#[derive(Default)]
struct ApcTestHandler {
    bytes: Vec<u8>,
    slices: usize,
    puts: usize,
    started: usize,
    ended: usize,
}

impl Handler for ApcTestHandler {
    fn apc_start(&mut self) {
        self.started += 1;
    }
    fn apc_end(&mut self, _v: ApcEnd) {
        self.ended += 1;
    }
    fn apc_put(&mut self, code: u8) {
        self.bytes.push(code);
        self.puts += 1;
    }
    fn apc_put_slice(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
        self.slices += 1;
    }
}

/// Drive input through the bulk path, where `consume_apc_string` is.
fn drive_apc_bulk(input: &[u8]) -> ApcTestHandler {
    drive_bulk::<ApcTestHandler>(input)
}

#[test]
fn stream_apc_bulk_slice() {
    let h = drive_apc_bulk(b"\x1b_Gf=24,s=10,v=20;aGVsbG8=\x1b\\");

    assert_eq!((1, 1), (h.started, h.ended));
    assert_eq!(b"Gf=24,s=10,v=20;aGVsbG8=".as_slice(), h.bytes);
    // The body arrives as a single slice.
    assert_eq!((1, 0), (h.slices, h.puts));
}

#[test]
fn stream_apc_bulk_slice_c1_st() {
    let h = drive_apc_bulk(b"\x1b_Gpayload\x9c");

    assert_eq!((1, 1), (h.started, h.ended));
    assert_eq!(b"Gpayload".as_slice(), h.bytes);
}

#[test]
fn stream_apc_bulk_slice_split_across_inputs() {
    let mut s = stream::<ApcTestHandler>();
    for chunk in [
        b"\x1b_Gf=24,s=10".as_slice(),
        b",v=20;aGVs".as_slice(),
        b"bG8=\x1b\\".as_slice(),
    ] {
        drive_bulk_into(&mut s, chunk);
    }
    let h = s.handler;

    assert_eq!((1, 1), (h.started, h.ended));
    assert_eq!(b"Gf=24,s=10,v=20;aGVsbG8=".as_slice(), h.bytes);
}

#[test]
fn stream_apc_bulk_slice_keeps_c0_bytes_as_data() {
    // BEL does not terminate an APC string; it is payload data.
    let h = drive_apc_bulk(b"\x1b_Gx\x07y\x1b\\");

    assert_eq!(1, h.ended);
    assert_eq!(b"Gx\x07y".as_slice(), h.bytes);
}

#[test]
fn stream_apc_aborted_by_can() {
    // CAN (0x18) aborts the APC string via the anywhere => ground transition. Exiting the sos_pm_apc_string state
    // emits apc_end, and the trailing bytes are printed, not treated as APC data.
    let h = drive_apc_bulk(b"\x1b_Gabcdefghijklmnopqrstuvwxyz0123456789\x18def");

    assert_eq!((1, 1), (h.started, h.ended));
    assert_eq!(b"Gabcdefghijklmnopqrstuvwxyz0123456789".as_slice(), h.bytes);
}

#[test]
fn stream_apc_scalar_path_matches() {
    let h = drive_per_byte::<ApcTestHandler>(b"\x1b_Gf=24;aGVsbG8=\x1b\\");

    assert_eq!((1, 1), (h.started, h.ended));
    assert_eq!(b"Gf=24;aGVsbG8=".as_slice(), h.bytes);
}

/// The bulk and the per-byte paths agree on `input`.
fn assert_apc_paths_agree(input: &[u8], ctx: &str) {
    let bulk = drive_apc_bulk(input);
    let scalar = drive_per_byte::<ApcTestHandler>(input);
    assert_eq!((scalar.started, scalar.ended), (bulk.started, bulk.ended), "{ctx}");
    assert_eq!(scalar.bytes, bulk.bytes, "{ctx}");
}

/// A control just around the lane boundaries ends the APC where the per-byte path does.
#[test]
fn stream_apc_vector_boundaries_match_scalar_path() {
    for position in [15usize, 16, 17, 31, 32, 33, 63, 64, 65] {
        for control in [0x18u8, 0x1A, 0x1B, 0x80, 0xFF] {
            let mut input = b"\x1b_G".to_vec();
            input.extend(core::iter::repeat_n(b'a', position));
            input.push(control);
            input.push(b'\\');
            assert_apc_paths_agree(&input, &format!("pos={position} ctl={control:#x}"));
        }
    }
}

/// The previous test's inputs end two bytes after the control, so the last full window always stops short of it and
/// the scalar tail finds it. Here the input is padded after the control so it lands mid-window and the vector loop
/// must detect it. 0x80..0xFF are ignored by the parse table (still APC state), so the bytes after them are payload
/// both paths must agree on.
#[test]
fn stream_apc_vector_invalid_branch_matches_scalar_path() {
    // Enough trailing bytes that the control is inside a full vector window for any plausible lane count.
    for position in 0..40usize {
        for control in [0x18u8, 0x1A, 0x1B, 0x80, 0xFF] {
            let mut input = b"\x1b_G".to_vec();
            input.extend(core::iter::repeat_n(b'a', position));
            input.push(control);
            input.extend(core::iter::repeat_n(b'z', 80));
            input.extend_from_slice(b"\x1b\\");
            assert_apc_paths_agree(&input, &format!("pos={position} ctl={control:#x}"));
        }
    }
}

/// The APC byte classes `consume_apc_string` stops on, as an independent oracle.
fn apc_stop_byte(b: u8) -> bool {
    matches!(b, 0x18 | 0x1A | 0x1B | 0x80..=0xFF)
}

/// The lanes of one payload scan group (one, where the target has no vectors).
const SCAN_N: usize = match PAYLOAD_SCAN_LANES {
    Some(n) => n,
    None => 1,
};

/// One vector group of `consume_apc_string` flags exactly the stop bytes, whichever lane they sit in: every byte
/// value at every lane offset, over several all-valid backgrounds (including the near misses 0x17, 0x19, 0x1C and
/// 0x7F).
#[test]
fn apc_group_invalid_flags_exactly_the_stop_bytes() {
    let fills: &[u8] = if cfg!(miri) {
        &[0x19, 0x7F]
    } else {
        &[0x00, 0x07, 0x17, 0x19, 0x1C, 0x41, 0x7F]
    };
    for &fill in fills {
        assert!(!apc_group_invalid(&[fill; SCAN_N]), "fill={fill:#x}");
        for lane in 0..SCAN_N {
            for b in 0..=u8::MAX {
                let mut group = [fill; SCAN_N];
                group[lane] = b;
                assert_eq!(
                    apc_stop_byte(b),
                    apc_group_invalid(&group),
                    "fill={fill:#x} lane={lane} byte={b:#x}"
                );
            }
        }
    }
}

/// `consume_apc_string` on `base` with `stop` at `stop_at` (none at `base.len()`) stops there and hands over exactly
/// the bytes before it, in one slice.
fn check_apc_stop(s: &mut Stream<ApcTestHandler>, base: &[u8], stop_at: usize, stop: u8) {
    let mut input = base.to_vec();
    if let Some(b) = input.get_mut(stop_at) {
        *b = stop;
    }
    s.handler = ApcTestHandler::default();
    s.parser.state = parser::State::SosPmApcString;
    let end = s.consume_apc_string(&input);
    let ctx = format!("len={} stop_at={stop_at} stop={stop:#x}", base.len());
    assert_eq!(stop_at, end, "{ctx}");
    assert_eq!(usize::from(stop_at > 0), s.handler.slices, "{ctx}");
    assert_eq!(&input[..stop_at], s.handler.bytes, "{ctx}");
}

/// `consume_apc_string` returns the offset of the first stop byte for a stop at every offset of the first three
/// vector groups and in every scalar tail length (inputs of 0 to 3 groups + 1 bytes), and consumes the whole input
/// when there is no stop byte. The payload bytes cycle through every non-stop value so each lane sees varied data.
#[test]
fn consume_apc_string_stops_at_every_offset() {
    let payload: Vec<u8> = (0..=u8::MAX).filter(|&b| !apc_stop_byte(b)).collect();
    // Miri: every offset and tail length still, with two stop bytes and one payload phase.
    let stops: &[u8] = if cfg!(miri) {
        &[0x1B, 0x80]
    } else {
        &[0x18, 0x1A, 0x1B, 0x80, 0x9C, 0xA0, 0xFF]
    };
    let shifts: &[usize] = if cfg!(miri) { &[0] } else { &[0, 7] };
    let mut s = stream::<ApcTestHandler>();

    for len in 0..=3 * SCAN_N + 1 {
        for &shift in shifts {
            let base: Vec<u8> = (0..len).map(|i| payload[(i + shift) % payload.len()]).collect();
            for stop_at in 0..len {
                for &stop in stops {
                    check_apc_stop(&mut s, &base, stop_at, stop);
                }
            }
            // No stop byte: the whole input.
            check_apc_stop(&mut s, &base, len, 0);
        }
    }
}

#[test]
fn stream_csi_bulk_params_match_per_byte() {
    for input in CSI_DIFFERENTIAL_CORPUS {
        let text = String::from_utf8_lossy(input);
        let per_byte = drive_per_byte::<RecordingHandler>(input);
        let bulk = drive_bulk::<RecordingHandler>(input);
        assert_eq!(
            per_byte.log, bulk.log,
            "bulk path diverged from per-byte path on {text:?}"
        );
        let sliced = drive_slice::<RecordingHandler>(input);
        assert_eq!(
            per_byte.log, sliced.log,
            "next_slice diverged from per-byte path on {text:?}"
        );

        // Split every input at every boundary and feed it as two chunks: the CSI parameter fast path carries the
        // accumulator state across calls, so a mid-parameter split is where a save/restore bug would hide.
        for split in 1..input.len() {
            let mut s = stream::<RecordingHandler>();
            drive_bulk_into(&mut s, &input[..split]);
            drive_bulk_into(&mut s, &input[split..]);
            assert_eq!(per_byte.log, s.handler.log, "split at {split} diverged on {text:?}");
        }
    }
}

/// The fast paths dispatch without going through `Parser::next`; they must not change what is dispatched. The
/// parser's state machine alone is the oracle: the bulk path and `next_slice` match it on the whole corpus.
#[test]
fn stream_fast_paths_match_the_state_machine() {
    let mut dispatched = 0usize;
    for input in CSI_DIFFERENTIAL_CORPUS {
        let oracle = drive_state_machine::<RecordingHandler>(input);
        let bulk = drive_bulk::<RecordingHandler>(input);
        let sliced = drive_slice::<RecordingHandler>(input);
        let text = String::from_utf8_lossy(input);
        assert_eq!(
            oracle.log, bulk.log,
            "the bulk path diverged from the state machine on {text:?}"
        );
        assert_eq!(
            oracle.log, sliced.log,
            "next_slice diverged from the state machine on {text:?}"
        );
        dispatched += oracle.log.len();
    }

    // Guard against the corpus silently ceasing to dispatch, which would make the comparison vacuous. A floor, so
    // adding corpus entries never reddens this.
    assert!(dispatched >= 30, "the corpus must dispatch; saw {dispatched} actions");
}

#[test]
fn stream_print() {
    assert_eq!(vec![u32::from(b'x')], printed_per_byte(b"x"));
}

/// REP delivers Ghostty's `print_repeat: usize` payload, the `u16` parameter widened.
#[test]
fn stream_print_repeat_payload_is_usize() {
    #[derive(Default)]
    struct H {
        count: Option<usize>,
    }
    impl Handler for H {
        fn print_repeat(&mut self, count: usize) {
            self.count = Some(count);
        }
    }

    assert_eq!(Some(500usize), drive_per_byte::<H>(b"\x1b[500b").count);
}

#[test]
fn simd_print_invalid_utf8() {
    assert_eq!(vec![0xFFFD], printed_slice(&[0xFF]));
}

#[test]
fn simd_complete_incomplete_utf8() {
    let mut s = stream::<PrintCollector>();
    s.next_slice(&[0xE0]); // 3 byte
    assert!(s.handler.cps.is_empty());
    s.next_slice(&[0xA0]); // still incomplete
    assert!(s.handler.cps.is_empty());
    s.next_slice(&[0x80]);
    assert_eq!(vec![0x800], s.handler.cps);
}

/// Every C0 control except ESC (which begins an escape sequence) is executed, never printed, in the ground state: on
/// the scalar path, the batched one, and a run long enough for the vectorized printable-run scan on either side.
#[test]
fn stream_ground_state_c0_controls_are_executed_not_printed() {
    for c in (0u8..0x20).filter(|&c| c != 0x1B) {
        assert_eq!(cps("AB"), printed_per_byte(&[b'A', c, b'B']), "{c:#x}");
        assert_eq!(cps("ABCD"), printed_slice(&[b'A', b'B', c, b'C', b'D']), "{c:#x}");

        let mut input = [b'A'; 65];
        input[32] = c;
        assert_eq!(vec![u32::from(b'A'); 64], printed_slice(&input), "{c:#x}");
    }
}

/// Every C1 control that arrives as well-formed UTF-8 (a two byte 0xC2-lead sequence) is dropped: not printed and
/// not interpreted as a control (U+009B must not start a CSI).
#[test]
fn stream_ground_state_utf8_decoded_c1_controls_are_ignored() {
    for c in 0x80u8..0xA0 {
        assert_eq!(cps("AB"), printed_per_byte(&[b'A', 0xC2, c, b'B']), "{c:#x}");
        assert_eq!(cps("ABCD"), printed_slice(&[b'A', b'B', 0xC2, c, b'C', b'D']), "{c:#x}");

        let mut input = [b'A'; 66];
        input[32] = 0xC2;
        input[33] = c;
        assert_eq!(vec![u32::from(b'A'); 64], printed_slice(&input), "{c:#x}");
    }
}

/// Next to the decoded C1 range: U+00A0 (NBSP) prints, a code point whose continuation byte falls in the C1 range
/// prints ("Ü" is 0xC3 0x9C), and a raw C1 byte is ill-formed UTF-8, a U+FFFD.
#[test]
fn stream_ground_state_prints_around_the_c1_range() {
    assert_eq!(vec![0xA0], printed_slice(&[0xC2, 0xA0]));
    assert_eq!(vec![0xDC], printed_slice("Ü".as_bytes()));
    assert_eq!(vec![0xFFFD], printed_slice(&[0x9B]));
}
