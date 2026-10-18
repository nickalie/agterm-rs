use crate::terminal::osc::Parser;

#[test]
fn osc_777_show_desktop_notification_with_title() {
    let mut p: Parser = Parser::new(false);

    let input = "777;notify;Title;Body";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(
        cmd,
        crate::terminal::osc::Command::ShowDesktopNotification { .. }
    ));
    match cmd {
        crate::terminal::osc::Command::ShowDesktopNotification { title, body } => {
            assert_eq!(title, b"Title");
            assert_eq!(body, b"Body");
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_777_unknown_extension_logs() {
    let mut p: Parser = Parser::new(false);
    let input = "777;bogus;whatever";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cap = crate::log_capture::start();
    assert!(p.end(Some(0x1b)).is_none());
    let messages = cap.messages();
    assert!(
        messages
            .iter()
            .any(|(level, target, msg)| *level == tracing::Level::WARN
                && target == "osc_rxvt_extension"
                && *msg == "unknown rxvt extension: bogus"),
        "expected the unknown-rxvt-extension diagnostic, got: {messages:?}"
    );
    drop(cap);
}

#[test]
fn osc_777_notify_missing_title_logs() {
    let mut p: Parser = Parser::new(false);
    let input = "777;notify;TitleOnlyNoSecondSemicolon";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cap = crate::log_capture::start();
    assert!(p.end(Some(0x1b)).is_none());
    let messages = cap.messages();
    assert!(
        messages
            .iter()
            .any(|(level, target, msg)| *level == tracing::Level::WARN
                && target == "osc_rxvt_extension"
                && *msg == "rxvt notify extension is missing the title"),
        "expected the missing-title diagnostic, got: {messages:?}"
    );
    drop(cap);
}
