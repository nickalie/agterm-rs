use crate::terminal::osc::{Command, Parser, Terminator};

#[test]
fn osc_72_metadata_only_no_payload() {
    let mut p: Parser = Parser::new(false);
    let input = "72;t=a";
    for ch in input.bytes() {
        p.next(ch);
    }
    let cmd = p.end(Some(0x1b)).unwrap();
    let Command::KittyDndProtocol(dnd) = cmd else {
        unreachable!()
    };
    assert_eq!(b"t=a".as_slice(), dnd.metadata);
    assert!(dnd.payload.is_none());
}

#[test]
fn osc_72_metadata_and_empty_payload() {
    let mut p: Parser = Parser::new(false);
    let input = "72;t=a;";
    for ch in input.bytes() {
        p.next(ch);
    }
    let cmd = p.end(Some(0x1b)).unwrap();
    let Command::KittyDndProtocol(dnd) = cmd else {
        unreachable!()
    };
    assert_eq!(b"t=a".as_slice(), dnd.metadata);
    assert_eq!(b"".as_slice(), dnd.payload.unwrap());
}

#[test]
fn osc_72_metadata_and_non_empty_payload() {
    let mut p: Parser = Parser::new(false);
    let input = "72;t=a:i=5;text/plain text/uri-list";
    for ch in input.bytes() {
        p.next(ch);
    }
    let cmd = p.end(Some(0x1b)).unwrap();
    let Command::KittyDndProtocol(dnd) = cmd else {
        unreachable!()
    };
    assert_eq!(b"t=a:i=5".as_slice(), dnd.metadata);
    assert_eq!(b"text/plain text/uri-list".as_slice(), dnd.payload.unwrap());
}

#[test]
fn osc_72_empty_metadata_with_payload() {
    let mut p: Parser = Parser::new(false);
    let input = "72;;payload";
    for ch in input.bytes() {
        p.next(ch);
    }
    let cmd = p.end(Some(0x1b)).unwrap();
    let Command::KittyDndProtocol(dnd) = cmd else {
        unreachable!()
    };
    assert_eq!(b"".as_slice(), dnd.metadata);
    assert_eq!(b"payload".as_slice(), dnd.payload.unwrap());
}

#[test]
fn osc_72_bel_terminator_recorded() {
    let mut p: Parser = Parser::new(false);
    let input = "72;t=q";
    for ch in input.bytes() {
        p.next(ch);
    }
    let cmd = p.end(Some(0x07)).unwrap();
    let Command::KittyDndProtocol(dnd) = cmd else {
        unreachable!()
    };
    assert_eq!(Terminator::Bel, dnd.terminator);
}
