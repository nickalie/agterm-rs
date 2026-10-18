use super::*;

#[test]
fn transmission_command() {
    let mut p = Parser::new(1024 * 1024);

    let input = b"f=24,s=10,v=20";
    for c in input {
        p.feed(*c).unwrap();
    }
    let command = p.complete().unwrap();

    let Control::Transmit(v) = command.control else {
        unreachable!()
    };
    assert_eq!(Format::Rgb, v.format);
    assert_eq!(10u32, v.width);
    assert_eq!(20u32, v.height);
    assert!(!v.usage.transient);
}

/// `finishValue` parses integer values with Zig's `std.fmt.parseInt`, which skips interior `_` digit separators
/// (rustty refuses them; libghostty-vt accepts them, and this crate answers as libghostty-vt does). A leading or
/// trailing `_` is still refused.
#[test]
fn integer_values_accept_zig_digit_separators() {
    let transmit = |input: &[u8]| match Parser::parse_string(input).unwrap().control {
        Control::Transmit(v) => v,
        other => panic!("expected a transmit command, got {other:?}"),
    };
    assert_eq!(
        (10, 20),
        (transmit(b"f=24,s=10,v=20").width, transmit(b"f=24,s=10,v=20").height)
    );
    assert_eq!(10, transmit(b"f=24,s=1_0,v=20").width);
    assert_eq!(20, transmit(b"f=24,s=10,v=2__0").height);

    // The sign handling stays `std.fmt.parseInt`'s.
    let display = |input: &[u8]| match Parser::parse_string(input).unwrap().control {
        Control::Display(d) => d,
        other => panic!("expected a display command, got {other:?}"),
    };
    assert_eq!(-10, display(b"a=p,i=1,z=-1_0").z);
    let d = display(b"a=p,i=+1,z=-10");
    assert_eq!((1, -10), (d.image_id, d.z));

    assert_eq!(Err(Error::InvalidFormat), Parser::parse_string(b"f=24,s=10_"));
    assert_eq!(Err(Error::InvalidFormat), Parser::parse_string(b"a=p,i=1,z=_1"));
}

#[test]
fn transmission_command_with_transient_hint() {
    let mut p = Parser::new(1024 * 1024);

    let input = b"f=24,s=10,v=20,N=1";
    for c in input {
        p.feed(*c).unwrap();
    }
    let command = p.complete().unwrap();

    let Control::Transmit(v) = command.control else {
        unreachable!()
    };
    assert_eq!(Format::Rgb, v.format);
    assert_eq!(10u32, v.width);
    assert_eq!(20u32, v.height);
    assert!(v.usage.transient);
}

/// Negative control: Ghostty bit-casts a
/// raw `u32` into `Usage`, so the BIT POSITION of `transient` is part of
/// the wire format. Upstream's single `N=1` case cannot distinguish bit 0
/// from "any non-zero value sets it". These cases can.
#[test]
fn transmission_usage_transient_is_bit_zero() {
    fn usage_of(input: &[u8]) -> Usage {
        let mut p = Parser::new(1024 * 1024);
        for c in input {
            p.feed(*c).unwrap();
        }
        match p.complete().unwrap().control {
            Control::Transmit(v) => v.usage,
            _ => unreachable!(),
        }
    }

    // Bit 0 clear: not transient, even though the value is non-zero.
    assert!(!usage_of(b"f=24,s=10,v=20,N=2").transient);
    assert!(!usage_of(b"f=24,s=10,v=20,N=4").transient);
    assert!(!usage_of(b"f=24,s=10,v=20,N=0").transient);
    // Bit 0 set.
    assert!(usage_of(b"f=24,s=10,v=20,N=1").transient);
    assert!(usage_of(b"f=24,s=10,v=20,N=3").transient);

    // Only bit 0 matters.
    assert!(Usage::from_bits(0xffff_ffff).transient);
    assert_eq!(Usage::DEFAULT, Usage::from_bits(0));
}

#[test]
fn feed_slice_matches_per_byte_feed() {
    let input = b"f=24,s=10,v=20;aGVsbG8gd29ybGQ=";

    let mut p1 = Parser::new(1024 * 1024);
    for c in input {
        p1.feed(*c).unwrap();
    }
    let c1 = p1.complete().unwrap();

    let mut p2 = Parser::new(1024 * 1024);
    p2.feed_slice(input).unwrap();
    let c2 = p2.complete().unwrap();

    assert!(matches!(c1.control, Control::Transmit(_)));
    assert!(matches!(c2.control, Control::Transmit(_)));
    assert_eq!(c1.data, c2.data);
}

#[test]
fn feed_slice_across_slice_boundaries() {
    let mut p = Parser::new(1024 * 1024);

    p.feed_slice(b"f=24,s=10").unwrap();
    p.feed_slice(b",v=20;aGVsbG8g").unwrap();
    p.feed_slice(b"d29ybGQ=").unwrap();
    let command = p.complete().unwrap();

    assert!(matches!(command.control, Control::Transmit(_)));

    // The payload is base64-decoded on completion.
    assert_eq!(b"hello world".as_slice(), command.data.as_slice());
}

#[test]
fn feed_slice_respects_max_bytes() {
    let mut p = Parser::new(4);

    p.feed_slice(b"f=24;ab").unwrap();
    assert_eq!(Err(Error::OutOfMemory), p.feed_slice(b"cde"));
}

#[test]
fn transmission_ignores_m_if_medium_is_not_direct() {
    let mut p = Parser::new(1024 * 1024);

    let input = b"a=t,t=t,m=1";
    for c in input {
        p.feed(*c).unwrap();
    }
    let command = p.complete().unwrap();

    let Control::Transmit(v) = command.control else {
        unreachable!()
    };
    assert_eq!(Medium::TemporaryFile, v.medium);
    assert!(!v.more_chunks);
}

#[test]
fn transmission_respects_m_if_medium_is_direct() {
    let mut p = Parser::new(1024 * 1024);

    let input = b"a=t,t=d,m=1";
    for c in input {
        p.feed(*c).unwrap();
    }
    let command = p.complete().unwrap();

    let Control::Transmit(v) = command.control else {
        unreachable!()
    };
    assert_eq!(Medium::Direct, v.medium);
    assert!(v.more_chunks);
}

#[test]
fn query_command() {
    let mut p = Parser::new(1024 * 1024);

    let input = b"i=31,s=1,v=1,a=q,t=d,f=24;QUFBQQ";
    for c in input {
        p.feed(*c).unwrap();
    }
    let command = p.complete().unwrap();

    let Control::Query(v) = command.control else {
        unreachable!()
    };
    assert_eq!(Medium::Direct, v.medium);
    assert_eq!(1u32, v.width);
    assert_eq!(1u32, v.height);
    assert_eq!(31u32, v.image_id);
    assert_eq!(b"AAAA".as_slice(), command.data.as_slice());
}

#[test]
fn display_command() {
    let mut p = Parser::new(1024 * 1024);

    let input = b"a=p,U=1,i=31,c=80,r=120";
    for c in input {
        p.feed(*c).unwrap();
    }
    let command = p.complete().unwrap();

    let Control::Display(v) = command.control else {
        unreachable!()
    };
    assert_eq!(80u32, v.columns);
    assert_eq!(120u32, v.rows);
    assert_eq!(31u32, v.image_id);
}

#[test]
fn delete_command() {
    let mut p = Parser::new(1024 * 1024);

    let input = b"a=d,d=p,x=3,y=4";
    for c in input {
        p.feed(*c).unwrap();
    }
    let command = p.complete().unwrap();

    assert!(matches!(command.control, Control::Delete(_)));
    let v = match command.control {
        Control::Delete(v) => v.action,
        _ => unreachable!(),
    };
    assert!(matches!(v, DeleteAction::IntersectCell { .. }));
    let dv = match v {
        DeleteAction::IntersectCell { delete, x, y } => (delete, x, y),
        _ => unreachable!(),
    };
    assert!(!dv.0);
    assert_eq!(3u32, dv.1);
    assert_eq!(4u32, dv.2);
}

#[test]
fn no_control_data() {
    let mut p = Parser::new(1024 * 1024);

    let input = b";QUFBQQ";
    for c in input {
        p.feed(*c).unwrap();
    }
    let command = p.complete().unwrap();

    assert!(matches!(command.control, Control::Transmit(_)));
    assert_eq!(b"AAAA".as_slice(), command.data.as_slice());
}

#[test]
fn ignore_unknown_keys_long() {
    let mut p = Parser::new(1024 * 1024);

    let input = b"f=24,s=10,v=20,hello=world";
    for c in input {
        p.feed(*c).unwrap();
    }
    let command = p.complete().unwrap();

    let Control::Transmit(v) = command.control else {
        unreachable!()
    };
    assert_eq!(Format::Rgb, v.format);
    assert_eq!(10u32, v.width);
    assert_eq!(20u32, v.height);
}

#[test]
fn ignore_unknown_keys_non_letter() {
    let mut p = Parser::new(1024 * 1024);

    let input = b"f=24,s=10,v=20,!=1";
    for c in input {
        p.feed(*c).unwrap();
    }
    let command = p.complete().unwrap();

    let Control::Transmit(v) = command.control else {
        unreachable!()
    };
    assert_eq!(Format::Rgb, v.format);
    assert_eq!(10u32, v.width);
    assert_eq!(20u32, v.height);
}

#[test]
fn ignore_very_long_values() {
    let mut p = Parser::new(1024 * 1024);

    let input = b"f=24,s=10,v=2000000000000000000000000000000000000000";
    for c in input {
        p.feed(*c).unwrap();
    }
    let command = p.complete().unwrap();

    let Control::Transmit(v) = command.control else {
        unreachable!()
    };
    assert_eq!(Format::Rgb, v.format);
    assert_eq!(10u32, v.width);
    assert_eq!(0u32, v.height);
}

#[test]
fn ensure_very_large_negative_values_dont_get_skipped() {
    let mut p = Parser::new(1024 * 1024);

    let input = b"a=p,i=1,z=-2000000000";
    for c in input {
        p.feed(*c).unwrap();
    }
    let command = p.complete().unwrap();

    let Control::Display(v) = command.control else {
        unreachable!()
    };
    assert_eq!(1, v.image_id);
    assert_eq!(-2_000_000_000, v.z);
}

#[test]
fn ensure_proper_overflow_error_for_u32() {
    let mut p = Parser::new(1024 * 1024);

    let input = b"a=p,i=10000000000";
    for c in input {
        p.feed(*c).unwrap();
    }
    assert_eq!(Err(Error::Overflow), p.complete());
}

/// The rows that pin the error *ordering* are the ones with a nonzero
/// leading digit followed by a non-digit byte (`"-1a"`, `"-1 "`):
/// Ghostty returns `Overflow` from the first digit, before the scan
/// ever reaches the offending byte, so an implementation that classified
/// the whole magnitude first would report an invalid-digit error instead.
#[test]
fn parse_int_u32_matches_reference_parse_int_with_sign() {
    // (input, expected)
    let cases: &[(&[u8], Result<u32, Error>)] = &[
        // Nonzero magnitude: the first digit fails `math.cast(u32, -digit)`.
        (b"-1", Err(Error::Overflow)),
        (b"-01", Err(Error::Overflow)),
        (b"-0000000001", Err(Error::Overflow)),
        (b"-2147483648", Err(Error::Overflow)),
        (b"-4294967295", Err(Error::Overflow)),
        // Magnitude that itself overflows u32: still the first digit.
        (b"-9999999999", Err(Error::Overflow)),
        // ORDERING: the first nonzero digit wins over the later garbage.
        (b"-1a", Err(Error::Overflow)),
        (b"-1 ", Err(Error::Overflow)),
        // `_` separators are skipped between digits, so the first nonzero digit still wins.
        (b"-1_2", Err(Error::Overflow)),
        // All-zero magnitudes survive the negation and yield 0.
        (b"-0", Ok(0)),
        (b"-000", Ok(0)),
        (b"-0_0", Ok(0)),
        (b"-00_00", Ok(0)),
        // `charToDigit` fires first when no nonzero digit precedes.
        (b"-0a", Err(Error::InvalidFormat)),
        (b"-a1", Err(Error::InvalidFormat)),
        (b"- 1", Err(Error::InvalidFormat)),
        // Exactly one sign byte is consumed, so a second sign is a digit
        // error, not an overflow.
        (b"--1", Err(Error::InvalidFormat)),
        (b"-+1", Err(Error::InvalidFormat)),
        // Empty magnitude (bare `-`).
        (b"-", Err(Error::InvalidFormat)),
        // Leading/trailing `_` is rejected before the digit loop runs,
        // so it beats the first-digit overflow.
        (b"-_1", Err(Error::InvalidFormat)),
        (b"-1_", Err(Error::InvalidFormat)),
        (b"-0_", Err(Error::InvalidFormat)),
        // Non-negative path, unchanged.
        (b"10", Ok(10)),
        (b"+10", Ok(10)),
        (b"4294967296", Err(Error::Overflow)),
        (b"", Err(Error::InvalidFormat)),
    ];

    for (input, want) in cases {
        assert_eq!(
            *want,
            crate::fmt::parse_int::<u32>(input, 10).map_err(Error::from),
            "input: {:?}",
            std::str::from_utf8(input)
        );
    }
}

/// The same negative-magnitude classification, observed through the real
/// parser (values are capped at `kv_temp`'s 11 bytes).
#[test]
fn ensure_proper_overflow_error_for_negative_u32() {
    for input in [
        b"a=p,i=-1".as_slice(),
        b"a=p,p=-1".as_slice(),
        // 11 bytes, the exact `kv_temp` capacity (longer values are
        // dropped by `accumulate_value` before they ever reach the parse).
        b"a=p,i=-4294967295".as_slice(),
        // Magnitude that itself overflows u32: same reported error.
        b"a=p,i=-9999999999".as_slice(),
        // ORDERING: `Overflow` from the leading `1`, not `InvalidFormat`
        // from the trailing non-digit.
        b"a=p,i=-1a".as_slice(),
        b"a=p,i=-1 ".as_slice(),
        // `_` is skipped, so the leading `1` overflows.
        b"a=p,i=-1_2".as_slice(),
    ] {
        let mut p = Parser::new(1024 * 1024);
        for c in input {
            p.feed(*c).unwrap();
        }
        assert_eq!(Err(Error::Overflow), p.complete(), "input: {input:?}");
    }
}

#[test]
fn negative_zero_is_accepted_for_u32() {
    for input in [
        b"a=p,i=-0,q=2".as_slice(),
        b"a=p,i=-000,q=2".as_slice(),
        b"a=p,i=-0_0,q=2".as_slice(),
    ] {
        let mut p = Parser::new(1024 * 1024);
        for c in input {
            p.feed(*c).unwrap();
        }
        let command = p.complete().unwrap();
        let Control::Display(v) = command.control else {
            unreachable!()
        };
        assert_eq!(0, v.image_id, "input: {input:?}");
    }
}

#[test]
fn invalid_format_cases_for_negative_u32() {
    for input in [
        b"a=p,i=--1".as_slice(),
        b"a=p,i=-+1".as_slice(),
        b"a=p,i=-a1".as_slice(),
        b"a=p,i=-0a".as_slice(),
        b"a=p,i=- 1".as_slice(),
        // NOTE: a bare `i=-` never reaches the integer parse at all: like
        // Ghostty, `finish_value` short-circuits a single non-digit
        // byte and stores it verbatim. `parse_int_u32(b"-")` is covered by
        // the table test above instead.
        b"a=p,i=-_1".as_slice(),
        b"a=p,i=-1_".as_slice(),
    ] {
        let mut p = Parser::new(1024 * 1024);
        for c in input {
            p.feed(*c).unwrap();
        }
        assert_eq!(Err(Error::InvalidFormat), p.complete(), "input: {input:?}");
    }
}

#[test]
fn ensure_proper_overflow_error_for_i32() {
    let mut p = Parser::new(1024 * 1024);

    let input = b"a=p,i=1,z=-9999999999";
    for c in input {
        p.feed(*c).unwrap();
    }
    assert_eq!(Err(Error::Overflow), p.complete());
}

#[test]
fn all_i32_values() {
    {
        // 'z' (usually z-axis values)
        let mut p = Parser::new(1024 * 1024);
        let input = b"a=p,i=1,z=-1";
        for c in input {
            p.feed(*c).unwrap();
        }
        let command = p.complete().unwrap();

        let Control::Display(v) = command.control else {
            unreachable!()
        };
        assert_eq!(1, v.image_id);
        assert_eq!(-1, v.z);
    }

    {
        // 'H' (relative placement, horizontal offset)
        let mut p = Parser::new(1024 * 1024);
        let input = b"a=p,i=1,H=-1";
        for c in input {
            p.feed(*c).unwrap();
        }
        let command = p.complete().unwrap();

        let Control::Display(v) = command.control else {
            unreachable!()
        };
        assert_eq!(1, v.image_id);
        assert_eq!(-1, v.horizontal_offset);
    }

    {
        // 'V' (relative placement, vertical offset)
        let mut p = Parser::new(1024 * 1024);
        let input = b"a=p,i=1,V=-1";
        for c in input {
            p.feed(*c).unwrap();
        }
        let command = p.complete().unwrap();

        let Control::Display(v) = command.control else {
            unreachable!()
        };
        assert_eq!(1, v.image_id);
        assert_eq!(-1, v.vertical_offset);
    }
}

#[test]
fn response_encode_nothing_without_id_or_image_number() {
    let mut buf: Vec<u8> = Vec::new();

    let r: Response = Response::default();
    r.encode(&mut buf).unwrap();
    assert_eq!(b"".as_slice(), buf.as_slice());
}

#[test]
fn response_encode_with_only_image_id() {
    let mut buf: Vec<u8> = Vec::new();

    let r: Response = Response {
        id: 4,
        ..Default::default()
    };
    r.encode(&mut buf).unwrap();
    assert_eq!(b"\x1b_Gi=4;OK\x1b\\".as_slice(), buf.as_slice());
}

#[test]
fn response_encode_with_only_image_number() {
    let mut buf: Vec<u8> = Vec::new();

    let r: Response = Response {
        image_number: 4,
        ..Default::default()
    };
    r.encode(&mut buf).unwrap();
    assert_eq!(b"\x1b_GI=4;OK\x1b\\".as_slice(), buf.as_slice());
}

#[test]
fn response_encode_with_image_id_and_number() {
    let mut buf: Vec<u8> = Vec::new();

    let r: Response = Response {
        id: 12,
        image_number: 4,
        ..Default::default()
    };
    r.encode(&mut buf).unwrap();
    assert_eq!(b"\x1b_Gi=12,I=4;OK\x1b\\".as_slice(), buf.as_slice());
}

#[test]
fn delete_range_command_1() {
    let mut p = Parser::new(1024 * 1024);

    let input = b"a=d,d=r,x=3,y=4";
    for c in input {
        p.feed(*c).unwrap();
    }
    let command = p.complete().unwrap();

    assert!(matches!(command.control, Control::Delete(_)));
    let v = match command.control {
        Control::Delete(v) => v.action,
        _ => unreachable!(),
    };
    assert!(matches!(v, DeleteAction::Range { .. }));
    let range = match v {
        DeleteAction::Range { delete, first, last } => (delete, first, last),
        _ => unreachable!(),
    };
    assert!(!range.0);
    assert_eq!(3u32, range.1);
    assert_eq!(4u32, range.2);
}

#[test]
fn delete_range_command_2() {
    let mut p = Parser::new(1024 * 1024);

    let input = b"a=d,d=R,x=5,y=11";
    for c in input {
        p.feed(*c).unwrap();
    }
    let command = p.complete().unwrap();

    assert!(matches!(command.control, Control::Delete(_)));
    let v = match command.control {
        Control::Delete(v) => v.action,
        _ => unreachable!(),
    };
    assert!(matches!(v, DeleteAction::Range { .. }));
    let range = match v {
        DeleteAction::Range { delete, first, last } => (delete, first, last),
        _ => unreachable!(),
    };
    assert!(range.0);
    assert_eq!(5u32, range.1);
    assert_eq!(11u32, range.2);
}

#[test]
fn delete_range_command_3() {
    // An inverted range is accepted; it simply matches no images. This
    // matches kitty, which does no validation and filters with
    // `x <= image_id <= y`.
    let mut p = Parser::new(1024 * 1024);

    let input = b"a=d,d=R,x=5,y=4";
    for c in input {
        p.feed(*c).unwrap();
    }
    let command = p.complete().unwrap();

    assert!(matches!(command.control, Control::Delete(_)));
    let v = match command.control {
        Control::Delete(v) => v.action,
        _ => unreachable!(),
    };
    assert!(matches!(v, DeleteAction::Range { .. }));
    let range = match v {
        DeleteAction::Range { delete, first, last } => (delete, first, last),
        _ => unreachable!(),
    };
    assert!(range.0);
    assert_eq!(5u32, range.1);
    assert_eq!(4u32, range.2);
}

#[test]
fn delete_range_command_4() {
    // An omitted upper bound defaults to zero, which matches no images
    // since image IDs are always non-zero.
    let mut p = Parser::new(1024 * 1024);

    let input = b"a=d,d=R,x=5";
    for c in input {
        p.feed(*c).unwrap();
    }
    let command = p.complete().unwrap();

    assert!(matches!(command.control, Control::Delete(_)));
    let v = match command.control {
        Control::Delete(v) => v.action,
        _ => unreachable!(),
    };
    assert!(matches!(v, DeleteAction::Range { .. }));
    let range = match v {
        DeleteAction::Range { delete, first, last } => (delete, first, last),
        _ => unreachable!(),
    };
    assert!(range.0);
    assert_eq!(5u32, range.1);
    assert_eq!(0u32, range.2);
}

#[test]
fn delete_range_command_5() {
    let mut p = Parser::new(1024 * 1024);

    let input = b"a=d,d=R,y=5";
    for c in input {
        p.feed(*c).unwrap();
    }
    let command = p.complete().unwrap();

    assert!(matches!(command.control, Control::Delete(_)));
    let v = match command.control {
        Control::Delete(v) => v.action,
        _ => unreachable!(),
    };
    assert!(matches!(v, DeleteAction::Range { .. }));
    let range = match v {
        DeleteAction::Range { delete, first, last } => (delete, first, last),
        _ => unreachable!(),
    };
    assert!(range.0);
    assert_eq!(0u32, range.1);
    assert_eq!(5u32, range.2);
}

#[test]
fn delete_range_command_6() {
    // Both bounds omitted defaults to the empty range [0, 0].
    let mut p = Parser::new(1024 * 1024);

    let input = b"a=d,d=r";
    for c in input {
        p.feed(*c).unwrap();
    }
    let command = p.complete().unwrap();

    assert!(matches!(command.control, Control::Delete(_)));
    let v = match command.control {
        Control::Delete(v) => v.action,
        _ => unreachable!(),
    };
    assert!(matches!(v, DeleteAction::Range { .. }));
    let range = match v {
        DeleteAction::Range { delete, first, last } => (delete, first, last),
        _ => unreachable!(),
    };
    assert!(!range.0);
    assert_eq!(0u32, range.1);
    assert_eq!(0u32, range.2);
}

// Kitty range-checks non-flag keys while handling the command, not while
// parsing, so a bad value never drops the command (which would make a
// response impossible even when the client gave an id). The flag keys
// "a", "d", "t" and "o" are the exception.

#[test]
fn unknown_format_value_is_deferred_to_execution() {
    let command = Parser::parse_string(b"a=t,f=42,i=31,s=1,v=1").unwrap();

    let Control::Transmit(v) = command.control else {
        unreachable!()
    };
    assert!(v.format_unknown);
    assert_eq!(31u32, v.image_id);
}

#[test]
fn zero_format_value_is_rgba() {
    let command = Parser::parse_string(b"a=t,f=0,i=31,s=1,v=1").unwrap();

    let Control::Transmit(v) = command.control else {
        unreachable!()
    };
    assert!(!v.format_unknown);
    assert_eq!(Format::Rgba, v.format);
}

#[test]
fn known_format_values_are_not_unknown() {
    for (input, want) in [
        (b"f=24".as_slice(), Format::Rgb),
        (b"f=32".as_slice(), Format::Rgba),
        (b"f=100".as_slice(), Format::Png),
    ] {
        let command = Parser::parse_string(input).unwrap();

        let Control::Transmit(v) = command.control else {
            unreachable!()
        };
        assert!(!v.format_unknown);
        assert_eq!(want, v.format);
    }
}

#[test]
fn quiet_value_above_two_suppresses_all_responses() {
    let command = Parser::parse_string(b"a=t,q=3,i=31,s=1,v=1").unwrap();

    assert_eq!(Quiet::Failures, command.quiet);
}

#[test]
fn cursor_movement_value_above_one_moves_the_cursor() {
    let command = Parser::parse_string(b"a=p,i=31,C=2").unwrap();

    let Control::Display(v) = command.control else {
        unreachable!()
    };
    assert_eq!(CursorMovement::After, v.cursor_movement);
}

#[test]
fn virtual_placement_value_above_one_is_virtual() {
    let command = Parser::parse_string(b"a=p,i=31,U=2").unwrap();

    let Control::Display(v) = command.control else {
        unreachable!()
    };
    assert!(v.virtual_placement);
}

#[test]
fn animation_frame_composition_value_above_one_alpha_blends() {
    let command = Parser::parse_string(b"a=f,i=31,X=2").unwrap();

    assert!(matches!(command.control, Control::TransmitAnimationFrame(_)));
    let Control::TransmitAnimationFrame(v) = command.control else {
        unreachable!()
    };
    assert_eq!(CompositionMode::AlphaBlend, v.composition_mode);
}

#[test]
fn animation_compose_value_above_one_overwrites() {
    let command = Parser::parse_string(b"a=c,i=31,C=2").unwrap();

    let Control::ComposeAnimation(v) = command.control else {
        unreachable!()
    };
    assert_eq!(CompositionMode::Overwrite, v.composition_mode);
}

#[test]
fn animation_control_action_above_three_is_ignored() {
    let command = Parser::parse_string(b"a=a,i=31,s=4").unwrap();

    let Control::ControlAnimation(v) = command.control else {
        unreachable!()
    };
    assert_eq!(AnimationAction::Invalid, v.action);
}

#[test]
fn unknown_flag_key_values_still_fail_parsing() {
    // Kitty rejects these while parsing too. See gen/apc_parsers.py.
    for input in [
        b"a=z,i=31".as_slice(),     // action
        b"a=d,d=w,i=31".as_slice(), // delete action
        b"a=t,t=q,i=31".as_slice(), // transmission medium
        b"a=t,o=q,i=31".as_slice(), // compression
    ] {
        assert_eq!(
            Err(Error::InvalidFormat),
            Parser::parse_string(input),
            "input: {input:?}"
        );
    }
}

// Golden-byte KAT (rustty's, no Ghostty test; `4b915e3bc`
// amends existing parse arms without adding `^test` blocks, so this pins
// the amended semantics directly): the a=f/a=a `z` key is parsed by
// `gap_ms` reconstructs it with a bit cast, never a checked narrowing.
#[test]
fn gap_ms_is_a_bit_pattern_cast() {
    // z=-5 → parse_int_i32 = -5i32 → stored as 0xFFFF_FFFB.
    let cmd = Parser::parse_string(b"a=f,z=-5").unwrap();
    let Control::TransmitAnimationFrame(f) = cmd.control else {
        unreachable!()
    };
    assert_eq!(-5i32, f.gap_ms);
    assert_eq!(0xFFFF_FFFBu32, f.gap_ms.cast_unsigned());

    let cmd = Parser::parse_string(b"a=a,i=1,z=-1").unwrap();
    let Control::ControlAnimation(a) = cmd.control else {
        unreachable!()
    };
    assert_eq!(-1i32, a.gap_ms);
    assert_eq!(u32::MAX, a.gap_ms.cast_unsigned());
}

// Golden-byte KAT (rustty's, see above): the `Y` background value
// is a `packed struct(u32) { a, b, g, r }` bit-cast — `a` in the least
// significant byte, `r` in the most significant, i.e. Y=0xRRGGBBAA.
// Y=4278190335 (0xff0000ff) is opaque red per Ghostty doc comment.
#[test]
fn background_bit_cast_is_0x_rrggbbaa() {
    let bg = Background::from_int(4_278_190_335);
    assert_eq!(0xff, bg.r);
    assert_eq!(0x00, bg.g);
    assert_eq!(0x00, bg.b);
    assert_eq!(0xff, bg.a);

    // A distinct-byte pattern pins each lane: 0x11223344 → r=0x11 g=0x22
    // b=0x33 a=0x44.
    let bg = Background::from_int(0x1122_3344);
    assert_eq!((0x11, 0x22, 0x33, 0x44), (bg.r, bg.g, bg.b, bg.a),);

    // Parsed through the real KV path.
    let cmd = Parser::parse_string(b"a=f,Y=4278190335").unwrap();
    let Control::TransmitAnimationFrame(f) = cmd.control else {
        unreachable!()
    };
    assert_eq!(
        Background {
            r: 255,
            g: 0,
            b: 0,
            a: 255
        },
        f.background
    );
}

// No Ghostty test: a payload that is not base64 logs the
// Ghostty's warning (graphics_command.zig:281) and fails with
// `InvalidData`.
#[test]
fn invalid_base64_payload_logs_a_warning() {
    let cap = crate::log_capture::start();
    let result = Parser::parse_string(b"a=t,f=24,s=1,v=1;!!!!");
    let messages = cap.messages();
    drop(cap);
    assert!(matches!(result, Err(Error::InvalidData)), "{result:?}");
    assert_eq!(1, messages.len(), "{messages:?}");
    assert_eq!(tracing::Level::WARN, messages[0].0);
    assert_eq!("kitty_gfx", messages[0].1);
    assert!(
        messages[0].2.starts_with("failed to decode base64 payload data: "),
        "{messages:?}"
    );
}

/// The command of `input`, the APC payload after its `G`.
fn command(input: &[u8]) -> Command {
    let mut p = Parser::new(1024 * 1024);
    p.feed_slice(input).unwrap();
    p.complete().unwrap()
}

/// A command, its action, and the image id, image number and placement id it names.
type IdentifiersCase = (&'static [u8], Action, (u32, u32, u32));

/// Each action key gives its control, and every control answers the image and placement it names.
#[test]
fn control_actions_and_identifiers() {
    let cases: [IdentifiersCase; 8] = [
        (b"a=q,i=1,I=2,p=3", Action::Query, (1, 2, 3)),
        (b"a=t,i=4,I=5", Action::Transmit, (4, 5, 0)),
        (b"a=T,i=6,p=7", Action::TransmitAndDisplay, (6, 0, 7)),
        (b"a=p,i=8,p=9", Action::Display, (8, 0, 9)),
        (b"a=d,d=i,i=10,p=11", Action::Delete, (10, 0, 11)),
        (b"a=f,i=12", Action::TransmitAnimationFrame, (12, 0, 0)),
        (b"a=a,i=13", Action::ControlAnimation, (13, 0, 0)),
        (b"a=c,i=14", Action::ComposeAnimation, (14, 0, 0)),
    ];
    for (input, action, (image_id, image_number, placement_id)) in cases {
        let control = command(input).control;
        let text = String::from_utf8_lossy(input);
        assert_eq!(action, control.action(), "{text}");
        let want = Identifiers {
            image_id,
            image_number,
            placement_id,
        };
        assert_eq!(want, control.identifiers(), "{text}");
    }
}

/// The transmission and display keys of the actions that carry them, and none of the others.
#[test]
fn transmission_and_display_of_each_action() {
    let cases: [(&[u8], bool, bool); 5] = [
        (b"a=q,i=1", true, false),
        (b"a=t,i=1", true, false),
        (b"a=T,i=1", true, true),
        (b"a=p,i=1", false, true),
        (b"a=d,d=a", false, false),
    ];
    for (input, transmission, display) in cases {
        let cmd = command(input);
        let text = String::from_utf8_lossy(input);
        assert_eq!(transmission, cmd.transmission().is_some(), "{text}");
        assert_eq!(display, cmd.display().is_some(), "{text}");
    }
    assert_eq!(Some(7), command(b"a=f,i=7").transmission().map(|t| t.image_id));
    assert_eq!(Some(3), command(b"a=T,c=3").display().map(|d| d.columns));
}

/// Taking the payload leaves the command without one.
#[test]
fn take_data_empties_the_command() {
    let mut cmd = command(b"a=t;aGVsbG8=");
    assert_eq!(b"hello".as_slice(), cmd.take_data());
    assert!(cmd.take_data().is_empty());
}

/// A response is OK only with the OK message.
#[test]
fn response_ok_is_the_ok_message() {
    let ok = Response {
        message: b"OK",
        ..Response::default()
    };
    assert!(ok.ok());
    let failed = Response {
        message: b"EINVAL: bad",
        ..Response::default()
    };
    assert!(!failed.ok());
}

/// The bytes per pixel of the raw formats.
#[test]
fn format_bytes_per_pixel() {
    for (format, bpp) in [
        (Format::Gray, 1),
        (Format::GrayAlpha, 2),
        (Format::Rgb, 3),
        (Format::Rgba, 4),
    ] {
        assert_eq!(bpp, Transmission::format_bpp(format), "{format:?}");
    }
}
