use super::*;
use crate::mem::testing::with_counting;

fn parser() -> Parser {
    Parser::new(true)
}

/// `p.end('\x1b').?.program_status` (ST) as the report.
fn report(p: &mut Parser) -> Report<'_> {
    match p.end(Some(0x1b)) {
        Some(OscCommand::ProgramStatus(Command::Report(r))) => r,
        other => panic!("expected a report, got {other:?}"),
    }
}

fn str_opt<'a>(r: &Report<'a>, o: OptionKey) -> Option<&'a [u8]> {
    match r.read_option(o) {
        OptionValue::Str(v) => v,
        other => panic!("expected a string option, got {other:?}"),
    }
}

fn kind_opt(r: &Report<'_>) -> Option<Kind> {
    match r.read_option(OptionKey::Kind) {
        OptionValue::Kind(v) => v,
        other => panic!("expected kind, got {other:?}"),
    }
}

fn progress_opt(r: &Report<'_>) -> Option<u8> {
    match r.read_option(OptionKey::Progress) {
        OptionValue::Progress(v) => v,
        other => panic!("expected progress, got {other:?}"),
    }
}

fn text(r: &Report<'_>, o: OptionKey) -> Vec<u8> {
    let mut out = Vec::new();
    r.write_text(o, &mut out).unwrap();
    out
}

#[test]
fn osc_7501_query() {
    let mut p = parser();

    p.next_slice(b"7501;?");
    assert_eq!(
        Some(OscCommand::ProgramStatus(Command::Query(Terminator::St))),
        p.end(Some(0x1b))
    );

    p.reset();
    p.next_slice(b"7501;?");
    assert_eq!(
        Some(OscCommand::ProgramStatus(Command::Query(Terminator::Bel))),
        p.end(Some(0x07))
    );
}

#[test]
fn osc_7501_minimal_report() {
    let mut p = parser();

    p.next_slice(b"7501;state=idle");
    let r = report(&mut p);
    assert_eq!(State::Idle, r.state);
    assert_eq!(None, kind_opt(&r));
    assert_eq!(None, progress_opt(&r));
    assert_eq!(None, str_opt(&r, OptionKey::Id));
    assert_eq!(None, str_opt(&r, OptionKey::App));
    assert_eq!(None, str_opt(&r, OptionKey::Title));
    assert_eq!(None, str_opt(&r, OptionKey::Msg));
}

#[test]
fn osc_7501_all_keys() {
    let mut p = parser();

    // "Plan" and "Apply 3 to add, 1 to change, 0 to destroy?"
    p.next_slice(
        b"7501;state=blocked:kind=permission:progress=42:id=tf/plan:app=terraform\
:title=UGxhbg==:msg=QXBwbHkgMyB0byBhZGQsIDEgdG8gY2hhbmdlLCAwIHRvIGRlc3Ryb3k/",
    );
    let r = report(&mut p);
    assert_eq!(State::Blocked, r.state);
    assert_eq!(Some(Kind::Permission), kind_opt(&r));
    assert_eq!(Some(42), progress_opt(&r));
    assert_eq!(Some(&b"tf/plan"[..]), str_opt(&r, OptionKey::Id));
    assert_eq!(Some(&b"terraform"[..]), str_opt(&r, OptionKey::App));
    assert_eq!(b"Plan".to_vec(), text(&r, OptionKey::Title));
    assert_eq!(
        b"Apply 3 to add, 1 to change, 0 to destroy?".to_vec(),
        text(&r, OptionKey::Msg)
    );
}

#[test]
fn osc_7501_base64_padding_is_optional() {
    let mut p = parser();

    p.next_slice(b"7501;state=done:msg=UGxhbg");
    let r = report(&mut p);
    assert_eq!(b"Plan".to_vec(), text(&r, OptionKey::Msg));
}

#[test]
fn osc_7501_whitespace_around_keys_and_values() {
    let mut p = parser();

    p.next_slice(b"7501; state = working :\tapp=cargo\t");
    let r = report(&mut p);
    assert_eq!(State::Working, r.state);
    assert_eq!(Some(&b"cargo"[..]), str_opt(&r, OptionKey::App));
}

#[test]
fn osc_7501_last_value_wins() {
    let mut p = parser();

    p.next_slice(b"7501;state=idle:app=a:state=done:app=b");
    let r = report(&mut p);
    assert_eq!(State::Done, r.state);
    assert_eq!(Some(&b"b"[..]), str_opt(&r, OptionKey::App));
}

#[test]
fn osc_7501_malformed_pairs_and_unknown_keys_are_skipped() {
    let mut p = parser();

    p.next_slice(b"7501;garbage:=x:Bad=1:app=b@d:future=yes:state=working:app=ok:::");
    let r = report(&mut p);
    assert_eq!(State::Working, r.state);
    assert_eq!(Some(&b"ok"[..]), str_opt(&r, OptionKey::App));
}

#[test]
fn osc_7501_kind_and_progress_only_apply_to_some_states() {
    let mut p = parser();

    p.next_slice(b"7501;state=working:kind=auth:progress=10");
    let r = report(&mut p);
    assert_eq!(None, kind_opt(&r));
    assert_eq!(Some(10), progress_opt(&r));

    p.reset();
    p.next_slice(b"7501;state=done:kind=auth:progress=10");
    let r = report(&mut p);
    assert_eq!(None, kind_opt(&r));
    assert_eq!(None, progress_opt(&r));

    p.reset();
    p.next_slice(b"7501;state=blocked:kind=dance");
    let r = report(&mut p);
    assert_eq!(None, kind_opt(&r));
}

#[test]
fn osc_7501_out_of_range_progress_is_absent() {
    let mut p = parser();

    let cases: [(&[u8], Option<u8>); 11] = [
        (b"0", Some(0)),
        (b"100", Some(100)),
        (b"101", None),
        (b"256", None),
        (b"99999999999999999999", None),
        (b"-1", None),
        (b"+1", None),
        (b"4.5", None),
        (b"1_0", None),
        (b"007", Some(7)),
        (b"", None),
    ];

    for (input, want) in cases {
        p.reset();
        p.next_slice(b"7501;state=working:progress=");
        p.next_slice(input);
        let r = report(&mut p);
        assert_eq!(want, progress_opt(&r), "progress={input:?}");
    }
}

#[test]
fn osc_7501_invalid_app_is_ignored() {
    let mut p = parser();

    p.next_slice(b"7501;state=idle:app=a/b");
    let r = report(&mut p);
    assert_eq!(None, str_opt(&r, OptionKey::App));
}

#[test]
fn osc_7501_ids() {
    let mut p = parser();

    let max_segment = "a".repeat(MAX_ID_SEGMENT_BYTES);
    let valid = [
        "a".to_string(),
        "build/test".to_string(),
        "A-Z_0.9+".to_string(),
        max_segment.clone(),
        "a/b/c/d/e/f/g/h".to_string(),
    ];
    for id in &valid {
        p.reset();
        p.next_slice(b"7501;state=clear:id=");
        p.next_slice(id.as_bytes());
        let r = report(&mut p);
        assert_eq!(Some(id.as_bytes()), str_opt(&r, OptionKey::Id));
    }

    // An invalid id discards the report so that it can never fall back
    // to the root record.
    let invalid = [
        String::new(),
        "/".to_string(),
        "a/".to_string(),
        "/a".to_string(),
        "a//b".to_string(),
        "a,b".to_string(),
        "a=b".to_string(),
        format!("{max_segment}a"),
        "a/b/c/d/e/f/g/h/i".to_string(),
        format!("{}a", format!("{}/", "a".repeat(31)).repeat(4)),
    ];
    for id in &invalid {
        p.reset();
        p.next_slice(b"7501;state=clear:id=");
        p.next_slice(id.as_bytes());
        assert_eq!(None, p.end(Some(0x1b)), "id={id:?}");
    }
}

#[test]
fn osc_7501_text_must_decode_to_safe_utf8() {
    let mut p = parser();

    let invalid: [&[u8]; 6] = [
        // Not base64.
        b"7501;state=done:msg=a",
        b"7501;state=done:msg=QQ=",
        b"7501;state=done:title=a",
        // "a\nb", a control character.
        b"7501;state=done:msg=YQpi",
        // "\x9b", a C1 control character as UTF-8.
        b"7501;state=done:msg=wps=",
        // Invalid UTF-8.
        b"7501;state=done:msg=/w==",
    ];
    for input in invalid {
        p.reset();
        p.next_slice(input);
        assert_eq!(None, p.end(Some(0x1b)), "input={input:?}");
    }

    // Unicode is fine. "安全"
    p.reset();
    p.next_slice(b"7501;state=done:msg=5a6J5YWo");
    let r = report(&mut p);
    assert_eq!("安全".as_bytes().to_vec(), text(&r, OptionKey::Msg));
}

#[test]
fn osc_7501_empty_text_is_absent() {
    let mut p = parser();

    p.next_slice(b"7501;state=done:msg=:title=");
    let r = report(&mut p);
    assert_eq!(None, str_opt(&r, OptionKey::Msg));
    assert_eq!(None, str_opt(&r, OptionKey::Title));
}

#[test]
fn osc_7501_discarded_reports() {
    let mut p = parser();

    // 192 bytes of title and 2048 bytes of message are the most allowed.
    // "QUFB" decodes to "AAA" and "QUE" to "AA".
    let title = "QUFB".repeat(MAX_TITLE_ENCODED_BYTES / 4);
    let msg = format!("{}QUE", "QUFB".repeat(MAX_MSG_BYTES / 3));
    let fill = MAX_SEQUENCE_BYTES - b"\x1b]7501;state=idle:".len() - b"\x1b\\".len();

    let cases: Vec<(String, bool)> = vec![
        // A missing or unknown state.
        ("7501;".into(), false),
        ("7501;app=cargo".into(), false),
        ("7501;state=sleeping".into(), false),
        ("7501;state=".into(), false),
        ("7501;state=IDLE".into(), false),
        // A key that is too long, even one we don't know.
        (format!("7501;state=idle:{}=1", "a".repeat(MAX_KEY_BYTES)), true),
        (format!("7501;state=idle:{}=1", "a".repeat(MAX_KEY_BYTES + 1)), false),
        // An app that is too long, even if a later pair replaces it.
        (format!("7501;state=idle:app={}", "a".repeat(MAX_APP_BYTES + 1)), false),
        (
            format!("7501;state=idle:app={}:app=ok", "a".repeat(MAX_APP_BYTES + 1)),
            false,
        ),
        // Text at its limits and one step over.
        (format!("7501;state=idle:title={title}"), true),
        (format!("7501;state=idle:title={title}QUFB"), false),
        (format!("7501;state=idle:msg={msg}"), true),
        (
            format!("7501;state=idle:msg={}", "QUFB".repeat(MAX_MSG_BYTES / 3 + 1)),
            false,
        ),
        // The whole sequence, including the terminator.
        (format!("7501;state=idle:{}", "x".repeat(fill)), true),
        (format!("7501;state=idle:{}", "x".repeat(fill + 1)), false),
        // Numbers that only start like 7501.
        ("75;state=idle".into(), false),
        ("750;state=idle".into(), false),
        ("75010;state=idle".into(), false),
        ("7501".into(), false),
    ];
    for (input, want) in &cases {
        p.reset();
        p.next_slice(input.as_bytes());
        assert_eq!(*want, p.end(Some(0x1b)).is_some(), "input len {}", input.len());
    }
}

#[test]
fn osc_7501_no_allocator() {
    let mut p = Parser::new(false);

    p.next_slice(b"7501;state=working:app=cargo");
    let r = report(&mut p);
    assert_eq!(State::Working, r.state);
    assert_eq!(Some(&b"cargo"[..]), str_opt(&r, OptionKey::App));
}

// No Ghostty test: the two log sites, verbatim.
#[test]
fn osc_7501_log_sites() {
    let mut p = parser();

    let cap = crate::log_capture::start();
    p.next_slice(b"7501;state=sleeping");
    assert!(p.end(Some(0x1b)).is_none());
    p.reset();
    p.next_slice(b"7501;state=idle:");
    p.next_slice("x".repeat(MAX_BODY_BYTES).as_bytes());
    assert!(p.end(Some(0x1b)).is_none());
    let messages = cap.messages();
    drop(cap);
    let want = |m: &str| (tracing::Level::WARN, "osc_program_status".to_string(), m.to_string());
    assert_eq!(
        vec![
            want("invalid OSC 7501 report err=error.InvalidState"),
            want(&format!(
                "OSC 7501 sequence too long len={}",
                "state=idle:".len() + MAX_BODY_BYTES
            )),
        ],
        messages
    );
}

// No Ghostty test: the allocating capture is freed (upstream's
// `testing.allocator` detects a leak).
#[test]
fn osc_7501_allocating_capture_is_balanced() {
    let ((), counting) = with_counting(|| {
        let mut p = Parser::new(true);
        p.next_slice(b"7501;state=working:app=cargo");
        let r = report(&mut p);
        assert_eq!(State::Working, r.state);
        p.reset();
        p.next_slice(b"7501;state=idle:bad");
        assert!(p.end(Some(0x1b)).is_some());
    });
    assert!(counting.alloc_calls > 0);
    assert_eq!(0, counting.live_bytes);
}

/// Drift `b7207b2ef`: the query reply lists every state but `clear` and every kind, in their declaration order, as
/// Zig builds it from the two enums.
#[test]
fn query_reply_lists_every_state_and_kind() {
    let states: Vec<&str> = State::ALL
        .iter()
        .filter(|&&s| s != State::Clear)
        .map(|s| s.name())
        .collect();
    let kinds: Vec<&str> = Kind::ALL.iter().map(|k| k.name()).collect();
    let built = format!("?:states={}:kinds={}", states.join(","), kinds.join(","));
    assert_eq!(built, QUERY_REPLY);
}

/// Every state and kind parses from its own name.
#[test]
fn state_and_kind_names_round_trip() {
    for state in State::ALL {
        assert_eq!(Some(state), State::from_name(state.name().as_bytes()), "{state:?}");
    }
    for kind in Kind::ALL {
        assert_eq!(Some(kind), Kind::from_name(kind.name().as_bytes()), "{kind:?}");
    }
}
