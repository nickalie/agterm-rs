use super::{ContextType, ExitStatus, Field};
use crate::terminal::osc::{Command, Parser};

#[test]
fn osc_3008_basic_start_command() {
    let mut p: Parser = Parser::new(false);
    let input = "3008;start=abc123";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    let Command::ContextSignal(cs) = cmd else {
        unreachable!()
    };
    assert_eq!(cs.action, super::Action::Start);
    assert_eq!(cs.id, b"abc123".as_slice());
    assert_eq!(cs.metadata, b"".as_slice());
}

#[test]
fn osc_3008_basic_end_command() {
    let mut p: Parser = Parser::new(false);
    let input = "3008;end=abc123";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    let Command::ContextSignal(cs) = cmd else {
        unreachable!()
    };
    assert_eq!(cs.action, super::Action::End);
    assert_eq!(cs.id, b"abc123".as_slice());
    assert_eq!(cs.metadata, b"".as_slice());
}

#[test]
fn osc_3008_start_with_metadata_fields() {
    let mut p: Parser = Parser::new(false);
    let input = "3008;start=bed86fab93af4328bbed0a1224af6d40;type=container;user=lennart;hostname=zeta";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    let Command::ContextSignal(cs) = cmd else {
        unreachable!()
    };
    assert_eq!(cs.action, super::Action::Start);
    assert_eq!(cs.id, b"bed86fab93af4328bbed0a1224af6d40".as_slice());

    // Read individual fields
    assert_eq!(cs.read_option(Field::Type).as_type().unwrap(), ContextType::Container);
    assert_eq!(cs.read_option(Field::User).as_str().unwrap().raw, b"lennart".as_slice());
    assert_eq!(
        cs.read_option(Field::Hostname).as_str().unwrap().raw,
        b"zeta".as_slice()
    );
}

#[test]
fn osc_3008_start_with_all_common_fields() {
    let mut p: Parser = Parser::new(false);
    let input = "3008;start=myctx;type=shell;user=root;hostname=myhost;machineid=3deb5353d3ba43d08201c136a47ead7b;bootid=d4a3d0fdf2e24fdea6d971ce73f4fbf2;pid=1062862;pidfdid=1063162;comm=bash";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    let Command::ContextSignal(cs) = cmd else {
        unreachable!()
    };
    assert_eq!(cs.read_option(Field::Type).as_type().unwrap(), ContextType::Shell);
    assert_eq!(cs.read_option(Field::User).as_str().unwrap().raw, b"root".as_slice());
    assert_eq!(
        cs.read_option(Field::Hostname).as_str().unwrap().raw,
        b"myhost".as_slice()
    );
    assert_eq!(
        cs.read_option(Field::Machineid).as_str().unwrap().raw,
        b"3deb5353d3ba43d08201c136a47ead7b".as_slice()
    );
    assert_eq!(
        cs.read_option(Field::Bootid).as_str().unwrap().raw,
        b"d4a3d0fdf2e24fdea6d971ce73f4fbf2".as_slice()
    );
    assert_eq!(cs.read_option(Field::Pid).as_int().unwrap(), 1_062_862_u64);
    assert_eq!(cs.read_option(Field::Pidfdid).as_int().unwrap(), 1_063_162_u64);
    assert_eq!(cs.read_option(Field::Comm).as_str().unwrap().raw, b"bash".as_slice());
}

#[test]
fn osc_3008_end_with_exit_metadata() {
    let mut p: Parser = Parser::new(false);
    let input = "3008;end=myctx;exit=success;status=0";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    let Command::ContextSignal(cs) = cmd else {
        unreachable!()
    };
    assert_eq!(cs.action, super::Action::End);
    assert_eq!(cs.id, b"myctx".as_slice());
    assert_eq!(cs.read_option(Field::Exit).as_exit().unwrap(), ExitStatus::Success);
    assert_eq!(cs.read_option(Field::Status).as_int().unwrap(), 0u64);
}

#[test]
fn osc_3008_end_with_failure_exit() {
    let mut p: Parser = Parser::new(false);
    let input = "3008;end=myctx;exit=failure;status=1;signal=SIGKILL";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    let Command::ContextSignal(cs) = cmd else {
        unreachable!()
    };
    assert_eq!(cs.read_option(Field::Exit).as_exit().unwrap(), ExitStatus::Failure);
    assert_eq!(cs.read_option(Field::Status).as_int().unwrap(), 1u64);
    assert_eq!(
        cs.read_option(Field::Signal).as_str().unwrap().raw,
        b"SIGKILL".as_slice()
    );
}

#[test]
fn osc_3008_numeric_fields_reject_non_digits() {
    let mut p: Parser = Parser::new(false);
    let input = "3008;start=myctx;pid=4_2;pidfdid=+42;status=-1";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    let Command::ContextSignal(cs) = cmd else {
        unreachable!()
    };
    assert!(cs.read_option(Field::Pid).as_int().is_none());
    assert!(cs.read_option(Field::Pidfdid).as_int().is_none());
    assert!(cs.read_option(Field::Status).as_int().is_none());
}

#[test]
fn osc_3008_unknown_fields_are_ignored() {
    let mut p: Parser = Parser::new(false);
    let input = "3008;start=myctx;type=shell;unknownfield=value;user=root";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    let Command::ContextSignal(cs) = cmd else {
        unreachable!()
    };
    assert_eq!(cs.read_option(Field::Type).as_type().unwrap(), ContextType::Shell);
    assert_eq!(cs.read_option(Field::User).as_str().unwrap().raw, b"root".as_slice());
}

#[test]
fn osc_3008_missing_field_returns_null() {
    let mut p: Parser = Parser::new(false);
    let input = "3008;start=myctx;user=lennart";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    let Command::ContextSignal(cs) = cmd else {
        unreachable!()
    };
    assert!(cs.read_option(Field::Type).is_null());
    assert!(cs.read_option(Field::Hostname).is_null());
    assert!(cs.read_option(Field::Pid).is_null());
}

#[test]
fn osc_3008_invalid_prefix() {
    let mut p: Parser = Parser::new(false);
    let input = "3008;bogus=abc123";
    for ch in input.bytes() {
        p.next(ch);
    }
    assert!(p.end(None).is_none());
}

#[test]
fn osc_3008_empty_data() {
    // Can't really produce empty data after "3008;" because the state machine
    // won't write a writer for that case, but we test the edge case where
    // only "start=" is present with no ID.
    let mut p: Parser = Parser::new(false);
    let input = "3008;start=";
    for ch in input.bytes() {
        p.next(ch);
    }
    assert!(p.end(None).is_none());
}

#[test]
fn osc_3008_max_length_context_id() {
    let mut p: Parser = Parser::new(false);
    let id = "a".repeat(64);
    let input = format!("3008;start={id}");
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    let Command::ContextSignal(cs) = cmd else {
        unreachable!()
    };
    assert_eq!(cs.id, id.as_bytes());
}

#[test]
fn osc_3008_over_length_context_id() {
    let mut p: Parser = Parser::new(false);
    let id = "a".repeat(65);
    let input = format!("3008;start={id}");
    for ch in input.bytes() {
        p.next(ch);
    }
    assert!(p.end(None).is_none());
}

#[test]
fn osc_3008_context_type_enum_coverage() {
    struct Case {
        str: &'static [u8],
        expected: ContextType,
    }
    let types = [
        Case {
            str: b"boot",
            expected: ContextType::Boot,
        },
        Case {
            str: b"container",
            expected: ContextType::Container,
        },
        Case {
            str: b"vm",
            expected: ContextType::Vm,
        },
        Case {
            str: b"elevate",
            expected: ContextType::Elevate,
        },
        Case {
            str: b"chpriv",
            expected: ContextType::Chpriv,
        },
        Case {
            str: b"subcontext",
            expected: ContextType::Subcontext,
        },
        Case {
            str: b"remote",
            expected: ContextType::Remote,
        },
        Case {
            str: b"shell",
            expected: ContextType::Shell,
        },
        Case {
            str: b"command",
            expected: ContextType::Command,
        },
        Case {
            str: b"app",
            expected: ContextType::App,
        },
        Case {
            str: b"service",
            expected: ContextType::Service,
        },
        Case {
            str: b"session",
            expected: ContextType::Session,
        },
    ];

    for t in types {
        assert_eq!(t.expected, ContextType::parse(t.str).unwrap());
    }

    assert!(ContextType::parse(b"invalid").is_none());
}

#[test]
fn osc_3008_exit_status_enum_coverage() {
    assert_eq!(ExitStatus::parse(b"success").unwrap(), ExitStatus::Success);
    assert_eq!(ExitStatus::parse(b"failure").unwrap(), ExitStatus::Failure);
    assert_eq!(ExitStatus::parse(b"crash").unwrap(), ExitStatus::Crash);
    assert_eq!(ExitStatus::parse(b"interrupt").unwrap(), ExitStatus::Interrupt);
    assert!(ExitStatus::parse(b"invalid").is_none());
}

#[test]
fn osc_3008_spec_example_container_start() {
    // From the spec: a new container "foobar" invoked by user "lennart" on host "zeta"
    let mut p: Parser = Parser::new(false);
    let input = "3008;start=bed86fab93af4328bbed0a1224af6d40;type=container;user=lennart;hostname=zeta;machineid=3deb5353d3ba43d08201c136a47ead7b;bootid=d4a3d0fdf2e24fdea6d971ce73f4fbf2;pid=1062862;pidfdid=1063162;comm=systemd-nspawn;container=foobar";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    let Command::ContextSignal(cs) = cmd else {
        unreachable!()
    };
    assert_eq!(cs.action, super::Action::Start);
    assert_eq!(cs.id, b"bed86fab93af4328bbed0a1224af6d40".as_slice());
    assert_eq!(cs.read_option(Field::Type).as_type().unwrap(), ContextType::Container);
    assert_eq!(cs.read_option(Field::User).as_str().unwrap().raw, b"lennart".as_slice());
    assert_eq!(
        cs.read_option(Field::Hostname).as_str().unwrap().raw,
        b"zeta".as_slice()
    );
    assert_eq!(
        cs.read_option(Field::Comm).as_str().unwrap().raw,
        b"systemd-nspawn".as_slice()
    );
    assert_eq!(
        cs.read_option(Field::Container).as_str().unwrap().raw,
        b"foobar".as_slice()
    );
    assert_eq!(cs.read_option(Field::Pid).as_int().unwrap(), 1_062_862_u64);
}

#[test]
fn osc_3008_spec_example_context_end() {
    // From the spec: context end
    let mut p: Parser = Parser::new(false);
    let input = "3008;end=bed86fab93af4328bbed0a1224af6d40";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    let Command::ContextSignal(cs) = cmd else {
        unreachable!()
    };
    assert_eq!(cs.action, super::Action::End);
    assert_eq!(cs.id, b"bed86fab93af4328bbed0a1224af6d40".as_slice());
}

#[test]
fn osc_3008_cwd_and_cmdline_fields() {
    let mut p: Parser = Parser::new(false);
    let input = "3008;start=myctx;type=command;cwd=/home/user;cmdline=ls -la";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    let Command::ContextSignal(cs) = cmd else {
        unreachable!()
    };
    assert_eq!(
        cs.read_option(Field::Cwd).as_str().unwrap().raw,
        b"/home/user".as_slice()
    );
    assert_eq!(
        cs.read_option(Field::Cmdline).as_str().unwrap().raw,
        b"ls -la".as_slice()
    );
}

#[test]
fn osc_3008_start_command_with_no_fields() {
    let mut p: Parser = Parser::new(false);
    let input = "3008;start=simpleid";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    let Command::ContextSignal(cs) = cmd else {
        unreachable!()
    };
    assert_eq!(cs.action, super::Action::Start);
    assert_eq!(cs.id, b"simpleid".as_slice());
    assert!(cs.read_option(Field::Type).is_null());
    assert!(cs.read_option(Field::User).is_null());
    assert!(cs.read_option(Field::Exit).is_null());
}

// No Ghostty test: each rejection arm of `parse` logs the
// Ghostty's warning (context_signal.zig:219, :234, :245, :256) under the
// `osc_context_signal` scope, and the command is still rejected.
fn rejected_with(input: &[u8]) -> Vec<crate::log_capture::CapturedRecord> {
    let mut p: Parser = Parser::new(false);
    let cap = crate::log_capture::start();
    for &ch in input {
        p.next(ch);
    }
    assert!(p.end(None).is_none(), "{input:?} must be rejected");
    let messages = cap.messages();
    drop(cap);
    messages
}

fn warn(msg: &str) -> crate::log_capture::CapturedRecord {
    (tracing::Level::WARN, "osc_context_signal".to_string(), msg.to_string())
}

#[test]
fn osc_3008_rejections_log_the_reference_warnings() {
    assert_eq!(
        vec![warn("OSC 3008: expected 'start=' or 'end=' prefix, got: middle=abc")],
        rejected_with(b"3008;middle=abcdefgh")
    );
    assert_eq!(
        vec![warn("OSC 3008: missing context ID")],
        rejected_with(b"3008;start=")
    );
    assert_eq!(
        vec![warn("OSC 3008: context ID length 0 out of range (1-64)")],
        rejected_with(b"3008;end=;type=boot")
    );
    let mut long = b"3008;start=".to_vec();
    long.extend(std::iter::repeat_n(b'a', 65));
    assert_eq!(
        vec![warn("OSC 3008: context ID length 65 out of range (1-64)")],
        rejected_with(&long)
    );
    assert_eq!(
        vec![warn("OSC 3008: invalid character 0x7f in context ID")],
        rejected_with(b"3008;start=ab\x7fcd")
    );
}

/// The metadata of the context signal `input` (an OSC 3008 without its introducer).
fn metadata(input: &str) -> Vec<u8> {
    let mut p: Parser = Parser::new(false);
    for ch in input.bytes() {
        p.next(ch);
    }
    let Some(Command::ContextSignal(cs)) = p.end(None) else {
        panic!("expected a context signal");
    };
    cs.metadata.to_vec()
}

/// Drift `46d6c3b50`: string fields decode `\x3b` and `\x5c`.
#[test]
fn osc_3008_string_unescaping() {
    let metadata = metadata("3008;start=myctx;cmdline=echo \\x3b \\x5c;cwd=\\x5csystem07\\x5cC$\\x5c");
    let cmdline = Field::Cmdline.read(&metadata).as_str().unwrap();
    assert!(cmdline.eql(b"echo ; \\"));
    assert!(!cmdline.eql(b"echo \\x3b \\x5c"));
    assert_eq!(b"echo ; \\".as_slice(), cmdline.to_vec().unwrap());
    let cwd = Field::Cwd.read(&metadata).as_str().unwrap();
    assert!(cwd.eql(b"\\system07\\C$\\"));
    assert_eq!(b"\\system07\\C$\\".as_slice(), cwd.iter().collect::<Vec<u8>>());
}

/// An escape needs all four of its bytes; a cut one and an unknown one stay as they are.
#[test]
fn osc_3008_partial_and_unknown_escapes_stay_literal() {
    let metadata = metadata("3008;start=myctx;comm=a\\x41b\\x3");
    let comm = Field::Comm.read(&metadata).as_str().unwrap();
    assert_eq!(b"a\\x41b\\x3".as_slice(), comm.to_vec().unwrap());
}

/// Every string field reads its own key.
#[test]
fn osc_3008_every_string_field_reads_its_key() {
    let fields = [
        (Field::User, "user"),
        (Field::Hostname, "hostname"),
        (Field::Machineid, "machineid"),
        (Field::Bootid, "bootid"),
        (Field::Comm, "comm"),
        (Field::Cwd, "cwd"),
        (Field::Cmdline, "cmdline"),
        (Field::Vm, "vm"),
        (Field::Container, "container"),
        (Field::Targetuser, "targetuser"),
        (Field::Targethost, "targethost"),
        (Field::Sessionid, "sessionid"),
        (Field::Signal, "signal"),
    ];
    for (field, key) in fields {
        let metadata = metadata(&format!("3008;start=myctx;{key}=v-{key}"));
        let value = field.read(&metadata).as_str().unwrap();
        assert!(value.eql(format!("v-{key}").as_bytes()), "{field:?}");
    }
}
