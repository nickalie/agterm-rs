//! Host seams of agterm: what the platform-neutral core and control server need from a frontend.
//!
//! - [`MainLoop`]: posts work to the thread that owns the app state (the GPUI main thread in `agterm-gpui`).
//! - [`Surface`]: one live terminal (libghostty-vt in the frontend's terminal element), owned on the main thread.
//! - [`SessionHost`]: the daemons that keep live sessions alive across app restarts (zmx).
//! - [`ProcessLauncher`]: starts helper processes (overlay programs, hooks, custom commands).
//! - [`ProcessTable`]: reads the process table (a pane's foreground program for the tree and the restore capture).
//!
//! In-memory fakes of each live in [`fakes`] behind the `fakes` feature.

use std::fmt;
use std::io;
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
    /// The visible screen (the viewport).
    Screen,
    /// Scrollback plus the screen.
    All,
}

/// One live terminal: a libghostty surface with its shell, owned by a session pane on the main thread (upstream's
/// `TerminalSurface` plus the `GhosttySurfaceView` I/O the control commands reach).
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

    /// Types `text` into the terminal as keystrokes through the keyboard's own path, never as a paste (upstream's
    /// `inject(text:)`): each printable run as key text, each line ending (`\n`, `\r`, `\r\n`) as one Return
    /// keypress, and a final Return after text sent only after `agterm_core::keystrokes::SUBMIT_GAP`. False when the
    /// terminal is not realized, so nothing was queued.
    fn type_text(&mut self, text: &str) -> bool;

    /// The terminal's text for `scope`, trailing blank rows included; an empty string for a blank screen. `None` only
    /// when the surface is not realized or the read failed.
    fn read_text(&self, scope: TextScope) -> Option<String>;

    /// The current selection's text; `None` with no or an empty selection, or no realized terminal.
    fn read_selection(&self) -> Option<String>;

    /// The cursor's zero-based column; `None` when the terminal is not realized or the geometry cannot be trusted
    /// (upstream's `readCursorColumn`).
    fn cursor_column(&self) -> Option<u32>;

    /// Runs a libghostty binding action on this terminal (`paste_from_clipboard`, `select_all`, `start_search`,
    /// `search:<needle>`, `navigate_search:next`, `end_search`, `increase_font_size:1`, ...). False when the terminal
    /// is not realized, so nothing ran.
    fn binding_action(&mut self, action: &str) -> bool;

    /// The pid of the terminal's foreground process; `None` when not realized or unknown.
    fn foreground_pid(&self) -> Option<u32>;

    /// The terminal's live font size in points; `None` when not realized (upstream's `currentFontSize`).
    fn font_size(&self) -> Option<f64>;

    /// The cell the terminal draws, width and height in points; `None` when not realized or unknown (upstream's
    /// `cellSize()`, which sizes a HUD panel).
    fn cell_size(&self) -> Option<(f64, f64)>;

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

/// A daemon's own terminal as the host reads it (upstream's `ZmxScreen`): its grid, the cursor and the text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveScreen {
    /// Counts the daemon's screen changes.
    pub revision: u64,
    pub columns: u32,
    pub rows: u32,
    /// Zero-based, in the active screen.
    pub cursor_column: u32,
    /// Zero-based, in the active screen.
    pub cursor_row: u32,
    /// The alternate screen is active.
    pub alternate: bool,
    /// The screen's text, verbatim: one line per row, trailing blank rows included.
    pub text: String,
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

/// One process of a terminal's foreground job, as a sweep matches it again later: by pid AND start time, so a pid the
/// kernel recycled meanwhile is never signalled (upstream's `ProcessRecord`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProcessRecord {
    pub pid: i32,
    /// When the process started, in the platform's own clock (Linux: clock ticks since boot).
    pub started: u64,
    /// Its process group.
    pub group: i32,
    /// The foreground process group of its controlling terminal; zero or negative without one.
    pub foreground: i32,
}

/// The daemons that keep live sessions running while no app is attached (zmx). Called from the main thread and
/// from worker threads, hence `Send + Sync`.
///
/// A confirmed [`SessionHost::kill`] or [`SessionHost::kill_batch`] also hangs up the foreground job the killed
/// daemon's shell leaves running, as closing a terminal would have (upstream's `ProcessSweeper`): the daemon signals
/// only the shell's own process group, and a pane's program runs in another.
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

    /// The daemon's own screen for `name`, with scrollback when `all`. It always has the leader's layout, so a caller
    /// must not fall back to a pane's own surface when the read fails: that layout is the thing in doubt.
    ///
    /// # Errors
    ///
    /// The read failed for any reason, an unknown name and a host without the query included; never a blank screen.
    fn screen(&self, name: &str, all: bool) -> Result<LiveScreen, HostError>;

    /// Queues `bytes` as typed input to `name` without taking the lead. Never retried by the host: a second attempt
    /// after an unclear failure would type twice.
    ///
    /// # Errors
    ///
    /// The daemon did not queue the bytes.
    fn type_input(&self, name: &str, bytes: &[u8]) -> Result<(), HostError>;

    /// Kills every daemon of `names` in one forced batch (the pane finalizer and the reset's launch kill). The result
    /// is diagnostic only: a batch failing part-way has already reached some daemons.
    fn kill_batch(&self, names: &[String]) -> bool {
        // every name is killed, a failed one included
        let killed: Vec<bool> = names
            .iter()
            .map(|name| matches!(self.kill(name, true), KillOutcome::Killed))
            .collect();
        killed.into_iter().all(|killed| killed)
    }

    /// The foreground job of the terminal the shell `shell` leads, for a caller that waits for it to end; empty when
    /// the shell holds its own foreground. `None` when the process table cannot be read: a caller waiting for the job
    /// must not take that for "no job". A host that never signals jobs answers empty.
    fn foreground_job(&self, _shell: i32) -> Option<Vec<ProcessRecord>> {
        Some(Vec::new())
    }

    /// Whether any process of `job` still runs; an unreadable table counts as running.
    fn job_running(&self, _job: &[ProcessRecord]) -> bool {
        false
    }

    /// Kills what is left of `job` (SIGKILL), for a restart whose old program ignored the hangup.
    fn end_job(&self, _job: &[ProcessRecord]) {}
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
    /// Capture the tail of what the process writes to stderr (to a temp file, never a pipe, so a descendant outliving
    /// the app is not killed by a broken pipe) and hand it back on exit; stderr is null otherwise.
    pub capture_stderr: bool,
}

/// A started helper process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaunchedProcess {
    pub pid: u32,
}

/// How a launched process ended, delivered once, after its stdin was written and closed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessExit {
    /// The exit status, or the number of the signal that ended the process.
    pub status: i32,
    /// The last [`STDERR_TAIL_LIMIT`] bytes the process wrote to stderr, when captured.
    pub stderr_tail: Vec<u8>,
    /// Why writing `stdin` failed, other than the process leaving without reading it (a broken pipe).
    pub stdin_failure: Option<String>,
}

/// How much of a captured stderr comes back (upstream's `CommandFailure.tailLimit`).
pub const STDERR_TAIL_LIMIT: usize = 16 * 1024;

/// Receives a launched process's [`ProcessExit`], on a thread of the launcher's.
pub type ExitHandler = Box<dyn FnOnce(ProcessExit) + Send + 'static>;

/// Starts helper processes (overlay programs, hooks, custom commands) and reaps them.
pub trait ProcessLauncher: Send + Sync {
    /// Starts `spec` detached; the launcher reaps it and calls `on_exit` exactly once. Never calls `on_exit` when the
    /// launch fails.
    ///
    /// # Errors
    ///
    /// The process could not be started.
    fn launch(&self, spec: &LaunchSpec, on_exit: ExitHandler) -> io::Result<LaunchedProcess>;
}

/// One process as the process table reports it: its id, its parent's, its process group's and its session's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessStat {
    pub pid: i32,
    pub ppid: i32,
    /// The process group id.
    pub pgrp: i32,
    /// The session id: the pid of the session leader (a terminal's spawned child).
    pub session: i32,
}

/// Reads the process table for a pane's foreground process (upstream's `sysctl` reads in
/// `Ghostty/ForegroundProcess.swift`). Only ever asked about processes of this app's own terminals.
pub trait ProcessTable {
    /// The argv of process `pid`, each argument decoded lossily (an undecodable byte becomes U+FFFD); `None` when the
    /// process is gone, has no argv (a zombie, a kernel thread) or cannot be read.
    fn argv(&self, pid: i32) -> Option<Vec<String>>;

    /// The ids of process `pid`; `None` when it is gone or cannot be read.
    fn stat(&self, pid: i32) -> Option<ProcessStat>;

    /// Every live member of process group `pgrp`; empty when none can be read.
    fn group(&self, pgrp: i32) -> Vec<ProcessStat>;

    /// The foreground process group of the terminal process `pid` controls (`tpgid`): how a live pane's foreground is
    /// read through its daemon's shell, whose terminal the pane's own surface does not hold. `None` when the process
    /// is gone, has no controlling terminal, or cannot be read.
    fn terminal_foreground(&self, _pid: i32) -> Option<i32> {
        None
    }

    /// The working directory of process `pid`; `None` when it cannot be read (another user's process).
    fn working_directory(&self, _pid: i32) -> Option<String> {
        None
    }
}
