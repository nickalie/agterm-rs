use super::{DecodeError, printf_q_decode, url_percent_decode};

fn printf_q(s: &[u8]) -> Result<Vec<u8>, DecodeError> {
    let mut w = Vec::new();
    printf_q_decode(&mut w, s).map(|()| w)
}

fn percent(s: &[u8]) -> Result<Vec<u8>, DecodeError> {
    let mut w = Vec::new();
    url_percent_decode(&mut w, s).map(|()| w)
}

#[test]
fn printf_q_decodes() {
    assert_eq!(printf_q(b"bobr\\ kurwa").unwrap(), b"bobr kurwa");
    assert_eq!(printf_q(b"bobr\\nkurwa").unwrap(), b"bobr\nkurwa");
    assert_eq!(printf_q(b"$'bobr kurwa'").unwrap(), b"bobr kurwa");
    assert_eq!(printf_q(b"'bobr kurwa'").unwrap(), b"bobr kurwa");
    assert_eq!(printf_q(b"\\e\\r\\t\\v\\\\\\\"\\'\\$").unwrap(), b"\x1b\r\t\x0b\\\"'$");
}

#[test]
fn printf_q_refuses_bad_escapes_and_quoting() {
    for s in [
        &b"bobr\\dkurwa"[..],
        b"bobr kurwa\\",
        b"$'bobr kurwa",
        b"$'",
        b"'bobr kurwa",
        b"'",
    ] {
        assert!(matches!(printf_q(s), Err(DecodeError::Decode)), "{s:?}");
    }
}

#[test]
fn singles_percent() {
    for c in 0..=u8::MAX {
        assert_eq!(percent(format!("%{c:02x}").as_bytes()).unwrap(), [c]);
        assert_eq!(percent(format!("%{c:02X}").as_bytes()).unwrap(), [c]);
    }
}

#[test]
fn percent_decodes() {
    assert_eq!(percent(b"bobr%20kurwa").unwrap(), b"bobr kurwa");
    assert_eq!(percent(b"bobr%20kurwa%20").unwrap(), b"bobr kurwa ");
}

#[test]
fn percent_refuses_short_and_bad_escapes() {
    for s in [
        &b"bobr%2kurwa"[..],
        b"bobr%kurwa",
        b"bobr%%kurwa",
        b"bobr%20kurwa%2",
        b"bobr%20kurwa%",
    ] {
        assert!(matches!(percent(s), Err(DecodeError::Decode)), "{s:?}");
    }
}

#[test]
fn a_failing_writer_is_reported() {
    let mut full = [0_u8; 2];
    let mut w = &mut full[..];
    let err = printf_q_decode(&mut w, b"abc");
    assert!(matches!(err, Err(DecodeError::Write(e)) if e.kind() == std::io::ErrorKind::WriteZero));
    let mut full = [0_u8; 1];
    let mut w = &mut full[..];
    let err = url_percent_decode(&mut w, b"%41%42");
    assert!(matches!(err, Err(DecodeError::Write(e)) if e.kind() == std::io::ErrorKind::WriteZero));
}
