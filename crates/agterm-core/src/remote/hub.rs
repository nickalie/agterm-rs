//! The origin's presentation hub (upstream's `PresentationHub.swift` and `PresenterGrant.swift`): it fans a session's
//! presentation state out to the viewers subscribed to it, decides which one viewer is the session's sole presenter,
//! and keeps every subscriber alive with pings or drops it.
//!
//! Where upstream's hub calls back into the server (`onPresenterLost`, `onPresenterFrame`), this one queues
//! [`HubEvent`]s its owner takes after each call, so the hub never needs to reach the model it serves.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::time::{Duration, Instant};

use uuid::Uuid;

use super::frames::{
    self, Body, PresentationFrame, PresentationHello, PresentationLayout, PresentationSnapshot, negotiated_version,
};

/// Where the hub hands a subscriber's frames. The bounded queue and the socket live behind it; the hub only learns
/// whether a frame was taken.
pub trait PresentationSink {
    /// False when the consumer cannot take the frame (its queue is full, or it is gone), which the hub treats as a
    /// stalled subscriber.
    fn offer(&mut self, frame: &PresentationFrame) -> bool;
    /// The hub dropped the subscriber.
    fn close(&mut self, reason: CloseReason);
}

/// Why the hub dropped a subscriber.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    /// It could not take a frame.
    Stalled,
    /// It acknowledged no ping within the stale timeout.
    Stale,
}

/// Why a viewer could not subscribe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscribeError {
    /// The viewer's hello carries no version both sides speak.
    UnsupportedVersion(i64),
}

/// One subscription, by the generation its stream was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SubscriberId(i64);

impl SubscriberId {
    /// The connection generation every frame to this subscriber carries.
    #[must_use]
    pub fn generation(self) -> i64 {
        self.0
    }
}

/// What the hub's owner acts on after a call.
#[derive(Debug, Clone, PartialEq)]
pub enum HubEvent {
    /// The session's presenter went away; the role is released.
    PresenterLost(Uuid),
    /// What the session's current presenter sent about work it was handed. Frames of these kinds from any other
    /// viewer are dropped before they get here.
    PresenterFrame(Uuid, Box<Body>),
}

struct Subscriber {
    session: Uuid,
    sink: Box<dyn PresentationSink>,
    revision: i64,
    last_ack: Instant,
    /// Deltas published while the snapshot is being taken, held so they land after it.
    held: Option<Vec<Body>>,
}

/// Decides which viewer connection is a session's sole presenter (upstream's `PresenterGrant`). The first acquire wins
/// and keeps the role until its connection goes; nothing preempts it and nothing hands it back early. Every change of
/// holder bumps the session's generation, which later work owned by a presenter is bound to.
#[derive(Debug, Default)]
struct PresenterGrant {
    holders: HashMap<Uuid, SubscriberId>,
    generations: HashMap<Uuid, i64>,
}

impl PresenterGrant {
    /// Grants `session` to `subscriber` when nobody holds it. True when `subscriber` holds it afterwards.
    fn acquire(&mut self, session: Uuid, subscriber: SubscriberId) -> bool {
        if let Some(holder) = self.holders.get(&session) {
            return *holder == subscriber;
        }
        self.holders.insert(session, subscriber);
        *self.generations.entry(session).or_default() += 1;
        true
    }

    /// Takes the role back from a connection that went away; the sessions it held.
    fn release(&mut self, subscriber: SubscriberId) -> Vec<Uuid> {
        let mut held: Vec<Uuid> = self
            .holders
            .iter()
            .filter(|(_, holder)| **holder == subscriber)
            .map(|(session, _)| *session)
            .collect();
        held.sort_unstable();
        for session in &held {
            self.holders.remove(session);
            *self.generations.entry(*session).or_default() += 1;
        }
        held
    }

    fn holder(&self, session: Uuid) -> Option<SubscriberId> {
        self.holders.get(&session).copied()
    }

    fn generation(&self, session: Uuid) -> i64 {
        self.generations.get(&session).copied().unwrap_or_default()
    }
}

/// The frame kinds this origin produces.
pub const SUPPORTED_KINDS: [&str; 5] = ["status", "hud", "notify", "context", "layout"];

/// Fans each session's presentation state out to its viewers.
pub struct PresentationHub {
    stale_timeout: Duration,
    now: Box<dyn Fn() -> Instant>,
    subscribers: BTreeMap<SubscriberId, Subscriber>,
    last_generation: i64,
    layouts: HashMap<Uuid, PresentationLayout>,
    grant: PresenterGrant,
    events: Vec<HubEvent>,
}

impl fmt::Debug for PresentationHub {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PresentationHub")
            .field("stale_timeout", &self.stale_timeout)
            .field("subscribers", &self.subscribers.keys().collect::<Vec<_>>())
            .field("grant", &self.grant)
            .finish_non_exhaustive()
    }
}

impl PresentationHub {
    /// A hub dropping a subscriber silent for longer than `stale_timeout`, reading the time from `now`.
    #[must_use]
    pub fn new(stale_timeout: Duration, now: Box<dyn Fn() -> Instant>) -> Self {
        Self {
            stale_timeout,
            now,
            subscribers: BTreeMap::new(),
            last_generation: 0,
            layouts: HashMap::new(),
            grant: PresenterGrant::default(),
            events: Vec::new(),
        }
    }

    /// Registers a viewer and sends it hello, then the snapshot, then whatever was published meanwhile.
    ///
    /// The subscriber is registered before `snapshot` runs (it gets the hub), so a change published while the
    /// snapshot is taken is held and delivered after it, and nothing falls between the two.
    ///
    /// # Errors
    ///
    /// The viewer's hello carries no usable version; nothing is sent.
    pub fn subscribe(
        &mut self,
        session: Uuid,
        hello: &PresentationHello,
        sink: Box<dyn PresentationSink>,
        snapshot: impl FnOnce(&mut Self) -> PresentationSnapshot,
    ) -> Result<SubscriberId, SubscribeError> {
        let version = negotiated_version(frames::VERSION, hello.version)
            .ok_or(SubscribeError::UnsupportedVersion(hello.version))?;
        self.last_generation += 1;
        let id = SubscriberId(self.last_generation);
        let now = (self.now)();
        self.subscribers.insert(
            id,
            Subscriber {
                session,
                sink,
                revision: 0,
                last_ack: now,
                held: Some(Vec::new()),
            },
        );
        let state = snapshot(self);
        if let Some(layout) = &state.layout {
            self.layouts.entry(session).or_insert_with(|| layout.clone());
        }
        let held = self
            .subscribers
            .get_mut(&id)
            .and_then(|subscriber| subscriber.held.take())
            .unwrap_or_default();
        // presenter is offered only to a viewer that asked for it, so an older viewer stays a mirror
        let answer = PresentationHello {
            version,
            kinds: SUPPORTED_KINDS
                .iter()
                .filter(|kind| hello.kinds.iter().any(|theirs| theirs == *kind))
                .map(|kind| (*kind).to_owned())
                .collect(),
            mode: hello.mode,
        };
        for body in [Body::Hello(answer), Body::Snapshot(state)].into_iter().chain(held) {
            if !self.send(body, id) {
                break;
            }
        }
        Ok(id)
    }

    /// Ends a subscription whose stream went away.
    pub fn unsubscribe(&mut self, id: SubscriberId) {
        if let Some(subscriber) = self.subscribers.remove(&id) {
            self.forget_layout_if_unwatched(subscriber.session);
        }
        self.release(id);
    }

    /// Sends `body` to `session`'s presenter alone. False when there is none, or it stalled and was dropped.
    pub fn send_to_presenter(&mut self, body: Body, session: Uuid) -> bool {
        match self.grant.holder(session) {
            Some(holder) => self.send(body, holder),
            None => false,
        }
    }

    /// Publishes `layout` to `session`'s viewers when it differs from the last one they got.
    pub fn publish_layout(&mut self, layout: PresentationLayout, session: Uuid) {
        if self.subscriber_count(session) == 0 || self.layouts.get(&session) == Some(&layout) {
            return;
        }
        self.layouts.insert(session, layout.clone());
        self.publish(&Body::Layout(layout), session);
    }

    /// Sends `body` to every viewer of `session`; one still taking its snapshot gets it after.
    pub fn publish(&mut self, body: &Body, session: Uuid) {
        let ids: Vec<SubscriberId> = self
            .subscribers
            .iter()
            .filter(|(_, subscriber)| subscriber.session == session)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            if let Some(held) = self
                .subscribers
                .get_mut(&id)
                .and_then(|subscriber| subscriber.held.as_mut())
            {
                held.push(body.clone());
                continue;
            }
            self.send(body.clone(), id);
        }
    }

    /// Handles a frame a viewer sent. One from another generation is left over from an earlier connection.
    pub fn receive(&mut self, frame: PresentationFrame, from: SubscriberId) {
        let Some(subscriber) = self.subscribers.get_mut(&from) else {
            return;
        };
        if frame.generation != from.0 {
            return;
        }
        let session = subscriber.session;
        match frame.body {
            Body::Ack => subscriber.last_ack = (self.now)(),
            Body::Ping => {
                self.send(Body::Ack, from);
            }
            Body::PresenterAcquire => {
                let granted = self.grant.acquire(session, from);
                let answer = if granted {
                    Body::PresenterGranted
                } else {
                    Body::PresenterRefused
                };
                self.send(answer, from);
            }
            body @ (Body::AskResolve(_) | Body::AskRejected(_) | Body::OverlayRejected(_) | Body::OverlayClosed(_))
                if self.grant.holder(session) == Some(from) =>
            {
                self.events.push(HubEvent::PresenterFrame(session, Box::new(body)));
            }
            _ => {}
        }
    }

    /// Drops every subscriber whose last ack is older than the stale timeout and pings the rest. The owner of the
    /// streams calls this on its own timer.
    pub fn heartbeat(&mut self) {
        let now = (self.now)();
        let ids: Vec<(SubscriberId, Instant)> = self
            .subscribers
            .iter()
            .map(|(id, subscriber)| (*id, subscriber.last_ack))
            .collect();
        for (id, last_ack) in ids {
            if now.saturating_duration_since(last_ack) > self.stale_timeout {
                self.drop_subscriber(id, CloseReason::Stale);
                continue;
            }
            self.send(Body::Ping, id);
        }
    }

    /// How many viewers `session` has.
    #[must_use]
    pub fn subscriber_count(&self, session: Uuid) -> usize {
        self.subscribers
            .values()
            .filter(|subscriber| subscriber.session == session)
            .count()
    }

    /// Whether any viewer is subscribed at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.subscribers.is_empty()
    }

    /// Whether a viewer holds `session`'s presenter role.
    #[must_use]
    pub fn has_presenter(&self, session: Uuid) -> bool {
        self.grant.holder(session).is_some()
    }

    /// Counts changes of `session`'s presenter, a grant and a loss alike.
    #[must_use]
    pub fn presenter_generation(&self, session: Uuid) -> i64 {
        self.grant.generation(session)
    }

    /// What the hub's owner has to act on since the last take, in order.
    pub fn take_events(&mut self) -> Vec<HubEvent> {
        std::mem::take(&mut self.events)
    }

    fn send(&mut self, body: Body, id: SubscriberId) -> bool {
        let Some(subscriber) = self.subscribers.get_mut(&id) else {
            return false;
        };
        let frame = PresentationFrame {
            generation: id.0,
            rev: subscriber.revision,
            body,
        };
        if !subscriber.sink.offer(&frame) {
            self.drop_subscriber(id, CloseReason::Stalled);
            return false;
        }
        subscriber.revision += 1;
        true
    }

    fn drop_subscriber(&mut self, id: SubscriberId, reason: CloseReason) {
        let Some(mut subscriber) = self.subscribers.remove(&id) else {
            return;
        };
        self.forget_layout_if_unwatched(subscriber.session);
        subscriber.sink.close(reason);
        self.release(id);
    }

    fn forget_layout_if_unwatched(&mut self, session: Uuid) {
        if self.subscriber_count(session) == 0 {
            self.layouts.remove(&session);
        }
    }

    fn release(&mut self, id: SubscriberId) {
        for session in self.grant.release(id) {
            self.events.push(HubEvent::PresenterLost(session));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use super::*;
    use crate::model::AgentStatus;
    use crate::remote::frames::{PresentationMode, PresentationStatus};

    /// What a test sink saw, shared with the test.
    #[derive(Default)]
    struct Seen {
        frames: Vec<PresentationFrame>,
        closed: Option<CloseReason>,
        capacity: Option<usize>,
    }

    struct Sink(Rc<RefCell<Seen>>);

    impl PresentationSink for Sink {
        fn offer(&mut self, frame: &PresentationFrame) -> bool {
            let mut seen = self.0.borrow_mut();
            if seen.capacity.is_some_and(|capacity| seen.frames.len() >= capacity) {
                return false;
            }
            seen.frames.push(frame.clone());
            true
        }

        fn close(&mut self, reason: CloseReason) {
            self.0.borrow_mut().closed = Some(reason);
        }
    }

    fn sink() -> (Box<dyn PresentationSink>, Rc<RefCell<Seen>>) {
        let seen = Rc::new(RefCell::new(Seen::default()));
        (Box::new(Sink(Rc::clone(&seen))), seen)
    }

    fn bodies(seen: &Rc<RefCell<Seen>>) -> Vec<Body> {
        seen.borrow().frames.iter().map(|frame| frame.body.clone()).collect()
    }

    fn hello(mode: PresentationMode) -> PresentationHello {
        PresentationHello {
            version: 1,
            kinds: vec!["status".to_owned(), "hud".to_owned(), "notify".to_owned()],
            mode,
        }
    }

    fn blocked() -> PresentationStatus {
        PresentationStatus {
            status: AgentStatus::Blocked,
            blink: false,
            color: None,
            shape: None,
            pane: None,
            changed_at: None,
        }
    }

    fn hub() -> (PresentationHub, Rc<Cell<Instant>>) {
        let clock = Rc::new(Cell::new(Instant::now()));
        let reading = Rc::clone(&clock);
        (
            PresentationHub::new(Duration::from_secs(30), Box::new(move || reading.get())),
            clock,
        )
    }

    fn advance(clock: &Cell<Instant>, seconds: u64) {
        clock.set(clock.get() + Duration::from_secs(seconds));
    }

    fn subscribe(
        hub: &mut PresentationHub,
        session: Uuid,
        mode: PresentationMode,
    ) -> (SubscriberId, Rc<RefCell<Seen>>) {
        let (sink, seen) = sink();
        let id = hub
            .subscribe(session, &hello(mode), sink, |_| PresentationSnapshot::default())
            .unwrap();
        (id, seen)
    }

    fn frame(id: SubscriberId, body: Body) -> PresentationFrame {
        PresentationFrame {
            generation: id.generation(),
            rev: 0,
            body,
        }
    }

    #[test]
    fn subscribe_answers_hello_then_the_snapshot_before_any_delta() {
        let (mut hub, _) = hub();
        let session = Uuid::new_v4();
        let (sink, seen) = sink();
        let snapshot = PresentationSnapshot {
            status: Some(blocked()),
            ..PresentationSnapshot::default()
        };
        let state = snapshot.clone();
        hub.subscribe(session, &hello(PresentationMode::Mirror), sink, move |_| state)
            .unwrap();
        hub.publish(&Body::Status(None), session);
        assert_eq!(
            bodies(&seen),
            vec![
                Body::Hello(hello(PresentationMode::Mirror)),
                Body::Snapshot(snapshot),
                Body::Status(None)
            ]
        );
    }

    #[test]
    fn a_change_made_while_the_snapshot_is_taken_lands_after_it() {
        let (mut hub, _) = hub();
        let session = Uuid::new_v4();
        let (sink, seen) = sink();
        hub.subscribe(session, &hello(PresentationMode::Mirror), sink, |hub| {
            hub.publish(&Body::Status(Some(blocked())), session);
            PresentationSnapshot::default()
        })
        .unwrap();
        assert_eq!(
            bodies(&seen),
            vec![
                Body::Hello(hello(PresentationMode::Mirror)),
                Body::Snapshot(PresentationSnapshot::default()),
                Body::Status(Some(blocked()))
            ]
        );
    }

    #[test]
    fn revisions_are_monotonic_within_one_generation() {
        let (mut hub, _) = hub();
        let session = Uuid::new_v4();
        let (_, seen) = subscribe(&mut hub, session, PresentationMode::Mirror);
        hub.publish(&Body::Status(Some(blocked())), session);
        hub.publish(&Body::Hud(None), session);
        let frames = &seen.borrow().frames;
        assert!(frames.iter().all(|frame| frame.generation == frames[0].generation));
        assert_eq!(
            frames.iter().map(|frame| frame.rev).collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
    }

    #[test]
    fn a_second_subscriber_to_one_session_gets_its_own_generation() {
        let (mut hub, _) = hub();
        let session = Uuid::new_v4();
        let (_, first) = subscribe(&mut hub, session, PresentationMode::Mirror);
        let (_, second) = subscribe(&mut hub, session, PresentationMode::Mirror);
        hub.publish(&Body::Status(Some(blocked())), session);
        assert_ne!(
            first.borrow().frames[0].generation,
            second.borrow().frames[0].generation
        );
        assert_eq!(bodies(&first).last(), Some(&Body::Status(Some(blocked()))));
        assert_eq!(bodies(&second).last(), Some(&Body::Status(Some(blocked()))));
        assert_eq!(hub.subscriber_count(session), 2);
    }

    #[test]
    fn a_publish_for_another_session_reaches_nobody() {
        let (mut hub, _) = hub();
        let (_, seen) = subscribe(&mut hub, Uuid::new_v4(), PresentationMode::Mirror);
        hub.publish(&Body::Status(Some(blocked())), Uuid::new_v4());
        assert_eq!(seen.borrow().frames.len(), 2);
    }

    #[test]
    fn a_full_queue_disconnects_that_subscriber_only() {
        let (mut hub, _) = hub();
        let session = Uuid::new_v4();
        let (stalled_sink, stalled) = sink();
        stalled.borrow_mut().capacity = Some(2);
        hub.subscribe(session, &hello(PresentationMode::Mirror), stalled_sink, |_| {
            PresentationSnapshot::default()
        })
        .unwrap();
        let (_, healthy) = subscribe(&mut hub, session, PresentationMode::Mirror);
        hub.publish(&Body::Status(Some(blocked())), session);
        assert_eq!(stalled.borrow().closed, Some(CloseReason::Stalled));
        assert_eq!(healthy.borrow().closed, None);
        assert_eq!(bodies(&healthy).last(), Some(&Body::Status(Some(blocked()))));
        assert_eq!(hub.subscriber_count(session), 1);
    }

    #[test]
    fn unsubscribe_releases_the_subscriber() {
        let (mut hub, _) = hub();
        let session = Uuid::new_v4();
        let (id, seen) = subscribe(&mut hub, session, PresentationMode::Mirror);
        hub.unsubscribe(id);
        hub.publish(&Body::Status(Some(blocked())), session);
        assert_eq!(seen.borrow().frames.len(), 2);
        assert_eq!(seen.borrow().closed, None);
        assert_eq!(hub.subscriber_count(session), 0);
    }

    #[test]
    fn a_new_subscriber_receives_a_return_to_the_previous_layout() {
        for stale_heartbeat in [false, true] {
            let (mut hub, clock) = hub();
            let session = Uuid::new_v4();
            let primary = Uuid::new_v4();
            let initial = PresentationLayout {
                panes: vec![primary],
                primary,
                axis: None,
                shown: false,
            };
            let changed = PresentationLayout {
                panes: vec![primary, Uuid::new_v4()],
                primary,
                axis: Some("vertical".to_owned()),
                shown: true,
            };
            let (first_sink, first) = sink();
            let layout = initial.clone();
            let id = hub
                .subscribe(session, &hello(PresentationMode::Mirror), first_sink, move |_| {
                    PresentationSnapshot {
                        layout: Some(layout),
                        ..PresentationSnapshot::default()
                    }
                })
                .unwrap();
            if stale_heartbeat {
                advance(&clock, 31);
                hub.heartbeat();
                assert_eq!(first.borrow().closed, Some(CloseReason::Stale));
            } else {
                hub.unsubscribe(id);
            }
            assert_eq!(hub.subscriber_count(session), 0);
            hub.publish_layout(changed.clone(), session);
            let (next_sink, next) = sink();
            let snapshot = PresentationSnapshot {
                layout: Some(changed),
                ..PresentationSnapshot::default()
            };
            let state = snapshot.clone();
            hub.subscribe(session, &hello(PresentationMode::Mirror), next_sink, move |_| state)
                .unwrap();
            assert_eq!(bodies(&next).last(), Some(&Body::Snapshot(snapshot)));
            hub.publish_layout(initial.clone(), session);
            assert_eq!(bodies(&next).last(), Some(&Body::Layout(initial)));
            assert_eq!(next.borrow().frames.len(), 3);
        }
    }

    #[test]
    fn the_answer_advertises_only_kinds_both_sides_speak() {
        let (mut hub, _) = hub();
        let (sink, seen) = sink();
        let asked = PresentationHello {
            version: 1,
            kinds: vec!["notify".to_owned(), "overlay.request".to_owned(), "status".to_owned()],
            mode: PresentationMode::Presenter,
        };
        hub.subscribe(Uuid::new_v4(), &asked, sink, |_| PresentationSnapshot::default())
            .unwrap();
        assert_eq!(
            bodies(&seen).first(),
            Some(&Body::Hello(PresentationHello {
                version: 1,
                kinds: vec!["status".to_owned(), "notify".to_owned()],
                mode: PresentationMode::Presenter,
            }))
        );
    }

    #[test]
    fn a_peer_with_no_usable_version_is_refused() {
        let (mut hub, _) = hub();
        let (sink, seen) = sink();
        let old = PresentationHello {
            version: 0,
            kinds: Vec::new(),
            mode: PresentationMode::Mirror,
        };
        assert_eq!(
            hub.subscribe(Uuid::new_v4(), &old, sink, |_| PresentationSnapshot::default()),
            Err(SubscribeError::UnsupportedVersion(0))
        );
        assert!(seen.borrow().frames.is_empty());
    }

    #[test]
    fn a_heartbeat_pings_and_an_ack_keeps_the_subscriber_alive() {
        let (mut hub, clock) = hub();
        let (id, seen) = subscribe(&mut hub, Uuid::new_v4(), PresentationMode::Mirror);
        advance(&clock, 20);
        hub.heartbeat();
        hub.receive(frame(id, Body::Ack), id);
        advance(&clock, 20);
        hub.heartbeat();
        assert_eq!(bodies(&seen).iter().filter(|body| **body == Body::Ping).count(), 2);
        assert_eq!(seen.borrow().closed, None);
    }

    #[test]
    fn a_missed_ack_past_the_stale_timeout_closes_the_subscriber() {
        let (mut hub, clock) = hub();
        let session = Uuid::new_v4();
        let (_, seen) = subscribe(&mut hub, session, PresentationMode::Mirror);
        advance(&clock, 20);
        hub.heartbeat();
        advance(&clock, 20);
        hub.heartbeat();
        assert_eq!(seen.borrow().closed, Some(CloseReason::Stale));
        assert_eq!(hub.subscriber_count(session), 0);
    }

    #[test]
    fn a_ping_from_the_viewer_is_answered_and_another_generation_ignored() {
        let (mut hub, _) = hub();
        let (id, seen) = subscribe(&mut hub, Uuid::new_v4(), PresentationMode::Mirror);
        let mut stale = frame(id, Body::Ping);
        stale.generation += 1;
        hub.receive(stale, id);
        assert_eq!(seen.borrow().frames.len(), 2);
        hub.receive(frame(id, Body::Ping), id);
        assert_eq!(bodies(&seen).last(), Some(&Body::Ack));
    }

    // MARK: - the presenter grant (upstream's `PresenterGrantTests`)

    #[test]
    fn the_first_acquire_is_granted_and_a_second_viewer_is_refused_and_keeps_mirroring() {
        let (mut hub, _) = hub();
        let session = Uuid::new_v4();
        let (first, first_seen) = subscribe(&mut hub, session, PresentationMode::Presenter);
        let (second, second_seen) = subscribe(&mut hub, session, PresentationMode::Presenter);
        assert_eq!(hub.presenter_generation(session), 0);
        hub.receive(frame(first, Body::PresenterAcquire), first);
        hub.receive(frame(second, Body::PresenterAcquire), second);
        assert_eq!(bodies(&first_seen).last(), Some(&Body::PresenterGranted));
        assert_eq!(bodies(&second_seen).last(), Some(&Body::PresenterRefused));
        assert!(hub.has_presenter(session));
        assert_eq!(hub.presenter_generation(session), 1);
        hub.publish(&Body::Status(None), session);
        assert_eq!(bodies(&second_seen).last(), Some(&Body::Status(None)), "still mirrors");
        // a repeated acquire never preempts the holder, and the holder's own repeat is granted again
        hub.receive(frame(second, Body::PresenterAcquire), second);
        hub.receive(frame(first, Body::PresenterAcquire), first);
        assert_eq!(bodies(&second_seen).last(), Some(&Body::PresenterRefused));
        assert_eq!(bodies(&first_seen).last(), Some(&Body::PresenterGranted));
        assert_eq!(hub.presenter_generation(session), 1);
    }

    #[test]
    fn a_closed_stream_or_a_missed_heartbeat_revokes_the_role() {
        for stale in [false, true] {
            let (mut hub, clock) = hub();
            let session = Uuid::new_v4();
            let (id, _) = subscribe(&mut hub, session, PresentationMode::Presenter);
            hub.receive(frame(id, Body::PresenterAcquire), id);
            if stale {
                advance(&clock, 31);
                hub.heartbeat();
            } else {
                hub.unsubscribe(id);
            }
            assert!(!hub.has_presenter(session));
            assert_eq!(hub.presenter_generation(session), 2);
            assert_eq!(hub.take_events(), vec![HubEvent::PresenterLost(session)]);
        }
    }

    #[test]
    fn each_session_has_its_own_presenter() {
        let (mut hub, _) = hub();
        let (one, two) = (Uuid::new_v4(), Uuid::new_v4());
        let (first, first_seen) = subscribe(&mut hub, one, PresentationMode::Presenter);
        let (second, second_seen) = subscribe(&mut hub, two, PresentationMode::Presenter);
        hub.receive(frame(first, Body::PresenterAcquire), first);
        hub.receive(frame(second, Body::PresenterAcquire), second);
        assert_eq!(bodies(&first_seen).last(), Some(&Body::PresenterGranted));
        assert_eq!(bodies(&second_seen).last(), Some(&Body::PresenterGranted));
        assert!(hub.send_to_presenter(Body::Ping, one));
        assert_eq!(bodies(&first_seen).last(), Some(&Body::Ping));
        assert_eq!(bodies(&second_seen).last(), Some(&Body::PresenterGranted));
    }

    #[test]
    fn only_the_current_presenters_work_frames_reach_the_owner() {
        let (mut hub, _) = hub();
        let session = Uuid::new_v4();
        let (presenter, _) = subscribe(&mut hub, session, PresentationMode::Presenter);
        let (mirror, _) = subscribe(&mut hub, session, PresentationMode::Mirror);
        let rejected = Body::AskRejected(crate::remote::frames::PresentationAskRef {
            id: "a".to_owned(),
            owner: 1,
        });
        hub.receive(frame(presenter, rejected.clone()), presenter);
        assert!(hub.take_events().is_empty(), "not the presenter yet");
        hub.receive(frame(presenter, Body::PresenterAcquire), presenter);
        hub.receive(frame(mirror, rejected.clone()), mirror);
        hub.receive(frame(presenter, rejected.clone()), presenter);
        assert_eq!(
            hub.take_events(),
            vec![HubEvent::PresenterFrame(session, Box::new(rejected))]
        );
        assert!(!hub.send_to_presenter(Body::Ping, Uuid::new_v4()));
    }
}
