//! What a model mutation asks of the world: control events for the app's event ring, snapshot writes, soft closes'
//! grace timers and Open Recent changes.
//!
//! Upstream's `AppStore` calls a library-owned event sink and its persistence directly on every mutation. Here a
//! store records both in its [`Outbox`], and the [`WindowLibrary`](super::WindowLibrary) drains them after each
//! mutation: it tags each event with its window, sequences it into the ring and performs the writes. A model with no
//! side channel stays a plain value its tests can inspect.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use agterm_proto::events::EventKind;
use uuid::Uuid;

use crate::persist::RecentClosedItem;

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

/// One change to the Open Recent list, for `recent-closed.json` (upstream's `RecentClosedStore` calls).
#[derive(Debug, Clone, PartialEq)]
pub enum RecentClosedChange {
    /// Record a closed session or workspace as the newest entry.
    Record(Box<RecentClosedItem>),
    /// Drop the entry with this id.
    Remove(Uuid),
    /// Empty the list.
    Clear,
}

/// A store's pending side effects, in order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Outbox {
    pub events: Vec<ModelEvent>,
    pub save: SaveRequest,
    /// Soft closes whose grace timer must be armed: the pending-close record id and its grace.
    pub pending_closes: Vec<(Uuid, Duration)>,
    /// Whether the window's armed debounced write must be dropped without writing (upstream's `cancelPendingSave`,
    /// which a soft close calls so the file keeps the closed sessions until the close resolves).
    pub cancel_debounced_save: bool,
    /// Open Recent changes, in order.
    pub recent_closed: Vec<RecentClosedChange>,
    /// Each event's place in the app-wide emission order ([`next_emission`]), parallel to `events`.
    emissions: Vec<u64>,
    /// The same for `recent_closed`, so the library can order them against its own changes.
    recent_emissions: Vec<u64>,
}

impl Outbox {
    /// Whether nothing is pending.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
            && self.save == SaveRequest::None
            && self.pending_closes.is_empty()
            && !self.cancel_debounced_save
            && self.recent_closed.is_empty()
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

    /// Drops the debounced write asked for so far and the one already armed; an immediate write stays.
    pub(crate) fn cancel_debounced(&mut self) {
        if self.save == SaveRequest::Debounced {
            self.save = SaveRequest::None;
        }
        self.cancel_debounced_save = true;
    }

    pub(crate) fn record_recent_closed(&mut self, change: RecentClosedChange) {
        self.recent_emissions.push(next_emission());
        self.recent_closed.push(change);
    }

    /// Takes the Open Recent changes with their places in the app-wide emission order.
    pub(crate) fn take_stamped_recent_closed(&mut self) -> Vec<(u64, RecentClosedChange)> {
        let emissions = std::mem::take(&mut self.recent_emissions);
        emissions
            .into_iter()
            .zip(std::mem::take(&mut self.recent_closed))
            .collect()
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
    fn a_cancel_drops_a_debounced_save_but_not_an_immediate_one() {
        let mut outbox = Outbox::default();
        outbox.request_save(SaveRequest::Debounced);
        outbox.cancel_debounced();
        assert_eq!(outbox.save, SaveRequest::None);
        assert!(outbox.cancel_debounced_save && !outbox.is_empty());
        outbox.request_save(SaveRequest::Now);
        outbox.cancel_debounced();
        assert_eq!(outbox.save, SaveRequest::Now);
    }

    #[test]
    fn recent_closed_changes_keep_their_order_and_stamps() {
        let mut outbox = Outbox::default();
        let (first, second) = (Uuid::new_v4(), Uuid::new_v4());
        outbox.record_recent_closed(RecentClosedChange::Remove(first));
        outbox.record_recent_closed(RecentClosedChange::Remove(second));
        assert!(!outbox.is_empty());
        let stamped = outbox.take_stamped_recent_closed();
        assert!(stamped[0].0 < stamped[1].0);
        let changes: Vec<_> = stamped.into_iter().map(|(_, change)| change).collect();
        assert_eq!(
            changes,
            [RecentClosedChange::Remove(first), RecentClosedChange::Remove(second)]
        );
        assert!(outbox.is_empty());
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
