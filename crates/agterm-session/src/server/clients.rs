//! The clients attached to one pane, and the queue of what each is sent: its snapshots, the pane's output, tagged
//! with its offset in the session's output stream, its size changes and its end.
//!
//! herdr: `src/server/client_transport.rs` (`ClientWriterQueue`, the per-client writer queue the PTY side pushes into
//! without waiting for the client's socket) and `src/server/clients.rs` (the registry of a session's clients) at
//! `HERDR_REV` (`build/pins.env`), licensed under the Apache License 2.0, modified. Kept: a queue per client, filled by
//! the pane and drained by the client's own writer, so a slow client never holds up the PTY reader or the other
//! clients. Changed: the queue carries agterm's raw output lane, not herdr's rendered frames, so it is lossless where
//! herdr's render lane keeps only the latest frame: it is bounded in bytes, and output that does not fit is never
//! dropped silently. The queue drops the output, size changes and unsent snapshot it holds, none of which was sent
//! yet, and ends with [`ClientEvent::Overflowed`] at the offset where the client's copy of the stream stops; the pane
//! then resyncs that client in band (a new [`ClientEvent::Snapshot`] in place of what it was not sent) or, past its
//! [`ResyncBudget`], disconnects it.
//!
//! # Epochs
//!
//! Every snapshot queued for a client opens an epoch, numbered from 1: its first snapshot, at its attach, then one per
//! resync. A snapshot an overflow drops unsent still used its number, so the first epoch a client is sent is 1 unless
//! its queue overflowed before its first snapshot went out. The output after a snapshot continues it. An answer the
//! client gives in an epoch that is no longer its current one (a delegated reply sent before its resync) is dropped
//! ([`Clients::epoch`]).
//!
//! # Order
//!
//! The pane queues under its own lock, the one that also feeds its terminal, so every client sees the same events in
//! the same order, each output contiguous with the one before it (`offset` plus the length of `data` is the next
//! offset), until its queue overflows or the session ends; a snapshot's offset is where the output after it starts. A
//! managed client's [`ClientEvent::Role`] reports ([`super::lead`]) sit in that order too, its first before anything
//! else it is sent, and outlast an overflow; so do every client's [`ClientEvent::ReplyOwner`] reports
//! ([`super::replies`]), its first right after its first role report. A [`ClientEvent::Delegated`] request and a
//! [`ClientEvent::KittyBarrier`] go to one client in its current epoch, after the output they concern, and an overflow
//! drops them with that output.
//! [`ClientEvent::Exit`] is always queued, after all output, and is the last event of a session.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::protocol::{DelegatedRequest, ExitStatus, GridSize, LeadRole};

/// The output bytes a client's queue holds before it overflows.
pub const DEFAULT_QUEUE_LIMIT: usize = 8 * 1024 * 1024;

/// How many resyncs a client gets within [`RESYNC_WINDOW`] before its next overflow disconnects it.
pub const RESYNC_LIMIT: u32 = 4;
/// The window [`RESYNC_LIMIT`] counts resyncs in.
pub const RESYNC_WINDOW: Duration = Duration::from_secs(60);

/// How often a client that keeps falling behind is resynced before it is disconnected: at most `resyncs` within any
/// `window`. Each resync costs the server a snapshot of the session, scrollback included, under the pane's lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResyncBudget {
    pub resyncs: u32,
    pub window: Duration,
}

impl Default for ResyncBudget {
    /// [`RESYNC_LIMIT`] within [`RESYNC_WINDOW`].
    fn default() -> Self {
        Self {
            resyncs: RESYNC_LIMIT,
            window: RESYNC_WINDOW,
        }
    }
}

/// The most output one [`ClientEvent::Output`] carries once queued reads are merged (well under the wire's frame
/// size, `crate::protocol::MAX_FRAME_SIZE`).
pub const MERGE_LIMIT: usize = 64 * 1024;

/// A client of one pane, unique within the server's life.
pub type ClientId = u64;

/// What a client is sent, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientEvent {
    /// The session's state at `offset` (its grid then `size`), which opens the client's epoch `epoch`: the client's
    /// terminal takes `data` as output, and the output from `offset` on continues it. It replaces the output the client
    /// was not sent; empty while the session has no output.
    Snapshot {
        epoch: u64,
        offset: u64,
        size: GridSize,
        data: Vec<u8>,
    },
    /// The session's output from `offset` on.
    Output { offset: u64, data: Vec<u8> },
    /// The session's grid changed, in the output at `offset`.
    Resized { offset: u64, size: GridSize },
    /// The client's role at the session changed (sent to a managed client only); `generation` counts the session's
    /// leadership changes.
    Role { role: LeadRole, generation: u32 },
    /// Whether the client owns the session's replies from here on ([`super::replies`]); `generation` counts the
    /// session's ownership changes. Sent to every client, its first before its first snapshot.
    ReplyOwner { owner: bool, generation: u32 },
    /// A request the client, as the reply owner, is to answer in its epoch `epoch` ([`super::replies`]).
    Delegated {
        id: u64,
        epoch: u64,
        request: DelegatedRequest,
    },
    /// The output before it ended with a Kitty graphics command the client, as the reply owner, is to answer in its
    /// epoch `epoch` ([`super::replies`]).
    KittyBarrier { id: u64, epoch: u64 },
    /// The client's queue was full: the output, size changes, delegated requests and unsent snapshot it held were
    /// dropped, output from `offset` on was not queued, and nothing but role and reply-ownership changes, the
    /// session's last snapshot and its exit is queued after this until the client is resynced (`Pane::resync`).
    Overflowed { offset: u64 },
    /// The session's program ended; the last event.
    Exit(ExitStatus),
}

/// Why [`Subscription::recv_timeout`] returned no event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RecvError {
    /// Nothing arrived in time.
    #[error("no client event in time")]
    Timeout,
    /// Nothing more will arrive: the client was detached, or the session ended and its exit was received.
    #[error("the client is detached")]
    Closed,
}

/// One client's queue.
#[derive(Debug)]
struct Queue {
    events: VecDeque<ClientEvent>,
    /// Output bytes queued.
    bytes: usize,
    limit: usize,
    /// Output stopped being queued: an [`ClientEvent::Overflowed`] is queued.
    overflowed: bool,
    /// Nothing more is queued: the client detached, or the session's exit is queued.
    closed: bool,
    /// The client's epoch: its snapshots so far.
    epoch: u64,
    /// When the client was resynced, within the last budget window.
    resyncs: VecDeque<Instant>,
}

impl Queue {
    fn new(limit: usize) -> Self {
        Self {
            events: VecDeque::new(),
            bytes: 0,
            limit,
            overflowed: false,
            closed: false,
            epoch: 0,
            resyncs: VecDeque::new(),
        }
    }

    fn push_output(&mut self, offset: u64, data: &[u8]) {
        if self.closed || self.overflowed || data.is_empty() {
            return;
        }
        // an empty queue takes any read, so a limit below one read cannot overflow a client that keeps up
        if self.bytes > 0 && self.bytes + data.len() > self.limit {
            self.overflow(offset);
            return;
        }
        self.bytes += data.len();
        if let Some(ClientEvent::Output {
            offset: last,
            data: queued,
        }) = self.events.back_mut()
            && *last + queued.len() as u64 == offset
            && queued.len() + data.len() <= MERGE_LIMIT
        {
            queued.extend_from_slice(data);
            return;
        }
        self.events.push_back(ClientEvent::Output {
            offset,
            data: data.to_vec(),
        });
    }

    /// Drop the snapshot, output and size changes queued (the client was sent none of them, and its resync replaces
    /// them), keeping the role changes, and report where its copy of the stream stops: at the first of them dropped,
    /// else at `offset`.
    fn overflow(&mut self, offset: u64) {
        let mut stops = offset;
        self.events.retain(|event| match event {
            ClientEvent::Snapshot { offset, .. }
            | ClientEvent::Output { offset, .. }
            | ClientEvent::Resized { offset, .. } => {
                stops = stops.min(*offset);
                false
            }
            ClientEvent::Delegated { .. } | ClientEvent::KittyBarrier { .. } => false,
            ClientEvent::Role { .. }
            | ClientEvent::ReplyOwner { .. }
            | ClientEvent::Overflowed { .. }
            | ClientEvent::Exit(_) => true,
        });
        self.bytes = 0;
        self.overflowed = true;
        self.events.push_back(ClientEvent::Overflowed { offset: stops });
    }

    /// Queue a snapshot as the client's next epoch, which ends its overflow; the epoch, `None` once nothing more is
    /// queued.
    fn push_snapshot(&mut self, offset: u64, size: GridSize, data: Vec<u8>) -> Option<u64> {
        if self.closed {
            return None;
        }
        self.epoch += 1;
        self.overflowed = false;
        self.events.push_back(ClientEvent::Snapshot {
            epoch: self.epoch,
            offset,
            size,
            data,
        });
        Some(self.epoch)
    }

    /// Count a resync at `now` against `budget`; false, counting nothing, once the client used it up.
    fn spend(&mut self, budget: ResyncBudget, now: Instant) -> bool {
        while self
            .resyncs
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= budget.window)
        {
            self.resyncs.pop_front();
        }
        if self.resyncs.len() >= usize::try_from(budget.resyncs).unwrap_or(usize::MAX) {
            return false;
        }
        self.resyncs.push_back(now);
        true
    }

    fn push_resized(&mut self, offset: u64, size: GridSize) {
        if !self.closed && !self.overflowed {
            self.events.push_back(ClientEvent::Resized { offset, size });
        }
    }

    /// A role is queued after an overflow too: the client keeps its role through a resync.
    fn push_role(&mut self, role: LeadRole, generation: u32) {
        if !self.closed {
            self.events.push_back(ClientEvent::Role { role, generation });
        }
    }

    /// Kept past an overflow, as a role is.
    fn push_reply_owner(&mut self, owner: bool, generation: u32) {
        if !self.closed {
            self.events.push_back(ClientEvent::ReplyOwner { owner, generation });
        }
    }

    /// Queue a delegated request in the client's current epoch; that epoch, `None` (nothing queued) once the queue
    /// overflowed or closed.
    fn push_delegated(&mut self, id: u64, request: DelegatedRequest) -> Option<u64> {
        self.push_in_epoch(|epoch| ClientEvent::Delegated { id, epoch, request })
    }

    /// Queue the event `make` builds around the client's current epoch; that epoch, `None` (nothing queued) once the
    /// queue overflowed or closed.
    fn push_in_epoch(&mut self, make: impl FnOnce(u64) -> ClientEvent) -> Option<u64> {
        if self.closed || self.overflowed {
            return None;
        }
        self.events.push_back(make(self.epoch));
        Some(self.epoch)
    }

    fn push_exit(&mut self, status: ExitStatus) {
        if !self.closed {
            self.events.push_back(ClientEvent::Exit(status));
            self.closed = true;
        }
    }

    fn pop(&mut self) -> Option<ClientEvent> {
        let event = self.events.pop_front()?;
        if let ClientEvent::Output { data, .. } = &event {
            self.bytes -= data.len();
        }
        Some(event)
    }
}

/// What the pane and the client share of one queue.
#[derive(Debug)]
struct Slot {
    queue: Mutex<Queue>,
    ready: Condvar,
}

impl Slot {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        // a queue a panicking thread left stays consistent: every change is one push or pop
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Change the queue, then wake its client.
    fn with(&self, change: impl FnOnce(&mut Queue)) {
        change(&mut self.lock());
        self.ready.notify_all();
    }
}

/// What runs when a subscription is dropped, once its queue is closed.
type OnDrop = Box<dyn FnOnce(ClientId) + Send + Sync>;

/// The client's end of its queue. Dropping it detaches the client: the pane stops queuing for it, and a pane's own
/// subscription ([`Subscription::on_drop`]) has the pane forget the client at once, so dropping one takes that pane's
/// lock.
pub struct Subscription {
    id: ClientId,
    slot: Arc<Slot>,
    on_drop: Option<OnDrop>,
}

impl std::fmt::Debug for Subscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Subscription")
            .field("id", &self.id)
            .field("slot", &self.slot)
            .finish_non_exhaustive()
    }
}

impl Subscription {
    #[must_use]
    pub fn id(&self) -> ClientId {
        self.id
    }

    /// Run `hook` with the client's id when this subscription is dropped, after its queue is closed (the pane forgets
    /// the client then, and its part in the lead, rather than at its next event); it replaces any hook set before.
    pub fn on_drop(&mut self, hook: impl FnOnce(ClientId) + Send + Sync + 'static) {
        self.on_drop = Some(Box::new(hook));
    }

    /// The next event, waiting at most `timeout` for one.
    ///
    /// # Errors
    ///
    /// [`RecvError::Timeout`] when none came in time, [`RecvError::Closed`] once the queue is empty and nothing more
    /// will be queued.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<ClientEvent, RecvError> {
        let deadline = Instant::now() + timeout;
        let mut queue = self.slot.lock();
        loop {
            if let Some(event) = queue.pop() {
                return Ok(event);
            }
            if queue.closed {
                return Err(RecvError::Closed);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(RecvError::Timeout);
            }
            queue = self
                .slot
                .ready
                .wait_timeout(queue, left)
                .map_or_else(|poisoned| poisoned.into_inner().0, |(guard, _)| guard);
        }
    }

    /// The next event if one is queued.
    ///
    /// # Errors
    ///
    /// As [`Subscription::recv_timeout`], without waiting.
    pub fn try_recv(&self) -> Result<ClientEvent, RecvError> {
        self.recv_timeout(Duration::ZERO)
    }

    /// The output bytes queued for this client and not yet received.
    #[must_use]
    pub fn queued(&self) -> usize {
        self.slot.lock().bytes
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.slot.with(|queue| queue.closed = true);
        if let Some(on_drop) = self.on_drop.take() {
            on_drop(self.id);
        }
    }
}

/// A pane's attached clients.
#[derive(Debug)]
pub struct Clients {
    slots: Vec<(ClientId, Arc<Slot>)>,
    limit: usize,
}

impl Clients {
    /// No clients yet; each client's queue holds `limit` output bytes ([`DEFAULT_QUEUE_LIMIT`]).
    #[must_use]
    pub fn new(limit: usize) -> Self {
        Self {
            slots: Vec::new(),
            limit,
        }
    }

    /// Attach the client `id`: it is sent every event queued from now on.
    pub fn attach(&mut self, id: ClientId) -> Subscription {
        let slot = Arc::new(Slot {
            queue: Mutex::new(Queue::new(self.limit)),
            ready: Condvar::new(),
        });
        self.slots.push((id, Arc::clone(&slot)));
        Subscription {
            id,
            slot,
            on_drop: None,
        }
    }

    /// Detach the client `id`; whether it was attached. What its queue holds is still received, then
    /// [`RecvError::Closed`].
    pub fn detach(&mut self, id: ClientId) -> bool {
        let Some(index) = self.slots.iter().position(|(client, _)| *client == id) else {
            return false;
        };
        let (_, slot) = self.slots.swap_remove(index);
        slot.with(|queue| queue.closed = true);
        true
    }

    /// The clients attached, those whose subscription was dropped not counted.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.iter().filter(|(_, slot)| !slot.lock().closed).count()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Queue the session's output `data`, which starts at `offset`, for every client. Never waits.
    pub fn output(&mut self, offset: u64, data: &[u8]) {
        for (_, slot) in &self.slots {
            slot.with(|queue| queue.push_output(offset, data));
        }
    }

    /// Queue the grid's change to `size` at `offset` for every client.
    pub fn resized(&mut self, offset: u64, size: GridSize) {
        for (_, slot) in &self.slots {
            slot.with(|queue| queue.push_resized(offset, size));
        }
    }

    /// Queue the client `id`'s new role; whether it is attached.
    pub fn role(&mut self, id: ClientId, role: LeadRole, generation: u32) -> bool {
        let Some(slot) = self.slot(id) else {
            return false;
        };
        slot.with(|queue| queue.push_role(role, generation));
        true
    }

    /// Queue whether the client `id` owns the replies; whether it is attached.
    pub fn reply_owner(&mut self, id: ClientId, owner: bool, generation: u32) -> bool {
        let Some(slot) = self.slot(id) else {
            return false;
        };
        slot.with(|queue| queue.push_reply_owner(owner, generation));
        true
    }

    /// Queue the delegated request `id` for the client `client` in its current epoch; that epoch, `None` when it is
    /// not attached or its queue overflowed (it would not get the request before its resync).
    pub fn delegate(&mut self, client: ClientId, id: u64, request: DelegatedRequest) -> Option<u64> {
        let slot = self.slot(client)?;
        let mut epoch = None;
        slot.with(|queue| epoch = queue.push_delegated(id, request));
        epoch
    }

    /// Queue the Kitty barrier `id` for the client `client` in its current epoch; that epoch, `None` when it is not
    /// attached or its queue overflowed (it would not get the barrier before its resync).
    pub fn barrier(&mut self, client: ClientId, id: u64) -> Option<u64> {
        let slot = self.slot(client)?;
        let mut epoch = None;
        slot.with(|queue| epoch = queue.push_in_epoch(|epoch| ClientEvent::KittyBarrier { id, epoch }));
        epoch
    }

    fn slot(&self, id: ClientId) -> Option<&Arc<Slot>> {
        self.slots
            .iter()
            .find(|(client, _)| *client == id)
            .map(|(_, slot)| slot)
    }

    /// Queue a snapshot for the client `id` as its next epoch ([`ClientEvent::Snapshot`]), which ends its overflow;
    /// the epoch, `None` when it is not attached.
    pub fn snapshot(&mut self, id: ClientId, offset: u64, size: GridSize, data: Vec<u8>) -> Option<u64> {
        let slot = self.slot(id)?;
        let mut epoch = None;
        slot.with(|queue| epoch = queue.push_snapshot(offset, size, data));
        epoch
    }

    /// Whether the client `id`'s queue overflowed and awaits its resync; `None` when it is not attached.
    #[must_use]
    pub fn overflowed(&self, id: ClientId) -> Option<bool> {
        self.slot(id).map(|slot| slot.lock().overflowed)
    }

    /// The clients whose queue overflowed and await their resync.
    #[must_use]
    pub fn overflowed_clients(&self) -> Vec<ClientId> {
        self.slots
            .iter()
            .filter(|(_, slot)| {
                let queue = slot.lock();
                queue.overflowed && !queue.closed
            })
            .map(|(id, _)| *id)
            .collect()
    }

    /// The client `id`'s current epoch (zero before its first snapshot); `None` when it is not attached.
    #[must_use]
    pub fn epoch(&self, id: ClientId) -> Option<u64> {
        self.slot(id).map(|slot| slot.lock().epoch)
    }

    /// Count a resync of the client `id` at `now` against `budget`: false, counting nothing, once it used the budget
    /// up or when it is not attached.
    pub fn spend_resync(&mut self, id: ClientId, budget: ResyncBudget, now: Instant) -> bool {
        self.slot(id).is_some_and(|slot| slot.lock().spend(budget, now))
    }

    /// Queue the session's end for every client and detach them all.
    pub fn exit(&mut self, status: &ExitStatus) {
        for (_, slot) in self.slots.drain(..) {
            slot.with(|queue| queue.push_exit(status.clone()));
        }
    }

    /// Forget the clients that dropped their subscription; their ids, so their part in the session (its lead) goes
    /// too.
    pub fn prune(&mut self) -> Vec<ClientId> {
        let mut dropped = Vec::new();
        self.slots.retain(|(id, slot)| {
            let open = !slot.lock().closed;
            if !open {
                dropped.push(*id);
            }
            open
        });
        dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: GridSize = GridSize { cols: 80, rows: 24 };

    fn drain(subscription: &Subscription) -> Vec<ClientEvent> {
        std::iter::from_fn(|| subscription.try_recv().ok()).collect()
    }

    #[test]
    fn every_client_gets_the_same_events_in_order() {
        let mut clients = Clients::new(DEFAULT_QUEUE_LIMIT);
        let one = clients.attach(1);
        let two = clients.attach(2);
        clients.output(0, b"abc");
        clients.resized(3, SIZE);
        clients.output(3, b"de");
        let expected = [
            ClientEvent::Output {
                offset: 0,
                data: b"abc".to_vec(),
            },
            ClientEvent::Resized { offset: 3, size: SIZE },
            ClientEvent::Output {
                offset: 3,
                data: b"de".to_vec(),
            },
        ];
        assert_eq!(drain(&one), expected);
        assert_eq!(drain(&two), expected);
        assert_eq!(one.try_recv(), Err(RecvError::Timeout));
    }

    #[test]
    fn contiguous_output_is_merged_up_to_the_limit() {
        let mut clients = Clients::new(DEFAULT_QUEUE_LIMIT);
        let client = clients.attach(1);
        clients.output(0, b"ab");
        clients.output(2, b"cd");
        let big = vec![b'x'; MERGE_LIMIT];
        clients.output(4, &big);
        let events = drain(&client);
        assert_eq!(
            events[0],
            ClientEvent::Output {
                offset: 0,
                data: b"abcd".to_vec()
            }
        );
        assert_eq!(events[1], ClientEvent::Output { offset: 4, data: big });
        assert_eq!(events.len(), 2);
        assert_eq!(client.queued(), 0);
    }

    #[test]
    fn a_full_queue_drops_what_it_holds_and_reports_where_the_clients_copy_stops() {
        let mut clients = Clients::new(4);
        let slow = clients.attach(1);
        assert_eq!(clients.snapshot(1, 0, SIZE, Vec::new()), Some(1));
        clients.output(0, b"abc");
        clients.output(3, b"de");
        clients.output(5, b"f");
        clients.resized(6, SIZE);
        assert_eq!(clients.overflowed(1), Some(true));
        clients.exit(&ExitStatus::Exited { code: 0, signal: None });
        assert_eq!(
            drain(&slow),
            [
                ClientEvent::Overflowed { offset: 0 },
                ClientEvent::Exit(ExitStatus::Exited { code: 0, signal: None }),
            ]
        );
        assert_eq!(slow.try_recv(), Err(RecvError::Closed));
        assert_eq!(slow.queued(), 0);
    }

    #[test]
    fn a_resync_snapshot_ends_the_overflow_and_opens_the_next_epoch() {
        let mut clients = Clients::new(4);
        let client = clients.attach(1);
        assert_eq!(clients.epoch(1), Some(0));
        assert_eq!(clients.snapshot(1, 0, SIZE, b"first".to_vec()), Some(1));
        // the first snapshot was sent, then output it never got
        assert!(matches!(client.try_recv(), Ok(ClientEvent::Snapshot { epoch: 1, .. })));
        clients.output(0, b"abc");
        clients.output(3, b"de");
        assert_eq!(drain(&client), [ClientEvent::Overflowed { offset: 0 }]);
        clients.output(5, b"lost");
        assert_eq!(clients.snapshot(1, 9, SIZE, b"state".to_vec()), Some(2));
        assert_eq!(clients.overflowed(1), Some(false));
        assert_eq!(clients.epoch(1), Some(2));
        clients.output(9, b"next");
        assert_eq!(
            drain(&client),
            [
                ClientEvent::Snapshot {
                    epoch: 2,
                    offset: 9,
                    size: SIZE,
                    data: b"state".to_vec()
                },
                ClientEvent::Output {
                    offset: 9,
                    data: b"next".to_vec()
                },
            ]
        );
        assert_eq!(clients.snapshot(7, 0, SIZE, Vec::new()), None, "no such client");
        assert_eq!(clients.epoch(7), None);
        clients.detach(1);
        assert_eq!(
            clients.snapshot(1, 13, SIZE, Vec::new()),
            None,
            "a detached client gets none"
        );
    }

    #[test]
    fn the_resync_budget_counts_within_its_window() {
        let mut clients = Clients::new(4);
        let _client = clients.attach(1);
        let budget = ResyncBudget {
            resyncs: 2,
            window: Duration::from_secs(10),
        };
        let start = Instant::now();
        assert!(clients.spend_resync(1, budget, start));
        assert!(clients.spend_resync(1, budget, start + Duration::from_secs(1)));
        assert!(
            !clients.spend_resync(1, budget, start + Duration::from_secs(2)),
            "spent"
        );
        assert!(
            !clients.spend_resync(1, budget, start + Duration::from_secs(9)),
            "a refusal counts nothing"
        );
        assert!(
            clients.spend_resync(1, budget, start + Duration::from_secs(10)),
            "the first one left the window"
        );
        assert!(!clients.spend_resync(2, budget, start), "not attached");
        let none = ResyncBudget {
            resyncs: 0,
            window: Duration::from_secs(10),
        };
        assert!(!clients.spend_resync(1, none, start));
    }

    #[test]
    fn an_empty_queue_takes_a_read_larger_than_its_limit() {
        let mut clients = Clients::new(4);
        let client = clients.attach(1);
        clients.output(0, b"a long read");
        assert_eq!(client.queued(), 11);
        assert!(matches!(client.try_recv(), Ok(ClientEvent::Output { offset: 0, .. })));
        clients.output(11, b"more");
        assert!(matches!(client.try_recv(), Ok(ClientEvent::Output { offset: 11, .. })));
    }

    #[test]
    fn a_detached_client_gets_what_was_queued_then_closed() {
        let mut clients = Clients::new(DEFAULT_QUEUE_LIMIT);
        let client = clients.attach(7);
        clients.output(0, b"x");
        assert!(clients.detach(7));
        assert!(!clients.detach(7));
        clients.output(1, b"y");
        assert_eq!(
            drain(&client),
            [ClientEvent::Output {
                offset: 0,
                data: b"x".to_vec()
            }]
        );
        assert_eq!(client.recv_timeout(Duration::from_secs(5)), Err(RecvError::Closed));
    }

    #[test]
    fn a_dropped_subscription_detaches_its_client() {
        let mut clients = Clients::new(DEFAULT_QUEUE_LIMIT);
        let kept = clients.attach(1);
        drop(clients.attach(2));
        assert_eq!(clients.len(), 1);
        clients.output(0, b"x");
        assert_eq!(kept.queued(), 1);
        assert_eq!(clients.prune(), [2]);
        assert!(clients.prune().is_empty());
        assert!(!clients.role(2, LeadRole::Leader, 1));
    }

    #[test]
    fn a_dropped_subscription_runs_its_hook_once_its_queue_is_closed() {
        let mut clients = Clients::new(DEFAULT_QUEUE_LIMIT);
        let mut subscription = clients.attach(3);
        let (sender, dropped) = std::sync::mpsc::channel();
        subscription.on_drop(move |id| sender.send(id).unwrap());
        assert!(dropped.try_recv().is_err());
        drop(subscription);
        assert_eq!(dropped.try_recv(), Ok(3));
        assert_eq!(clients.prune(), [3], "its client is detached");
    }

    #[test]
    fn a_role_goes_to_its_client_alone_in_order_and_past_an_overflow() {
        let mut clients = Clients::new(4);
        let told = clients.attach(1);
        let other = clients.attach(2);
        assert!(clients.role(1, LeadRole::Leader, 1));
        clients.output(0, b"abc");
        clients.output(3, b"de");
        assert!(clients.role(1, LeadRole::Follower, 2));
        assert_eq!(
            drain(&told),
            [
                ClientEvent::Role {
                    role: LeadRole::Leader,
                    generation: 1
                },
                ClientEvent::Overflowed { offset: 0 },
                ClientEvent::Role {
                    role: LeadRole::Follower,
                    generation: 2
                },
            ]
        );
        assert!(
            drain(&other)
                .iter()
                .all(|event| !matches!(event, ClientEvent::Role { .. }))
        );
        clients.exit(&ExitStatus::Unknown);
        assert!(
            !clients.role(1, LeadRole::Unowned, 3),
            "no client is left after the exit"
        );
    }

    #[test]
    fn reply_ownership_outlasts_an_overflow_and_a_delegated_request_does_not() {
        let mut clients = Clients::new(4);
        let owner = clients.attach(1);
        let request = DelegatedRequest::ClipboardRead {
            selection: "c".to_owned(),
            list_only: false,
        };
        assert!(clients.reply_owner(1, true, 3));
        assert_eq!(clients.snapshot(1, 0, SIZE, Vec::new()), Some(1));
        assert_eq!(clients.delegate(1, 7, request.clone()), Some(1), "asked in its epoch");
        assert_eq!(clients.delegate(2, 8, request.clone()), None, "no such client");
        clients.output(0, b"abc");
        clients.output(3, b"de");
        assert_eq!(
            clients.delegate(1, 9, request.clone()),
            None,
            "behind: it would not get it"
        );
        assert_eq!(
            drain(&owner),
            [
                ClientEvent::ReplyOwner {
                    owner: true,
                    generation: 3
                },
                ClientEvent::Overflowed { offset: 0 },
            ]
        );
        assert_eq!(clients.snapshot(1, 5, SIZE, Vec::new()), Some(2));
        assert_eq!(clients.delegate(1, 10, request), Some(2), "asked in its new epoch");
        assert!(!clients.reply_owner(2, false, 4));
    }

    #[test]
    fn a_waiting_client_wakes_for_output() {
        let mut clients = Clients::new(DEFAULT_QUEUE_LIMIT);
        let client = clients.attach(1);
        let waiter = std::thread::spawn(move || client.recv_timeout(Duration::from_secs(30)));
        std::thread::sleep(Duration::from_millis(50));
        clients.output(0, b"late");
        assert_eq!(
            waiter.join().unwrap(),
            Ok(ClientEvent::Output {
                offset: 0,
                data: b"late".to_vec()
            })
        );
    }
}
