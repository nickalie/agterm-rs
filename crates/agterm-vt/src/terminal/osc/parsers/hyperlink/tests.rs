use crate::terminal::osc::{Command, Parser};

#[test]
fn osc_8_hyperlink() {
    let mut p: Parser = Parser::new(false);

    let input = "8;;http://example.com";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::HyperlinkStart { .. }));
    match cmd {
        Command::HyperlinkStart { uri, .. } => {
            assert_eq!(uri, b"http://example.com");
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_8_hyperlink_with_id_set() {
    let mut p: Parser = Parser::new(false);

    let input = "8;id=foo;http://example.com";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::HyperlinkStart { .. }));
    match cmd {
        Command::HyperlinkStart { id, uri } => {
            assert_eq!(id.unwrap(), b"foo");
            assert_eq!(uri, b"http://example.com");
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_8_hyperlink_with_empty_id() {
    let mut p: Parser = Parser::new(false);

    let input = "8;id=;http://example.com";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::HyperlinkStart { .. }));
    match cmd {
        Command::HyperlinkStart { id, uri } => {
            assert_eq!(id, None);
            assert_eq!(uri, b"http://example.com");
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_8_hyperlink_with_incomplete_key() {
    let mut p: Parser = Parser::new(false);

    let input = "8;id;http://example.com";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::HyperlinkStart { .. }));
    match cmd {
        Command::HyperlinkStart { id, uri } => {
            assert_eq!(id, None);
            assert_eq!(uri, b"http://example.com");
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_8_hyperlink_with_empty_key() {
    let mut p: Parser = Parser::new(false);

    let input = "8;=value;http://example.com";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::HyperlinkStart { .. }));
    match cmd {
        Command::HyperlinkStart { id, uri } => {
            assert_eq!(id, None);
            assert_eq!(uri, b"http://example.com");
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_8_hyperlink_with_empty_key_and_id() {
    let mut p: Parser = Parser::new(false);

    let input = "8;=value:id=foo;http://example.com";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::HyperlinkStart { .. }));
    match cmd {
        Command::HyperlinkStart { id, uri } => {
            assert_eq!(id.unwrap(), b"foo");
            assert_eq!(uri, b"http://example.com");
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_8_hyperlink_with_empty_uri() {
    let mut p: Parser = Parser::new(false);

    let input = "8;id=foo;";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b));
    assert!(cmd.is_none());
}

#[test]
fn osc_8_hyperlink_end() {
    let mut p: Parser = Parser::new(false);

    let input = "8;;";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::HyperlinkEnd));
}

// No Ghostty test: an option other than `id` logs the
// Ghostty's warning (hyperlink.zig:43) and the link still parses.
#[test]
fn osc_8_unknown_option_logs_a_warning() {
    let mut p: Parser = Parser::new(false);
    let cap = crate::log_capture::start();
    for ch in "8;foo=bar;http://example.com".bytes() {
        p.next(ch);
    }
    let cmd = p.end(Some(0x1b)).unwrap();
    let messages = cap.messages();
    drop(cap);
    assert!(matches!(cmd, Command::HyperlinkStart { .. }));
    assert_eq!(
        vec![(
            tracing::Level::WARN,
            "osc_hyperlink".to_string(),
            "unknown hyperlink option: 'foo'".to_string(),
        )],
        messages
    );
}
