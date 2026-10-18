use crate::terminal::osc::{Parser, Terminator};

#[test]
fn osc_52_get_set_clipboard() {
    let mut p: Parser = Parser::new(false);

    let input = "52;s;?";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, crate::terminal::osc::Command::ClipboardContents { .. }));
    match cmd {
        crate::terminal::osc::Command::ClipboardContents { kind, data, terminator } => {
            assert_eq!(kind, b's');
            assert_eq!(b"?", data);
            assert_eq!(Terminator::St, terminator);
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_52_get_clipboard_with_bel_terminator() {
    let mut p: Parser = Parser::new(false);

    let input = "52;c;?";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x07)).unwrap();
    assert!(matches!(cmd, crate::terminal::osc::Command::ClipboardContents { .. }));
    match cmd {
        crate::terminal::osc::Command::ClipboardContents { terminator, .. } => {
            assert_eq!(Terminator::Bel, terminator);
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_52_get_set_clipboard_optional_parameter() {
    let mut p: Parser = Parser::new(false);

    let input = "52;;?";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, crate::terminal::osc::Command::ClipboardContents { .. }));
    match cmd {
        crate::terminal::osc::Command::ClipboardContents { kind, data, .. } => {
            assert_eq!(kind, b'c');
            assert_eq!(b"?", data);
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_52_get_set_clipboard_with_allocator() {
    let mut p: Parser = Parser::new(true);

    let input = "52;s;?";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, crate::terminal::osc::Command::ClipboardContents { .. }));
    match cmd {
        crate::terminal::osc::Command::ClipboardContents { kind, data, .. } => {
            assert_eq!(kind, b's');
            assert_eq!(b"?", data);
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_52_clear_clipboard() {
    let mut p: Parser = Parser::new(false);

    let input = "52;;";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, crate::terminal::osc::Command::ClipboardContents { .. }));
    match cmd {
        crate::terminal::osc::Command::ClipboardContents { kind, data, .. } => {
            assert_eq!(kind, b'c');
            assert_eq!(b"", data);
        }
        _ => unreachable!(),
    }
}
