//! The unit tests' log capture: the `tracing` events the calling thread emits, so a test can assert that a code path
//! logged what Ghostty logs there (its `log.warn` and friends are this crate's `tracing` events).
//!
//! Ghostty: none; its tests do not observe logs.
//! rustty: `crates/rustty-vt/src/log_capture.rs` at `RUSTTY_REV` (MIT).
//! Changes: rustty installs one process-wide `log` logger and serializes its capturing tests behind a lock, admitting
//! records only from the thread that holds it. `tracing` has a per-thread default subscriber, so [`start`] sets one
//! for the calling thread instead: records from other threads never reach it, and capturing tests need no lock. As in
//! rustty, a capture sees only what the thread that called [`start`] logs, not threads it spawns. The records are made
//! outside `mem::testing`'s failure injection and counting, so a test can capture the log of an allocation failure
//! it induces. Two never-default dispatchers stay registered (`ANCHORS`), so that `tracing-core`'s shortcut for a
//! single registered dispatcher cannot cache a callsite as never enabled while another thread captures.

use std::sync::{Arc, Mutex, PoisonError};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::subscriber::{DefaultGuard, Interest};
use tracing::{Event, Level, Metadata, Subscriber};

/// One captured event: its level, target, and formatted message.
pub(crate) type CapturedRecord = (Level, String, String);

/// The events captured so far.
type Records = Arc<Mutex<Vec<CapturedRecord>>>;

/// The subscriber [`start`] sets for its thread: it records every event.
struct CaptureSubscriber {
    records: Records,
}

/// Collects an event's `message` field.
struct MessageVisitor(String);

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn core::fmt::Debug) {
        if field.name() == "message" {
            // `format_args!`'s `Debug` writes the formatted message itself.
            self.0 = format!("{value:?}");
        }
    }
}

impl Subscriber for CaptureSubscriber {
    fn register_callsite(&self, _metadata: &'static Metadata<'static>) -> Interest {
        // Other threads have no subscriber, so the callsite's interest is decided per event.
        Interest::sometimes()
    }

    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
        Some(tracing::level_filters::LevelFilter::TRACE)
    }

    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        // The record's allocations are the capture's, not the code under test's: they neither fail nor count in a
        // `mem::testing` scope.
        crate::mem::testing::suspended(|| {
            let mut visitor = MessageVisitor(String::new());
            event.record(&mut visitor);
            let meta = event.metadata();
            self.records.lock().unwrap_or_else(PoisonError::into_inner).push((
                *meta.level(),
                meta.target().to_owned(),
                visitor.0,
            ));
        });
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

/// A live capture: this thread's events go into it until it is dropped.
pub(crate) struct Capture {
    records: Records,
    _guard: DefaultGuard,
}

impl Capture {
    /// Every event captured on this thread since [`start`].
    pub(crate) fn messages(&self) -> Vec<CapturedRecord> {
        self.records.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

/// A subscriber that is never any thread's default: it only keeps a callsite's interest decided per event.
struct Anchor;

impl Subscriber for Anchor {
    fn register_callsite(&self, _metadata: &'static Metadata<'static>) -> Interest {
        Interest::sometimes()
    }

    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        false
    }

    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, _event: &Event<'_>) {}

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

/// Two live dispatchers that are never dropped. `tracing-core` decides a callsite's cached interest from the
/// registered dispatchers, except when at most one is registered: then it asks the registering thread's default
/// alone. A callsite first reached on a thread with no capture while exactly one other thread captures would cache
/// "never" and hide its events from that capture. With these two always registered, the interest comes from every
/// live dispatcher, and theirs is "sometimes".
static ANCHORS: std::sync::OnceLock<[tracing::Dispatch; 2]> = std::sync::OnceLock::new();

/// Starts capturing the calling thread's events.
pub(crate) fn start() -> Capture {
    ANCHORS.get_or_init(|| [tracing::Dispatch::new(Anchor), tracing::Dispatch::new(Anchor)]);
    let records = Records::default();
    let subscriber = CaptureSubscriber {
        records: Arc::clone(&records),
    };
    Capture {
        records,
        _guard: tracing::subscriber::set_default(subscriber),
    }
}

#[cfg(test)]
mod tests;
