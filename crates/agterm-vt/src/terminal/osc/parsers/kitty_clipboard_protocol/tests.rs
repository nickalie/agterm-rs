use super::{Location, Operation, OptionKey, Status};
use crate::terminal::osc::{Command, Parser};

// No Ghostty test here; `kitty_metadata.zig:76-84`
// locks the same `ValueIterator` semantics. `Option.read` matches a field
// whose trimmed key is EXACTLY the option name, and skips (does not stop
// at) a field with a longer key or no `=`. The trim set is
// `std.ascii.whitespace`, which includes VT (0x0B).
#[test]
fn option_read_uses_the_metadata_value_iterator() {
    use super::OptionValue;
    let str_of = |v: OptionValue<'static>| match v {
        OptionValue::Str(s) => s,
        other => panic!("expected a string option, got {other:?}"),
    };
    assert_eq!(Some(b"B".as_slice()), str_of(OptionKey::Pw.read(b"pwx=1:pw=B")));
    assert_eq!(Some(b"5".as_slice()), str_of(OptionKey::Id.read(b"id:id=5")));
    assert_eq!(
        Some(b"B".as_slice()),
        str_of(OptionKey::Pw.read(b"\x0bpw\x0b=\x0bB\x0b"))
    );
    assert!(matches!(
        OptionKey::Type.read(b"typeX=1:type=read"),
        OptionValue::Type(Some(Operation::Read))
    ));
}

#[test]
fn osc_5522_empty_metadata_and_missing_payload() {
    let mut p: Parser = Parser::new(false);

    let input = "5522;";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    let Command::KittyClipboardProtocol(kcp) = cmd else {
        unreachable!()
    };
    assert_eq!(b"".as_slice(), kcp.metadata);
    assert!(kcp.payload.is_none());
    assert!(kcp.read_option(OptionKey::Id).is_null());
    assert!(kcp.read_option(OptionKey::Loc).is_null());
    assert!(kcp.read_option(OptionKey::Mime).is_null());
    assert!(kcp.read_option(OptionKey::Name).is_null());
    assert!(kcp.read_option(OptionKey::Password).is_null());
    assert!(kcp.read_option(OptionKey::Pw).is_null());
    assert!(kcp.read_option(OptionKey::Status).is_null());
    assert!(kcp.read_option(OptionKey::Type).is_null());
}

#[test]
fn osc_5522_empty_metadata_and_empty_payload() {
    let mut p: Parser = Parser::new(false);

    let input = "5522;;";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    let Command::KittyClipboardProtocol(kcp) = cmd else {
        unreachable!()
    };
    assert_eq!(b"".as_slice(), kcp.metadata);
    assert_eq!(b"".as_slice(), kcp.payload.unwrap());
    assert!(kcp.read_option(OptionKey::Id).is_null());
    assert!(kcp.read_option(OptionKey::Loc).is_null());
    assert!(kcp.read_option(OptionKey::Mime).is_null());
    assert!(kcp.read_option(OptionKey::Name).is_null());
    assert!(kcp.read_option(OptionKey::Password).is_null());
    assert!(kcp.read_option(OptionKey::Pw).is_null());
    assert!(kcp.read_option(OptionKey::Status).is_null());
    assert!(kcp.read_option(OptionKey::Type).is_null());
}

#[test]
fn osc_5522_non_empty_metadata_and_payload() {
    let mut p: Parser = Parser::new(false);

    let input = "5522;type=read;dGV4dC9wbGFpbg==";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    let Command::KittyClipboardProtocol(kcp) = cmd else {
        unreachable!()
    };
    assert_eq!(b"type=read".as_slice(), kcp.metadata);
    assert_eq!(b"dGV4dC9wbGFpbg==".as_slice(), kcp.payload.unwrap());
    assert!(kcp.read_option(OptionKey::Id).is_null());
    assert!(kcp.read_option(OptionKey::Loc).is_null());
    assert!(kcp.read_option(OptionKey::Mime).is_null());
    assert!(kcp.read_option(OptionKey::Name).is_null());
    assert!(kcp.read_option(OptionKey::Password).is_null());
    assert!(kcp.read_option(OptionKey::Pw).is_null());
    assert!(kcp.read_option(OptionKey::Status).is_null());
    assert_eq!(Operation::Read, kcp.read_option(OptionKey::Type).as_type().unwrap());
}

#[test]
fn osc_5522_empty_id() {
    let mut p: Parser = Parser::new(false);

    let input = "5522;id=";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    let Command::KittyClipboardProtocol(kcp) = cmd else {
        unreachable!()
    };
    assert!(kcp.read_option(OptionKey::Id).is_null());
}

#[test]
fn osc_5522_valid_id() {
    let mut p: Parser = Parser::new(false);

    let input = "5522;id=5c076ad9-d36f-4705-847b-d4dbf356cc0d";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    let Command::KittyClipboardProtocol(kcp) = cmd else {
        unreachable!()
    };
    assert_eq!(
        b"5c076ad9-d36f-4705-847b-d4dbf356cc0d".as_slice(),
        kcp.read_option(OptionKey::Id).as_bytes().unwrap()
    );
}

#[test]
fn osc_5522_invalid_id() {
    let mut p: Parser = Parser::new(false);

    let input = "5522;id=*42*";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    let Command::KittyClipboardProtocol(kcp) = cmd else {
        unreachable!()
    };
    assert!(kcp.read_option(OptionKey::Id).is_null());
}

#[test]
fn osc_5522_invalid_status() {
    let mut p: Parser = Parser::new(false);

    let input = "5522;status=BOBR";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    let Command::KittyClipboardProtocol(kcp) = cmd else {
        unreachable!()
    };
    assert!(kcp.read_option(OptionKey::Status).is_null());
}

#[test]
fn osc_5522_valid_status() {
    let mut p: Parser = Parser::new(false);

    let input = "5522;status=DONE";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    let Command::KittyClipboardProtocol(kcp) = cmd else {
        unreachable!()
    };
    assert_eq!(Status::Done, kcp.read_option(OptionKey::Status).as_status().unwrap());
}

#[test]
fn osc_5522_invalid_location() {
    let mut p: Parser = Parser::new(false);

    let input = "5522;loc=bobr";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    let Command::KittyClipboardProtocol(kcp) = cmd else {
        unreachable!()
    };
    assert!(kcp.read_option(OptionKey::Loc).is_null());
}

#[test]
fn osc_5522_valid_location() {
    let mut p: Parser = Parser::new(false);

    let input = "5522;loc=primary";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    let Command::KittyClipboardProtocol(kcp) = cmd else {
        unreachable!()
    };
    assert_eq!(Location::Primary, kcp.read_option(OptionKey::Loc).as_loc().unwrap());
}

#[test]
fn osc_5522_password_1() {
    let mut p: Parser = Parser::new(false);

    let input = "5522;pw=R2hvc3R0eQ==:name=Qk9CUiBLVVJXQQ==";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    let Command::KittyClipboardProtocol(kcp) = cmd else {
        unreachable!()
    };
    assert_eq!(
        b"R2hvc3R0eQ==".as_slice(),
        kcp.read_option(OptionKey::Pw).as_bytes().unwrap()
    );
    assert_eq!(
        b"Qk9CUiBLVVJXQQ==".as_slice(),
        kcp.read_option(OptionKey::Name).as_bytes().unwrap()
    );
}

#[test]
fn osc_5522_password_2() {
    let mut p: Parser = Parser::new(false);

    let input = "5522;password=R2hvc3R0eQ==";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    let Command::KittyClipboardProtocol(kcp) = cmd else {
        unreachable!()
    };
    assert_eq!(
        b"R2hvc3R0eQ==".as_slice(),
        kcp.read_option(OptionKey::Password).as_bytes().unwrap()
    );
}

#[test]
fn osc_5522_example_1() {
    let mut p: Parser = Parser::new(false);

    let input = "5522;type=read:status=OK";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    let Command::KittyClipboardProtocol(kcp) = cmd else {
        unreachable!()
    };
    assert!(kcp.payload.is_none());
    assert!(kcp.read_option(OptionKey::Id).is_null());
    assert!(kcp.read_option(OptionKey::Loc).is_null());
    assert!(kcp.read_option(OptionKey::Mime).is_null());
    assert!(kcp.read_option(OptionKey::Name).is_null());
    assert!(kcp.read_option(OptionKey::Password).is_null());
    assert!(kcp.read_option(OptionKey::Pw).is_null());
    assert_eq!(Status::Ok, kcp.read_option(OptionKey::Status).as_status().unwrap());
    assert_eq!(Operation::Read, kcp.read_option(OptionKey::Type).as_type().unwrap());
}

#[test]
fn osc_5522_example_2() {
    let mut p: Parser = Parser::new(false);

    let input = "5522;type=read:mime=dGV4dC9wbGFpbg==;R2hvc3R0eQ==";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    let Command::KittyClipboardProtocol(kcp) = cmd else {
        unreachable!()
    };
    assert_eq!(b"R2hvc3R0eQ==".as_slice(), kcp.payload.unwrap());
    assert!(kcp.read_option(OptionKey::Id).is_null());
    assert!(kcp.read_option(OptionKey::Loc).is_null());
    assert_eq!(
        b"dGV4dC9wbGFpbg==".as_slice(),
        kcp.read_option(OptionKey::Mime).as_bytes().unwrap()
    );
    assert!(kcp.read_option(OptionKey::Name).is_null());
    assert!(kcp.read_option(OptionKey::Password).is_null());
    assert!(kcp.read_option(OptionKey::Pw).is_null());
    assert!(kcp.read_option(OptionKey::Status).is_null());
    assert_eq!(Operation::Read, kcp.read_option(OptionKey::Type).as_type().unwrap());
}

#[test]
fn osc_5522_example_3() {
    let mut p: Parser = Parser::new(false);

    let input = "5522;type=read:status=OK";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    let Command::KittyClipboardProtocol(kcp) = cmd else {
        unreachable!()
    };
    assert!(kcp.payload.is_none());
    assert!(kcp.read_option(OptionKey::Id).is_null());
    assert!(kcp.read_option(OptionKey::Loc).is_null());
    assert!(kcp.read_option(OptionKey::Mime).is_null());
    assert!(kcp.read_option(OptionKey::Name).is_null());
    assert!(kcp.read_option(OptionKey::Password).is_null());
    assert!(kcp.read_option(OptionKey::Pw).is_null());
    assert_eq!(Status::Ok, kcp.read_option(OptionKey::Status).as_status().unwrap());
    assert_eq!(Operation::Read, kcp.read_option(OptionKey::Type).as_type().unwrap());
}

#[test]
fn osc_5522_example_4() {
    let mut p: Parser = Parser::new(false);

    let input = "5522;type=write";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    let Command::KittyClipboardProtocol(kcp) = cmd else {
        unreachable!()
    };
    assert!(kcp.payload.is_none());
    assert!(kcp.read_option(OptionKey::Id).is_null());
    assert!(kcp.read_option(OptionKey::Loc).is_null());
    assert!(kcp.read_option(OptionKey::Mime).is_null());
    assert!(kcp.read_option(OptionKey::Name).is_null());
    assert!(kcp.read_option(OptionKey::Password).is_null());
    assert!(kcp.read_option(OptionKey::Pw).is_null());
    assert!(kcp.read_option(OptionKey::Status).is_null());
    assert_eq!(Operation::Write, kcp.read_option(OptionKey::Type).as_type().unwrap());
}

#[test]
fn osc_5522_example_5() {
    let mut p: Parser = Parser::new(false);

    let input = "5522;type=wdata:mime=dGV4dC9wbGFpbg==;R2hvc3R0eQ==";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    let Command::KittyClipboardProtocol(kcp) = cmd else {
        unreachable!()
    };
    assert_eq!(b"R2hvc3R0eQ==".as_slice(), kcp.payload.unwrap());
    assert!(kcp.read_option(OptionKey::Id).is_null());
    assert!(kcp.read_option(OptionKey::Loc).is_null());
    assert_eq!(
        b"dGV4dC9wbGFpbg==".as_slice(),
        kcp.read_option(OptionKey::Mime).as_bytes().unwrap()
    );
    assert!(kcp.read_option(OptionKey::Name).is_null());
    assert!(kcp.read_option(OptionKey::Password).is_null());
    assert!(kcp.read_option(OptionKey::Pw).is_null());
    assert!(kcp.read_option(OptionKey::Status).is_null());
    assert_eq!(Operation::Wdata, kcp.read_option(OptionKey::Type).as_type().unwrap());
}

#[test]
fn osc_5522_example_6() {
    let mut p: Parser = Parser::new(false);

    let input = "5522;type=wdata";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    let Command::KittyClipboardProtocol(kcp) = cmd else {
        unreachable!()
    };
    assert!(kcp.payload.is_none());
    assert!(kcp.read_option(OptionKey::Id).is_null());
    assert!(kcp.read_option(OptionKey::Loc).is_null());
    assert!(kcp.read_option(OptionKey::Mime).is_null());
    assert!(kcp.read_option(OptionKey::Name).is_null());
    assert!(kcp.read_option(OptionKey::Password).is_null());
    assert!(kcp.read_option(OptionKey::Pw).is_null());
    assert!(kcp.read_option(OptionKey::Status).is_null());
    assert_eq!(Operation::Wdata, kcp.read_option(OptionKey::Type).as_type().unwrap());
}

#[test]
fn osc_5522_example_7() {
    let mut p: Parser = Parser::new(false);

    let input = "5522;type=write:status=DONE";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    let Command::KittyClipboardProtocol(kcp) = cmd else {
        unreachable!()
    };
    assert!(kcp.payload.is_none());
    assert!(kcp.read_option(OptionKey::Id).is_null());
    assert!(kcp.read_option(OptionKey::Loc).is_null());
    assert!(kcp.read_option(OptionKey::Mime).is_null());
    assert!(kcp.read_option(OptionKey::Name).is_null());
    assert!(kcp.read_option(OptionKey::Password).is_null());
    assert!(kcp.read_option(OptionKey::Pw).is_null());
    assert_eq!(Status::Done, kcp.read_option(OptionKey::Status).as_status().unwrap());
    assert_eq!(Operation::Write, kcp.read_option(OptionKey::Type).as_type().unwrap());
}

#[test]
fn osc_5522_example_8() {
    let mut p: Parser = Parser::new(false);

    let input = "5522;type=write:status=EPERM";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    let Command::KittyClipboardProtocol(kcp) = cmd else {
        unreachable!()
    };
    assert!(kcp.payload.is_none());
    assert!(kcp.read_option(OptionKey::Id).is_null());
    assert!(kcp.read_option(OptionKey::Loc).is_null());
    assert!(kcp.read_option(OptionKey::Mime).is_null());
    assert!(kcp.read_option(OptionKey::Name).is_null());
    assert!(kcp.read_option(OptionKey::Password).is_null());
    assert!(kcp.read_option(OptionKey::Pw).is_null());
    assert_eq!(Status::Eperm, kcp.read_option(OptionKey::Status).as_status().unwrap());
    assert_eq!(Operation::Write, kcp.read_option(OptionKey::Type).as_type().unwrap());
}

#[test]
fn osc_5522_example_9() {
    let mut p: Parser = Parser::new(false);

    let input = "5522;type=walias:mime=dGV4dC9wbGFpbg==;dGV4dC9odG1sIGFwcGxpY2F0aW9uL2pzb24=";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    let Command::KittyClipboardProtocol(kcp) = cmd else {
        unreachable!()
    };
    assert_eq!(b"dGV4dC9odG1sIGFwcGxpY2F0aW9uL2pzb24=".as_slice(), kcp.payload.unwrap());
    assert!(kcp.read_option(OptionKey::Id).is_null());
    assert!(kcp.read_option(OptionKey::Loc).is_null());
    assert_eq!(
        b"dGV4dC9wbGFpbg==".as_slice(),
        kcp.read_option(OptionKey::Mime).as_bytes().unwrap()
    );
    assert!(kcp.read_option(OptionKey::Name).is_null());
    assert!(kcp.read_option(OptionKey::Password).is_null());
    assert!(kcp.read_option(OptionKey::Pw).is_null());
    assert!(kcp.read_option(OptionKey::Status).is_null());
    assert_eq!(Operation::Walias, kcp.read_option(OptionKey::Type).as_type().unwrap());
}

#[test]
fn osc_5522_example_10() {
    let mut p: Parser = Parser::new(false);

    let input = "5522;type=read:status=OK:password=Qk9CUiBLVVJXQQ==";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    let Command::KittyClipboardProtocol(kcp) = cmd else {
        unreachable!()
    };
    assert!(kcp.payload.is_none());
    assert!(kcp.read_option(OptionKey::Id).is_null());
    assert!(kcp.read_option(OptionKey::Loc).is_null());
    assert!(kcp.read_option(OptionKey::Mime).is_null());
    assert!(kcp.read_option(OptionKey::Name).is_null());
    assert_eq!(
        b"Qk9CUiBLVVJXQQ==".as_slice(),
        kcp.read_option(OptionKey::Password).as_bytes().unwrap()
    );
    assert!(kcp.read_option(OptionKey::Pw).is_null());
    assert_eq!(Status::Ok, kcp.read_option(OptionKey::Status).as_status().unwrap());
    assert_eq!(Operation::Read, kcp.read_option(OptionKey::Type).as_type().unwrap());
}

#[test]
fn osc_5522_example_11() {
    let mut p: Parser = Parser::new(false);

    let input = "5522;type=read:status=DATA:mime=dGV4dC9wbGFpbg==";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    let Command::KittyClipboardProtocol(kcp) = cmd else {
        unreachable!()
    };
    assert!(kcp.payload.is_none());
    assert!(kcp.read_option(OptionKey::Id).is_null());
    assert!(kcp.read_option(OptionKey::Loc).is_null());
    assert_eq!(
        b"dGV4dC9wbGFpbg==".as_slice(),
        kcp.read_option(OptionKey::Mime).as_bytes().unwrap()
    );
    assert!(kcp.read_option(OptionKey::Name).is_null());
    assert!(kcp.read_option(OptionKey::Password).is_null());
    assert!(kcp.read_option(OptionKey::Pw).is_null());
    assert_eq!(Status::Data, kcp.read_option(OptionKey::Status).as_status().unwrap());
    assert_eq!(Operation::Read, kcp.read_option(OptionKey::Type).as_type().unwrap());
}

#[test]
fn osc_5522_example_12() {
    let mut p: Parser = Parser::new(false);

    let input = "5522;type=read:mime=dGV4dC9wbGFpbg==:password=Qk9CUiBLVVJXQQ==";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    let Command::KittyClipboardProtocol(kcp) = cmd else {
        unreachable!()
    };
    assert!(kcp.payload.is_none());
    assert!(kcp.read_option(OptionKey::Id).is_null());
    assert!(kcp.read_option(OptionKey::Loc).is_null());
    assert_eq!(
        b"dGV4dC9wbGFpbg==".as_slice(),
        kcp.read_option(OptionKey::Mime).as_bytes().unwrap()
    );
    assert!(kcp.read_option(OptionKey::Name).is_null());
    assert_eq!(
        b"Qk9CUiBLVVJXQQ==".as_slice(),
        kcp.read_option(OptionKey::Password).as_bytes().unwrap()
    );
    assert!(kcp.read_option(OptionKey::Pw).is_null());
    assert!(kcp.read_option(OptionKey::Status).is_null());
    assert_eq!(Operation::Read, kcp.read_option(OptionKey::Type).as_type().unwrap());
}

#[test]
fn osc_5522_example_13() {
    let mut p: Parser = Parser::new(false);

    let input = "5522;type=read:status=OK";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    let Command::KittyClipboardProtocol(kcp) = cmd else {
        unreachable!()
    };
    assert!(kcp.payload.is_none());
    assert!(kcp.read_option(OptionKey::Id).is_null());
    assert!(kcp.read_option(OptionKey::Loc).is_null());
    assert!(kcp.read_option(OptionKey::Mime).is_null());
    assert!(kcp.read_option(OptionKey::Name).is_null());
    assert!(kcp.read_option(OptionKey::Password).is_null());
    assert!(kcp.read_option(OptionKey::Pw).is_null());
    assert_eq!(Status::Ok, kcp.read_option(OptionKey::Status).as_status().unwrap());
    assert_eq!(Operation::Read, kcp.read_option(OptionKey::Type).as_type().unwrap());
}

#[test]
fn osc_5522_example_14() {
    let mut p: Parser = Parser::new(false);

    let input = "5522;type=read:status=DATA:mime=dGV4dC9wbGFpbg==;Qk9CUiBLVVJXQQ==";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    let Command::KittyClipboardProtocol(kcp) = cmd else {
        unreachable!()
    };
    assert_eq!(b"Qk9CUiBLVVJXQQ==".as_slice(), kcp.payload.unwrap());
    assert!(kcp.read_option(OptionKey::Id).is_null());
    assert!(kcp.read_option(OptionKey::Loc).is_null());
    assert_eq!(
        b"dGV4dC9wbGFpbg==".as_slice(),
        kcp.read_option(OptionKey::Mime).as_bytes().unwrap()
    );
    assert!(kcp.read_option(OptionKey::Name).is_null());
    assert!(kcp.read_option(OptionKey::Password).is_null());
    assert!(kcp.read_option(OptionKey::Pw).is_null());
    assert_eq!(Status::Data, kcp.read_option(OptionKey::Status).as_status().unwrap());
    assert_eq!(Operation::Read, kcp.read_option(OptionKey::Type).as_type().unwrap());
}

#[test]
fn osc_5522_example_15() {
    let mut p: Parser = Parser::new(false);

    let input = "5522;type=read:status=OK";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    let Command::KittyClipboardProtocol(kcp) = cmd else {
        unreachable!()
    };
    assert!(kcp.payload.is_none());
    assert!(kcp.read_option(OptionKey::Id).is_null());
    assert!(kcp.read_option(OptionKey::Loc).is_null());
    assert!(kcp.read_option(OptionKey::Mime).is_null());
    assert!(kcp.read_option(OptionKey::Name).is_null());
    assert!(kcp.read_option(OptionKey::Password).is_null());
    assert!(kcp.read_option(OptionKey::Pw).is_null());
    assert_eq!(Status::Ok, kcp.read_option(OptionKey::Status).as_status().unwrap());
    assert_eq!(Operation::Read, kcp.read_option(OptionKey::Type).as_type().unwrap());
}
