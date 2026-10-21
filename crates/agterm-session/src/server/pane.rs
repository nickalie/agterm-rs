//! A live session's pane on the server: its program on a PTY ([`agterm_pty`]), the terminal state that program's
//! output builds ([`agterm_ghostty_vt::Terminal`]), the clients its output fans out to ([`super::clients`]), and its
//! end; [`Panes`] holds them by session name.
//!
//! herdr: `src/pane.rs` (`PaneRuntime::spawn_command_builder`: the terminal, the PTY spawn, the child watcher and the
//! PTY actor feeding the terminal; `apply_pane_terminal_env`; `shutdown_pane_processes` and its graces),
//! `src/terminal/runtime.rs` (`TerminalRuntime`, the runtime facade) and `src/terminal/runtime_registry.rs`
//! (`TerminalRuntimeRegistry`, the runtimes by id) at `HERDR_REV` (`build/pins.env`), licensed under the Apache
//! License 2.0, modified:
//! - the runtime facade and the pane runtime are one type, [`Pane`], over `agterm-pty`'s actor and std threads in
//!   place of tokio tasks; agent detection, render signalling, compression and herdr's own terminal identity
//!   (`TERM_PROGRAM`, the host handles it removes) are not ported;
//! - the child's environment is the session's creation input, as zmx gave it ([`child_environment`]), not herdr's
//!   launch environment on top of the server's;
//! - the output is fanned out raw to the attached clients, every read tagged with its offset in the session's
//!   output stream, under the lock that feeds the terminal (the pane's one serialization point);
//! - the shutdown signals the child's process group through `agterm-pty`'s [`Killer`] and, on Unix, every other
//!   process of its session (herdr's session process list, [`crate::job::session_members`]), so a job-controlling
//!   shell's jobs in groups of their own are reached too; on Windows the child's job, its whole tree. A hangup, then
//!   a kill after [`KILL_GRACE`] or once the child, the PTY and the session's other processes ended (herdr's
//!   terminate step dropped), the child left unreaped meanwhile, so its group and its session, which its id names,
//!   are still reached once it exited and cannot be reused;
//! - the registry is keyed by session name and drops a pane once its program ended.
//!
//! # The terminal
//!
//! Every read of the PTY is written into the pane's terminal and queued for each client under one lock, so the
//! terminal's state and the clients' streams agree at every offset; a client attaching later starts at the offset
//! the terminal had then. The terminal answers the program's queries itself, with the reply owner's facts; a clipboard
//! read stops the stream for the owner's answer, and the owner's terminal answers Kitty graphics commands, the
//! server's later answers waiting behind them ([`super::replies`]). Every answer is queued under that lock and written
//! by one thread ([`run_helper`]), so the program reads them in the order of its queries. The terminal loads no Kitty
//! images from files, temporary files or shared memory: an image shown that way is read once, by the client that shows
//! it, and places nothing on the server's terminal, whose cursor it does not move.
//!
//! # Snapshots
//!
//! That lock is the pane's one serialization point: the parsing, the output offsets, the clients' registration, the
//! lead and its resizes, and the snapshots ([`super::snapshot`]) all happen under it, so a snapshot is the terminal's
//! state at exactly the offset the client's output resumes at. Every client's first event (after its role, when it
//! is managed) is a [`ClientEvent::Snapshot`](super::clients::ClientEvent) opening its first epoch, normally 1 (an
//! overflow before it is sent drops it unsent, and the resync's opens the next): the creator's is empty, an attach's
//! holds the session's state, scrollback included. A snapshot needs a cut, which the terminal's
//! stream continuation gives; while the program stands inside a sequence too long to hand on, the attach waits for
//! one, up to [`PaneConfig::snapshot_wait`], while the PTY keeps being read, and then fails without having changed
//! anything ([`AttachError::Unavailable`]).
//!
//! A client whose queue overflowed ([`ClientEvent::Overflowed`](super::clients::ClientEvent)) is resynced in band
//! by [`Pane::resync`]: a new snapshot, led by [`super::snapshot::RESYNC_RESET`], in place of the output it was not
//! sent, opening its next epoch; it keeps its place in the lead. Past its [`ResyncBudget`], or without a cut in time,
//! it is detached instead, and its connection is closed. One that fell behind and was not resynced when the session
//! ends is sent the session's last snapshot ahead of its exit ([`super::snapshot::Kind::Last`]). Either snapshot
//! gives it back each clipboard's latest write it missed, the program's or printed ([`super::recovery`]).
//!
//! # Leadership
//!
//! Every client attaches with its grid and, when it is managed, its nonce and whether it claims the lead; its input
//! and its resizes go through [`Pane::input`] and [`Pane::resize_client`]. [`super::lead`] decides which of them owns
//! the session's size, and the pane applies the decision under the same lock: the leader's size, in order with the
//! output, then the managed clients' role reports ([`ClientEvent::Role`](super::clients::ClientEvent)). A client
//! detached, or whose subscription was dropped, leaves the lead as well, at once (dropping a subscription takes the
//! pane's lock). The program's end leaves no client attached or leading. [`Pane::write`] (typing on behalf of no
//! client) and [`Pane::resize`] (the server's own) bypass the lead.
//!
//! # On behalf of no client
//!
//! The api ([`super::api`]) reads and writes a session without attaching to it. [`Pane::observe`] reads its terminal
//! together with its revision ([`Pane::revision`]), which counts the changes of the terminal's screen: every read of
//! the PTY, every resize and every [`Pane::print`] moves it (zmx's `screen_revision`), nothing else does, so two reads
//! with the same revision saw the same screen. [`Pane::write`] queues typing that bypasses the lead, and
//! [`Pane::print`] writes bytes into the session's output as if its program had printed them (zmx's injected output,
//! for `ngtermctl clipboard set`'s OSC 52 write): the terminal and every client take them at the next offset, once the
//! program's output stands outside a sequence they could split.
//!
//! # The end
//!
//! When the program exits, the pane waits up to [`EXIT_GRACE`] for the PTY's end, so output written before the exit
//! reaches the terminal and every client first, then queues [`ClientEvent::Exit`](super::clients::ClientEvent) with
//! the program's code and signal (what `agterm-term`'s `ExitPolicy` decides on) for every client and detaches them;
//! a client whose queue overflowed and was not resynced yet is sent the session's last snapshot before it. [`Panes`]
//! then forgets the session.

mod handoff;

pub use handoff::{Cut, Limits};

use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant, SystemTime};

use agterm_ghostty_vt as vt;
use agterm_pty::{Actor, ActorConfig, ActorHandle, Child, ExitReason, Killer, ReadPause, Size, Spawned, WriteError};

use super::clients::{ClientId, Clients, DEFAULT_QUEUE_LIMIT, ResyncBudget, Subscription};
use super::lead::{Change, Lead, RoleNotice};
use super::recovery::Recovery;
use super::replies::{
    Answer, DELEGATION_WAIT, Facts, KITTY_STORAGE_LIMIT, KITTY_WAIT, Outlet, PARK_LOW, Replies, ServerEffects,
};
use super::snapshot::{self, Kind, SnapshotError};
use crate::protocol::{DelegatedReply, ExitStatus, GridSize, LeadRole, Managed, ReplyMetadata, SessionSpec};

/// The variable naming a session in its program's environment.
pub const SESSION_VARIABLE: &str = "AGTERM_LIVE_SESSION";
/// The variable naming the session's namespace directory in its program's environment.
pub const DIRECTORY_VARIABLE: &str = "AGTERM_LIVE_DIR";
/// The explicit-leadership variables of the attach that created the session: a nested attach run from inside it must
/// not opt in on the outer terminal's behalf or learn its nonce, so its program never sees them.
pub const MANAGED_VARIABLES: [&str; 2] = ["AGTERM_LIVE_MANAGED", "AGTERM_LIVE_MANAGED_CLAIM"];
/// `TERM` for a program whose environment has none, or `dumb` (zmx's rule).
pub const FALLBACK_TERM: &str = "xterm-256color";

/// How long the end of a program waits for the PTY's end, so its last output is processed first (a background
/// process holding the PTY keeps it open).
pub const EXIT_GRACE: Duration = Duration::from_secs(1);
/// How long a kill waits for the program to end after the hangup, and again after the kill (herdr's grace).
pub const KILL_GRACE: Duration = Duration::from_millis(250);
/// How long an attach or a resync waits for a cut its snapshot can be taken at.
pub const SNAPSHOT_WAIT: Duration = Duration::from_secs(2);
/// How often a kill's grace looks at what of the program's session lives on (herdr's poll).
#[cfg(unix)]
const SESSION_POLL: Duration = Duration::from_millis(20);
/// How long the waiter holds off reaping the program for a kill under way, which holds it for the hangup, at most
/// [`KILL_GRACE`] and the kill; past it the program is reaped, and the kill then signals nothing.
const REAP_HOLD_LIMIT: Duration = Duration::from_secs(5);
/// How long the replies' helper thread sleeps with nothing to do (it is woken for whatever it does).
const HELPER_IDLE: Duration = Duration::from_secs(60);

/// The pane ids of this process, for the PTY threads' names and to tell a session from a later one of its name.
static NEXT_PANE: AtomicU64 = AtomicU64::new(1);
/// The client ids of this process.
static NEXT_CLIENT: AtomicU64 = AtomicU64::new(1);

/// What a pane is started with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneConfig {
    /// The session's name.
    pub name: String,
    /// The namespace directory the server serves, set in the program's environment.
    pub namespace: PathBuf,
    pub spec: SessionSpec,
    /// The grid the session starts with: its creator's.
    pub size: GridSize,
    /// The creator's explicit leadership; `None` for an unmanaged creator. Either way the creator leads, as no client
    /// does before it.
    pub managed: Option<Managed>,
    /// The output bytes each client's queue holds.
    pub client_queue_limit: usize,
    /// How often a client that falls behind is resynced before it is disconnected.
    pub resync_budget: ResyncBudget,
    /// How long an attach or a resync waits for a cut ([`SNAPSHOT_WAIT`]).
    pub snapshot_wait: Duration,
    /// The longest unfinished sequence a snapshot can hand on ([`snapshot::CONTINUATION_LIMIT`]): the session's
    /// terminal keeps a copy of it while it lasts.
    pub continuation_limit: usize,
    /// The creator's facts for the replies, which it owns ([`super::replies`]).
    pub metadata: ReplyMetadata,
    /// How long a clipboard read waits for the reply owner's answer ([`DELEGATION_WAIT`]).
    pub delegation_wait: Duration,
    /// How long a Kitty barrier waits for the reply owner's completion ([`KITTY_WAIT`]).
    pub kitty_wait: Duration,
}

impl PaneConfig {
    #[must_use]
    pub fn new(name: impl Into<String>, namespace: impl Into<PathBuf>, spec: SessionSpec, size: GridSize) -> Self {
        Self {
            name: name.into(),
            namespace: namespace.into(),
            spec,
            size,
            managed: None,
            client_queue_limit: DEFAULT_QUEUE_LIMIT,
            resync_budget: ResyncBudget::default(),
            snapshot_wait: SNAPSHOT_WAIT,
            continuation_limit: snapshot::CONTINUATION_LIMIT,
            metadata: ReplyMetadata::default(),
            delegation_wait: DELEGATION_WAIT,
            kitty_wait: KITTY_WAIT,
        }
    }
}

/// Why a client could not attach.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AttachError {
    /// The session's program ended.
    #[error("the live session ended")]
    Ended,
    /// No snapshot could be taken: no cut came within the wait, or the terminal could not be formatted.
    #[error("{0}")]
    Unavailable(SnapshotError),
}

/// Why [`Pane::print`] wrote nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PrintError {
    /// The session's program ended.
    #[error("the live session ended")]
    Ended,
    /// The program's output stood inside a sequence for the whole wait.
    #[error("the live session's output stands inside an escape sequence")]
    InsideSequence,
}

/// Why a client was not resynced.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResyncError {
    /// It is not attached.
    #[error("the client is not attached")]
    Detached,
    /// The session's program ended; the client's exit is queued.
    #[error("the live session ended")]
    Ended,
    /// It used its resync budget up: it was detached.
    #[error("the client fell behind more often than its resync budget allows")]
    OverBudget,
    /// No snapshot could be taken: it was detached.
    #[error("{0}")]
    Unavailable(SnapshotError),
}

/// A client just attached to a pane.
#[derive(Debug)]
pub struct Attached {
    /// Its events: the session's output from `offset` on.
    pub subscription: Subscription,
    /// The output offset the client starts at: the terminal holds everything before it.
    pub offset: u64,
    /// The session's grid at `offset`.
    pub size: GridSize,
}

/// One live session's pane. Dropping it ends the program unless it already ended.
pub struct Pane {
    id: u64,
    name: String,
    pid: u32,
    /// Whole seconds since the Unix epoch.
    created_at: u64,
    tracked_env: Vec<(String, String)>,
    resync_budget: ResyncBudget,
    snapshot_wait: Duration,
    core: Arc<Core>,
    actor: ActorHandle,
    killer: Killer,
    /// The program's `TERM`, which its terminal names in XTGETTCAP's `TN`; an update handoff carries it.
    term: Option<String>,
    /// The pane passed to another server in an update handoff: dropping it ends nothing.
    handed_over: AtomicBool,
    /// A program an update handoff brought over on Unix, which this server cannot wait for ([`handoff`]).
    #[cfg(unix)]
    adopted: Option<handoff::Adopted>,
    /// The program's process and job, which an update handoff passes on.
    #[cfg(windows)]
    origin: handoff::Origin,
}

impl std::fmt::Debug for Pane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pane")
            .field("name", &self.name)
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

/// What the PTY actor's callbacks, the child's waiter, the replies' helper thread and the pane share.
struct Core {
    /// The pane's one serialization point: its terminal, its clients and its output offset change together.
    state: Mutex<State>,
    /// Notified when the PTY ends, when output is fed and when the program's end is recorded.
    changed: Condvar,
    /// Wakes the replies' helper thread ([`run_helper`]): a read delegated, ended or cancelled, a Kitty barrier
    /// opened, replies to write.
    timer: Arc<Condvar>,
    /// The replies for the program in order (changed only under `state`'s lock, taken only by the helper thread), and
    /// what the terminal's effects answer from.
    outlet: Arc<Outlet>,
    /// Stops the PTY's reads while a waiting clipboard read holds too much output ([`super::replies`]).
    pause: ReadPause,
}

/// A pane's state under its lock.
pub(super) struct State {
    pub(super) terminal: vt::Terminal,
    pub(super) clients: Clients,
    /// Which client owns the session's size.
    pub(super) lead: Lead,
    /// Who owns the session's replies, and the clipboard read waiting for an answer ([`super::replies`]).
    pub(super) replies: Replies,
    /// The session's output bytes so far: the offset of the next read.
    pub(super) offset: u64,
    /// Counts the changes of the terminal's screen: reads, resizes and prints ([`Pane::revision`]).
    pub(super) revision: u64,
    pub(super) size: GridSize,
    /// The PTY actor stopped.
    pty_ended: bool,
    /// The program exited; it is not reaped before [`State::reaping`].
    exited: bool,
    /// The kills under way that need the program unreaped, so its process group can still be signalled.
    reap_holds: u32,
    /// The program is being reaped (or was): its id, and its group's, may be reused, so nothing holds it any more.
    reaping: bool,
    /// The program ended: nothing more is fed or queued.
    exit: Option<ExitStatus>,
    /// Run once the program's end is recorded.
    on_end: Vec<Box<dyn FnOnce() + Send>>,
    /// What an update handoff's cut holds ([`handoff`]).
    hold: Hold,
    /// Each clipboard's latest write, which a client that fell behind gets back ([`super::recovery`]).
    pub(super) recovery: Recovery,
}

/// An update handoff's hold on a pane ([`handoff`]).
#[derive(Debug, Default)]
struct Hold {
    /// The handoff holds the pane at its cut: the replies' helper takes and writes nothing and ends nothing due, and
    /// the child's waiter publishes no end, until it is thawed (a rollback, or an imported pane's activation); after a
    /// commit it stays frozen for good.
    frozen: bool,
    /// The replies' helper is writing a batch it took: a freeze waits for it, so no reply it took lands behind the cut.
    helper_busy: bool,
    /// The child's waiter saw the program end and is publishing it: no handoff takes the pane any more.
    ending: bool,
}

impl State {
    /// After a change of the lead: queue each notice for its client, then the change of the reply owner it makes.
    fn tell(&mut self, notices: &[RoleNotice]) {
        for notice in notices {
            self.clients.role(notice.client, notice.role, notice.generation);
        }
        self.refresh_owner();
    }

    /// Forget the clients whose subscription was dropped, and their part in the lead.
    pub(super) fn prune(&mut self) {
        for id in self.clients.prune() {
            let change = self.lead.detach(id);
            self.tell(&change.notices);
        }
    }
}

impl Core {
    fn lock(&self) -> MutexGuard<'_, State> {
        // a panic inside the terminal's callbacks leaves the state as the last full read made it
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Feed a read of the PTY to the terminal and the clients, up to a clipboard read the reply owner is to answer
    /// (held behind it while it waits, [`super::replies`]). Returns no replies: the terminal's are queued in the
    /// outlet, in order with every other reply, and the helper thread writes them.
    fn read(&self, bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut state = self.lock();
        if state.exit.is_some() {
            return Vec::new();
        }
        state.take_output(bytes, &self.pause);
        drop(state);
        // a snapshot waiting for a cut looks again
        self.changed.notify_all();
        self.wake_helper();
        Vec::new()
    }

    /// Have the helper thread write the replies ready (it looks under the pane's lock, so one made ready before this
    /// is never missed).
    fn wake_helper(&self) {
        if self.outlet.ready() {
            self.timer.notify_all();
        }
    }

    /// Whether the PTY's reads, paused for output held behind a waiting read, are to go on now (the pause is
    /// lifted; the caller wakes the actor once it let go of the lock).
    fn unpause(&self, state: &State) -> bool {
        if !self.pause.is_paused() || state.replies.parked() >= PARK_LOW {
            return false;
        }
        self.pause.resume();
        true
    }

    fn pty_ended(&self) {
        self.lock().pty_ended = true;
        self.changed.notify_all();
    }

    /// The program exited: a clipboard read waiting for an answer, and every later one, is denied, so its last output
    /// reaches the terminal and the clients, and the PTY is read to its end.
    fn child_exited(&self, actor: &ActorHandle) {
        let mut state = self.lock();
        state.exited = true;
        state.deny_from_now();
        let wake = self.unpause(&state);
        drop(state);
        self.changed.notify_all();
        self.wake_helper();
        if wake {
            actor.wake_reader();
        }
    }

    /// Keep the program unreaped, its process group signallable, until [`Core::release_reap`]; `false`, holding
    /// nothing, once it is being reaped.
    fn hold_reap(&self) -> bool {
        let mut state = self.lock();
        if state.reaping || state.exit.is_some() {
            return false;
        }
        state.reap_holds += 1;
        true
    }

    fn release_reap(&self) {
        let mut state = self.lock();
        state.reap_holds = state.reap_holds.saturating_sub(1);
        drop(state);
        self.changed.notify_all();
    }

    /// The program ended: wait while a handoff holds the pane frozen, then mark it ending, so no handoff takes it from
    /// then on. A pane a handoff committed stays frozen, so its waiter publishes nothing in the old server.
    fn hold_for_handoff(&self) {
        let mut state = self.lock();
        while state.hold.frozen {
            state = self
                .changed
                .wait_timeout(state, HELPER_IDLE)
                .map_or_else(|poisoned| poisoned.into_inner().0, |(guard, _)| guard);
        }
        state.hold.ending = true;
    }

    /// The waiter's turn to reap the program: once no kill holds it (at most [`REAP_HOLD_LIMIT`]), mark it reaping,
    /// so no kill holds it from then on.
    fn begin_reap(&self) {
        let (mut state, _) = self.wait_until(REAP_HOLD_LIMIT, |state| state.reap_holds == 0);
        state.reaping = true;
    }

    /// Wait until `done` holds of the state, at most `timeout`; whether it does.
    fn wait_for(&self, timeout: Duration, done: impl Fn(&State) -> bool) -> bool {
        self.wait_until(timeout, done).1
    }

    /// Wait until `done` holds of the state, at most `timeout`; the state still locked, and whether it does.
    fn wait_until(&self, timeout: Duration, done: impl Fn(&State) -> bool) -> (MutexGuard<'_, State>, bool) {
        let deadline = Instant::now() + timeout;
        let mut state = self.lock();
        while !done(&state) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return (state, false);
            }
            state = self
                .changed
                .wait_timeout(state, left)
                .map_or_else(|poisoned| poisoned.into_inner().0, |(guard, _)| guard);
        }
        (state, true)
    }

    /// Forget the clients whose subscription was dropped now, not at the pane's next event: a leader gone that way
    /// leaves the session unowned and its managed clients are told at once.
    fn prune(&self) {
        self.lock().prune();
    }

    /// Record the program's end and tell every client, a client that fell behind first given the session's last
    /// snapshot ([`last_snapshots`]); no client is attached, or leads, from then on.
    fn end(&self, status: ExitStatus) {
        let mut state = self.lock();
        // output still held behind a read (the PTY ended before the program) goes in before the end
        state.deny_from_now();
        drop(self.outlet.take());
        last_snapshots(&mut state);
        state.clients.exit(&status);
        state.lead = Lead::new();
        state.exit = Some(status);
        let on_end = std::mem::take(&mut state.on_end);
        drop(state);
        self.changed.notify_all();
        self.timer.notify_all();
        for hook in on_end {
            hook();
        }
    }

    /// Wait, at most `wait`, until a snapshot can be taken (the terminal's continuation is known, or the session has
    /// no output yet); the state, still locked.
    ///
    /// # Errors
    ///
    /// [`AttachError::Ended`] once the program ended; [`AttachError::Unavailable`] when no cut came in time.
    fn wait_for_cut(&self, wait: Duration) -> Result<MutexGuard<'_, State>, AttachError> {
        let cut = |state: &State| state.offset == 0 || snapshot::cut_available(&state.terminal);
        let (state, _) = self.wait_until(wait, |state| state.exit.is_some() || cut(state));
        if state.exit.is_some() {
            return Err(AttachError::Ended);
        }
        if !cut(&state) {
            return Err(AttachError::Unavailable(SnapshotError::Unavailable));
        }
        Ok(state)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Pane {
    /// Start the session `config` names: its program on a PTY of `config.size`, with the environment
    /// [`child_environment`] makes of its spec, in its spec's working directory (else the home directory of that
    /// environment). The client creating it is attached before the program runs, so it is sent all of its output
    /// from offset zero on, as an attach that creates its session is; it leads the session (told so first when it is
    /// managed).
    ///
    /// # Errors
    ///
    /// [`io::ErrorKind::InvalidInput`] for an empty argv; the terminal could not be created, the PTY opened, the
    /// program started or the PTY's thread started.
    pub fn spawn(config: PaneConfig) -> io::Result<(Self, Attached)> {
        let size = GridSize {
            cols: config.size.cols.max(1),
            rows: config.size.rows.max(1),
        };
        let env = child_environment(&config.spec.env, &config.name, &config.namespace);
        let command = command(&config.spec, &env)?;
        let term = lookup(&env, "TERM").map(str::to_owned);
        let (core, creator) = new_core(&config, size, term.as_deref())?;
        let Spawned { master, child } = agterm_pty::spawn(&command, Size::cells(size.rows, size.cols))?;
        let id = NEXT_PANE.fetch_add(1, Ordering::Relaxed);
        let killer = child.killer();
        let pid = child.pid();
        #[cfg(windows)]
        let origin = match handoff::Origin::of(&child) {
            Ok(origin) => origin,
            Err(err) => {
                let _ = killer.kill_tree();
                return Err(err);
            }
        };
        let actor = start_threads(id, master, child, &core)?;
        tracing::debug!(session = %config.name, pid, "live session started");
        let pane = Self {
            id,
            pid,
            created_at: unix_seconds(SystemTime::now()),
            tracked_env: tracked(&env, &config.spec.tracked_env),
            resync_budget: config.resync_budget,
            snapshot_wait: config.snapshot_wait,
            name: config.name,
            core,
            actor,
            killer,
            term,
            handed_over: AtomicBool::new(false),
            #[cfg(unix)]
            adopted: None,
            #[cfg(windows)]
            origin,
        };
        Ok((pane, creator))
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The program's process id.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// When the session was created, in whole seconds since the Unix epoch.
    #[must_use]
    pub fn created_at(&self) -> u64 {
        self.created_at
    }

    /// The values of the spec's tracked variables in the program's environment, in the spec's order; a variable the
    /// environment lacks is left out.
    #[must_use]
    pub fn tracked_env(&self) -> &[(String, String)] {
        &self.tracked_env
    }

    /// Attach a client whose grid is `size`, under explicit leadership when `managed` is given: it is sent a snapshot
    /// of the session at the offset returned, opening its first epoch (normally 1; later when its queue overflows
    /// before that snapshot is sent, which drops it unsent), then the session's output from that offset on. When it
    /// takes the lead ([`super::lead`]), the session takes its size first, so the offset, size and snapshot are the
    /// resized session's; a managed client is told its role before anything else. Waits up to
    /// [`PaneConfig::snapshot_wait`] for a cut, the PTY read meanwhile.
    ///
    /// # Errors
    ///
    /// [`AttachError::Ended`] once the program ended; [`AttachError::Unavailable`] when no snapshot could be taken,
    /// with nothing changed (no cut in time), or with the client attached for a moment and gone again (the formatter
    /// failed after the lead took its size).
    pub fn try_attach(&self, size: GridSize, managed: Option<&Managed>) -> Result<Attached, AttachError> {
        self.attach_with(size, managed, ReplyMetadata::default())
    }

    /// [`Pane::try_attach`] of a client whose facts for the replies are `metadata` ([`Pane::set_metadata`]), which
    /// count from its attach on when it owns the replies. It is told whether it owns them after its role, before its
    /// snapshot.
    ///
    /// # Errors
    ///
    /// As [`Pane::try_attach`].
    pub fn attach_with(
        &self,
        size: GridSize,
        managed: Option<&Managed>,
        metadata: ReplyMetadata,
    ) -> Result<Attached, AttachError> {
        let mut state = self.core.wait_for_cut(self.snapshot_wait)?;
        state.prune();
        let id = NEXT_CLIENT.fetch_add(1, Ordering::Relaxed);
        let change = state.lead.attach(id, size, managed);
        if let Some(size) = change.resize {
            self.resize_locked(&mut state, size);
        }
        let data = match capture(&mut state, Kind::Attach) {
            Ok(data) => data,
            Err(err) => {
                let undo = state.lead.detach(id);
                state.tell(&change.notices);
                state.tell(&undo.notices);
                self.release(state);
                return Err(AttachError::Unavailable(err));
            }
        };
        let mut subscription = state.clients.attach(id);
        prune_on_drop(&self.core, &mut subscription);
        state.remember(id, metadata);
        state.tell(&change.notices);
        state.introduce(id);
        let (offset, size) = (state.offset, state.size);
        state.clients.snapshot(id, offset, size, data);
        let attached = Attached {
            subscription,
            offset,
            size,
        };
        self.release(state);
        Ok(attached)
    }

    /// Resync the client `id` after its queue overflowed: a snapshot of the session now, led by the reset
    /// ([`super::snapshot::RESYNC_RESET`]), in place of the output it was not sent, opening its next epoch; its role
    /// is kept. Nothing when it did not overflow. Waits up to [`PaneConfig::snapshot_wait`] for a cut.
    ///
    /// # Errors
    ///
    /// [`ResyncError::Detached`] when it is not attached, [`ResyncError::Ended`] once the program ended, before or
    /// during the wait (the session's last snapshot and its exit are queued instead); [`ResyncError::OverBudget`] past
    /// its [`ResyncBudget`] and [`ResyncError::Unavailable`] without a snapshot, both of which detach it (it leaves the
    /// lead, as a detach does), so its connection is to be closed.
    pub fn resync(&self, id: ClientId) -> Result<(), ResyncError> {
        let mut state = self.core.lock();
        if state.exit.is_some() {
            return Err(ResyncError::Ended);
        }
        match state.clients.overflowed(id) {
            None => return Err(ResyncError::Detached),
            Some(false) => return Ok(()),
            Some(true) => {}
        }
        state.fell_behind(id);
        if !state.clients.spend_resync(id, self.resync_budget, Instant::now()) {
            self.disconnect(state, id);
            return Err(ResyncError::OverBudget);
        }
        drop(state);
        let mut state = match self.core.wait_for_cut(self.snapshot_wait) {
            Ok(state) => state,
            Err(AttachError::Ended) => return Err(ResyncError::Ended),
            Err(AttachError::Unavailable(err)) => {
                self.disconnect(self.core.lock(), id);
                return Err(ResyncError::Unavailable(err));
            }
        };
        if state.clients.overflowed(id).is_none() {
            return Err(ResyncError::Detached);
        }
        match capture(&mut state, Kind::Resync) {
            Ok(mut data) => {
                recover(&state, id, &mut data);
                let (offset, size) = (state.offset, state.size);
                state.clients.snapshot(id, offset, size, data);
                self.release(state);
                Ok(())
            }
            Err(err) => {
                self.disconnect(state, id);
                Err(ResyncError::Unavailable(err))
            }
        }
    }

    /// Detach the client `id` under the pane's lock, already held, and let go of it.
    fn disconnect(&self, mut state: MutexGuard<'_, State>, id: ClientId) {
        state.clients.detach(id);
        let change = state.lead.detach(id);
        state.tell(&change.notices);
        self.release(state);
    }

    /// Whether an answer the client `id` gave in its epoch `epoch` still counts: the epoch is its current one. An
    /// answer of a superseded epoch (given before a resync) is dropped.
    #[must_use]
    pub fn accepts_reply(&self, id: ClientId, epoch: u64) -> bool {
        self.core.lock().clients.epoch(id) == Some(epoch)
    }

    /// Detach the client `id`; whether it was attached. When it led, the session is left unowned, keeping its size.
    #[must_use]
    pub fn detach(&self, id: ClientId) -> bool {
        let mut state = self.core.lock();
        let attached = state.clients.detach(id);
        let change = state.lead.detach(id);
        state.tell(&change.notices);
        attached
    }

    /// The role of the client `id`; `None` when it is not attached.
    #[must_use]
    pub fn role(&self, id: ClientId) -> Option<LeadRole> {
        let mut state = self.core.lock();
        state.prune();
        state.lead.role(id)
    }

    /// The client leading the session.
    #[must_use]
    pub fn leader(&self) -> Option<ClientId> {
        let mut state = self.core.lock();
        state.prune();
        state.lead.leader()
    }

    /// Input the client `id` sent for the program: queued when it leads, or when it takes the lead by typing (an
    /// unmanaged client; the session takes its size first); dropped otherwise. Whether it was queued.
    ///
    /// # Errors
    ///
    /// As [`Pane::write`].
    pub fn input(&self, id: ClientId, data: &[u8]) -> Result<bool, WriteError> {
        let mut state = self.core.lock();
        if state.exit.is_some() {
            return Ok(false);
        }
        state.prune();
        let input = state.lead.input(id, data);
        self.apply(state, &input.change);
        if !input.forward {
            return Ok(false);
        }
        self.actor.write_input(data)?;
        Ok(true)
    }

    /// The client `id`'s grid is now `size`: the session takes it while the client leads, or once it takes the lead
    /// of an unowned session by resizing (an unmanaged client); otherwise it is only held, for when the client leads.
    pub fn resize_client(&self, id: ClientId, size: GridSize) {
        let mut state = self.core.lock();
        if state.exit.is_some() {
            return;
        }
        state.prune();
        let change = state.lead.resize(id, size);
        self.apply(state, &change);
    }

    /// The clients attached.
    #[must_use]
    pub fn clients(&self) -> usize {
        self.core.lock().clients.len()
    }

    /// The session's output bytes so far.
    #[must_use]
    pub fn offset(&self) -> u64 {
        self.core.lock().offset
    }

    /// The session's grid.
    #[must_use]
    pub fn size(&self) -> GridSize {
        self.core.lock().size
    }

    /// Queue `data` as the program's input.
    ///
    /// # Errors
    ///
    /// As [`ActorHandle::write_input`]: the input queue is full, or the PTY takes no more input.
    pub fn write(&self, data: &[u8]) -> Result<(), WriteError> {
        self.actor.write_input(data)
    }

    /// Resize the session's grid: its terminal, its PTY (the program sees the new size before any input queued after
    /// this) and, in order with the output, every client. A zero dimension counts as one; the size it had already
    /// changes nothing. The server's own resize: it bypasses the lead ([`Pane::resize_client`] does not).
    pub fn resize(&self, size: GridSize) {
        let mut state = self.core.lock();
        self.resize_locked(&mut state, size);
        self.release(state);
    }

    /// [`Pane::resize`] under the pane's lock, already held.
    fn resize_locked(&self, state: &mut State, size: GridSize) {
        let size = GridSize {
            cols: size.cols.max(1),
            rows: size.rows.max(1),
        };
        if state.exit.is_some() || state.size == size {
            return;
        }
        let facts = state.replies.facts();
        let (width, height) = (facts.cell_width_px, facts.cell_height_px);
        if let Err(err) = state.terminal.resize(size.cols, size.rows, width, height) {
            tracing::warn!(session = %self.name, %err, "resizing the session's terminal failed");
        }
        self.actor.resize(Size::cells(size.rows, size.cols));
        state.size = size;
        state.revision = state.revision.wrapping_add(1);
        let offset = state.offset;
        state.clients.resized(offset, size);
    }

    /// Apply a leadership change under the pane's lock (the leader's size, then the role reports) and let go of it.
    fn apply(&self, mut state: MutexGuard<'_, State>, change: &Change) {
        if let Some(size) = change.resize {
            self.resize_locked(&mut state, size);
        }
        state.tell(&change.notices);
        self.release(state);
    }

    /// Let go of the pane's lock; have the helper thread write the replies made ready under it (a size report the
    /// program asked for, in-band resize notifications, a clipboard read answered, a reply owner's Kitty answers), in
    /// order with every other reply; and read the PTY again once the output held behind a clipboard read went
    /// through.
    fn release(&self, state: MutexGuard<'_, State>) {
        let wake = self.core.unpause(&state);
        drop(state);
        self.core.changed.notify_all();
        self.core.wake_helper();
        if wake {
            self.actor.wake_reader();
        }
    }

    /// The client `id`'s facts for the replies are now `metadata` (they replace its earlier ones whole): the session's
    /// terminal answers with them while it owns the replies. Nothing for a client not attached.
    pub fn set_metadata(&self, id: ClientId, metadata: ReplyMetadata) {
        let mut state = self.core.lock();
        if state.exit.is_some() {
            return;
        }
        state.remember(id, metadata);
        self.release(state);
    }

    /// The client `id`'s answer to the delegated request `request`, given in its epoch `epoch`: it ends the clipboard
    /// read waiting for it, and the output held behind that read goes on. Whether it was taken: dropped unless that
    /// read was sent to this client in that epoch, still waits (no denial ended it: the owner changing, a deadline)
    /// and the client has not fallen behind since.
    #[must_use]
    pub fn answer(&self, id: ClientId, request: u64, epoch: u64, reply: DelegatedReply) -> bool {
        let mut state = self.core.lock();
        if state.exit.is_some() {
            return false;
        }
        let taken = state.answer(id, request, epoch, Answer::from(reply));
        self.release(state);
        taken
    }

    /// The client `id`'s completion of the Kitty barrier `barrier` it was sent in its epoch `epoch`
    /// ([`ClientEvent::KittyBarrier`](super::clients::ClientEvent)), with what its terminal answered the program's Kitty
    /// graphics commands with since its previous barrier (`answers`, empty for none): they go to the program, whatever
    /// the lead, in the barrier's place among the server's replies. Dropped unless that barrier was sent to this client
    /// in that epoch, still waits and the client has not fallen behind since; answers that are not whole Kitty answers
    /// end the barrier empty. Whether they were taken.
    #[must_use]
    pub fn complete(&self, id: ClientId, barrier: u64, epoch: u64, answers: &[u8]) -> bool {
        let mut state = self.core.lock();
        if state.exit.is_some() {
            return false;
        }
        let taken = state.complete(id, barrier, epoch, answers);
        self.release(state);
        taken
    }

    /// The client owning the session's replies ([`super::replies`]) and the generation of its ownership.
    #[must_use]
    pub fn reply_owner(&self) -> (Option<ClientId>, u32) {
        let mut state = self.core.lock();
        state.prune();
        (state.replies.owner(), state.replies.generation())
    }

    /// Run `read` on the session's terminal, under the pane's lock (no output is fed meanwhile).
    pub fn with_terminal<R>(&self, read: impl FnOnce(&mut vt::Terminal) -> R) -> R {
        read(&mut self.core.lock().terminal)
    }

    /// Run `read` on the session's terminal and its revision ([`Pane::revision`]) together, under the pane's lock;
    /// `None` once the program ended, whose session is gone.
    pub fn observe<R>(&self, read: impl FnOnce(&mut vt::Terminal, u64) -> R) -> Option<R> {
        let mut state = self.core.lock();
        if state.exit.is_some() {
            return None;
        }
        let revision = state.revision;
        Some(read(&mut state.terminal, revision))
    }

    /// Counts the changes of the session's screen: it moves with every read of the PTY, every resize and every
    /// [`Pane::print`], and with nothing else (it starts at zero and wraps).
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.core.lock().revision
    }

    /// Write `data` into the session's output as if its program had printed it: the session's terminal and every
    /// client take it at the next offset, in order with the program's output, and the revision moves. Waits up to
    /// [`PaneConfig::snapshot_wait`] for the program's output to stand outside any sequence (the PTY read
    /// meanwhile), so the bytes neither split one of the program's sequences nor are split by it. The terminal's
    /// replies to them are dropped: the program asked for none. Nothing for empty `data`.
    ///
    /// On Windows the pseudoconsole's buffer never holds them, so a later resize of the session, which makes conhost
    /// repaint its viewport from that buffer, paints over what of them the viewport shows
    /// (`docs/backlog/a-windows-resize-repaints-over-printed-text.md`).
    ///
    /// # Errors
    ///
    /// [`PrintError::Ended`] once the program ended; [`PrintError::InsideSequence`] when its output still stood
    /// inside a sequence at the end of the wait, with nothing written.
    pub fn print(&self, data: &[u8]) -> Result<(), PrintError> {
        let (mut state, _) = self.core.wait_until(self.snapshot_wait, |state| {
            state.exit.is_some() || state.terminal.at_ground()
        });
        if state.exit.is_some() {
            return Err(PrintError::Ended);
        }
        if !state.terminal.at_ground() {
            return Err(PrintError::InsideSequence);
        }
        if data.is_empty() {
            return Ok(());
        }
        // the terminal's replies to bytes the program never printed are dropped
        self.core.outlet.discard(true);
        state.terminal.vt_write(data);
        self.core.outlet.discard(false);
        // the clipboard-read scanner follows the terminal's parser
        let kitty = state.replies.observe(data);
        // a clipboard write printed is kept for a client that falls behind on it, as the program's are
        state.advance(data);
        if kitty {
            // the owner's terminal answers the printed Kitty commands too: a barrier takes those answers, dropped
            state.barrier(true);
        }
        self.release(state);
        Ok(())
    }

    /// How the program ended; `None` while it runs.
    #[must_use]
    pub fn exit_status(&self) -> Option<ExitStatus> {
        self.core.lock().exit.clone()
    }

    /// Wait at most `timeout` for the program's end to be recorded; how it ended, `None` if it still runs.
    #[must_use]
    pub fn wait_exit(&self, timeout: Duration) -> Option<ExitStatus> {
        self.core.wait_for(timeout, |state| state.exit.is_some());
        self.exit_status()
    }

    /// End the session's program and its tree. Without `force`, a hangup to its process group, so a program or a
    /// member of its group that ignores it lives on; with `force`, [`Pane::end_tree`]'s hangup and then kill of the
    /// whole group and of every other process of its session, which ends those too. On Windows either ends the
    /// program's job, its whole tree, at once.
    /// Whether the program's end was recorded within [`KILL_GRACE`] after the last signal plus [`EXIT_GRACE`] for
    /// its last output.
    ///
    /// # Errors
    ///
    /// A signal could not be sent (on Windows, the job could not be terminated).
    pub fn kill(&self, force: bool) -> io::Result<bool> {
        if self.exit_status().is_some() {
            return Ok(true);
        }
        if !still_the_program(self) {
            return Ok(self.ended_within(KILL_GRACE));
        }
        if force {
            self.end_tree()?;
        } else {
            self.killer.hang_up()?;
        }
        Ok(self.ended_within(KILL_GRACE))
    }

    /// herdr's `shutdown_pane_processes`: a hangup to the program's process group and the rest of its session, up
    /// to [`KILL_GRACE`] for the program, the PTY and the session's other processes to end (so a job that handles
    /// the hangup has the grace to, though it let go of the PTY or the tty was revoked, as macOS does when the
    /// session's leader exits), then a kill of both, which ends a process that ignores the hangup, in the program's
    /// group or in one of its own (a job-controlling shell's `nohup` or `disown`ed job). The program is held
    /// unreaped throughout, so its group and its session, both named by its id, are still reached once it exited.
    /// Nothing once it is being reaped: its id may be reused by then.
    ///
    /// # Errors
    ///
    /// As [`Pane::kill`]; the kill is sent even when the hangup fails.
    fn end_tree(&self) -> io::Result<()> {
        if !still_the_program(self) || !self.core.hold_reap() {
            return Ok(());
        }
        let hung_up = self.killer.hang_up();
        signal_session(self.pid, crate::job::Signal::HangUp);
        if hung_up.is_ok() {
            let deadline = Instant::now() + KILL_GRACE;
            self.core.wait_for(KILL_GRACE, |state| state.exited && state.pty_ended);
            wait_for_session(self.pid, deadline);
        }
        let killed = self.killer.kill_tree();
        signal_session(self.pid, crate::job::Signal::Kill);
        self.core.release_reap();
        hung_up.and(killed)
    }

    /// Whether the program ends within `grace` (its end then recorded within [`EXIT_GRACE`] more).
    fn ended_within(&self, grace: Duration) -> bool {
        self.wait_exit(grace + EXIT_GRACE).is_some()
    }

    /// Run `on_end` once the program's end is recorded (now, if it already is), after the hooks given before it.
    pub fn on_end(&self, on_end: Box<dyn FnOnce() + Send>) {
        let mut state = self.core.lock();
        if state.exit.is_none() {
            state.on_end.push(on_end);
            return;
        }
        drop(state);
        on_end();
    }
}

impl Drop for Pane {
    /// A pane let go of while its program runs ends it and its tree ([`Pane::end_tree`]). The waiter's clone of the
    /// actor keeps reading the PTY until the program's end is recorded, so its last output still reaches the clients.
    fn drop(&mut self) {
        // a pane handed over runs in the new server, or was not the new server's yet (an import that never committed)
        if self.handed_over.load(Ordering::Acquire) {
            return;
        }
        if self.exit_status().is_none()
            && let Err(err) = self.end_tree()
        {
            tracing::warn!(session = %self.name, %err, "ending a dropped session's program failed");
        }
    }
}

/// Whether `pane`'s pid still names its program, so its group and session may be signalled: always for a child of
/// this server, which holds it unreaped (and on Windows, whose job names the tree); for a program an update handoff
/// brought over on Unix, only while the process of that pid has the program's start time (best effort: nothing holds
/// the pid between this look and the signal).
fn still_the_program(pane: &Pane) -> bool {
    #[cfg(unix)]
    {
        pane.adopted.as_ref().is_none_or(|adopted| adopted.alive(pane.pid))
    }
    #[cfg(windows)]
    {
        let _ = pane;
        true
    }
}

/// `signal` to every process of the session the program `pid` leads (agterm-pty starts it in a session of its own,
/// so the session's id is its pid), read afresh, so a process the hangup reparented or one forked meanwhile is
/// reached. Only while [`Pane::end_tree`] holds the program unreaped: its pid, and so the session's id, cannot be
/// reused meanwhile. Best effort, as herdr's: a member gone by the signal is passed over, and an unreadable table
/// leaves only the group's signal. Nothing on Windows, whose job holds the tree.
#[cfg(unix)]
fn signal_session(pid: u32, signal: crate::job::Signal) {
    let Ok(session) = i32::try_from(pid) else {
        return;
    };
    for record in crate::job::session_members(session).unwrap_or_default() {
        crate::job::signal(signal, &record);
    }
}

#[cfg(windows)]
fn signal_session(_pid: u32, _signal: crate::job::Signal) {}

/// Until `deadline`, while a process of the session the program `pid` leads, other than the program, lives on
/// (herdr's `wait_for_processes_to_exit`, which counts a zombie as alive too), read afresh each [`SESSION_POLL`].
/// A session with no other process, or a deadline already past, costs at most one read and no wait. Under
/// [`Pane::end_tree`]'s hold, as [`signal_session`]; an unreadable table ends the wait. Nothing on Windows, whose
/// job holds the tree.
#[cfg(unix)]
fn wait_for_session(pid: u32, deadline: Instant) {
    let Ok(session) = i32::try_from(pid) else {
        return;
    };
    let lives_on = || {
        crate::job::session_members(session).is_some_and(|members| members.iter().any(|member| member.pid != session))
    };
    while Instant::now() < deadline && lives_on() {
        std::thread::sleep(SESSION_POLL);
    }
}

#[cfg(windows)]
fn wait_for_session(_pid: u32, _deadline: Instant) {}

/// The program of `spec` with the environment `env` as its whole environment.
fn command(spec: &SessionSpec, env: &[(String, String)]) -> io::Result<agterm_pty::Command> {
    let (program, args) = spec
        .argv
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "a session needs a program to run"))?;
    let mut command = agterm_pty::Command::new(program).args(args).env_clear();
    for (key, value) in env {
        command = command.env(key, value);
    }
    if let Some(cwd) = spec.cwd.as_deref().map(PathBuf::from).or_else(|| home(env)) {
        command = command.cwd(cwd);
    }
    Ok(command)
}

/// The home directory of the environment `env`: `HOME`, or `USERPROFILE` on Windows.
fn home(env: &[(String, String)]) -> Option<PathBuf> {
    let name = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    lookup(env, name)
        .filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(OsString::from(home)))
}

/// Attach the client creating a session of `size`: it leads, as no client does before it, a managed one is told so
/// before anything else, and its epoch 1 opens with the empty snapshot of a session with no output yet.
fn attach_creator(clients: &mut Clients, lead: &mut Lead, size: GridSize, managed: Option<&Managed>) -> Attached {
    let id = NEXT_CLIENT.fetch_add(1, Ordering::Relaxed);
    // the session starts at the creator's size: the change's resize is the size it has
    let change = lead.attach(id, size, managed);
    let subscription = clients.attach(id);
    for notice in &change.notices {
        clients.role(notice.client, notice.role, notice.generation);
    }
    // the leader owns the replies, in the session's first ownership ([`Replies::new`])
    clients.reply_owner(id, true, 1);
    clients.snapshot(id, 0, size, Vec::new());
    Attached {
        subscription,
        offset: 0,
        size,
    }
}

/// The state of a pane for `config`, its grid `size` and its program's `TERM` `term`, with its creator attached (as
/// [`Pane::spawn`] says), before any program runs.
fn new_core(config: &PaneConfig, size: GridSize, term: Option<&str>) -> io::Result<(Arc<Core>, Attached)> {
    let outlet = Arc::new(Outlet::default());
    let timer = Arc::new(Condvar::new());
    let mut clients = Clients::new(config.client_queue_limit);
    let mut lead = Lead::new();
    let mut creator = attach_creator(&mut clients, &mut lead, size, config.managed.as_ref());
    let replies = Replies::new(
        Arc::clone(&outlet),
        Arc::clone(&timer),
        creator.subscription.id(),
        config.metadata.clone(),
        (config.delegation_wait, config.kitty_wait),
    );
    let terminal = server_terminal(size, config.continuation_limit, &outlet, replies.facts(), term)?;
    let parts = Parts {
        terminal,
        clients,
        lead,
        replies,
        offset: 0,
        revision: 0,
        size,
        frozen: false,
    };
    let core = assemble(parts, timer, outlet);
    prune_on_drop(&core, &mut creator.subscription);
    Ok((core, creator))
}

/// What a pane's state starts from: a new session's, or one an update handoff brought over.
struct Parts {
    terminal: vt::Terminal,
    clients: Clients,
    lead: Lead,
    replies: Replies,
    offset: u64,
    revision: u64,
    size: GridSize,
    frozen: bool,
}

/// A pane's shared state from `parts`, its replies' helper woken by `timer` and its replies queued in `outlet`.
fn assemble(parts: Parts, timer: Arc<Condvar>, outlet: Arc<Outlet>) -> Arc<Core> {
    Arc::new(Core {
        state: Mutex::new(State {
            terminal: parts.terminal,
            clients: parts.clients,
            lead: parts.lead,
            replies: parts.replies,
            offset: parts.offset,
            revision: parts.revision,
            size: parts.size,
            pty_ended: false,
            exited: false,
            reap_holds: 0,
            reaping: false,
            exit: None,
            on_end: Vec::new(),
            hold: Hold {
                frozen: parts.frozen,
                ..Hold::default()
            },
            recovery: Recovery::default(),
        }),
        changed: Condvar::new(),
        timer,
        outlet,
        pause: ReadPause::new(),
    })
}

/// The snapshot of the session `state` holds ([`snapshot::capture`]) of the `kind` given; empty while the session has
/// no output, which its client's terminal already shows.
fn capture(state: &mut State, kind: Kind) -> Result<Vec<u8>, SnapshotError> {
    if state.offset == 0 {
        return Ok(Vec::new());
    }
    snapshot::capture(&mut state.terminal, kind)
}

/// Queue the session's last snapshot ([`Kind::Last`]) for each client whose queue overflowed and that was not resynced
/// yet, ahead of its exit, so it ends on the session's final terminal rather than on the output it was sent before it
/// fell behind (a resync of an ended session is none). It needs no cut and is not counted against the client's resync
/// budget.
fn last_snapshots(state: &mut State) {
    let behind = state.clients.overflowed_clients();
    if behind.is_empty() {
        return;
    }
    match capture(state, Kind::Last) {
        Ok(data) => {
            let (offset, size) = (state.offset, state.size);
            for id in behind {
                let mut data = data.clone();
                recover(state, id, &mut data);
                state.clients.snapshot(id, offset, size, data);
            }
        }
        Err(err) => {
            tracing::warn!(%err, "a live session's clients that fell behind end without its last snapshot");
        }
    }
}

/// Put into the resync or last `snapshot` of the client `id` the clipboard writes it missed since its copy of the
/// stream stopped ([`super::recovery`]), right after the snapshot's reset.
fn recover(state: &State, id: ClientId, snapshot: &mut Vec<u8>) {
    if let Some(missed_since) = state.clients.missed_since(id) {
        snapshot::insert_after_reset(snapshot, &state.recovery.since(missed_since));
    }
}

/// Have the pane forget the client of `subscription` as soon as it is dropped ([`Core::prune`]), under the pane's
/// lock; nothing once the pane is gone.
fn prune_on_drop(core: &Arc<Core>, subscription: &mut Subscription) {
    let core = Arc::downgrade(core);
    subscription.on_drop(move |_| {
        if let Some(core) = core.upgrade() {
            core.prune();
        }
    });
}

/// The server's terminal for a session of `size`: its replies into `outlet` ([`ServerEffects`]), answering with
/// `facts` and as the terminfo entry `term` (the program's `TERM`, XTGETTCAP's `TN`); Kitty images placed but not
/// loaded from files, temporary files or shared memory, in at most [`KITTY_STORAGE_LIMIT`]; the continuation its
/// snapshots need tracked up to `continuation_limit` bytes.
fn server_terminal(
    size: GridSize,
    continuation_limit: usize,
    outlet: &Arc<Outlet>,
    facts: &Facts,
    term: Option<&str>,
) -> io::Result<vt::Terminal> {
    let mut terminal = vt::Terminal::new(size.cols, size.rows).map_err(vt_error)?;
    terminal.set_continuation_limit(continuation_limit).map_err(vt_error)?;
    configure(&mut terminal, size, outlet, facts, term)?;
    Ok(terminal)
}

/// The server's settings of a session's terminal of `size` ([`server_terminal`]) but its continuation tracking, on a
/// new terminal or one an update handoff decoded.
fn configure(
    terminal: &mut vt::Terminal,
    size: GridSize,
    outlet: &Arc<Outlet>,
    facts: &Facts,
    term: Option<&str>,
) -> io::Result<()> {
    terminal
        .set_kitty_image_storage_limit(KITTY_STORAGE_LIMIT)
        .map_err(vt_error)?;
    terminal.set_kitty_image_media(false, None, false).map_err(vt_error)?;
    if let Err(err) = terminal.set_terminfo_name(term.filter(|term| !term.is_empty())) {
        tracing::debug!(%err, ?term, "the session's TERM is no terminfo name; XTGETTCAP TN goes unanswered");
    }
    facts.apply(terminal, None, size.cols, size.rows).map_err(vt_error)?;
    terminal
        .set_effects(Box::new(ServerEffects::new(Arc::clone(outlet))))
        .map_err(vt_error)?;
    Ok(())
}

fn vt_error(err: vt::Error) -> io::Error {
    io::Error::other(format!("the session's terminal: {err}"))
}

/// The PTY actor: every read into the terminal and the clients, the terminal's replies back to the program; its
/// reads pause on the pane's [`ReadPause`].
fn start_actor(id: u64, master: agterm_pty::Master, core: &Arc<Core>, quiesced: bool) -> io::Result<ActorHandle> {
    let reading = Arc::clone(core);
    let mut config = ActorConfig::new(format!("live-{id}"), master, Box::new(move |bytes| reading.read(bytes)));
    config.read_pause = core.pause.clone();
    config.initially_quiesced = quiesced;
    let ending = Arc::downgrade(core);
    config.on_exit = Some(Box::new(move |reason: ExitReason| {
        tracing::debug!(?reason, "a live session's PTY actor stopped");
        if let Some(core) = ending.upgrade() {
            core.pty_ended();
        }
    }));
    Actor::spawn(config)
}

/// The child's waiter: its exit, then the PTY's end (within [`EXIT_GRACE`]), then, once no kill holds it, its
/// reaping, then the end recorded for the clients. The child stays unreaped until then, so a kill still reaches the
/// members of its process group that outlived it.
fn start_waiter(id: u64, mut child: Child, core: &Arc<Core>, actor: &ActorHandle) -> io::Result<()> {
    let core = Arc::clone(core);
    // a clone keeps the actor running until the end is recorded, even once the pane is dropped
    let actor = actor.clone();
    std::thread::Builder::new()
        .name(format!("live-{id}-wait"))
        .spawn(move || {
            let reaped = child.wait_exit().and_then(|()| {
                core.hold_for_handoff();
                core.child_exited(&actor);
                actor.child_exited();
                core.wait_for(EXIT_GRACE, |state| state.pty_ended);
                core.begin_reap();
                child.wait()
            });
            let status = match reaped {
                Ok(status) => ExitStatus::Exited {
                    code: status.exit_code(),
                    signal: status.signal().map(str::to_owned),
                },
                Err(err) => {
                    tracing::warn!(%err, "waiting for a live session's program failed");
                    ExitStatus::Exited { code: 1, signal: None }
                }
            };
            actor.shutdown();
            tracing::debug!(?status, "a live session's program ended");
            core.end(status);
        })
        .map(drop)
}

/// The pane's threads: the PTY actor, the replies' helper and the child's waiter. When one cannot start, the
/// program is ended (it would never get its output read) and so is what started.
fn start_threads(id: u64, master: agterm_pty::Master, child: Child, core: &Arc<Core>) -> io::Result<ActorHandle> {
    let killer = child.killer();
    let actor = match start_actor(id, master, core, false) {
        Ok(actor) => actor,
        Err(err) => {
            let _ = killer.kill_tree();
            return Err(err);
        }
    };
    if let Err(err) = start_helper(id, core, &actor) {
        let _ = killer.kill_tree();
        actor.shutdown();
        return Err(err);
    }
    if let Err(err) = start_waiter(id, child, core, &actor) {
        let _ = killer.kill_tree();
        actor.shutdown();
        // the helper ends with the session
        core.end(ExitStatus::Exited { code: 1, signal: None });
        return Err(err);
    }
    Ok(actor)
}

/// The replies' helper thread ([`run_helper`]), until the session's end.
fn start_helper(id: u64, core: &Arc<Core>, actor: &ActorHandle) -> io::Result<()> {
    let core = Arc::clone(core);
    let actor = actor.clone();
    std::thread::Builder::new()
        .name(format!("live-{id}-replies"))
        .spawn(move || run_helper(&core, &actor))
        .map(drop)
}

/// The one writer of the terminal's replies: it takes the replies ready under the pane's lock and writes each batch
/// before it takes the next, so the program reads them in the order they were made ready, whichever thread made them
/// (a read of the PTY, a client's answer, a resize, a deadline). It also denies a clipboard read whose time is up or
/// that was cancelled, ends the Kitty barriers whose time is up, and lets the PTY be read again once the output held
/// behind a read went through. It sleeps on [`Core::timer`] until the next deadline, and ends once the session ended.
fn run_helper(core: &Core, actor: &ActorHandle) {
    let mut state = core.lock();
    while state.exit.is_none() {
        if state.hold.frozen {
            // a handoff's cut: nothing is written or ended until a thaw wakes it
            state = core
                .timer
                .wait_timeout(state, HELPER_IDLE)
                .map_or_else(|poisoned| poisoned.into_inner().0, |(guard, _)| guard);
            continue;
        }
        let now = Instant::now();
        state.expire(now);
        let replies = core.outlet.take();
        let wake = core.unpause(&state);
        if !replies.is_empty() || wake {
            state.hold.helper_busy = true;
            drop(state);
            for reply in replies {
                actor.write_reply(&reply);
            }
            if wake {
                actor.wake_reader();
            }
            state = core.lock();
            state.hold.helper_busy = false;
            core.changed.notify_all();
            continue;
        }
        let wait = state
            .replies
            .deadline()
            .map_or(HELPER_IDLE, |deadline| deadline.saturating_duration_since(now));
        state = core
            .timer
            .wait_timeout(state, wait)
            .map_or_else(|poisoned| poisoned.into_inner().0, |(guard, _)| guard);
    }
}

/// The program's environment for the session `name` of the namespace `namespace`: the spec's `env` as given (a later
/// assignment of a name replacing the earlier one in place), without [`MANAGED_VARIABLES`], with
/// [`SESSION_VARIABLE`] and [`DIRECTORY_VARIABLE`] set, and `TERM` set to [`FALLBACK_TERM`] when it is unset or
/// `dumb`. Names compare without case on Windows, as its environment does.
#[must_use]
pub fn child_environment(env: &[(String, String)], name: &str, namespace: &Path) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::with_capacity(env.len() + 3);
    for (key, value) in env {
        set(&mut out, key, value.clone());
    }
    out.retain(|(key, _)| !MANAGED_VARIABLES.iter().any(|managed| same_name(key, managed)));
    set(&mut out, SESSION_VARIABLE, name.to_owned());
    set(&mut out, DIRECTORY_VARIABLE, namespace.to_string_lossy().into_owned());
    if lookup(&out, "TERM").is_none_or(|term| term == "dumb") {
        set(&mut out, "TERM", FALLBACK_TERM.to_owned());
    }
    out
}

/// Assign `key` in `env`: in place when it is there, else at the end.
fn set(env: &mut Vec<(String, String)>, key: &str, value: String) {
    match env.iter_mut().find(|(existing, _)| same_name(existing, key)) {
        Some((_, existing)) => *existing = value,
        None => env.push((key.to_owned(), value)),
    }
}

fn same_name(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

fn lookup<'a>(env: &'a [(String, String)], name: &str) -> Option<&'a str> {
    env.iter()
        .find(|(key, _)| same_name(key, name))
        .map(|(_, value)| value.as_str())
}

/// The `names` of `env` with their values, in the order of `names`.
fn tracked(env: &[(String, String)], names: &[String]) -> Vec<(String, String)> {
    names
        .iter()
        .filter_map(|name| lookup(env, name).map(|value| (name.clone(), value.to_owned())))
        .collect()
}

fn unix_seconds(time: SystemTime) -> u64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Why [`Panes::spawn`] started no session.
#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    /// A session of that name runs.
    #[error("a live session named {0:?} already exists")]
    Exists(String),
    #[error("the live session could not be started: {0}")]
    Io(#[from] io::Error),
}

/// The server's panes by session name. A pane is forgotten once its program ended. Clones share the panes.
#[derive(Debug, Clone, Default)]
pub struct Panes {
    panes: Arc<Mutex<HashMap<String, Arc<Pane>>>>,
}

impl Panes {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Start the session `config` names, its creator attached ([`Pane::spawn`]), and hold it until its program
    /// ends.
    ///
    /// # Errors
    ///
    /// [`SpawnError::Exists`] while a session of that name runs; [`Pane::spawn`]'s errors.
    pub fn spawn(&self, config: PaneConfig) -> Result<(Arc<Pane>, Attached), SpawnError> {
        let mut panes = lock(&self.panes);
        if panes.get(&config.name).is_some_and(|pane| pane.exit_status().is_none()) {
            return Err(SpawnError::Exists(config.name));
        }
        let (pane, creator) = Pane::spawn(config)?;
        let pane = Arc::new(pane);
        panes.insert(pane.name.clone(), Arc::clone(&pane));
        drop(panes);
        self.forget_on_end(&pane);
        Ok((pane, creator))
    }

    /// Hold `pane`, one an update handoff brought over ([`Pane::import`]), until its program ends, as
    /// [`Panes::spawn`] holds a new one; it replaces a pane of its name (a server taking over has none).
    pub fn adopt(&self, pane: Pane) -> Arc<Pane> {
        let pane = Arc::new(pane);
        let replaced = lock(&self.panes).insert(pane.name.clone(), Arc::clone(&pane));
        drop(replaced);
        self.forget_on_end(&pane);
        pane
    }

    /// Every pane held, sorted by name.
    #[must_use]
    pub fn all(&self) -> Vec<Arc<Pane>> {
        let mut panes: Vec<Arc<Pane>> = lock(&self.panes).values().cloned().collect();
        panes.sort_unstable_by(|a, b| a.name.cmp(&b.name));
        panes
    }

    /// Forget `pane` once its program ended, unless a later session of its name took its place.
    fn forget_on_end(&self, pane: &Arc<Pane>) {
        let registry: Weak<Mutex<HashMap<String, Arc<Pane>>>> = Arc::downgrade(&self.panes);
        let (name, id) = (pane.name.clone(), pane.id);
        pane.on_end(Box::new(move || {
            let Some(registry) = registry.upgrade() else {
                return;
            };
            let mut panes = lock(&registry);
            // a later session of the same name stays
            if panes.get(&name).is_some_and(|pane| pane.id == id) {
                let ended = panes.remove(&name);
                drop(panes);
                drop(ended);
            }
        }));
    }

    /// The running session `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<Pane>> {
        lock(&self.panes).get(name).cloned()
    }

    /// Stop holding the session `name`; it ends once the last holder of the pane lets go.
    #[must_use]
    pub fn remove(&self, name: &str) -> Option<Arc<Pane>> {
        lock(&self.panes).remove(name)
    }

    /// The running sessions' names, sorted.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = lock(&self.panes).keys().cloned().collect();
        names.sort_unstable();
        names
    }

    #[must_use]
    pub fn len(&self) -> usize {
        lock(&self.panes).len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ColorScheme;
    use crate::server::clients::ClientEvent;

    /// A pane's state with no program (none is needed to feed it), its creator owning the replies with `metadata`.
    fn core(metadata: ReplyMetadata) -> (Arc<Core>, Attached) {
        let spec = SessionSpec {
            argv: vec!["none".to_owned()],
            env: Vec::new(),
            cwd: None,
            tracked_env: Vec::new(),
        };
        let mut config = PaneConfig::new("unit", "unit", spec, GridSize { cols: 80, rows: 24 });
        config.metadata = metadata;
        new_core(&config, config.size, None).unwrap()
    }

    #[test]
    fn a_reply_made_off_the_ptys_thread_is_not_overtaken_by_a_later_reads() {
        let (core, creator) = core(ReplyMetadata::default());
        let owner = creator.subscription.id();
        // the read callback hands no reply back itself: the helper thread writes them all
        assert!(core.read(b"\x1b[?2031h").is_empty());
        // off the PTY's thread, under the pane's lock: the owner turns dark, a mode 2031 report is made
        let dark = ReplyMetadata {
            color_scheme: Some(ColorScheme::Dark),
            ..ReplyMetadata::default()
        };
        core.lock().remember(owner, dark);
        // a read after it: its answer goes behind the report
        assert!(core.read(b"\x1b[5n").is_empty());
        assert_eq!(core.outlet.take(), [b"\x1b[?997;1n".to_vec(), b"\x1b[0n".to_vec()]);
    }

    #[test]
    fn the_replies_after_a_kitty_command_wait_for_the_owners_answers() {
        let shows = ReplyMetadata {
            kitty_graphics: true,
            ..ReplyMetadata::default()
        };
        let (core, creator) = core(shows);
        let owner = creator.subscription.id();
        let query = b"\x1b_Ga=q,i=31,s=1,v=1,t=d,f=24;AAAA\x1b\\";
        assert!(
            core.read(&[b"\x1b[5n".as_slice(), query, b"\x1b[c"].concat())
                .is_empty()
        );
        // the answer before the command goes; DA1's waits
        assert_eq!(core.outlet.take(), [b"\x1b[0n".to_vec()]);
        // the owner was sent the output up to the command's ESC, then the barrier, then the rest
        let mut events = Vec::new();
        while let Ok(event) = creator.subscription.recv_timeout(Duration::ZERO) {
            events.push(event);
        }
        let barrier = events
            .iter()
            .position(|event| matches!(event, ClientEvent::KittyBarrier { .. }))
            .unwrap();
        let ClientEvent::KittyBarrier { id, epoch } = events[barrier].clone() else {
            unreachable!()
        };
        let ClientEvent::Output { data, .. } = &events[barrier - 1] else {
            panic!("{events:?}")
        };
        assert!(data.ends_with(&query[..query.len() - 1]), "{data:?}");
        assert!(matches!(&events[barrier + 1], ClientEvent::Output { data, .. } if data == b"\\\x1b[c"));
        // whole Kitty answers only: anything else ends it empty
        let ok = b"\x1b_Gi=31;OK\x1b\\";
        assert!(core.lock().complete(owner, id, epoch, ok));
        assert!(!core.lock().complete(owner, id, epoch, ok), "a second completion");
        assert_eq!(core.outlet.take(), [ok.to_vec(), b"\x1b[?62;22;52c".to_vec()]);
        assert!(core.read(&[query.as_slice(), b"\x1b[c"].concat()).is_empty());
        let next = id + 1;
        assert!(!core.lock().complete(owner, next, epoch, b"\x1b[c"));
        assert_eq!(core.outlet.take(), [b"\x1b[?62;22;52c".to_vec()]);
    }

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn the_environment_is_the_specs_with_the_sessions_variables() {
        let env = pairs(&[
            ("PATH", "/bin"),
            ("TERM", "xterm-ghostty"),
            ("AGTERM_LIVE_MANAGED", "nonce"),
            ("AGTERM_LIVE_MANAGED_CLAIM", "1"),
            ("AGTERM_LIVE_SESSION", "outer"),
            ("PATH", "/usr/bin"),
        ]);
        let out = child_environment(&env, "agterm-1", Path::new("/tmp/ngterm-live-x"));
        assert_eq!(
            out,
            pairs(&[
                ("PATH", "/usr/bin"),
                ("TERM", "xterm-ghostty"),
                ("AGTERM_LIVE_SESSION", "agterm-1"),
                ("AGTERM_LIVE_DIR", "/tmp/ngterm-live-x"),
            ])
        );
    }

    #[test]
    fn term_is_set_when_unset_or_dumb() {
        for env in [pairs(&[]), pairs(&[("TERM", "dumb")])] {
            let out = child_environment(&env, "s", Path::new("d"));
            assert_eq!(lookup(&out, "TERM"), Some(FALLBACK_TERM));
        }
        let out = child_environment(&pairs(&[("TERM", "")]), "s", Path::new("d"));
        assert_eq!(lookup(&out, "TERM"), Some(""), "an empty TERM is set, and kept");
    }

    #[test]
    fn names_compare_as_the_os_compares_them() {
        let env = pairs(&[("Path", "a"), ("agterm_live_managed", "n")]);
        let out = child_environment(&env, "s", Path::new("d"));
        if cfg!(windows) {
            assert_eq!(lookup(&out, "PATH"), Some("a"));
            assert!(lookup(&out, "AGTERM_LIVE_MANAGED").is_none());
        } else {
            assert_eq!(lookup(&out, "PATH"), None);
            assert_eq!(lookup(&out, "agterm_live_managed"), Some("n"));
        }
    }

    #[test]
    fn tracked_variables_keep_the_specs_order_and_skip_the_missing() {
        let env = pairs(&[("A", "1"), ("B", "2")]);
        let names = ["B".to_owned(), "C".to_owned(), "A".to_owned()];
        assert_eq!(tracked(&env, &names), pairs(&[("B", "2"), ("A", "1")]));
    }

    #[test]
    fn the_working_directory_falls_back_to_the_home_of_the_environment() {
        let home_name = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        let env = pairs(&[(home_name, "/home/someone")]);
        let spec = |cwd: Option<&str>| SessionSpec {
            argv: vec!["prog".to_owned(), "arg".to_owned()],
            env: Vec::new(),
            cwd: cwd.map(str::to_owned),
            tracked_env: Vec::new(),
        };
        let given = command(&spec(Some("/work")), &env).unwrap();
        assert_eq!(given.cwd.as_deref(), Some(Path::new("/work")));
        assert_eq!(given.args, [OsString::from("arg")]);
        assert!(given.env_clear);
        let fallback = command(&spec(None), &env).unwrap();
        assert_eq!(fallback.cwd.as_deref(), Some(Path::new("/home/someone")));
        assert_eq!(command(&spec(None), &[]).unwrap().cwd, None);
    }

    #[test]
    fn an_empty_argv_is_refused() {
        let spec = SessionSpec {
            argv: Vec::new(),
            env: Vec::new(),
            cwd: None,
            tracked_env: Vec::new(),
        };
        let err = command(&spec, &[]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn a_pre_epoch_clock_reads_as_zero() {
        assert_eq!(unix_seconds(SystemTime::UNIX_EPOCH), 0);
        assert_eq!(unix_seconds(SystemTime::UNIX_EPOCH + Duration::from_secs(5)), 5);
    }
}
