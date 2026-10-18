//! The stream's CSI dispatch (modes, DECSCA/DECSED/DECSEL, DECSCUSR, XTSHIFTESCAPE, CSI t/W, SGR, tab clear, device
//! status) and its OSC title and clipboard paths.

use super::*;

#[test]
fn stream_cursor_right_cuf() {
    struct H {
        amount: u16,
    }
    impl Handler for H {
        fn cursor_right(&mut self, v: CursorMovement) {
            self.amount = v.value;
        }
    }

    let mut s: Stream<H> = Stream::new(Options::with_handler(H { amount: 0 }));
    s.next_slice(b"\x1B[C");
    assert_eq!(1u16, s.handler.amount);

    s.next_slice(b"\x1B[5C");
    assert_eq!(5u16, s.handler.amount);

    s.handler.amount = 0;
    s.next_slice(b"\x1B[5;4C");
    assert_eq!(0u16, s.handler.amount);

    s.handler.amount = 0;
    s.next_slice(b"\x1b[?3C");
    assert_eq!(0u16, s.handler.amount);
}

#[test]
fn stream_dec_set_mode_sm_and_reset_mode_rm() {
    struct H {
        mode: modes::Mode,
    }
    impl Handler for H {
        fn set_mode(&mut self, v: Mode) {
            self.mode = v.mode;
        }
        fn reset_mode(&mut self, _v: Mode) {
            self.mode = modes::Mode::CursorKeys;
        }
    }

    let mut s: Stream<H> = Stream::new(Options::with_handler(H {
        mode: modes::Mode::CursorKeys,
    }));
    s.next_slice(b"\x1B[?6h");
    assert_eq!(modes::Mode::Origin, s.handler.mode);

    s.next_slice(b"\x1B[?6l");
    assert_eq!(modes::Mode::CursorKeys, s.handler.mode);

    s.handler.mode = modes::Mode::CursorKeys;
    s.next_slice(b"\x1B[6 h");
    assert_eq!(modes::Mode::CursorKeys, s.handler.mode);
}

#[test]
fn stream_ansi_set_mode_sm_and_reset_mode_rm() {
    struct H {
        mode: Option<modes::Mode>,
    }
    impl Handler for H {
        fn set_mode(&mut self, v: Mode) {
            self.mode = Some(v.mode);
        }
        fn reset_mode(&mut self, _v: Mode) {
            self.mode = None;
        }
    }

    let mut s: Stream<H> = Stream::new(Options::with_handler(H { mode: None }));
    s.next_slice(b"\x1B[4h");
    assert_eq!(modes::Mode::Insert, s.handler.mode.unwrap());

    s.next_slice(b"\x1B[4l");
    assert!(s.handler.mode.is_none());

    s.handler.mode = None;
    s.next_slice(b"\x1B[>5h");
    assert!(s.handler.mode.is_none());
}

#[test]
fn stream_decstr_dispatch() {
    #[derive(Default)]
    struct H {
        calls: usize,
    }
    impl Handler for H {
        fn soft_reset(&mut self) {
            self.calls += 1;
        }
    }

    struct Case {
        input: &'static [u8],
        calls: usize,
    }
    let cases = [
        Case {
            input: b"\x1b[!p",
            calls: 1,
        },
        Case {
            input: b"\x1b[1!p",
            calls: 0,
        },
        Case {
            input: b"\x1b[?!p",
            calls: 0,
        },
        Case {
            input: b"\x1b[$p",
            calls: 0,
        },
    ];

    for case in cases {
        let mut s: Stream<H> = Stream::new(Options::with_handler(H::default()));
        s.next_slice(case.input);
        assert_eq!(case.calls, s.handler.calls);
    }
}

#[test]
fn stream_decrqm_dispatch() {
    #[derive(Default)]
    struct H {
        calls: usize,
        mode: Option<modes::Mode>,
        raw: Option<RawMode>,
    }
    impl Handler for H {
        fn request_mode(&mut self, v: Mode) {
            self.calls += 1;
            self.mode = Some(v.mode);
        }
        fn request_mode_unknown(&mut self, v: RawMode) {
            self.calls += 1;
            self.raw = Some(v);
        }
    }

    struct Case {
        input: &'static [u8],
        mode: Option<modes::Mode>,
        raw: Option<RawMode>,
    }
    const fn c(input: &'static [u8]) -> Case {
        Case {
            input,
            mode: None,
            raw: None,
        }
    }
    let cases = [
        Case {
            mode: Some(modes::Mode::Insert),
            ..c(b"\x1b[4$p")
        },
        Case {
            mode: Some(modes::Mode::SlowScroll),
            ..c(b"\x1b[?4$p")
        },
        Case {
            raw: Some(RawMode { mode: 9999, ansi: true }),
            ..c(b"\x1b[9999$p")
        },
        Case {
            raw: Some(RawMode {
                mode: 9999,
                ansi: false,
            }),
            ..c(b"\x1b[?9999$p")
        },
        c(b"\x1b[4p"),
        c(b"\x1b[?4p"),
        c(b"\x1b[4!p"),
        c(b"\x1b[4 p"),
        c(b"\x1b[>4$p"),
        c(b"\x1b[?4!p"),
        c(b"\x1b[$p"),
        c(b"\x1b[?$p"),
        c(b"\x1b[4;20$p"),
        c(b"\x1b[?4;7$p"),
        c(b"\x1b[4:20$p"),
    ];
    for case in &cases {
        for split in 0..=case.input.len() {
            let mut s: Stream<H> = Stream::new(Options::with_handler(H::default()));
            s.next_slice(&case.input[..split]);
            if split < case.input.len() {
                assert_eq!(0, s.handler.calls);
            }
            s.next_slice(&case.input[split..]);
            assert_eq!(usize::from(case.mode.is_some() || case.raw.is_some()), s.handler.calls);
            assert_eq!(case.mode, s.handler.mode);
            assert_eq!(case.raw, s.handler.raw);
        }
    }
}

#[test]
fn stream_ansi_set_mode_sm_and_reset_mode_rm_with_unknown_value() {
    struct H {
        mode: Option<modes::Mode>,
    }
    impl Handler for H {}

    let mut s: Stream<H> = Stream::new(Options::with_handler(H { mode: None }));
    s.next_slice(b"\x1B[6h");
    assert!(s.handler.mode.is_none());

    s.next_slice(b"\x1B[6l");
    assert!(s.handler.mode.is_none());
}

#[test]
fn stream_restore_mode() {
    struct H {
        called: bool,
    }
    impl Handler for H {
        fn top_and_bottom_margin(&mut self, _v: Margin) {
            self.called = true;
        }
    }

    let mut s: Stream<H> = Stream::new(Options::with_handler(H { called: false }));
    for c in b"\x1B[?42r" {
        s.next(*c);
    }
    assert!(!s.handler.called);
}

#[test]
fn stream_pop_kitty_keyboard_with_no_params_defaults_to_1() {
    struct H {
        n: u16,
    }
    impl Handler for H {
        fn kitty_keyboard_pop(&mut self, v: u16) {
            self.n = v;
        }
    }

    let mut s: Stream<H> = Stream::new(Options::with_handler(H { n: 0 }));
    for c in b"\x1B[<u" {
        s.next(*c);
    }
    assert_eq!(1u16, s.handler.n);
}

#[test]
fn stream_decsca() {
    struct H {
        v: Option<ansi::ProtectedMode>,
    }
    impl Handler for H {
        fn protected_mode_off(&mut self) {
            self.v = Some(ansi::ProtectedMode::Off);
        }
        fn protected_mode_iso(&mut self) {
            self.v = Some(ansi::ProtectedMode::Iso);
        }
        fn protected_mode_dec(&mut self) {
            self.v = Some(ansi::ProtectedMode::Dec);
        }
    }

    let mut s: Stream<H> = Stream::new(Options::with_handler(H { v: None }));
    {
        for c in b"\x1B[\"q" {
            s.next(*c);
        }
        assert_eq!(ansi::ProtectedMode::Off, s.handler.v.unwrap());
    }
    {
        for c in b"\x1B[0\"q" {
            s.next(*c);
        }
        assert_eq!(ansi::ProtectedMode::Off, s.handler.v.unwrap());
    }
    {
        for c in b"\x1B[2\"q" {
            s.next(*c);
        }
        assert_eq!(ansi::ProtectedMode::Off, s.handler.v.unwrap());
    }
    {
        for c in b"\x1B[1\"q" {
            s.next(*c);
        }
        assert_eq!(ansi::ProtectedMode::Dec, s.handler.v.unwrap());
    }
}

#[test]
fn stream_deced_decsed() {
    #[derive(Default)]
    struct H {
        mode: Option<csi::EraseDisplay>,
        protected: Option<bool>,
    }
    impl H {
        fn erased(&mut self, mode: csi::EraseDisplay, protected: bool) {
            self.mode = Some(mode);
            self.protected = Some(protected);
        }
    }
    impl Handler for H {
        fn erase_display_below(&mut self, protected: bool) {
            self.erased(csi::EraseDisplay::Below, protected);
        }
        fn erase_display_above(&mut self, protected: bool) {
            self.erased(csi::EraseDisplay::Above, protected);
        }
        fn erase_display_complete(&mut self, protected: bool) {
            self.erased(csi::EraseDisplay::Complete, protected);
        }
        fn erase_display_scrollback(&mut self, protected: bool) {
            self.erased(csi::EraseDisplay::Scrollback, protected);
        }
        fn erase_display_scroll_complete(&mut self, protected: bool) {
            self.erased(csi::EraseDisplay::ScrollComplete, protected);
        }
    }

    let cases: [(&[u8], csi::EraseDisplay, bool); 11] = [
        (b"\x1B[?J", csi::EraseDisplay::Below, true),
        (b"\x1B[?0J", csi::EraseDisplay::Below, true),
        (b"\x1B[?1J", csi::EraseDisplay::Above, true),
        (b"\x1B[?2J", csi::EraseDisplay::Complete, true),
        (b"\x1B[?3J", csi::EraseDisplay::Scrollback, true),
        (b"\x1B[J", csi::EraseDisplay::Below, false),
        (b"\x1B[0J", csi::EraseDisplay::Below, false),
        (b"\x1B[1J", csi::EraseDisplay::Above, false),
        (b"\x1B[2J", csi::EraseDisplay::Complete, false),
        (b"\x1B[3J", csi::EraseDisplay::Scrollback, false),
        // Invalid and ignored by the handler
        (b"\x1B[>0J", csi::EraseDisplay::Scrollback, false),
    ];
    let mut s = stream::<H>();
    for (input, mode, protected) in cases {
        for &c in input {
            s.next(c);
        }
        let text = String::from_utf8_lossy(input);
        assert_eq!(
            (Some(mode), Some(protected)),
            (s.handler.mode, s.handler.protected),
            "{text:?}"
        );
    }
}

#[test]
fn stream_decel_decsel() {
    #[derive(Default)]
    struct H {
        mode: Option<csi::EraseLine>,
        protected: Option<bool>,
    }
    impl H {
        fn erased(&mut self, mode: csi::EraseLine, protected: bool) {
            self.mode = Some(mode);
            self.protected = Some(protected);
        }
    }
    impl Handler for H {
        fn erase_line_right(&mut self, protected: bool) {
            self.erased(csi::EraseLine::Right, protected);
        }
        fn erase_line_left(&mut self, protected: bool) {
            self.erased(csi::EraseLine::Left, protected);
        }
        fn erase_line_complete(&mut self, protected: bool) {
            self.erased(csi::EraseLine::Complete, protected);
        }
        fn erase_line_right_unless_pending_wrap(&mut self, protected: bool) {
            self.erased(csi::EraseLine::RightUnlessPendingWrap, protected);
        }
    }

    let cases: [(&[u8], csi::EraseLine, bool); 9] = [
        (b"\x1B[?K", csi::EraseLine::Right, true),
        (b"\x1B[?0K", csi::EraseLine::Right, true),
        (b"\x1B[?1K", csi::EraseLine::Left, true),
        (b"\x1B[?2K", csi::EraseLine::Complete, true),
        (b"\x1B[K", csi::EraseLine::Right, false),
        (b"\x1B[0K", csi::EraseLine::Right, false),
        (b"\x1B[1K", csi::EraseLine::Left, false),
        (b"\x1B[2K", csi::EraseLine::Complete, false),
        // Invalid and ignored by the handler
        (b"\x1B[<1K", csi::EraseLine::Complete, false),
    ];
    let mut s = stream::<H>();
    for (input, mode, protected) in cases {
        for &c in input {
            s.next(c);
        }
        let text = String::from_utf8_lossy(input);
        assert_eq!(
            (Some(mode), Some(protected)),
            (s.handler.mode, s.handler.protected),
            "{text:?}"
        );
    }
}

#[test]
fn stream_decscusr() {
    struct H {
        style: Option<ansi::CursorStyle>,
    }
    impl Handler for H {
        fn cursor_style(&mut self, v: ansi::CursorStyle) {
            self.style = Some(v);
        }
    }

    let mut s: Stream<H> = Stream::new(Options::with_handler(H { style: None }));
    s.next_slice(b"\x1B[ q");
    assert_eq!(s.handler.style.unwrap(), ansi::CursorStyle::Default);

    s.next_slice(b"\x1B[1 q");
    assert_eq!(s.handler.style.unwrap(), ansi::CursorStyle::BlinkingBlock);

    // Invalid and ignored by the handler
    s.next_slice(b"\x1B[?0 q");
    assert_eq!(s.handler.style.unwrap(), ansi::CursorStyle::BlinkingBlock);
}

#[test]
fn stream_decscusr_without_space() {
    struct H {
        style: Option<ansi::CursorStyle>,
    }
    impl Handler for H {
        fn cursor_style(&mut self, v: ansi::CursorStyle) {
            self.style = Some(v);
        }
    }

    let mut s: Stream<H> = Stream::new(Options::with_handler(H { style: None }));
    s.next_slice(b"\x1B[q");
    assert!(s.handler.style.is_none());

    s.next_slice(b"\x1B[1q");
    assert!(s.handler.style.is_none());
}

#[test]
fn stream_xtshiftescape() {
    struct H {
        escape: Option<bool>,
    }
    impl Handler for H {
        fn mouse_shift_capture(&mut self, capture: bool) {
            self.escape = Some(capture);
        }
    }

    let mut s: Stream<H> = Stream::new(Options::with_handler(H { escape: None }));
    s.next_slice(b"\x1B[>2s");
    assert!(s.handler.escape.is_none());

    s.next_slice(b"\x1B[>s");
    assert!(!s.handler.escape.unwrap());

    s.next_slice(b"\x1B[>0s");
    assert!(!s.handler.escape.unwrap());

    s.next_slice(b"\x1B[>1s");
    assert!(s.handler.escape.unwrap());

    // Invalid and ignored by the handler
    s.next_slice(b"\x1B[1 s");
    assert!(s.handler.escape.unwrap());
}

#[test]
fn stream_change_window_title_with_invalid_utf8() {
    struct H {
        seen: bool,
    }
    impl Handler for H {
        fn window_title(&mut self, _v: WindowTitle) {
            self.seen = true;
        }
    }

    {
        let mut s: Stream<H> = Stream::new(Options::with_handler(H { seen: false }));
        s.next_slice(b"\x1b]2;abc\x1b\\");
        assert!(s.handler.seen);
    }

    {
        let mut s: Stream<H> = Stream::new(Options::with_handler(H { seen: false }));
        s.next_slice(b"\x1b]2;abc\xc0\x1b\\");
        assert!(!s.handler.seen);
    }
}

#[test]
fn stream_osc_52_large_payload_in_chunks() {
    struct H {
        data: Vec<u8>,
        kind: u8,
        // terminator: ?osc.Terminator = null,
        terminator: Option<osc::Terminator>,
        count: usize,
    }
    impl Handler for H {
        fn clipboard_contents(&mut self, v: ClipboardContents) {
            self.count += 1;
            self.kind = v.kind;
            self.terminator = Some(v.terminator);
            self.data.extend_from_slice(v.data);
        }
    }

    // A payload that far exceeds the fixed buffer so the allocating
    // capture is used, fed in chunk sizes that don't align with the
    // sequence so the bulk path sees every kind of boundary.
    let prefix = b"\x1b]52;c;";
    let payload_len: usize = 150_000;
    let mut input: Vec<u8> = Vec::new();
    input.extend_from_slice(prefix);
    for i in 0..payload_len {
        input.push(b'A' + u8::try_from(i % 26).unwrap());
    }
    input.extend_from_slice(b"\x1b\\");

    let mut s: Stream<H> = Stream::new(Options {
        handler: H {
            data: Vec::new(),
            kind: 0,
            terminator: None,
            count: 0,
        },
        alloc: true,
        continuation_max_bytes: None,
        osc_unknown_max_bytes: 0,
    });

    let mut i: usize = 0;
    while i < input.len() {
        let end = (i + 4093).min(input.len());
        s.next_slice(&input[i..end]);
        i = end;
    }

    assert_eq!(1usize, s.handler.count);
    assert_eq!(b'c', s.handler.kind);
    assert_eq!(osc::Terminator::St, s.handler.terminator.unwrap());
    assert_eq!(
        &input[prefix.len()..prefix.len() + payload_len],
        s.handler.data.as_slice()
    );
}

#[test]
fn stream_osc_bulk_path_matches_per_byte_path() {
    for case in &osc_bulk_cases() {
        // Reference: byte-at-a-time.
        let mut r = osc_journal_stream();
        for &c in case {
            r.next(c);
        }

        // The whole slice at once.
        let mut s = osc_journal_stream();
        s.next_slice(case);
        assert_eq!(r.handler.journal, s.handler.journal);

        // Split into two slices at every possible boundary.
        for split in 0..=case.len() {
            let mut s = osc_journal_stream();
            s.next_slice(&case[..split]);
            s.next_slice(&case[split..]);
            assert_eq!(r.handler.journal, s.handler.journal, "split at {split}");
        }
    }
}

#[test]
fn stream_insert_characters() {
    struct H {
        called: bool,
    }
    impl Handler for H {
        fn insert_blanks(&mut self, _count: usize) {
            self.called = true;
        }
    }

    let mut s: Stream<H> = Stream::new(Options::with_handler(H { called: false }));
    for c in b"\x1B[42@" {
        s.next(*c);
    }
    assert!(s.handler.called);

    s.handler.called = false;
    for c in b"\x1B[?42@" {
        s.next(*c);
    }
    assert!(!s.handler.called);
}

#[test]
fn stream_insert_characters_explicit_zero_clamps_to_1() {
    struct H {
        value: Option<usize>,
    }
    impl Handler for H {
        fn insert_blanks(&mut self, count: usize) {
            self.value = Some(count);
        }
    }

    let mut s: Stream<H> = Stream::new(Options::with_handler(H { value: None }));
    for c in b"\x1B[0@" {
        s.next(*c);
    }
    assert_eq!(1usize, s.handler.value.unwrap());
}

#[test]
fn stream_scosc() {
    struct H {
        called: bool,
    }
    impl Handler for H {
        fn left_and_right_margin(&mut self, _v: Margin) {
            panic!("bad");
        }
        fn left_and_right_margin_ambiguous(&mut self) {
            self.called = true;
        }
    }

    let mut s: Stream<H> = Stream::new(Options::with_handler(H { called: false }));
    for c in b"\x1B[s" {
        s.next(*c);
    }
    assert!(s.handler.called);
}

#[test]
fn stream_scorc() {
    struct H {
        called: bool,
    }
    impl Handler for H {
        fn restore_cursor(&mut self) {
            self.called = true;
        }
    }

    let mut s: Stream<H> = Stream::new(Options::with_handler(H { called: false }));
    for c in b"\x1B[u" {
        s.next(*c);
    }
    assert!(s.handler.called);
}

#[test]
fn stream_too_many_csi_params() {
    struct H {}
    impl Handler for H {
        fn cursor_right(&mut self, _v: CursorMovement) {
            unreachable!();
        }
    }

    let mut s: Stream<H> = Stream::new(Options::with_handler(H {}));
    s.next_slice(b"\x1B[1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1C");
}

#[test]
fn stream_csi_param_too_long() {
    struct H {}
    impl Handler for H {}

    let mut s: Stream<H> = Stream::new(Options::with_handler(H {}));
    s.next_slice(b"\x1B[1111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111C");
}

#[test]
fn stream_send_report_with_csi_t() {
    #[derive(Default)]
    struct H {
        style: Option<csi::SizeReportStyle>,
    }
    impl Handler for H {
        fn size_report(&mut self, v: csi::SizeReportStyle) {
            self.style = Some(v);
        }
    }

    let mut s = stream::<H>();
    for (input, style) in [
        (b"\x1b[14t", csi::SizeReportStyle::Csi14T),
        (b"\x1b[16t", csi::SizeReportStyle::Csi16T),
        (b"\x1b[18t", csi::SizeReportStyle::Csi18T),
        // Drift `6104c78f1`: CSI 19 t reports the screen size in characters.
        (b"\x1b[19t", csi::SizeReportStyle::Csi19T),
        (b"\x1b[21t", csi::SizeReportStyle::Csi21T),
    ] {
        s.next_slice(input);
        assert_eq!(Some(style), s.handler.style, "{:?}", String::from_utf8_lossy(input));
    }
}

#[test]
fn stream_csi_8_t_resize_window() {
    struct H {
        size: Option<ResizeWindow>,
    }
    impl Handler for H {
        fn resize_window(&mut self, v: ResizeWindow) {
            self.size = Some(v);
        }
    }

    let mut s: Stream<H> = Stream::new(Options::with_handler(H { size: None }));

    s.next_slice(b"\x1b[8;40;120t");
    assert_eq!(40, s.handler.size.unwrap().rows);
    assert_eq!(120, s.handler.size.unwrap().columns);

    // Omitted parameters keep the current size
    s.next_slice(b"\x1b[8;;100t");
    assert_eq!(0, s.handler.size.unwrap().rows);
    assert_eq!(100, s.handler.size.unwrap().columns);

    s.next_slice(b"\x1b[8;30t");
    assert_eq!(30, s.handler.size.unwrap().rows);
    assert_eq!(0, s.handler.size.unwrap().columns);

    s.next_slice(b"\x1b[8t");
    assert_eq!(0, s.handler.size.unwrap().rows);
    assert_eq!(0, s.handler.size.unwrap().columns);

    // Extra parameters are invalid
    s.handler.size = None;
    s.next_slice(b"\x1b[8;30;100;1t");
    assert!(s.handler.size.is_none());
}

#[test]
fn stream_invalid_csi_t() {
    struct H {
        style: Option<csi::SizeReportStyle>,
    }
    impl Handler for H {}

    let mut s: Stream<H> = Stream::new(Options::with_handler(H { style: None }));

    s.next_slice(b"\x1b[19t");
    assert_eq!(None, s.handler.style);
}

#[test]
fn stream_csi_t_push_title() {
    struct H {
        index: Option<u16>,
    }
    impl Handler for H {
        fn title_push(&mut self, v: u16) {
            self.index = Some(v);
        }
    }

    let mut s: Stream<H> = Stream::new(Options::with_handler(H { index: None }));

    s.next_slice(b"\x1b[22;0t");
    assert_eq!(0u16, s.handler.index.unwrap());
}

#[test]
fn stream_csi_t_push_title_with_explicit_window() {
    struct H {
        index: Option<u16>,
    }
    impl Handler for H {
        fn title_push(&mut self, v: u16) {
            self.index = Some(v);
        }
    }

    let mut s: Stream<H> = Stream::new(Options::with_handler(H { index: None }));

    s.next_slice(b"\x1b[22;2t");
    assert_eq!(0u16, s.handler.index.unwrap());
}

#[test]
fn stream_csi_t_push_title_with_explicit_icon() {
    struct H {
        index: Option<u16>,
    }
    impl Handler for H {
        fn title_push(&mut self, v: u16) {
            self.index = Some(v);
        }
    }

    let mut s: Stream<H> = Stream::new(Options::with_handler(H { index: None }));

    s.next_slice(b"\x1b[22;1t");
    assert_eq!(None, s.handler.index);
}

#[test]
fn stream_csi_t_push_title_with_index() {
    struct H {
        index: Option<u16>,
    }
    impl Handler for H {
        fn title_push(&mut self, v: u16) {
            self.index = Some(v);
        }
    }

    let mut s: Stream<H> = Stream::new(Options::with_handler(H { index: None }));

    s.next_slice(b"\x1b[22;0;5t");
    assert_eq!(5u16, s.handler.index.unwrap());
}

#[test]
fn stream_csi_t_pop_title() {
    struct H {
        index: Option<u16>,
    }
    impl Handler for H {
        fn title_pop(&mut self, v: u16) {
            self.index = Some(v);
        }
    }

    let mut s: Stream<H> = Stream::new(Options::with_handler(H { index: None }));

    s.next_slice(b"\x1b[23;0t");
    assert_eq!(0u16, s.handler.index.unwrap());
}

#[test]
fn stream_csi_t_pop_title_with_explicit_window() {
    struct H {
        index: Option<u16>,
    }
    impl Handler for H {
        fn title_pop(&mut self, v: u16) {
            self.index = Some(v);
        }
    }

    let mut s: Stream<H> = Stream::new(Options::with_handler(H { index: None }));

    s.next_slice(b"\x1b[23;2t");
    assert_eq!(0u16, s.handler.index.unwrap());
}

#[test]
fn stream_csi_t_pop_title_with_explicit_icon() {
    struct H {
        index: Option<u16>,
    }
    impl Handler for H {
        fn title_pop(&mut self, v: u16) {
            self.index = Some(v);
        }
    }

    let mut s: Stream<H> = Stream::new(Options::with_handler(H { index: None }));

    s.next_slice(b"\x1b[23;1t");
    assert_eq!(None, s.handler.index);
}

#[test]
fn stream_csi_t_pop_title_with_index() {
    struct H {
        index: Option<u16>,
    }
    impl Handler for H {
        fn title_pop(&mut self, v: u16) {
            self.index = Some(v);
        }
    }

    let mut s: Stream<H> = Stream::new(Options::with_handler(H { index: None }));

    s.next_slice(b"\x1b[23;0;5t");
    assert_eq!(5u16, s.handler.index.unwrap());
}

#[test]
fn stream_csi_w_clear_tab_stops() {
    assert_tab_actions(&[
        (b"\x1b[2W", Some("tab_clear_current")),
        (b"\x1b[5W", Some("tab_clear_all")),
    ]);
}

#[test]
fn stream_csi_w_tab_set() {
    assert_tab_actions(&[
        (b"\x1b[W", Some("tab_set")),
        (b"\x1b[0W", Some("tab_set")),
        (b"\x1b[>W", None),
        (b"\x1b[99W", None),
    ]);
}

#[test]
fn stream_csi_question_w_reset_tab_stops() {
    assert_tab_actions(&[
        (b"\x1b[?2W", None),
        (b"\x1b[?5W", Some("tab_reset")),
        // Invalid and ignored by the handler
        (b"\x1b[?1;2;3W", None),
    ]);
}

#[test]
fn stream_sgr_with_17_plus_parameters_for_underline_color() {
    struct H {
        called: bool,
    }
    impl Handler for H {
        fn set_attribute(&mut self, _attr: sgr::Attribute<'_>) {
            self.called = true;
        }
    }

    let mut s: Stream<H> = Stream::new(Options::with_handler(H { called: false }));

    // Kakoune-style SGR with underline color as 17th parameter
    // This tests the fix where param 17 was being dropped
    s.next_slice(b"\x1b[4:3;38;2;51;51;51;48;2;170;170;170;58;2;255;97;136;0m");
    assert!(s.handler.called);
}

#[test]
fn stream_tab_clear_with_overflowing_param() {
    // Regression test for a fuzz crash: CSI with a parameter value that
    // saturates to 65535 (u16 max) causes @enumFromInt to panic when
    // converting to TabClear (enum(u8)).
    struct H {
        called: bool,
    }
    impl Handler for H {
        fn tab_clear_current(&mut self) {
            self.called = true;
        }
        fn tab_clear_all(&mut self) {
            self.called = true;
        }
    }

    let mut s: Stream<H> = Stream::new(Options::with_handler(H { called: false }));
    // This is the exact input from the fuzz crash (minus the mode byte):
    // CSI with a huge numeric param that saturates to 65535, followed by 'g'.
    s.next_slice(b"\x1b[388888888888888888888888888888888888g\x1b[0m");
}

/// Drift `6104c78f1`: CSI 19 t with more parameters is refused, with Ghostty's warning, as CSI 18 t is.
#[test]
fn stream_csi_19_t_with_extra_parameters_is_ignored() {
    #[derive(Default)]
    struct H {
        style: Option<csi::SizeReportStyle>,
    }
    impl Handler for H {
        fn size_report(&mut self, v: csi::SizeReportStyle) {
            self.style = Some(v);
        }
    }

    let mut s = stream::<H>();
    let cap = crate::log_capture::start();
    s.next_slice(b"\x1b[19;1t");
    let messages = cap.messages();
    drop(cap);
    assert_eq!(None, s.handler.style);
    let want = (
        tracing::Level::WARN,
        "stream".to_owned(),
        "ignoring CSI 19 t with extra parameters: ESC [  { 19, 1 } t".to_owned(),
    );
    assert_eq!(vec![want], messages);
}

/// Drift `d0c0d4008`: TBC with no parameter clears the current tab stop, as `0` does.
#[test]
fn stream_tab_clear_with_no_param_clears_the_current_stop() {
    #[derive(Default)]
    struct H {
        action: Option<&'static str>,
    }
    impl Handler for H {
        fn tab_clear_current(&mut self) {
            self.action = Some("tab_clear_current");
        }
        fn tab_clear_all(&mut self) {
            self.action = Some("tab_clear_all");
        }
    }

    let mut s = stream::<H>();
    for (input, action) in [
        (b"\x1b[g".as_slice(), "tab_clear_current"),
        (b"\x1b[0g".as_slice(), "tab_clear_current"),
        (b"\x1b[3g".as_slice(), "tab_clear_all"),
    ] {
        s.handler.action = None;
        s.next_slice(input);
        assert_eq!(Some(action), s.handler.action, "{:?}", String::from_utf8_lossy(input));
    }
}

/// Drift `fbda61b87`: `CSI ? 6 n` is DECXCPR, the extended cursor position report, and `CSI 6 n` the plain CPR.
#[test]
fn stream_device_status_extended_cursor_position() {
    #[derive(Default)]
    struct H {
        request: Option<device_status::Request>,
    }
    impl Handler for H {
        fn device_status(&mut self, v: DeviceStatus) {
            self.request = Some(v.request);
        }
    }

    let mut s = stream::<H>();
    for (input, request) in [
        (b"\x1b[?6n".as_slice(), device_status::Request::CursorPositionExtended),
        (b"\x1b[6n".as_slice(), device_status::Request::CursorPosition),
        (b"\x1b[5n".as_slice(), device_status::Request::OperatingStatus),
        (b"\x1b[?996n".as_slice(), device_status::Request::ColorScheme),
    ] {
        s.handler.request = None;
        s.next_slice(input);
        assert_eq!(Some(request), s.handler.request, "{:?}", String::from_utf8_lossy(input));
    }

    // The `?` form of the operating status is not a request.
    s.handler.request = None;
    s.next_slice(b"\x1b[?5n");
    assert_eq!(None, s.handler.request);
}

/// The tab stop action each input dispatches on a fresh recorder (`None` for none).
fn assert_tab_actions(cases: &[(&[u8], Option<&'static str>)]) {
    let mut s = stream::<TabRecorder>();
    for &(input, action) in cases {
        s.handler.action = None;
        s.next_slice(input);
        assert_eq!(action, s.handler.action, "{:?}", String::from_utf8_lossy(input));
    }
}

/// The inputs the OSC bulk path must journal as the per-byte one does.
fn osc_bulk_cases() -> Vec<Vec<u8>> {
    let mut cases: Vec<Vec<u8>> = [
        b"\x1b]52;c;aGVsbG8=\x1b\\".as_slice(),
        b"\x1b]52;c;aGVsbG8=\x07",
        b"\x1b]52;;aGVsbG8=\x07",
        // Ignored C0 byte embedded in the payload.
        b"\x1b]52;c;aGVs\x01bG8=\x07",
        // CAN and SUB aborts.
        b"\x1b]52;c;aGVsbG8=\x18",
        b"\x1b]52;c;aGVsbG8=\x1a",
        // C1 byte embedded in the payload is data.
        b"\x1b]0;ab\x9ccd\x07",
        // Terminated with trailing printable text.
        b"\x1b]0;a title\x07x",
        // Back-to-back sequences.
        b"\x1b]2;another title\x1b\\\x1b]0;t2\x07",
        b"\x1b]5522;type=write;aGVsbG8=\x1b\\",
        // Invalid OSC number.
        b"\x1b]999;junk\x07",
    ]
    .iter()
    .map(|c| c.to_vec())
    .collect();
    // Exceeds the fixed buffer: allocating capture.
    let mut c = b"\x1b]52;c;".to_vec();
    c.extend(std::iter::repeat_n(b'y', 3000));
    c.extend_from_slice(b"\x1b\\");
    cases.push(c);
    // Exceeds the fixed buffer: overflow, no dispatch.
    let mut c = b"\x1b]0;".to_vec();
    c.extend(std::iter::repeat_n(b'x', 3000));
    c.extend_from_slice(b"\x07");
    cases.push(c);
    cases
}

/// A journaling stream that may allocate.
fn osc_journal_stream() -> Stream<OscJournal> {
    Stream::new(Options {
        alloc: true,
        ..Options::with_handler(OscJournal::default())
    })
}

/// Records the tab stop action a CSI W (or TBC) dispatched.
#[derive(Default)]
struct TabRecorder {
    action: Option<&'static str>,
}

impl Handler for TabRecorder {
    fn tab_clear_current(&mut self) {
        self.action = Some("tab_clear_current");
    }
    fn tab_clear_all(&mut self) {
        self.action = Some("tab_clear_all");
    }
    fn tab_set(&mut self) {
        self.action = Some("tab_set");
    }
    fn tab_reset(&mut self) {
        self.action = Some("tab_reset");
    }
}

/// Journals every dispatch relevant to OSC processing in a normalized text form, so streams fed different ways can
/// be compared byte for byte.
#[derive(Default)]
struct OscJournal {
    journal: Vec<u8>,
}

impl Handler for OscJournal {
    fn clipboard_contents(&mut self, v: ClipboardContents<'_>) {
        self.journal.extend_from_slice(b"clip kind=");
        self.journal.push(v.kind);
        self.journal.extend_from_slice(b" term=");
        self.journal.extend_from_slice(match v.terminator {
            osc::Terminator::St => b"st".as_slice(),
            osc::Terminator::Bel => b"bel".as_slice(),
        });
        self.journal.extend_from_slice(b" data=");
        self.journal.extend_from_slice(v.data);
        self.journal.push(b'\n');
    }
    fn kitty_clipboard(&mut self, v: osc::KittyClipboardProtocol<'_>) {
        self.journal.extend_from_slice(b"kitty meta=");
        self.journal.extend_from_slice(v.metadata);
        self.journal.extend_from_slice(b" payload=");
        // Zig's `{?s}` prints "null" for a null payload.
        self.journal.extend_from_slice(v.payload.unwrap_or(b"null"));
        self.journal.push(b'\n');
    }
    fn window_title(&mut self, v: WindowTitle<'_>) {
        self.journal.extend_from_slice(b"title ");
        self.journal.extend_from_slice(v.title);
        self.journal.push(b'\n');
    }
    // Prints are journaled per code point so the per-byte and slice paths journal identically.
    fn print(&mut self, cp: u32) {
        let c = char::from_u32(cp).unwrap();
        self.journal.extend_from_slice(format!("print {c}\n").as_bytes());
    }
    fn print_slice(&mut self, cps: &[u32]) {
        for &cp in cps {
            self.print(cp);
        }
    }
}
