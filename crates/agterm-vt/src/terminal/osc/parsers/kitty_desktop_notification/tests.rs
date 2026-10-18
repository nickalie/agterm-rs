use super::{Action, Occasion, OptionKey, Osc, Payload, Urgency};
use super::{MAX_ENCODED_PAYLOAD_BYTES, MAX_PLAIN_PAYLOAD_BYTES};
use crate::terminal::osc::{Command, Parser, Terminator};

/// Drive `input` through a fresh parser and return the parsed
/// `kitty_desktop_notification` command (the shared preamble of the
/// reference tests: `var p: Parser = .init(null); for (input) |ch|
/// p.next(ch); const cmd = p.end('\x1b').?.*;`).
macro_rules! parse_kdn {
    ($kdn:ident, $input:expr) => {
        let mut p: Parser = Parser::new(false);

        for ch in $input.bytes() {
            p.next(ch);
        }

        let Some(Command::KittyDesktopNotification($kdn)) = p.end(Some(0x1b)) else {
            panic!("expected a kitty desktop notification");
        };
    };
}

/// Every option of `kdn` reads its protocol default.
fn assert_all_defaults(kdn: &Osc<'_>) {
    assert_eq!(OptionKey::A.default(), kdn.read_option(OptionKey::A));
    assert!(!kdn.read_option(OptionKey::C).as_bool().unwrap());
    assert!(kdn.read_option(OptionKey::D).as_bool().unwrap());
    assert!(!kdn.read_option(OptionKey::E).as_bool().unwrap());
    assert!(kdn.read_option(OptionKey::F).as_opt_bytes().is_none());
    assert!(kdn.read_option(OptionKey::G).as_opt_bytes().is_none());
    assert!(kdn.read_option(OptionKey::I).as_opt_bytes().is_none());
    {
        let mut it = kdn.read_option(OptionKey::N).into_iterator().unwrap();
        assert!(it.next().is_none());
    }
    assert_eq!(Occasion::Always, kdn.read_option(OptionKey::O).as_occasion().unwrap());
    assert_eq!(Payload::Title, kdn.read_option(OptionKey::P).as_payload().unwrap());
    assert_eq!(b"system".as_slice(), kdn.read_option(OptionKey::S).as_bytes().unwrap());
    {
        let mut it = kdn.read_option(OptionKey::T).into_iterator().unwrap();
        assert!(it.next().is_none());
    }
    assert_eq!(Urgency::Normal, kdn.read_option(OptionKey::U).as_urgency().unwrap());
    assert_eq!(-1, kdn.read_option(OptionKey::W).as_int().unwrap());
}

#[test]
fn osc_99_empty_metadata_and_payload() {
    parse_kdn!(kdn, "99;;");
    assert_eq!(b"".as_slice(), kdn.metadata);
    assert_eq!(b"".as_slice(), kdn.payload);
    assert_all_defaults(&kdn);
    assert_eq!(Terminator::St, kdn.terminator);
}

#[test]
fn osc_99_empty_metadata_with_payload() {
    parse_kdn!(kdn, "99;;bobr");
    assert_eq!(b"".as_slice(), kdn.metadata);
    assert_eq!(b"bobr".as_slice(), kdn.payload);
    assert_all_defaults(&kdn);
}

#[test]
fn osc_99_payload_size_limits() {
    struct Case {
        metadata: &'static str,
        payload_size: usize,
        valid: bool,
    }
    let cases = [
        Case {
            metadata: "",
            payload_size: MAX_PLAIN_PAYLOAD_BYTES,
            valid: true,
        },
        Case {
            metadata: "",
            payload_size: MAX_PLAIN_PAYLOAD_BYTES + 1,
            valid: false,
        },
        Case {
            metadata: "e=1",
            payload_size: MAX_ENCODED_PAYLOAD_BYTES,
            valid: true,
        },
        Case {
            metadata: "e=1",
            payload_size: MAX_ENCODED_PAYLOAD_BYTES + 1,
            valid: false,
        },
    ];

    for case in cases {
        let mut p: Parser = Parser::new(true);

        for ch in "99;".bytes() {
            p.next(ch);
        }
        for ch in case.metadata.bytes() {
            p.next(ch);
        }
        p.next(b';');
        for _ in 0..case.payload_size {
            p.next(b'a');
        }

        assert_eq!(case.valid, p.end(Some(0x1b)).is_some());
    }
}

#[test]
fn osc_99_unknown_prefix_does_not_hide_dotted_identifier() {
    parse_kdn!(kdn, "99;invalid=wrong:i=org.ghostty;payload");
    assert_eq!(
        b"org.ghostty".as_slice(),
        kdn.read_option(OptionKey::I).as_opt_bytes().unwrap()
    );
}

#[test]
fn osc_99_single_parameter_i() {
    parse_kdn!(kdn, "99;i=bobr;payload");
    assert_eq!(
        b"bobr".as_slice(),
        kdn.read_option(OptionKey::I).as_opt_bytes().unwrap()
    );
    assert_eq!(b"payload".as_slice(), kdn.payload);
}

#[test]
fn osc_99_repeated_parameter_i() {
    parse_kdn!(kdn, "99;i=bobr:i=foobar;payload");
    assert_eq!(
        b"bobr".as_slice(),
        kdn.read_option(OptionKey::I).as_opt_bytes().unwrap()
    );
    assert_eq!(b"payload".as_slice(), kdn.payload);
}

#[test]
fn osc_99_multiple_types() {
    parse_kdn!(kdn, "99;t=mail: t = chat : t = alert ;notification");
    assert_eq!(b"notification".as_slice(), kdn.payload);
    let mut it = kdn.read_option(OptionKey::T).into_iterator().unwrap();
    assert_eq!(b"mail".as_slice(), it.next().unwrap());
    assert_eq!(b"chat".as_slice(), it.next().unwrap());
    assert_eq!(b"alert".as_slice(), it.next().unwrap());
    assert!(it.next().is_none());
}

#[test]
fn osc_99_a_1() {
    parse_kdn!(kdn, "99;a=report,focus;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert_eq!(
        Action {
            report: true,
            focus: true
        },
        kdn.read_option(OptionKey::A).as_action().unwrap()
    );
}

#[test]
fn osc_99_a_2() {
    parse_kdn!(kdn, "99;a=report,-focus;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert_eq!(
        Action {
            report: true,
            focus: false
        },
        kdn.read_option(OptionKey::A).as_action().unwrap()
    );
}

#[test]
fn osc_99_a_3() {
    parse_kdn!(kdn, "99;a=-report,focus;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert_eq!(
        Action {
            report: false,
            focus: true
        },
        kdn.read_option(OptionKey::A).as_action().unwrap()
    );
}

#[test]
fn osc_99_a_4() {
    parse_kdn!(kdn, "99;a=-report,-focus;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert_eq!(
        Action {
            report: false,
            focus: false
        },
        kdn.read_option(OptionKey::A).as_action().unwrap()
    );
}

#[test]
fn osc_99_c_1() {
    parse_kdn!(kdn, "99;c=0;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert!(!kdn.read_option(OptionKey::C).as_bool().unwrap());
}

#[test]
fn osc_99_c_2() {
    parse_kdn!(kdn, "99;c=1;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert!(kdn.read_option(OptionKey::C).as_bool().unwrap());
}

#[test]
fn osc_99_c_3() {
    parse_kdn!(kdn, "99;c=bobr;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert!(!kdn.read_option(OptionKey::C).as_bool().unwrap());
}

#[test]
fn osc_99_d_1() {
    parse_kdn!(kdn, "99;d=0;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert!(!kdn.read_option(OptionKey::D).as_bool().unwrap());
}

#[test]
fn osc_99_d_2() {
    parse_kdn!(kdn, "99;d=1;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert!(kdn.read_option(OptionKey::D).as_bool().unwrap());
}

#[test]
fn osc_99_d_3() {
    parse_kdn!(kdn, "99;d=bobr;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert!(kdn.read_option(OptionKey::D).as_bool().unwrap());
}

#[test]
fn osc_99_e_1() {
    parse_kdn!(kdn, "99;e=0;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert!(!kdn.read_option(OptionKey::E).as_bool().unwrap());
}

#[test]
fn osc_99_e_2() {
    parse_kdn!(kdn, "99;e=1;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert!(kdn.read_option(OptionKey::E).as_bool().unwrap());
}

#[test]
fn osc_99_e_3() {
    parse_kdn!(kdn, "99;e=bobr;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert!(!kdn.read_option(OptionKey::E).as_bool().unwrap());
}

#[test]
fn osc_99_f_1() {
    parse_kdn!(kdn, "99;f=R2hvc3R0eQ==;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert_eq!(
        b"R2hvc3R0eQ==".as_slice(),
        kdn.read_option(OptionKey::F).as_opt_bytes().unwrap()
    );
}

#[test]
fn osc_99_f_2() {
    parse_kdn!(kdn, "99;c=0:f= R2hvc3R0eQ== ;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert_eq!(
        b"R2hvc3R0eQ==".as_slice(),
        kdn.read_option(OptionKey::F).as_opt_bytes().unwrap()
    );
}

#[test]
fn osc_99_g_1() {
    parse_kdn!(kdn, "99;c=0:g=7f8a9129-a35d-4e9f-8043-ce2700e15e2c;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert_eq!(
        b"7f8a9129-a35d-4e9f-8043-ce2700e15e2c".as_slice(),
        kdn.read_option(OptionKey::G).as_opt_bytes().unwrap()
    );
}

#[test]
fn osc_99_g_2() {
    parse_kdn!(kdn, "99;c=0:g=aaa*bbb;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert!(kdn.read_option(OptionKey::G).as_opt_bytes().is_none());
}

#[test]
fn osc_99_i_1() {
    parse_kdn!(kdn, "99;;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert!(kdn.read_option(OptionKey::I).as_opt_bytes().is_none());
}

#[test]
fn osc_99_i_2() {
    parse_kdn!(kdn, "99;i=bobr;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert_eq!(
        b"bobr".as_slice(),
        kdn.read_option(OptionKey::I).as_opt_bytes().unwrap()
    );
}

#[test]
fn osc_99_i_3() {
    parse_kdn!(kdn, "99;i=;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert_eq!(b"".as_slice(), kdn.read_option(OptionKey::I).as_opt_bytes().unwrap());
}

#[test]
fn osc_99_i_4() {
    parse_kdn!(kdn, "99;i= :;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert_eq!(b"".as_slice(), kdn.read_option(OptionKey::I).as_opt_bytes().unwrap());
}

#[test]
fn osc_99_i_5() {
    parse_kdn!(kdn, "99;i= bobr ;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert_eq!(
        b"bobr".as_slice(),
        kdn.read_option(OptionKey::I).as_opt_bytes().unwrap()
    );
}

#[test]
fn osc_99_i_6() {
    parse_kdn!(kdn, "99;i= bobr : i=kurwa ;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert_eq!(
        b"bobr".as_slice(),
        kdn.read_option(OptionKey::I).as_opt_bytes().unwrap()
    );
}

#[test]
fn osc_99_n_1() {
    parse_kdn!(kdn, "99;n=R2hvc3R0eQ==;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    let mut it = kdn.read_option(OptionKey::N).into_iterator().unwrap();
    assert_eq!(b"R2hvc3R0eQ==".as_slice(), it.next().unwrap());
    assert!(it.next().is_none());
}

#[test]
fn osc_99_n_2() {
    parse_kdn!(kdn, "99;n=R2hvc3R0eQ==:n=R2hvc3R0eQ==;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    let mut it = kdn.read_option(OptionKey::N).into_iterator().unwrap();
    assert_eq!(b"R2hvc3R0eQ==".as_slice(), it.next().unwrap());
    assert_eq!(b"R2hvc3R0eQ==".as_slice(), it.next().unwrap());
    assert!(it.next().is_none());
}

#[test]
fn osc_99_o_1() {
    parse_kdn!(kdn, "99;o= ;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert_eq!(Occasion::Always, kdn.read_option(OptionKey::O).as_occasion().unwrap());
}

#[test]
fn osc_99_o_2() {
    parse_kdn!(kdn, "99;o=always;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert_eq!(Occasion::Always, kdn.read_option(OptionKey::O).as_occasion().unwrap());
}

#[test]
fn osc_99_o_3() {
    parse_kdn!(kdn, "99;o=unfocused;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert_eq!(
        Occasion::Unfocused,
        kdn.read_option(OptionKey::O).as_occasion().unwrap()
    );
}

#[test]
fn osc_99_o_4() {
    parse_kdn!(kdn, "99;o=invisible;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert_eq!(
        Occasion::Invisible,
        kdn.read_option(OptionKey::O).as_occasion().unwrap()
    );
}

#[test]
fn osc_99_o_5() {
    parse_kdn!(kdn, "99;o=bobr;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert_eq!(Occasion::Always, kdn.read_option(OptionKey::O).as_occasion().unwrap());
}

#[test]
fn osc_99_p_1() {
    parse_kdn!(kdn, "99;p=alive;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert_eq!(Payload::Alive, kdn.read_option(OptionKey::P).as_payload().unwrap());
}

#[test]
fn osc_99_p_2() {
    parse_kdn!(kdn, "99;p=body;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert_eq!(Payload::Body, kdn.read_option(OptionKey::P).as_payload().unwrap());
}

#[test]
fn osc_99_p_3() {
    parse_kdn!(kdn, "99;p=buttons;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert_eq!(Payload::Buttons, kdn.read_option(OptionKey::P).as_payload().unwrap());
}

#[test]
fn osc_99_p_4() {
    parse_kdn!(kdn, "99;p=close;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert_eq!(Payload::Close, kdn.read_option(OptionKey::P).as_payload().unwrap());
}

#[test]
fn osc_99_p_5() {
    parse_kdn!(kdn, "99;p=icon;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert_eq!(Payload::Icon, kdn.read_option(OptionKey::P).as_payload().unwrap());
}

#[test]
fn osc_99_p_6() {
    parse_kdn!(kdn, "99;p=?;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert_eq!(Payload::Query, kdn.read_option(OptionKey::P).as_payload().unwrap());
}

#[test]
fn osc_99_p_7() {
    parse_kdn!(kdn, "99;p=title;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert_eq!(Payload::Title, kdn.read_option(OptionKey::P).as_payload().unwrap());
}

#[test]
fn osc_99_p_8() {
    parse_kdn!(kdn, "99;p=query;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert_eq!(Payload::Unknown, kdn.read_option(OptionKey::P).as_payload().unwrap());
}

#[test]
fn osc_99_p_9() {
    parse_kdn!(kdn, "99;p=bobr;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert_eq!(Payload::Unknown, kdn.read_option(OptionKey::P).as_payload().unwrap());
}

#[test]
fn osc_99_s_1() {
    parse_kdn!(kdn, "99;s=R2hvc3R0eQ==;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert_eq!(
        b"R2hvc3R0eQ==".as_slice(),
        kdn.read_option(OptionKey::S).as_bytes().unwrap()
    );
}

#[test]
fn osc_99_t_1() {
    parse_kdn!(kdn, "99;t=R2hvc3R0eQ==;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    let mut it = kdn.read_option(OptionKey::T).into_iterator().unwrap();
    assert_eq!(b"R2hvc3R0eQ==".as_slice(), it.next().unwrap());
    assert!(it.next().is_none());
}

#[test]
fn osc_99_t_2() {
    parse_kdn!(kdn, "99;t=R2hvc3R0eQ==:t=R2hvc3R0eQ==;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    let mut it = kdn.read_option(OptionKey::T).into_iterator().unwrap();
    assert_eq!(b"R2hvc3R0eQ==".as_slice(), it.next().unwrap());
    assert_eq!(b"R2hvc3R0eQ==".as_slice(), it.next().unwrap());
    assert!(it.next().is_none());
}

#[test]
fn osc_99_u_1() {
    parse_kdn!(kdn, "99;u=0;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert_eq!(Urgency::Low, kdn.read_option(OptionKey::U).as_urgency().unwrap());
}

#[test]
fn osc_99_u_2() {
    parse_kdn!(kdn, "99;u=1;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert_eq!(Urgency::Normal, kdn.read_option(OptionKey::U).as_urgency().unwrap());
}

#[test]
fn osc_99_u_3() {
    parse_kdn!(kdn, "99;u=2;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert_eq!(Urgency::High, kdn.read_option(OptionKey::U).as_urgency().unwrap());
}

#[test]
fn osc_99_u_4() {
    parse_kdn!(kdn, "99;u=bobr;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert_eq!(Urgency::Normal, kdn.read_option(OptionKey::U).as_urgency().unwrap());
}

#[test]
fn osc_99_w_1() {
    parse_kdn!(kdn, "99;w=0;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert_eq!(0, kdn.read_option(OptionKey::W).as_int().unwrap());
}

#[test]
fn osc_99_w_2() {
    parse_kdn!(kdn, "99;w=-1;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert_eq!(-1, kdn.read_option(OptionKey::W).as_int().unwrap());
}

#[test]
fn osc_99_w_3() {
    parse_kdn!(kdn, "99;w=-42;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert_eq!(-1, kdn.read_option(OptionKey::W).as_int().unwrap());
}

#[test]
fn osc_99_w_4() {
    parse_kdn!(kdn, "99;w=4294967296;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert_eq!(-1, kdn.read_option(OptionKey::W).as_int().unwrap());
}

#[test]
fn osc_99_w_5() {
    parse_kdn!(kdn, "99;w=4_2;foobar");
    assert_eq!(b"foobar".as_slice(), kdn.payload);
    assert_eq!(-1, kdn.read_option(OptionKey::W).as_int().unwrap());
}

// No Ghostty test: the three rejection arms of `parse` log
// Ghostty's warnings (kitty_desktop_notification.zig:299, :312,
// :322) under the `kitty_desktop_notification` scope.
fn rejected_with(input: &[u8]) -> Vec<crate::log_capture::CapturedRecord> {
    let mut p: Parser = Parser::new(true);
    let cap = crate::log_capture::start();
    for &ch in input {
        p.next(ch);
    }
    assert!(p.end(Some(0x1b)).is_none(), "the command must be rejected");
    let messages = cap.messages();
    drop(cap);
    messages
}

fn warn(msg: &str) -> crate::log_capture::CapturedRecord {
    (
        tracing::Level::WARN,
        "kitty_desktop_notification".to_string(),
        msg.to_string(),
    )
}

#[test]
fn osc_99_rejections_log_the_reference_warnings() {
    assert_eq!(vec![warn("missing semicolon before payload")], rejected_with(b"99;i=1"));
    let mut big = b"99;;".to_vec();
    big.extend(std::iter::repeat_n(b'a', MAX_PLAIN_PAYLOAD_BYTES + 1));
    assert_eq!(
        vec![warn(&format!(
            "payload is too large: size={} max={}",
            MAX_PLAIN_PAYLOAD_BYTES + 1,
            MAX_PLAIN_PAYLOAD_BYTES
        ))],
        rejected_with(&big)
    );
    assert_eq!(
        vec![warn("payload is not escape code safe UTF-8")],
        rejected_with(b"99;;a\x07b")
    );
}
