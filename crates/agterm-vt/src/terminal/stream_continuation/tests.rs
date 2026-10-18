use super::*;
use crate::mem::testing::{with_counting, with_failing};

#[test]
fn boundary_scanner_classifies_effects_and_ground() {
    let mut scanner = BoundaryScanner::new();

    assert!(scanner.ground());
    assert_eq!(Effect::Committed, scanner.next(b'A'));
    assert!(scanner.ground());

    assert_eq!(Effect::Uncommitted, scanner.next(0x1B));
    assert!(!scanner.ground());
    assert_eq!(Effect::Uncommitted, scanner.next(b'['));
    assert_eq!(Effect::Uncommitted, scanner.next(b'1'));

    // BEL commits an execute action without changing the unfinished CSI.
    assert_eq!(Effect::Omittable, scanner.next(0x07));
    assert!(!scanner.ground());

    assert_eq!(Effect::Committed, scanner.next(b'm'));
    assert!(scanner.ground());
}

#[test]
fn boundary_scanner_preserves_unfinished_builder_actions() {
    let mut apc = BoundaryScanner::new();
    assert_eq!(Effect::Uncommitted, apc.next(0x1B));
    assert_eq!(Effect::Uncommitted, apc.next(b'_'));
    assert_eq!(Effect::Uncommitted, apc.next(b'G'));
    assert_eq!(Effect::Committed, apc.next(0x1B));

    let mut dcs = BoundaryScanner::new();
    assert_eq!(Effect::Uncommitted, dcs.next(0x1B));
    assert_eq!(Effect::Uncommitted, dcs.next(b'P'));
    assert_eq!(Effect::Uncommitted, dcs.next(b'q'));
    assert_eq!(Effect::Uncommitted, dcs.next(b'x'));
    assert_eq!(Effect::Committed, dcs.next(0x1B));
}

#[test]
fn boundary_scanner_handles_utf8_and_malformed_retries() {
    let mut valid = BoundaryScanner::new();
    assert_eq!(Effect::Uncommitted, valid.next(0xF0));
    assert!(!valid.ground());
    assert_eq!(Effect::Uncommitted, valid.next(0x9F));
    assert_eq!(Effect::Uncommitted, valid.next(0x98));
    assert_eq!(Effect::Committed, valid.next(0x84));
    assert!(valid.ground());

    let mut malformed = BoundaryScanner::new();
    assert_eq!(Effect::Uncommitted, malformed.next(0xE0));
    assert_eq!(Effect::Uncommitted, malformed.next(0xA0));
    assert_ne!(malformed.next(0xF0), Effect::Uncommitted);
    assert!(!malformed.ground());
}

#[test]
fn find_vt_replay_start_finds_the_latest_esc_across_vector_boundaries() {
    assert_eq!(None, find_vt_replay_start(b""));
    assert_eq!(None, find_vt_replay_start(b"no escapes here"));
    assert_eq!(Some(0), find_vt_replay_start(b"\x1b"));
    assert_eq!(Some(5), find_vt_replay_start(b"\x1b[1m\x20\x1b[2"));

    // Exercise both the vector loop and the scalar remainder with replay
    // starts at every position of a buffer larger than any vector width.
    let mut buf = [0u8; 193];
    buf.fill(b'a');
    assert_eq!(None, find_vt_replay_start(&buf));
    for idx in 0..buf.len() {
        buf.fill(b'a');
        buf[idx] = 0x1B;
        assert_eq!(Some(idx), find_vt_replay_start(&buf));

        // The latest ESC wins.
        if idx > 0 {
            buf[idx - 1] = 0x1B;
            assert_eq!(Some(idx), find_vt_replay_start(&buf));
        }
    }
}

#[test]
fn find_utf8_replay_start_finds_the_pending_lead_byte() {
    assert_eq!(None, find_utf8_replay_start(b""));
    assert_eq!(Some(4), find_utf8_replay_start(b"text\xF0"));
    assert_eq!(Some(4), find_utf8_replay_start(b"text\xF0\x9F"));
    assert_eq!(Some(4), find_utf8_replay_start(b"text\xF0\x9F\x98"));
    assert_eq!(Some(0), find_utf8_replay_start(b"\xE0\xA0"));

    // A malformed prefix rejected earlier doesn't hide the pending lead.
    assert_eq!(Some(2), find_utf8_replay_start(b"\xE0\xA0\xF0"));

    // Continuation bytes of a sequence that started in an earlier input have
    // no replay start of their own.
    assert_eq!(None, find_utf8_replay_start(b"\x9F"));
    assert_eq!(None, find_utf8_replay_start(b"\x9F\x98"));
}

#[test]
fn tracker_retains_and_normalizes_replay_safe_bytes() {
    let mut tracker = Tracker::new(64);

    tracker.append(Pending::Vt, b"committed\x1b[1\x07");
    tracker.append(Pending::Vt, b";2");
    assert!(!tracker.broken);

    let mut buf = [0u8; 64];
    let mut writer = FixedIoWriter::new(&mut buf);
    tracker.write(&mut writer).unwrap();
    assert_eq!(b"\x1b[1;2".as_slice(), writer.buffered());

    // A feed with a later replay start drops the previous suffix entirely.
    tracker.append(Pending::Vt, b"committed\x1b]0;t");
    let mut replaced_buf = [0u8; 64];
    let mut replaced_writer = FixedIoWriter::new(&mut replaced_buf);
    tracker.write(&mut replaced_writer).unwrap();
    assert_eq!(b"\x1b]0;t".as_slice(), replaced_writer.buffered());

    // An incomplete UTF-8 codepoint seeds at its lead byte and grows with
    // continuation bytes from later feeds.
    tracker.append(Pending::Utf8, b"committed\xF0");
    tracker.append(Pending::Utf8, b"\x9F");
    let mut utf8_buf = [0u8; 4];
    let mut utf8_writer = FixedIoWriter::new(&mut utf8_buf);
    tracker.write(&mut utf8_writer).unwrap();
    assert_eq!(b"\xF0\x9F".as_slice(), utf8_writer.buffered());
}

#[test]
fn tracker_cap_reset_and_broken_recovery() {
    let mut tracker = Tracker::new(4);

    tracker.append(Pending::Vt, b"\x1b[123");
    assert!(tracker.broken);

    // Feeds without a replay start are dropped while broken.
    tracker.append(Pending::Vt, b"4");
    assert!(tracker.broken);
    assert_eq!(0, tracker.bytes.len());

    // Suffixes that grow past the cap break tracking.
    tracker.reset();
    tracker.append(Pending::Vt, b"\x1b[1");
    tracker.append(Pending::Vt, b"23");
    assert!(tracker.broken);

    // A new replay start that fits recovers without a reset.
    tracker.append(Pending::Vt, b"\x1b[");
    assert!(!tracker.broken);
    let mut buf = [0u8; 4];
    let mut writer = FixedIoWriter::new(&mut buf);
    tracker.write(&mut writer).unwrap();
    assert_eq!(b"\x1b[".as_slice(), writer.buffered());

    tracker.append(Pending::Vt, b"\x1b[123");
    assert!(tracker.broken);
    tracker.reset();
    assert!(!tracker.broken);
    let mut empty_buf = [0u8; 1];
    let mut empty_writer = FixedIoWriter::new(&mut empty_buf);
    tracker.write(&mut empty_writer).unwrap();
    assert_eq!(0, empty_writer.end);
}

#[test]
fn tracker_reports_writer_failure_and_defers_allocation_failure() {
    let mut tracker = Tracker::new(64);
    tracker.append(Pending::Vt, b"\x1b[");

    let mut buf = [0u8; 1];
    let mut writer = FixedIoWriter::new(&mut buf);
    assert!(tracker.write(&mut writer).is_err());

    let (mut failing_tracker, _) = with_failing(0, || Tracker::new(64));
    assert!(!failing_tracker.broken);

    // The best-effort initial reservation failed, so the first required retention retries allocation and marks the
    // tracker broken.
    let ((), stats) = with_failing(0, || failing_tracker.append(Pending::Vt, b"\x1b["));
    assert!(stats.has_induced_failure);
    assert!(failing_tracker.broken);
}

/// The tracker reserves `min(max_bytes, 4096)` up front and grows past it; dropping it returns every byte.
#[test]
fn tracker_reserves_up_front_and_frees_on_drop() {
    let ((), counting) = with_counting(|| {
        let mut tracker = Tracker::new(16384);
        assert!(tracker.bytes.capacity() >= 4096);
        let mut input = vec![b'1'; 7000];
        input[0..2].copy_from_slice(b"\x1b[");
        tracker.append(Pending::Vt, &input);
        assert!(!tracker.broken);
        assert_eq!(7000, tracker.bytes.len());
    });
    assert_eq!(0, counting.live_bytes, "every byte is returned");
    assert!(counting.alloc_calls >= 1);
}
