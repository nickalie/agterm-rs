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
//! the terminal had then. The terminal's replies to the program's queries (`write_pty`) go back to the program, in
//! order with its input. It loads no Kitty images from files, temporary files or shared memory: an image shown that
//! way is read once, by the client that shows it.
//!
//! # The end
//!
//! When the program exits, the pane waits up to [`EXIT_GRACE`] for the PTY's end, so output written before the exit
//! reaches the terminal and every client first, then queues [`ClientEvent::Exit`](super::clients::ClientEvent) with
//! the program's code and signal (what `agterm-term`'s `ExitPolicy` decides on) for every client and detaches them.
//! [`Panes`] then forgets the session.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant, SystemTime};

use agterm_ghostty_vt as vt;
use agterm_pty::{Actor, ActorConfig, ActorHandle, Child, ExitReason, Killer, Size, Spawned, WriteError};

use super::clients::{ClientId, Clients, DEFAULT_QUEUE_LIMIT, Subscription};
use crate::protocol::{ExitStatus, GridSize, SessionSpec};

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
/// How often a kill's grace looks at what of the program's session lives on (herdr's poll).
#[cfg(unix)]
const SESSION_POLL: Duration = Duration::from_millis(20);
/// How long the waiter holds off reaping the program for a kill under way, which holds it for the hangup, at most
/// [`KILL_GRACE`] and the kill; past it the program is reaped, and the kill then signals nothing.
const REAP_HOLD_LIMIT: Duration = Duration::from_secs(5);

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
    /// The grid the session starts with.
    pub size: GridSize,
    /// The output bytes each client's queue holds.
    pub client_queue_limit: usize,
}

impl PaneConfig {
    #[must_use]
    pub fn new(name: impl Into<String>, namespace: impl Into<PathBuf>, spec: SessionSpec, size: GridSize) -> Self {
        Self {
            name: name.into(),
            namespace: namespace.into(),
            spec,
            size,
            client_queue_limit: DEFAULT_QUEUE_LIMIT,
        }
    }
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
    core: Arc<Core>,
    actor: ActorHandle,
    killer: Killer,
}

impl std::fmt::Debug for Pane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pane")
            .field("name", &self.name)
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

/// What the PTY actor's callbacks, the child's waiter and the pane share.
struct Core {
    /// The pane's one serialization point: its terminal, its clients and its output offset change together.
    state: Mutex<State>,
    /// Notified when the PTY ends and when the program's end is recorded.
    changed: Condvar,
    /// The terminal's replies of the write in progress.
    replies: Arc<Mutex<Vec<Vec<u8>>>>,
}

struct State {
    terminal: vt::Terminal,
    clients: Clients,
    /// The session's output bytes so far: the offset of the next read.
    offset: u64,
    size: GridSize,
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
    on_end: Option<Box<dyn FnOnce() + Send>>,
}

impl Core {
    fn lock(&self) -> MutexGuard<'_, State> {
        // a panic inside the terminal's callbacks leaves the state as the last full read made it
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Feed a read of the PTY to the terminal and the clients; the terminal's replies.
    fn read(&self, bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut state = self.lock();
        if state.exit.is_some() {
            return Vec::new();
        }
        state.terminal.vt_write(bytes);
        let offset = state.offset;
        state.offset += bytes.len() as u64;
        state.clients.output(offset, bytes);
        drop(state);
        std::mem::take(&mut *lock(&self.replies))
    }

    fn pty_ended(&self) {
        self.lock().pty_ended = true;
        self.changed.notify_all();
    }

    fn child_exited(&self) {
        self.lock().exited = true;
        self.changed.notify_all();
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

    /// Record the program's end and tell every client.
    fn end(&self, status: ExitStatus) {
        let mut state = self.lock();
        state.clients.exit(&status);
        state.exit = Some(status);
        let on_end = state.on_end.take();
        drop(state);
        self.changed.notify_all();
        if let Some(on_end) = on_end {
            on_end();
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The server terminal's callbacks: its replies go back to the program; everything else is the clients' to show.
struct ServerEffects {
    replies: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl vt::Effects for ServerEffects {
    fn write_pty(&mut self, data: &[u8]) {
        lock(&self.replies).push(data.to_vec());
    }
}

impl Pane {
    /// Start the session `config` names: its program on a PTY of `config.size`, with the environment
    /// [`child_environment`] makes of its spec, in its spec's working directory (else the home directory of that
    /// environment). The client creating it is attached before the program runs, so it is sent all of its output
    /// from offset zero on, as an attach that creates its session is.
    ///
    /// # Errors
    ///
    /// [`io::ErrorKind::InvalidInput`] for an empty argv; the terminal could not be created, the PTY opened, the
    /// program started or the PTY's thread started.
    pub fn spawn(config: PaneConfig) -> io::Result<(Self, Attached)> {
        let PaneConfig {
            name,
            namespace,
            spec,
            size,
            client_queue_limit,
        } = config;
        let size = GridSize {
            cols: size.cols.max(1),
            rows: size.rows.max(1),
        };
        let env = child_environment(&spec.env, &name, &namespace);
        let command = command(&spec, &env)?;
        let replies = Arc::new(Mutex::new(Vec::new()));
        let terminal = server_terminal(size, &replies)?;
        let Spawned { master, child } = agterm_pty::spawn(&command, Size::cells(size.rows, size.cols))?;
        let mut clients = Clients::new(client_queue_limit);
        let creator = Attached {
            subscription: clients.attach(NEXT_CLIENT.fetch_add(1, Ordering::Relaxed)),
            offset: 0,
            size,
        };
        let core = Arc::new(Core {
            state: Mutex::new(State {
                terminal,
                clients,
                offset: 0,
                size,
                pty_ended: false,
                exited: false,
                reap_holds: 0,
                reaping: false,
                exit: None,
                on_end: None,
            }),
            changed: Condvar::new(),
            replies,
        });
        let id = NEXT_PANE.fetch_add(1, Ordering::Relaxed);
        let killer = child.killer();
        let pid = child.pid();
        let actor = match start_actor(id, master, &core) {
            Ok(actor) => actor,
            Err(err) => {
                // the program never gets its output read: end it
                let _ = killer.kill_tree();
                return Err(err);
            }
        };
        if let Err(err) = start_waiter(id, child, &core, &actor) {
            let _ = killer.kill_tree();
            actor.shutdown();
            return Err(err);
        }
        tracing::debug!(session = %name, pid, "live session started");
        let pane = Self {
            id,
            name,
            pid,
            created_at: unix_seconds(SystemTime::now()),
            tracked_env: tracked(&env, &spec.tracked_env),
            core,
            actor,
            killer,
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

    /// Attach a client: it is sent the session's output from the offset returned on. `None` once the program ended.
    #[must_use]
    pub fn attach(&self) -> Option<Attached> {
        let mut state = self.core.lock();
        if state.exit.is_some() {
            return None;
        }
        let subscription = state.clients.attach(NEXT_CLIENT.fetch_add(1, Ordering::Relaxed));
        Some(Attached {
            subscription,
            offset: state.offset,
            size: state.size,
        })
    }

    /// Detach the client `id`; whether it was attached.
    #[must_use]
    pub fn detach(&self, id: ClientId) -> bool {
        self.core.lock().clients.detach(id)
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
    /// changes nothing.
    pub fn resize(&self, size: GridSize) {
        let size = GridSize {
            cols: size.cols.max(1),
            rows: size.rows.max(1),
        };
        let mut state = self.core.lock();
        if state.exit.is_some() || state.size == size {
            return;
        }
        if let Err(err) = state.terminal.resize(size.cols, size.rows, 0, 0) {
            tracing::warn!(session = %self.name, %err, "resizing the session's terminal failed");
        }
        self.actor.resize(Size::cells(size.rows, size.cols));
        state.size = size;
        let offset = state.offset;
        state.clients.resized(offset, size);
        // a size report the program asked for (in-band resize notifications); written once the lock is let go, as
        // the PTY's read callback may be waiting for it
        let replies = std::mem::take(&mut *lock(&self.core.replies));
        drop(state);
        for reply in replies {
            self.actor.write_reply(&reply);
        }
    }

    /// Run `read` on the session's terminal, under the pane's lock (no output is fed meanwhile).
    pub fn with_terminal<R>(&self, read: impl FnOnce(&mut vt::Terminal) -> R) -> R {
        read(&mut self.core.lock().terminal)
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
        if !self.core.hold_reap() {
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

    /// Run `on_end` once the program's end is recorded (now, if it already is).
    fn on_end(&self, on_end: Box<dyn FnOnce() + Send>) {
        let mut state = self.core.lock();
        if state.exit.is_none() {
            state.on_end = Some(on_end);
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
        if self.exit_status().is_none()
            && let Err(err) = self.end_tree()
        {
            tracing::warn!(session = %self.name, %err, "ending a dropped session's program failed");
        }
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

/// The server's terminal for a session of `size`: its replies into `replies`, no Kitty image media.
fn server_terminal(size: GridSize, replies: &Arc<Mutex<Vec<Vec<u8>>>>) -> io::Result<vt::Terminal> {
    let vt_error = |err: vt::Error| io::Error::other(format!("the session's terminal: {err}"));
    let mut terminal = vt::Terminal::new(size.cols, size.rows).map_err(vt_error)?;
    terminal.set_kitty_image_media(false, None, false).map_err(vt_error)?;
    terminal
        .set_effects(Box::new(ServerEffects {
            replies: Arc::clone(replies),
        }))
        .map_err(vt_error)?;
    Ok(terminal)
}

/// The PTY actor: every read into the terminal and the clients, the terminal's replies back to the program.
fn start_actor(id: u64, master: agterm_pty::Master, core: &Arc<Core>) -> io::Result<ActorHandle> {
    let reading = Arc::clone(core);
    let mut config = ActorConfig::new(format!("live-{id}"), master, Box::new(move |bytes| reading.read(bytes)));
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
                core.child_exited();
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
        Ok((pane, creator))
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
        let out = child_environment(&env, "agterm-1", Path::new("/tmp/agterm-live-x"));
        assert_eq!(
            out,
            pairs(&[
                ("PATH", "/usr/bin"),
                ("TERM", "xterm-ghostty"),
                ("AGTERM_LIVE_SESSION", "agterm-1"),
                ("AGTERM_LIVE_DIR", "/tmp/agterm-live-x"),
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
