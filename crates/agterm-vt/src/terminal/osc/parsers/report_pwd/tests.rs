use crate::terminal::osc::Parser;

#[test]
fn osc_7_report_pwd() {
    let mut p: Parser = Parser::new(false);

    let input = "7;file:///tmp/example";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, crate::terminal::osc::Command::ReportPwd { .. }));
    match cmd {
        crate::terminal::osc::Command::ReportPwd { value } => {
            assert_eq!(b"file:///tmp/example", value);
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_7_report_pwd_empty() {
    let mut p: Parser = Parser::new(false);

    let input = "7;";
    for ch in input.bytes() {
        p.next(ch);
    }
    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, crate::terminal::osc::Command::ReportPwd { .. }));
    match cmd {
        crate::terminal::osc::Command::ReportPwd { value } => assert_eq!(b"", value),
        _ => unreachable!(),
    }
}
