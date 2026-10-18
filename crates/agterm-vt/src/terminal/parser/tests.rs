use super::*;
use crate::terminal::osc;

/// An action with no borrow of the parser, for every variant but `OscDispatch`, which is handed back in `Err`: the
/// tests keep the actions of one step while they feed the next.
fn into_static(action: Action<'_>) -> Result<Action<'static>, osc::Command<'_>> {
    Ok(match action {
        Action::Print(c) => Action::Print(c),
        Action::Execute(c) => Action::Execute(c),
        Action::CsiDispatch(v) => Action::CsiDispatch(v),
        Action::EscDispatch(v) => Action::EscDispatch(v),
        Action::OscDispatch(cmd) => return Err(cmd),
        Action::DcsHook(v) => Action::DcsHook(v),
        Action::DcsPut(c) => Action::DcsPut(c),
        Action::DcsUnhook => Action::DcsUnhook,
        Action::ApcStart => Action::ApcStart,
        Action::ApcPut(c) => Action::ApcPut(c),
        Action::ApcEnd => Action::ApcEnd,
    })
}

/// The actions of one byte.
type Actions = [Option<Action<'static>>; 3];

fn next(p: &mut Parser, c: u8) -> Actions {
    p.next(c)
        .map(|a| a.map(|a| into_static(a).expect("not an osc_dispatch action")))
}

/// Feed `bytes`, none of which may produce an action.
fn feed_silent(p: &mut Parser, bytes: &[u8]) {
    for &c in bytes {
        let a = next(p, c);
        assert!(a.iter().all(Option::is_none), "{:?} produced {a:?}", char::from(c));
    }
}

/// The transition action of `a`, with no exit or entry action.
fn transition(a: &Actions) -> &Action<'static> {
    assert!(a[0].is_none() && a[2].is_none(), "{a:?}");
    a[1].as_ref().expect("a transition action")
}

/// The CSI the final byte `c` dispatches, which returns the parser to ground.
fn csi(p: &mut Parser, c: u8) -> Csi {
    let a = next(p, c);
    assert_eq!(p.state, State::Ground);
    match transition(&a) {
        Action::CsiDispatch(d) => d.clone(),
        other => panic!("expected a CSI dispatch, got {other:?}"),
    }
}

/// The DCS hook the final byte `c` enters passthrough with.
fn dcs_hook(p: &mut Parser, c: u8) -> Dcs {
    let a = next(p, c);
    assert_eq!(p.state, State::DcsPassthrough);
    assert!(a[0].is_none() && a[1].is_none(), "{a:?}");
    match a[2].as_ref() {
        Some(Action::DcsHook(hook)) => hook.clone(),
        other => panic!("expected a DCS hook, got {other:?}"),
    }
}

/// The parameters of `d` with whether a colon follows each.
fn params_with_colons(d: &Csi) -> Vec<(u16, bool)> {
    d.params
        .iter()
        .enumerate()
        .map(|(i, &v)| (v, d.params_sep.is_set(i)))
        .collect()
}

/// Feed an OSC (after `ESC ]`) and its terminator `end`, and check the command it dispatches.
fn osc_dispatch(p: &mut Parser, body: &[u8], end: u8, check: impl FnOnce(&osc::Command<'_>)) {
    let _ = p.next(0x1B);
    let _ = p.next(b']');
    for &c in body {
        let _ = p.next(c);
    }
    let a = p.next(end);
    assert!(a[1].is_none() && a[2].is_none(), "{a:?}");
    let Some(Action::OscDispatch(cmd)) = a[0].as_ref() else {
        panic!("expected an OSC dispatch, got {a:?}");
    };
    check(cmd);
}

#[test]
fn basic_apc_print_execute() {
    let mut p = Parser::new();
    let _ = next(&mut p, 0x9E);
    assert_eq!(p.state, State::SosPmApcString);
    let _ = next(&mut p, 0x9C);
    assert_eq!(p.state, State::Ground);

    let a = next(&mut p, b'a');
    assert_eq!(p.state, State::Ground);
    assert!(matches!(transition(&a), Action::Print(_)));

    let a = next(&mut p, 0x19);
    assert_eq!(p.state, State::Ground);
    assert!(matches!(transition(&a), Action::Execute(_)));
}

#[test]
fn esc_esc_paren_b() {
    let mut p = Parser::new();
    let _ = next(&mut p, 0x1B);
    let _ = next(&mut p, b'(');

    let a = next(&mut p, b'B');
    assert_eq!(p.state, State::Ground);
    let Action::EscDispatch(d) = transition(&a) else {
        panic!("expected an ESC dispatch, got {a:?}");
    };
    assert_eq!(d.final_, b'B');
    assert_eq!(d.intermediates, *b"(");
}

#[test]
fn csi_esc_bracket_h() {
    let mut p = Parser::new();
    feed_silent(&mut p, b"\x1b[");
    let d = csi(&mut p, b'H');
    assert_eq!(d.final_, b'H');
    assert!(d.params.is_empty());
}

#[test]
fn csi_esc_bracket_1_4_h() {
    let mut p = Parser::new();
    feed_silent(&mut p, b"\x1b[1;4");
    let d = csi(&mut p, b'H');
    assert_eq!(d.final_, b'H');
    assert_eq!(d.params, [1, 4]);
}

#[test]
fn csi_sgr_esc_38_2_m() {
    let mut p = Parser::new();
    feed_silent(&mut p, b"\x1b[38:2");
    let d = csi(&mut p, b'm');
    assert_eq!(d.final_, b'm');
    assert_eq!(vec![(38, true), (2, false)], params_with_colons(&d));
}

#[test]
fn csi_sgr_colon_followed_by_semicolon() {
    let mut p = Parser::new();
    feed_silent(&mut p, b"\x1b[48:2");
    let _ = csi(&mut p, b'm');

    feed_silent(&mut p, b"\x1b[");
    let _ = csi(&mut p, b'H');
}

#[test]
fn csi_sgr_mixed_colon_and_semicolon() {
    let mut p = Parser::new();
    feed_silent(&mut p, b"\x1b[38:5:1;48:5:0");
    let _ = csi(&mut p, b'm');
}

#[test]
fn csi_sgr_esc_48_2_m() {
    let mut p = Parser::new();
    feed_silent(&mut p, b"\x1b[48:2:240:143:104");
    let d = csi(&mut p, b'm');
    assert_eq!(d.final_, b'm');
    assert_eq!(
        vec![(48, true), (2, true), (240, true), (143, true), (104, false)],
        params_with_colons(&d)
    );
}

#[test]
fn csi_sgr_esc_4_3_m_colon() {
    let mut p = Parser::new();
    feed_silent(&mut p, b"\x1b[4:3");
    let d = csi(&mut p, b'm');
    assert_eq!(d.final_, b'm');
    assert_eq!(vec![(4, true), (3, false)], params_with_colons(&d));
}

#[test]
fn csi_sgr_with_many_blank_and_colon() {
    let mut p = Parser::new();
    feed_silent(&mut p, b"\x1b[58:2::240:143:104");
    let d = csi(&mut p, b'm');
    assert_eq!(d.final_, b'm');
    assert_eq!(
        vec![(58, true), (2, true), (0, true), (240, true), (143, true), (104, false)],
        params_with_colons(&d)
    );
}

// This is from a Kakoune actual SGR sequence.
#[test]
fn csi_sgr_mixed_colon_and_semicolon_with_blank() {
    let mut p = Parser::new();
    feed_silent(&mut p, b"\x1b[;4:3;38;2;175;175;215;58:2::190:80:70");
    let d = csi(&mut p, b'm');
    assert_eq!(d.final_, b'm');
    let want = vec![
        (0, false),
        (4, true),
        (3, false),
        (38, false),
        (2, false),
        (175, false),
        (175, false),
        (215, false),
        (58, true),
        (2, true),
        (0, true),
        (190, true),
        (80, true),
        (70, false),
    ];
    assert_eq!(want, params_with_colons(&d));
}

// This is from a Kakoune actual SGR sequence also.
#[test]
fn csi_sgr_mixed_colon_and_semicolon_setting_underline_bg_fg() {
    let mut p = Parser::new();
    feed_silent(&mut p, b"\x1b[4:3;38;2;51;51;51;48;2;170;170;170;58;2;255;97;136");
    let d = csi(&mut p, b'm');
    assert_eq!(d.final_, b'm');
    let want = vec![
        (4, true),
        (3, false),
        (38, false),
        (2, false),
        (51, false),
        (51, false),
        (51, false),
        (48, false),
        (2, false),
        (170, false),
        (170, false),
        (170, false),
        (58, false),
        (2, false),
        (255, false),
        (97, false),
        (136, false),
    ];
    assert_eq!(want, params_with_colons(&d));
}

#[test]
fn csi_colon_for_non_m_final() {
    let mut p = Parser::new();
    feed_silent(&mut p, b"\x1b[38:2h");
    assert_eq!(p.state, State::Ground);
}

#[test]
fn csi_request_mode_decrqm() {
    let mut p = Parser::new();
    feed_silent(&mut p, b"\x1b[?2026$");
    let d = csi(&mut p, b'p');
    assert_eq!(d.final_, b'p');
    assert_eq!(d.intermediates, *b"?$");
    assert_eq!(d.params, [2026]);
}

#[test]
fn csi_change_cursor() {
    let mut p = Parser::new();
    feed_silent(&mut p, b"\x1b[3 ");
    let d = csi(&mut p, b'q');
    assert_eq!(d.final_, b'q');
    assert_eq!(d.intermediates, *b" ");
    assert_eq!(d.params, [3]);
}

#[test]
fn osc_change_window_title() {
    let mut p = Parser::new();
    osc_dispatch(&mut p, b"0;abc", 0x07, |cmd| {
        assert_eq!(&osc::Command::ChangeWindowTitle(b"abc".as_slice()), cmd);
    });
    assert_eq!(p.state, State::Ground);
}

#[test]
fn osc_change_window_title_end_in_esc() {
    let mut p = Parser::new();
    osc_dispatch(&mut p, b"0;abc", 0x1B, |cmd| {
        assert_eq!(&osc::Command::ChangeWindowTitle(b"abc".as_slice()), cmd);
    });
    let _ = p.next(b'\\');
    assert_eq!(p.state, State::Ground);
}

/// The color operation `cmd` is, with its requests.
fn color_operation(cmd: &osc::Command<'_>) -> (osc::color::Operation, Vec<osc::color::Request>, osc::Terminator) {
    let osc::Command::ColorOperation {
        op,
        requests,
        terminator,
    } = cmd
    else {
        panic!("expected a color operation, got {cmd:?}");
    };
    (*op, (*requests).clone(), *terminator)
}

// https://github.com/darrenstarr/VtNetCore/pull/14
// Saw this on HN, decided to add a test case because why not.
#[test]
fn osc_112_incomplete_sequence() {
    let mut p: Parser = Parser::new();
    p.osc.alloc = true;
    osc_dispatch(&mut p, b"112", 0x07, |cmd| {
        let (op, requests, terminator) = color_operation(cmd);
        assert_eq!(terminator, osc::Terminator::Bel);
        assert_eq!(op, osc::color::Operation::Osc112);
        let cursor = osc::color::Target::Dynamic(crate::terminal::color::Dynamic::Cursor);
        assert_eq!(vec![osc::color::Request::Reset(cursor)], requests);
    });
    assert_eq!(p.state, State::Ground);
}

#[test]
fn osc_104_empty() {
    let mut p: Parser = Parser::new();
    p.osc.alloc = true;
    osc_dispatch(&mut p, b"104", 0x07, |cmd| {
        let (op, requests, terminator) = color_operation(cmd);
        assert_eq!(terminator, osc::Terminator::Bel);
        assert_eq!(op, osc::color::Operation::Osc104);
        assert_eq!(vec![osc::color::Request::ResetPalette], requests);
    });
    assert_eq!(p.state, State::Ground);
}

#[test]
fn osc_105_empty() {
    let mut p: Parser = Parser::new();
    p.osc.alloc = true;
    osc_dispatch(&mut p, b"105", 0x07, |cmd| {
        assert_eq!(osc::color::Operation::Osc105, color_operation(cmd).0);
    });
}

#[test]
fn csi_too_many_params() {
    let mut p = Parser::new();
    let mut input = b"\x1b[".to_vec();
    for _ in 0..100 {
        input.extend_from_slice(b"1;");
    }
    input.extend_from_slice(b"1C");
    feed_silent(&mut p, &input);
    assert_eq!(p.state, State::Ground);
}

#[test]
fn csi_sgr_with_up_to_our_max_parameters() {
    for max in 1..=MAX_PARAMS {
        let mut p = Parser::new();
        let mut input = b"\x1b[".to_vec();
        for _ in 0..max - 1 {
            input.extend_from_slice(b"1;");
        }
        input.push(b'2');
        feed_silent(&mut p, &input);

        let d = csi(&mut p, b'H');
        assert_eq!(max, d.params.len());
        assert_eq!(2u16, d.params[max - 1]);
    }
}

#[test]
fn csi_sgr_beyond_our_max_drops_it() {
    // Has to be +2 for the loops below
    let max = MAX_PARAMS + 2;

    let mut p = Parser::new();
    let mut input = b"\x1b[".to_vec();
    for _ in 0..max - 1 {
        input.extend_from_slice(b"1;");
    }
    input.extend_from_slice(b"2H");
    feed_silent(&mut p, &input);
    assert_eq!(p.state, State::Ground);
}

#[test]
fn dcs_xtgettcap() {
    let mut p = Parser::new();
    feed_silent(&mut p, b"\x1bP+");
    let hook = dcs_hook(&mut p, b'q');
    assert_eq!(hook.intermediates, *b"+");
    assert!(hook.params.is_empty());
    assert_eq!(b'q', hook.final_);
}

#[test]
fn dcs_params() {
    let mut p = Parser::new();
    feed_silent(&mut p, b"\x1bP1000");
    let hook = dcs_hook(&mut p, b'p');
    assert_eq!(hook.params, [1000]);
    assert_eq!(b'p', hook.final_);
}

#[test]
fn dcs_too_many_params() {
    // Regression test for a crash found by fuzzing (afl). When a DCS sequence has more than MAX_PARAMS parameters
    // and param_acc_idx > 0, entering dcs_passthrough wrote to params[params_idx] without a bounds check, causing an
    // out-of-bounds access.
    let mut p = Parser::new();
    // A digit then MAX_PARAMS semicolons fill all param slots; another digit makes param_acc_idx > 0 while
    // params_idx == MAX_PARAMS. The final byte triggers entry to dcs_passthrough: the DCS should be dropped
    // entirely, consistent with how CSI handles overflow.
    let mut input = b"\x1bP6".to_vec();
    input.extend(std::iter::repeat_n(b';', MAX_PARAMS));
    input.extend_from_slice(b"7p");
    feed_silent(&mut p, &input);
}

#[test]
fn collect_logs_when_intermediates_overflow() {
    let cap = crate::log_capture::start();
    let mut p = Parser::new();
    let _ = next(&mut p, 0x1B); // ESC
    let _ = next(&mut p, b'['); // CSI entry
    // Intermediate bytes are 0x20..=0x2F; feed one more than `MAX_INTERMEDIATE` (4) to overflow `collect`.
    for _ in 0..=MAX_INTERMEDIATE {
        let _ = next(&mut p, b'!');
    }
    let _ = next(&mut p, b'm');

    let messages = cap.messages();
    drop(cap);
    let want = (
        tracing::Level::WARN,
        "parser".to_owned(),
        "invalid intermediates count".to_owned(),
    );
    assert!(
        messages.contains(&want),
        "expected the invalid-intermediates-count diagnostic, got: {messages:?}"
    );
}

#[test]
fn csi_dispatch_logs_on_colon_separator_mismatch_for_non_m_command() {
    let cap = crate::log_capture::start();
    let mut p = Parser::new();
    // A colon separator is only valid for 'm': the dispatch of the cursor position is suppressed entirely.
    feed_silent(&mut p, b"\x1b[1:2H");

    let messages = cap.messages();
    drop(cap);
    let want = (
        tracing::Level::WARN,
        "parser".to_owned(),
        "CSI colon or mixed separators only allowed for 'm' command, got: ESC [  { 1, 2 } H".to_owned(),
    );
    assert!(
        messages.contains(&want),
        "expected the CSI-sep-mismatch diagnostic, got: {messages:?}"
    );
}
