//! The app side of restore modes (upstream's `GhosttyApp.restoreLaunchDecision`, `restoreDenylist`, the
//! `SpawnRegistry` and `LaunchSeedProvider` wiring in `agtermApp.swift`, and `AppDelegate`'s exit captures): the
//! launch decision and denylist read once, the pane spawns the frontend asks permission for, the paced grants it
//! collects after each settle, and the foreground captures at quit and at the last window's close.
//!
//! The frontend creates a session pane's terminal only once the core handed it the pane's [`LaunchSeed`]: it asks
//! with [`Core::request_pane_spawn`] when the terminal is ready, gets the seed at once or later through
//! [`Core::take_spawn_grants`], and arms one main-loop timer from [`Core::take_spawn_wake`] for the next paced grant.

use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::time::{Duration, Instant};

use agterm_host::ProcessTable;
use uuid::Uuid;

use crate::Core;
use crate::command_restore::{DENYLIST_FILE, DENYLIST_STARTER, parse_denylist};
use agterm_proto::ControlTree;

use crate::command_restore::PaneForeground;
use crate::commands::zmx::launch::{live_foreground_group, pane_paces, pane_seed};
use crate::model::{AppStore, Session, StatusPane, SurfaceSlot, WindowLibrary};
use crate::restore::capture::{CaptureKind, ForegroundReader, capture_foreground_commands};
use crate::restore::mode::{RestoreLaunchDecision, captures_on_exit};
use crate::restore::pacer::{DEFAULT_INTERVAL, SpawnPacer};
use crate::restore::spawn_plan::{LaunchSeed, LaunchSeedPolicy, LaunchSpawnPlan};

/// How long an exit capture may read processes before the rest of the panes keep what they had (upstream's
/// `exitCaptureBudget`).
pub const EXIT_CAPTURE_BUDGET: Duration = Duration::from_millis(500);

/// A paced pane the launch now lets spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneSpawn {
    pub session: Uuid,
    /// The pane's token, which its terminal carries.
    pub token: String,
    pub seed: LaunchSeed,
}

/// The restore state of the running app.
pub(crate) struct RestoreRuntime {
    pub(crate) decision: RestoreLaunchDecision,
    denylist: HashSet<String>,
    table: Option<Box<dyn ProcessTable>>,
    /// The user's `$SHELL` basename.
    shell: Option<String>,
    pacer: SpawnPacer,
    /// Keys the pacer granted since the frontend last collected them.
    grants: Vec<Uuid>,
    /// Panes whose terminal asked, or was granted, while the pane could not show: a hidden split, or a session a
    /// pending close holds. Upstream unmounts such a pane and its remounted view asks again; here the terminal stays
    /// and asks only once, so the pane is granted when it shows again, and forgotten once it leaves the model.
    held: HashSet<Uuid>,
}

impl Default for RestoreRuntime {
    fn default() -> Self {
        Self {
            decision: RestoreLaunchDecision::default(),
            denylist: HashSet::new(),
            table: None,
            shell: None,
            pacer: SpawnPacer::new(DEFAULT_INTERVAL),
            grants: Vec::new(),
            held: HashSet::new(),
        }
    }
}

impl fmt::Debug for RestoreRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RestoreRuntime")
            .field("decision", &self.decision)
            .field("denylist", &self.denylist)
            .field("shell", &self.shell)
            .finish_non_exhaustive()
    }
}

impl RestoreRuntime {
    pub(crate) fn policy(&self) -> LaunchSeedPolicy {
        LaunchSeedPolicy {
            restore_enabled: self.decision.replays(),
            live_fell_back: self.decision.live_fell_back(),
            denylist: self.denylist.clone(),
            ..LaunchSeedPolicy::default()
        }
    }

    /// The frontend's process table; `None` until one is installed.
    pub(crate) fn table(&self) -> Option<&dyn ProcessTable> {
        self.table.as_deref()
    }

    /// The user's `$SHELL` basename.
    pub(crate) fn shell(&self) -> Option<&str> {
        self.shell.as_deref()
    }

    /// The restore denylist read at launch.
    pub(crate) fn denylist(&self) -> &HashSet<String> {
        &self.denylist
    }
}

/// A reader over `table` that reads a live pane's foreground through its daemon's shell.
fn reader_with_live<'a>(
    table: &'a dyn ProcessTable,
    shell: Option<&'a str>,
    live: &'a dyn Fn(&str) -> Option<i32>,
) -> ForegroundReader<'a> {
    ForegroundReader::new(table, shell).with_live(live)
}

impl Core {
    /// Records the restore decision of this launch, taken before the first terminal; it never changes for the process.
    pub fn set_restore_launch(&mut self, decision: RestoreLaunchDecision) {
        self.restore.decision = decision;
    }

    /// The restore decision this launch runs under.
    #[must_use]
    pub fn restore_launch(&self) -> &RestoreLaunchDecision {
        &self.restore.decision
    }

    /// Installs the frontend's process table and the user's `$SHELL` basename, which the `tree`'s foreground fields
    /// and the restore captures read.
    pub fn set_process_table(&mut self, table: Box<dyn ProcessTable>, shell: Option<String>) {
        self.restore.table = Some(table);
        self.restore.shell = shell;
    }

    /// Seeds a commented starter `restore-denylist.conf` when the config directory has none, then reads it (upstream's
    /// `ensureStarterRestoreDenylist` and `loadRestoreDenylist`). Read at launch only; a missing or unreadable file is
    /// an empty denylist.
    pub fn load_restore_denylist_at_launch(&mut self) {
        let path = self.config_dir().join(DENYLIST_FILE);
        if !path.exists() {
            let written = path
                .parent()
                .map_or(Ok(()), fs::create_dir_all)
                .and_then(|()| fs::write(&path, DENYLIST_STARTER));
            if let Err(error) = written {
                tracing::info!(path = %path.display(), %error, "could not write the starter restore-denylist.conf");
            }
        }
        let text = fs::read_to_string(&path).unwrap_or_default();
        self.restore.denylist = parse_denylist(&text).into_iter().collect();
    }

    /// Arms the launch spawn order over the restored model, before any window shows (upstream's
    /// `pacer.arm(order:burst:)` at app init): the selected sessions' panes spawn at once, the rest one per interval.
    pub fn arm_launch_spawns(&mut self, now: Instant) {
        let plan = LaunchSpawnPlan::of(self.library());
        tracing::debug!(
            panes = plan.order.len(),
            burst = plan.burst.len(),
            "launch spawn queue armed"
        );
        self.restore.pacer.arm(plan.order, &plan.burst, now);
    }

    /// The terminal of `session`'s pane carrying `token` is ready to spawn: its seed when it may spawn now, consuming
    /// the slots the launch armed for it; `None` while it waits for its paced turn, which
    /// [`Core::take_spawn_grants`] later hands back. A pane that replays nothing leaves the queue and spawns at once;
    /// one a pending close holds waits for the undo; one no session holds any more spawns a plain shell.
    pub fn request_pane_spawn(&mut self, session: Uuid, token: &str, now: Instant) -> Option<LaunchSeed> {
        let seed = self.answer_pane_spawn(session, token, now);
        self.log_spawn_drain(now);
        seed
    }

    fn answer_pane_spawn(&mut self, session: Uuid, token: &str, now: Instant) -> Option<LaunchSeed> {
        let policy = self.seed_policy();
        let Some((role, key, disposed, paces)) = self.find_session(session).and_then(|session| {
            let role = session.pane_role_for_token(token)?;
            let key = pane_key(session, role)?;
            let disposed = self.pane_disposition(Some(key), session.remote_host().is_none());
            let paces = pane_paces(session, role, &policy, &disposed);
            Some((role, key, disposed, paces))
        }) else {
            // a session a pending close holds spawns on its undo, as upstream's remounted view would
            if let Some(key) = self.pending_close_pane_key(session, token) {
                self.restore.held.insert(key);
                return None;
            }
            return Some(LaunchSeed::default());
        };
        if !paces {
            self.restore.pacer.discard(key);
        } else if !self.restore.pacer.request(key, now) {
            return None;
        }
        let window = self.library.window_holding_session(session)?;
        let session = self.library.store_mut(window)?.session_mut(session)?;
        Some(pane_seed(&mut self.zmx, session, role, &policy, disposed))
    }

    /// Lets the queued panes of `session` spawn now (upstream's `expediteSpawn`): the session is on screen, or a command
    /// waits on its terminal. A no-op once the launch queue drained.
    pub fn expedite_pane_spawns(&mut self, session: Uuid, now: Instant) {
        if self.restore.pacer.is_passthrough() {
            return;
        }
        let keys: Vec<Uuid> = self
            .find_session(session)
            .map(|session| {
                [StatusPane::Left, StatusPane::Right]
                    .into_iter()
                    .filter_map(|role| pane_key(session, role))
                    .collect()
            })
            .unwrap_or_default();
        for key in keys {
            if self.restore.pacer.expedite(key, now) {
                self.restore.grants.push(key);
            }
        }
        self.log_spawn_drain(now);
    }

    /// The panes granted since the last call, with their seeds; a granted pane whose session or slot is gone is
    /// dropped.
    pub fn take_spawn_grants(&mut self) -> Vec<PaneSpawn> {
        let policy = self.seed_policy();
        let keys = std::mem::take(&mut self.restore.grants);
        let mut spawns = Vec::new();
        for key in keys {
            let Some(id) = self.find_session_by_pane(key) else {
                // a pane a pending close holds gets its grant back on the undo
                if pane_hold(self.library(), key) == PaneHold::Held {
                    self.restore.held.insert(key);
                }
                continue;
            };
            let Some(local) = self.find_session(id).map(|session| session.remote_host().is_none()) else {
                continue;
            };
            let disposed = self.pane_disposition(Some(key), local);
            let Some(window) = self.library.window_holding_session(id) else {
                continue;
            };
            let Some(session) = self.library.store_mut(window).and_then(|store| store.session_mut(id)) else {
                continue;
            };
            let role = if session.pane_identity == key {
                StatusPane::Left
            } else {
                StatusPane::Right
            };
            let Some(token) = session
                .pane_slot(role)
                .get()
                .map(|surface| surface.pane_token().to_owned())
            else {
                continue;
            };
            spawns.push(PaneSpawn {
                session: id,
                token,
                seed: pane_seed(&mut self.zmx, session, role, &policy, disposed),
            });
        }
        spawns
    }

    /// The delay after which [`Core::spawn_wake`] can grant the next queued pane, once per outstanding wake: `None`
    /// while a wake is already armed or nothing waits.
    pub fn take_spawn_wake(&mut self, now: Instant) -> Option<Duration> {
        self.restore.pacer.take_wake_request(now)
    }

    /// The armed wake fired: grants the queue's head when its interval passed.
    pub fn spawn_wake(&mut self, now: Instant) {
        if let Some(key) = self.restore.pacer.wake(now) {
            self.restore.grants.push(key);
        }
        self.log_spawn_drain(now);
    }

    /// Drops the queued keys of panes that cannot show (upstream's `launchPaneDrop`): a pane no open window holds, a
    /// hidden split, a session a pending close holds; or the queue would wait on them. A dropped pane whose terminal
    /// already asked is held, and granted once it shows again (see `RestoreRuntime::held`). Part of every commit.
    pub(crate) fn prune_spawn_queue(&mut self) {
        self.release_held_spawns();
        if self.restore.pacer.is_passthrough() {
            return;
        }
        let live: HashSet<Uuid> = self
            .library()
            .all_open_sessions()
            .into_iter()
            .flat_map(|session| {
                let split = session.split_pane_identity.filter(|_| session.split.shown);
                std::iter::once(session.pane_identity).chain(split)
            })
            .collect();
        for key in self.restore.pacer.waiting_keys() {
            if !live.contains(&key) && pane_hold(self.library(), key) == PaneHold::Held {
                self.restore.held.insert(key);
            }
        }
        self.restore.pacer.retain(|key| live.contains(key));
        self.log_spawn_drain(Instant::now());
    }

    /// Grants the held panes that show again, and forgets the ones that left the model, whose terminal went with them.
    fn release_held_spawns(&mut self) {
        if self.restore.held.is_empty() {
            return;
        }
        for key in std::mem::take(&mut self.restore.held) {
            match pane_hold(self.library(), key) {
                PaneHold::Shown => self.restore.grants.push(key),
                PaneHold::Held => {
                    self.restore.held.insert(key);
                }
                PaneHold::Gone => {}
            }
        }
    }

    /// Logs how long the launch spawn queue took to drain, once, right after the call that drained it (upstream's
    /// `pacer.onDrain`).
    fn log_spawn_drain(&mut self, now: Instant) {
        if let Some(drained) = self.restore.pacer.take_drained(now) {
            tracing::debug!(ms = drained.as_millis(), "launch spawn queue drained");
        }
    }

    /// The quit capture (upstream's `makeExitCapture` closure from `applicationWillTerminate`): with `rerun` or `live`
    /// configured for the next launch, every open pane's foreground program goes into its slot for the quit save;
    /// otherwise every captured command is cleared. Runs before the quit save, while every terminal lives.
    pub fn capture_on_exit(&mut self) {
        let ids = self.library().open_ids();
        self.capture_windows_on_exit(&ids);
    }

    /// The exit capture over windows `ids`.
    pub(crate) fn capture_windows_on_exit(&mut self, ids: &[Uuid]) {
        let capturing = captures_on_exit(self.settings().effective_restore_mode());
        if !capturing {
            for id in ids {
                if let Some(store) = self.library.store_mut(*id) {
                    store
                        .sessions_mut()
                        .for_each(Session::clear_captured_foreground_commands);
                }
            }
            return;
        }
        let deadline = Instant::now() + EXIT_CAPTURE_BUDGET;
        let count = self.capture_windows(ids, CaptureKind::Exit, &|| Instant::now() < deadline);
        tracing::debug!(count, "exit capture");
    }

    /// Captures every pane of windows `ids` into its slot (see [`capture_foreground_commands`]), a live pane through
    /// its daemon's shell; how many slots a read filled. Nothing without a process table.
    pub(crate) fn capture_windows(
        &mut self,
        ids: &[Uuid],
        kind: CaptureKind,
        time_remaining: &dyn Fn() -> bool,
    ) -> usize {
        let (zmx, restore, library) = (&self.zmx, &self.restore, &mut self.library);
        let Some(table) = restore.table() else {
            return 0;
        };
        let live = |token: &str| live_foreground_group(zmx, table, token);
        let reader = reader_with_live(table, restore.shell(), &live);
        let mut captured = 0;
        for id in ids {
            if let Some(store) = library.store_mut(*id) {
                captured += capture_foreground_commands(store.sessions_mut(), &reader, kind, time_remaining);
            }
        }
        captured
    }

    /// What a window close does to the captures before the window is written (upstream's `willClose` arms): the last
    /// open window captures as the quit would, since its terminals are gone by the time the app quits; any other window
    /// clears its captured commands, since a launch could not tell its file from one open at exit. Nothing while
    /// terminating: the quit capture already ran.
    pub(crate) fn captures_on_window_close(&mut self, id: Uuid) {
        if self.library().is_terminating || !self.library().is_open(id) {
            return;
        }
        if self.library().open_ids() == [id] {
            self.capture_windows_on_exit(&[id]);
        } else if let Some(store) = self.library_mut().store_mut(id) {
            store
                .sessions_mut()
                .for_each(Session::clear_captured_foreground_commands);
        }
    }

    /// Fills the sessions' live foreground fields of `tree`, projected from `store` (upstream's `paneForeground` and
    /// `splitPaneForeground` closures): each pane's program as `foreground`, or the recognized shell holding it as
    /// `foregroundShell`; neither when it cannot be read.
    pub(crate) fn project_foreground(&self, store: &AppStore, tree: &mut ControlTree) {
        let Some(table) = self.restore.table() else {
            return;
        };
        let live = |token: &str| live_foreground_group(&self.zmx, table, token);
        let reader = reader_with_live(table, self.restore.shell(), &live);
        let read = |slot: &SurfaceSlot| slot.get().and_then(|surface| reader.running_in(surface));
        let nodes = tree
            .workspaces
            .iter_mut()
            .flat_map(|workspace| workspace.sessions.iter_mut());
        for (node, session) in nodes.zip(store.sessions()) {
            let main = read(&session.surface);
            node.foreground = main.as_ref().and_then(foreground_argv);
            node.foreground_shell = main.as_ref().and_then(foreground_shell);
            let split = read(&session.split_surface);
            node.split_foreground = split.as_ref().and_then(foreground_argv);
            node.split_foreground_shell = split.as_ref().and_then(foreground_shell);
        }
    }

    fn find_session(&self, id: Uuid) -> Option<&Session> {
        let window = self.library().window_holding_session(id)?;
        self.library().store(window)?.session(id)
    }

    /// The pacer key of `session`'s pane carrying `token`, when a pending close of an open window holds the session.
    fn pending_close_pane_key(&self, session: Uuid, token: &str) -> Option<Uuid> {
        let library = self.library();
        let held = library
            .open_ids()
            .into_iter()
            .find_map(|id| library.store(id)?.pending_close_session(session))?;
        pane_key(held, held.pane_role_for_token(token)?)
    }

    fn find_session_by_pane(&self, key: Uuid) -> Option<Uuid> {
        Some(
            self.library()
                .all_open_sessions()
                .into_iter()
                .find(|session| session.pane_identity == key || session.split_pane_identity == Some(key))?
                .id(),
        )
    }
}

/// Whether the pane keyed `key` can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneHold {
    /// An open window's session shows it: a main pane, or a shown split.
    Shown,
    /// Its terminal lives but the pane cannot show: a hidden split, or a session a pending close holds.
    Held,
    /// No open window holds it.
    Gone,
}

fn pane_hold(library: &WindowLibrary, key: Uuid) -> PaneHold {
    let holds = |session: &Session| session.pane_identity == key || session.split_pane_identity == Some(key);
    for store in library.open_ids().into_iter().filter_map(|id| library.store(id)) {
        if let Some(session) = store.sessions().find(|session| holds(session)) {
            return if session.pane_identity == key || session.split.shown {
                PaneHold::Shown
            } else {
                PaneHold::Held
            };
        }
        if store
            .pending_close_members()
            .into_iter()
            .any(|member| holds(member.session))
        {
            return PaneHold::Held;
        }
    }
    PaneHold::Gone
}

/// The pacer key of `session`'s pane `role`: its pane identity.
fn pane_key(session: &Session, role: StatusPane) -> Option<Uuid> {
    match role {
        StatusPane::Left => Some(session.pane_identity),
        StatusPane::Right => session.split_pane_identity,
        StatusPane::Scratch => None,
    }
}

fn foreground_argv(pane: &PaneForeground) -> Option<Vec<String>> {
    pane.command().map(ToOwned::to_owned)
}

fn foreground_shell(pane: &PaneForeground) -> Option<String> {
    pane.shell_name().map(ToOwned::to_owned)
}
