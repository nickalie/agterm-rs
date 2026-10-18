use crate::terminal::osc::Parser;

#[test]
fn osc_1_change_window_icon() {
    let mut p: Parser = Parser::new(false);
    p.next(b'1');
    p.next(b';');
    p.next(b'a');
    p.next(b'b');
    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, crate::terminal::osc::Command::ChangeWindowIcon(_)));
    match cmd {
        crate::terminal::osc::Command::ChangeWindowIcon(v) => assert_eq!(b"ab", v),
        _ => unreachable!(),
    }
}
