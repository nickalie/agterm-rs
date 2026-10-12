//! What a model mutation asks of the world: control events for the app's event ring and snapshot writes.
//!
//! Upstream's `AppStore` calls a library-owned event sink and its persistence directly on every mutation. Here a
//! store records both in its [`Outbox`], and the [`WindowLibrary`](super::WindowLibrary) drains them after each
//! mutation: it tags each event with its window, sequences it into the ring and performs the writes. A model with no
//! side channel stays a plain value its tests can inspect.

use std::sync::atomic::{AtomicU64, Ordering};

use agterm_proto::events::EventKind;
use uuid::Uuid;

/// The app-wide emission counter. Each store keeps its own outbox, so the library needs a shared stamp to merge the
/// outboxes back into the order the events were emitted in.
static EMISSION_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The next place in the app-wide emission order: strictly increasing across every store and the library.
pub(crate) fn next_emission() -> u64 {
    EMISSION_COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// The kind-specific fields of one event (upstream's `ControlEventPayload`), as far as the model fills them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EventPayload {
    /// The session's display name at emit time.
    pub name: Option<String>,
    /// `session.selected`: the selection it replaced.
    pub previous: Option<Uuid>,
    /// `status`: the new status' wire name.
    pub status: Option<String>,
    /// `status`: the pane that set it.
    pub pane: Option<String>,
}

/// One event a store emitted, before the library adds its window and the ring its sequence (upstream's
/// `ControlEventDraft`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelEvent {
    pub kind: EventKind,
    pub workspace: Option<Uuid>,
    pub session: Option<Uuid>,
    pub payload: EventPayload,
}

impl ModelEvent {
    /// An event with no identities or payload, e.g. `tree.changed`.
    #[must_use]
    pub fn bare(kind: EventKind) -> Self {
        Self {
            kind,
            workspace: None,
            session: None,
            payload: EventPayload::default(),
        }
    }
}

/// Which snapshot write a store asked for since the last drain. An immediate write covers a debounced one: it
/// writes the latest state, and upstream's `save()` cancels the pending debounce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum SaveRequest {
    /// Nothing to write.
    #[default]
    None,
    /// A coalesced write after the quiet window (a selection or font-size change; upstream's `scheduleSave`).
    Debounced,
    /// A write now (every structural mutation; upstream's `save`).
    Now,
}

/// A store's pending side effects, in order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Outbox {
    pub events: Vec<ModelEvent>,
    pub save: SaveRequest,
    /// Each event's place in the app-wide emission order ([`next_emission`]), parallel to `events`.
    emissions: Vec<u64>,
}

impl Outbox {
    /// Whether nothing is pending.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty() && self.save == SaveRequest::None
    }

    pub(crate) fn emit(&mut self, event: ModelEvent) {
        self.emissions.push(next_emission());
        self.events.push(event);
    }

    /// The events with their places in the app-wide emission order, for the library to merge outboxes by.
    pub(crate) fn into_stamped_events(self) -> impl Iterator<Item = (u64, ModelEvent)> {
        self.emissions.into_iter().zip(self.events)
    }

    pub(crate) fn request_save(&mut self, request: SaveRequest) {
        self.save = self.save.max(request);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_immediate_save_covers_a_debounced_one() {
        let mut outbox = Outbox::default();
        assert!(outbox.is_empty());
        outbox.request_save(SaveRequest::Debounced);
        assert_eq!(outbox.save, SaveRequest::Debounced);
        outbox.request_save(SaveRequest::Now);
        outbox.request_save(SaveRequest::Debounced);
        assert_eq!(outbox.save, SaveRequest::Now);
        assert!(!outbox.is_empty());
    }

    #[test]
    fn events_keep_their_order() {
        let mut outbox = Outbox::default();
        outbox.emit(ModelEvent::bare(EventKind::SessionCreated));
        outbox.emit(ModelEvent::bare(EventKind::TreeChanged));
        let kinds: Vec<_> = outbox.events.iter().map(|event| event.kind).collect();
        assert_eq!(kinds, [EventKind::SessionCreated, EventKind::TreeChanged]);
    }

    #[test]
    fn stamps_follow_the_emission_order_across_outboxes() {
        let (mut first, mut second) = (Outbox::default(), Outbox::default());
        first.emit(ModelEvent::bare(EventKind::SessionCreated));
        second.emit(ModelEvent::bare(EventKind::SessionClosed));
        first.emit(ModelEvent::bare(EventKind::TreeChanged));
        let mut stamped: Vec<_> = first
            .into_stamped_events()
            .chain(second.into_stamped_events())
            .collect();
        stamped.sort_by_key(|(emission, _)| *emission);
        let kinds: Vec<_> = stamped.iter().map(|(_, event)| event.kind).collect();
        assert_eq!(
            kinds,
            [
                EventKind::SessionCreated,
                EventKind::SessionClosed,
                EventKind::TreeChanged
            ]
        );
    }
}
