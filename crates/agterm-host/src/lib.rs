//! Host seams of agterm: what the platform-neutral core and control server need from a frontend.
//!
//! - [`MainLoop`]: posts work to the thread that owns the app state (the GTK main thread on Linux).
//! - [`Surface`]: one live terminal (libghostty in a widget), owned on the main thread.
//! - [`SessionHost`]: the daemons that keep live sessions alive across app restarts (zmx).
//! - [`ProcessLauncher`]: starts helper processes (overlay programs, hooks, custom commands).
//!
//! In-memory fakes of each live in [`fakes`] behind the `fakes` feature.

use std::fmt;
use std::io;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::Duration;

#[cfg(any(test, feature = "fakes"))]
pub mod fakes;

/// A unit of work for the main thread, run with the state the main loop owns.
pub type MainTask<S> = Box<dyn FnOnce(&mut S) + Send + 'static>;

/// The main loop is gone: the task was dropped without running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MainLoopClosed;

impl fmt::Display for MainLoopClosed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the main loop has shut down")
    }
}

impl std::error::Error for MainLoopClosed {}

/// The thread that owns the app state `S`. Any thread may post; tasks run one at a time, in posting order, on the
/// owning thread. A task accepted by [`MainLoop::post`] may still be dropped unrun if the loop shuts down first, so a
/// caller waiting on a task's answer must treat a dropped reply channel as "closed".
pub trait MainLoop<S>: Send + Sync {
    /// Queues `task` for the main thread.
    ///
    /// # Errors
    ///
    /// [`MainLoopClosed`] when the loop no longer accepts work; `task` is dropped unrun.
    fn post(&self, task: MainTask<S>) -> Result<(), MainLoopClosed>;

    /// Queues `task` for the main thread once `delay` has passed: a timer on the loop, for debounced work (snapshot
    /// writes, `tree.changed`). A loop that shuts down first drops the task unrun.
    ///
    /// # Errors
    ///
    /// [`MainLoopClosed`] when the loop no longer accepts work; `task` is dropped unrun.
    fn post_after(&self, delay: Duration, task: MainTask<S>) -> Result<(), MainLoopClosed>;
}

/// How much of a terminal's text [`Surface::read_text`] returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextScope {
    /// The visible screen.
    Screen,
    /// Scrollback plus the screen.
    All,
    /// The last `n` lines of scrollback plus screen.
    LastLines(NonZeroU32),
}

/// One live terminal: a libghostty surface with its shell, owned by a session pane on the main thread.
pub trait Surface {
    /// The stable per-surface identity baked into the shell as `AGTERM_PANE_ID`; empty for a surface spawned with
    /// no pane identity (overlay, quick terminal).
    fn pane_token(&self) -> &str;

    /// Whether the terminal exists (the libghostty surface was created and its program spawned), as opposed to the
    /// widget merely occupying a pane slot while creation waits for a nonzero size.
    fn is_realized(&self) -> bool;

    /// Whether the pane is attached through a [`SessionHost`] daemon rather than owning its shell.
    fn is_live_session(&self) -> bool;

    /// Reassigns a split survivor to the primary role, so its pwd and title reports reach the session's main fields.
    fn promote_to_primary(&mut self);

    /// Sends `text` to the terminal as typed input.
    fn type_text(&mut self, text: &str);

    /// The terminal's text for `scope`; `None` when the surface is not realized or the read failed.
    fn read_text(&self, scope: TextScope) -> Option<String>;

    /// Frees the libghostty surface and its shell. Taking the box makes a second teardown impossible.
    fn teardown(self: Box<Self>);
}

/// A failure of a [`SessionHost`] operation, with the host's own message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostError(pub String);

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HostError {}

/// One live-session daemon as the host lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveSessionRecord {
    /// The daemon's name (derived from the pane identity).
    pub name: String,
    /// Attached clients; `None` when the host reported none.
    pub clients: Option<u32>,
    /// The pid of the shell the daemon runs.
    pub leader_pid: Option<i32>,
    /// When the daemon started, in whole seconds since the Unix epoch.
    pub created_at: Option<u64>,
}

/// What a kill actually did. Only [`KillOutcome::Killed`] means the shell is gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KillOutcome {
    /// The daemon and its shell were killed.
    Killed,
    /// Only a stale socket was removed; no daemon answered on it.
    StaleSocket,
    /// The kill failed, with the host's first line of output.
    Failed(String),
}

/// The daemons that keep live sessions running while no app is attached (zmx). Called from the main thread and
/// from worker threads, hence `Send + Sync`.
pub trait SessionHost: Send + Sync {
    /// Whether the host binary is staged and usable.
    fn is_available(&self) -> bool;

    /// Every daemon in this instance's namespace.
    ///
    /// # Errors
    ///
    /// The listing could not be read or parsed. That is not an empty namespace: a caller must not act on it as one.
    fn list(&self) -> Result<Vec<LiveSessionRecord>, HostError>;

    /// Kills the daemon `name`; `force` also removes a stale socket.
    fn kill(&self, name: &str, force: bool) -> KillOutcome;

    /// The daemon's own screen for `name`, with scrollback when `all`.
    ///
    /// # Errors
    ///
    /// The read failed for any reason, an unknown name included.
    fn screen(&self, name: &str, all: bool) -> Result<String, HostError>;

    /// Queues `bytes` as typed input to `name` without taking the lead. Never retried by the host: a second attempt
    /// after an unclear failure would type twice.
    ///
    /// # Errors
    ///
    /// The daemon did not queue the bytes.
    fn type_input(&self, name: &str, bytes: &[u8]) -> Result<(), HostError>;
}

/// A helper process to start.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchSpec {
    /// The executable, a path or a name looked up in `PATH`.
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Variables set on top of the app's own environment.
    pub env: Vec<(String, String)>,
    /// The working directory; the app's own when `None`.
    pub cwd: Option<PathBuf>,
    /// Bytes written to the process's stdin, which is then closed; stdin is null when `None`.
    pub stdin: Option<Vec<u8>>,
}

/// A started helper process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaunchedProcess {
    pub pid: u32,
}

/// Starts helper processes (overlay programs, hooks, custom commands) and reaps them.
pub trait ProcessLauncher: Send + Sync {
    /// Starts `spec` detached; the launcher reaps it.
    ///
    /// # Errors
    ///
    /// The process could not be started.
    fn launch(&self, spec: &LaunchSpec) -> io::Result<LaunchedProcess>;
}
