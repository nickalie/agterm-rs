//! Reset Live Sessions and `live.reset` (upstream's `LiveResetCoordinator.swift`, `Ghostty/LiveResetConsumer.swift`
//! and the marker arming in `AppDelegate`): the one confirm path refuses in a fixed order and holds the selection for
//! the quit; the quit writes the one-shot marker and the app reopens itself; the next launch that gets live consumes
//! the marker before any kill, narrows it against its own claims and listing, ends the confirmed daemons in one batch
//! and waits for their leaders. Panes whose old leader survived start neither their replay nor their durable command.
//!
//! Linux has no responsibility API to tell a daemon's supervising process (upstream's `LiveAttributionProbe` over
//! `responsibility_get_pid_responsible_for_pid`) and no session host, so every leader attributes as unknown: a reset
//! offers the daemons that predate the recorded zmx build, never an "unsupervised" one.

use std::collections::BTreeSet;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use agterm_host::SessionHost;
use agterm_proto::{Args, ControlLiveResetStatus, ControlResult};

use crate::Core;
use crate::commands::Outcome;
use crate::error::ControlError;
use crate::live::reset::{
    Attribution, Marker, Outcome as ResetOutcome, Selection, dialog_text, menu_visible, narrow, notification_text,
    outcome, select,
};

/// How long a launch may spend consuming a reset before every selected pane stays suppressed.
const CONSUME_BUDGET: Duration = Duration::from_secs(15);
/// How often a killed leader is polled for its exit.
const LEADER_POLL: Duration = Duration::from_millis(100);

/// The reset this run confirmed, held until the quit, and the outcome the launch's consumption left.
#[derive(Debug)]
pub(crate) struct ResetState {
    pub(crate) pending: Option<Selection>,
    pub(crate) last: Option<ResetOutcome>,
    /// How long the launch's consumption may wait for the killed leaders ([`CONSUME_BUDGET`]; shorter in tests).
    pub(crate) consume_budget: Duration,
}

impl Default for ResetState {
    fn default() -> Self {
        Self {
            pending: None,
            last: None,
            consume_budget: CONSUME_BUDGET,
        }
    }
}

/// Why a reset is refused, in the order the checks run (upstream's `LiveResetCoordinator.Refusal`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetRefusal {
    NotLive,
    ListingFailed,
    InventoryIncomplete,
    NothingToReset,
}

impl ResetRefusal {
    /// The control error.
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::NotLive => "live.reset requires Live sessions mode both configured and active for this launch",
            Self::ListingFailed => "live.reset could not read the live session list",
            Self::InventoryIncomplete => "live.reset refused: the pane inventory is incomplete",
            Self::NothingToReset => "live.reset found no live session to reset",
        }
    }

    /// What the menu item shows.
    #[must_use]
    pub fn user_message(self) -> &'static str {
        match self {
            Self::NotLive => "Reset Live Sessions needs Live sessions mode for this launch and the next.",
            Self::ListingFailed => "The live session list could not be read. Nothing was reset.",
            Self::InventoryIncomplete => "The saved sessions could not be verified. Nothing was reset.",
            Self::NothingToReset => "No live sessions could be selected for reset.",
        }
    }
}

/// A quit a command asked for once its reply is out; `relaunch` reopens the app after this process exited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveQuit {
    pub relaunch: bool,
}

/// What the reset dialog asks: its title and body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetPrompt {
    pub title: String,
    pub body: String,
}

/// What every leader attributes as on Linux (module docs).
pub(crate) const LINUX_ATTRIBUTION: Attribution = Attribution::Unknown;

fn classify(_name: &str, _leader: i32) -> Attribution {
    LINUX_ATTRIBUTION
}

impl Core {
    /// Whether Reset Live Sessions… is offered: live both configured and running.
    #[must_use]
    pub fn live_reset_menu_visible(&self) -> bool {
        menu_visible(self.settings().effective_restore_mode(), self.restore.decision.active)
    }

    /// The panes a reset would end; `None` when the listing failed.
    fn live_reset_selection(&self) -> Option<Selection> {
        let host = self.live.host()?;
        let records = host.list().ok()?;
        Some(select(
            &self.pane_claims(),
            &records,
            self.live.outdated_before,
            classify,
        ))
    }

    /// The menu item's first step (upstream's `request(confirmed: false)` up to the dialog): the refusal, or the prompt
    /// to confirm with [`Core::confirm_live_reset`].
    ///
    /// # Errors
    ///
    /// The first [`ResetRefusal`] that applies.
    pub fn prepare_live_reset(&self) -> Result<ResetPrompt, ResetRefusal> {
        let selection = self.checked_live_reset_selection()?;
        let (title, body) = dialog_text(selection.session_count(), selection.outdated_session_count());
        Ok(ResetPrompt { title, body })
    }

    /// The dialog confirmed: selects again and holds the selection for the quit, which the caller then requests.
    ///
    /// # Errors
    ///
    /// The first [`ResetRefusal`] that applies now.
    pub fn confirm_live_reset(&mut self) -> Result<(), ResetRefusal> {
        let selection = self.checked_live_reset_selection()?;
        self.live.reset.pending = Some(selection);
        Ok(())
    }

    fn checked_live_reset_selection(&self) -> Result<Selection, ResetRefusal> {
        if !self.live_reset_menu_visible() {
            return Err(ResetRefusal::NotLive);
        }
        let selection = self.live_reset_selection().ok_or(ResetRefusal::ListingFailed)?;
        if !selection.inventory_complete {
            return Err(ResetRefusal::InventoryIncomplete);
        }
        if selection.targets.is_empty() {
            return Err(ResetRefusal::NothingToReset);
        }
        Ok(selection)
    }

    /// The quit path (upstream's `applicationWillTerminate` arming): writes the confirmed selection as the marker the
    /// next launch consumes, while live is still both configured and running; a mode changed since makes the next
    /// launch unable to suppress survivors, so nothing is armed then. Whether the app must reopen itself.
    pub fn arm_live_reset_on_quit(&mut self) -> bool {
        let Some(selection) = self.live.reset.pending.take() else {
            return false;
        };
        if !self.live_reset_menu_visible() {
            return false;
        }
        let Some(markers) = &self.live.markers else {
            return false;
        };
        match markers.write(&Marker::new(selection.targets, SystemTime::now())) {
            Ok(()) => true,
            Err(error) => {
                tracing::error!(%error, "could not write the live sessions reset marker");
                false
            }
        }
    }

    /// The quit's relauncher did not start (upstream's `armLiveReset` after a failed spawn): the marker armed by
    /// [`Self::arm_live_reset_on_quit`] is removed, so the next launch, started by hand, kills nothing.
    pub fn disarm_live_reset(&self) {
        if let Some(markers) = &self.live.markers {
            markers.remove();
        }
        tracing::error!("live sessions reset not armed: the relauncher did not start");
    }

    /// The launch half (upstream's `LiveResetConsumer.run`): no marker, or one that does not decode, kills nothing.
    pub(crate) fn consume_live_reset(&mut self, host: &dyn SessionHost) {
        let Some(markers) = self.live.markers.clone() else {
            return;
        };
        let marker = match markers.consume() {
            Ok(Some(marker)) => marker,
            Ok(None) => return,
            Err(error) => {
                tracing::error!(%error, "live sessions reset marker discarded");
                return;
            }
        };
        let deadline = Instant::now() + self.live.reset.consume_budget;
        let walk = self.pane_claims();
        let claimed: Option<BTreeSet<uuid::Uuid>> = walk
            .complete
            .then(|| walk.claims.iter().map(|claim| claim.pane_identity).collect());
        let records = host.list().ok();
        let narrowed = narrow(
            &marker,
            claimed.as_ref(),
            records.as_deref(),
            self.live.outdated_before,
            classify,
        );
        let kill = narrowed.kill();
        let mut survivors = BTreeSet::new();
        if !kill.is_empty() {
            let names: Vec<String> = kill.iter().map(|target| target.daemon.clone()).collect();
            if !host.kill_batch(&names) {
                tracing::error!("live sessions reset: the batched kill did not complete; polling every leader anyway");
            }
            survivors = self.leaders_alive_after(kill.iter().map(|target| target.leader_pid).collect(), deadline);
        }
        let result = outcome(&narrowed, &survivors, narrowed.inventory_failed);
        self.live.suppressed = result.unconfirmed.iter().copied().collect();
        markers.remove_consumed();
        tracing::info!(
            killed = result.panes.killed,
            gone = result.panes.gone,
            skipped = result.panes.skipped,
            unconfirmed = result.unconfirmed.len(),
            "live sessions reset"
        );
        if let Some(body) = notification_text(&result) {
            self.post_app_banner("live-reset", "Live Sessions", body);
        }
        self.live.reset.last = Some(result);
    }

    /// The pids of `leaders` still alive at `deadline`, polled every [`LEADER_POLL`]; every one without a process table
    /// to read, so a survivor is never taken for gone.
    fn leaders_alive_after(&self, mut leaders: BTreeSet<i32>, deadline: Instant) -> BTreeSet<i32> {
        let Some(table) = self.restore.table() else {
            return leaders;
        };
        loop {
            leaders.retain(|pid| table.stat(*pid).is_some());
            if leaders.is_empty() || Instant::now() >= deadline {
                return leaders;
            }
            thread::sleep(LEADER_POLL);
        }
    }
}

/// `live.reset`: the dialog's confirm path without the dialog. The quit is requested for once the reply is out.
pub(crate) fn reset(core: &mut Core, args: &Args) -> Outcome {
    if args.force != Some(true) {
        return Err(ControlError::rejected("live.reset requires --force"));
    }
    if core.live.host().is_none() {
        return Err(ControlError::rejected("live.reset is not supported on this platform"));
    }
    let selection = core
        .checked_live_reset_selection()
        .map_err(|refusal| ControlError::rejected(refusal.message()))?;
    let outdated = selection.outdated_session_count();
    let count = |value: usize| u32::try_from(value).unwrap_or(u32::MAX);
    let status = ControlLiveResetStatus {
        sessions: count(selection.session_count()),
        panes: count(selection.targets.len()),
        pending: true,
        outdated: (outdated > 0).then(|| count(outdated)),
    };
    let (_, body) = dialog_text(selection.session_count(), outdated);
    core.live.reset.pending = Some(selection);
    core.request_live_quit(LiveQuit { relaunch: true });
    Ok(ControlResult {
        text: Some(body),
        live_reset: Some(status),
        ..ControlResult::default()
    })
}
