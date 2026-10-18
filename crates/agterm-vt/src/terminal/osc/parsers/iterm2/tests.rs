use crate::terminal::osc::{Command, Parser};

#[test]
fn osc_1337_test_valid_unimplemented_key_with_no_value() {
    let mut p: Parser = Parser::new(false);

    let input = "1337;SetBadgeFormat";
    for ch in input.bytes() {
        p.next(ch);
    }

    assert!(p.end(Some(b'\x1b')).is_none());
}

#[test]
fn osc_1337_test_valid_unimplemented_key_with_empty_value() {
    let mut p: Parser = Parser::new(false);

    let input = "1337;SetBadgeFormat=";
    for ch in input.bytes() {
        p.next(ch);
    }

    assert!(p.end(Some(b'\x1b')).is_none());
}

#[test]
fn osc_1337_test_valid_unimplemented_key_with_non_empty_value() {
    let mut p: Parser = Parser::new(false);

    let input = "1337;SetBadgeFormat=abc123";
    for ch in input.bytes() {
        p.next(ch);
    }

    assert!(p.end(Some(b'\x1b')).is_none());
}

#[test]
fn osc_1337_test_valid_key_with_lower_case_and_with_no_value() {
    let mut p: Parser = Parser::new(false);

    let input = "1337;setbadgeformat";
    for ch in input.bytes() {
        p.next(ch);
    }

    assert!(p.end(Some(b'\x1b')).is_none());
}

#[test]
fn osc_1337_test_valid_key_with_lower_case_and_with_empty_value() {
    let mut p: Parser = Parser::new(false);

    let input = "1337;setbadgeformat=";
    for ch in input.bytes() {
        p.next(ch);
    }

    assert!(p.end(Some(b'\x1b')).is_none());
}

#[test]
fn osc_1337_test_valid_key_with_lower_case_and_with_non_empty_value() {
    let mut p: Parser = Parser::new(false);

    let input = "1337;setbadgeformat=abc123";
    for ch in input.bytes() {
        p.next(ch);
    }

    assert!(p.end(Some(b'\x1b')).is_none());
}

#[test]
fn osc_1337_test_invalid_key_with_no_value() {
    let mut p: Parser = Parser::new(false);

    let input = "1337;BobrKurwa";
    for ch in input.bytes() {
        p.next(ch);
    }

    assert!(p.end(Some(b'\x1b')).is_none());
}

#[test]
fn osc_1337_test_invalid_key_with_empty_value() {
    let mut p: Parser = Parser::new(false);

    let input = "1337;BobrKurwa=";
    for ch in input.bytes() {
        p.next(ch);
    }

    assert!(p.end(Some(b'\x1b')).is_none());
}

#[test]
fn osc_1337_test_invalid_key_with_non_empty_value() {
    let mut p: Parser = Parser::new(false);

    let input = "1337;BobrKurwa=abc123";
    for ch in input.bytes() {
        p.next(ch);
    }

    assert!(p.end(Some(b'\x1b')).is_none());
}

#[test]
fn osc_1337_test_copy_with_no_value() {
    let mut p: Parser = Parser::new(false);

    let input = "1337;Copy";
    for ch in input.bytes() {
        p.next(ch);
    }

    assert!(p.end(Some(b'\x1b')).is_none());
}

#[test]
fn osc_1337_test_copy_with_empty_value() {
    let mut p: Parser = Parser::new(false);

    let input = "1337;Copy=";
    for ch in input.bytes() {
        p.next(ch);
    }

    assert!(p.end(Some(b'\x1b')).is_none());
}

#[test]
fn osc_1337_test_copy_with_only_prefix_colon() {
    let mut p: Parser = Parser::new(false);

    let input = "1337;Copy=:";
    for ch in input.bytes() {
        p.next(ch);
    }

    assert!(p.end(Some(b'\x1b')).is_none());
}

#[test]
fn osc_1337_test_copy_with_question_mark() {
    let mut p: Parser = Parser::new(false);

    let input = "1337;Copy=:?";
    for ch in input.bytes() {
        p.next(ch);
    }

    assert!(p.end(Some(b'\x1b')).is_none());
}

#[test]
#[ignore = "return error.SkipZigTest"]
fn osc_1337_test_copy_with_non_empty_value_that_is_invalid_base64() {
    // For performance reasons, we don't check for valid base64 data
    // right now.
}

#[test]
fn osc_1337_test_copy_with_non_empty_value_that_is_valid_base64_but_not_prefixed_with_a_colon() {
    let mut p: Parser = Parser::new(false);

    let input = "1337;Copy=YWJjMTIz";
    for ch in input.bytes() {
        p.next(ch);
    }

    assert!(p.end(Some(b'\x1b')).is_none());
}

#[test]
fn osc_1337_test_copy_with_non_empty_value_that_is_valid_base64() {
    let mut p: Parser = Parser::new(false);

    let input = "1337;Copy=:YWJjMTIz";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(b'\x1b')).unwrap();
    assert!(matches!(cmd, Command::ClipboardContents { .. }));
    match cmd {
        Command::ClipboardContents { kind, data, .. } => {
            assert_eq!(b'c', kind);
            assert_eq!(b"YWJjMTIz", data);
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_1337_test_current_dir_with_no_value() {
    let mut p: Parser = Parser::new(false);

    let input = "1337;CurrentDir";
    for ch in input.bytes() {
        p.next(ch);
    }

    assert!(p.end(Some(b'\x1b')).is_none());
}

#[test]
fn osc_1337_test_current_dir_with_empty_value() {
    let mut p: Parser = Parser::new(false);

    let input = "1337;CurrentDir=";
    for ch in input.bytes() {
        p.next(ch);
    }

    assert!(p.end(Some(b'\x1b')).is_none());
}

#[test]
fn osc_1337_test_current_dir_with_non_empty_value() {
    let mut p: Parser = Parser::new(false);

    let input = "1337;CurrentDir=abc123";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(b'\x1b')).unwrap();
    assert!(matches!(cmd, Command::ReportPwd { .. }));
    match cmd {
        Command::ReportPwd { value } => assert_eq!(b"abc123", value),
        _ => unreachable!(),
    }
}

// No Ghostty test: an unimplemented key logs Ghostty's
// debug line (iterm2.zig:192) with the key's tag name, whatever its case
// on the wire, and every Key has a tag name.
#[test]
fn osc_1337_unimplemented_key_logs_its_tag_name() {
    assert_eq!(36, super::KEYS.len());
    let mut p: Parser = Parser::new(false);
    let cap = crate::log_capture::start();
    for ch in "1337;setbadgeformat=abc".bytes() {
        p.next(ch);
    }
    assert!(p.end(Some(b'\x1b')).is_none());
    let messages = cap.messages();
    drop(cap);
    assert_eq!(
        vec![(
            tracing::Level::DEBUG,
            "osc_iterm2".to_string(),
            "unimplemented OSC 1337: SetBadgeFormat".to_string(),
        )],
        messages
    );
}
