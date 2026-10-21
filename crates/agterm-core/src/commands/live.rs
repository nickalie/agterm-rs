//! Live sessions in the app (upstream's `agterm/Control/ControlServer+Zmx.swift`, `ControlDispatcher+Zmx.swift`, the
//! zmx wiring of `agtermApp.swift`, `Ghostty/ZmxLaunch.swift`, `ZmxForegroundResolver.swift`, `PaneLead.swift` and the
//! Live sessions reset): the frontend installs a [`SessionHost`] over the staged session server and the wrap inputs,
//! and this module owns everything the core decides with them.
//!
//! - the runtime (here): what the frontend installed, the launch reap's findings, the pane finalizer that ends a
//!   removed pane's daemon, and the requests a command leaves for the frontend (a pane replaced in place, a quit);
//! - [`claims`]: every pane expecting a daemon, open, pending-close, closed and unindexed windows alike;
//! - [`inventory`]: `live.list`, `live.prune`, `live.kill`, `live.screen`;
//! - [`lead`]: the lead book fed by the panes' role reports, the covered panes' reads and typing through the daemon,
//!   `session.lead` and the reattach;
//! - [`launch`]: a pane's launch disposition and the live seed, the leader cache behind a live pane's foreground, and
//!   the launch reap;
//! - [`live_reset`]: `live.reset`, its marker at quit and its consumption at the next launch.

mod claims;
mod inventory;
pub(crate) mod launch;
mod lead;
mod live_reset;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use agterm_host::SessionHost;
use agterm_proto::command::LiveCmd;
use agterm_proto::{Command, ControlLiveEndpoint};
use uuid::Uuid;

pub(crate) use self::inventory::{control_inventory, dispatch};
pub use self::lead::LeadCover;
pub(crate) use self::lead::{LeadBook, PaneSource};
pub use self::live_reset::{LiveQuit, ResetPrompt, ResetRefusal};
use crate::Core;
use crate::error::ControlError;
use crate::live::daemon_name;
use crate::live::lead::{LeadAttachment, LeadState};
use crate::live::reset::{MarkerStore, Outcome, Selection};
use crate::live::support::Inputs;

/// What the frontend hands the core once, before the first terminal: the daemons' host and what a wrap runs with.
pub struct LiveInstall {
    pub host: Arc<dyn SessionHost>,
    /// The session host executable and the namespace every invocation runs in.
    pub endpoint: ControlLiveEndpoint,
    /// The wrap inputs every pane's configuration extends (the pane identity, base environment and lead are filled
    /// per pane); the session host executable, the login shell and the integration are checked again per pane.
    pub wrap: Inputs,
    /// The state directory the reset marker lives in.
    pub state_dir: PathBuf,
    /// The launch's session host build cutoff (upstream's `ZmxBuildRecord`), whole seconds since the epoch.
    pub outdated_before: Option<u64>,
}

impl fmt::Debug for LiveInstall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LiveInstall")
            .field("endpoint", &self.endpoint)
            .field("state_dir", &self.state_dir)
            .finish_non_exhaustive()
    }
}

/// A pane's terminal the frontend replaces in place, keeping its pane token: the fresh attach of a lead takeover or a
/// restart (upstream's `PaneReattach` through `PaneLead.replace`). It runs `command` without holding its exit, with
/// `env` over the pane's own environment, in `cwd` (the pane's when `None`). The replaced terminal's exit closes
/// nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneReplacement {
    pub session: Uuid,
    pub token: String,
    pub command: String,
    /// A live pane's attach as data, which the replacement runs in process in place of `command` (its shell line).
    pub attach: Option<crate::live::support::LiveAttach>,
    pub env: Vec<(String, String)>,
    pub cwd: Option<String>,
    /// A remote replica's ssh attach rather than a live-session client: its terminal holds after the command exits,
    /// as the attach pane's own does, so its last line stays readable.
    pub remote: bool,
}

/// The installed live-session host and what the core keeps about its daemons.
#[derive(Default)]
pub(crate) struct LiveRuntime {
    host: Option<Arc<dyn SessionHost>>,
    endpoint: Option<ControlLiveEndpoint>,
    wrap: Option<Inputs>,
    markers: Option<MarkerStore>,
    /// The launch's session host build cutoff.
    pub(crate) outdated_before: Option<u64>,
    /// Panes whose reset could not be confirmed at this launch: they attach with no replay and no durable command.
    pub(crate) suppressed: HashSet<Uuid>,
    /// Each pane's lead as its session host reports it.
    pub(crate) leads: LeadBook,
    /// The daemons' leader pids behind the live panes' foreground reads.
    pub(crate) leaders: RefCell<launch::LeaderCache>,
    /// The Live sessions reset confirmed this run, and the outcome the launch's consumption left.
    pub(crate) reset: live_reset::ResetState,
    /// A pane whose daemon a command already destroyed: its removal must not ask the host to kill a name that is gone.
    already_finalized: HashSet<Uuid>,
    /// Pane tokens whose shell exit a restart claimed: the dying client must not close the pane it rebuilds.
    pub(crate) claimed_exits: HashSet<String>,
    /// Pane terminals commands asked the frontend to replace, in order.
    replacements: Vec<PaneReplacement>,
    /// The lead state each pane in a replacement had before it, by pane token, until the frontend reports the
    /// replacement installed or dropped: a dropped one puts it back, so the pane is not left covered for good.
    superseded: HashMap<String, Option<LeadState>>,
    /// A quit a command asked for once its reply is out.
    quit: Option<LiveQuit>,
}

impl fmt::Debug for LiveRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LiveRuntime")
            .field("endpoint", &self.endpoint)
            .field("replacements", &self.replacements)
            .finish_non_exhaustive()
    }
}

impl LiveRuntime {
    /// The installed host; `None` when this instance has no session host.
    pub(crate) fn host(&self) -> Option<&Arc<dyn SessionHost>> {
        self.host.as_ref()
    }

    /// The installed host, or the refusal every `live.*` command gives without one.
    fn require_host(&self) -> Result<Arc<dyn SessionHost>, ControlError> {
        self.host.clone().ok_or_else(|| ControlError::rejected(UNAVAILABLE))
    }
}

/// The refusal of a `live.*` command in an instance without a session host (upstream's `ControlZmxError.unavailable`).
pub(crate) const UNAVAILABLE: &str = "live sessions are unavailable in this instance";

impl Core {
    /// Installs the live-session host and the wrap inputs, before the first terminal (upstream's `restoredRuntime`).
    pub fn install_live_sessions(&mut self, install: LiveInstall) {
        self.live.markers = Some(MarkerStore::new(&install.state_dir));
        self.live.host = Some(install.host);
        self.live.endpoint = Some(install.endpoint);
        self.live.wrap = Some(install.wrap);
        self.live.outdated_before = install.outdated_before;
    }

    /// The pane terminals commands asked to replace since the last take, in order.
    pub fn take_pane_replacements(&mut self) -> Vec<PaneReplacement> {
        std::mem::take(&mut self.live.replacements)
    }

    /// The quit a command asked for, once: the frontend quits after the command's reply went out.
    pub fn take_live_quit(&mut self) -> Option<LiveQuit> {
        self.live.quit.take()
    }

    pub(crate) fn request_pane_replacement(&mut self, replacement: PaneReplacement) {
        self.live.replacements.push(replacement);
    }

    /// Starts the attachment a replacement of the pane carrying `token` runs with, keeping what it replaces until the
    /// frontend reports the replacement ([`Core::pane_replaced`], [`Core::replacement_dropped`]).
    pub(crate) fn begin_replacement_lead(
        &mut self,
        token: &str,
        identity: Uuid,
        attachment: LeadAttachment,
        reattaching: bool,
    ) {
        let previous = self.live.leads.snapshot(identity);
        self.live.superseded.entry(token.to_owned()).or_insert(previous);
        self.live.leads.begin(identity, attachment, reattaching);
    }

    /// The frontend installed `replacement`'s terminal, claiming the replaced one's exit itself: a restart's claim on
    /// the pane token ends here, so the new terminal's own exit closes the pane (upstream claims per surface).
    pub fn pane_replaced(&mut self, replacement: &PaneReplacement) {
        self.live.superseded.remove(&replacement.token);
        self.live.claimed_exits.remove(&replacement.token);
    }

    /// The frontend could not install `replacement`'s terminal (no page holds the pane): the pane keeps the lead state
    /// it had, and a pane whose shell a restart already ended is closed by the exit the restart claimed, as upstream's
    /// `closeEndedPane`. Whether the model changed; commit afterwards.
    pub fn replacement_dropped(&mut self, replacement: &PaneReplacement) -> bool {
        if let (Some(previous), Ok(identity)) = (
            self.live.superseded.remove(&replacement.token),
            Uuid::parse_str(&replacement.token),
        ) {
            self.live.leads.restore(identity, previous);
        }
        if !self.live.claimed_exits.remove(&replacement.token) {
            return false;
        }
        if let Ok(identity) = Uuid::parse_str(&replacement.token) {
            self.note_already_finalized(identity);
        }
        self.pane_exited(replacement.session, &replacement.token)
    }

    pub(crate) fn request_live_quit(&mut self, quit: LiveQuit) {
        self.live.quit = Some(quit);
    }

    /// Ends the daemons of pane identities the model removed for good (upstream's `paneFinalizer`), with their lead
    /// state; a pane a command already finalized is skipped once. Runs on every commit.
    pub(crate) fn finalize_panes(&mut self, panes: Vec<Uuid>) {
        if panes.is_empty() {
            return;
        }
        for pane in &panes {
            self.live.leads.forget(*pane);
        }
        let names: Vec<String> = panes
            .into_iter()
            .filter(|pane| !self.live.already_finalized.remove(pane))
            .map(daemon_name)
            .collect();
        self.live.leaders.borrow_mut().note_lifecycle_change();
        let Some(host) = self.live.host.clone() else {
            return;
        };
        if !names.is_empty() && !host.kill_batch(&names) {
            tracing::warn!(
                names = names.join(","),
                "the removed panes' daemons could not all be ended"
            );
        }
    }

    /// Marks `pane` as finalized by the caller, which destroyed its daemon already.
    pub(crate) fn note_already_finalized(&mut self, pane: Uuid) {
        self.live.already_finalized.insert(pane);
    }
}

/// The commands of the `live.*` group this build answers at once; `live.tree` starts through `deferred`, and
/// `live.attach` comes with the viewer side of remote sessions.
pub(crate) fn answers(cmd: LiveCmd) -> bool {
    matches!(
        cmd,
        LiveCmd::List | LiveCmd::Prune | LiveCmd::Kill | LiveCmd::Screen | LiveCmd::Reset | LiveCmd::Present
    )
}

/// The refusal of a `live.*` command this build does not answer yet.
pub(crate) fn not_yet(cmd: LiveCmd) -> ControlError {
    ControlError::NotSupported(Command::Live(cmd))
}

/// The selection a confirmed reset holds and the outcome the launch left, for the tree and `live.list`.
pub(crate) fn readback(pending: Option<&Selection>, last: Option<&Outcome>) -> Option<serde_json::Value> {
    if pending.is_none() && last.is_none() {
        return None;
    }
    let mut object = serde_json::Map::new();
    if let Some(selection) = pending {
        object.insert("pending".to_owned(), serde_json::json!(selection.targets.len()));
    }
    if let Some(outcome) = last {
        object.insert("last".to_owned(), serde_json::to_value(outcome).ok()?);
    }
    Some(serde_json::Value::Object(object))
}
