//! Remote sessions: what this instance offers another machine attaching one of its live sessions (the origin side),
//! and the sessions it attached from other machines (the viewer side).
//!
//! - [`tree`]: `live.tree`, the attachable sessions here or, over ssh, on another host;
//! - [`frames`] and [`hub`]: the `live.present` stream that carries a session's presentation (status, context, layout,
//!   HUD, notifications) to its viewers, and decides which one viewer is the session's sole presenter;
//! - [`remotes`]: `remotes.conf`, the hosts offered for an attach;
//! - the viewer side: `live.attach` and File ▸ Attach Remote ([`attach`]), the ssh lines it runs ([`ssh`]), each
//!   attached row's presentation client ([`client`]) and what its frames put on the row ([`viewer`], [`layout`]), and
//!   the panes whose ssh lost the connection ([`reconnect`]);
//! - the presenter's work (upstream's `AppStore+RemoteAsk.swift`, `AppStore+RemoteOverlay.swift` and the server's
//!   `presentAskRemotely`/`takeBackRemoteAsk`/`receivePresenterFrame`): an ask or a program overlay opened on a session
//!   whose panes follow a remote lead goes to its presenter instead of being drawn here.
//!
//! The streams are the frontend's: it hands the core each adopted connection as a [`LineChannel`] and reports
//! the lines read from it and its close, arms the hello deadline and the heartbeat, and the core does the rest (upstream's
//! `ControlServer+Presentation.swift`). State frames are published by comparing each watched session with what its
//! viewers last got, on every commit; a notification and a HUD publication are sent when they happen.

pub(crate) mod attach;
pub mod client;
pub mod frames;
mod handoff;
pub mod hub;
mod layout;
pub mod reconnect;
pub mod remotes;
pub mod ssh;
pub mod tree;
mod viewer;

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agterm_proto::{ControlResult, ControlTree, Request, Response};
use uuid::Uuid;

pub use self::client::{LinkKey, PresentationLink, PresentationTransport};
use self::frames::{
    Body, PresentationFrame, PresentationHud, PresentationLayout, PresentationNotify, PresentationPane,
    PresentationSnapshot, PresentationStatus,
};
pub use self::handoff::PRESENTATION_LOST;
pub(crate) use self::handoff::{
    AskPlacement, RemoteOverlayOpen, RemoteProgram, open_remote_overlay, present_ask_remotely,
};
use self::hub::{CloseReason, HubEvent, PresentationHub, PresentationSink, SubscriberId};
use self::tree::{ProcessRemoteRunner, RemoteCommandResult, RemoteCommandRunner, RemoteWindowProjection};
pub(crate) use self::viewer::row_changes;
use crate::Core;
use crate::commands::Outcome;
pub use crate::commands::overlay::LineChannel;
use crate::dispatch::Dispatched;
use crate::error::ControlError;
use crate::model::{AgentStatus, Session, StatusPane};
use crate::pane_env::uuid_string;
use crate::resolve::{Resolver, trimmed};

/// A viewer silent for longer than this is dropped (upstream's hub `staleTimeout`).
pub const STALE_TIMEOUT: Duration = Duration::from_secs(30);
/// How often the frontend runs [`Core::presentation_heartbeat`] while a stream is up (upstream's
/// `presentationHeartbeatSeconds`).
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
/// How long a stream may stay without a hello before it is closed (upstream's `presentationHelloDeadline`).
pub const HELLO_DEADLINE: Duration = Duration::from_secs(10);
/// Lines a viewer may have waiting for the main loop; the reader stops reading past this, so a fast or faulty peer
/// backs up into its own socket instead of this app's memory (upstream's `presentationInboundLimit`).
pub const INBOUND_LIMIT: usize = 64;
/// How long a stream's blocked write may take before the peer counts as stalled.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// How often a waiting `live.tree HOST` checks for the far side's answer.
const TREE_POLL: Duration = Duration::from_millis(50);

const PRESENT_REQUIRES_SESSION: &str = "live.present requires a session";
const INVALID_SESSION: &str = "invalid session";
const NOT_LIVE: &str = "session is not live-backed, so nothing can be attached to it";

/// The hub's sink over a channel: a frame that cannot be encoded counts as not taken, as reporting it taken would
/// advance the revision over a frame the viewer never got.
struct ChannelSink(Rc<dyn LineChannel>);

impl PresentationSink for ChannelSink {
    fn offer(&mut self, frame: &PresentationFrame) -> bool {
        match frames::encode(frame) {
            Ok(mut line) => {
                line.pop();
                self.0.send(line)
            }
            Err(error) => {
                tracing::error!(%error, "closing a presentation stream on an unsendable frame");
                false
            }
        }
    }

    fn close(&mut self, reason: CloseReason) {
        tracing::info!(?reason, "closing a presentation stream");
        self.0.shutdown();
    }
}

/// One viewer's stream: subscribed once its hello arrived.
struct Stream {
    session: Uuid,
    channel: Rc<dyn LineChannel>,
    subscriber: Option<SubscriberId>,
}

/// What a watched session's viewers last got of its replaceable state.
#[derive(Debug, Default)]
struct Published {
    status: Option<PresentationStatus>,
    context: Option<String>,
}

/// A session's HUD as published to viewers (upstream's `hudPublishGeneration`, `hudExpiresAt` and `onHudWithdrawn`).
#[derive(Debug, Clone, Copy)]
struct HudPublication {
    /// Counts the publications; kept across a withdrawal, so a later panel's generation still grows.
    generation: i64,
    /// When the origin hides the panel; `None` for a persistent one.
    expires_at: Option<Instant>,
    /// Whether the panel is still up; a withdrawal is published once.
    live: bool,
}

/// The presentation streams and the hub (upstream's `presentationHub`, `presentationStreams`).
pub(crate) struct PresentationRuntime {
    pub(crate) hub: PresentationHub,
    streams: BTreeMap<u64, Stream>,
    next_serial: u64,
    published: HashMap<Uuid, Published>,
    huds: HashMap<Uuid, HudPublication>,
    runner: Arc<dyn RemoteCommandRunner>,
    /// The viewer side: the attached rows' clients and their reconnects.
    pub(crate) viewer: viewer::ViewerRuntime,
    /// `remotes.conf`, the machines File ▸ Attach Remote offers.
    pub(crate) remotes: remotes::RemotesState,
}

impl Default for PresentationRuntime {
    fn default() -> Self {
        Self {
            hub: PresentationHub::new(STALE_TIMEOUT, Box::new(Instant::now)),
            streams: BTreeMap::new(),
            next_serial: 0,
            published: HashMap::new(),
            huds: HashMap::new(),
            runner: Arc::new(ProcessRemoteRunner),
            viewer: viewer::ViewerRuntime::default(),
            remotes: remotes::RemotesState::default(),
        }
    }
}

impl fmt::Debug for PresentationRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PresentationRuntime")
            .field("hub", &self.hub)
            .field("streams", &self.streams.keys().collect::<Vec<_>>())
            .field("huds", &self.huds)
            .field("viewer", &self.viewer)
            .field("remotes", &self.remotes)
            .finish_non_exhaustive()
    }
}

// MARK: - live.present and live.tree

/// `live.present` (upstream's dispatcher arm and `openPresentation`): no `active` default, since a viewer names the
/// session it attached and a guess would stream another. The stream starts once this reply is out.
pub(crate) fn open_presentation(core: &mut Core, target: Option<&str>) -> Outcome {
    let target = trimmed(target).ok_or_else(|| ControlError::rejected(PRESENT_REQUIRES_SESSION))?;
    if !tree::is_plain(target) {
        return Err(ControlError::rejected(INVALID_SESSION));
    }
    let resolved = Resolver::new(core.library()).session(Some(target), None)?;
    let session = core
        .library()
        .store(resolved.window)
        .and_then(|store| store.session(resolved.session))
        .ok_or_else(|| ControlError::rejected("no such session"))?;
    if !crate::projection::all_panes_backed(session) {
        return Err(ControlError::rejected(NOT_LIVE));
    }
    Ok(ControlResult::id(uuid_string(resolved.session)))
}

/// `live.tree [HOST]` (upstream's `remoteTree`): this app's own attachable sessions without a host, another machine's
/// with one, read over ssh off the main thread.
pub(crate) fn begin_tree(core: &mut Core, request: &Request) -> Dispatched {
    let host = request
        .args
        .as_ref()
        .and_then(|args| trimmed(args.host.as_deref()))
        .map(str::to_owned);
    let Some(host) = host else {
        return Dispatched::done(local_tree(core));
    };
    match attach::start_tree_read(core, &host) {
        Ok(answer) => poll_tree(answer, host),
        Err(error) => Dispatched::done(Err(error)),
    }
}

/// Waits for the far side's answer without holding the main loop.
fn poll_tree(answer: viewer::Answer, host: String) -> Dispatched {
    Dispatched::Pending {
        after: TREE_POLL,
        resume: Box::new(move |_core: &mut Core| match viewer::take_answer(&answer) {
            Some(result) => Dispatched::Done(remote_tree_answer(&result, &host)),
            None => poll_tree(answer, host),
        }),
    }
}

/// The answer of a finished remote read ([`attach::read_remote_tree`]).
fn remote_tree_answer(result: &RemoteCommandResult, host: &str) -> Response {
    match attach::read_remote_tree(result, host) {
        Ok(remote) => {
            let result = ControlResult {
                remote: Some(remote),
                ..ControlResult::default()
            };
            serde_json::to_value(result).map_or_else(|_| ControlError::Encode.into(), Response::with_result)
        }
        Err(error) => Response::failure(error),
    }
}

/// This app's own attachable sessions across every open window (upstream's `localAttachableSessions`).
fn local_tree(core: &Core) -> Outcome {
    let inventory = crate::commands::live::control_inventory(core)?;
    let windows: Vec<RemoteWindowProjection> = core
        .library()
        .windows()
        .iter()
        .filter(|info| core.library().is_open(info.id))
        .map(|info| RemoteWindowProjection {
            id: uuid_string(info.id),
            name: info.name.clone(),
            tree: core.tree_of(Some(info.id)),
        })
        .collect();
    let remote = tree::candidates(&windows, &inventory).map_err(|error| ControlError::rejected(error.message()))?;
    Ok(ControlResult {
        remote: Some(remote),
        ..ControlResult::default()
    })
}

// MARK: - presentation state

/// The session's status as it travels, `None` when idle (upstream's `presentationStatus`).
fn presentation_status(session: &Session) -> Option<PresentationStatus> {
    let indicator = &session.agent_indicator;
    if indicator.status == AgentStatus::Idle {
        return None;
    }
    // an unspecified owner is the primary pane, and a swap rewrites it to a role without publishing, so it travels as
    // that pane's identity from the start
    let pane = match indicator.status_pane {
        None | Some(StatusPane::Left) => PresentationPane::Identity(session.pane_identity),
        Some(StatusPane::Right) => {
            PresentationPane::Identity(session.split_pane_identity.unwrap_or(session.pane_identity))
        }
        Some(StatusPane::Scratch) => PresentationPane::Scratch,
    };
    Some(PresentationStatus {
        status: indicator.status,
        blink: indicator.blink,
        color: indicator.color.clone(),
        shape: indicator.shape,
        pane: Some(pane),
        changed_at: session.status_changed_at.map(crate::projection::epoch_seconds),
    })
}

/// The session's pane membership and arrangement (upstream's `presentationLayout`); `None` for a remote session.
fn presentation_layout(session: &Session) -> Option<PresentationLayout> {
    if session.remote_host().is_some() {
        return None;
    }
    let split = session.split.exists;
    let mut panes = vec![session.pane_identity];
    if split && let Some(identity) = session.split_pane_identity {
        panes.push(identity);
    }
    Some(PresentationLayout {
        panes,
        primary: session.pane_identity,
        axis: split.then(|| session.split.axis.wire_name().to_owned()),
        shown: session.split.shown,
    })
}

impl PresentationRuntime {
    /// The published HUD as it travels, its remaining lifetime sampled at `now` (upstream's `presentationHud`).
    fn hud(&self, session: &Session, now: Instant) -> Option<PresentationHud> {
        let publication = self.huds.get(&session.id()).filter(|publication| publication.live)?;
        if !session.overlay.hud_active() {
            return None;
        }
        let spec = session.overlay.effective_hud_spec()?;
        let pane = session.overlay.hud.as_ref()?.pane_identity;
        Some(PresentationHud {
            spec,
            pane: pane.map(PresentationPane::Identity),
            generation: publication.generation,
            remaining: publication
                .expires_at
                .map(|deadline| deadline.saturating_duration_since(now).as_secs_f64()),
        })
    }

    /// The sessions some subscribed viewer watches.
    fn watched(&self) -> Vec<Uuid> {
        let mut sessions: Vec<Uuid> = self
            .streams
            .values()
            .filter(|stream| stream.subscriber.is_some())
            .map(|stream| stream.session)
            .collect();
        sessions.sort_unstable();
        sessions.dedup();
        sessions
    }
}

impl Core {
    /// Installs how `live.tree HOST` runs ssh (tests install a fake).
    pub fn set_remote_runner(&mut self, runner: Arc<dyn RemoteCommandRunner>) {
        self.remote.runner = runner;
    }

    /// The session `id` in an open window.
    fn presented_session(&self, id: Uuid) -> Option<&Session> {
        self.library().store_for_session(id).and_then(|store| store.session(id))
    }

    /// The replaceable state a viewer of `session` starts from; empty for a session gone (upstream's
    /// `presentationSnapshot`).
    fn presentation_snapshot(&self, session: Uuid) -> PresentationSnapshot {
        let Some(found) = self.presented_session(session) else {
            return PresentationSnapshot::default();
        };
        PresentationSnapshot {
            status: presentation_status(found),
            hud: self.remote.hud(found, Instant::now()),
            context: found.context.clone(),
            layout: presentation_layout(found),
        }
    }

    /// The `presenters` read-back of each session in `tree` some viewer watches (upstream's `presentersNode`).
    pub(crate) fn project_presenters(&self, tree: &mut ControlTree) {
        if self.remote.hub.is_empty() {
            return;
        }
        for node in tree
            .workspaces
            .iter_mut()
            .flat_map(|workspace| workspace.sessions.iter_mut())
        {
            let Some(id) = frames::parse_uuid(&node.id) else {
                continue;
            };
            let viewers = self.remote.hub.subscriber_count(id);
            if viewers == 0 {
                continue;
            }
            let presented = self.remote.hub.has_presenter(id);
            let mirrors = if presented { viewers - 1 } else { viewers };
            node.presenters = Some(agterm_proto::tree::ControlPresentersNode {
                mirrors: i64::try_from(mirrors).unwrap_or(i64::MAX),
                presenter: presented.then_some(true),
            });
        }
    }

    // MARK: - the frontend's stream hooks

    /// Takes over a connection whose `live.present` for `session` (its reply's id) was just answered ok (upstream's
    /// `adoptPresentationStream`). Returns the stream's serial, which the frontend's reports carry; the frontend arms
    /// [`HELLO_DEADLINE`] and, while streams are up, [`HEARTBEAT_INTERVAL`].
    pub fn adopt_presentation_stream(&mut self, session: &str, channel: Box<dyn LineChannel>) -> u64 {
        self.remote.next_serial += 1;
        let serial = self.remote.next_serial;
        let channel: Rc<dyn LineChannel> = Rc::from(channel);
        let Some(session) = frames::parse_uuid(session) else {
            channel.shutdown();
            return serial;
        };
        self.remote.streams.insert(
            serial,
            Stream {
                session,
                channel,
                subscriber: None,
            },
        );
        serial
    }

    /// One line from stream `serial` (upstream's `PresentationStream.receive`): the viewer speaks first, its hello
    /// subscribes it, and everything after goes to the hub. A bad frame, a first frame that is not a hello, a session
    /// gone, or a hello with no usable version closes the stream.
    pub fn presentation_line(&mut self, serial: u64, line: &[u8]) {
        let Some(stream) = self.remote.streams.get(&serial) else {
            return;
        };
        let (session, subscriber, channel) = (stream.session, stream.subscriber, Rc::clone(&stream.channel));
        let frame = match frames::decode(line) {
            Ok(frame) => frame,
            Err(error) => {
                tracing::warn!(%error, "closing a presentation stream on a bad frame");
                channel.shutdown();
                return;
            }
        };
        if let Some(subscriber) = subscriber {
            self.remote.hub.receive(frame, subscriber);
            self.run_hub_events();
            return;
        }
        let Body::Hello(hello) = frame.body else {
            tracing::warn!("closing a presentation stream that did not open with hello");
            channel.shutdown();
            return;
        };
        if self.presented_session(session).is_none() {
            tracing::info!("closing a presentation stream whose session is gone");
            channel.shutdown();
            return;
        }
        let snapshot = self.presentation_snapshot(session);
        self.remote.published.entry(session).or_insert_with(|| Published {
            status: snapshot.status.clone(),
            context: snapshot.context.clone(),
        });
        let sink = Box::new(ChannelSink(Rc::clone(&channel)));
        match self.remote.hub.subscribe(session, &hello, sink, move |_| snapshot) {
            Ok(id) => {
                if let Some(stream) = self.remote.streams.get_mut(&serial) {
                    stream.subscriber = Some(id);
                }
            }
            Err(error) => {
                tracing::warn!(?error, "refusing a presentation stream");
                channel.shutdown();
            }
        }
        self.run_hub_events();
    }

    /// Stream `serial` closed (upstream's `PresentationStream.closed`).
    pub fn presentation_closed(&mut self, serial: u64) {
        let Some(stream) = self.remote.streams.remove(&serial) else {
            return;
        };
        if let Some(subscriber) = stream.subscriber {
            self.remote.hub.unsubscribe(subscriber);
        }
        if self.remote.hub.subscriber_count(stream.session) == 0 {
            self.remote.published.remove(&stream.session);
        }
        self.run_hub_events();
    }

    /// Stream `serial`'s hello deadline passed: one that sent none is closed, as reads carry no idle timeout and the
    /// hub's heartbeat only knows subscribers.
    pub fn presentation_hello_deadline(&mut self, serial: u64) {
        if let Some(stream) = self.remote.streams.get(&serial)
            && stream.subscriber.is_none()
        {
            tracing::info!("closing a presentation stream that sent no hello");
            stream.channel.shutdown();
        }
    }

    /// The heartbeat (upstream's `startPresentationHeartbeat` body): streams whose session left every open window are
    /// closed, stale viewers dropped and the rest pinged. Whether any stream is still up, so the frontend keeps its
    /// timer only while one is.
    pub fn presentation_heartbeat(&mut self) -> bool {
        self.drop_orphaned_presentation_streams();
        self.remote.hub.heartbeat();
        self.run_hub_events();
        !self.remote.streams.is_empty()
    }

    /// Whether any presentation stream is up.
    #[must_use]
    pub fn has_presentation_streams(&self) -> bool {
        !self.remote.streams.is_empty()
    }

    /// Closes every presentation stream (the quit path).
    pub fn shutdown_presentation_streams(&mut self) {
        for stream in self.remote.streams.values() {
            stream.channel.shutdown();
        }
    }

    /// Ends the streams of sessions no longer in any open window, so a viewer does not keep the last mirrored state
    /// for good on a stream that still answers pings.
    fn drop_orphaned_presentation_streams(&mut self) {
        let orphaned: Vec<Rc<dyn LineChannel>> = self
            .remote
            .streams
            .values()
            .filter(|stream| self.presented_session(stream.session).is_none())
            .map(|stream| Rc::clone(&stream.channel))
            .collect();
        for channel in orphaned {
            tracing::info!("closing a presentation stream whose session is gone");
            channel.shutdown();
        }
    }

    /// Acts on what the hub queued: a presenter lost takes its work back, a presenter's frame is applied.
    fn run_hub_events(&mut self) {
        loop {
            let events = self.remote.hub.take_events();
            if events.is_empty() {
                return;
            }
            for event in events {
                match event {
                    HubEvent::PresenterLost(session) => self.presenter_lost(session),
                    HubEvent::PresenterFrame(session, body) => self.receive_presenter_frame(session, *body),
                }
            }
        }
    }

    // MARK: - publishing

    /// Publishes a control-origin notification to `session`'s viewers: a live event, never part of a snapshot.
    pub(crate) fn publish_notify(&mut self, session: Uuid, title: String, body: &str) {
        let notify = PresentationNotify {
            title,
            body: body.to_owned(),
            pane: None,
            source: "control".to_owned(),
        };
        self.remote.hub.publish(&Body::Notify(notify), session);
        self.run_hub_events();
    }

    /// Publishes `session`'s HUD once its body is on disk, never before: a refused open or update must not reach a
    /// viewer as a panel this instance is not showing (upstream's `publishHud`). `hide_after` 0 is a persistent panel.
    pub(crate) fn publish_hud(&mut self, session: Uuid, hide_after: f64) {
        let now = Instant::now();
        let expires_at = (hide_after > 0.0)
            .then(|| Duration::try_from_secs_f64(hide_after).ok())
            .flatten()
            .map(|lifetime| now + lifetime);
        let publication = self.remote.huds.entry(session).or_insert(HudPublication {
            generation: 0,
            expires_at: None,
            live: false,
        });
        publication.generation += 1;
        publication.expires_at = expires_at;
        publication.live = true;
        self.send_hud(session, now);
    }

    /// Republishes a published HUD after `overlay.resize` changed its width, keeping its deadline (upstream's
    /// `publishHudResize`).
    pub(crate) fn publish_hud_resize(&mut self, session: Uuid) {
        let Some(publication) = self
            .remote
            .huds
            .get_mut(&session)
            .filter(|publication| publication.live)
        else {
            return;
        };
        publication.generation += 1;
        self.send_hud(session, Instant::now());
    }

    fn send_hud(&mut self, session: Uuid, now: Instant) {
        let hud = self
            .presented_session(session)
            .and_then(|found| self.remote.hud(found, now));
        if let Some(hud) = hud {
            self.remote.hub.publish(&Body::Hud(Some(hud)), session);
            self.run_hub_events();
        }
    }

    /// Brings the viewers in line with the model after a mutation, on every commit: the remote overlays the model
    /// closed and the jobs that ended, a withdrawn HUD, and each watched session's status, context and layout where they
    /// changed since its viewers last got them.
    pub(crate) fn sync_presentation(&mut self) {
        self.settle_remote_overlays();
        self.drop_orphaned_presentation_streams();
        self.withdraw_huds();
        for session in self.remote.watched() {
            self.publish_state(session);
        }
        self.run_hub_events();
    }

    fn withdraw_huds(&mut self) {
        let withdrawn: Vec<Uuid> = self
            .remote
            .huds
            .iter()
            .filter(|(_, publication)| publication.live)
            .map(|(session, _)| *session)
            .filter(|session| {
                self.library()
                    .store_for_session(*session)
                    .and_then(|store| store.session(*session))
                    .is_none_or(|found| !found.overlay.hud_active())
            })
            .collect();
        for session in withdrawn {
            if let Some(publication) = self.remote.huds.get_mut(&session) {
                publication.live = false;
            }
            self.remote.hub.publish(&Body::Hud(None), session);
        }
        // a session gone for good takes its counter with it
        let library = &self.library;
        self.remote
            .huds
            .retain(|session, publication| publication.live || library.window_holding_session(*session).is_some());
    }

    fn publish_state(&mut self, session: Uuid) {
        let Some(found) = self.presented_session(session) else {
            return;
        };
        let (status, context, layout) = (
            presentation_status(found),
            found.context.clone(),
            presentation_layout(found),
        );
        let published = self.remote.published.entry(session).or_default();
        let status_changed = published.status != status;
        let context_changed = published.context != context;
        published.status.clone_from(&status);
        published.context.clone_from(&context);
        if status_changed {
            self.remote.hub.publish(&Body::Status(status), session);
        }
        if context_changed {
            self.remote.hub.publish(&Body::Context(context), session);
        }
        if let Some(layout) = layout {
            self.remote.hub.publish_layout(layout, session);
        }
    }
}
