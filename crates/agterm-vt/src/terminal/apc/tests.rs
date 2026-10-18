use super::*;

#[test]
fn unknown_apc_command() {
    let mut h: Handler = Handler::default();
    h.start();
    for c in *b"Xabcdef1234" {
        h.feed(c);
    }
    assert!(h.end().is_none());
}

#[test]
fn capture_unknown_apc_command() {
    let mut h: Handler = Handler {
        unknown_max_bytes: 5,
        ..Handler::default()
    };
    h.start();
    h.feed_slice(b"abcd;payload");

    let result = h.end().unwrap();
    let Command::Unknown(unknown) = &result else {
        panic!("expected unknown");
    };
    assert_eq!(b"abcd;".as_slice(), unknown.content.as_slice());
    assert!(unknown.truncated);
}

#[test]
fn capture_short_unknown_apc_command() {
    let mut h: Handler = Handler {
        unknown_max_bytes: 16,
        ..Handler::default()
    };
    h.start();
    h.feed(b'X');

    let mut result = h.end().unwrap();
    {
        let Command::Unknown(unknown) = &result else {
            panic!("expected unknown");
        };
        assert_eq!(b"X".as_slice(), unknown.content.as_slice());
        assert!(!unknown.truncated);
    }

    h.unknown_max_bytes = 1;
    h.start();
    h.feed_slice(b"XYZ");
    result = h.end().unwrap();
    {
        let Command::Unknown(truncated) = &result else {
            panic!("expected unknown");
        };
        assert_eq!(b"X".as_slice(), truncated.content.as_slice());
        assert!(truncated.truncated);
    }
}

#[test]
fn disabled_known_apc_protocol_is_not_unknown() {
    let mut h: Handler = Handler {
        unknown_max_bytes: 64,
        ..Handler::default()
    };
    h.start();
    h.feed_slice(b"25a1;q;cp=E0A0");
    assert!(h.end().is_none());

    // An incomplete known protocol identifier is malformed, not unknown.
    h.start();
    h.feed_slice(b"25a");
    assert!(h.end().is_none());
}

#[test]
fn garbage_kitty_command() {
    let mut h: Handler = Handler::default();
    h.start();
    for c in *b"Gabcdef1234" {
        h.feed(c);
    }
    assert!(h.end().is_none());
}

#[test]
fn kitty_command_with_overflow_u32() {
    let mut h: Handler = Handler::default();
    h.start();
    for c in *b"Ga=p,i=10000000000" {
        h.feed(c);
    }
    assert!(h.end().is_none());
}

#[test]
fn kitty_command_with_overflow_i32() {
    let mut h: Handler = Handler::default();
    h.start();
    for c in *b"Ga=p,i=1,z=-9999999999" {
        h.feed(c);
    }
    assert!(h.end().is_none());
}

#[test]
fn kitty_feed_error_deinits_parser() {
    // Feed a valid kitty command start to allocate parser state, then
    // trigger an error during feed via an integer overflow. The testing
    // allocator will detect leaks if deinit is not called.
    let mut h: Handler = Handler::default();
    h.start();
    for c in *b"Ga=p,i=10000000000;" {
        h.feed(c);
    }
    assert!(matches!(h.state, State::Ignore));
}

#[test]
fn kitty_max_bytes_exceeded() {
    let mut h: Handler = Handler {
        max_bytes: {
            let mut mb = [0usize; Protocol::COUNT];
            mb[Protocol::Kitty as usize] = 4;
            mb
        },
        ..Handler::default()
    };
    h.start();
    // 'G' identifies kitty, 'a=t;' moves to data state, then feed exceeds max_bytes.
    for c in *b"Ga=t;" {
        h.feed(c);
    }
    assert!(!matches!(h.state, State::Ignore));
    for c in *b"abcd" {
        h.feed(c);
    }
    assert!(!matches!(h.state, State::Ignore));
    // The 5th data byte exceeds the 4-byte limit.
    h.feed(b'e');
    assert!(matches!(h.state, State::Ignore));
}

#[test]
fn valid_kitty_command() {
    let mut h: Handler = Handler::default();
    h.start();
    let input = *b"Gf=24,s=10,v=20,hello=world";
    for c in input {
        h.feed(c);
    }

    let result = h.end().unwrap();
    assert!(matches!(result, Command::Kitty(_)));
}

#[test]
fn identify_with_unrecognized_command() {
    let mut h: Handler = Handler::default();
    h.start();
    for c in *b"abcd;payload" {
        h.feed(c);
    }
    assert!(h.end().is_none());
}

#[test]
fn identify_buffer_overflow() {
    let mut h: Handler = Handler::default();
    h.start();
    for c in *b"abcde;payload" {
        h.feed(c);
    }
    assert!(h.end().is_none());
}

#[test]
fn identify_with_no_input() {
    let mut h: Handler = Handler::default();
    h.start();
    assert!(h.end().is_none());
}

#[test]
fn identify_with_unknown_partial_input() {
    let mut h: Handler = Handler::default();
    h.start();
    for c in *b"25a" {
        h.feed(c);
    }
    assert!(h.end().is_none());
}

#[test]
fn garbage_glyph_command() {
    let mut h: Handler = Handler::default();
    h.start();
    for c in *b"25a1;X" {
        h.feed(c);
    }

    assert!(h.end().is_none());
}

#[test]
fn feed_slice_valid_kitty_command() {
    let mut h: Handler = Handler::default();
    h.start();
    h.feed_slice(b"Gf=24,s=10,v=20;aGVsbG8=");

    let result = h.end().unwrap();
    assert!(matches!(result, Command::Kitty(_)));

    // The payload is base64-decoded by the parser on completion.
    if let Command::Kitty(ref k) = result {
        assert_eq!(b"hello".as_slice(), k.data.as_slice());
    }
}

#[test]
fn feed_slice_identify_split_across_slices() {
    let mut h: Handler = Handler::default();
    h.start();
    h.feed_slice(b"G");
    h.feed_slice(b"f=24,s=10,");
    h.feed_slice(b"v=20;aGVsbG8=");

    let result = h.end().unwrap();
    assert!(matches!(result, Command::Kitty(_)));

    // The payload is base64-decoded by the parser on completion.
    if let Command::Kitty(ref k) = result {
        assert_eq!(b"hello".as_slice(), k.data.as_slice());
    }
}

#[test]
fn feed_slice_unknown_apc_command_is_ignored() {
    let mut h: Handler = Handler::default();
    h.start();
    h.feed_slice(b"Xabcdef1234");
    assert!(matches!(h.state, State::Ignore));
    h.feed_slice(b"more data that is dropped");
    assert!(h.end().is_none());
}

#[test]
fn feed_slice_kitty_max_bytes_exceeded() {
    let mut h: Handler = Handler {
        max_bytes: {
            let mut mb = [0usize; Protocol::COUNT];
            mb[Protocol::Kitty as usize] = 4;
            mb
        },
        ..Handler::default()
    };
    h.start();
    h.feed_slice(b"Ga=t;abcd");
    assert!(!matches!(h.state, State::Ignore));
    h.feed_slice(b"e");
    assert!(matches!(h.state, State::Ignore));
}

/// `feed_slice` must be observationally identical
/// to `feed`-per-byte for every chunking of the same input — that is the
/// whole claim of `f6f79acce` ("equivalent to calling feed for each byte in
/// order"), and it is the property the union-payload borrow reshape could
/// break silently.
#[test]
fn feed_slice_matches_per_byte_for_every_chunking() {
    let inputs: &[&[u8]] = &[
        b"Gf=24,s=10,v=20;aGVsbG8=",
        b"25a1;q;cp=E0A0",
        b"Xabcdef1234",
        b"G",
        b"25a1;",
        b"",
        b"Ga=t;abcdefghijklmnop",
    ];

    for input in inputs {
        let per_byte_end = {
            let mut h: Handler = Handler::default();
            h.start();
            for &c in *input {
                h.feed(c);
            }
            let e = h.end();
            format!("{}", e.is_some())
        };

        // Every split point, as two slices.
        for split in 0..=input.len() {
            let mut h: Handler = Handler::default();
            h.start();
            h.feed_slice(&input[..split]);
            h.feed_slice(&input[split..]);
            let e = h.end();
            assert_eq!(
                per_byte_end,
                format!("{}", e.is_some()),
                "input={:?} split={split}",
                String::from_utf8_lossy(input)
            );
        }
    }
}

#[test]
fn disabled_glyph_command_is_ignored() {
    let mut h: Handler = Handler::default();
    h.start();
    for c in *b"25a1;q;cp=e0a0" {
        h.feed(c);
    }
    assert!(h.end().is_none());
}
