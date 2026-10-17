//! The pane against real PTYs and real children: output reaches the screen, effects become events in stream order,
//! clipboard reads are answered through the PTY, the exit status and resizes reach their ends, and the error paths
//! (spawn failure, a closed PTY, a full input queue) report instead of hanging; a password prompt shows in the PTY's
//! modes.

mod support;

use std::time::{Duration, Instant};

use agterm_term::{
    ClipboardAnswer, ClipboardContent, ClipboardLocation, ClipboardWrite, ColorScheme, GridSize, PaneCommand,
    PaneConfig, PaneError, PaneEvent, Progress, ReadScope, Rgb,
};
use support::{SIZE, TestResult, spawn, spawn_with, wait_events, wait_file, wait_file_containing, wait_text};

fn exited(events: &[PaneEvent]) -> bool {
    events.iter().any(|event| matches!(event, PaneEvent::Exited(_)))
}

#[test]
fn printf_output_reaches_the_screen() {
    let fixture = spawn("printf 'hello\\r\\nworld'; exec sleep 5").unwrap();
    let frame = wait_text(&fixture.pane, "world");
    assert_eq!(frame.lines()[..2], ["hello", "world"]);
    assert_eq!(frame.cols, SIZE.cols);
    assert_eq!(frame.rows.len(), usize::from(SIZE.rows));
}

#[test]
fn osc_sequences_and_bel_become_events_in_stream_order() {
    let fixture = spawn(concat!(
        "printf '\\033]0;zero\\007\\033]2;my title\\007\\033]7;file://host/tmp/x\\033\\\\'; ",
        "printf '\\033]9;hello\\007\\033]777;notify;T;B\\007\\033]9;4;1;50\\007\\007'; ",
        "printf '\\033]52;p;Y29weQ==\\007'"
    ))
    .unwrap();
    let events: Vec<_> = wait_events(&fixture.pane, exited)
        .into_iter()
        .filter(|event| !matches!(event, PaneEvent::Exited(_)))
        .collect();
    assert_eq!(
        events,
        [
            PaneEvent::Title("zero".to_owned()),
            PaneEvent::Title("my title".to_owned()),
            PaneEvent::Pwd("file://host/tmp/x".to_owned()),
            PaneEvent::Notification {
                title: String::new(),
                body: "hello".to_owned(),
            },
            PaneEvent::Notification {
                title: "T".to_owned(),
                body: "B".to_owned(),
            },
            PaneEvent::Progress(Progress::Set(50)),
            PaneEvent::Bell,
            PaneEvent::ClipboardWrite(ClipboardWrite {
                location: ClipboardLocation::Primary,
                contents: vec![ClipboardContent {
                    mime: "text/plain".to_owned(),
                    data: b"copy".to_vec(),
                }],
            }),
        ]
    );
}

#[test]
fn a_color_change_is_reported_once() {
    let fixture = spawn("printf '\\033]11;rgb:12/34/56\\007'; printf '\\033]11;rgb:12/34/56\\007'").unwrap();
    let colors: Vec<_> = wait_events(&fixture.pane, exited)
        .into_iter()
        .filter_map(|event| match event {
            PaneEvent::Colors(colors) => Some(colors),
            _ => None,
        })
        .collect();
    assert_eq!(colors.len(), 1, "{colors:?}");
    assert_eq!(colors[0].background, Some(Rgb::new(0x12, 0x34, 0x56)));
}

/// Run a program that sends `request` in raw mode and saves the next `reply_len` bytes it reads; answer its read
/// with `answer` and return what it read.
fn clipboard_round_trip(request: &str, answer: ClipboardAnswer, reply_len: usize) -> TestResult<(ReadScope, Vec<u8>)> {
    let fixture = spawn(&format!(
        "stty raw -echo; printf '{request}'; dd bs=1 count={reply_len} of=\"$DIR/reply\" 2>/dev/null; printf done; exec sleep 5"
    ))?;
    let events = wait_events(&fixture.pane, |events| {
        events.iter().any(|event| matches!(event, PaneEvent::ClipboardRead(_)))
    });
    let read = events
        .iter()
        .find_map(|event| match event {
            PaneEvent::ClipboardRead(read) => Some(*read),
            _ => None,
        })
        .ok_or("no clipboard read")?;
    fixture.pane.answer_clipboard_read(read.id + 1, ClipboardAnswer::Denied);
    fixture.pane.answer_clipboard_read(read.id, answer);
    let reply = wait_file(&fixture.path("reply"), reply_len);
    wait_text(&fixture.pane, "done");
    Ok((read.scope, reply))
}

#[test]
fn an_osc_52_read_is_answered_with_the_clipboard_text_and_its_terminator() {
    let text = || ClipboardAnswer::Text(Some("abc".to_owned()));
    // "abc" is YWJj; the reply ends the way the request did (an unknown id is ignored first)
    let (scope, reply) = clipboard_round_trip("\\033]52;c;?\\007", text(), 12).unwrap();
    assert_eq!(scope, ReadScope::Data);
    assert_eq!(reply, b"\x1b]52;c;YWJj\x07");
    let (_, reply) = clipboard_round_trip("\\033]52;c;?\\033\\\\", text(), 13).unwrap();
    assert_eq!(reply, b"\x1b]52;c;YWJj\x1b\\");
}

#[test]
fn a_denied_or_empty_osc_52_read_answers_an_empty_clipboard() {
    let (_, reply) = clipboard_round_trip("\\033]52;c;?\\007", ClipboardAnswer::Denied, 8).unwrap();
    assert_eq!(reply, b"\x1b]52;c;\x07");
    let (_, reply) = clipboard_round_trip("\\033]52;c;?\\007", ClipboardAnswer::Text(None), 8).unwrap();
    assert_eq!(reply, b"\x1b]52;c;\x07");
}

#[test]
fn a_kitty_listing_needs_no_consent_and_is_answered() {
    // "." in base64: the available types only; the reply is the whole OK, listing, DONE sequence
    let fixture = spawn(concat!(
        "stty raw -echo; printf '\\033]5522;type=read;Lg==\\033\\\\'; ",
        "timeout --foreground 10 cat > \"$DIR/reply\"; exec sleep 5"
    ))
    .unwrap();
    let events = wait_events(&fixture.pane, |events| {
        events.iter().any(|event| matches!(event, PaneEvent::ClipboardRead(_)))
    });
    let Some(PaneEvent::ClipboardRead(read)) = events.last() else {
        unreachable!("waited for it")
    };
    assert_eq!(read.scope, ReadScope::ListOnly);
    fixture
        .pane
        .answer_clipboard_read(read.id, ClipboardAnswer::Text(Some("abc".to_owned())));
    let reply = wait_file_containing(&fixture.path("reply"), b"status=DONE");
    let reply = String::from_utf8_lossy(&reply);
    assert!(reply.starts_with("\x1b]5522;"), "{reply:?}");
    assert!(reply.contains("status=OK"), "{reply:?}");
    // the listing: "text/plain\n" in base64
    assert!(reply.contains("dGV4dC9wbGFpbgo="), "{reply:?}");
    // no data was served to a listing, and nothing was refused
    assert!(!reply.contains("YWJj") && !reply.contains("EPERM"), "{reply:?}");
}

#[test]
fn a_read_still_waiting_when_the_child_exits_is_denied_and_the_output_after_it_shown() {
    let fixture = spawn("printf '\\033]52;c;?\\007'; printf 'after the read'; exit 0").unwrap();
    // nobody answers the read: the exit releases it
    let events = wait_events(&fixture.pane, exited);
    assert!(events.iter().any(|event| matches!(event, PaneEvent::ClipboardRead(_))));
    let frame = fixture.pane.frame().frame;
    assert!(frame.lines()[0].contains("after the read"), "{:?}", frame.lines());
}

#[test]
fn replies_keep_their_order_around_a_parked_read() {
    // DA1, a clipboard read, DA1: the replies arrive in request order
    let (_, reply) = clipboard_round_trip(
        "\\033[c\\033]52;c;?\\007\\033[c",
        ClipboardAnswer::Text(Some("abc".to_owned())),
        12 + 12 + 12,
    )
    .unwrap();
    assert_eq!(reply, b"\x1b[?62;22;52c\x1b]52;c;YWJj\x07\x1b[?62;22;52c");
}

#[test]
fn dropping_a_pane_parked_on_a_read_does_not_hang() {
    let fixture = spawn("printf '\\033]52;c;?\\007'; exec sleep 30").unwrap();
    wait_events(&fixture.pane, |events| {
        events.iter().any(|event| matches!(event, PaneEvent::ClipboardRead(_)))
    });
    let started = Instant::now();
    drop(fixture.pane);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn the_exit_status_is_reported_after_the_last_output() {
    let fixture = spawn("printf 'last words'; exit 3").unwrap();
    let events = wait_events(&fixture.pane, exited);
    assert_eq!(
        events.last(),
        Some(&PaneEvent::Exited(agterm_term::ExitStatus {
            code: 3,
            signal: None,
            held: false,
        }))
    );
    wait_text(&fixture.pane, "last words");

    let killed = spawn("kill -KILL $$").unwrap();
    let events = wait_events(&killed.pane, exited);
    let Some(PaneEvent::Exited(status)) = events.last() else {
        unreachable!("waited for it")
    };
    assert!(status.signal.is_some(), "{status:?}");
}

#[test]
fn a_resize_reaches_the_child() {
    let fixture = spawn("stty size; read _; stty size; exec sleep 5").unwrap();
    wait_text(&fixture.pane, "24 80");
    let size = GridSize {
        cols: 100,
        rows: 30,
        ..SIZE
    };
    fixture.pane.resize(size).unwrap();
    fixture.pane.write(b"\n").unwrap();
    let frame = wait_text(&fixture.pane, "30 100");
    assert_eq!(frame.cols, 100);
    assert_eq!(frame.rows.len(), 30);
    assert!(fixture.pane.resize(GridSize { cols: 0, ..size }).is_err());
}

#[test]
fn a_program_that_cannot_start_is_a_spawn_error() {
    let dir = tempfile::tempdir().unwrap();
    let command = PaneCommand::new(dir.path().join("no-such-program")).cwd(dir.path());
    let error = agterm_term::Pane::spawn(&command, &PaneConfig::new(SIZE)).unwrap_err();
    assert!(matches!(error, PaneError::Spawn(_)), "{error:?}");
}

#[test]
fn writing_to_a_pane_whose_child_is_gone_reports_closed() {
    let fixture = spawn("exit 0").unwrap();
    wait_events(&fixture.pane, exited);
    assert_eq!(fixture.pane.write(b"x"), Err(PaneError::Closed));
}

#[test]
fn a_child_that_exits_with_input_pending_releases_the_writer() {
    let config = PaneConfig {
        write_limit: 4096,
        ..PaneConfig::new(SIZE)
    };
    // the child reads nothing, so the writer blocks on a full pty, until the child goes
    let fixture = spawn_with(
        "stty raw -echo; printf ready; read -r _ < /dev/null; exec sleep 1",
        &config,
    )
    .unwrap();
    wait_text(&fixture.pane, "ready");
    while fixture.pane.write(&[b'x'; 1024]).is_ok() {}
    wait_events(&fixture.pane, exited);
    let deadline = Instant::now() + support::DEADLINE;
    while fixture.pane.write(b"x") != Err(PaneError::Closed) {
        assert!(Instant::now() < deadline, "the write never failed");
        std::thread::sleep(Duration::from_millis(5));
    }
    let started = Instant::now();
    drop(fixture);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn input_beyond_the_queue_limit_is_refused_while_the_child_does_not_read() {
    let config = PaneConfig {
        write_limit: 4096,
        ..PaneConfig::new(SIZE)
    };
    let fixture = spawn_with("stty raw -echo; printf ready; exec sleep 30", &config).unwrap();
    wait_text(&fixture.pane, "ready");
    let chunk = [b'x'; 1024];
    let deadline = Instant::now() + support::DEADLINE;
    loop {
        match fixture.pane.write(&chunk) {
            Ok(()) => {}
            Err(PaneError::Backpressure) => break,
            Err(other) => panic!("{other:?}"),
        }
        assert!(Instant::now() < deadline, "the queue never filled");
    }
    assert!(fixture.pane.queued_input() <= 4096);
}

/// Polls `password_input` until it is `expected`.
fn wait_password_input(fixture: &support::Fixture, expected: bool) {
    let deadline = Instant::now() + support::DEADLINE;
    while fixture.pane.password_input() != expected {
        assert!(Instant::now() < deadline, "password_input never became {expected}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_canonical_read_without_echo_is_password_input() {
    let fixture = spawn(concat!(
        "printf 'user:'; read user; stty -echo; printf 'pw:'; read pw; stty echo; printf done; ",
        "exec sleep 5"
    ))
    .unwrap();
    wait_text(&fixture.pane, "user:");
    assert!(!fixture.pane.password_input(), "an echoed line is not a password");
    fixture.pane.write(b"me\n").unwrap();
    wait_text(&fixture.pane, "pw:");
    wait_password_input(&fixture, true);
    fixture.pane.write(b"secret\n").unwrap();
    wait_text(&fixture.pane, "done");
    wait_password_input(&fixture, false);
}

#[test]
fn raw_input_without_echo_is_not_password_input() {
    let fixture = spawn("stty raw -echo; printf ready; exec sleep 5").unwrap();
    wait_text(&fixture.pane, "ready");
    assert!(
        !fixture.pane.password_input(),
        "a full-screen program reads raw keys, not a password"
    );
}

#[test]
fn color_scheme_queries_are_answered_and_mode_2031_gets_a_report_on_a_change_only() {
    let mut config = PaneConfig::new(SIZE);
    config.color_scheme = ColorScheme::Dark;
    let fixture = spawn_with(
        concat!(
            "stty raw -echo; printf '\\033[?996n'; dd bs=1 count=9 of=\"$DIR/query\" 2>/dev/null; ",
            "while [ ! -e \"$DIR/go\" ]; do sleep 0.02; done; ",
            "printf '\\033[?2031h'; printf ready; dd bs=1 count=9 of=\"$DIR/report\" 2>/dev/null; printf done; ",
            "exec sleep 5"
        ),
        &config,
    )
    .unwrap();
    assert_eq!(
        wait_file(&fixture.path("query"), 9),
        b"\x1b[?997;1n",
        "the query answers dark"
    );
    assert_eq!(fixture.pane.color_scheme(), ColorScheme::Dark);
    // without mode 2031 a change is only remembered: a report of it would read light, not the dark one expected
    fixture.pane.set_color_scheme(ColorScheme::Light);
    assert_eq!(fixture.pane.color_scheme(), ColorScheme::Light);
    std::fs::write(fixture.path("go"), b"").unwrap();
    wait_text(&fixture.pane, "ready");
    // the same scheme again is no change (a report of it would read light too)
    fixture.pane.set_color_scheme(ColorScheme::Light);
    fixture.pane.set_color_scheme(ColorScheme::Dark);
    assert_eq!(fixture.pane.color_scheme(), ColorScheme::Dark);
    assert_eq!(
        wait_file(&fixture.path("report"), 9),
        b"\x1b[?997;1n",
        "one report, for the change made with mode 2031 on"
    );
    wait_text(&fixture.pane, "done");
}
