//! A pane's launch under live sessions (upstream's `ZmxLaunch.swift`, the pane factories of `agtermApp.swift`,
//! `ZmxForegroundResolver.swift` and `LaunchOrchestration`): the disposition each pane spawns under, the leader cache a
//! live pane's foreground is read through, and the launch reap that runs before any window mounts.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use agterm_host::{ProcessTable, SessionHost};
use uuid::Uuid;

use super::ZmxRuntime;
use crate::Core;
use crate::model::{Session, StatusPane};
use crate::persist::RestoreMode;
use crate::restore::spawn_plan::{
    LaunchSeed, LaunchSeedPolicy, resolve_live_seed, resolve_seed, should_pace, should_pace_live,
};
use crate::zmx::lead::LeadAttachment;
use crate::zmx::lifecycle::{RefreshGate, leaders, names_to_kill};
use crate::zmx::support::{self, Configuration, LaunchDisposition, launch_disposition};
use crate::zmx::{daemon_name, pane_identity};

/// The daemons' leader pids as last listed (upstream's `ZmxForegroundResolver`): refreshed when a lifecycle change
/// invalidated it or the reconcile interval passed, and taken whole from every fresh listing the tree makes.
#[derive(Debug, Default)]
pub(crate) struct LeaderCache {
    leaders: BTreeMap<String, i32>,
    gate: RefreshGate,
}

impl LeaderCache {
    pub(crate) fn note_lifecycle_change(&mut self) {
        self.gate.note_lifecycle_change();
    }

    /// Takes a fresh listing's leaders; a failed one (`None`) drops the cached leaders rather than keep them, so no
    /// pane reports a foreground from a daemon that may be gone. Either counts as a refresh.
    pub(crate) fn accept(&mut self, snapshot: Option<BTreeMap<String, i32>>) {
        self.leaders = snapshot.unwrap_or_default();
        self.gate.note_lifecycle_change();
        let _ = self.gate.should_refresh(Instant::now());
    }

    fn refresh_if_needed(&mut self, host: &dyn SessionHost, now: Instant) {
        if !self.gate.should_refresh(now) {
            return;
        }
        if let Ok(records) = host.list() {
            self.leaders = leaders(&records);
        }
    }

    /// The leader of daemon `name`, as last listed.
    pub(crate) fn leader(&self, name: &str) -> Option<i32> {
        self.leaders.get(name).copied()
    }
}

/// The foreground process group of the live pane carrying `token`: its daemon's shell's terminal foreground (upstream's
/// `ZmxForegroundResolver.foregroundPID`). A leader the table no longer holds is evicted and the cache invalidated.
pub(crate) fn live_foreground_group(zmx: &ZmxRuntime, table: &dyn ProcessTable, token: &str) -> Option<i32> {
    let name = daemon_name(Uuid::parse_str(token).ok()?);
    let mut cache = zmx.leaders.borrow_mut();
    if let Some(host) = zmx.host() {
        cache.refresh_if_needed(host.as_ref(), Instant::now());
    }
    let Some(leader) = cache.leader(&name) else {
        cache.note_lifecycle_change();
        return None;
    };
    if table.stat(leader).is_none() {
        cache.leaders.remove(&name);
        cache.note_lifecycle_change();
        tracing::debug!(leader, "evicted a dead zmx leader");
        return None;
    }
    table.terminal_foreground(leader).filter(|pgid| *pgid > 0)
}

/// A pane's disposition and, when wrapped, the lead attachment its configuration carries.
pub(crate) struct Disposed {
    pub(crate) disposition: LaunchDisposition,
    attachment: Option<LeadAttachment>,
}

impl Core {
    /// The seed policy of this launch: the restore decision, the denylist, and what the launch reap and the reset left.
    pub(crate) fn seed_policy(&self) -> LaunchSeedPolicy {
        LaunchSeedPolicy {
            running_names: self.zmx.running_names.clone(),
            suppressed_daemons: self.zmx.suppressed.iter().copied().map(daemon_name).collect(),
            ..self.restore.policy()
        }
    }

    /// The disposition pane `identity` of a session spawns under (upstream's `ZmxLaunch.disposition` over
    /// `wrapsLocally` and `configuration`): ordinary outside a live request, wrapped when this launch runs live and the
    /// pane is local and its configuration builds, else the fallback. A wrapped pane attaches without the claim: one
    /// relaunched while another client leads its daemon comes back covered.
    pub(crate) fn pane_disposition(&self, identity: Option<Uuid>, local: bool) -> Disposed {
        let decision = &self.restore.decision;
        let attachment = LeadAttachment::new(false);
        let configuration = (decision.active == RestoreMode::Live && local)
            .then(|| self.wrap_configuration(identity?, Some(attachment.clone())))
            .flatten();
        let disposition = launch_disposition(decision.requested, decision.active, configuration);
        Disposed {
            attachment: disposition.backed_by_zmx().then_some(attachment),
            disposition,
        }
    }

    /// Pane `identity`'s wrap configuration with `lead`; `None` (logged) when this instance cannot wrap it.
    pub(crate) fn wrap_configuration(&self, identity: Uuid, lead: Option<LeadAttachment>) -> Option<Configuration> {
        let mut inputs = self.zmx.wrap.clone()?;
        inputs.pane_identity = identity;
        inputs.lead = lead;
        support::configuration(&inputs)
            .inspect_err(|rejection| {
                tracing::error!(pane = %identity, reason = rejection.message(), "zmx configuration failed");
            })
            .ok()
    }

    /// The launch steps that follow one another before any window mounts (upstream's `LaunchOrchestration.run`): a
    /// Live sessions reset marker is consumed by a launch that got live and discarded by any other, then the reap ends
    /// the zero-client daemons no pane restores (every app daemon outside a live request). A no-op without zmx.
    pub fn run_live_launch(&mut self) {
        let Some(host) = self.zmx.host().cloned() else {
            return;
        };
        let decision = self.restore.decision.clone();
        let live = decision.requested == RestoreMode::Live && decision.active == RestoreMode::Live;
        if live {
            self.consume_live_reset(host.as_ref());
        } else if let Some(markers) = &self.zmx.markers {
            markers.remove();
        }
        let walk = self.pane_claims();
        let known: Option<BTreeSet<String>> = walk.complete.then(|| {
            walk.claims
                .iter()
                .map(|claim| daemon_name(claim.pane_identity))
                .collect()
        });
        self.zmx.running_names = reap(host.as_ref(), known.as_ref(), decision.requested);
        self.zmx.leaders.borrow_mut().note_lifecycle_change();
    }
}

impl Core {
    /// Fills the tree's live read-back projected from `store` (upstream's `buildTree` live parts): a fresh listing of
    /// the leaders whenever a local pane is wrapped, each backed pane's attribution, each pane's lead, and the reset
    /// read-back at the top level.
    pub(crate) fn project_live(&self, store: &crate::model::AppStore, tree: &mut agterm_proto::ControlTree) {
        tree.live_reset = super::readback(self.zmx.reset.pending.as_ref(), self.zmx.reset.last.as_ref());
        let backed = |slot: &crate::model::SurfaceSlot| slot.get().is_some_and(agterm_host::Surface::is_live_session);
        let wrapped = store.sessions().any(|session| {
            session.remote_host().is_none() && (backed(&session.surface) || backed(&session.split_surface))
        });
        let mut listed = false;
        if wrapped && let Some(host) = self.zmx.host() {
            let snapshot = host.list().ok().map(|records| leaders(&records));
            listed = snapshot.is_some();
            self.zmx.leaders.borrow_mut().accept(snapshot);
        }
        let nodes = tree
            .workspaces
            .iter_mut()
            .flat_map(|workspace| workspace.sessions.iter_mut());
        for (node, session) in nodes.zip(store.sessions()) {
            if listed && session.remote_host().is_none() {
                // Linux has no responsibility API to attribute a leader by (see `live_reset`)
                let attribution =
                    |slot| backed(slot).then(|| <&str>::from(super::live_reset::LINUX_ATTRIBUTION).to_owned());
                node.live_attribution = attribution(&session.surface);
                node.split_live_attribution = session
                    .split
                    .exists
                    .then(|| attribution(&session.split_surface))
                    .flatten();
            }
            for surface in node.surfaces.iter_mut().flatten() {
                surface.lead = surface
                    .pane_id
                    .as_deref()
                    .and_then(|token| self.pane_lead(token))
                    .map(str::to_owned);
                surface.reconnect = surface
                    .pane_id
                    .as_deref()
                    .and_then(|token| uuid::Uuid::parse_str(token).ok())
                    .and_then(|pane| self.reconnect_readback(pane));
            }
        }
    }
}

/// Whether pane `role` of `session` waits for its paced launch turn under its disposition.
pub(crate) fn pane_paces(session: &Session, role: StatusPane, policy: &LaunchSeedPolicy, disposed: &Disposed) -> bool {
    match &disposed.disposition {
        LaunchDisposition::Wrapped(configuration) => should_pace_live(session, role, policy, configuration),
        LaunchDisposition::Ordinary | LaunchDisposition::Fallback => should_pace(session, role, policy),
    }
}

/// Pane `role`'s seed under its disposition, consuming the slots the launch armed for it; a wrapped pane's lead
/// attachment starts here, where its attach is decided.
pub(crate) fn pane_seed(
    zmx: &mut ZmxRuntime,
    session: &mut Session,
    role: StatusPane,
    policy: &LaunchSeedPolicy,
    disposed: Disposed,
) -> LaunchSeed {
    match &disposed.disposition {
        LaunchDisposition::Wrapped(configuration) => {
            if let (Some(identity), Some(attachment)) = (pane_identity(&configuration.daemon_name), disposed.attachment)
            {
                zmx.leads.begin(identity, attachment, false);
            }
            zmx.leaders.borrow_mut().note_lifecycle_change();
            resolve_live_seed(session, role, policy, configuration)
        }
        LaunchDisposition::Ordinary | LaunchDisposition::Fallback => resolve_seed(session, role, policy),
    }
}

/// The launch reap (upstream's `ZmxClient.reap`): the names of every daemon whose client count the listing could
/// read, alive with or without a client, after killing the zero-client app daemons [`names_to_kill`] picks. `None`
/// when the live inventory is incomplete (nothing is listed or killed) or the listing failed.
pub(crate) fn reap(
    host: &dyn SessionHost,
    known_names: Option<&BTreeSet<String>>,
    requested: RestoreMode,
) -> Option<BTreeSet<String>> {
    if requested == RestoreMode::Live && known_names.is_none() {
        tracing::error!("skipping the live zmx reap: the pane inventory is incomplete");
        return None;
    }
    let records = host
        .list()
        .inspect_err(|error| tracing::error!(%error, "zmx list failed during the launch reap"))
        .ok()?;
    let running = records
        .iter()
        .filter(|record| record.clients.is_some())
        .map(|record| record.name.clone())
        .collect();
    if let Some(names) = names_to_kill(&records, requested, known_names)
        && !names.is_empty()
        && !host.kill_batch(&names)
    {
        tracing::error!(
            names = names.join(","),
            "the launch reap could not end every orphan daemon"
        );
    }
    Some(running)
}

#[cfg(test)]
mod tests {
    use agterm_host::fakes::FakeSessionHost;

    use super::*;

    fn app_name(n: u128) -> String {
        daemon_name(Uuid::from_u128(n))
    }

    #[test]
    fn the_live_reap_ends_only_unclaimed_detached_daemons_and_reports_the_running_ones() {
        let host = FakeSessionHost::new();
        host.set_record(&app_name(1), Some(0), Some(10), None);
        host.set_record(&app_name(2), Some(0), Some(11), None);
        host.set_record(&app_name(3), Some(1), Some(12), None);
        host.set_record("notes", Some(0), Some(13), None);
        let known = BTreeSet::from([app_name(1)]);
        let running = reap(&host, Some(&known), RestoreMode::Live).unwrap();
        assert_eq!(host.kills(), [(app_name(2), true)]);
        // read from the listing before the kill, as upstream's reap: a pacing hint only
        assert_eq!(
            running,
            BTreeSet::from([app_name(1), app_name(2), app_name(3), "notes".to_owned()])
        );
    }

    #[test]
    fn a_mode_switch_reaps_every_detached_app_daemon() {
        let host = FakeSessionHost::new();
        host.set_record(&app_name(1), Some(0), Some(10), None);
        host.set_record(&app_name(2), Some(1), Some(11), None);
        let _ = reap(&host, None, RestoreMode::None);
        assert_eq!(host.kills(), [(app_name(1), true)]);
        let rerun = FakeSessionHost::new();
        rerun.set_record(&app_name(4), Some(0), Some(10), None);
        let _ = reap(&rerun, Some(&BTreeSet::from([app_name(4)])), RestoreMode::Rerun);
        assert_eq!(rerun.kills(), [(app_name(4), true)]);
    }

    #[test]
    fn an_incomplete_live_inventory_or_a_failed_listing_reaps_nothing() {
        let host = FakeSessionHost::new();
        host.set_record(&app_name(1), Some(0), Some(10), None);
        assert_eq!(reap(&host, None, RestoreMode::Live), None);
        host.fail_list("down");
        assert_eq!(reap(&host, Some(&BTreeSet::new()), RestoreMode::Live), None);
        assert!(host.kills().is_empty());
    }
}
