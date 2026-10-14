//! Work handed to the viewer presenting a session, on the origin: asks (upstream's `AppStore+RemoteAsk.swift` and
//! `ControlServer+Ask.swift`'s `presentAskRemotely`, `takeBackRemoteAsk`, `receivePresenterFrame`) and program
//! overlays (`AppStore+RemoteOverlay.swift`, `ControlServer+OverlayJobs.swift`'s `openRemoteOverlay`).
//!
//! An ask or overlay opened on a session whose panes follow a remote lead goes to its presenter: the ask keeps the
//! session's slot and its result here, marked remote, so every cancel and teardown path that ends an ask ends it and
//! tells the presenter; the overlay reserves its slot for a job the presenter's helper runs.

use agterm_proto::{ControlAskOutcome, ControlAskResult, ControlResult};
use uuid::Uuid;

use super::frames::{
    Body, MAX_FRAME_BYTES, PresentationAsk, PresentationAskRef, PresentationOverlay, PresentationOverlayChange,
    PresentationPane,
};
use crate::Core;
use crate::commands::Outcome;
use crate::error::ControlError;
use crate::modal::AskOwner;
use crate::model::{
    AskAnchor, AskStyle, EndedAsk, OverlayPane, OverlayProgram, PendingAsk, RemoteOverlayOutcome, RemoteOverlaySlot,
    Session,
};
use crate::overlay_capture::OverlayLaunchContext;
use crate::overlay_jobs::{OverlayJobFrame, OverlayJobOutcome, OverlayJobState};
use crate::pane_env::{self, SessionPane};
use crate::zmx::lead::LeadRole;

/// The reason an ask carries when its presenter was lost and this instance could not show it.
pub const PRESENTATION_LOST: &str = "presentation-lost";

/// What [`open_remote_overlay`] did with a request (upstream's `RemoteOverlayOpen`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RemoteOverlayOpen {
    /// The caller opens the overlay here.
    NotPresented,
    /// The slot holds an overlay, local or remote.
    SlotTaken,
    /// The session has no pane in the requested role.
    PaneMissing,
    /// The launch context is larger than the helper reads in one frame.
    TooLarge,
    Opened,
}

impl Core {
    /// Whether every pane `pane_identity` names, or each of the session's panes when `None`, follows a remote lead
    /// (upstream's `Session.followsRemotely`).
    fn follows_remotely(&self, session: &Session, pane_identity: Option<Uuid>) -> bool {
        let panes: Vec<Uuid> = match pane_identity {
            Some(identity) => vec![identity],
            None => std::iter::once(session.pane_identity)
                .chain(session.split_pane_identity)
                .collect(),
        };
        panes
            .iter()
            .all(|pane| self.zmx.leads.role(*pane) == Some(LeadRole::Follower))
    }

    /// Tells each ended ask's presenter to take its dialog down; an ask that never left this instance has none.
    pub(crate) fn dismiss_remote_asks(&mut self, ended: &[EndedAsk]) {
        for ask in ended {
            if let Some(owner) = ask.remote_owner {
                let dismiss = Body::AskDismiss(PresentationAskRef {
                    id: ask.id.clone(),
                    owner,
                });
                self.remote.hub.send_to_presenter(dismiss, ask.session);
            }
        }
    }

    /// Takes back the ask a viewer was presenting for `session`, after the viewer went away or refused it (upstream's
    /// `takeBackRemoteAsk`). A terminal ask is drawn here from now on; a GUI one moves into its window's slot when its
    /// target is shown and the slot is free, and otherwise ends cancelled with `presentation-lost`.
    pub(crate) fn take_back_remote_ask(&mut self, session: Uuid) {
        let Some(window) = self.library().window_holding_session(session) else {
            return;
        };
        let Some(found) = self.library().store(window).and_then(|store| store.session(session)) else {
            return;
        };
        let Some(pending) = found.ask().filter(|pending| pending.remote_owner.is_some()) else {
            return;
        };
        let id = pending.ask.id.clone();
        if pending.ask.style != AskStyle::Gui {
            if let Some(store) = self.library_mut().store_mut(window) {
                store.take_session_ask_back(session);
            }
            return;
        }
        let local = PendingAsk {
            anchor: Some(AskAnchor {
                session,
                pane: found.ask_target_pane(),
                pane_identity: pending.pane_identity,
            }),
            ..pending.ask.clone()
        };
        let fits = self.gui_ask_fits(found, window);
        let moved = fits && self.modals.open_window_ask(window, local).is_ok();
        let Some(store) = self.library_mut().store_mut(window) else {
            return;
        };
        if moved {
            store.release_session_ask(session);
            self.modals.reassign_ask(&id, AskOwner::Window(window));
        } else {
            let lost = ControlAskResult {
                reason: Some(PRESENTATION_LOST.to_owned()),
                ..ControlAskResult::outcome(ControlAskOutcome::Cancelled)
            };
            store.resolve_session_ask(session, &id, lost);
        }
    }

    /// Whether a GUI ask anchored to `session` could be shown now, the rule `ask.open` applies: the session selected
    /// with no zoom and no dashboard over it, and a pane anchor laid out (upstream's `guiAskFits`).
    fn gui_ask_fits(&self, session: &Session, window: Uuid) -> bool {
        let shown = self
            .library()
            .store(window)
            .is_some_and(|store| store.selected_session_id() == Some(session.id()))
            && self.zoom_target(window).is_none()
            && self.dashboard(window).is_none();
        if !shown {
            return false;
        }
        if session.ask().is_some_and(|pending| pending.pane_identity.is_none()) {
            return true;
        }
        session
            .ask_target_pane()
            .is_some_and(|pane| session.renders_overlay_pane(pane))
    }

    /// The viewer presenting `session` is gone: its ask comes back, and every overlay it held goes with it; a job
    /// still unclaimed is cancelled and can no longer launch (upstream's `onPresenterLost`).
    pub(crate) fn presenter_lost(&mut self, session: Uuid) {
        self.take_back_remote_ask(session);
        let jobs = self
            .library_mut()
            .store_for_session_mut(session)
            .map(|store| store.remote_overlay_presenter_lost(session))
            .unwrap_or_default();
        for job in jobs {
            let unclaimed = self
                .jobs
                .jobs
                .job(&job)
                .is_some_and(|found| matches!(found.state, OverlayJobState::Unclaimed { .. }));
            if unclaimed {
                self.cancel_overlay_job(&job);
            }
        }
    }

    /// Applies what `session`'s presenter sent about work it was handed (upstream's `receivePresenterFrame`).
    pub(crate) fn receive_presenter_frame(&mut self, session: Uuid, body: Body) {
        match body {
            Body::AskResolve(answer) => {
                if let Some(store) = self.library_mut().store_for_session_mut(session) {
                    store.resolve_remote_ask(session, &answer.id, answer.owner, answer.button.as_deref());
                }
            }
            Body::AskRejected(reference) => {
                let presenting = self
                    .library()
                    .store_for_session(session)
                    .is_some_and(|store| store.is_presenting_remotely(session, &reference.id, reference.owner));
                if presenting {
                    self.take_back_remote_ask(session);
                }
            }
            Body::OverlayRejected(change) => {
                let held = self
                    .library_mut()
                    .store_for_session_mut(session)
                    .is_some_and(|store| store.reject_remote_overlay(session, &change.job));
                if held {
                    self.jobs.jobs.finish(&change.job, OverlayJobOutcome::LaunchFailed);
                }
            }
            Body::OverlayClosed(change) => {
                if let Some(store) = self.library_mut().store_for_session_mut(session) {
                    store.remote_overlay_surface_closed(session, &change.job);
                }
            }
            _ => {}
        }
        self.sync_modals();
    }

    /// The remote overlays the model closed are taken down on their presenter and their jobs cancelled, and each job
    /// that ended records its outcome into its slot (upstream's `closeRemoteOverlay` hub half and `onFinished`).
    pub(crate) fn settle_remote_overlays(&mut self) {
        for close in self.library_mut().take_remote_overlay_closes() {
            let body = Body::OverlayClose(PresentationOverlayChange::job(close.job.clone()));
            self.remote.hub.send_to_presenter(body, close.session);
            self.cancel_overlay_job(&close.job);
        }
        for job in self.take_finished_overlay_jobs() {
            let OverlayJobState::Finished(outcome) = job.state else {
                continue;
            };
            let outcome = match outcome {
                OverlayJobOutcome::Exited(code) => RemoteOverlayOutcome::Exited(code),
                other => RemoteOverlayOutcome::Failed(other.failure_name().unwrap_or("unknown").to_owned()),
            };
            if let Some(store) = self.library_mut().store_for_session_mut(job.session) {
                store.finish_remote_overlay(job.session, &job.id, &outcome);
            }
        }
    }

    /// Asks the presenter to resize session `session`'s session-wide remote overlay and records the size (upstream's
    /// `resizeRemoteOverlay`). `None` when no viewer holds the slot; false when the stream the job was handed to is
    /// gone, even if another presents now.
    pub(crate) fn resize_remote_overlay(&mut self, session: Uuid, size_percent: Option<i64>) -> Option<bool> {
        let slot = self
            .library()
            .store_for_session(session)
            .and_then(|store| store.session(session))
            .and_then(|found| found.remote_overlays().slot(None).cloned())?;
        let hub = &self.remote.hub;
        if !hub.has_presenter(session) || hub.presenter_generation(session) != slot.owner {
            return Some(false);
        }
        let size = size_percent.map(|percent| percent.clamp(1, 100));
        if let Some(store) = self.library_mut().store_for_session_mut(session) {
            store.resize_remote_overlay(session, &slot.job, size);
        }
        let body = Body::OverlayResize(PresentationOverlayChange {
            job: slot.job,
            size_percent: size,
        });
        Some(self.remote.hub.send_to_presenter(body, session))
    }

    /// Whether a viewer presents `session`.
    pub(crate) fn has_presenter(&self, session: Uuid) -> bool {
        self.remote.hub.has_presenter(session)
    }
}

/// The placement of an ask handed over: the pane only has to exist, since nothing is drawn here and the viewer refuses
/// one it cannot show.
pub(crate) struct AskPlacement<'a> {
    pub pane: Option<OverlayPane>,
    pub pane_id: Option<&'a str>,
    pub invalid_pane: &'a str,
}

/// Hands a session-associated ask to the viewer presenting the session (upstream's `presentAskRemotely`), or returns
/// `None` for the caller to handle it here. `follow` raises nothing, as nothing is shown here.
pub(crate) fn present_ask_remotely(
    core: &mut Core,
    ask: &PendingAsk,
    window: Uuid,
    session: Uuid,
    placement: &AskPlacement<'_>,
) -> Option<Outcome> {
    if !core.remote.hub.has_presenter(session) {
        return None;
    }
    let found = core.library().store(window).and_then(|store| store.session(session))?;
    let resolved = match crate::commands::hud::resolve_pane_placement(
        found,
        placement.pane,
        placement.pane_id,
        false,
        placement.invalid_pane,
    ) {
        Ok(resolved) => resolved,
        Err(error) => return Some(Err(error)),
    };
    let identity = resolved.map(|(identity, _)| identity);
    if !core.follows_remotely(found, identity) {
        return None;
    }
    let owner = core.remote.hub.presenter_generation(session);
    let store = core.library_mut().store_mut(window)?;
    if !store.open_session_ask_owned(session, ask.clone(), identity, Some(owner)) {
        return Some(Err(ControlError::rejected("ask already pending")));
    }
    core.modals
        .register_ask(ask.id.clone(), AskOwner::Session { session, window });
    let request = PresentationAsk::new(ask, identity.map(PresentationPane::Identity), owner);
    core.remote.hub.send_to_presenter(Body::AskRequest(request), session);
    Some(Ok(ControlResult {
        id: Some(ask.id.clone()),
        pane: resolved.map(|(_, pane)| pane.wire_name().to_owned()),
        ..ControlResult::default()
    }))
}

/// A program overlay to hand over: what `session.overlay.open` asked for.
pub(crate) struct RemoteProgram<'a> {
    pub pane: Option<OverlayPane>,
    pub program: &'a OverlayProgram,
    pub follow: bool,
}

/// Hands an overlay to the viewer presenting `session` (upstream's `openRemoteOverlay`, both halves): registers the
/// job, reserves the slot so no second overlay opens there, and asks the presenter to show it. Nothing is mounted
/// here, so the session stays uncovered. The launch context is built as a local overlay's would be.
pub(crate) fn open_remote_overlay(
    core: &mut Core,
    window: Uuid,
    session: Uuid,
    request: &RemoteProgram<'_>,
) -> RemoteOverlayOpen {
    let pane = request.pane;
    if !core.remote.hub.has_presenter(session) {
        return RemoteOverlayOpen::NotPresented;
    }
    let Some(found) = core.library().store(window).and_then(|store| store.session(session)) else {
        return RemoteOverlayOpen::NotPresented;
    };
    if found.remote_overlays().slot(pane).is_some() || found.local_overlay_holds(pane) {
        return RemoteOverlayOpen::SlotTaken;
    }
    let identity = match pane {
        None => None,
        Some(OverlayPane::Left) => Some(found.pane_identity),
        Some(OverlayPane::Right) => match found.split_pane_identity {
            Some(identity) => Some(identity),
            None => return RemoteOverlayOpen::PaneMissing,
        },
    };
    if !core.follows_remotely(found, identity) {
        return RemoteOverlayOpen::NotPresented;
    }
    let context = launch_context(core, window, found, request.program);
    let fits = OverlayJobFrame::Context {
        context: context.clone(),
    }
    .line()
    .is_ok_and(|line| line.len() <= MAX_FRAME_BYTES);
    if !fits {
        return RemoteOverlayOpen::TooLarge;
    }
    let hud = found.overlay.hud_active();
    let owner = core.remote.hub.presenter_generation(session);
    let Some(store) = core.library_mut().store_mut(window) else {
        return RemoteOverlayOpen::NotPresented;
    };
    // a HUD yields the session-wide slot to a program, as it does to a local one
    if pane.is_none() && hud {
        store.close_hud(session);
    }
    let job = core.jobs.jobs.register(session, pane, owner, context);
    core.arm_job_launch_expiry();
    let size = if pane.is_none() {
        request.program.size_percent.map(|percent| percent.clamp(1, 100))
    } else {
        None
    };
    if let Some(store) = core.library_mut().store_mut(window) {
        store.reserve_remote_overlay(
            session,
            RemoteOverlaySlot {
                job: job.clone(),
                pane,
                owner,
                size_percent: size,
                wait: request.program.wait,
                ended: false,
            },
        );
    }
    let overlay = PresentationOverlay {
        job,
        pane: identity.map(PresentationPane::Identity),
        size_percent: size,
        background_color: request.program.background_color.clone(),
        follow: request.follow,
        wait: request.program.wait,
    };
    core.remote
        .hub
        .send_to_presenter(Body::OverlayRequest(overlay), session);
    RemoteOverlayOpen::Opened
}

/// What the program starts with on this instance, the same identities a local overlay gets.
fn launch_context(core: &Core, window: Uuid, session: &Session, program: &OverlayProgram) -> OverlayLaunchContext {
    let workspace = core
        .library()
        .store(window)
        .and_then(|store| store.workspace_for_session(session.id()))
        .map(crate::model::Workspace::id);
    let socket = core.child_socket();
    let environment = pane_env::session(&SessionPane {
        session: session.id(),
        window: Some(window),
        workspace,
        socket_path: &socket,
        program_version: &core.version().app.version,
        pane: None,
        pane_token: None,
    });
    let home = core.library().home();
    let cwd = program
        .cwd
        .clone()
        .unwrap_or_else(|| session.local_working_directory(session.effective_cwd(), home));
    OverlayLaunchContext::new(&program.command, &cwd, environment)
}
