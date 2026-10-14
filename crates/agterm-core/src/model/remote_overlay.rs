//! Overlays handed to the viewer presenting a session, on the origin (upstream's `RemoteOverlays.swift` and the model
//! half of `AppStore+RemoteOverlay.swift`).
//!
//! A slot reserved for a remote job covers nothing here, but a second overlay cannot open on it until the job ends.
//! What a close needs beyond the model (telling the presenter, cancelling the job) is queued as a
//! [`RemoteOverlayClose`] for the core, as the store cannot reach the presentation hub or the job table.

use uuid::Uuid;

use super::{AppStore, OverlayPane, Session};

/// A session overlay slot reserved for a job a viewer presents (upstream's `RemoteOverlaySlot`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteOverlaySlot {
    pub job: String,
    /// The pane the slot covers, `None` for the session-wide slot. Follows its pane across a swap.
    pub pane: Option<OverlayPane>,
    /// The presenter generation the job was handed to; only that stream can still reach its surface.
    pub owner: i64,
    /// The size requested for a session-wide overlay, `None` for full. What the viewer applied is not known.
    pub size_percent: Option<i64>,
    /// The viewer's surface stays up after the program ends (`--wait`), which keeps the slot held until it closes.
    pub wait: bool,
    /// The program ended, its result recorded, while a held surface keeps the slot.
    pub ended: bool,
}

/// A session's remote overlay state on the origin: the reserved slots, and per slot the failure a remote job ended
/// with, which `session.overlay.result` reports until the next open there (upstream's `RemoteOverlays`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RemoteOverlays {
    slots: Vec<RemoteOverlaySlot>,
    failures: [Option<String>; 3],
}

/// The failure index of a slot: the session-wide one, then the panes.
fn failure_index(pane: Option<OverlayPane>) -> usize {
    match pane {
        None => 0,
        Some(OverlayPane::Left) => 1,
        Some(OverlayPane::Right) => 2,
    }
}

impl RemoteOverlays {
    #[must_use]
    pub fn slots(&self) -> &[RemoteOverlaySlot] {
        &self.slots
    }

    #[must_use]
    pub fn slot(&self, pane: Option<OverlayPane>) -> Option<&RemoteOverlaySlot> {
        self.slots.iter().find(|slot| slot.pane == pane)
    }

    #[must_use]
    pub fn slot_of_job(&self, job: &str) -> Option<&RemoteOverlaySlot> {
        self.slots.iter().find(|slot| slot.job == job)
    }

    #[must_use]
    pub fn failure(&self, pane: Option<OverlayPane>) -> Option<&str> {
        self.failures[failure_index(pane)].as_deref()
    }

    pub(crate) fn reserve(&mut self, slot: RemoteOverlaySlot) {
        self.failures[failure_index(slot.pane)] = None;
        self.slots.push(slot);
    }

    /// The program ended: the slot is freed, unless a held surface still occupies it on the viewer.
    pub(crate) fn end(&mut self, job: &str) -> Option<RemoteOverlaySlot> {
        let index = self.slots.iter().position(|slot| slot.job == job)?;
        let slot = self.slots[index].clone();
        if slot.wait {
            self.slots[index].ended = true;
        } else {
            self.slots.remove(index);
        }
        Some(slot)
    }

    /// The viewer's surface is gone: a slot whose program ended is freed; a running one is freed when it ends.
    pub(crate) fn surface_gone(&mut self, job: &str) {
        let Some(index) = self.slots.iter().position(|slot| slot.job == job) else {
            return;
        };
        if self.slots[index].ended {
            self.slots.remove(index);
        } else {
            self.slots[index].wait = false;
        }
    }

    pub(crate) fn resize(&mut self, job: &str, size_percent: Option<i64>) {
        if let Some(slot) = self.slots.iter_mut().find(|slot| slot.job == job) {
            slot.size_percent = size_percent;
        }
    }

    pub(crate) fn remove(&mut self, job: &str) {
        self.slots.retain(|slot| slot.job != job);
    }

    /// The origin swapped its panes: each pane's slot and result go with it.
    pub(crate) fn swap_panes(&mut self) {
        for slot in &mut self.slots {
            slot.pane = slot.pane.map(|pane| match pane {
                OverlayPane::Left => OverlayPane::Right,
                OverlayPane::Right => OverlayPane::Left,
            });
        }
        self.failures.swap(1, 2);
    }

    /// The split was promoted to the primary pane, whose own slot is already dropped.
    pub(crate) fn promote_right(&mut self) {
        for slot in &mut self.slots {
            if slot.pane == Some(OverlayPane::Right) {
                slot.pane = Some(OverlayPane::Left);
            }
        }
        self.failures[1] = self.failures[2].take();
    }

    pub(crate) fn record_failure(&mut self, name: &str, pane: Option<OverlayPane>) {
        self.failures[failure_index(pane)] = Some(name.to_owned());
    }

    /// A local open on the slot starts a new result, as the local exit code does.
    pub(crate) fn clear_failure(&mut self, pane: Option<OverlayPane>) {
        self.failures[failure_index(pane)] = None;
    }
}

/// A remote overlay the model closed, for the core to tell the presenter and cancel the job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteOverlayClose {
    pub session: Uuid,
    pub job: String,
}

/// How a remote job ended, as the slot's result records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteOverlayOutcome {
    Exited(i32),
    /// A failure name (`canceled`, `launch-failed`, `unknown`).
    Failed(String),
}

impl Session {
    /// The overlay slots a viewer's presenter holds and the outcomes of remote jobs.
    #[must_use]
    pub fn remote_overlays(&self) -> &RemoteOverlays {
        &self.remote_overlays
    }

    /// Whether the slot `pane` addresses is taken by an overlay here (upstream's `localOverlayHolds`).
    #[must_use]
    pub fn local_overlay_holds(&self, pane: Option<OverlayPane>) -> bool {
        match pane {
            None => self.cover_overlay_active(),
            Some(pane) => self.pane_overlay(pane).is_some(),
        }
    }
}

impl AppStore {
    /// Reserves the slot `slot.pane` of session `session` for a remote job and clears that slot's last result, as a
    /// local open clears it. False for an unknown session.
    pub fn reserve_remote_overlay(&mut self, session: Uuid, slot: RemoteOverlaySlot) -> bool {
        let Some(found) = self.session_mut(session) else {
            return false;
        };
        let pane = slot.pane;
        found.remote_overlays.reserve(slot);
        match pane {
            Some(pane) => found.pane_overlays.slot_mut(pane).exit_code = None,
            None => found.overlay.exit_code = None,
        }
        true
    }

    /// Records a remote job's first outcome into its slot's result, and frees the slot unless a held `--wait` surface
    /// still occupies it on the viewer (upstream's `finishRemoteOverlay`).
    pub fn finish_remote_overlay(&mut self, session: Uuid, job: &str, outcome: &RemoteOverlayOutcome) {
        let Some(found) = self.session_mut(session) else {
            return;
        };
        let Some(slot) = found.remote_overlays.end(job) else {
            return;
        };
        match outcome {
            RemoteOverlayOutcome::Exited(code) => match slot.pane {
                Some(pane) => found.pane_overlays.slot_mut(pane).exit_code = Some(*code),
                None => found.overlay.exit_code = Some(*code),
            },
            RemoteOverlayOutcome::Failed(name) => found.remote_overlays.record_failure(name, slot.pane),
        }
    }

    /// Closes the remote overlay on a slot (upstream's `closeRemoteOverlay`): its surface counts as gone and the core
    /// is asked to take the presenter's surface down and cancel the job. It reports the request, not the program's end.
    /// False when no viewer holds the slot.
    pub fn close_remote_overlay(&mut self, session: Uuid, pane: Option<OverlayPane>) -> bool {
        let Some(found) = self.session_mut(session) else {
            return false;
        };
        let Some(job) = found.remote_overlays.slot(pane).map(|slot| slot.job.clone()) else {
            return false;
        };
        found.remote_overlays.surface_gone(&job);
        self.remote_overlay_closes.push(RemoteOverlayClose { session, job });
        true
    }

    /// The viewer closed its surface for `job`, so a slot held only for that surface is freed.
    pub fn remote_overlay_surface_closed(&mut self, session: Uuid, job: &str) {
        if let Some(found) = self.session_mut(session) {
            found.remote_overlays.surface_gone(job);
        }
    }

    /// Records the size asked of a session-wide remote overlay.
    pub fn resize_remote_overlay(&mut self, session: Uuid, job: &str, size_percent: Option<i64>) {
        if let Some(found) = self.session_mut(session) {
            found.remote_overlays.resize(job, size_percent);
        }
    }

    /// The viewer presenting `session` is gone, and every overlay it held goes with it: each slot's surface counts as
    /// gone. The jobs still unclaimed, which the caller cancels, are returned.
    pub fn remote_overlay_presenter_lost(&mut self, session: Uuid) -> Vec<String> {
        let Some(found) = self.session_mut(session) else {
            return Vec::new();
        };
        let jobs: Vec<String> = found
            .remote_overlays
            .slots()
            .iter()
            .map(|slot| slot.job.clone())
            .collect();
        for job in &jobs {
            found.remote_overlays.surface_gone(job);
        }
        jobs
    }

    /// The presenter refused to show `job`: no surface was ever shown, so a `--wait` slot has nothing to hold it.
    /// Whether the session holds that job.
    pub fn reject_remote_overlay(&mut self, session: Uuid, job: &str) -> bool {
        let Some(found) = self.session_mut(session) else {
            return false;
        };
        if found.remote_overlays.slot_of_job(job).is_none() {
            return false;
        }
        found.remote_overlays.surface_gone(job);
        true
    }

    /// `pane` is gone here: its job is ended and its slot and result dropped, since nothing is left for a late outcome
    /// to describe (upstream's `dropRemoteOverlay`).
    pub(crate) fn drop_remote_overlay(&mut self, session: Uuid, pane: OverlayPane) {
        let job = self
            .session(session)
            .and_then(|found| found.remote_overlays.slot(Some(pane)))
            .map(|slot| slot.job.clone());
        if let Some(job) = job {
            self.close_remote_overlay(session, Some(pane));
            if let Some(found) = self.session_mut(session) {
                found.remote_overlays.remove(&job);
            }
        }
        if let Some(found) = self.session_mut(session) {
            found.remote_overlays.clear_failure(Some(pane));
        }
    }

    /// Ends every overlay `session` handed out before it leaves the store, so an undo brings back no reservation.
    pub(crate) fn release_remote_overlays(&mut self, session: Uuid) {
        let panes: Vec<Option<OverlayPane>> = self
            .session(session)
            .map(|found| found.remote_overlays.slots().iter().map(|slot| slot.pane).collect())
            .unwrap_or_default();
        for pane in panes {
            self.close_remote_overlay(session, pane);
        }
        if let Some(found) = self.session_mut(session) {
            found.remote_overlays = RemoteOverlays::default();
        }
    }

    /// Takes the remote overlay closes since the last take, in order.
    pub fn take_remote_overlay_closes(&mut self) -> Vec<RemoteOverlayClose> {
        std::mem::take(&mut self.remote_overlay_closes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(job: &str, pane: Option<OverlayPane>, wait: bool) -> RemoteOverlaySlot {
        RemoteOverlaySlot {
            job: job.to_owned(),
            pane,
            owner: 1,
            size_percent: None,
            wait,
            ended: false,
        }
    }

    #[test]
    fn a_waiting_slot_outlives_its_program_until_its_surface_goes() {
        let mut overlays = RemoteOverlays::default();
        overlays.reserve(slot("a", None, true));
        assert!(overlays.end("a").is_some());
        assert!(
            overlays.slot(None).is_some_and(|slot| slot.ended),
            "held for the surface"
        );
        overlays.surface_gone("a");
        assert!(overlays.slot(None).is_none());
        overlays.reserve(slot("b", None, true));
        overlays.surface_gone("b");
        assert!(overlays.slot(None).is_some(), "a running job keeps its slot");
        overlays.end("b");
        assert!(
            overlays.slot(None).is_none(),
            "freed when it ends, the surface already gone"
        );
        assert!(overlays.end("missing").is_none());
    }

    #[test]
    fn slots_and_results_follow_swaps_and_promotions() {
        let mut overlays = RemoteOverlays::default();
        overlays.reserve(slot("l", Some(OverlayPane::Left), false));
        overlays.record_failure("canceled", Some(OverlayPane::Right));
        overlays.swap_panes();
        assert_eq!(
            overlays.slot(Some(OverlayPane::Right)).map(|slot| slot.job.as_str()),
            Some("l")
        );
        assert_eq!(overlays.failure(Some(OverlayPane::Left)), Some("canceled"));
        overlays.promote_right();
        assert_eq!(
            overlays.slot(Some(OverlayPane::Left)).map(|slot| slot.job.as_str()),
            Some("l")
        );
        assert_eq!(overlays.failure(Some(OverlayPane::Left)), None);
        overlays.record_failure("unknown", None);
        overlays.reserve(slot("n", None, false));
        assert_eq!(overlays.failure(None), None, "a new reservation starts a new result");
    }
}
