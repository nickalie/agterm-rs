//! Remote sessions, viewer side (upstream's `ControlServer+RemotePresentation.swift`,
//! `ControlServer+RemoteReconnect.swift`, the app half of `AppStore+RemotePresentation.swift` and the remote half of
//! `Ghostty/PaneLead.swift`): the presentation client of every attached row, what its frames put on the row (status,
//! context, HUD, notifications, the replicas of asks and overlays the origin hands over, the layout), and the panes
//! whose ssh lost the connection waiting to be attached again.
//!
//! The frontend installs the [`PresentationTransport`] that runs each bridge and reports its lines and its end, and
//! runs [`Core::remote_tick`] whenever [`Core::remote_wake`] asks; everything else happens here.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use agterm_proto::EventKind;
use uuid::Uuid;

use super::client::{ClientEffect, LinkKey, PresentationClient, PresentationTransport};
use super::frames::{
    Body, PresentationAsk, PresentationAskAnswer, PresentationAskRef, PresentationHud, PresentationNotify,
    PresentationOverlay, PresentationOverlayChange,
};
use super::reconnect::{ReconnectBook, RemoteLinkNotice};
use super::tree::{RemoteCommandResult, RemoteCommandRunner};
use crate::Core;
use crate::command_restore::shell_quoted_line;
use crate::commands::hud::Placement;
use crate::model::{
    AskStyle, EndedAsk, OverlayPane, OverlayProgram, PendingAsk, RemoteConnection, Session, StatusPane, WindowEvent,
};
use crate::pane_env::uuid_string;
use crate::zmx::lead::LeadAttachment;

/// How long a reconnect probe may take (upstream's `reconnectProbeDeadline`).
const PROBE_DEADLINE: Duration = Duration::from_secs(10);
/// How often the clients and the reconnect book are ticked while anything is up (upstream's one-second remote tick).
const TICK: Duration = Duration::from_secs(1);
/// How often a running probe or attach is polled for its answer.
const POLL: Duration = Duration::from_millis(100);

/// The answer slot of a command run off the main thread.
pub(crate) type Answer = Arc<Mutex<Option<RemoteCommandResult>>>;

/// Runs `argv` on a thread of its own through `runner`; the slot fills when it is done. `None` when no thread starts.
pub(crate) fn run_off_thread(
    runner: &Arc<dyn RemoteCommandRunner>,
    argv: Vec<String>,
    deadline: Duration,
) -> Option<Answer> {
    let answer: Answer = Arc::new(Mutex::new(None));
    let (runner, slot) = (Arc::clone(runner), Arc::clone(&answer));
    std::thread::Builder::new()
        .name("agterm.remote-command".to_owned())
        .spawn(move || {
            let result = runner.run(&argv, deadline);
            *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(result);
        })
        .ok()?;
    Some(answer)
}

/// Takes a finished command's result out of its slot.
pub(crate) fn take_answer(answer: &Answer) -> Option<RemoteCommandResult> {
    answer.lock().unwrap_or_else(PoisonError::into_inner).take()
}

/// An overlay the origin handed over, shown here in an overlay slot (upstream's `OverlayReplica`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReplicaOverlay {
    job: String,
    /// The pane identity a pane overlay covers; `None` for the session-wide slot.
    pane: Option<Uuid>,
    /// The slot generation the replica occupies; a slot holding another one means the replica closed.
    generation: u64,
    /// The job's ssh ended and `--wait` holds the surface.
    ended: bool,
    /// The stream left `connected`: nothing could close the replica later, so it closes when its ssh ends.
    orphaned: bool,
}

/// A reconnect probe in flight.
struct Probe {
    pane: Uuid,
    answer: Answer,
}

/// Every attached row's presentation client and what the viewer side keeps beside them.
#[derive(Default)]
pub(crate) struct ViewerRuntime {
    clients: HashMap<Uuid, PresentationClient>,
    transport: Option<Rc<dyn PresentationTransport>>,
    pub(crate) reconnects: ReconnectBook,
    probes: Vec<Probe>,
    replicas: HashMap<Uuid, Vec<ReplicaOverlay>>,
    /// The File ▸ Attach Remote flows in progress ([`super::attach`]).
    pub(crate) attach_flows: Vec<super::attach::AttachFlow>,
    /// The id of the last client made, so a row's recreated client never reuses an earlier one's link keys.
    last_client: u64,
}

impl std::fmt::Debug for ViewerRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ViewerRuntime")
            .field("clients", &self.clients.keys().collect::<Vec<_>>())
            .field("reconnects", &self.reconnects)
            .field("replicas", &self.replicas)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
impl ViewerRuntime {
    /// Whether no reconnect probe is running.
    pub(crate) fn probes_idle(&self) -> bool {
        self.probes.is_empty()
    }
}

/// The rows a commit's events showed or hid, in order: `remote.opened` is `true`.
pub(crate) fn row_changes(events: &[WindowEvent]) -> Vec<(Uuid, bool)> {
    events
        .iter()
        .filter_map(|event| {
            let shown = match event.event.kind {
                EventKind::RemoteOpened => true,
                EventKind::RemoteClosed => false,
                _ => return None,
            };
            event.event.session.map(|session| (session, shown))
        })
        .collect()
}

impl Core {
    // MARK: - the frontend's side

    /// Installs how a presentation bridge is run (the app's child process; tests a fake).
    pub fn set_presentation_transport(&mut self, transport: Rc<dyn PresentationTransport>) {
        self.remote.viewer.transport = Some(transport);
    }

    /// One line, without its newline, a bridge read; commit afterwards.
    pub fn viewer_line(&mut self, key: LinkKey, line: &[u8]) {
        let Some(client) = self
            .remote
            .viewer
            .clients
            .get_mut(&key.session)
            .filter(|client| client.id() == key.client)
        else {
            return;
        };
        let effects = client.receive(key.launch, line, Instant::now());
        self.apply_client_effects(key.session, effects);
    }

    /// A bridge ended with `reason`; commit afterwards.
    pub fn viewer_closed(&mut self, key: LinkKey, reason: &str) {
        let Some(client) = self
            .remote
            .viewer
            .clients
            .get_mut(&key.session)
            .filter(|client| client.id() == key.client)
        else {
            return;
        };
        let effects = client.link_closed(key.launch, reason, Instant::now());
        self.apply_client_effects(key.session, effects);
    }

    /// The remote tick (upstream's `startRemoteTick` body): every client reconnects or goes stale on its schedule, the
    /// due reconnect probes start and the finished ones attach their pane again, and the attach flows move on. Commit
    /// afterwards.
    pub fn remote_tick(&mut self, now: Instant) {
        let sessions: Vec<Uuid> = self.remote.viewer.clients.keys().copied().collect();
        for session in sessions {
            let Some(transport) = self.remote.viewer.transport.clone() else {
                break;
            };
            let Some(client) = self.remote.viewer.clients.get_mut(&session) else {
                continue;
            };
            let effects = client.tick(transport.as_ref(), now);
            self.apply_client_effects(session, effects);
        }
        self.tick_reconnects(now);
        self.advance_attach_flows();
    }

    /// When the frontend should run [`Core::remote_tick`] next, `None` while nothing remote is up.
    #[must_use]
    pub fn remote_wake(&self) -> Option<Duration> {
        let viewer = &self.remote.viewer;
        if !viewer.probes.is_empty() || !viewer.attach_flows.is_empty() {
            return Some(POLL);
        }
        (!viewer.clients.is_empty() || !viewer.reconnects.is_empty()).then_some(TICK)
    }

    /// Every waiting pane and dropped stream retries now, not on its backoff (upstream's `retryRemoteLinksNow`).
    pub fn retry_remote_links_now(&mut self, now: Instant) {
        self.remote.viewer.reconnects.retry_all_now(now);
        for client in self.remote.viewer.clients.values_mut() {
            client.retry_now(now);
        }
        self.remote_tick(now);
    }

    /// Ends every presentation stream of an attached row (the quit path).
    pub fn shutdown_remote_presentations(&mut self) {
        for client in self.remote.viewer.clients.values_mut() {
            client.stop();
        }
        self.remote.viewer.clients.clear();
    }

    /// Whether the pane carrying `token` waits to be attached again: a key press on it retries rather than reaching
    /// the terminal.
    #[must_use]
    pub fn reconnect_waiting(&self, token: &str) -> bool {
        Uuid::parse_str(token).is_ok_and(|pane| self.remote.viewer.reconnects.waiting(pane))
    }

    /// What ssh said on the last failed probe of the pane carrying `token` (upstream's `RemoteReconnectNote`), never
    /// the outcome of the attach after a probe that answered.
    #[must_use]
    pub fn pane_reconnect_note(&self, token: &str) -> Option<String> {
        let pane = Uuid::parse_str(token).ok()?;
        self.remote.viewer.reconnects.readback(pane)?.reason
    }

    /// The `reconnect` read-back of the pane `pane`.
    pub(crate) fn reconnect_readback(&self, pane: Uuid) -> Option<agterm_proto::tree::ControlReconnect> {
        self.remote.viewer.reconnects.readback(pane)
    }

    /// The terminal of session `session`'s overlay at generation `generation`, over `pane` (`None` for the
    /// session-wide slot), finished its program while `--wait` holds it (upstream's `replicaOverlayHeld`): a replica
    /// cut off from its stream closes now, since nothing could close it later; otherwise it stays for the user. The
    /// session-wide and the pane overlays count their generations apart, so the replica is matched by its slot too; a
    /// pane overlay's pane is found from its generation, since the role the frontend captured at mount names the other
    /// pane after a swap or none after a promotion (as `pane_overlay_exited`). Whether anything changed; commit
    /// afterwards.
    pub fn replica_overlay_held(&mut self, session: Uuid, pane: Option<OverlayPane>, generation: u64) -> bool {
        let identity = match pane {
            Some(_) => match self.library().held_session(session).and_then(|found| {
                found
                    .pane_overlays
                    .pane_of(generation)
                    .and_then(|role| found.overlay_pane_identity(role))
            }) {
                Some(identity) => Some(identity),
                None => return false,
            },
            None => None,
        };
        let Some(replicas) = self.remote.viewer.replicas.get_mut(&session) else {
            return false;
        };
        let Some(replica) = replicas
            .iter_mut()
            .find(|replica| replica.generation == generation && replica.pane == identity)
        else {
            return false;
        };
        if replica.orphaned {
            let job = replica.job.clone();
            self.close_replica_overlay(&job, session);
        } else {
            replica.ended = true;
        }
        true
    }

    // MARK: - row visibility

    /// Starts and stops the clients of the rows a commit showed or hid: a soft close hides a row whose panes live on
    /// for undo, and an undo brings it back without an attach, so the client follows the row (upstream's
    /// `remoteRowVisibilityChanged`).
    pub(crate) fn follow_remote_rows(&mut self, rows: Vec<(Uuid, bool)>) {
        for (session, shown) in rows {
            if shown {
                self.start_remote_presentation(session);
            } else {
                self.release_leaving_row(session);
                self.stop_remote_presentation(session);
            }
        }
    }

    /// A row leaving (soft close, close, window removal) closes every replica overlay its origin handed over, and the
    /// origin hears `overlay.closed` while the stream is still up, so an undo brings none of them back (the replica
    /// half of upstream's `releaseLeavingSession`; its replica ask is cancelled with the session's ask).
    fn release_leaving_row(&mut self, session: Uuid) {
        let jobs: Vec<String> = self
            .remote
            .viewer
            .replicas
            .get(&session)
            .map(|replicas| replicas.iter().map(|replica| replica.job.clone()).collect())
            .unwrap_or_default();
        if jobs.is_empty() {
            return;
        }
        for job in &jobs {
            self.close_replica_overlay(job, session);
        }
        self.sync_replica_overlays();
    }

    /// Opens `session`'s presentation stream (upstream's `startRemotePresentation`); a row not attached, or whose
    /// stream is up, is left alone, and a failure here never touches the terminal attach.
    pub(crate) fn start_remote_presentation(&mut self, session: Uuid) {
        if self.remote.viewer.clients.contains_key(&session) {
            return;
        }
        let Some(found) = self
            .library()
            .store_for_session(session)
            .and_then(|store| store.session(session))
        else {
            return;
        };
        let (Some(host), Some(state)) = (found.remote_host(), found.remote_presentation()) else {
            return;
        };
        let binding = &state.binding;
        let Ok(argv) = super::ssh::present_command(host, &binding.remote_session_id) else {
            tracing::error!(%session, "no presentation stream: the origin's session id cannot be sent");
            return;
        };
        let version = binding.presentation_version;
        self.remote.viewer.last_client += 1;
        let mut client = PresentationClient::new(session, self.remote.viewer.last_client, argv, version);
        let Some(transport) = self.remote.viewer.transport.clone() else {
            tracing::debug!(%session, "no presentation transport installed");
            return;
        };
        let effects = client.start(transport.as_ref(), Instant::now());
        self.remote.viewer.clients.insert(session, client);
        self.apply_client_effects(session, effects);
    }

    pub(crate) fn stop_remote_presentation(&mut self, session: Uuid) {
        if let Some(mut client) = self.remote.viewer.clients.remove(&session) {
            client.stop();
        }
    }

    // MARK: - effects

    fn apply_client_effects(&mut self, session: Uuid, effects: Vec<ClientEffect>) {
        for effect in effects {
            self.apply_client_effect(session, effect);
        }
    }

    fn apply_client_effect(&mut self, session: Uuid, effect: ClientEffect) {
        match effect {
            ClientEffect::Status(status) => {
                if let Some(store) = self.library_mut().store_for_session_mut(session) {
                    store.apply_remote_status(status.as_ref(), session);
                }
            }
            ClientEffect::SnapshotStatus(status) => {
                if let Some(store) = self.library_mut().store_for_session_mut(session) {
                    store.apply_remote_snapshot_status(status.as_ref(), session);
                }
            }
            ClientEffect::Context(context) => {
                if let Some(store) = self.library_mut().store_for_session_mut(session) {
                    store.apply_remote_context(context.as_deref(), session);
                }
            }
            ClientEffect::Mode(mode) => {
                if let Some(store) = self.library_mut().store_for_session_mut(session) {
                    store.set_remote_mode(mode, session);
                }
            }
            ClientEffect::Connection(connection) => self.set_remote_connection(session, connection),
            ClientEffect::Hud(hud) => self.show_remote_hud(hud.as_ref(), session),
            ClientEffect::Notify(notify) => self.mirror_notification(session, &notify),
            ClientEffect::Layout(layout) => self.apply_remote_layout(&layout, session),
            ClientEffect::AskRequest(ask) => {
                if !self.show_replica_ask(&ask, session) {
                    self.answer_origin(
                        session,
                        Body::AskRejected(PresentationAskRef {
                            id: ask.id,
                            owner: ask.owner,
                        }),
                    );
                }
            }
            ClientEffect::AskDismiss(reference) => self.dismiss_replica_ask(&reference, session),
            ClientEffect::OverlayRequest(overlay) => {
                if !self.show_replica_overlay(&overlay, session) {
                    self.answer_origin(
                        session,
                        Body::OverlayRejected(PresentationOverlayChange::job(overlay.job)),
                    );
                }
            }
            ClientEffect::OverlayClose(change) => {
                self.close_replica_overlay(&change.job, session);
            }
            ClientEffect::OverlayResize(change) => self.resize_replica_overlay(&change, session),
            ClientEffect::Warn(reason) => {
                tracing::warn!(%session, %reason, "presentation stream");
            }
        }
    }

    /// Sends what this machine answered to `session`'s origin; dropped with no stream.
    fn answer_origin(&self, session: Uuid, body: Body) {
        if let Some(client) = self.remote.viewer.clients.get(&session) {
            client.answer(body);
        }
    }

    /// Moves the stream's state; leaving `connected` orphans the replica overlays: a held surface closes now, and a
    /// running one keeps its program and closes when its ssh ends, since no later stream adopts it.
    fn set_remote_connection(&mut self, session: Uuid, connection: RemoteConnection) {
        let left = self
            .library_mut()
            .store_for_session_mut(session)
            .is_some_and(|store| store.set_remote_connection(connection, session));
        if !left {
            return;
        }
        let replicas = self.remote.viewer.replicas.get(&session).cloned().unwrap_or_default();
        for replica in replicas {
            if replica.ended {
                self.close_replica_overlay(&replica.job, session);
            } else if let Some(kept) = self
                .remote
                .viewer
                .replicas
                .get_mut(&session)
                .and_then(|replicas| replicas.iter_mut().find(|kept| kept.job == replica.job))
            {
                kept.orphaned = true;
            }
        }
    }

    /// A notification the origin sent, delivered as this machine's own and never relayed onward.
    fn mirror_notification(&mut self, session: Uuid, notify: &PresentationNotify) {
        let Some(window) = self.library().window_holding_session(session) else {
            return;
        };
        let pane = crate::commands::notify::PaneRole::Main;
        self.deliver_notification(window, session, pane, &notify.title, &notify.body);
    }

    /// Shows, updates or removes the HUD mirrored from `session`'s origin (upstream's `showRemoteHud`): only a panel the
    /// bridge opened is ever replaced or closed, and a panel this machine's own program put up is left alone. This
    /// machine counts down what is left of the origin's interval, not the configured one again.
    fn show_remote_hud(&mut self, hud: Option<&PresentationHud>, session: Uuid) {
        let Some(found) = self
            .library()
            .store_for_session(session)
            .and_then(|store| store.session(session))
            .filter(|found| found.remote_presentation().is_some())
        else {
            return;
        };
        let Some(hud) = hud.filter(|hud| hud.remaining != Some(0.0)) else {
            if let Some(store) = self.library_mut().store_for_session_mut(session) {
                store.close_bridged_hud(session);
            }
            return;
        };
        let bridged = found.hud_bridged();
        if !bridged && found.overlay.hud_active() {
            return;
        }
        let mut spec = hud.spec.clone();
        spec.hide_after = hud.remaining;
        let pane = found
            .local_pane(hud.pane.as_ref())
            .filter(|pane| found.renders_overlay_pane(*pane));
        let placement = Placement { pane, pane_id: None };
        let target = uuid_string(session);
        let shown = if bridged {
            crate::commands::hud::update(self, Some(&target), None, &spec, &placement)
        } else {
            crate::commands::hud::open(self, Some(&target), None, spec, &placement)
        };
        match shown {
            Ok(_) => {
                if let Some(store) = self.library_mut().store_for_session_mut(session) {
                    store.mark_hud_bridged(session);
                }
            }
            Err(error) => tracing::info!(%session, %error, "mirrored HUD not shown"),
        }
    }

    // MARK: - replica asks

    /// Shows an ask the origin handed over (upstream's `showReplicaAsk` and `presentReplicaAsk`): over the mapped pane or
    /// the session, a GUI one only on a target shown on screen, as the local rule. False when it cannot be shown,
    /// which the caller reports as a refusal.
    fn show_replica_ask(&mut self, ask: &PresentationAsk, session: Uuid) -> bool {
        let Some(window) = self.library().window_holding_session(session) else {
            return false;
        };
        let Some(store) = self.library().store(window) else {
            return false;
        };
        let Some(found) = store
            .session(session)
            .filter(|found| found.remote_presentation().is_some())
        else {
            return false;
        };
        let mut pane_identity = None;
        if ask.pane.is_some() {
            let Some(role) = found
                .local_pane(ask.pane.as_ref())
                .filter(|role| found.renders_overlay_pane(*role))
            else {
                return false;
            };
            pane_identity = found.overlay_pane_identity(role);
        }
        // a GUI one keeps the local rule of refusing a target not on screen (upstream's `guiTargetShown`): the row
        // selected, and neither a terminal zoom nor the dashboard covering the window
        let shown = store.selected_session_id() == Some(session)
            && self.zoom_target(window).is_none()
            && self.dashboard(window).is_none();
        if ask.style == AskStyle::Gui && !shown {
            return false;
        }
        let pending = PendingAsk {
            id: ask.id.clone(),
            title: ask.title.clone(),
            message: ask.message.clone(),
            buttons: ask.buttons.clone(),
            default_id: ask.default_id.clone(),
            destructive_id: ask.destructive_id.clone(),
            style: ask.style,
            align: ask.align,
            width: ask.width,
            anchor: None,
        };
        self.library_mut()
            .store_mut(window)
            .is_some_and(|store| store.open_replica_ask(session, pending, pane_identity, ask.owner))
    }

    /// Takes a replica down without answering it, once its origin ended the ask (upstream's `dismissReplicaAsk`).
    fn dismiss_replica_ask(&mut self, reference: &PresentationAskRef, session: Uuid) {
        let Some(store) = self.library_mut().store_for_session_mut(session) else {
            return;
        };
        let replica = store
            .session(session)
            .and_then(Session::ask)
            .is_some_and(|pending| pending.replica.is_some() && pending.ask.id == reference.id);
        if replica {
            store.release_session_ask(session);
        }
    }

    /// Sends each ended replica's outcome to its origin (upstream's `onReplicaResolved`): the button id alone, none for
    /// Escape, and a refusal for a cancel here, so the origin takes it back.
    pub(crate) fn answer_replica_asks(&mut self, ended: &[EndedAsk]) {
        use agterm_proto::ControlAskOutcome;
        for ask in ended {
            let Some(owner) = ask.replica else {
                continue;
            };
            let body = match ask.result.result {
                ControlAskOutcome::Answered => Body::AskResolve(PresentationAskAnswer {
                    id: ask.id.clone(),
                    owner,
                    button: ask.result.id.clone(),
                }),
                ControlAskOutcome::Escaped => Body::AskResolve(PresentationAskAnswer {
                    id: ask.id.clone(),
                    owner,
                    button: None,
                }),
                ControlAskOutcome::Cancelled | ControlAskOutcome::Pending => Body::AskRejected(PresentationAskRef {
                    id: ask.id.clone(),
                    owner,
                }),
            };
            self.answer_origin(ask.session, body);
        }
    }

    // MARK: - replica overlays

    /// Shows an overlay the origin handed over (upstream's `showReplicaOverlay` and `presentReplicaOverlay`): a local
    /// overlay running the job's helper on the origin over ssh, in the mapped pane or the session-wide slot. False when
    /// the slot is taken or the pane is not shown here, which the caller reports as a refusal.
    fn show_replica_overlay(&mut self, overlay: &PresentationOverlay, session: Uuid) -> bool {
        let Some(store) = self.library_mut().store_for_session_mut(session) else {
            return false;
        };
        let Some(found) = store
            .session(session)
            .filter(|found| found.remote_presentation().is_some())
        else {
            return false;
        };
        let Some(Ok(argv)) = found
            .remote_host()
            .map(|host| super::ssh::run_job_command(host, &overlay.job))
        else {
            return false;
        };
        let program = OverlayProgram {
            command: shell_quoted_line(&argv),
            wait: overlay.wait,
            size_percent: overlay.size_percent,
            background_color: overlay.background_color.clone(),
            ..OverlayProgram::default()
        };
        let (pane, generation) = if overlay.pane.is_some() {
            let Some(role) = found.local_pane(overlay.pane.as_ref()) else {
                return false;
            };
            let identity = found.overlay_pane_identity(role);
            if store.open_pane_overlay(session, role, program).is_err() {
                return false;
            }
            let generation = store
                .session(session)
                .and_then(|found| found.pane_overlay(role))
                .map(|open| open.generation);
            (identity, generation)
        } else {
            let occupied = found.overlay.active && !found.overlay.hud_active();
            if occupied || !store.open_overlay(session, program) {
                return false;
            }
            (None, store.session(session).map(|found| found.overlay.generation))
        };
        let Some(generation) = generation else {
            return false;
        };
        if overlay.follow {
            store.select_session(Some(session));
        }
        self.remote
            .viewer
            .replicas
            .entry(session)
            .or_default()
            .push(ReplicaOverlay {
                job: overlay.job.clone(),
                pane,
                generation,
                ended: false,
                orphaned: false,
            });
        true
    }

    /// Takes down the overlay showing `job`, once its origin closed it (upstream's `closeReplicaOverlay`).
    fn close_replica_overlay(&mut self, job: &str, session: Uuid) -> bool {
        let Some(replica) = self
            .remote
            .viewer
            .replicas
            .get(&session)
            .and_then(|replicas| replicas.iter().find(|replica| replica.job == job))
            .cloned()
        else {
            return false;
        };
        let Some(found) = self.library_mut().held_session_mut(session) else {
            return false;
        };
        match replica.pane.and_then(|identity| found.pane_role_for_identity(identity)) {
            Some(role) => found.pane_overlays.slot_mut(role).close(),
            None if replica.pane.is_none() && found.overlay.generation == replica.generation => found.overlay.close(),
            None => false,
        }
    }

    /// Resizes the session-wide replica showing the change's job (upstream's `resizeReplicaOverlay`).
    fn resize_replica_overlay(&mut self, change: &PresentationOverlayChange, session: Uuid) {
        let generation = self
            .remote
            .viewer
            .replicas
            .get(&session)
            .and_then(|replicas| {
                replicas
                    .iter()
                    .find(|replica| replica.job == change.job && replica.pane.is_none())
            })
            .map(|replica| replica.generation);
        let Some(store) = self.library_mut().store_for_session_mut(session) else {
            return;
        };
        if generation.is_some() && store.session(session).map(|found| found.overlay.generation) == generation {
            store.resize_overlay(session, change.size_percent);
        }
    }

    /// Tells each origin about the replicas whose surface went here, whatever closed it (upstream's
    /// `onReplicaOverlayClosed`): a replica whose slot no longer holds its generation is gone. Runs on every commit.
    pub(crate) fn sync_replica_overlays(&mut self) {
        let sessions: Vec<Uuid> = self.remote.viewer.replicas.keys().copied().collect();
        for session in sessions {
            let replicas = self.remote.viewer.replicas.remove(&session).unwrap_or_default();
            let (kept, gone): (Vec<ReplicaOverlay>, Vec<ReplicaOverlay>) = replicas
                .into_iter()
                .partition(|replica| self.replica_shown(session, replica));
            for replica in gone {
                self.answer_origin(
                    session,
                    Body::OverlayClosed(PresentationOverlayChange::job(replica.job)),
                );
            }
            if !kept.is_empty() {
                self.remote.viewer.replicas.insert(session, kept);
            }
        }
    }

    /// Whether `replica`'s slot still holds it.
    fn replica_shown(&self, session: Uuid, replica: &ReplicaOverlay) -> bool {
        let library = self.library();
        let Some(found) = library.held_session(session) else {
            return false;
        };
        match replica.pane {
            None => found.overlay.program_active() && found.overlay.generation == replica.generation,
            Some(identity) => found
                .pane_role_for_identity(identity)
                .and_then(|role| found.pane_overlay(role))
                .is_some_and(|open| open.generation == replica.generation),
        }
    }

    // MARK: - reconnecting panes

    /// The attach wrapper reported that ssh lost the connection (upstream's `PaneLead.linkLost` and
    /// `waitToReconnect`): only the pane's current attachment is believed, and only an origin that reported a role
    /// before (or a reconnect still waiting for its first report) covers the fresh attach. Whether it was taken.
    pub(crate) fn remote_link_lost(&mut self, session: Uuid, token: &str, notice: &RemoteLinkNotice) -> bool {
        let Ok(pane) = Uuid::parse_str(token) else {
            return false;
        };
        if self.zmx.leads.nonce(pane) != Some(notice.nonce.as_str()) {
            return false;
        }
        let cover = self.zmx.leads.role(pane).is_some() || self.zmx.leads.reattaching(pane);
        let library = self.library();
        let Some(found) = library.held_session(session) else {
            return false;
        };
        let Some(host) = found.remote_host().map(str::to_owned) else {
            return false;
        };
        if !matches!(
            found.pane_role_for_token(token),
            Some(StatusPane::Left | StatusPane::Right)
        ) {
            return false;
        }
        self.remote
            .viewer
            .reconnects
            .wait(pane, session, &host, cover, Instant::now());
        true
    }

    /// The pane `pane` retries now, its backoff over.
    pub(crate) fn retry_reconnect(&mut self, pane: Uuid, now: Instant) {
        self.remote.viewer.reconnects.retry_now(pane, now);
    }

    /// Runs the due probes and attaches each pane whose host answered (upstream's `tickReconnects`).
    fn tick_reconnects(&mut self, now: Instant) {
        for pane in self.remote.viewer.reconnects.due(now) {
            let argv = self
                .remote
                .viewer
                .reconnects
                .entry(pane)
                .filter(|entry| self.waiting_surface(pane, entry.session))
                .and_then(|entry| super::ssh::probe_command(&entry.host).ok());
            let answer = argv.and_then(|argv| run_off_thread(&self.remote.runner, argv, PROBE_DEADLINE));
            match answer {
                Some(answer) => self.remote.viewer.probes.push(Probe { pane, answer }),
                None => self.remote.viewer.reconnects.cancel(pane),
            }
        }
        let probes = std::mem::take(&mut self.remote.viewer.probes);
        for probe in probes {
            let Some(result) = take_answer(&probe.answer) else {
                self.remote.viewer.probes.push(probe);
                continue;
            };
            self.probe_finished(probe.pane, &result, now);
        }
    }

    fn probe_finished(&mut self, pane: Uuid, result: &RemoteCommandResult, now: Instant) {
        let Some(entry) = self
            .remote
            .viewer
            .reconnects
            .finished(pane, result.status == 0, &result.stderr, now)
        else {
            return;
        };
        if !self.waiting_surface(pane, entry.session) {
            return;
        }
        // a row hidden for undo keeps waiting; finalizing its close lets the next due probe drop it
        let shown = self.library().store_for_session(entry.session).is_some();
        if shown && self.reattach_remote_pane(entry.session, pane, LeadAttachment::new(false), entry.cover) {
            if let Some(store) = self.library_mut().store_for_session_mut(entry.session) {
                store.remote_pane_resumed(pane, entry.session);
            }
            return;
        }
        self.remote
            .viewer
            .reconnects
            .wait(pane, entry.session, &entry.host, entry.cover, now);
    }

    /// Whether the pane `pane` of session `session` still has its terminal, its row in a window or a pending close.
    fn waiting_surface(&self, pane: Uuid, session: Uuid) -> bool {
        let library = self.library();
        library
            .held_session(session)
            .and_then(|found| found.pane_role_for_identity(pane).map(|role| (found, role)))
            .is_some_and(|(found, role)| found.overlay_pane_slot(role).is_present())
    }

    /// Replaces the remote replica `pane` of session `session` with a fresh ssh attach of its daemon under `lead`
    /// (upstream's `PaneReattach.launch` remote half), covered until its first report when `cover`. False when the
    /// binding cannot attach the pane again.
    pub(crate) fn reattach_remote_pane(
        &mut self,
        session: Uuid,
        pane: Uuid,
        lead: LeadAttachment,
        cover: bool,
    ) -> bool {
        // only a row in a window's tree: the frontend installs no replacement in a pending close, and a rotated nonce
        // would then drop every later report of the terminal an undo brings back
        let Some(found) = self
            .library()
            .store_for_session(session)
            .and_then(|store| store.session(session))
        else {
            return false;
        };
        let Some(role) = found.pane_role_for_identity(pane) else {
            return false;
        };
        let Some(token) = found
            .overlay_pane_slot(role)
            .get()
            .map(|surface| surface.pane_token().to_owned())
        else {
            return false;
        };
        let Some(state) = found.remote_presentation() else {
            return false;
        };
        let (Some(origin), Some(daemon)) = (state.binding.origin.as_ref(), state.binding.daemon(pane)) else {
            return false;
        };
        let label = super::ssh::PaneLabel {
            session: &origin.session_name,
            pane: role.wire_name(),
        };
        let Ok(command) = super::ssh::attach_pane_command(&origin.host, &origin.endpoint, daemon, label, Some(&lead))
        else {
            return false;
        };
        self.zmx.leads.begin(pane, lead, cover);
        self.request_pane_replacement(crate::commands::zmx::PaneReplacement {
            session,
            token,
            command,
            env: Vec::new(),
            cwd: None,
            remote: true,
        });
        true
    }

    /// Whether `identity` names a pane of a session attached from another machine.
    pub(crate) fn pane_is_remote(&self, identity: Uuid) -> bool {
        let library = self.library();
        library
            .open_ids()
            .into_iter()
            .filter_map(|window| library.store(window))
            .any(|store| {
                store.sessions().any(|session| {
                    session.remote_host().is_some() && session.pane_role_for_identity(identity).is_some()
                })
            })
    }
}
