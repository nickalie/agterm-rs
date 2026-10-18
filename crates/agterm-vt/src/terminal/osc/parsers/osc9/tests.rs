use crate::terminal::osc::{Command, ConemuChangeTabTitle, Parser, ProgressReportState};

#[test]
fn osc_9_numeric_fields_reject_digit_separators() {
    let mut sleep: Parser = Parser::new(false);
    for ch in "9;1;4_2".bytes() {
        sleep.next(ch);
    }
    match sleep.end(None).unwrap() {
        Command::ConemuSleep { duration_ms } => assert_eq!(100, duration_ms),
        other => panic!("expected conemu_sleep, got {other:?}"),
    }

    let mut progress: Parser = Parser::new(false);
    for ch in "9;4;1;4_2".bytes() {
        progress.next(ch);
    }
    match progress.end(None).unwrap() {
        Command::ConemuProgressReport(r) => assert_eq!(None, r.progress),
        other => panic!("expected conemu_progress_report, got {other:?}"),
    }
}

#[test]
fn osc_9_show_desktop_notification() {
    let mut p: Parser = Parser::new(false);

    let input = "9;Hello world";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ShowDesktopNotification { .. }));
    match cmd {
        Command::ShowDesktopNotification { title, body } => {
            assert_eq!(b"", title);
            assert_eq!(b"Hello world", body);
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_show_single_character_desktop_notification() {
    let mut p: Parser = Parser::new(false);

    let input = "9;H";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ShowDesktopNotification { .. }));
    match cmd {
        Command::ShowDesktopNotification { title, body } => {
            assert_eq!(b"", title);
            assert_eq!(b"H", body);
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_1_conemu_sleep() {
    let mut p: Parser = Parser::new(false);

    let input = "9;1;420";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();

    assert!(matches!(cmd, Command::ConemuSleep { .. }));
    match cmd {
        Command::ConemuSleep { duration_ms } => assert_eq!(420, duration_ms),
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_1_conemu_sleep_with_no_value_default_to_100ms() {
    let mut p: Parser = Parser::new(false);

    let input = "9;1;";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();

    assert!(matches!(cmd, Command::ConemuSleep { .. }));
    match cmd {
        Command::ConemuSleep { duration_ms } => assert_eq!(100, duration_ms),
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_1_conemu_sleep_cannot_exceed_10000ms() {
    let mut p: Parser = Parser::new(false);

    let input = "9;1;12345";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();

    assert!(matches!(cmd, Command::ConemuSleep { .. }));
    match cmd {
        Command::ConemuSleep { duration_ms } => assert_eq!(10000, duration_ms),
        _ => unreachable!(),
    }
}

// No Ghostty test: the sleep duration and the progress
// value are parsed with `lib.parseInt` (`73768913b`), which rejects any
// sign byte on these unsigned types and `_` digit separators (the
// `std.fmt.parseUnsigned` it replaced read `4_20` as 420 and `4_2` as 42).
#[test]
fn osc_9_numbers_use_zig_parse_unsigned() {
    fn parse(input: &str) -> Command<'static> {
        let mut p: Parser = Parser::new(false);
        for ch in input.bytes() {
            p.next(ch);
        }
        match p.end(Some(0x1b)).unwrap() {
            Command::ConemuSleep { duration_ms } => Command::ConemuSleep { duration_ms },
            Command::ConemuProgressReport(r) => Command::ConemuProgressReport(r),
            other => panic!("unexpected command {other:?}"),
        }
    }
    match parse("9;1;420") {
        Command::ConemuSleep { duration_ms } => assert_eq!(420, duration_ms),
        other => panic!("expected sleep, got {other:?}"),
    }
    match parse("9;1;4_20") {
        Command::ConemuSleep { duration_ms } => assert_eq!(100, duration_ms),
        other => panic!("expected sleep, got {other:?}"),
    }
    match parse("9;1;+420") {
        Command::ConemuSleep { duration_ms } => assert_eq!(100, duration_ms),
        other => panic!("expected sleep, got {other:?}"),
    }
    match parse("9;4;1;42") {
        Command::ConemuProgressReport(r) => assert_eq!(Some(42), r.progress),
        other => panic!("expected progress, got {other:?}"),
    }
    match parse("9;4;1;4_2") {
        Command::ConemuProgressReport(r) => assert_eq!(None, r.progress),
        other => panic!("expected progress, got {other:?}"),
    }
    match parse("9;4;1;+42") {
        Command::ConemuProgressReport(r) => assert_eq!(None, r.progress),
        other => panic!("expected progress, got {other:?}"),
    }
}

#[test]
fn osc_9_1_conemu_sleep_invalid_input() {
    let mut p: Parser = Parser::new(false);

    let input = "9;1;foo";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();

    assert!(matches!(cmd, Command::ConemuSleep { .. }));
    match cmd {
        Command::ConemuSleep { duration_ms } => assert_eq!(100, duration_ms),
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_1_conemu_sleep_to_desktop_notification_1() {
    let mut p: Parser = Parser::new(false);

    let input = "9;1";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();

    assert!(matches!(cmd, Command::ShowDesktopNotification { .. }));
    match cmd {
        Command::ShowDesktopNotification { body, .. } => assert_eq!(b"1", body),
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_1_conemu_sleep_to_desktop_notification_2() {
    let mut p: Parser = Parser::new(false);

    let input = "9;1a";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();

    assert!(matches!(cmd, Command::ShowDesktopNotification { .. }));
    match cmd {
        Command::ShowDesktopNotification { body, .. } => assert_eq!(b"1a", body),
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_2_conemu_message_box() {
    let mut p: Parser = Parser::new(false);

    let input = "9;2;hello world";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ConemuShowMessageBox(_)));
    match cmd {
        Command::ConemuShowMessageBox(s) => assert_eq!(b"hello world", s),
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_2_conemu_message_box_invalid_input() {
    let mut p: Parser = Parser::new(false);

    let input = "9;2";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ShowDesktopNotification { .. }));
    match cmd {
        Command::ShowDesktopNotification { body, .. } => assert_eq!(b"2", body),
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_2_conemu_message_box_empty_message() {
    let mut p: Parser = Parser::new(false);

    let input = "9;2;";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ConemuShowMessageBox(_)));
    match cmd {
        Command::ConemuShowMessageBox(s) => assert_eq!(b"", s),
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_2_conemu_message_box_spaces_only_message() {
    let mut p: Parser = Parser::new(false);

    let input = "9;2;   ";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ConemuShowMessageBox(_)));
    match cmd {
        Command::ConemuShowMessageBox(s) => assert_eq!(b"   ", s),
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_2_message_box_to_desktop_notification_1() {
    let mut p: Parser = Parser::new(false);

    let input = "9;2";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();

    assert!(matches!(cmd, Command::ShowDesktopNotification { .. }));
    match cmd {
        Command::ShowDesktopNotification { body, .. } => assert_eq!(b"2", body),
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_2_message_box_to_desktop_notification_2() {
    let mut p: Parser = Parser::new(false);

    let input = "9;2a";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();

    assert!(matches!(cmd, Command::ShowDesktopNotification { .. }));
    match cmd {
        Command::ShowDesktopNotification { body, .. } => assert_eq!(b"2a", body),
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_3_conemu_change_tab_title() {
    let mut p: Parser = Parser::new(false);

    let input = "9;3;foo bar";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ConemuChangeTabTitle(_)));
    match cmd {
        Command::ConemuChangeTabTitle(ConemuChangeTabTitle::Value(v)) => {
            assert_eq!(b"foo bar", v);
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_3_conemu_change_tab_title_reset() {
    let mut p: Parser = Parser::new(false);

    let input = "9;3;";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();

    let expected_command: Command = Command::ConemuChangeTabTitle(ConemuChangeTabTitle::Reset);
    assert_eq!(expected_command, cmd);
}

#[test]
fn osc_9_3_conemu_change_tab_title_spaces_only() {
    let mut p: Parser = Parser::new(false);

    let input = "9;3;   ";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();

    assert!(matches!(cmd, Command::ConemuChangeTabTitle(_)));
    match cmd {
        Command::ConemuChangeTabTitle(ConemuChangeTabTitle::Value(v)) => {
            assert_eq!(b"   ", v);
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_3_change_tab_title_to_desktop_notification_1() {
    let mut p: Parser = Parser::new(false);

    let input = "9;3";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();

    assert!(matches!(cmd, Command::ShowDesktopNotification { .. }));
    match cmd {
        Command::ShowDesktopNotification { body, .. } => assert_eq!(b"3", body),
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_3_message_box_to_desktop_notification_2() {
    let mut p: Parser = Parser::new(false);

    let input = "9;3a";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();

    assert!(matches!(cmd, Command::ShowDesktopNotification { .. }));
    match cmd {
        Command::ShowDesktopNotification { body, .. } => assert_eq!(b"3a", body),
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_4_conemu_progress_set() {
    let mut p: Parser = Parser::new(false);

    let input = "9;4;1;100";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ConemuProgressReport(_)));
    match cmd {
        Command::ConemuProgressReport(r) => {
            assert_eq!(r.state, ProgressReportState::Set);
            assert_eq!(r.progress, Some(100));
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_4_conemu_progress_set_overflow() {
    let mut p: Parser = Parser::new(false);

    let input = "9;4;1;900";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ConemuProgressReport(_)));
    match cmd {
        Command::ConemuProgressReport(r) => {
            assert_eq!(r.state, ProgressReportState::Set);
            assert_eq!(Some(100), r.progress);
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_4_conemu_progress_set_single_digit() {
    let mut p: Parser = Parser::new(false);

    let input = "9;4;1;9";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ConemuProgressReport(_)));
    match cmd {
        Command::ConemuProgressReport(r) => {
            assert_eq!(r.state, ProgressReportState::Set);
            assert_eq!(r.progress, Some(9));
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_4_conemu_progress_set_double_digit() {
    let mut p: Parser = Parser::new(false);

    let input = "9;4;1;94";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ConemuProgressReport(_)));
    match cmd {
        Command::ConemuProgressReport(r) => {
            assert_eq!(r.state, ProgressReportState::Set);
            assert_eq!(Some(94), r.progress);
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_4_conemu_progress_set_extra_semicolon_ignored() {
    let mut p: Parser = Parser::new(false);

    let input = "9;4;1;100";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ConemuProgressReport(_)));
    match cmd {
        Command::ConemuProgressReport(r) => {
            assert_eq!(r.state, ProgressReportState::Set);
            assert_eq!(Some(100), r.progress);
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_4_conemu_progress_remove_with_no_progress() {
    let mut p: Parser = Parser::new(false);

    let input = "9;4;0;";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ConemuProgressReport(_)));
    match cmd {
        Command::ConemuProgressReport(r) => {
            assert_eq!(r.state, ProgressReportState::Remove);
            assert!(r.progress.is_none());
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_4_conemu_progress_remove_with_double_semicolon() {
    let mut p: Parser = Parser::new(false);

    let input = "9;4;0;;";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ConemuProgressReport(_)));
    match cmd {
        Command::ConemuProgressReport(r) => {
            assert_eq!(r.state, ProgressReportState::Remove);
            assert!(r.progress.is_none());
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_4_conemu_progress_remove_ignores_progress() {
    let mut p: Parser = Parser::new(false);

    let input = "9;4;0;100";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ConemuProgressReport(_)));
    match cmd {
        Command::ConemuProgressReport(r) => {
            assert_eq!(r.state, ProgressReportState::Remove);
            assert!(r.progress.is_none());
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_4_conemu_progress_remove_extra_semicolon() {
    let mut p: Parser = Parser::new(false);

    let input = "9;4;0;100;";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ConemuProgressReport(_)));
    match cmd {
        Command::ConemuProgressReport(r) => {
            assert_eq!(r.state, ProgressReportState::Remove);
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_4_conemu_progress_error() {
    let mut p: Parser = Parser::new(false);

    let input = "9;4;2";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ConemuProgressReport(_)));
    match cmd {
        Command::ConemuProgressReport(r) => {
            assert_eq!(r.state, ProgressReportState::Error);
            assert!(r.progress.is_none());
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_4_conemu_progress_error_with_progress() {
    let mut p: Parser = Parser::new(false);

    let input = "9;4;2;100";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ConemuProgressReport(_)));
    match cmd {
        Command::ConemuProgressReport(r) => {
            assert_eq!(r.state, ProgressReportState::Error);
            assert_eq!(r.progress, Some(100));
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_4_progress_pause() {
    let mut p: Parser = Parser::new(false);

    let input = "9;4;4";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ConemuProgressReport(_)));
    match cmd {
        Command::ConemuProgressReport(r) => {
            assert_eq!(r.state, ProgressReportState::Pause);
            assert!(r.progress.is_none());
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_4_conemu_progress_pause_with_progress() {
    let mut p: Parser = Parser::new(false);

    let input = "9;4;4;100";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ConemuProgressReport(_)));
    match cmd {
        Command::ConemuProgressReport(r) => {
            assert_eq!(r.state, ProgressReportState::Pause);
            assert_eq!(r.progress, Some(100));
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_4_progress_to_desktop_notification_1() {
    let mut p: Parser = Parser::new(false);

    let input = "9;4";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();

    assert!(matches!(cmd, Command::ShowDesktopNotification { .. }));
    match cmd {
        Command::ShowDesktopNotification { body, .. } => assert_eq!(b"4", body),
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_4_progress_to_desktop_notification_2() {
    let mut p: Parser = Parser::new(false);

    let input = "9;4;";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();

    assert!(matches!(cmd, Command::ShowDesktopNotification { .. }));
    match cmd {
        Command::ShowDesktopNotification { body, .. } => assert_eq!(b"4;", body),
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_4_progress_to_desktop_notification_3() {
    let mut p: Parser = Parser::new(false);

    let input = "9;4;5";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();

    assert!(matches!(cmd, Command::ShowDesktopNotification { .. }));
    match cmd {
        Command::ShowDesktopNotification { body, .. } => assert_eq!(b"4;5", body),
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_4_progress_to_desktop_notification_4() {
    let mut p: Parser = Parser::new(false);

    let input = "9;4;5a";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();

    assert!(matches!(cmd, Command::ShowDesktopNotification { .. }));
    match cmd {
        Command::ShowDesktopNotification { body, .. } => assert_eq!(b"4;5a", body),
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_5_conemu_wait_input() {
    let mut p: Parser = Parser::new(false);

    let input = "9;5";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ConemuWaitInput));
}

#[test]
fn osc_9_5_conemu_wait_ignores_trailing_characters() {
    let mut p: Parser = Parser::new(false);

    let input = "9;5;foo";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ConemuWaitInput));
}

#[test]
fn osc_9_6_conemu_guimacro_1() {
    let mut p: Parser = Parser::new(false);

    let input = "9;6;a";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ConemuGuimacro(_)));
    match cmd {
        Command::ConemuGuimacro(s) => assert_eq!(b"a", s),
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_6_conemu_guimacro_2() {
    let mut p: Parser = Parser::new(false);

    let input = "9;6;ab";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ConemuGuimacro(_)));
    match cmd {
        Command::ConemuGuimacro(s) => assert_eq!(b"ab", s),
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_6_conemu_guimacro_3_incomplete_to_desktop_notification() {
    let mut p: Parser = Parser::new(false);

    let input = "9;6";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ShowDesktopNotification { .. }));
    match cmd {
        Command::ShowDesktopNotification { body, .. } => assert_eq!(b"6", body),
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_7_conemu_run_process_1() {
    let mut p: Parser = Parser::new(false);

    let input = "9;7;ab";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ConemuRunProcess(_)));
    match cmd {
        Command::ConemuRunProcess(s) => assert_eq!(b"ab", s),
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_7_conemu_run_process_2() {
    let mut p: Parser = Parser::new(false);

    let input = "9;7;";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ConemuRunProcess(_)));
    match cmd {
        Command::ConemuRunProcess(s) => assert_eq!(b"", s),
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_7_conemu_run_process_incomplete_to_desktop_notification() {
    let mut p: Parser = Parser::new(false);

    let input = "9;7";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ShowDesktopNotification { .. }));
    match cmd {
        Command::ShowDesktopNotification { body, .. } => assert_eq!(b"7", body),
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_8_conemu_output_environment_variable_1() {
    let mut p: Parser = Parser::new(false);

    let input = "9;8;ab";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ConemuOutputEnvironmentVariable(_)));
    match cmd {
        Command::ConemuOutputEnvironmentVariable(s) => assert_eq!(b"ab", s),
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_8_conemu_output_environment_variable_2() {
    let mut p: Parser = Parser::new(false);

    let input = "9;8;";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ConemuOutputEnvironmentVariable(_)));
    match cmd {
        Command::ConemuOutputEnvironmentVariable(s) => assert_eq!(b"", s),
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_8_conemu_output_environment_variable_incomplete_to_desktop_notification() {
    let mut p: Parser = Parser::new(false);

    let input = "9;8";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ShowDesktopNotification { .. }));
    match cmd {
        Command::ShowDesktopNotification { body, .. } => assert_eq!(b"8", body),
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_9_conemu_set_current_working_directory() {
    let mut p: Parser = Parser::new(false);

    let input = "9;9;ab";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ReportPwd { .. }));
    match cmd {
        Command::ReportPwd { value } => assert_eq!(b"ab", value),
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_9_conemu_set_current_working_directory_incomplete_to_desktop_notification() {
    let mut p: Parser = Parser::new(false);

    let input = "9;9";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ShowDesktopNotification { .. }));
    match cmd {
        Command::ShowDesktopNotification { body, .. } => assert_eq!(b"9", body),
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_10_conemu_xterm_keyboard_and_output_emulation_1() {
    let mut p: Parser = Parser::new(false);

    let input = "9;10";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ConemuXtermEmulation { .. }));
    match cmd {
        Command::ConemuXtermEmulation { keyboard, output } => {
            assert!(keyboard.is_some());
            assert!(keyboard.unwrap());
            assert!(output.is_some());
            assert!(output.unwrap());
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_10_conemu_xterm_keyboard_and_output_emulation_2() {
    let mut p: Parser = Parser::new(false);

    let input = "9;10;0";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ConemuXtermEmulation { .. }));
    match cmd {
        Command::ConemuXtermEmulation { keyboard, output } => {
            assert!(keyboard.is_some());
            assert!(!keyboard.unwrap());
            assert!(output.is_some());
            assert!(!output.unwrap());
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_10_conemu_xterm_keyboard_and_output_emulation_3() {
    let mut p: Parser = Parser::new(false);

    let input = "9;10;1";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ConemuXtermEmulation { .. }));
    match cmd {
        Command::ConemuXtermEmulation { keyboard, output } => {
            assert!(keyboard.is_some());
            assert!(keyboard.unwrap());
            assert!(output.is_some());
            assert!(output.unwrap());
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_10_conemu_xterm_keyboard_and_output_emulation_4() {
    let mut p: Parser = Parser::new(false);

    let input = "9;10;2";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ConemuXtermEmulation { .. }));
    match cmd {
        Command::ConemuXtermEmulation { keyboard, output } => {
            assert!(keyboard.is_none());
            assert!(output.is_some());
            assert!(!output.unwrap());
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_10_conemu_xterm_keyboard_and_output_emulation_5() {
    let mut p: Parser = Parser::new(false);

    let input = "9;10;3";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ConemuXtermEmulation { .. }));
    match cmd {
        Command::ConemuXtermEmulation { keyboard, output } => {
            assert!(keyboard.is_none());
            assert!(output.is_some());
            assert!(output.unwrap());
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_10_conemu_xterm_keyboard_and_output_emulation_6() {
    let mut p: Parser = Parser::new(false);

    let input = "9;10;4";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ShowDesktopNotification { .. }));
    match cmd {
        Command::ShowDesktopNotification { body, .. } => assert_eq!(b"10;4", body),
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_10_conemu_xterm_keyboard_and_output_emulation_7() {
    let mut p: Parser = Parser::new(false);

    let input = "9;10;";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ShowDesktopNotification { .. }));
    match cmd {
        Command::ShowDesktopNotification { body, .. } => assert_eq!(b"10;", body),
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_10_conemu_xterm_keyboard_and_output_emulation_8() {
    let mut p: Parser = Parser::new(false);

    let input = "9;10;abc";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ShowDesktopNotification { .. }));
    match cmd {
        Command::ShowDesktopNotification { body, .. } => assert_eq!(b"10;abc", body),
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_11_conemu_comment() {
    let mut p: Parser = Parser::new(false);

    let input = "9;11;ab";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ConemuComment(_)));
    match cmd {
        Command::ConemuComment(s) => assert_eq!(b"ab", s),
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_11_conemu_comment_incomplete_to_desktop_notification() {
    let mut p: Parser = Parser::new(false);

    let input = "9;11";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::ShowDesktopNotification { .. }));
    match cmd {
        Command::ShowDesktopNotification { body, .. } => assert_eq!(b"11", body),
        _ => unreachable!(),
    }
}

#[test]
fn osc_9_12_conemu_mark_prompt_start_1() {
    let mut p: Parser = Parser::new(false);

    let input = "9;12";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::SemanticPrompt(_)));
}

#[test]
fn osc_9_12_conemu_mark_prompt_start_2() {
    let mut p: Parser = Parser::new(false);

    let input = "9;12;abc";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::SemanticPrompt(_)));
}
