//! The origin's pane layout applied to a viewer's replicas (upstream's `AppStore+RemoteLayout.swift` and
//! `agtermApp+RemoteLayout.swift`): the replicas that already exist follow the origin's order, axis and visibility, a
//! replica the origin removed is closed once that is safe, and a pane closed or split here is never recreated or
//! reordered by a layout.

use ngterm_host::Surface;
use uuid::Uuid;

use super::frames::PresentationLayout;
use crate::Core;
use crate::model::{AppStore, Session, SplitAxis, StatusPane};

fn realized(slot: &crate::model::SurfaceSlot) -> bool {
    slot.get().is_some_and(Surface::is_realized)
}

impl AppStore {
    /// Applies `layout` to session `id`'s existing replicas (upstream's `applyRemoteLayout`): only when both panes here
    /// are realized replicas of exactly the origin's two panes does it swap, set the axis and show or hide the split.
    /// The local identities a removal frame now lets close.
    pub fn apply_remote_layout(&mut self, layout: &PresentationLayout, id: Uuid) -> Vec<Uuid> {
        if !layout.is_valid() {
            return Vec::new();
        }
        let Some(state) = self
            .session_mut(id)
            .and_then(|session| session.remote_presentation.as_mut())
        else {
            return Vec::new();
        };
        state.layout = Some(layout.clone());
        if let Some(swap) = self.session(id).and_then(|session| replicas_follow(session, layout)) {
            if swap && self.swap_panes(id).is_err() {
                return Vec::new();
            }
            if let Some(axis) = layout.axis.as_deref().and_then(SplitAxis::from_wire)
                && let Some(session) = self.session_mut(id)
            {
                session.split.axis = axis;
            }
            self.set_split_visibility(id, layout.shown, None);
        }
        let Some(session) = self.session(id) else {
            return Vec::new();
        };
        [session.split_pane_identity, Some(session.pane_identity)]
            .into_iter()
            .flatten()
            .filter(|local| self.can_close_removed_remote_pane(*local, id))
            .collect()
    }

    /// Whether a confirmed removal on the origin lets this replica close without discarding a pending local split
    /// (upstream's `canCloseRemovedRemotePane`).
    #[must_use]
    pub fn can_close_removed_remote_pane(&self, local: Uuid, id: Uuid) -> bool {
        let Some(session) = self.session(id) else {
            return false;
        };
        let Some(state) = session.remote_presentation() else {
            return false;
        };
        let (Some(layout), Some(remote)) = (state.layout.as_ref(), state.binding.remote_pane(local)) else {
            return false;
        };
        if session.pane_role_for_identity(local).is_none() || layout.panes.contains(&remote) {
            return false;
        }
        // the ordinary primary close would discard a local split that has not produced its terminal yet
        if local == session.pane_identity
            && let Some(split) = session.split_pane_identity
            && state.binding.remote_pane(split).is_none()
            && !realized(&session.split_surface)
        {
            return false;
        }
        true
    }

    /// Records that the ssh of the mapped replica `local` ended and its terminal holds, for a later removal frame
    /// (upstream's `remotePaneHeld`).
    pub fn remote_pane_held(&mut self, local: Uuid, id: Uuid) {
        let Some(session) = self.session_mut(id) else {
            return;
        };
        if session.pane_role_for_identity(local).is_none() {
            return;
        }
        if let Some(state) = session.remote_presentation.as_mut()
            && state.binding.remote_pane(local).is_some()
        {
            state.held_panes.insert(local);
        }
    }

    /// Whether a held exit was recorded for the replica `local`.
    #[must_use]
    pub fn remote_pane_is_held(&self, local: Uuid, id: Uuid) -> bool {
        self.session(id)
            .and_then(Session::remote_presentation)
            .is_some_and(|state| state.held_panes.contains(&local))
    }

    /// The held replica `local` attached again.
    pub fn remote_pane_resumed(&mut self, local: Uuid, id: Uuid) {
        if let Some(state) = self
            .session_mut(id)
            .and_then(|session| session.remote_presentation.as_mut())
        {
            state.held_panes.remove(&local);
        }
    }
}

/// `Some(swap)` when both panes are realized replicas of exactly the layout's two panes, `swap` when the layout's
/// primary is the local split; `None` otherwise, which leaves the arrangement alone.
fn replicas_follow(session: &Session, layout: &PresentationLayout) -> Option<bool> {
    let split = session.split_pane_identity.filter(|_| session.split.exists)?;
    if !realized(&session.surface) || !realized(&session.split_surface) {
        return None;
    }
    let binding = &session.remote_presentation()?.binding;
    let primary_origin = binding.remote_pane(session.pane_identity)?;
    let split_origin = binding.remote_pane(split)?;
    let mut wanted = layout.panes.clone();
    wanted.sort_unstable();
    let mut have = vec![primary_origin, split_origin];
    have.sort_unstable();
    (wanted == have).then(|| binding.local_pane(layout.primary) == Some(split))
}

impl Core {
    /// Applies the origin's layout to session `id`'s replicas and closes each one the origin removed (upstream's
    /// `agtermApp.applyRemoteLayout`).
    pub(crate) fn apply_remote_layout(&mut self, layout: &PresentationLayout, id: Uuid) {
        let removed = self
            .library_mut()
            .store_for_session_mut(id)
            .map(|store| store.apply_remote_layout(layout, id))
            .unwrap_or_default();
        for local in removed {
            self.close_removed_remote_pane(local, id);
        }
    }

    /// The ssh of a replica's pane ended and its terminal holds (upstream's `handleRemotePaneHeld`): recorded, and the
    /// pane closes now when the origin already removed it.
    pub fn remote_pane_held(&mut self, session: Uuid, token: &str) -> bool {
        let Ok(local) = Uuid::parse_str(token) else {
            return false;
        };
        let Some(store) = self.library_mut().store_for_session_mut(session) else {
            return false;
        };
        store.remote_pane_held(local, session);
        self.close_removed_remote_pane(local, session)
    }

    /// Closes the replica `local` the origin removed, when that is safe (upstream's `closeRemovedRemotePane`): a split
    /// with no terminal yet closes outright; otherwise the last replica keeps its terminal until its ssh ends, so no
    /// empty row needs replacing. Whether it closed.
    fn close_removed_remote_pane(&mut self, local: Uuid, id: Uuid) -> bool {
        let Some(store) = self.library_mut().store_for_session_mut(id) else {
            return false;
        };
        if !store.can_close_removed_remote_pane(local, id) {
            return false;
        }
        let held = store.remote_pane_is_held(local, id);
        let Some(session) = store.session(id) else {
            return false;
        };
        let split = session.split_pane_identity == Some(local);
        let (slot, survivor) = if split {
            (&session.split_surface, &session.surface)
        } else {
            (&session.surface, &session.split_surface)
        };
        let Some(token) = slot.get().map(|surface| surface.pane_token().to_owned()) else {
            if split {
                store.close_split(id);
                return true;
            }
            return false;
        };
        if !realized(survivor) && !held {
            return false;
        }
        let role = session.pane_role_for_token(&token);
        if role == Some(StatusPane::Scratch) || role.is_none() {
            return false;
        }
        self.pane_exited(id, &token)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use ngterm_host::fakes::FakeSurface;

    use super::*;
    use crate::live::daemon_name;
    use crate::model::{NewSession, RemoteBinding};

    struct Attached {
        store: AppStore,
        id: Uuid,
        origin_a: Uuid,
        origin_b: Uuid,
    }

    fn fake(identity: Uuid, realized: bool) -> Box<dyn ngterm_host::Surface> {
        Box::new(FakeSurface::new(crate::pane_env::uuid_string(identity)).with_realized(realized))
    }

    fn attached() -> Attached {
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
        store.toggle_split(id, None);
        let session = store.session_mut(id).unwrap();
        let (primary, split) = (session.pane_identity, session.split_pane_identity.unwrap());
        session.surface.replace(fake(primary, true));
        session.split_surface.replace(fake(split, true));
        let (origin_a, origin_b) = (Uuid::new_v4(), Uuid::new_v4());
        let daemons = HashMap::from([(primary, daemon_name(origin_a)), (split, daemon_name(origin_b))]);
        store.bind_remote(RemoteBinding::new("ORIGIN", daemons, Some(1), None), id);
        Attached {
            store,
            id,
            origin_a,
            origin_b,
        }
    }

    fn layout(panes: Vec<Uuid>, primary: Uuid, axis: Option<&str>, shown: bool) -> PresentationLayout {
        PresentationLayout {
            panes,
            primary,
            axis: axis.map(str::to_owned),
            shown,
        }
    }

    #[test]
    fn a_layout_reorders_only_existing_replicas_and_keeps_focus_on_its_pane() {
        let mut a = attached();
        let session = a.store.session_mut(a.id).unwrap();
        let (primary, split) = (session.pane_identity, session.split_pane_identity.unwrap());
        session.split.focused = false;
        session.split.ratio = Some(0.35);
        let swapped = layout(vec![a.origin_b, a.origin_a], a.origin_b, Some("horizontal"), false);
        assert!(a.store.apply_remote_layout(&swapped, a.id).is_empty());
        let session = a.store.session(a.id).unwrap();
        assert_eq!(session.pane_identity, split);
        assert_eq!(session.split_pane_identity, Some(primary));
        assert!(session.split.focused);
        assert_eq!(session.split.ratio, Some(0.35));
        assert_eq!(session.split.axis, SplitAxis::from_wire("horizontal").unwrap());
        assert!(!session.split.shown);
        assert!(session.split.exists);
        let removal = layout(vec![a.origin_b], a.origin_b, None, false);
        assert_eq!(a.store.apply_remote_layout(&removal, a.id), [primary]);
    }

    #[test]
    fn a_closed_replica_is_never_recreated() {
        let mut a = attached();
        a.store.close_split(a.id);
        let both = layout(vec![a.origin_a, a.origin_b], a.origin_a, Some("horizontal"), true);
        assert!(a.store.apply_remote_layout(&both, a.id).is_empty());
        let session = a.store.session(a.id).unwrap();
        assert!(!session.split.exists);
        assert_eq!(session.split_pane_identity, None);
        assert!(!session.split_surface.is_present());
    }

    #[test]
    fn a_local_split_is_neither_reordered_nor_hidden() {
        for pending in [false, true] {
            let mut a = attached();
            a.store.close_split(a.id);
            a.store.toggle_split(a.id, None);
            if !pending {
                let session = a.store.session_mut(a.id).unwrap();
                let split = session.split_pane_identity.unwrap();
                session.split_surface.replace(fake(split, true));
            }
            let session = a.store.session(a.id).unwrap();
            let (local, primary, axis) = (session.split_pane_identity, session.pane_identity, session.split.axis);
            let swapped = layout(vec![a.origin_b, a.origin_a], a.origin_b, Some("horizontal"), false);
            assert!(a.store.apply_remote_layout(&swapped, a.id).is_empty());
            let session = a.store.session(a.id).unwrap();
            assert_eq!(session.split_pane_identity, local);
            assert_eq!(session.pane_identity, primary);
            assert!(session.split.shown);
            assert_eq!(session.split.axis, axis);
            let removed = layout(vec![a.origin_b], a.origin_b, None, false);
            a.store.apply_remote_layout(&removed, a.id);
            assert_eq!(
                a.store.can_close_removed_remote_pane(primary, a.id),
                !pending,
                "pending: {pending}"
            );
            assert!(!a.store.can_close_removed_remote_pane(local.unwrap(), a.id));
        }
    }

    #[test]
    fn an_unrealized_replica_is_not_shown_by_a_layout() {
        let mut a = attached();
        let session = a.store.session_mut(a.id).unwrap();
        let split = session.split_pane_identity.unwrap();
        session.split_surface.replace(fake(split, false));
        a.store.set_split_visibility(a.id, false, None);
        let shown = layout(vec![a.origin_a, a.origin_b], a.origin_a, Some("vertical"), true);
        a.store.apply_remote_layout(&shown, a.id);
        assert!(!a.store.session(a.id).unwrap().split.shown);
    }

    #[test]
    fn a_replica_that_attached_again_is_no_longer_held() {
        let mut a = attached();
        let primary = a.store.session(a.id).unwrap().pane_identity;
        a.store.remote_pane_held(primary, a.id);
        assert!(a.store.remote_pane_is_held(primary, a.id));
        a.store.remote_pane_resumed(primary, a.id);
        assert!(!a.store.remote_pane_is_held(primary, a.id));
    }
}
