use crate::terminal::osc::Parser;

#[test]
fn osc_22_pointer_cursor() {
    let mut p: Parser = Parser::new(false);

    let input = "22;pointer";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, crate::terminal::osc::Command::MouseShape { .. }));
    match cmd {
        crate::terminal::osc::Command::MouseShape { value } => assert_eq!(b"pointer", value),
        _ => unreachable!(),
    }
}
