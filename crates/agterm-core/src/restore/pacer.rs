//! The launch spawn pacer (upstream's `SpawnPacer.swift`): rate-limits terminal spawns so a launch that replays
//! captured commands does not start every pane's program at the same instant. It is a leaky bucket: at most one paced
//! grant per interval, the next deadline measured from the actual previous grant, so a stalled main loop releases one
//! grant on waking rather than a catch-up burst.
//!
//! Every key is a pane identity, stable for the pane's lifetime. A launch restore arms the pacer with the expected
//! pane order and the burst set (the on-screen panes of every window's selected session, which the user is looking
//! at). Arming grants nothing: a key becomes grantable only once its view calls [`SpawnPacer::request`], so a grant
//! never releases a pane whose widget does not exist yet.
//!
//! # The pull model
//!
//! Upstream drives the pacer with an injected clock and timer and hands each grant to an `onGrant` callback. Here
//! the pacer owns no timer and calls nothing back: every call takes the current [`Instant`], and each grant comes
//! back to the caller as a return value, which spawns that pane.
//!
//! - [`SpawnPacer::request`] and [`SpawnPacer::expedite`] answer whether the asking key spawns now.
//! - After any call, the frontend asks [`SpawnPacer::take_wake_request`]; a `Some(delay)` means "arm one main-loop
//!   timer for `delay`". It answers `Some` at most once until the next [`SpawnPacer::wake`], so exactly one timer is
//!   outstanding at a time.
//! - When that timer fires the frontend calls [`SpawnPacer::wake`], which grants the head of the queue when it is
//!   ready and its interval has passed (the caller spawns it), and asks [`SpawnPacer::take_wake_request`] again: a
//!   wake that came early, or one after which more keys wait, arms the next timer. The frontend must call `wake`
//!   for every timer it armed, or no further timer is requested.
//!
//! [`SpawnPacer::next_wake`] is the same delay without the bookkeeping, for a caller that tracks its timer itself.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use uuid::Uuid;

/// The minimum gap between paced grants, unless the frontend picks another.
pub const DEFAULT_INTERVAL: Duration = Duration::from_millis(120);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Armed in the order; its view has not asked yet.
    Expected,
    /// Its view asked; it waits for its turn.
    Ready,
    /// It may spawn.
    Granted,
}

/// The launch spawn pacer (upstream's `SpawnPacer`), driven by the caller's clock: see the [module docs](self).
#[derive(Debug)]
pub struct SpawnPacer {
    interval: Duration,
    armed: bool,
    /// The keys still to leave the queue, in grant order.
    pending: Vec<Uuid>,
    states: HashMap<Uuid, State>,
    /// The burst and expedited keys, granted on their request without waiting their turn.
    immediate: HashSet<Uuid>,
    last_grant: Option<Instant>,
    armed_at: Option<Instant>,
    /// A wake delay was handed out by [`Self::take_wake_request`] and its [`Self::wake`] has not come yet.
    wake_outstanding: bool,
    /// The armed launch drained and [`Self::take_drained`] has not reported it yet.
    drained: bool,
}

impl Default for SpawnPacer {
    fn default() -> Self {
        Self::new(DEFAULT_INTERVAL)
    }
}

impl SpawnPacer {
    /// An unarmed pacer granting at most one paced key per `interval`.
    #[must_use]
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            armed: false,
            pending: Vec::new(),
            states: HashMap::new(),
            immediate: HashSet::new(),
            last_grant: None,
            armed_at: None,
            wake_outstanding: false,
            drained: false,
        }
    }

    /// Records the expected spawn order and the keys exempt from pacing, and nothing else: no grant is issued and no
    /// wake is requested. Called once per launch restore, before any window mounts.
    pub fn arm(&mut self, order: Vec<Uuid>, burst: &HashSet<Uuid>, now: Instant) {
        self.armed = true;
        self.states = order.iter().map(|key| (*key, State::Expected)).collect();
        self.immediate = burst
            .iter()
            .copied()
            .filter(|key| self.states.contains_key(key))
            .collect();
        self.pending = order;
        self.last_grant = None;
        self.wake_outstanding = false;
        self.armed_at = Some(now);
        self.drained = false;
    }

    /// True when nothing is queued: unarmed, or every armed key granted, cancelled or discarded. A request then
    /// answers at once, so a session created after the launch never waits.
    #[must_use]
    pub fn is_passthrough(&self) -> bool {
        !self.armed || self.pending.is_empty()
    }

    /// Marks `key` ready to spawn and answers whether it spawns now. False leaves it queued for a later
    /// [`Self::wake`]. True for a burst or expedited key (granted here), for a key already granted, and for any key
    /// outside the armed order, so asking again after a grant is safe.
    pub fn request(&mut self, key: Uuid, now: Instant) -> bool {
        if self.is_passthrough() {
            return true;
        }
        match self.states.get(&key) {
            None | Some(State::Granted) => true,
            Some(_) if self.immediate.contains(&key) => {
                self.grant(key, now);
                true
            }
            Some(_) => {
                self.states.insert(key, State::Ready);
                false
            }
        }
    }

    /// Grants `key` now if its view already asked, consuming the next token so the queue waits a full interval after
    /// it; true means the caller spawns it. A key that has not asked yet is marked instead and granted on its
    /// [`Self::request`]. No-op for a granted, unknown or already-expedited key, so repeated selection of one pane
    /// cannot mint tokens.
    pub fn expedite(&mut self, key: Uuid, now: Instant) -> bool {
        let Some(state) = self.states.get(&key).copied() else {
            return false;
        };
        if state == State::Granted || self.immediate.contains(&key) {
            return false;
        }
        if state == State::Expected {
            self.immediate.insert(key);
            return false;
        }
        self.grant(key, now);
        true
    }

    /// Moves `keys` to the front of the queue in the given order, releasing nothing. For a view that opens on many
    /// queued panes at once, such as the dashboard.
    pub fn prioritize(&mut self, keys: &[Uuid]) {
        let mut seen = HashSet::new();
        let mut order: Vec<Uuid> = keys
            .iter()
            .copied()
            .filter(|key| self.pending.contains(key) && seen.insert(*key))
            .collect();
        if order.is_empty() {
            return;
        }
        order.extend(self.pending.iter().copied().filter(|key| !seen.contains(key)));
        self.pending = order;
    }

    /// Drops `key` because its pane or its view is gone.
    pub fn cancel(&mut self, key: Uuid) {
        self.drop_key(key);
    }

    /// Drops `key` because it spawns without a permit: a pane that replays nothing, or one already realized
    /// elsewhere. It leaves the queue without a grant, so the pacing of the rest is unchanged.
    pub fn discard(&mut self, key: Uuid) {
        self.drop_key(key);
    }

    /// Drops every known key `keep` rejects, as [`Self::cancel`] does: the caller prunes the panes that left the
    /// model.
    pub fn retain(&mut self, mut keep: impl FnMut(&Uuid) -> bool) {
        let gone: Vec<Uuid> = self.states.keys().filter(|key| !keep(key)).copied().collect();
        for key in gone {
            self.drop_key(key);
        }
    }

    /// Answers a fired wake timer: grants the head of the queue when its view has asked and the interval since the
    /// last grant has passed, and the caller spawns it. `None` when the wake came early or nothing is grantable; ask
    /// [`Self::take_wake_request`] after it either way.
    pub fn wake(&mut self, now: Instant) -> Option<Uuid> {
        self.wake_outstanding = false;
        let head = self.grantable_head()?;
        if !self.remaining_delay(now).is_zero() {
            return None;
        }
        self.grant(head, now);
        Some(head)
    }

    /// The delay before the head of the queue can be granted; `None` while the head's view has not asked or nothing
    /// is queued. A later ready key never jumps the head, so the queue drains in model order.
    #[must_use]
    pub fn next_wake(&self, now: Instant) -> Option<Duration> {
        self.grantable_head().map(|_| self.remaining_delay(now))
    }

    /// [`Self::next_wake`] for a caller arming one timer: `Some(delay)` means "arm a timer for `delay`, then call
    /// [`Self::wake`]", and it is answered at most once until that wake. `None` while a timer is outstanding or no
    /// wake is needed.
    pub fn take_wake_request(&mut self, now: Instant) -> Option<Duration> {
        if self.wake_outstanding {
            return None;
        }
        let delay = self.next_wake(now)?;
        self.wake_outstanding = true;
        Some(delay)
    }

    /// Once per launch, after the last armed key was granted, cancelled or discarded: the time from [`Self::arm`] to
    /// `now`, so ask right after the call that may drain. Diagnostics only: the pacer is already passthrough by then.
    pub fn take_drained(&mut self, now: Instant) -> Option<Duration> {
        if !std::mem::take(&mut self.drained) {
            return None;
        }
        Some(now.saturating_duration_since(self.armed_at.take()?))
    }

    fn drop_key(&mut self, key: Uuid) {
        if self.states.remove(&key).is_none() {
            return;
        }
        self.pending.retain(|pending| *pending != key);
        self.immediate.remove(&key);
        self.note_drain();
    }

    fn grant(&mut self, key: Uuid, now: Instant) {
        self.states.insert(key, State::Granted);
        self.pending.retain(|pending| *pending != key);
        self.immediate.remove(&key);
        self.last_grant = Some(now);
        self.note_drain();
    }

    fn note_drain(&mut self) {
        if self.armed && self.pending.is_empty() {
            self.armed = false;
            self.drained = true;
        }
    }

    fn grantable_head(&self) -> Option<Uuid> {
        let head = *self.pending.first()?;
        (self.states.get(&head) == Some(&State::Ready)).then_some(head)
    }

    fn remaining_delay(&self, now: Instant) -> Duration {
        self.last_grant.map_or(Duration::ZERO, |last| {
            self.interval.saturating_sub(now.saturating_duration_since(last))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INTERVAL: Duration = Duration::from_millis(100);

    /// Upstream's fake clock and grant log over the pull model: one timer at a time, armed from
    /// `take_wake_request` after every call, fired late or early at will.
    struct Harness {
        pacer: SpawnPacer,
        now: Instant,
        timer: Option<Instant>,
        grants: Vec<(Uuid, Instant)>,
    }

    impl Harness {
        fn new() -> Self {
            Self {
                pacer: SpawnPacer::new(INTERVAL),
                now: Instant::now(),
                timer: None,
                grants: Vec::new(),
            }
        }

        fn keys(&self) -> Vec<Uuid> {
            self.grants.iter().map(|(key, _)| *key).collect()
        }

        fn gap(&self, later: usize) -> Duration {
            self.grants[later].1 - self.grants[later - 1].1
        }

        fn sync(&mut self) {
            if let Some(delay) = self.pacer.take_wake_request(self.now) {
                self.timer = Some(self.now + delay);
            }
        }

        fn arm(&mut self, order: &[Uuid], burst: &[Uuid]) {
            self.pacer
                .arm(order.to_vec(), &burst.iter().copied().collect(), self.now);
            self.sync();
        }

        /// The view's request; a grant the pacer issued here (not a passthrough answer) joins the log.
        fn request(&mut self, key: Uuid) -> bool {
            let queued = !self.pacer.is_passthrough()
                && self
                    .pacer
                    .states
                    .get(&key)
                    .is_some_and(|state| *state != State::Granted);
            let spawn = self.pacer.request(key, self.now);
            if spawn && queued {
                self.grants.push((key, self.now));
            }
            self.sync();
            spawn
        }

        fn expedite(&mut self, key: Uuid) {
            if self.pacer.expedite(key, self.now) {
                self.grants.push((key, self.now));
            }
            self.sync();
        }

        fn prioritize(&mut self, keys: &[Uuid]) {
            self.pacer.prioritize(keys);
            self.sync();
        }

        fn cancel(&mut self, key: Uuid) {
            self.pacer.cancel(key);
            self.sync();
        }

        fn discard(&mut self, key: Uuid) {
            self.pacer.discard(key);
            self.sync();
        }

        fn advance(&mut self, by: Duration) {
            self.now += by;
        }

        /// Fires the outstanding timer where the clock stands.
        fn fire_pending_wake(&mut self) {
            if self.timer.take().is_none() {
                return;
            }
            if let Some(key) = self.pacer.wake(self.now) {
                self.grants.push((key, self.now));
            }
            self.sync();
        }

        /// Fires the outstanding timer at its deadline.
        fn fire_next_wake(&mut self) {
            if let Some(deadline) = self.timer
                && deadline > self.now
            {
                self.now = deadline;
            }
            self.fire_pending_wake();
        }

        fn drain(&mut self) {
            for _ in 0..20 {
                self.fire_next_wake();
            }
        }
    }

    fn uuids(count: usize) -> Vec<Uuid> {
        (0..count).map(|_| Uuid::new_v4()).collect()
    }

    #[test]
    fn an_unarmed_pacer_grants_at_once() {
        let mut h = Harness::new();
        assert!(h.pacer.is_passthrough());
        assert!(h.request(Uuid::new_v4()));
        assert!(h.timer.is_none());
        assert!(h.grants.is_empty());
    }

    #[test]
    fn arming_grants_nothing_while_no_key_requests() {
        let mut h = Harness::new();
        let keys = uuids(3);
        h.arm(&keys, &keys[..1]);
        assert!(!h.pacer.is_passthrough());
        assert!(h.timer.is_none());
        h.advance(INTERVAL * 10);
        h.drain();
        assert!(h.grants.is_empty());
    }

    #[test]
    fn ready_keys_grant_in_expected_order_an_interval_apart() {
        let mut h = Harness::new();
        let keys = uuids(3);
        h.arm(&keys, &[]);
        assert!(!h.request(keys[2]));
        assert!(!h.request(keys[0]));
        assert!(!h.request(keys[1]));
        assert!(h.grants.is_empty());

        h.drain();
        assert_eq!(h.keys(), keys);
        assert_eq!((h.gap(1), h.gap(2)), (INTERVAL, INTERVAL));
        assert!(h.pacer.is_passthrough());
    }

    #[test]
    fn a_burst_key_grants_on_its_request_not_on_arm() {
        let mut h = Harness::new();
        let [visible, queued] = [Uuid::new_v4(), Uuid::new_v4()];
        h.arm(&[visible, queued], &[visible]);
        h.advance(INTERVAL * 5);
        assert!(h.grants.is_empty());

        assert!(h.request(visible));
        assert_eq!(h.keys(), [visible]);
        assert!(!h.request(queued));
        assert_eq!(h.keys(), [visible]);
    }

    #[test]
    fn a_paced_grant_waits_for_an_expected_key_that_has_not_requested() {
        let mut h = Harness::new();
        let keys = uuids(3);
        h.arm(&keys, &[]);
        assert!(!h.request(keys[1]));
        assert!(!h.request(keys[2]));
        assert!(h.timer.is_none());

        h.advance(INTERVAL * 10);
        h.drain();
        assert!(h.grants.is_empty());

        assert!(!h.request(keys[0]));
        h.drain();
        assert_eq!(h.keys(), keys);
    }

    #[test]
    fn a_late_wake_releases_one_grant_only() {
        let mut h = Harness::new();
        let keys = uuids(4);
        h.arm(&keys, &[]);
        for key in &keys {
            h.request(*key);
        }

        h.fire_next_wake();
        assert_eq!(h.keys(), [keys[0]]);

        h.advance(INTERVAL * 3);
        h.fire_pending_wake();
        assert_eq!(h.keys(), [keys[0], keys[1]]);

        h.fire_pending_wake();
        assert_eq!(h.keys(), [keys[0], keys[1]]);
    }

    #[test]
    fn expedite_grants_now_and_resets_the_deadline() {
        let mut h = Harness::new();
        let keys = uuids(3);
        h.arm(&keys, &[]);
        for key in &keys {
            h.request(*key);
        }
        h.fire_next_wake();
        assert_eq!(h.keys(), [keys[0]]);

        h.expedite(keys[2]);
        assert_eq!(h.keys(), [keys[0], keys[2]]);
        assert_eq!(h.gap(1), Duration::ZERO);

        h.fire_next_wake();
        assert_eq!(h.keys(), [keys[0], keys[2], keys[1]]);
        assert_eq!(h.gap(2), INTERVAL);
    }

    #[test]
    fn expedite_before_a_request_grants_on_that_request() {
        let mut h = Harness::new();
        let [first, late] = [Uuid::new_v4(), Uuid::new_v4()];
        h.arm(&[first, late], &[]);
        h.expedite(late);
        assert!(h.grants.is_empty());
        h.expedite(late);
        assert!(h.grants.is_empty());

        assert!(h.request(late));
        assert_eq!(h.keys(), [late]);
    }

    #[test]
    fn expedite_is_a_no_op_for_granted_and_unknown_keys() {
        let mut h = Harness::new();
        let keys = uuids(2);
        h.arm(&keys, &[]);
        for key in &keys {
            h.request(*key);
        }
        h.fire_next_wake();
        assert_eq!(h.keys(), [keys[0]]);

        h.expedite(keys[0]);
        h.expedite(Uuid::new_v4());
        assert_eq!(h.keys(), [keys[0]]);
    }

    #[test]
    fn prioritize_reorders_without_granting() {
        let mut h = Harness::new();
        let keys = uuids(3);
        h.arm(&keys, &[]);
        for key in &keys {
            h.request(*key);
        }
        h.prioritize(&[keys[2]]);
        assert!(h.grants.is_empty());

        h.drain();
        assert_eq!(h.keys(), [keys[2], keys[0], keys[1]]);
    }

    #[test]
    fn a_repeated_request_of_a_queued_key_is_idempotent() {
        let mut h = Harness::new();
        let keys = uuids(2);
        h.arm(&keys, &[]);
        assert!(!h.request(keys[0]));
        assert!(!h.request(keys[0]));
        assert!(!h.request(keys[1]));

        h.drain();
        assert_eq!(h.keys(), keys);
        assert!(h.request(keys[0]));
    }

    #[test]
    fn cancel_and_discard_drop_without_grant_or_catch_up() {
        let mut h = Harness::new();
        let keys = uuids(3);
        h.arm(&keys, &[]);
        for key in &keys {
            h.request(*key);
        }
        h.fire_next_wake();
        assert_eq!(h.keys(), [keys[0]]);

        h.cancel(keys[1]);
        h.discard(keys[2]);
        assert_eq!(h.keys(), [keys[0]]);
        assert!(h.pacer.is_passthrough());

        h.drain();
        assert_eq!(h.keys(), [keys[0]]);
    }

    #[test]
    fn discarding_an_expected_key_that_never_requests_does_not_block_the_drain() {
        let mut h = Harness::new();
        let [unmounted, queued] = [Uuid::new_v4(), Uuid::new_v4()];
        h.arm(&[unmounted, queued], &[]);
        assert!(!h.request(queued));
        assert!(h.timer.is_none());

        h.discard(unmounted);
        h.drain();
        assert_eq!(h.keys(), [queued]);
        assert!(h.pacer.is_passthrough());
    }

    #[test]
    fn retain_drops_the_keys_that_left_the_model() {
        let mut h = Harness::new();
        let [gone, queued] = [Uuid::new_v4(), Uuid::new_v4()];
        h.arm(&[gone, queued], &[]);
        assert!(!h.request(queued));
        assert!(h.timer.is_none());

        h.pacer.retain(|key| *key != gone);
        h.sync();
        h.drain();
        assert_eq!(h.keys(), [queued]);
        assert!(h.pacer.is_passthrough());
    }

    #[test]
    fn a_request_after_the_drain_is_answered_at_once() {
        let mut h = Harness::new();
        let armed = Uuid::new_v4();
        h.arm(&[armed], &[]);
        assert!(!h.request(armed));
        h.drain();
        assert!(h.pacer.is_passthrough());

        assert!(h.request(Uuid::new_v4()));
        assert_eq!(h.keys(), [armed]);
    }

    #[test]
    fn a_key_outside_the_armed_order_is_answered_at_once() {
        let mut h = Harness::new();
        h.arm(&[Uuid::new_v4()], &[]);
        assert!(h.request(Uuid::new_v4()));
        assert!(h.grants.is_empty());
        assert!(!h.pacer.is_passthrough());
    }

    #[test]
    fn a_burst_grant_releases_a_follower_that_requested_first() {
        let mut h = Harness::new();
        let [visible, later] = [Uuid::new_v4(), Uuid::new_v4()];
        h.arm(&[visible, later], &[visible]);
        assert!(!h.request(later));
        assert!(h.timer.is_none());

        h.advance(INTERVAL * 5);
        assert!(h.request(visible));
        assert_eq!(h.keys(), [visible]);

        h.advance(INTERVAL / 2);
        h.fire_pending_wake();
        assert_eq!(h.keys(), [visible]);

        h.fire_next_wake();
        assert_eq!(h.keys(), [visible, later]);
        assert_eq!(h.gap(1), INTERVAL);
        assert!(h.pacer.is_passthrough());
    }

    #[test]
    fn a_deferred_expedite_grant_releases_a_follower_that_requested_first() {
        let mut h = Harness::new();
        let [head, follower] = [Uuid::new_v4(), Uuid::new_v4()];
        h.arm(&[head, follower], &[]);
        assert!(!h.request(follower));
        assert!(h.timer.is_none());

        h.expedite(head);
        assert!(h.grants.is_empty());
        assert!(h.request(head));
        assert_eq!(h.keys(), [head]);

        h.fire_next_wake();
        assert_eq!(h.keys(), [head, follower]);
        assert_eq!(h.gap(1), INTERVAL);
        assert!(h.pacer.is_passthrough());
    }

    #[test]
    fn the_drain_is_reported_once_with_the_time_since_arm() {
        let mut h = Harness::new();
        let keys = uuids(2);
        h.arm(&keys, &keys[..1]);

        assert!(h.request(keys[0]));
        assert_eq!(h.pacer.take_drained(h.now), None);
        h.advance(INTERVAL);
        h.discard(keys[1]);

        assert_eq!(h.pacer.take_drained(h.now), Some(INTERVAL));
        assert!(h.pacer.is_passthrough());
        h.cancel(keys[0]);
        assert_eq!(
            h.pacer.take_drained(h.now),
            None,
            "a drop after the drain must not report again"
        );
    }

    /// A second window mounting after the first drained: its selected pane is burst and spawns on request, and its
    /// follower is spaced from that grant, never released together with it.
    #[test]
    fn a_window_mounting_after_the_first_drained_still_spaces_its_follower() {
        let mut h = Harness::new();
        let first = uuids(2);
        let second = uuids(2);
        let order = [first.clone(), second.clone()].concat();
        h.arm(&order, &[first[0], second[0]]);
        assert!(h.request(first[0]));
        assert!(!h.request(first[1]));
        h.fire_next_wake();
        assert_eq!(h.keys(), first);
        h.advance(INTERVAL * 3);

        assert!(h.request(second[0]));
        assert!(!h.request(second[1]));
        h.fire_next_wake();

        assert_eq!(h.keys(), order);
        assert_eq!(h.gap(3), INTERVAL);
        assert!(h.pacer.is_passthrough());
    }

    #[test]
    fn one_wake_is_requested_at_a_time() {
        let mut pacer = SpawnPacer::new(INTERVAL);
        let now = Instant::now();
        let keys = uuids(2);
        pacer.arm(keys.clone(), &HashSet::new(), now);
        assert!(!pacer.request(keys[0], now));
        assert!(!pacer.request(keys[1], now));
        assert_eq!(pacer.next_wake(now), Some(Duration::ZERO));
        assert_eq!(pacer.take_wake_request(now), Some(Duration::ZERO));
        assert_eq!(pacer.take_wake_request(now), None);

        assert_eq!(pacer.wake(now), Some(keys[0]));
        assert_eq!(pacer.next_wake(now + INTERVAL / 4), Some(INTERVAL * 3 / 4));
        assert_eq!(pacer.take_wake_request(now), Some(INTERVAL));
        assert_eq!(pacer.wake(now + INTERVAL), Some(keys[1]));
        assert_eq!(pacer.take_wake_request(now + INTERVAL), None);
        assert_eq!(pacer.next_wake(now + INTERVAL), None);
    }
}
