//! The attach role: a connection that attaches to one session, creating it when asked, and carries its stream both
//! ways until the client detaches or goes, or the session ends.
//!
//! herdr: `src/server/client_transport.rs` (a reader and a writer per connection, the writer draining the client's
//! queue so the pane never waits for the socket) and `src/server/headless/bootstrap.rs` (`attach_terminal_client`: the
//! session found or created by name, the client registered, then its input and resizes) at `HERDR_REV`
//! (`build/pins.env`), licensed under the Apache License 2.0, modified: agterm's raw attach stream
//! ([`crate::protocol::AttachServerMessage`]) in place of herdr's rendered frames, threads in place of tokio tasks,
//! and the in-band resync of a client that fell behind.
//!
//! # The stream
//!
//! The first message is [`AttachClientMessage::Attach`], within [`HANDSHAKE_TIMEOUT`] (on Unix). The session it names
//! is attached to, or created with its spec when it does not exist and the request carries one; otherwise the server
//! answers [`AttachServerMessage::Refused`] and closes. Then a writer thread sends the client's events in order:
//!
//! - each snapshot as `SnapshotBegin { epoch, offset, size }`, chunks of at most [`snapshot::CHUNK_SIZE`] bytes and
//!   `SnapshotEnd`;
//! - the output, size changes, role and reply-ownership changes and the requests delegated to it as they come, the
//!   output contiguous from the snapshot's offset;
//! - on an overflow, the client's resync ([`Pane::resync`]): a new snapshot under the next epoch. A client past its
//!   resync budget, or one no snapshot could be cut for in time, is detached and its connection closed. One that
//!   overflowed when the session ends is sent the session's last snapshot instead, ahead of its exit;
//! - the session's exit, after which the server closes the connection in order ([`endpoint::close`]): the client
//!   reads all that came before, the exit, then the connection's end, however late it reads them (on Windows the
//!   writer waits for that until the client closes or detaches).
//!
//! Meanwhile the connection's own thread reads the client's input, resizes, reply metadata, delegated answers
//! ([`Pane::answer`]) and its terminal's Kitty answers ([`Pane::complete`]), until it detaches or the connection ends;
//! either detaches it and closes the connection.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use super::clients::{ClientEvent, RecvError, Subscription};
use super::pane::{AttachError, Attached, Pane, PaneConfig, ResyncError, SpawnError};
use super::{Connection, HANDSHAKE_TIMEOUT, Shared, set_recv_timeout, snapshot};
use crate::endpoint::{self, Stream};
use crate::protocol::{
    AttachClientMessage, AttachRefusal, AttachRequest, AttachServerMessage, FramingError, PROTOCOL_VERSION, read_frame,
    write_message,
};

/// How long the writer waits for an event before it looks again; it wakes for every event and for the client's
/// detach, so this only bounds an idle wait.
const WRITER_IDLE: Duration = Duration::from_secs(60);

/// How often an attach looks the session up again when it ended, or another client created it, in between.
const OPEN_ATTEMPTS: usize = 3;

/// Why an attach was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Refusal {
    reason: AttachRefusal,
    message: String,
}

impl Refusal {
    fn new(reason: AttachRefusal, message: impl Into<String>) -> Self {
        Self {
            reason,
            message: message.into(),
        }
    }
}

/// An attach connection after its welcome; its operations run as `connection`'s ([`Connection::operate`]).
pub(super) fn serve(
    stream: &Arc<Stream>,
    shared: &Arc<Shared>,
    connection: &Connection<'_>,
) -> Result<(), FramingError> {
    set_recv_timeout(stream, Some(HANDSHAKE_TIMEOUT));
    let first = read_frame(&mut &**stream)?.message::<AttachClientMessage>(PROTOCOL_VERSION)?;
    set_recv_timeout(stream, None);
    let AttachClientMessage::Attach(request) = first else {
        // a client that does not start with its attach is broken: closed without an answer
        return Ok(());
    };
    // an attach decoded before a handoff began is not served: its client reconnects
    let Some(opened) = connection.operate(|| open(shared, &request)) else {
        return Ok(());
    };
    let (pane, attached) = match opened {
        Ok(opened) => opened,
        Err(refusal) => {
            let refused = AttachServerMessage::Refused {
                reason: refusal.reason,
                message: refusal.message,
            };
            return write_message(&mut &**stream, &refused);
        }
    };
    let id = attached.subscription.id();
    let stream = Arc::clone(stream);
    let writer = {
        let (stream, pane) = (Arc::clone(&stream), Arc::clone(&pane));
        thread::Builder::new()
            .name("agterm-session-attach-writer".to_owned())
            .spawn(move || write_events(&stream, &pane, attached.subscription))
    };
    let writer = match writer {
        Ok(writer) => writer,
        Err(err) => {
            let _ = pane.detach(id);
            return Err(FramingError::Io(err));
        }
    };
    let read = read_messages(&stream, &pane, id, connection);
    let _ = pane.detach(id);
    endpoint::disconnect(&stream);
    let _ = writer.join();
    read
}

/// Attach to the session `request` names, creating it when it does not exist and the request carries its spec.
fn open(shared: &Arc<Shared>, request: &AttachRequest) -> Result<(Arc<Pane>, Attached), Refusal> {
    for _ in 0..OPEN_ATTEMPTS {
        if let Some(pane) = shared.panes.get(&request.name) {
            match pane.attach_with(request.size, request.managed.as_ref(), request.metadata.clone()) {
                Ok(attached) => return Ok((pane, attached)),
                // ended meanwhile: as if it did not exist
                Err(AttachError::Ended) => {}
                Err(AttachError::Unavailable(err)) => {
                    return Err(Refusal::new(AttachRefusal::Unavailable, err.to_string()));
                }
            }
        }
        let Some(spec) = &request.create else {
            return Err(gone(&request.name));
        };
        if !shared.session_started() {
            return Err(Refusal::new(
                AttachRefusal::CreateFailed,
                "the agterm-session server is stopping; connect again",
            ));
        }
        let mut config = PaneConfig::new(&request.name, &shared.namespace, spec.clone(), request.size);
        config.managed.clone_from(&request.managed);
        config.metadata.clone_from(&request.metadata);
        config.client_queue_limit = shared.client_queue_limit;
        config.resync_budget = shared.resync_budget;
        match shared.panes.spawn(config) {
            Ok((pane, attached)) => {
                let ending = Arc::clone(shared);
                pane.on_end(Box::new(move || ending.session_ended()));
                return Ok((pane, attached));
            }
            // another client created it in between: attach to that one
            Err(SpawnError::Exists(_)) => shared.session_ended(),
            Err(SpawnError::Io(err)) => {
                shared.session_ended();
                return Err(Refusal::new(
                    AttachRefusal::CreateFailed,
                    format!("the live session {:?} could not be started: {err}", request.name),
                ));
            }
        }
    }
    Err(gone(&request.name))
}

fn gone(name: &str) -> Refusal {
    Refusal::new(AttachRefusal::Gone, format!("no live session named {name:?}"))
}

/// The client's messages until it detaches or the connection ends: its input, resizes and answers to the pane, each
/// as one of `connection`'s operations (none once a handoff began: the connection then ends).
fn read_messages(stream: &Stream, pane: &Pane, id: u64, connection: &Connection<'_>) -> Result<(), FramingError> {
    loop {
        let frame = match read_frame(&mut &*stream) {
            Ok(frame) => frame,
            Err(FramingError::Closed) => return Ok(()),
            Err(FramingError::Io(err)) if endpoint::is_connection_closed_error(&err) => return Ok(()),
            Err(err) => return Err(err),
        };
        let message = frame.message::<AttachClientMessage>(PROTOCOL_VERSION)?;
        match connection.operate(|| take_message(pane, id, message)) {
            Some(Ok(true)) => {}
            Some(Ok(false)) | None => return Ok(()),
            Some(Err(err)) => return Err(err),
        }
    }
}

/// One message of the attached client `id`: whether the connection goes on.
fn take_message(pane: &Pane, id: u64, message: AttachClientMessage) -> Result<bool, FramingError> {
    match message {
        AttachClientMessage::Input { data } => {
            if let Err(err) = pane.input(id, &data) {
                tracing::debug!(%err, "a live session's input was dropped");
            }
        }
        AttachClientMessage::Resize { size } => pane.resize_client(id, size),
        AttachClientMessage::Metadata(metadata) => pane.set_metadata(id, metadata),
        AttachClientMessage::DelegatedReply {
            id: request,
            epoch,
            reply,
        } => {
            if !pane.answer(id, request, epoch, reply) {
                tracing::debug!(request, epoch, "a delegated reply nothing waits for was dropped");
            }
        }
        AttachClientMessage::KittyComplete {
            id: barrier,
            epoch,
            data,
        } => {
            if !pane.complete(id, barrier, epoch, &data) {
                tracing::debug!(barrier, epoch, "a Kitty completion nothing waits for was dropped");
            }
        }
        AttachClientMessage::Detach => return Ok(false),
        // one attach per connection
        AttachClientMessage::Attach(_) => {
            return Err(FramingError::Malformed(
                "a second attach on an attached connection".to_owned(),
            ));
        }
    }
    Ok(true)
}

/// Send the client's events until its queue closes, its session's exit is sent or it is dropped; then let go of its
/// subscription.
fn write_events(stream: &Stream, pane: &Pane, subscription: Subscription) {
    write_queue(stream, pane, &subscription);
    drop(subscription);
}

fn write_queue(stream: &Stream, pane: &Pane, subscription: &Subscription) {
    let id = subscription.id();
    loop {
        let event = match subscription.recv_timeout(WRITER_IDLE) {
            Ok(event) => event,
            Err(RecvError::Timeout) => continue,
            Err(RecvError::Closed) => return,
        };
        let sent = match event {
            ClientEvent::Overflowed { offset } => match pane.resync(id) {
                // resynced; or the session's last snapshot and its exit are queued next; or it was detached meanwhile
                Ok(()) | Err(ResyncError::Ended | ResyncError::Detached) => Ok(()),
                Err(err @ (ResyncError::OverBudget | ResyncError::Unavailable(_))) => {
                    tracing::info!(session = %pane.name(), offset, %err, "a live session's client was disconnected");
                    endpoint::disconnect(stream);
                    return;
                }
            },
            ClientEvent::Exit(status) => {
                // nothing follows it: the connection ends once the client has it, which ends the reading too
                let _ = send(stream, &AttachServerMessage::Exit { status });
                endpoint::close(stream);
                return;
            }
            event => send_event(stream, event),
        };
        if sent.is_err() {
            let _ = pane.detach(id);
            return;
        }
    }
}

/// One queued event as its messages.
fn send_event(stream: &Stream, event: ClientEvent) -> Result<(), FramingError> {
    match event {
        ClientEvent::Snapshot {
            epoch,
            offset,
            size,
            data,
        } => {
            send(stream, &AttachServerMessage::SnapshotBegin { epoch, offset, size })?;
            for chunk in snapshot::chunks(&data) {
                send(
                    stream,
                    &AttachServerMessage::SnapshotChunk {
                        epoch,
                        data: chunk.to_vec(),
                    },
                )?;
            }
            send(stream, &AttachServerMessage::SnapshotEnd { epoch })
        }
        ClientEvent::Output { offset, data } => send(stream, &AttachServerMessage::Output { offset, data }),
        ClientEvent::Resized { offset, size } => send(stream, &AttachServerMessage::Resized { offset, size }),
        ClientEvent::Role { role, generation } => send(stream, &AttachServerMessage::Role { role, generation }),
        ClientEvent::ReplyOwner { owner, generation } => {
            send(stream, &AttachServerMessage::ReplyOwner { owner, generation })
        }
        ClientEvent::Delegated { id, epoch, request } => {
            send(stream, &AttachServerMessage::DelegatedRequest { id, epoch, request })
        }
        ClientEvent::KittyBarrier { id, epoch } => send(stream, &AttachServerMessage::KittyBarrier { id, epoch }),
        // handled by the writer itself
        ClientEvent::Overflowed { .. } | ClientEvent::Exit(_) => Ok(()),
    }
}

fn send(stream: &Stream, message: &AttachServerMessage) -> Result<(), FramingError> {
    write_message(&mut &*stream, message)
}

impl Shared {
    /// Count a session about to be created; false once the server decided to stop.
    pub(super) fn session_started(&self) -> bool {
        let started = self.lock().session_started();
        self.changed.notify_all();
        started
    }

    /// A session ended, or one counted was not created.
    pub(super) fn session_ended(&self) {
        self.lock().session_ended(Instant::now());
        self.changed.notify_all();
    }
}
