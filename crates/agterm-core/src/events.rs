//! The app-run control event ring and `events.read` (upstream's `ControlEvents.swift`, the library's event sink in
//! `WindowLibrary.makeStore` with its per-window `scheduleTreeChanged(for:)`, and
//! `ControlDispatcher.dispatchEventsRead`).
//!
//! The model's stores emit drafts into their outboxes; every commit hands them here in emission order
//! ([`ControlEvents::record`]). An event is sequenced at once, except `tree.changed`: that one coalesces per window
//! behind a [`TREE_CHANGED_DEBOUNCE`] quiet window, re-armed by every later one, so a burst of structural changes
//! reads as one invalidation. The ring is bounded and reads are non-destructive: each consumer keeps its own cursor
//! (the run id and the last sequence it saw), and a cursor the ring can no longer serve fails loudly, carrying the
//! ring's current anchor so the consumer can rebaseline.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use agterm_proto::{Args, ControlEvent, EventBatch, EventKind, EventPayload};
use uuid::Uuid;

use crate::error::ControlError;
use crate::model::WindowEvent;
use crate::pane_env::uuid_string;
use crate::persist::parse_uuid_string;

/// How many events the ring keeps (upstream's `ControlEventRing.defaultCapacity`).
pub const DEFAULT_CAPACITY: usize = 4_096;
/// The quiet window that coalesces a window's `tree.changed` events (upstream's 0.1 s debouncer).
pub const TREE_CHANGED_DEBOUNCE: Duration = Duration::from_millis(100);
/// The page size of a read that names none.
pub const DEFAULT_LIMIT: usize = 100;
/// The largest page a read may ask for.
pub const MAX_LIMIT: usize = 1_000;

/// An event before the ring assigns its sequence and timestamp (upstream's `ControlEventDraft`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventDraft {
    pub kind: EventKind,
    pub window: Option<String>,
    pub workspace: Option<String>,
    pub session: Option<String>,
    pub payload: EventPayload,
}

impl From<WindowEvent> for EventDraft {
    /// The library's sink: the store's draft, tagged with its window.
    fn from(event: WindowEvent) -> Self {
        Self {
            kind: event.event.kind,
            window: Some(uuid_string(event.window)),
            workspace: event.event.workspace.map(uuid_string),
            session: event.event.session.map(uuid_string),
            payload: event.event.payload,
        }
    }
}

/// A consumer's position within one app run (upstream's `ControlEventCursor`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventCursor {
    pub run: Uuid,
    /// The last sequence the consumer saw.
    pub after: u64,
}

/// A cursor the ring cannot serve (upstream's `ControlEventReadError`); the consumer chooses whether to rebaseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EventReadError {
    /// The cursor belongs to another app run.
    #[error("event run changed")]
    RunChanged,
    /// The events after the cursor were evicted.
    #[error("event cursor expired")]
    CursorExpired,
    /// The cursor names a sequence not reached yet.
    #[error("event cursor is ahead of the current sequence")]
    CursorAhead,
}

/// A failed read: the error and the ring's current anchor (an empty page at the tail).
#[derive(Debug, Clone, PartialEq)]
pub struct EventReadFailure {
    pub error: EventReadError,
    pub anchor: EventBatch,
}

/// An `events.read` request the dispatcher refuses before reading (upstream's `ControlEventRequestError`); the CLI
/// refuses the same arguments with the same text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EventRequestError {
    #[error("events.read requires --run and --after together")]
    CursorPair,
    #[error("invalid event cursor")]
    InvalidCursor,
    #[error("invalid event run id")]
    InvalidRun,
    #[error("event limit must be between 1 and 1000")]
    InvalidLimit,
    #[error("invalid event kind: {0}")]
    InvalidKind(String),
}

impl From<EventRequestError> for ControlError {
    fn from(error: EventRequestError) -> Self {
        Self::Rejected(error.to_string())
    }
}

/// A validated `events.read` request (upstream's `ControlEventReadOptions`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadOptions {
    /// `None` subscribes from now: the read anchors at the tail and returns no history.
    pub cursor: Option<EventCursor>,
    /// The kinds to return; `None` returns every kind.
    pub kinds: Option<HashSet<EventKind>>,
    /// The most events one page returns.
    pub limit: usize,
}

impl Default for ReadOptions {
    fn default() -> Self {
        Self {
            cursor: None,
            kinds: None,
            limit: DEFAULT_LIMIT,
        }
    }
}

impl ReadOptions {
    /// Validates `events.read`'s arguments as upstream's dispatcher does, in its order: the cursor pair, the run id,
    /// the cursor, the limit, then each kind. A kinds entry may hold several comma-separated kinds, each trimmed; an
    /// empty component is an unknown kind.
    ///
    /// # Errors
    ///
    /// The first refused argument.
    pub fn parse(args: Option<&Args>) -> Result<Self, EventRequestError> {
        let cursor = match (
            args.and_then(|args| args.run.as_deref()),
            args.and_then(|args| args.after.as_deref()),
        ) {
            (None, None) => None,
            (Some(_), None) | (None, Some(_)) => return Err(EventRequestError::CursorPair),
            (Some(run), Some(after)) => Some(EventCursor {
                run: parse_uuid_string(run).ok_or(EventRequestError::InvalidRun)?,
                after: after.parse().map_err(|_| EventRequestError::InvalidCursor)?,
            }),
        };
        let limit = match args.and_then(|args| args.limit) {
            None => DEFAULT_LIMIT,
            Some(limit) => usize::try_from(limit)
                .ok()
                .filter(|limit| (1..=MAX_LIMIT).contains(limit))
                .ok_or(EventRequestError::InvalidLimit)?,
        };
        let fields = args.and_then(|args| args.kinds.as_deref()).unwrap_or_default();
        let kinds = parse_kinds(fields)?;
        Ok(Self {
            cursor,
            kinds: (!kinds.is_empty()).then_some(kinds),
            limit,
        })
    }
}

/// Every kind named in `fields`, each field split on commas and each component trimmed.
fn parse_kinds(fields: &[String]) -> Result<HashSet<EventKind>, EventRequestError> {
    fields
        .iter()
        .flat_map(|field| field.split(','))
        .map(str::trim)
        .map(|raw| {
            raw.parse::<EventKind>()
                .map_err(|_| EventRequestError::InvalidKind(raw.to_owned()))
        })
        .collect()
}

/// Seconds since the Unix epoch, now.
fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64())
}

/// The ring's clock, in seconds since the Unix epoch.
pub type Clock = Box<dyn Fn() -> f64>;

/// The bounded, non-destructive event history of one app run (upstream's `ControlEventRing`). It lives on the main
/// thread with the model, so it needs no lock and no subscriber lifecycle.
pub struct EventRing {
    capacity: usize,
    run: Uuid,
    now: Clock,
    entries: VecDeque<ControlEvent>,
    /// The last sequence assigned; 0 before the first event.
    current: u64,
}

impl fmt::Debug for EventRing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EventRing")
            .field("capacity", &self.capacity)
            .field("run", &self.run)
            .field("entries", &self.entries.len())
            .field("current", &self.current)
            .finish_non_exhaustive()
    }
}

impl Default for EventRing {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY, Uuid::new_v4(), Box::new(unix_now))
    }
}

impl EventRing {
    /// A ring keeping `capacity` events (at least one) for the run `run`, stamping events with `now`.
    #[must_use]
    pub fn new(capacity: usize, run: Uuid, now: Clock) -> Self {
        Self {
            capacity: capacity.max(1),
            run,
            now,
            entries: VecDeque::new(),
            current: 0,
        }
    }

    /// The app run's id.
    #[must_use]
    pub fn run(&self) -> Uuid {
        self.run
    }

    /// The last sequence assigned.
    #[must_use]
    pub fn current_sequence(&self) -> u64 {
        self.current
    }

    /// Appends one event with the next sequence and the current time, evicting the oldest past the capacity.
    pub fn append(&mut self, draft: EventDraft) -> ControlEvent {
        self.current += 1;
        let event = ControlEvent {
            seq: self.current,
            ts: (self.now)(),
            kind: draft.kind,
            window: draft.window,
            workspace: draft.workspace,
            session: draft.session,
            payload: draft.payload,
        };
        self.entries.push_back(event.clone());
        while self.entries.len() > self.capacity {
            self.entries.pop_front();
        }
        event
    }

    fn batch(&self, next: u64, items: Vec<ControlEvent>) -> EventBatch {
        EventBatch {
            run: uuid_string(self.run),
            next,
            items,
        }
    }

    /// The retained events after the options' cursor, of its kinds, at most its limit of them. A page that filled up
    /// stops its `next` at its last event, so the later matches are not skipped; otherwise `next` is the tail, the
    /// filtered-out events included. Without a cursor the read anchors at the tail and returns no history.
    ///
    /// # Errors
    ///
    /// A cursor of another run, ahead of the tail, or older than the oldest retained event's predecessor; the
    /// failure carries the current anchor.
    pub fn read(&self, options: &ReadOptions) -> Result<EventBatch, EventReadFailure> {
        let Some(cursor) = options.cursor else {
            return Ok(self.batch(self.current, Vec::new()));
        };
        let fail = |error| EventReadFailure {
            error,
            anchor: self.batch(self.current, Vec::new()),
        };
        if cursor.run != self.run {
            return Err(fail(EventReadError::RunChanged));
        }
        if cursor.after > self.current {
            return Err(fail(EventReadError::CursorAhead));
        }
        let oldest = self.entries.front().map_or(self.current + 1, |event| event.seq);
        if cursor.after + 1 < oldest {
            return Err(fail(EventReadError::CursorExpired));
        }
        // sequences are contiguous, so the first event after the cursor sits at a known index
        let start = usize::try_from(cursor.after + 1 - oldest).unwrap_or(usize::MAX);
        let mut items = Vec::new();
        for event in self.entries.iter().skip(start) {
            if options.kinds.as_ref().is_some_and(|kinds| !kinds.contains(&event.kind)) {
                continue;
            }
            items.push(event.clone());
            if items.len() == options.limit {
                return Ok(self.batch(event.seq, items));
            }
        }
        Ok(self.batch(self.current, items))
    }
}

/// A window's debounced `tree.changed`, armed on the main loop for [`TREE_CHANGED_DEBOUNCE`]; stale once a later
/// structural change of the window re-armed the debounce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeChangedTimer {
    pub window: Uuid,
    token: u64,
}

/// The ring with its sink: what the model's commits feed, and what `events.read` reads.
#[derive(Debug, Default)]
pub struct ControlEvents {
    ring: EventRing,
    /// The live debounce of each window with a `tree.changed` pending, by token.
    pending_tree_changed: HashMap<Uuid, u64>,
    next_token: u64,
}

impl ControlEvents {
    /// The sink over `ring`.
    #[must_use]
    pub fn new(ring: EventRing) -> Self {
        Self {
            ring,
            pending_tree_changed: HashMap::new(),
            next_token: 0,
        }
    }

    /// The ring.
    #[must_use]
    pub fn ring(&self) -> &EventRing {
        &self.ring
    }

    /// Takes a commit's events, in emission order: each is sequenced now, except `tree.changed`, which (re)arms its
    /// window's debounce. The timers to arm come back, one per window whatever its count of `tree.changed` events.
    pub fn record(&mut self, events: Vec<WindowEvent>) -> Vec<TreeChangedTimer> {
        let mut debounced: Vec<Uuid> = Vec::new();
        for event in events {
            if event.event.kind == EventKind::TreeChanged {
                if !debounced.contains(&event.window) {
                    debounced.push(event.window);
                }
                continue;
            }
            self.ring.append(event.into());
        }
        debounced.into_iter().map(|window| self.arm(window)).collect()
    }

    fn arm(&mut self, window: Uuid) -> TreeChangedTimer {
        self.next_token += 1;
        self.pending_tree_changed.insert(window, self.next_token);
        TreeChangedTimer {
            window,
            token: self.next_token,
        }
    }

    /// A debounce's quiet window elapsed: sequences the window's `tree.changed`, unless a later change re-armed it.
    /// The window may be gone by now; its invalidation still lands. The event, when one was appended.
    pub fn fire_tree_changed(&mut self, timer: TreeChangedTimer) -> Option<ControlEvent> {
        if self.pending_tree_changed.get(&timer.window) != Some(&timer.token) {
            return None;
        }
        self.pending_tree_changed.remove(&timer.window);
        Some(self.ring.append(tree_changed(timer.window)))
    }

    /// Sequences every pending `tree.changed` now, in the order they were armed (upstream's `flushTreeEvents`, a
    /// test seam); their armed timers turn stale.
    pub fn flush_tree_changed(&mut self) {
        let mut pending: Vec<(u64, Uuid)> = self
            .pending_tree_changed
            .drain()
            .map(|(window, token)| (token, window))
            .collect();
        pending.sort_unstable();
        for (_, window) in pending {
            self.ring.append(tree_changed(window));
        }
    }

    /// Reads the ring (see [`EventRing::read`]).
    ///
    /// # Errors
    ///
    /// A cursor the ring cannot serve.
    pub fn read(&self, options: &ReadOptions) -> Result<EventBatch, EventReadFailure> {
        self.ring.read(options)
    }
}

/// A window's `tree.changed`: its window and an empty payload.
fn tree_changed(window: Uuid) -> EventDraft {
    EventDraft {
        kind: EventKind::TreeChanged,
        window: Some(uuid_string(window)),
        workspace: None,
        session: None,
        payload: EventPayload::default(),
    }
}

#[cfg(test)]
mod tests;
