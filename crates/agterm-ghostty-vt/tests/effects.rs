//! The effect callbacks through real VT bytes: each sequence reaches its [`Effects`] method with its data, replies go
//! out through `write_pty`, and a render hold can capture the terminal from inside its callback.

use std::sync::{Arc, Mutex, PoisonError};

use agterm_ghostty_vt::{
    ClipboardContent, ClipboardLocation, ClipboardReadReply, ClipboardReadRequest, ClipboardWriteRequest,
    ClipboardWriteResult, ColorScheme, DeviceAttributes, Effects, Error, HeldTerminal, Mode, MouseShape, ProgramStatus,
    ProgramStatusKind, ProgramStatusState, ProgressReport, ProgressState, RenderState, Rgb, ScrollbackLimits,
    SizeReport, Terminal,
};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Seen {
    Pty(Vec<u8>),
    Bell,
    Title(String),
    Pwd(String),
    Notification(String, String),
    Progress(ProgressReport),
    Status(ProgramStatus),
    Write(ClipboardLocation, Vec<(Vec<u8>, Vec<u8>)>),
    Read(ClipboardLocation, Vec<Vec<u8>>, bool),
    Hold(bool, Vec<String>),
    Reset,
}

/// What the recorder saw, shared with the test.
type Log = Arc<Mutex<Vec<Seen>>>;

#[derive(Default)]
struct Recorder {
    seen: Log,
    clipboard: Option<String>,
    capture: Option<RenderState>,
    scheme: Option<ColorScheme>,
}

impl Recorder {
    fn push(&self, seen: Seen) {
        self.seen.lock().unwrap_or_else(PoisonError::into_inner).push(seen);
    }
}

impl Effects for Recorder {
    fn write_pty(&mut self, data: &[u8]) {
        self.push(Seen::Pty(data.to_vec()));
    }

    fn bell(&mut self) {
        self.push(Seen::Bell);
    }

    fn title_changed(&mut self, title: &str) {
        self.push(Seen::Title(title.to_owned()));
    }

    fn pwd_changed(&mut self, pwd: &str) {
        self.push(Seen::Pwd(pwd.to_owned()));
    }

    fn desktop_notification(&mut self, title: &str, body: &str) {
        self.push(Seen::Notification(title.to_owned(), body.to_owned()));
    }

    fn progress_report(&mut self, report: ProgressReport) {
        self.push(Seen::Progress(report));
    }

    fn program_status(&mut self, status: ProgramStatus) {
        self.push(Seen::Status(status));
    }

    fn clipboard_write(&mut self, request: &ClipboardWriteRequest<'_>) -> ClipboardWriteResult {
        let contents = request
            .contents
            .iter()
            .map(|content| (content.mime.to_vec(), content.data.to_vec()))
            .collect();
        self.push(Seen::Write(request.location, contents));
        ClipboardWriteResult::Success
    }

    fn clipboard_read(&mut self, request: &ClipboardReadRequest<'_>) -> ClipboardReadReply {
        let mimes = request.mimes.iter().map(|mime| mime.to_vec()).collect();
        self.push(Seen::Read(request.location, mimes, request.list));
        match &self.clipboard {
            Some(text) => ClipboardReadReply::Success {
                contents: vec![ClipboardContent {
                    mime: "text/plain".to_owned(),
                    data: text.clone().into_bytes(),
                }],
                available: vec!["text/plain".to_owned()],
            },
            None => ClipboardReadReply::Denied,
        }
    }

    fn render_hold(&mut self, held: bool, terminal: &mut HeldTerminal<'_>) {
        let lines = match self.capture.as_mut() {
            Some(state) if held => held_screen(state, terminal).unwrap_or_else(|err| vec![format!("error: {err}")]),
            _ => Vec::new(),
        };
        self.push(Seen::Hold(held, lines));
    }

    fn size(&mut self, current: SizeReport) -> Option<SizeReport> {
        Some(current)
    }

    fn device_attributes(&mut self) -> Option<DeviceAttributes> {
        Some(DeviceAttributes {
            conformance_level: 62,
            features: vec![22],
            device_type: 1,
            firmware_version: 10,
            rom_cartridge: 0,
            unit_id: 0,
        })
    }

    fn xtversion(&mut self) -> Option<&str> {
        Some("agterm-test 1.0")
    }

    fn color_scheme(&mut self) -> Option<ColorScheme> {
        self.scheme
    }

    fn reset(&mut self) {
        self.push(Seen::Reset);
    }
}

/// The screen a hold captured, captured from inside the callback.
fn held_screen(state: &mut RenderState, terminal: &mut HeldTerminal<'_>) -> Result<Vec<String>, Error> {
    state.update_held(terminal)?;
    let mut rows = state.rows()?;
    let mut lines = Vec::new();
    while let Some(mut row) = rows.next_row() {
        lines.push(row.text()?.trim_end().to_owned());
    }
    Ok(lines)
}

fn terminal(recorder: Recorder) -> Result<(Terminal, Log), Error> {
    let seen = Arc::clone(&recorder.seen);
    let mut terminal = Terminal::new(20, 4)?;
    terminal.set_effects(Box::new(recorder))?;
    Ok((terminal, seen))
}

fn take(seen: &Log) -> Vec<Seen> {
    std::mem::take(&mut *seen.lock().unwrap_or_else(PoisonError::into_inner))
}

fn pty(seen: &[Seen]) -> Vec<u8> {
    seen.iter()
        .filter_map(|seen| match seen {
            Seen::Pty(data) => Some(data.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

#[test]
fn title_pwd_bell_and_notifications_reach_their_callbacks() {
    let (mut terminal, seen) = terminal(Recorder::default()).unwrap();
    terminal.vt_write(b"\x1b]2;hello\x07\x1b]7;file://host/tmp\x1b\\\x07");
    terminal.vt_write(b"\x1b]9;plain body\x07\x1b]777;notify;Title;Body\x07");
    assert_eq!(
        take(&seen),
        [
            Seen::Title("hello".to_owned()),
            Seen::Pwd("file://host/tmp".to_owned()),
            Seen::Bell,
            Seen::Notification(String::new(), "plain body".to_owned()),
            Seen::Notification("Title".to_owned(), "Body".to_owned()),
        ]
    );
    assert_eq!(terminal.title(), "hello");
    assert_eq!(terminal.pwd(), "file://host/tmp");
}

#[test]
fn progress_and_program_status_are_decoded() {
    let (mut terminal, seen) = terminal(Recorder::default()).unwrap();
    terminal.vt_write(b"\x1b]9;4;1;42\x07\x1b]9;4;3\x07\x1b]9;4;0\x07");
    // "Apply?" in base64
    terminal.vt_write(b"\x1b]7501;state=blocked:kind=permission:app=terraform:msg=QXBwbHk/\x1b\\");
    let seen = take(&seen);
    assert_eq!(
        seen[..3],
        [
            Seen::Progress(ProgressReport {
                state: ProgressState::Set,
                progress: Some(42),
            }),
            Seen::Progress(ProgressReport {
                state: ProgressState::Indeterminate,
                progress: None,
            }),
            Seen::Progress(ProgressReport {
                state: ProgressState::Remove,
                progress: None,
            }),
        ]
    );
    assert_eq!(
        seen[3],
        Seen::Status(ProgramStatus {
            state: ProgramStatusState::Blocked,
            kind: ProgramStatusKind::Permission,
            progress: None,
            id: String::new(),
            app: "terraform".to_owned(),
            title: String::new(),
            message: "Apply?".to_owned(),
        })
    );
}

#[test]
fn osc_52_writes_are_decoded_and_reads_are_answered_through_the_pty() {
    let recorder = Recorder {
        clipboard: Some("from clipboard".to_owned()),
        ..Recorder::default()
    };
    let (mut terminal, seen) = terminal(recorder).unwrap();
    // "copied" in base64, to the primary selection
    terminal.vt_write(b"\x1b]52;p;Y29waWVk\x07");
    terminal.vt_write(b"\x1b]52;c;?\x1b\\");
    let seen = take(&seen);
    assert_eq!(
        seen[0],
        Seen::Write(
            ClipboardLocation::Primary,
            vec![(b"text/plain".to_vec(), b"copied".to_vec())]
        )
    );
    assert_eq!(
        seen[1],
        Seen::Read(ClipboardLocation::Standard, vec![b"text/plain".to_vec()], false)
    );
    // "from clipboard" in base64, with the request's terminator
    assert_eq!(pty(&seen), b"\x1b]52;c;ZnJvbSBjbGlwYm9hcmQ=\x1b\\");
}

#[test]
fn a_denied_read_answers_an_empty_clipboard() {
    let (mut terminal, seen) = terminal(Recorder::default()).unwrap();
    terminal.vt_write(b"\x1b]52;c;?\x07");
    assert_eq!(pty(&take(&seen)), b"\x1b]52;c;\x07");
}

#[test]
fn queries_are_answered_from_the_handler() {
    let (mut terminal, seen) = terminal(Recorder::default()).unwrap();
    terminal.resize(20, 4, 8, 16).unwrap();
    take(&seen);
    terminal.vt_write(b"\x1b[c");
    assert_eq!(pty(&take(&seen)), b"\x1b[?62;22c");
    terminal.vt_write(b"\x1b[>q");
    assert_eq!(pty(&take(&seen)), b"\x1bP>|agterm-test 1.0\x1b\\");
    terminal.vt_write(b"\x1b[18t");
    assert_eq!(pty(&take(&seen)), b"\x1b[8;4;20t");
    terminal.vt_write(b"\x1b[14t");
    assert_eq!(pty(&take(&seen)), b"\x1b[4;64;160t");
}

#[test]
fn a_color_scheme_query_is_answered_from_the_handler_and_mode_2031_is_tracked() {
    let (mut unanswered, seen) = terminal(Recorder::default()).unwrap();
    unanswered.vt_write(b"\x1b[?996n");
    assert_eq!(pty(&take(&seen)), b"", "no scheme: the query goes unanswered");
    for (scheme, report) in [
        (ColorScheme::Dark, b"\x1b[?997;1n"),
        (ColorScheme::Light, b"\x1b[?997;2n"),
    ] {
        let (mut answering, seen) = terminal(Recorder {
            scheme: Some(scheme),
            ..Recorder::default()
        })
        .unwrap();
        answering.vt_write(b"\x1b[?996n");
        let answer = pty(&take(&seen));
        assert_eq!(answer, report, "{scheme:?}");
        assert_eq!(
            scheme.report(),
            answer.as_slice(),
            "an unsolicited report is the query's answer"
        );
    }
    assert!(!unanswered.mode(Mode::COLOR_SCHEME_REPORT));
    unanswered.vt_write(b"\x1b[?2031h");
    assert!(unanswered.mode(Mode::COLOR_SCHEME_REPORT));
    unanswered.vt_write(b"\x1b[?2031l");
    assert!(!unanswered.mode(Mode::COLOR_SCHEME_REPORT));
}

#[test]
fn xtgettcap_tn_answers_the_terminfo_name_once_one_is_set() {
    // XTGETTCAP for "TN", hex-encoded
    const QUERY: &[u8] = b"\x1bP+q544e\x1b\\";
    let (mut terminal, seen) = terminal(Recorder::default()).unwrap();
    terminal.vt_write(QUERY);
    let unset = pty(&take(&seen));
    assert!(!unset.starts_with(b"\x1bP1+r"), "no name: no answer, got {unset:?}");

    terminal.set_terminfo_name(Some("xterm-ghostty")).unwrap();
    terminal.vt_write(QUERY);
    let answer = String::from_utf8(pty(&take(&seen))).unwrap();
    // "xterm-ghostty", hex-encoded
    assert!(
        answer.eq_ignore_ascii_case("\x1bP1+r544e=787465726D2D67686F73747479\x1b\\"),
        "{answer:?}"
    );

    terminal.set_terminfo_name(None).unwrap();
    terminal.vt_write(QUERY);
    assert!(!pty(&take(&seen)).starts_with(b"\x1bP1+r"), "cleared");
    assert_eq!(
        terminal.set_terminfo_name(Some(&"x".repeat(129))),
        Err(Error::InvalidValue)
    );
}

#[test]
fn a_render_hold_captures_the_frame_the_program_left_on_screen() {
    let recorder = Recorder {
        capture: Some(RenderState::new().unwrap()),
        ..Recorder::default()
    };
    let (mut terminal, seen) = terminal(recorder).unwrap();
    // the hold begins after "before"; "after" is drawn during it, in the same write
    terminal.vt_write(b"before\x1b[?2026hafter");
    let held = take(&seen);
    assert_eq!(
        held,
        [Seen::Hold(
            true,
            vec!["before".to_owned(), String::new(), String::new(), String::new()]
        )]
    );
    assert!(terminal.mode(Mode::SYNC_OUTPUT));
    terminal.vt_write(b"\x1b[?2026l");
    assert_eq!(take(&seen), [Seen::Hold(false, Vec::new())]);
    // setting the mode directly runs no callback
    terminal.set_mode(Mode::SYNC_OUTPUT, true).unwrap();
    terminal.set_mode(Mode::SYNC_OUTPUT, false).unwrap();
    assert!(take(&seen).is_empty());
}

#[test]
fn a_reset_ends_a_hold_and_reports_itself() {
    let (mut terminal, seen) = terminal(Recorder::default()).unwrap();
    terminal.vt_write(b"\x1b]2;title\x07\x1b[?2026h");
    take(&seen);
    terminal.vt_write(b"\x1bc");
    // a full reset also removes any progress report and every program status record, before its own callback
    assert_eq!(
        take(&seen),
        [
            Seen::Hold(false, Vec::new()),
            Seen::Progress(ProgressReport {
                state: ProgressState::Remove,
                progress: None,
            }),
            Seen::Status(ProgramStatus {
                state: ProgramStatusState::Clear,
                kind: ProgramStatusKind::None,
                progress: None,
                id: String::new(),
                app: String::new(),
                title: String::new(),
                message: String::new(),
            }),
            Seen::Reset,
        ]
    );
    assert_eq!(terminal.title(), "");
}

#[test]
fn colors_and_mouse_shape_follow_the_program() {
    let mut terminal = Terminal::new(10, 2).unwrap();
    let before = terminal.colors();
    terminal.vt_write(b"\x1b]11;rgb:12/34/56\x07\x1b]4;1;rgb:ff/00/00\x07\x1b]22;pointer\x07");
    let after = terminal.colors();
    assert_eq!(after.background, Some(Rgb::new(0x12, 0x34, 0x56)));
    assert_eq!(after.palette[1], Rgb::new(0xff, 0, 0));
    assert_ne!(before, after);
    assert_eq!(terminal.mouse_shape(), MouseShape::Pointer);
}

#[test]
fn default_colors_reach_the_render_state_until_a_program_overrides_them() {
    let mut terminal = Terminal::new(10, 2).unwrap();
    let mut state = RenderState::new().unwrap();
    let mut palette = *terminal.default_palette();
    palette[1] = Rgb::new(1, 1, 1);
    terminal
        .set_default_colors(Some(Rgb::new(0xee, 0xee, 0xee)), Some(Rgb::new(0x11, 0x11, 0x11)), None)
        .unwrap();
    terminal.set_default_palette(Some(&palette)).unwrap();
    state.update(&mut terminal).unwrap();
    let colors = state.colors().unwrap();
    assert_eq!(colors.background, Rgb::new(0x11, 0x11, 0x11));
    assert_eq!(colors.foreground, Rgb::new(0xee, 0xee, 0xee));
    assert_eq!(colors.cursor, None);
    assert_eq!(colors.palette[1], Rgb::new(1, 1, 1));
    terminal.vt_write(b"\x1b]11;rgb:22/22/22\x07");
    state.update(&mut terminal).unwrap();
    assert_eq!(state.colors().unwrap().background, Rgb::new(0x22, 0x22, 0x22));
    assert_eq!(terminal.default_palette()[1], Rgb::new(1, 1, 1));
    terminal.set_default_palette(None).unwrap();
    assert_ne!(terminal.default_palette()[1], Rgb::new(1, 1, 1));
}

#[test]
fn scrollback_limits_round_trip() {
    let mut terminal = Terminal::new(10, 2).unwrap();
    let limits = ScrollbackLimits {
        max_bytes: Some(1_000_000),
        max_lines: Some(5_000),
    };
    terminal.set_scrollback_limits(limits).unwrap();
    assert_eq!(terminal.scrollback_limits(), limits);
    terminal.set_scrollback_limits(ScrollbackLimits::default()).unwrap();
    assert_eq!(terminal.scrollback_limits(), ScrollbackLimits::default());
}

#[test]
fn replacing_the_effects_drops_the_old_handler() {
    let (mut terminal, old) = terminal(Recorder::default()).unwrap();
    let fresh = Recorder::default();
    let new = Arc::clone(&fresh.seen);
    terminal.set_effects(Box::new(fresh)).unwrap();
    assert_eq!(Arc::strong_count(&old), 1);
    terminal.vt_write(b"\x07");
    assert_eq!(take(&new), [Seen::Bell]);
}
