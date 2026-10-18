//! What a viewer keeps about a session attached from another machine (upstream's `RemotePresentationState.swift` and
//! the model half of `AppStore+RemotePresentation.swift`): which session it is on the origin, which local pane stands
//! for each of the origin's panes, the presentation stream's state, and what the stream put on the row (a mirrored
//! context, a bridged status, a bridged HUD).
//!
//! Both sides mint their own pane identities, so everything the origin says about a pane names the origin's identity
//! and is routed through the [`RemoteBinding`]. Nothing here is ever persisted: a remote session never is.

use std::collections::{HashMap, HashSet};

use agterm_proto::ControlLiveEndpoint;
use agterm_proto::tree::ControlPresentationNode;
use uuid::Uuid;

use super::{AgentIndicator, AgentStatus, AppStore, OverlayPane, Session, StatusPane, validate_context};
use crate::remote::frames::{PresentationLayout, PresentationMode, PresentationPane, PresentationStatus};

/// What attaching one of the origin's panes again takes (upstream's `RemoteBinding.Origin`): taking the lead back is a
/// fresh attach, and the pane's first command line cannot be reused, since it carries that attachment's nonce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteOrigin {
    pub host: String,
    pub endpoint: ControlLiveEndpoint,
    pub session_name: String,
}

/// Ties a session attached from another machine to the origin's own model (upstream's `RemoteBinding`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteBinding {
    /// The origin's session id, which `live present` names.
    pub remote_session_id: String,
    /// The origin's presentation protocol version, `None` for an origin that predates the stream.
    pub presentation_version: Option<i64>,
    /// `None` for a binding built without one, which can then not be attached again.
    pub origin: Option<RemoteOrigin>,
    local_by_remote: HashMap<Uuid, Uuid>,
    daemons_by_local: HashMap<Uuid, String>,
}

impl RemoteBinding {
    /// `daemons_by_local` is what an attach knows: the daemon each local pane runs `zmx attach` against. A daemon name
    /// encodes the origin's pane identity, so the mapping is decoded once, here.
    #[must_use]
    pub fn new(
        remote_session_id: impl Into<String>,
        daemons_by_local: HashMap<Uuid, String>,
        presentation_version: Option<i64>,
        origin: Option<RemoteOrigin>,
    ) -> Self {
        let local_by_remote = daemons_by_local
            .iter()
            .filter_map(|(local, daemon)| crate::live::pane_identity(daemon).map(|remote| (remote, *local)))
            .collect();
        Self {
            remote_session_id: remote_session_id.into(),
            presentation_version,
            origin,
            local_by_remote,
            daemons_by_local,
        }
    }

    /// The local pane standing for the origin's pane `remote`.
    #[must_use]
    pub fn local_pane(&self, remote: Uuid) -> Option<Uuid> {
        self.local_by_remote.get(&remote).copied()
    }

    /// The daemon the local pane `local` attaches to.
    #[must_use]
    pub fn daemon(&self, local: Uuid) -> Option<&str> {
        self.daemons_by_local.get(&local).map(String::as_str)
    }

    /// The origin's pane the local pane `local` stands for.
    #[must_use]
    pub fn remote_pane(&self, local: Uuid) -> Option<Uuid> {
        self.local_by_remote
            .iter()
            .find(|(_, mapped)| **mapped == local)
            .map(|(remote, _)| *remote)
    }
}

/// The state of a viewer's presentation stream, as read-back reports it (upstream's `RemotePresentationConnection`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteConnection {
    Connecting,
    Connected,
    /// The origin predates the stream; final for the row, as nothing is ever launched.
    Unsupported,
    Failed(String),
}

impl RemoteConnection {
    /// What a remote row tells the user while its stream is not up (upstream's `rowNotice(host:)`); `None` when there
    /// is nothing to say, an origin too old for the stream included, as nothing the user does here changes it.
    #[must_use]
    pub fn row_notice(&self, host: &str) -> Option<String> {
        match self {
            Self::Connected | Self::Unsupported => None,
            Self::Connecting => Some(format!(
                "Connecting to {host} for status, context, notifications, dialogs and overlays"
            )),
            Self::Failed(reason) => Some(format!(
                "Lost the connection to {host} ({reason}), retrying. Close and reattach the session to retry now."
            )),
        }
    }
}

/// Everything a viewer keeps about one attached session's presentation (upstream's `RemotePresentationState`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePresentationState {
    pub binding: RemoteBinding,
    pub connection: RemoteConnection,
    pub mode: PresentationMode,
    /// The origin's last layout.
    pub(crate) layout: Option<PresentationLayout>,
    /// Local panes whose ssh ended and whose terminal holds, waiting for a removal frame.
    pub(crate) held_panes: HashSet<Uuid>,
    /// Whether the glyph on the row is the bridge's. A flag rather than a comparison of values: a pane swap rewrites
    /// the indicator without changing who set it, and a local write of the very same value does. Every local write
    /// clears it.
    pub status_bridged: bool,
    /// False when the origin named a pane this machine has no counterpart for; the glyph then carries no local owner.
    pub status_owner_resolved: bool,
    /// The overlay slot generation of the HUD the bridge opened. A panel this machine's own program opens afterwards
    /// has another generation, so it is never the bridge's to close.
    pub(crate) hud_generation: Option<u64>,
}

impl RemotePresentationState {
    #[must_use]
    pub fn new(binding: RemoteBinding) -> Self {
        let connection = if binding.presentation_version.is_none() {
            RemoteConnection::Unsupported
        } else {
            RemoteConnection::Connecting
        };
        Self {
            binding,
            connection,
            mode: PresentationMode::Mirror,
            layout: None,
            held_panes: HashSet::new(),
            status_bridged: false,
            status_owner_resolved: true,
            hud_generation: None,
        }
    }

    /// Whether the glyph on the row stands for a pane this machine has no counterpart for.
    #[must_use]
    pub fn status_owner_unknown(&self) -> bool {
        self.status_bridged && !self.status_owner_resolved
    }

    /// Typing in a pane here must not clear a status that pane does not stand for.
    #[must_use]
    pub fn allows_keystroke_status_clear(&self) -> bool {
        !self.status_owner_unknown()
    }

    /// The `tree` read-back of the stream (upstream's `presentationNode`).
    #[must_use]
    pub fn node(&self) -> ControlPresentationNode {
        let mode = match self.mode {
            PresentationMode::Mirror => "mirror",
            PresentationMode::Presenter => "presenter",
        };
        let (state, error) = match &self.connection {
            RemoteConnection::Connecting => ("connecting", None),
            RemoteConnection::Connected => ("connected", None),
            RemoteConnection::Unsupported => ("unsupported", None),
            RemoteConnection::Failed(reason) => ("failed", Some(reason.clone())),
        };
        ControlPresentationNode {
            state: state.to_owned(),
            mode: mode.to_owned(),
            error,
        }
    }
}

impl Session {
    /// What this machine keeps about the session's origin; `None` for a local session.
    #[must_use]
    pub fn remote_presentation(&self) -> Option<&RemotePresentationState> {
        self.remote_presentation.as_ref()
    }

    /// The presentation stream's state; `None` for a local session.
    #[must_use]
    pub fn remote_connection(&self) -> Option<&RemoteConnection> {
        self.remote_presentation.as_ref().map(|state| &state.connection)
    }

    /// What the row tells the user while its presentation stream is not up (upstream's sidebar
    /// `presentationNotice(for:)`): `None` for a local row and for a remote one whose stream is connected or
    /// unsupported. The sidebar draws the remote glyph disconnected while there is one, and shows it as its tooltip.
    #[must_use]
    pub fn presentation_notice(&self) -> Option<String> {
        let host = self.remote_host()?;
        self.remote_connection()?.row_notice(host)
    }

    /// Whether the live HUD is the one the bridge opened.
    #[must_use]
    pub fn hud_bridged(&self) -> bool {
        self.overlay.hud_active()
            && self.remote_presentation.as_ref().and_then(|state| state.hud_generation) == Some(self.overlay.generation)
    }

    /// The local role standing for one of the origin's panes, resolved at use so a swap or a promotion here since the
    /// attach is honoured (upstream's `localPane(_:in:)`); `None` for a pane with no counterpart here.
    #[must_use]
    pub fn local_pane(&self, pane: Option<&PresentationPane>) -> Option<OverlayPane> {
        let Some(PresentationPane::Identity(remote)) = pane else {
            return None;
        };
        let local = self.remote_presentation.as_ref()?.binding.local_pane(*remote)?;
        self.pane_role_for_identity(local)
    }

    /// An unspecified owner is the origin's primary, which it sends as an identity, so `None` here is session-wide;
    /// the origin's scratch has no counterpart at all (upstream's `localStatusPane`). The pane, and whether it
    /// resolved.
    fn local_status_pane(&self, pane: Option<&PresentationPane>) -> (Option<StatusPane>, bool) {
        if pane.is_none() {
            return (None, true);
        }
        match self.local_pane(pane) {
            Some(OverlayPane::Left) => (Some(StatusPane::Left), true),
            Some(OverlayPane::Right) => (Some(StatusPane::Right), true),
            None => (None, false),
        }
    }
}

impl AppStore {
    /// Records what an attach learned about the origin (upstream's `bindRemote`), right after the row was created.
    pub fn bind_remote(&mut self, binding: RemoteBinding, id: Uuid) {
        if let Some(session) = self.session_mut(id) {
            session.remote_presentation = Some(RemotePresentationState::new(binding));
        }
    }

    /// Takes the origin's context, `None` or an invalid one clearing it (upstream's `applyRemoteContext`); a local
    /// context keeps winning over it, and the mirrored one is kept underneath. Never saved.
    pub fn apply_remote_context(&mut self, context: Option<&str>, id: Uuid) {
        let Some(session) = self
            .session_mut(id)
            .filter(|session| session.remote_presentation.is_some())
        else {
            return;
        };
        let previous = session.effective_context().map(str::to_owned);
        session.mirrored_context = context.and_then(|raw| validate_context(raw).ok());
        if previous.as_deref() != session.effective_context() {
            self.schedule_tree_changed();
        }
    }

    /// Applies the origin's status, `None` for idle (upstream's `applyRemoteStatus`). It bypasses
    /// [`AppStore::apply_control_status`], which arbitrates between panes writing locally and would refuse a clear the
    /// origin already accepted.
    pub fn apply_remote_status(&mut self, status: Option<&PresentationStatus>, id: Uuid) {
        let Some(session) = self.session(id).filter(|session| session.remote_presentation.is_some()) else {
            return;
        };
        let owner = status.map(|status| session.local_status_pane(status.pane.as_ref()));
        let indicator = status.map_or_else(AgentIndicator::default, |status| AgentIndicator {
            status: status.status,
            blink: status.blink,
            color: status.color.clone(),
            shape: status.shape,
            status_pane: owner.and_then(|(pane, _)| pane),
            ..AgentIndicator::default()
        });
        let bridged = indicator.status != AgentStatus::Idle;
        self.set_agent_indicator(indicator, id);
        let Some(session) = self.session_mut(id) else {
            return;
        };
        // the setter stamps this machine's clock; the origin's orders the rows after a reconnect
        if let Some(stamp) = status.and_then(|status| status.changed_at) {
            session.status_changed_at = crate::projection::system_time(stamp);
        }
        if let Some(state) = session.remote_presentation.as_mut() {
            state.status_bridged = bridged;
            state.status_owner_resolved = owner.is_none_or(|(_, resolved)| resolved);
        }
    }

    /// Applies the status a snapshot carries, unless the row holds a non-idle one set on this machine (upstream's
    /// `applyRemoteSnapshotStatus`): a snapshot comes with every reconnect and must not end what a local write started.
    pub fn apply_remote_snapshot_status(&mut self, status: Option<&PresentationStatus>, id: Uuid) {
        let Some(session) = self.session(id) else {
            return;
        };
        let Some(state) = session.remote_presentation.as_ref() else {
            return;
        };
        if !state.status_bridged && session.agent_indicator.status != AgentStatus::Idle {
            return;
        }
        self.apply_remote_status(status, id);
    }

    /// Moves the stream's state (upstream's `setRemoteConnection`). Leaving `connected` withdraws what the bridge put on
    /// the row: the mirrored context, a bridged status, a bridged HUD and a replica ask, since the origin takes a
    /// handed-over ask back when the stream goes. Whether the stream left `connected`, so the caller orphans the
    /// replica overlays.
    pub fn set_remote_connection(&mut self, connection: RemoteConnection, id: Uuid) -> bool {
        let Some(state) = self
            .session_mut(id)
            .and_then(|session| session.remote_presentation.as_mut())
        else {
            return false;
        };
        let previous = std::mem::replace(&mut state.connection, connection);
        let left = previous == RemoteConnection::Connected && state.connection != RemoteConnection::Connected;
        if previous != state.connection {
            self.schedule_tree_changed();
        }
        if !left {
            return false;
        }
        self.apply_remote_context(None, id);
        let bridged = self
            .session(id)
            .and_then(Session::remote_presentation)
            .is_some_and(|state| state.status_bridged);
        if bridged {
            self.set_agent_indicator(AgentIndicator::default(), id);
        }
        self.close_bridged_hud(id);
        if self
            .session(id)
            .and_then(Session::ask)
            .is_some_and(|ask| ask.replica.is_some())
        {
            self.release_session_ask(id);
        }
        true
    }

    /// Records whether this machine is the session's presenter or a mirror beside the origin.
    pub fn set_remote_mode(&mut self, mode: PresentationMode, id: Uuid) {
        if let Some(state) = self
            .session_mut(id)
            .and_then(|session| session.remote_presentation.as_mut())
            && state.mode != mode
        {
            state.mode = mode;
            self.schedule_tree_changed();
        }
    }

    /// Marks the live HUD as the bridge's, once the mirrored panel is up (upstream's `markHudBridged`).
    pub fn mark_hud_bridged(&mut self, id: Uuid) {
        let Some(session) = self.session_mut(id) else {
            return;
        };
        if !session.overlay.hud_active() {
            return;
        }
        let generation = session.overlay.generation;
        if let Some(state) = session.remote_presentation.as_mut() {
            state.hud_generation = Some(generation);
        }
    }

    /// Closes the HUD only when the bridge opened it (upstream's `closeBridgedHud`); false when there was nothing of
    /// the bridge's to close.
    pub fn close_bridged_hud(&mut self, id: Uuid) -> bool {
        if !self.session(id).is_some_and(Session::hud_bridged) {
            return false;
        }
        self.close_hud(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::daemon_name;
    use crate::model::{NewSession, StatusShape};

    fn binding(local: Uuid, remote: Uuid, version: Option<i64>) -> RemoteBinding {
        RemoteBinding::new("ORIGIN", HashMap::from([(local, daemon_name(remote))]), version, None)
    }

    fn attached() -> (AppStore, Uuid, Uuid) {
        let mut store = AppStore::new();
        let workspace = store.add_workspace("work");
        let id = store
            .add_session(
                workspace,
                NewSession {
                    remote_host: Some("origin".to_owned()),
                    ..NewSession::at_cwd("/tmp")
                },
            )
            .unwrap();
        let remote = Uuid::new_v4();
        let local = store.session(id).unwrap().pane_identity;
        store.bind_remote(binding(local, remote, Some(1)), id);
        (store, id, remote)
    }

    fn status(pane: Option<PresentationPane>) -> PresentationStatus {
        PresentationStatus {
            status: AgentStatus::Blocked,
            blink: true,
            color: Some("#ff0000".to_owned()),
            shape: Some(StatusShape::Diamond),
            pane,
            changed_at: Some(1_000.0),
        }
    }

    #[test]
    fn the_binding_maps_each_remote_pane_to_its_local_one() {
        let (local, remote) = (Uuid::new_v4(), Uuid::new_v4());
        let mapped = binding(local, remote, Some(1));
        assert_eq!(mapped.local_pane(remote), Some(local));
        assert_eq!(mapped.remote_pane(local), Some(remote));
        assert_eq!(mapped.daemon(local), Some(daemon_name(remote).as_str()));
        assert_eq!(mapped.local_pane(local), None);
    }

    #[test]
    fn an_origin_without_the_stream_starts_unsupported() {
        let (local, remote) = (Uuid::new_v4(), Uuid::new_v4());
        assert_eq!(
            RemotePresentationState::new(binding(local, remote, Some(1))).connection,
            RemoteConnection::Connecting
        );
        assert_eq!(
            RemotePresentationState::new(binding(local, remote, None)).connection,
            RemoteConnection::Unsupported
        );
    }

    #[test]
    fn the_row_notice_names_the_host_and_the_manual_recovery() {
        assert_eq!(RemoteConnection::Connected.row_notice("box"), None);
        assert_eq!(RemoteConnection::Unsupported.row_notice("box"), None);
        assert_eq!(
            RemoteConnection::Failed("exit 255".to_owned())
                .row_notice("box")
                .unwrap(),
            "Lost the connection to box (exit 255), retrying. Close and reattach the session to retry now."
        );
        assert!(RemoteConnection::Connecting.row_notice("box").unwrap().contains("box"));
    }

    #[test]
    fn a_mirrored_status_lands_on_the_mapped_local_pane_with_its_glyph_overrides() {
        let (mut store, id, remote) = attached();
        store.apply_remote_status(Some(&status(Some(PresentationPane::Identity(remote)))), id);
        let session = store.session(id).unwrap();
        assert_eq!(session.agent_indicator.status, AgentStatus::Blocked);
        assert_eq!(session.agent_indicator.status_pane, Some(StatusPane::Left));
        assert_eq!(session.agent_indicator.shape, Some(StatusShape::Diamond));
        assert!(session.remote_presentation().unwrap().status_bridged);
        // the origin's clock orders the row, not the arrival time
        assert_eq!(session.status_changed_at, crate::projection::system_time(1_000.0));
    }

    #[test]
    fn a_status_for_a_pane_with_no_counterpart_carries_no_owner() {
        for pane in [PresentationPane::Identity(Uuid::new_v4()), PresentationPane::Scratch] {
            let (mut store, id, _) = attached();
            store.apply_remote_status(Some(&status(Some(pane))), id);
            let session = store.session(id).unwrap();
            assert_eq!(session.agent_indicator.status_pane, None);
            assert!(session.remote_presentation().unwrap().status_owner_unknown());
            assert!(!session.remote_presentation().unwrap().allows_keystroke_status_clear());
        }
    }

    #[test]
    fn a_local_write_takes_the_status_away_from_the_bridge_and_a_snapshot_leaves_it() {
        let (mut store, id, remote) = attached();
        let mirrored = status(Some(PresentationPane::Identity(remote)));
        store.apply_remote_status(Some(&mirrored), id);
        store.set_agent_indicator(store.session(id).unwrap().agent_indicator.clone(), id);
        assert!(!store.session(id).unwrap().remote_presentation().unwrap().status_bridged);
        store.apply_remote_snapshot_status(None, id);
        assert_eq!(store.session(id).unwrap().agent_indicator.status, AgentStatus::Blocked);
        // a delta still replaces it
        store.apply_remote_status(None, id);
        assert_eq!(store.session(id).unwrap().agent_indicator.status, AgentStatus::Idle);
        // and an idle row takes the snapshot's
        store.apply_remote_snapshot_status(Some(&mirrored), id);
        assert_eq!(store.session(id).unwrap().agent_indicator.status, AgentStatus::Blocked);
    }

    #[test]
    fn a_mirrored_context_shows_under_a_local_one_and_an_invalid_one_clears_it() {
        let (mut store, id, _) = attached();
        store.apply_remote_context(Some("  release prep  "), id);
        assert_eq!(store.session(id).unwrap().effective_context(), Some("release prep"));
        store.set_context(Some("mine".to_owned()), id);
        assert_eq!(store.session(id).unwrap().effective_context(), Some("mine"));
        store.apply_remote_context(Some("first\nsecond"), id);
        store.set_context(None, id);
        assert_eq!(store.session(id).unwrap().effective_context(), None);
    }

    #[test]
    fn a_mirrored_context_is_ignored_on_a_local_session() {
        let mut store = AppStore::new();
        let workspace = store.add_workspace("work");
        let id = store.add_session(workspace, NewSession::at_cwd("/tmp")).unwrap();
        store.apply_remote_context(Some("origin"), id);
        assert_eq!(store.session(id).unwrap().effective_context(), None);
        store.apply_remote_status(Some(&status(None)), id);
        assert_eq!(store.session(id).unwrap().agent_indicator.status, AgentStatus::Idle);
    }

    #[test]
    fn losing_the_stream_withdraws_the_bridge_and_keeps_what_was_set_here() {
        let (mut store, id, remote) = attached();
        assert!(!store.set_remote_connection(RemoteConnection::Connected, id));
        store.apply_remote_context(Some("origin"), id);
        store.apply_remote_status(Some(&status(Some(PresentationPane::Identity(remote)))), id);
        assert!(store.set_remote_connection(RemoteConnection::Failed("exit 255".to_owned()), id));
        let session = store.session(id).unwrap();
        assert_eq!(session.effective_context(), None);
        assert_eq!(session.agent_indicator.status, AgentStatus::Idle);
        // a status set here survives the next loss
        store.set_remote_connection(RemoteConnection::Connected, id);
        store.set_agent_indicator(
            AgentIndicator {
                status: AgentStatus::Active,
                ..AgentIndicator::default()
            },
            id,
        );
        store.set_remote_connection(RemoteConnection::Connecting, id);
        assert_eq!(store.session(id).unwrap().agent_indicator.status, AgentStatus::Active);
        let session = store.session(id).unwrap();
        assert!(
            session
                .presentation_notice()
                .unwrap()
                .starts_with("Connecting to origin")
        );
        let node = session.remote_presentation().unwrap().node();
        assert_eq!((node.state.as_str(), node.mode.as_str()), ("connecting", "mirror"));
    }
}
