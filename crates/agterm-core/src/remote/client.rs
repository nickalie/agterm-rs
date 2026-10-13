//! A viewer's presentation stream to one attached session's origin (upstream's `RemotePresentationClient.swift`): it
//! opens the bridge (`ssh HOST agtermctl zmx present SESSION`), applies the snapshot and the deltas behind it, asks for
//! the presenter role the origin offers, answers pings, and reconnects with backoff when the link ends or goes quiet.
//!
//! Event-driven, with no timer or callbacks of its own: the owner passes `now` into every call and runs [`tick`] on
//! its clock, and each call hands back what the owner must apply as [`ClientEffect`]s. The link's lines and its end
//! reach [`PresentationClient::receive`] and [`PresentationClient::link_closed`] tagged with the launch they belong to,
//! so a late report from an earlier launch is dropped.
//!
//! [`tick`]: PresentationClient::tick

use std::time::{Duration, Instant};

use uuid::Uuid;

use super::frames::{
    self, Body, PresentationAsk, PresentationAskRef, PresentationFrame, PresentationHello, PresentationHud,
    PresentationLayout, PresentationMode, PresentationNotify, PresentationOverlay, PresentationOverlayChange,
    PresentationStatus,
};
use super::hub::SUPPORTED_KINDS;
use super::reconnect::retry_delay;
use crate::model::RemoteConnection;

/// The origin pings every ten seconds, so three missed is a dead link and not a quiet one.
pub const STALE_AFTER: Duration = Duration::from_secs(30);

/// One launch of the bridge, as the transport's reports name it: the row, the client (a row's client is recreated
/// when the row comes back, and a late report from the previous one's bridge must not reach the new one) and the
/// client's launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LinkKey {
    pub session: Uuid,
    pub client: u64,
    pub launch: u64,
}

/// One running bridge to an origin (upstream's `RemotePresentationLink`).
pub trait PresentationLink {
    /// Sends one line, its newline included.
    fn send(&self, line: &[u8]);
    /// Ends the bridge; nothing it reports afterwards is read.
    fn stop(&self);
}

/// Launches the bridge process (upstream's `RemotePresentationTransport`). The owner reports each line it reads and its
/// end under `key`, through [`crate::Core::viewer_line`] and [`crate::Core::viewer_closed`].
pub trait PresentationTransport {
    fn open(&self, argv: &[String], key: LinkKey) -> Box<dyn PresentationLink>;
}

/// What the owner does with what arrives (upstream's `RemotePresentationEffects`).
#[derive(Debug, Clone, PartialEq)]
pub enum ClientEffect {
    Status(Option<PresentationStatus>),
    /// The status a snapshot carries: no change on the origin, as one arrives with every reconnect.
    SnapshotStatus(Option<PresentationStatus>),
    Hud(Option<PresentationHud>),
    Notify(PresentationNotify),
    Connection(RemoteConnection),
    Context(Option<String>),
    Layout(PresentationLayout),
    Mode(PresentationMode),
    /// Shows an ask the origin handed over; the owner sends `ask.rejected` when it cannot.
    AskRequest(PresentationAsk),
    AskDismiss(PresentationAskRef),
    /// Shows an overlay the origin handed over; the owner sends `overlay.rejected` when it cannot.
    OverlayRequest(PresentationOverlay),
    OverlayClose(PresentationOverlayChange),
    OverlayResize(PresentationOverlayChange),
    Warn(String),
}

/// Keeps one attached session's presentation stream up (upstream's `RemotePresentationClient`).
pub struct PresentationClient {
    session: Uuid,
    /// Unique among the clients this instance made, so their links' reports cannot be mixed up.
    id: u64,
    argv: Vec<String>,
    presentation_version: Option<i64>,
    link: Option<Box<dyn PresentationLink>>,
    /// Counts launches; a report carries the count it was made under, and one from an earlier launch is dropped.
    launch_count: u64,
    running: bool,
    generation: Option<i64>,
    revision: i64,
    last_frame_at: Option<Instant>,
    retry_at: Option<Instant>,
    failures: u32,
    warned_reason: Option<String>,
    connection: Option<RemoteConnection>,
    /// `None` until reported, so a new client's first report reaches the row whatever an earlier client left on it.
    mode: Option<PresentationMode>,
}

impl std::fmt::Debug for PresentationClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PresentationClient")
            .field("session", &self.session)
            .field("argv", &self.argv)
            .field("launch_count", &self.launch_count)
            .field("running", &self.running)
            .field("connection", &self.connection)
            .finish_non_exhaustive()
    }
}

impl PresentationClient {
    /// A stopped client of row `session`; `id` must be unique among the instance's clients.
    #[must_use]
    pub fn new(session: Uuid, id: u64, argv: Vec<String>, presentation_version: Option<i64>) -> Self {
        Self {
            session,
            id,
            argv,
            presentation_version,
            link: None,
            launch_count: 0,
            running: false,
            generation: None,
            revision: -1,
            last_frame_at: None,
            retry_at: None,
            failures: 0,
            warned_reason: None,
            connection: None,
            mode: None,
        }
    }

    /// The client's unique id, which its links' keys carry.
    #[must_use]
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Opens the stream; an origin that predates the protocol is never launched at all.
    pub fn start(&mut self, transport: &dyn PresentationTransport, now: Instant) -> Vec<ClientEffect> {
        let mut effects = Vec::new();
        if self.running {
            return effects;
        }
        if self.presentation_version.is_none() {
            self.report(RemoteConnection::Unsupported, &mut effects);
            return effects;
        }
        self.running = true;
        self.report_mode(PresentationMode::Mirror, &mut effects);
        self.launch(transport, now, &mut effects);
        effects
    }

    /// Ends the stream for good, until [`PresentationClient::start`] again. Reports nothing: the row is leaving.
    pub fn stop(&mut self) {
        self.running = false;
        self.retry_at = None;
        self.drop_link();
    }

    /// A pending retry is due now with the backoff over; a launch still connecting is not started twice.
    pub fn retry_now(&mut self, now: Instant) {
        self.failures = 0;
        if self.retry_at.is_some() {
            self.retry_at = Some(now);
        }
    }

    /// Sends what this machine answered about work the origin handed over. Dropped with no link: the origin takes that
    /// work back when the stream goes.
    pub fn answer(&self, body: Body) {
        if let Some(link) = self.link.as_deref() {
            self.send(body, link);
        }
    }

    /// Reconnects when a retry is due and drops a link that has gone quiet.
    pub fn tick(&mut self, transport: &dyn PresentationTransport, now: Instant) -> Vec<ClientEffect> {
        let mut effects = Vec::new();
        if !self.running {
            return effects;
        }
        if self.link.is_some()
            && self
                .last_frame_at
                .is_some_and(|at| now.saturating_duration_since(at) > STALE_AFTER)
        {
            self.fail(
                &format!("no frames for {} seconds", STALE_AFTER.as_secs()),
                now,
                &mut effects,
            );
            return effects;
        }
        if self.link.is_none() && self.retry_at.is_some_and(|at| now >= at) {
            self.launch(transport, now, &mut effects);
        }
        effects
    }

    /// One line, without its newline, from the link opened by launch `launch`; one from an earlier launch is dropped.
    pub fn receive(&mut self, launch: u64, line: &[u8], now: Instant) -> Vec<ClientEffect> {
        let mut effects = Vec::new();
        if !self.running || self.link.is_none() || launch != self.launch_count {
            return effects;
        }
        let Ok(frame) = frames::decode(line) else {
            self.fail("bad frame", now, &mut effects);
            return effects;
        };
        if !self.accept(&frame) {
            return effects;
        }
        self.last_frame_at = Some(now);
        self.apply(frame.body, &mut effects);
        effects
    }

    /// The link opened by launch `launch` ended; one from an earlier launch says nothing about this one.
    pub fn link_closed(&mut self, launch: u64, reason: &str, now: Instant) -> Vec<ClientEffect> {
        let mut effects = Vec::new();
        if self.running && self.link.is_some() && launch == self.launch_count {
            self.fail(reason, now, &mut effects);
        }
        effects
    }

    /// Whether `frame` is next in order: the first hello fixes the generation, everything else must match it and
    /// advance the revision, which drops what an earlier connection left in flight.
    fn accept(&mut self, frame: &PresentationFrame) -> bool {
        let Some(generation) = self.generation else {
            if !matches!(frame.body, Body::Hello(_)) {
                return false;
            }
            self.generation = Some(frame.generation);
            self.revision = frame.rev;
            return true;
        };
        if frame.generation != generation || frame.rev <= self.revision {
            return false;
        }
        self.revision = frame.rev;
        true
    }

    fn apply(&mut self, body: Body, effects: &mut Vec<ClientEffect>) {
        match body {
            Body::Snapshot(snapshot) => {
                self.failures = 0;
                self.warned_reason = None;
                self.report(RemoteConnection::Connected, effects);
                if let Some(layout) = snapshot.layout {
                    apply_layout(layout, effects);
                }
                effects.push(ClientEffect::SnapshotStatus(snapshot.status));
                effects.push(ClientEffect::Hud(snapshot.hud));
                effects.push(ClientEffect::Context(snapshot.context));
            }
            Body::Status(status) => effects.push(ClientEffect::Status(status)),
            Body::Context(context) => effects.push(ClientEffect::Context(context)),
            Body::Layout(layout) => apply_layout(layout, effects),
            Body::Hud(hud) => effects.push(ClientEffect::Hud(hud)),
            Body::Notify(notify) => effects.push(ClientEffect::Notify(notify)),
            Body::Ping => self.answer(Body::Ack),
            // an origin that predates the role answers mirror, and nothing is asked of it
            Body::Hello(answer) if answer.mode == PresentationMode::Presenter => self.answer(Body::PresenterAcquire),
            Body::PresenterGranted => self.report_mode(PresentationMode::Presenter, effects),
            Body::PresenterRefused => self.report_mode(PresentationMode::Mirror, effects),
            Body::AskRequest(ask) => effects.push(ClientEffect::AskRequest(ask)),
            Body::AskDismiss(reference) => effects.push(ClientEffect::AskDismiss(reference)),
            Body::OverlayRequest(overlay) => effects.push(ClientEffect::OverlayRequest(overlay)),
            Body::OverlayClose(change) => effects.push(ClientEffect::OverlayClose(change)),
            Body::OverlayResize(change) => effects.push(ClientEffect::OverlayResize(change)),
            Body::Hello(_)
            | Body::Ack
            | Body::PresenterAcquire
            | Body::AskResolve(_)
            | Body::AskRejected(_)
            | Body::OverlayRejected(_)
            | Body::OverlayClosed(_)
            | Body::Unknown(_) => {}
        }
    }

    fn launch(&mut self, transport: &dyn PresentationTransport, now: Instant, effects: &mut Vec<ClientEffect>) {
        self.retry_at = None;
        self.generation = None;
        self.revision = -1;
        self.last_frame_at = Some(now);
        self.report(RemoteConnection::Connecting, effects);
        self.launch_count += 1;
        let key = LinkKey {
            session: self.session,
            client: self.id,
            launch: self.launch_count,
        };
        let link = transport.open(&self.argv, key);
        let hello = PresentationHello {
            version: frames::VERSION,
            kinds: SUPPORTED_KINDS.iter().map(|kind| (*kind).to_owned()).collect(),
            mode: PresentationMode::Presenter,
        };
        self.send(Body::Hello(hello), link.as_ref());
        self.link = Some(link);
    }

    fn fail(&mut self, reason: &str, now: Instant, effects: &mut Vec<ClientEffect>) {
        self.drop_link();
        self.failures += 1;
        if self.warned_reason.as_deref() != Some(reason) {
            self.warned_reason = Some(reason.to_owned());
            effects.push(ClientEffect::Warn(reason.to_owned()));
        }
        self.report(RemoteConnection::Failed(reason.to_owned()), effects);
        // the role goes with the link it was granted to
        self.report_mode(PresentationMode::Mirror, effects);
        self.retry_at = Some(now + retry_delay(self.failures));
    }

    fn drop_link(&mut self) {
        if let Some(link) = self.link.take() {
            link.stop();
        }
    }

    fn send(&self, body: Body, link: &dyn PresentationLink) {
        let frame = PresentationFrame {
            generation: self.generation.unwrap_or(0),
            rev: 0,
            body,
        };
        match frames::encode(&frame) {
            Ok(encoded) => link.send(&encoded),
            Err(error) => tracing::warn!(?error, "a presentation frame could not be encoded"),
        }
    }

    fn report(&mut self, next: RemoteConnection, effects: &mut Vec<ClientEffect>) {
        if self.connection.as_ref() == Some(&next) {
            return;
        }
        self.connection = Some(next.clone());
        effects.push(ClientEffect::Connection(next));
    }

    fn report_mode(&mut self, next: PresentationMode, effects: &mut Vec<ClientEffect>) {
        if self.mode == Some(next) {
            return;
        }
        self.mode = Some(next);
        effects.push(ClientEffect::Mode(next));
    }
}

fn apply_layout(layout: PresentationLayout, effects: &mut Vec<ClientEffect>) {
    if layout.is_valid() {
        effects.push(ClientEffect::Layout(layout));
    } else {
        effects.push(ClientEffect::Warn("invalid layout".to_owned()));
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;
    use crate::model::AgentStatus;
    use crate::remote::frames::{PresentationAskAnswer, PresentationSnapshot};

    #[derive(Default)]
    struct LinkState {
        sent: Vec<PresentationFrame>,
        stopped: bool,
    }

    #[derive(Clone, Default)]
    struct Link(Rc<RefCell<LinkState>>);

    impl PresentationLink for Link {
        fn send(&self, line: &[u8]) {
            let line = line.strip_suffix(b"\n").unwrap_or(line);
            if let Ok(frame) = frames::decode(line) {
                self.0.borrow_mut().sent.push(frame);
            }
        }

        fn stop(&self) {
            self.0.borrow_mut().stopped = true;
        }
    }

    #[derive(Default)]
    struct Transport {
        links: RefCell<Vec<Link>>,
        launches: RefCell<Vec<(Vec<String>, LinkKey)>>,
    }

    impl PresentationTransport for Transport {
        fn open(&self, argv: &[String], key: LinkKey) -> Box<dyn PresentationLink> {
            self.launches.borrow_mut().push((argv.to_vec(), key));
            let link = Link::default();
            self.links.borrow_mut().push(link.clone());
            Box::new(link)
        }
    }

    impl Transport {
        fn link(&self, index: usize) -> Link {
            self.links.borrow()[index].clone()
        }

        fn launched(&self) -> usize {
            self.launches.borrow().len()
        }

        fn last_launch(&self) -> u64 {
            self.launches.borrow().last().map_or(0, |(_, key)| key.launch)
        }
    }

    /// A client, its transport, its clock and everything it asked the owner to apply.
    struct Harness {
        client: PresentationClient,
        transport: Transport,
        now: Instant,
        effects: Vec<ClientEffect>,
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

    fn line(body: Body, generation: i64, rev: i64) -> Vec<u8> {
        let mut line = frames::encode(&PresentationFrame { generation, rev, body }).unwrap();
        line.pop();
        line
    }

    impl Harness {
        fn new(version: Option<i64>) -> Self {
            Self {
                client: PresentationClient::new(
                    Uuid::new_v4(),
                    1,
                    vec!["ssh".to_owned(), "buildbox".to_owned(), "present".to_owned()],
                    version,
                ),
                transport: Transport::default(),
                now: Instant::now(),
                effects: Vec::new(),
            }
        }

        fn started() -> Self {
            let mut harness = Self::new(Some(1));
            harness.start();
            harness
        }

        fn start(&mut self) {
            let effects = self.client.start(&self.transport, self.now);
            self.effects.extend(effects);
        }

        fn tick(&mut self) {
            let effects = self.client.tick(&self.transport, self.now);
            self.effects.extend(effects);
        }

        fn advance(&mut self, seconds: u64) {
            self.now += Duration::from_secs(seconds);
        }

        fn deliver(&mut self, line: &[u8]) {
            let effects = self.client.receive(self.transport.last_launch(), line, self.now);
            self.effects.extend(effects);
        }

        fn close(&mut self, reason: &str) {
            let effects = self.client.link_closed(self.transport.last_launch(), reason, self.now);
            self.effects.extend(effects);
        }

        fn connect(&mut self, snapshot: PresentationSnapshot, generation: i64, mode: PresentationMode) {
            let hello = PresentationHello {
                version: 1,
                kinds: vec!["status".to_owned(), "hud".to_owned(), "notify".to_owned()],
                mode,
            };
            self.deliver(&line(Body::Hello(hello), generation, 0));
            self.deliver(&line(Body::Snapshot(snapshot), generation, 1));
        }

        fn connections(&self) -> Vec<RemoteConnection> {
            self.effects
                .iter()
                .filter_map(|effect| match effect {
                    ClientEffect::Connection(connection) => Some(connection.clone()),
                    _ => None,
                })
                .collect()
        }

        fn modes(&self) -> Vec<PresentationMode> {
            self.effects
                .iter()
                .filter_map(|effect| match effect {
                    ClientEffect::Mode(mode) => Some(*mode),
                    _ => None,
                })
                .collect()
        }

        fn statuses(&self) -> Vec<Option<PresentationStatus>> {
            self.effects
                .iter()
                .filter_map(|effect| match effect {
                    ClientEffect::Status(status) | ClientEffect::SnapshotStatus(status) => Some(status.clone()),
                    _ => None,
                })
                .collect()
        }

        fn warnings(&self) -> usize {
            self.effects
                .iter()
                .filter(|effect| matches!(effect, ClientEffect::Warn(_)))
                .count()
        }

        /// Ticks a second at a time until a new launch, at most `limit` seconds; how long it waited.
        fn wait_for_launch(&mut self, limit: u64) -> u64 {
            let launched = self.transport.launched();
            let mut waited = 0;
            while self.transport.launched() == launched && waited < limit {
                self.advance(1);
                waited += 1;
                self.tick();
            }
            waited
        }
    }

    #[test]
    fn starting_launches_the_bridge_and_opens_with_hello() {
        let harness = Harness::started();
        assert_eq!(harness.transport.launches.borrow()[0].0, ["ssh", "buildbox", "present"]);
        let hello = harness.transport.link(0).0.borrow().sent[0].body.clone();
        assert_eq!(
            hello,
            Body::Hello(PresentationHello {
                version: frames::VERSION,
                kinds: SUPPORTED_KINDS.iter().map(|kind| (*kind).to_owned()).collect(),
                mode: PresentationMode::Presenter,
            })
        );
        assert_eq!(harness.connections(), [RemoteConnection::Connecting]);
    }

    #[test]
    fn an_origin_offering_the_role_is_asked_for_it_and_a_grant_makes_this_machine_the_presenter() {
        let mut harness = Harness::started();
        harness.connect(PresentationSnapshot::default(), 7, PresentationMode::Presenter);
        assert_eq!(
            harness
                .transport
                .link(0)
                .0
                .borrow()
                .sent
                .last()
                .map(|frame| frame.body.clone()),
            Some(Body::PresenterAcquire)
        );
        harness.deliver(&line(Body::PresenterGranted, 7, 2));
        assert_eq!(harness.modes(), [PresentationMode::Mirror, PresentationMode::Presenter]);
    }

    #[test]
    fn an_origin_answering_mirror_is_never_asked_for_the_role() {
        let mut harness = Harness::started();
        harness.connect(PresentationSnapshot::default(), 7, PresentationMode::Mirror);
        let sent: Vec<Body> = harness
            .transport
            .link(0)
            .0
            .borrow()
            .sent
            .iter()
            .map(|frame| frame.body.clone())
            .collect();
        assert!(!sent.contains(&Body::PresenterAcquire));
    }

    #[test]
    fn losing_the_stream_drops_the_role_and_a_reconnect_asks_again() {
        let mut harness = Harness::started();
        harness.connect(PresentationSnapshot::default(), 7, PresentationMode::Presenter);
        harness.deliver(&line(Body::PresenterGranted, 7, 2));
        harness.close("exit 255");
        assert_eq!(
            harness.modes(),
            [
                PresentationMode::Mirror,
                PresentationMode::Presenter,
                PresentationMode::Mirror
            ]
        );
        harness.advance(2);
        harness.tick();
        harness.connect(PresentationSnapshot::default(), 8, PresentationMode::Presenter);
        harness.deliver(&line(Body::PresenterGranted, 8, 2));
        assert_eq!(
            harness
                .transport
                .link(1)
                .0
                .borrow()
                .sent
                .last()
                .map(|frame| frame.body.clone()),
            Some(Body::PresenterAcquire)
        );
        assert_eq!(harness.modes().last(), Some(&PresentationMode::Presenter));
    }

    #[test]
    fn handed_over_work_reaches_the_owner_and_an_answer_goes_out_on_the_link() {
        let mut harness = Harness::started();
        harness.connect(PresentationSnapshot::default(), 7, PresentationMode::Mirror);
        let reference = PresentationAskRef {
            id: "a1".to_owned(),
            owner: 2,
        };
        harness.deliver(&line(Body::AskDismiss(reference.clone()), 7, 2));
        assert!(harness.effects.contains(&ClientEffect::AskDismiss(reference)));
        let answer = Body::AskResolve(PresentationAskAnswer {
            id: "a1".to_owned(),
            owner: 2,
            button: Some("yes".to_owned()),
        });
        harness.client.answer(answer.clone());
        assert_eq!(
            harness
                .transport
                .link(0)
                .0
                .borrow()
                .sent
                .last()
                .map(|frame| frame.body.clone()),
            Some(answer.clone())
        );
        // with the stream down it is dropped
        harness.close("exit 255");
        let sent = harness.transport.link(0).0.borrow().sent.len();
        harness.client.answer(answer);
        assert_eq!(harness.transport.link(0).0.borrow().sent.len(), sent);
    }

    #[test]
    fn the_snapshot_marks_the_stream_connected_and_its_status_is_reported_apart_from_a_delta() {
        let mut harness = Harness::started();
        let snapshot = PresentationSnapshot {
            status: Some(blocked()),
            context: Some("release".to_owned()),
            ..PresentationSnapshot::default()
        };
        harness.connect(snapshot, 7, PresentationMode::Mirror);
        harness.deliver(&line(Body::Status(None), 7, 2));
        assert!(harness.effects.contains(&ClientEffect::SnapshotStatus(Some(blocked()))));
        assert!(
            harness
                .effects
                .contains(&ClientEffect::Context(Some("release".to_owned())))
        );
        assert_eq!(harness.statuses(), [Some(blocked()), None]);
        assert_eq!(
            harness.connections(),
            [RemoteConnection::Connecting, RemoteConnection::Connected]
        );
    }

    #[test]
    fn a_frame_from_another_generation_or_an_old_revision_is_ignored() {
        let mut harness = Harness::started();
        harness.connect(PresentationSnapshot::default(), 7, PresentationMode::Mirror);
        harness.deliver(&line(Body::Status(Some(blocked())), 7, 5));
        harness.deliver(&line(Body::Status(None), 6, 9));
        harness.deliver(&line(Body::Status(None), 7, 5));
        harness.deliver(&line(Body::Status(None), 7, 3));
        assert_eq!(harness.statuses(), [None, Some(blocked())]);
    }

    #[test]
    fn an_unknown_kind_is_skipped_and_a_ping_is_acked() {
        let mut harness = Harness::started();
        harness.connect(PresentationSnapshot::default(), 7, PresentationMode::Mirror);
        harness.deliver(&line(Body::Unknown("future.kind".to_owned()), 7, 2));
        harness.deliver(&line(Body::Ping, 7, 3));
        let link = harness.transport.link(0);
        let state = link.0.borrow();
        assert!(!state.stopped);
        assert_eq!(
            state.sent.last().map(|frame| (&frame.body, frame.generation)),
            Some((&Body::Ack, 7))
        );
    }

    #[test]
    fn an_invalid_layout_is_only_a_warning() {
        let mut harness = Harness::started();
        harness.connect(PresentationSnapshot::default(), 7, PresentationMode::Mirror);
        harness.deliver(&line(Body::Layout(PresentationLayout::invalid()), 7, 2));
        assert!(
            harness
                .effects
                .contains(&ClientEffect::Warn("invalid layout".to_owned()))
        );
        assert!(!harness.transport.link(0).0.borrow().stopped);
    }

    #[test]
    fn an_undecodable_line_drops_the_link_and_reconnects() {
        let mut harness = Harness::started();
        harness.connect(PresentationSnapshot::default(), 7, PresentationMode::Mirror);
        harness.deliver(b"not json");
        assert!(harness.transport.link(0).0.borrow().stopped);
        assert_eq!(
            harness.connections().last(),
            Some(&RemoteConnection::Failed("bad frame".to_owned()))
        );
        assert_eq!(harness.wait_for_launch(10), 1);
    }

    #[test]
    fn retry_now_skips_the_backoff_and_starts_it_over() {
        let mut harness = Harness::started();
        for _ in 0..3 {
            harness.close("exit 255");
            harness.advance(100);
            harness.tick();
        }
        harness.close("exit 255");
        let launched = harness.transport.launched();
        harness.tick();
        assert_eq!(harness.transport.launched(), launched);
        harness.client.retry_now(harness.now);
        harness.tick();
        assert_eq!(harness.transport.launched(), launched + 1);
        harness.close("exit 255");
        assert_eq!(
            harness.wait_for_launch(10),
            1,
            "a retry that failed ramps from one second again"
        );
    }

    #[test]
    fn retry_now_while_a_launch_is_connecting_still_starts_the_backoff_over() {
        let mut harness = Harness::started();
        for _ in 0..4 {
            harness.close("exit 255");
            harness.advance(100);
            harness.tick();
        }
        let launched = harness.transport.launched();
        harness.client.retry_now(harness.now);
        harness.tick();
        assert_eq!(
            harness.transport.launched(),
            launched,
            "the connecting launch is not started twice"
        );
        harness.close("exit 255");
        assert_eq!(harness.wait_for_launch(10), 1);
    }

    #[test]
    fn a_link_that_ends_is_retried_after_a_backoff_that_doubles_to_thirty_seconds_then_five_minutes() {
        let mut harness = Harness::started();
        let mut delays = Vec::new();
        for _ in 0..7 {
            harness.close("exit 255");
            delays.push(harness.wait_for_launch(1000));
        }
        assert_eq!(delays, [1, 2, 4, 8, 16, 30, 30]);
        let mut last = 0;
        for _ in 0..33 {
            harness.close("exit 255");
            last = harness.wait_for_launch(1000);
        }
        assert_eq!(last, 300);
        assert_eq!(harness.transport.launched(), 41, "it never stops");
    }

    #[test]
    fn a_healthy_connection_resets_the_backoff_and_the_warnings() {
        let mut harness = Harness::started();
        for reason in ["exit 255", "exit 255", "exit 1"] {
            harness.close(reason);
            harness.advance(400);
            harness.tick();
        }
        harness.connect(PresentationSnapshot::default(), 7, PresentationMode::Mirror);
        harness.close("exit 255");
        assert_eq!(
            harness.warnings(),
            3,
            "one warning per failure episode or changed reason"
        );
        assert_eq!(harness.wait_for_launch(10), 1, "one second was enough again");
    }

    #[test]
    fn a_quiet_stream_goes_stale_and_any_frame_keeps_it_fresh() {
        let mut harness = Harness::started();
        harness.connect(PresentationSnapshot::default(), 7, PresentationMode::Mirror);
        for rev in 2..8 {
            harness.advance(20);
            harness.deliver(&line(Body::Ping, 7, rev));
            harness.tick();
        }
        assert!(!harness.transport.link(0).0.borrow().stopped);
        harness.advance(29);
        harness.tick();
        assert!(!harness.transport.link(0).0.borrow().stopped);
        harness.advance(2);
        harness.tick();
        assert!(harness.transport.link(0).0.borrow().stopped);
        assert_eq!(
            harness.connections().last(),
            Some(&RemoteConnection::Failed("no frames for 30 seconds".to_owned()))
        );
    }

    #[test]
    fn an_origin_without_the_capability_is_never_launched() {
        let mut harness = Harness::new(None);
        harness.start();
        harness.advance(1000);
        harness.tick();
        assert_eq!(harness.transport.launched(), 0);
        assert_eq!(harness.connections(), [RemoteConnection::Unsupported]);
    }

    #[test]
    fn stopping_ends_the_link_and_nothing_reconnects_until_started_again() {
        let mut harness = Harness::started();
        harness.connect(PresentationSnapshot::default(), 7, PresentationMode::Mirror);
        harness.client.stop();
        harness.close("exit 143");
        harness.advance(1000);
        harness.tick();
        assert!(harness.transport.link(0).0.borrow().stopped);
        assert_eq!(harness.transport.launched(), 1);
        harness.start();
        let snapshot = PresentationSnapshot {
            status: Some(blocked()),
            ..PresentationSnapshot::default()
        };
        harness.connect(snapshot, 9, PresentationMode::Mirror);
        assert_eq!(harness.transport.launched(), 2);
        assert_eq!(harness.statuses().last(), Some(&Some(blocked())));
    }

    #[test]
    fn a_late_report_from_an_earlier_link_does_not_touch_the_current_one() {
        let mut harness = Harness::started();
        harness.connect(PresentationSnapshot::default(), 7, PresentationMode::Mirror);
        harness.close("exit 255");
        harness.advance(1);
        harness.tick();
        // a queued line of the first launch is not taken for the current link's first frame
        let hello = PresentationHello {
            version: 1,
            kinds: vec!["status".to_owned()],
            mode: PresentationMode::Mirror,
        };
        let effects = harness.client.receive(1, &line(Body::Hello(hello), 7, 0), harness.now);
        assert!(effects.is_empty());
        let snapshot = PresentationSnapshot {
            status: Some(blocked()),
            ..PresentationSnapshot::default()
        };
        harness.connect(snapshot, 9, PresentationMode::Mirror);
        let effects = harness.client.link_closed(1, "exit 255", harness.now);
        assert!(effects.is_empty());
        assert_eq!(harness.connections().last(), Some(&RemoteConnection::Connected));
        assert_eq!(harness.statuses().last(), Some(&Some(blocked())));
    }
}
