//! The app side of restore modes (upstream's `GhosttyApp.restoreLaunchDecision`, `restoreDenylist`, the
//! `SpawnRegistry` and `LaunchSeedProvider` wiring in `agtermApp.swift`, and `AppDelegate`'s exit captures): the
//! launch decision and denylist read once, the pane spawns the frontend asks permission for, the grants it collects
//! after each settle, and the foreground captures at quit and at the last window's close.
//!
//! The frontend creates a session pane's terminal only once the core handed it the pane's [`LaunchSeed`]: it asks
//! with [`Core::request_pane_spawn`] when the terminal is ready, and gets the seed at once or later through
//! [`Core::take_spawn_grants`].
//!
//! # Lazy restore
//!
//! A restored session spawns nothing until it is first shown (the GTK fork's deck, whose page realizes only while it
//! is visible; upstream mounts every restored pane at launch and paces their spawns): its panes stay dormant, their
//! seeds and captured commands on the session, so a quit still saves them for the next launch and a live pane's
//! daemon runs on unattached. [`Core::wake_session`] wakes a session: it is on screen ([`Core::wake_shown_sessions`],
//! the selected session, a zoom, a dashboard cell) or a command needs its terminal. A session made at runtime spawns
//! at once, shown or not. With only the shown sessions spawning, upstream's launch spawn pacer has nothing to pace,
//! and the build has none.

use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::time::{Duration, Instant};

use ngterm_host::ProcessTable;
use uuid::Uuid;

use crate::Core;
use crate::command_restore::{DENYLIST_FILE, DENYLIST_STARTER, parse_denylist};
use ngterm_proto::ControlTree;

use crate::command_restore::PaneForeground;
use crate::commands::live::launch::{live_foreground_group, pane_seed};
use crate::model::{AppStore, Session, StatusPane, SurfaceSlot, WindowLibrary};
use crate::restore::capture::{CaptureKind, ForegroundReader, capture_foreground_commands};
use crate::restore::mode::{RestoreLaunchDecision, captures_on_exit};
use crate::restore::spawn_plan::{LaunchSeed, LaunchSeedPolicy};

/// How long an exit capture may read processes before the rest of the panes keep what they had (upstream's
/// `exitCaptureBudget`).
pub const EXIT_CAPTURE_BUDGET: Duration = Duration::from_millis(500);

/// A pane the core now lets spawn, with its seed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneSpawn {
    pub session: Uuid,
    /// The pane's token, which its terminal carries.
    pub token: String,
    pub seed: LaunchSeed,
}

/// The restore state of the running app.
#[derive(Default)]
pub(crate) struct RestoreRuntime {
    pub(crate) decision: RestoreLaunchDecision,
    denylist: HashSet<String>,
    table: Option<Box<dyn ProcessTable>>,
    /// The user's `$SHELL` basename.
    shell: Option<String>,
    /// Keys granted since the frontend last collected them.
    grants: Vec<Uuid>,
    /// Panes whose terminal asked, or was granted, while a pending close held its session. Upstream unmounts such a
    /// pane and its remounted view asks again; here the terminal stays and asks only once, so the pane is granted on
    /// the undo (parked instead while its session is dormant), and forgotten once the close is final.
    held: HashSet<Uuid>,
    /// The restored sessions woken since this launch (see the module docs): every other restored session is dormant.
    woken: HashSet<Uuid>,
    /// Dormant panes whose terminal asked; granted when their session wakes, forgotten once they leave the model.
    parked: HashSet<Uuid>,
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

    /// The terminal of `session`'s pane carrying `token` is ready to spawn: its seed when it may spawn now, consuming
    /// the slots the launch armed for it; `None` while its session is dormant or a pending close holds it, which
    /// [`Core::take_spawn_grants`] later hands back once it wakes or the close is undone. One no session holds any more
    /// spawns a plain shell.
    pub fn request_pane_spawn(&mut self, session: Uuid, token: &str) -> Option<LaunchSeed> {
        let policy = self.seed_policy();
        let Some((role, key, disposed, dormant)) = self.find_session(session).and_then(|session| {
            let role = session.pane_role_for_token(token)?;
            let key = pane_key(session, role)?;
            let disposed = self.pane_disposition(Some(key), session.remote_host().is_none());
            Some((role, key, disposed, self.is_dormant(session)))
        }) else {
            // a session a pending close holds spawns on its undo, as upstream's remounted view would
            if let Some(key) = self.pending_close_pane_key(session, token) {
                self.restore.held.insert(key);
                return None;
            }
            return Some(LaunchSeed::default());
        };
        if dormant {
            self.restore.parked.insert(key);
            return None;
        }
        let window = self.library.window_holding_session(session)?;
        let session = self.library.store_mut(window)?.session_mut(session)?;
        Some(pane_seed(&mut self.live, session, role, &policy, disposed))
    }

    /// Wakes `session` (upstream's `expediteSpawn`, which jumps a paced launch queue): it is on screen, or a command
    /// needs its terminal. Its parked panes are granted; one that has not asked yet spawns once it does. A no-op for a
    /// session that is not dormant.
    pub fn wake_session(&mut self, session: Uuid) {
        let Some(keys) = self
            .find_session(session)
            .filter(|found| self.is_dormant(found))
            .map(|session| {
                [StatusPane::Left, StatusPane::Right]
                    .into_iter()
                    .filter_map(|role| pane_key(session, role))
                    .collect::<Vec<_>>()
            })
        else {
            return;
        };
        self.restore.woken.insert(session);
        for key in keys {
            if self.restore.parked.remove(&key) {
                self.restore.grants.push(key);
            }
        }
    }

    /// Wakes every session on screen (see the module docs): each open window's zoomed session, else its dashboard's
    /// cells while it is open, else its selected session. The frontend calls it on every settle, and once at launch
    /// before any window shows, so the selected sessions' panes spawn on their first ask.
    pub fn wake_shown_sessions(&mut self) {
        let library = self.library();
        let mut shown = Vec::new();
        for (window, store) in library
            .open_ids()
            .into_iter()
            .filter_map(|id| Some((id, library.store(id)?)))
        {
            if let Some(zoom) = self.zoom_target(window) {
                shown.push(zoom.session);
            } else if let Some(dashboard) = self.dashboard(window).filter(|dashboard| dashboard.is_open()) {
                shown.extend(dashboard.members().iter().map(|member| member.session));
            } else {
                shown.extend(store.selected_session_id());
            }
        }
        for session in shown {
            self.wake_session(session);
        }
    }

    /// Forgets the woken sessions no open window holds, a pending close included: a closed window's sessions are
    /// dormant again once it reopens from its file, as a launch restores them.
    pub(crate) fn forget_woken_sessions(&mut self) {
        let library = &self.library;
        let open: HashSet<Uuid> = library
            .open_ids()
            .into_iter()
            .filter_map(|id| library.store(id))
            .flat_map(|store| {
                let held = store
                    .pending_close_members()
                    .into_iter()
                    .map(|member| member.session.id());
                store.sessions().map(Session::id).chain(held).collect::<Vec<_>>()
            })
            .collect();
        self.restore.woken.retain(|id| open.contains(id));
    }

    /// Whether `session` is a restored one not woken since the launch, or since its window reopened (see the module
    /// docs).
    fn is_dormant(&self, session: &Session) -> bool {
        session.was_restored && !self.restore.woken.contains(&session.id())
    }

    /// Whether pane `role` of `session` runs in a live-session daemon (the tree's `backedByLive`; upstream reads it off
    /// the view's `zmxSessionName`, fixed by its disposition): a realized terminal by its attach, a mounted one not up
    /// yet (dormant, or granted and starting) by the disposition it attaches under, its daemon running on meanwhile. A
    /// pane with no terminal mounted, such as a hidden split, is not.
    pub(crate) fn pane_backed(&self, session: &Session, role: StatusPane) -> bool {
        let Some(surface) = session.pane_slot(role).get() else {
            return false;
        };
        if surface.is_realized() {
            return surface.is_live_session();
        }
        pane_key(session, role).is_some_and(|key| {
            self.pane_disposition(Some(key), session.remote_host().is_none())
                .disposition
                .backed_by_live()
        })
    }

    /// Whether every pane of `session` runs in a live-session daemon (see [`Core::pane_backed`]).
    pub(crate) fn session_backed(&self, session: &Session) -> bool {
        self.pane_backed(session, StatusPane::Left)
            && (!session.split.exists || self.pane_backed(session, StatusPane::Right))
    }

    /// Whether the pane keyed `key` belongs to a dormant session.
    fn pane_dormant(&self, key: Uuid) -> bool {
        self.find_session_by_pane(key)
            .and_then(|id| self.find_session(id))
            .is_some_and(|session| self.is_dormant(session))
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
                seed: pane_seed(&mut self.live, session, role, &policy, disposed),
            });
        }
        spawns
    }

    /// Brings the waiting panes in line with the model (upstream's `launchPaneDrop`, which keeps a paced queue from
    /// waiting on a pane that left): a held pane whose close was undone is granted, or parked again while its session
    /// is dormant; a held or parked pane no open window holds any more is forgotten. Part of every commit.
    pub(crate) fn prune_spawn_queue(&mut self) {
        for key in std::mem::take(&mut self.restore.held) {
            match pane_hold(self.library(), key) {
                PaneHold::Open if self.pane_dormant(key) => {
                    self.restore.parked.insert(key);
                }
                PaneHold::Open => self.restore.grants.push(key),
                PaneHold::Held => {
                    self.restore.held.insert(key);
                }
                PaneHold::Gone => {}
            }
        }
        let library = &self.library;
        self.restore
            .parked
            .retain(|key| pane_hold(library, *key) != PaneHold::Gone);
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
        let (runtime, restore, library) = (&self.live, &self.restore, &mut self.library);
        let Some(table) = restore.table() else {
            return 0;
        };
        let live = |token: &str| live_foreground_group(runtime, table, token);
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
        let live = |token: &str| live_foreground_group(&self.live, table, token);
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

    /// The spawn key of `session`'s pane carrying `token`, when a pending close of an open window holds the session.
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

/// Where the pane keyed `key` stands in the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneHold {
    /// An open window's session holds it: a main pane, or a split shown or hidden, which may spawn.
    Open,
    /// A session a pending close holds it: it waits for the undo.
    Held,
    /// No open window holds it.
    Gone,
}

fn pane_hold(library: &WindowLibrary, key: Uuid) -> PaneHold {
    let holds = |session: &Session| session.pane_identity == key || session.split_pane_identity == Some(key);
    for store in library.open_ids().into_iter().filter_map(|id| library.store(id)) {
        if store.sessions().any(&holds) {
            return PaneHold::Open;
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

/// The spawn key of `session`'s pane `role`: its pane identity.
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
