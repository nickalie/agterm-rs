use tracing::Level;

use super::{CapturedRecord, start};

/// An event emitted on another thread, with the same target as the one under test, does not enter the capture.
#[test]
fn capture_admits_only_the_owning_thread() {
    let cap = start();
    tracing::warn!(target: "stream", "mine");
    std::thread::scope(|s| {
        s.spawn(|| tracing::warn!(target: "stream", "foreign"));
    });
    let messages = cap.messages();
    drop(cap);
    assert_eq!(vec![(Level::WARN, "stream".to_owned(), "mine".to_owned())], messages);
}

/// Outside a capture events go nowhere, so a later capture starts empty.
#[test]
fn records_emitted_outside_a_capture_are_discarded() {
    tracing::warn!(target: "stream", "before any capture");
    let cap = start();
    let messages = cap.messages();
    drop(cap);
    assert_eq!(Vec::<CapturedRecord>::new(), messages);
}

#[test]
fn levels_targets_and_formatted_messages_are_kept() {
    let cap = start();
    let n = 7;
    tracing::debug!(target: "osc", "value {n}");
    tracing::error!(target: "parser", n, "with a field");
    assert_eq!(
        cap.messages(),
        [
            (Level::DEBUG, "osc".to_owned(), "value 7".to_owned()),
            (Level::ERROR, "parser".to_owned(), "with a field".to_owned()),
        ]
    );
}

/// A callsite first reached on a thread with no capture still reaches a capture on another thread: the anchors keep
/// `tracing-core` from caching the uncaptured thread's "never" for it.
#[test]
fn a_callsite_first_reached_outside_a_capture_still_reaches_one() {
    let cap = start();
    let emit = || tracing::warn!(target: "stream", "shared callsite");
    std::thread::scope(|s| {
        s.spawn(emit);
    });
    emit();
    let messages = cap.messages();
    drop(cap);
    assert_eq!(
        vec![(Level::WARN, "stream".to_owned(), "shared callsite".to_owned())],
        messages
    );
}
