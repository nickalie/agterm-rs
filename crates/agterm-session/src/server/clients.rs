//! The clients attached to one pane, and the queue of what each is sent: the pane's output, tagged with its offset in
//! the session's output stream, its size changes and its end.
//!
//! herdr: `src/server/client_transport.rs` (`ClientWriterQueue`, the per-client writer queue the PTY side pushes into
//! without waiting for the client's socket) and `src/server/clients.rs` (the registry of a session's clients) at
//! `HERDR_REV` (`build/pins.env`), licensed under the Apache License 2.0, modified. Kept: a queue per client, filled by
//! the pane and drained by the client's own writer, so a slow client never holds up the PTY reader or the other
//! clients. Changed: the queue carries agterm's raw output lane, not herdr's rendered frames, so it is lossless where
//! herdr's render lane keeps only the latest frame: it is bounded in bytes, and output that does not fit is never
//! dropped silently. The queue keeps everything before it and ends with [`ClientEvent::Overflowed`] at the offset
//! where the client's copy of the stream stops, and the server resyncs that client in band or disconnects it.
//!
//! # Order
//!
//! The pane queues under its own lock, the one that also feeds its terminal, so every client sees the same events in
//! the same order, each output contiguous with the one before it (`offset` plus the length of `data` is the next
//! offset), until its queue overflows or the session ends. A managed client's [`ClientEvent::Role`] reports
//! ([`super::lead`]) sit in that order too, its first before anything else it is sent. [`ClientEvent::Exit`] is always
//! queued, after all output, and is the last event of a session.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::protocol::{ExitStatus, GridSize, LeadRole};

/// The output bytes a client's queue holds before it overflows.
pub const DEFAULT_QUEUE_LIMIT: usize = 8 * 1024 * 1024;

/// The most output one [`ClientEvent::Output`] carries once queued reads are merged (well under the wire's frame
/// size, `crate::protocol::MAX_FRAME_SIZE`).
pub const MERGE_LIMIT: usize = 64 * 1024;

/// A client of one pane, unique within the server's life.
pub type ClientId = u64;

/// What a client is sent, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientEvent {
    /// The session's output from `offset` on.
    Output { offset: u64, data: Vec<u8> },
    /// The session's grid changed, in the output at `offset`.
    Resized { offset: u64, size: GridSize },
    /// The client's role at the session changed (sent to a managed client only); `generation` counts the session's
    /// leadership changes.
    Role { role: LeadRole, generation: u32 },
    /// The client's queue was full: output from `offset` on was not queued, and nothing but role changes and the
    /// session's exit is queued after this until the client is resynced.
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
}

impl Queue {
    fn push_output(&mut self, offset: u64, data: &[u8]) {
        if self.closed || self.overflowed || data.is_empty() {
            return;
        }
        // an empty queue takes any read, so a limit below one read cannot overflow a client that keeps up
        if self.bytes > 0 && self.bytes + data.len() > self.limit {
            self.overflowed = true;
            self.events.push_back(ClientEvent::Overflowed { offset });
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

/// The client's end of its queue. Dropping it detaches the client: the pane stops queuing for it.
#[derive(Debug)]
pub struct Subscription {
    id: ClientId,
    slot: Arc<Slot>,
}

impl Subscription {
    #[must_use]
    pub fn id(&self) -> ClientId {
        self.id
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
            queue: Mutex::new(Queue {
                events: VecDeque::new(),
                bytes: 0,
                limit: self.limit,
                overflowed: false,
                closed: false,
            }),
            ready: Condvar::new(),
        });
        self.slots.push((id, Arc::clone(&slot)));
        Subscription { id, slot }
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
        let Some((_, slot)) = self.slots.iter().find(|(client, _)| *client == id) else {
            return false;
        };
        slot.with(|queue| queue.push_role(role, generation));
        true
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
    fn a_full_queue_keeps_what_it_has_and_reports_where_it_stopped() {
        let mut clients = Clients::new(4);
        let slow = clients.attach(1);
        clients.output(0, b"abc");
        clients.output(3, b"de");
        clients.output(5, b"f");
        clients.resized(6, SIZE);
        clients.exit(&ExitStatus::Exited { code: 0, signal: None });
        assert_eq!(
            drain(&slow),
            [
                ClientEvent::Output {
                    offset: 0,
                    data: b"abc".to_vec()
                },
                ClientEvent::Overflowed { offset: 3 },
                ClientEvent::Exit(ExitStatus::Exited { code: 0, signal: None }),
            ]
        );
        assert_eq!(slow.try_recv(), Err(RecvError::Closed));
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
                ClientEvent::Output {
                    offset: 0,
                    data: b"abc".to_vec()
                },
                ClientEvent::Overflowed { offset: 3 },
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
