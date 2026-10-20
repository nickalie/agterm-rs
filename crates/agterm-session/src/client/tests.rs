//! The attach stream's contract as the reader checks it ([`StreamState`]), message by message, on every OS.

use super::*;

const SIZE: GridSize = GridSize { cols: 80, rows: 24 };

fn begin(epoch: u64, offset: u64) -> AttachServerMessage {
    AttachServerMessage::SnapshotBegin {
        epoch,
        offset,
        size: SIZE,
    }
}

fn chunk(epoch: u64, data: &[u8]) -> AttachServerMessage {
    AttachServerMessage::SnapshotChunk {
        epoch,
        data: data.to_vec(),
    }
}

fn end(epoch: u64) -> AttachServerMessage {
    AttachServerMessage::SnapshotEnd { epoch }
}

fn output(offset: u64, data: &[u8]) -> AttachServerMessage {
    AttachServerMessage::Output {
        offset,
        data: data.to_vec(),
    }
}

fn read(clip: &str) -> DelegatedRequest {
    DelegatedRequest::ClipboardRead {
        selection: clip.to_owned(),
        list_only: false,
    }
}

/// A state past the snapshot of `epoch` at `offset`.
fn snapshotted(epoch: u64, offset: u64) -> StreamState {
    let mut state = StreamState::default();
    for message in [begin(epoch, offset), chunk(epoch, b"snap"), end(epoch)] {
        assert!(state.accept(message).unwrap().is_some());
    }
    state
}

fn is_protocol(result: &Result<Option<AttachEvent>, ClientError>) -> bool {
    matches!(result, Err(ClientError::Protocol(_)))
}

#[test]
fn a_snapshot_then_contiguous_output_and_size_changes_pass_in_order() {
    let mut state = StreamState::default();
    assert_eq!(
        state
            .accept(AttachServerMessage::ReplyOwner {
                owner: true,
                generation: 1
            })
            .unwrap(),
        Some(AttachEvent::ReplyOwner {
            owner: true,
            generation: 1
        }),
        "ownership comes before the first snapshot"
    );
    assert_eq!(
        state.accept(begin(1, 100)).unwrap(),
        Some(AttachEvent::SnapshotBegin {
            epoch: 1,
            offset: 100,
            size: SIZE
        })
    );
    assert_eq!(
        state.accept(chunk(1, b"ab")).unwrap(),
        Some(AttachEvent::SnapshotChunk(b"ab".to_vec()))
    );
    assert_eq!(state.accept(end(1)).unwrap(), Some(AttachEvent::SnapshotEnd));
    assert!(state.accept(output(100, b"xyz")).unwrap().is_some());
    let resized = AttachServerMessage::Resized {
        offset: 103,
        size: GridSize { cols: 100, rows: 30 },
    };
    assert!(state.accept(resized).unwrap().is_some());
    assert!(state.accept(output(103, b"!")).unwrap().is_some());
    assert_eq!(state.next, 104);
}

#[test]
fn output_that_is_not_contiguous_or_outside_a_snapshot_breaks_the_stream() {
    assert!(
        is_protocol(&StreamState::default().accept(output(0, b"x"))),
        "no snapshot yet"
    );
    let mut open = StreamState::default();
    open.accept(begin(1, 0)).unwrap();
    assert!(is_protocol(&open.accept(output(0, b"x"))), "inside the snapshot");
    assert!(is_protocol(&snapshotted(1, 10).accept(output(11, b"x"))), "a gap");
    assert!(is_protocol(&snapshotted(1, 10).accept(output(9, b"x"))), "an overlap");
    let resized = AttachServerMessage::Resized { offset: 5, size: SIZE };
    assert!(
        is_protocol(&snapshotted(1, 10).accept(resized)),
        "a size change elsewhere"
    );
}

#[test]
fn each_snapshot_opens_a_later_epoch_and_may_replace_an_open_one() {
    let mut state = snapshotted(1, 0);
    state.accept(output(0, b"abc")).unwrap();
    // a resync: the output restarts at the new snapshot's offset
    state.accept(begin(2, 50)).unwrap();
    state.accept(chunk(2, b"x")).unwrap();
    // a newer epoch replaces a snapshot not yet ended
    state.accept(begin(4, 60)).unwrap();
    assert!(
        is_protocol(&state.accept(chunk(2, b"y"))),
        "a chunk of the replaced snapshot"
    );
    let mut state = snapshotted(3, 0);
    assert!(is_protocol(&state.accept(begin(3, 0))), "the same epoch again");
    assert!(is_protocol(&snapshotted(3, 0).accept(begin(2, 0))), "an earlier epoch");
    assert!(
        is_protocol(&snapshotted(1, 0).accept(end(1))),
        "an end outside a snapshot"
    );
    let mut resynced = snapshotted(1, 0);
    resynced.accept(begin(2, 50)).unwrap();
    resynced.accept(end(2)).unwrap();
    assert!(resynced.accept(output(50, b"z")).unwrap().is_some());
}

#[test]
fn requests_of_a_superseded_epoch_are_dropped_and_of_a_future_one_refused() {
    let mut state = snapshotted(2, 0);
    let superseded = AttachServerMessage::DelegatedRequest {
        id: 7,
        epoch: 1,
        request: read("c"),
    };
    assert_eq!(state.accept(superseded).unwrap(), None);
    assert_eq!(
        state
            .accept(AttachServerMessage::KittyBarrier { id: 3, epoch: 1 })
            .unwrap(),
        None
    );
    let current = AttachServerMessage::DelegatedRequest {
        id: 8,
        epoch: 2,
        request: read("p"),
    };
    assert_eq!(
        state.accept(current).unwrap(),
        Some(AttachEvent::Delegated(Delegated {
            id: 8,
            epoch: 2,
            request: read("p")
        }))
    );
    assert_eq!(
        state
            .accept(AttachServerMessage::KittyBarrier { id: 4, epoch: 2 })
            .unwrap(),
        Some(AttachEvent::KittyBarrier(Barrier { id: 4, epoch: 2 }))
    );
    assert!(is_protocol(
        &state.accept(AttachServerMessage::KittyBarrier { id: 5, epoch: 3 })
    ));
}

#[test]
fn nothing_follows_the_exit_and_a_refusal_after_the_attach_breaks_the_stream() {
    let mut state = snapshotted(1, 0);
    let exit = AttachServerMessage::Exit {
        status: ExitStatus::Exited { code: 0, signal: None },
    };
    assert!(matches!(state.accept(exit).unwrap(), Some(AttachEvent::Exit(_))));
    assert!(state.ended);
    assert!(is_protocol(&state.accept(output(0, b"late"))));
    let refused = AttachServerMessage::Refused {
        reason: AttachRefusal::Gone,
        message: "gone".to_owned(),
    };
    assert!(is_protocol(&snapshotted(1, 0).accept(refused)));
    let mut shut = snapshotted(1, 0);
    assert_eq!(
        shut.accept(AttachServerMessage::Shutdown { reason: None }).unwrap(),
        Some(AttachEvent::Shutdown(None))
    );
    assert!(shut.ended);
}
