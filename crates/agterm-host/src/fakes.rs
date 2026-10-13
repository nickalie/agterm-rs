//! In-memory fakes of the host traits, for tests that run the core or the control server without a window, a
//! terminal or a daemon. Each records what was asked of it so a test can assert on it afterwards.

use std::any::Any;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread::{self, JoinHandle, ThreadId};
use std::time::Duration;

use crate::{
    ExitHandler, HostError, KillOutcome, LaunchSpec, LaunchedProcess, LiveScreen, LiveSessionRecord, MainLoop,
    MainLoopClosed, MainTask, ProcessExit, ProcessLauncher, SessionHost, Surface, TextScope,
};

/// A fake never leaves a lock poisoned on purpose; a panicking test thread must not cascade into every other one.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the loop's thread hands back when it ends: the state, for a loop started with a state that can cross threads.
type Returned = Option<Box<dyn Any + Send>>;

/// A main loop on a thread of its own that owns the state `S` and runs posted tasks in order.
pub struct FakeMainLoop<S> {
    /// `None` once shut down. Shared with the timers of [`MainLoop::post_after`], which post through it only while
    /// the loop is open, so a pending timer neither holds the loop open nor runs after a shutdown.
    sender: Arc<Mutex<Option<mpsc::Sender<MainTask<S>>>>>,
    thread: Mutex<Option<JoinHandle<Returned>>>,
    thread_id: ThreadId,
}

impl<S: Send + 'static> FakeMainLoop<S> {
    /// Starts the loop's thread with `state`.
    ///
    /// # Errors
    ///
    /// The thread could not be spawned.
    pub fn spawn(state: S) -> io::Result<Self> {
        Self::start(move || state, |state| Some(Box::new(state)))
    }

    /// Stops accepting tasks, lets the queued ones run, and returns the state; a delayed task whose timer is still
    /// pending is dropped unrun. `None` when already shut down or when a
    /// task panicked and took the thread with it.
    pub fn shutdown(&self) -> Option<S> {
        let returned = self.join()??;
        returned.downcast::<S>().ok().map(|state| *state)
    }
}

impl<S: 'static> FakeMainLoop<S> {
    /// Starts the loop's thread with the state `make` builds there: a state that must not cross threads, as the
    /// app's own, which holds its terminals, never does.
    ///
    /// # Errors
    ///
    /// The thread could not be spawned.
    pub fn spawn_with(make: impl FnOnce() -> S + Send + 'static) -> io::Result<Self> {
        Self::start(make, |_| None)
    }

    fn start(
        make: impl FnOnce() -> S + Send + 'static,
        finish: impl FnOnce(S) -> Returned + Send + 'static,
    ) -> io::Result<Self> {
        let (sender, receiver) = mpsc::channel::<MainTask<S>>();
        let handle = thread::Builder::new()
            .name("fake-main-loop".to_owned())
            .spawn(move || {
                let mut state = make();
                for task in receiver {
                    task(&mut state);
                }
                finish(state)
            })?;
        let thread_id = handle.thread().id();
        Ok(Self {
            sender: Arc::new(Mutex::new(Some(sender))),
            thread: Mutex::new(Some(handle)),
            thread_id,
        })
    }

    /// The id of the thread tasks run on.
    #[must_use]
    pub fn thread_id(&self) -> ThreadId {
        self.thread_id
    }

    /// Stops accepting tasks and lets the queued ones run, as [`FakeMainLoop::shutdown`] without returning the state.
    /// False when already shut down or when a task panicked and took the thread with it.
    pub fn close(&self) -> bool {
        self.join().is_some()
    }

    /// Closes the channel and joins the thread; `None` when already joined or the thread panicked.
    fn join(&self) -> Option<Returned> {
        lock(&self.sender).take();
        let handle = lock(&self.thread).take()?;
        handle.join().ok()
    }
}

impl<S: 'static> MainLoop<S> for FakeMainLoop<S> {
    fn post(&self, task: MainTask<S>) -> Result<(), MainLoopClosed> {
        let sender = lock(&self.sender);
        let sender = sender.as_ref().ok_or(MainLoopClosed)?;
        sender.send(task).map_err(|_| MainLoopClosed)
    }

    /// A timer thread posts the task when the delay is up, if the loop is still open then. As the trait says, a
    /// [`FakeMainLoop::shutdown`] before that drops the task unrun, and it does not wait for the timer.
    fn post_after(&self, delay: Duration, task: MainTask<S>) -> Result<(), MainLoopClosed> {
        if lock(&self.sender).is_none() {
            return Err(MainLoopClosed);
        }
        let sender = Arc::clone(&self.sender);
        thread::Builder::new()
            .name("fake-main-loop-timer".to_owned())
            .spawn(move || {
                thread::sleep(delay);
                if let Some(sender) = lock(&sender).as_ref() {
                    let _ = sender.send(task);
                }
            })
            .map(drop)
            .map_err(|_| MainLoopClosed)
    }
}

#[derive(Debug, Default)]
struct SurfaceState {
    realized: bool,
    lines: Vec<String>,
    selection: Option<String>,
    cursor_column: Option<u32>,
    foreground_pid: Option<u32>,
    cell_size: Option<(f64, f64)>,
    typed: Vec<String>,
    actions: Vec<String>,
    promotions: u32,
    teardowns: u32,
}

/// A terminal whose text is set by the test and whose input is recorded. Main-thread only, like a real surface.
///
/// `select_all` selects every line; any other binding action is only recorded.
pub struct FakeSurface {
    token: String,
    live_session: bool,
    rows: usize,
    state: Rc<RefCell<SurfaceState>>,
}

/// The test's handle on a [`FakeSurface`], still usable after the surface is boxed or torn down.
#[derive(Clone)]
pub struct FakeSurfaceProbe {
    state: Rc<RefCell<SurfaceState>>,
}

impl FakeSurface {
    /// The visible screen height [`TextScope::Screen`] reads.
    pub const ROWS: usize = 24;

    /// The font size a realized fake reports.
    pub const FONT_SIZE: f64 = 13.0;

    /// A realized surface that owns its shell, with pane token `token` and no text.
    #[must_use]
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            live_session: false,
            rows: Self::ROWS,
            state: Rc::new(RefCell::new(SurfaceState {
                realized: true,
                ..SurfaceState::default()
            })),
        }
    }

    /// Sets whether the terminal exists yet.
    #[must_use]
    pub fn with_realized(self, realized: bool) -> Self {
        self.state.borrow_mut().realized = realized;
        self
    }

    /// Sets whether the pane is attached through a live-session daemon.
    #[must_use]
    pub fn with_live_session(mut self, live_session: bool) -> Self {
        self.live_session = live_session;
        self
    }

    /// The test's handle on this surface.
    #[must_use]
    pub fn probe(&self) -> FakeSurfaceProbe {
        FakeSurfaceProbe {
            state: Rc::clone(&self.state),
        }
    }
}

impl FakeSurfaceProbe {
    /// Replaces the terminal's text, scrollback first, one entry per line.
    pub fn set_lines<I: IntoIterator<Item = S>, S: Into<String>>(&self, lines: I) {
        self.state.borrow_mut().lines = lines.into_iter().map(Into::into).collect();
    }

    /// Sets whether the terminal exists, as its creation landing later would.
    pub fn set_realized(&self, realized: bool) {
        self.state.borrow_mut().realized = realized;
    }

    /// Sets the selection [`Surface::read_selection`] reports.
    pub fn set_selection(&self, selection: Option<&str>) {
        self.state.borrow_mut().selection = selection.map(str::to_owned);
    }

    /// Sets the column [`Surface::cursor_column`] reports.
    pub fn set_cursor_column(&self, column: Option<u32>) {
        self.state.borrow_mut().cursor_column = column;
    }

    /// Sets the cell [`Surface::cell_size`] reports while realized, in points.
    pub fn set_cell_size(&self, cell: Option<(f64, f64)>) {
        self.state.borrow_mut().cell_size = cell;
    }

    /// Sets the pid [`Surface::foreground_pid`] reports.
    pub fn set_foreground_pid(&self, pid: Option<u32>) {
        self.state.borrow_mut().foreground_pid = pid;
    }

    /// Every binding action run, in order.
    #[must_use]
    pub fn actions(&self) -> Vec<String> {
        self.state.borrow().actions.clone()
    }

    /// Every [`Surface::type_text`] call, in order.
    #[must_use]
    pub fn typed(&self) -> Vec<String> {
        self.state.borrow().typed.clone()
    }

    /// How many times the surface was promoted to the primary role.
    #[must_use]
    pub fn promotions(&self) -> u32 {
        self.state.borrow().promotions
    }

    /// How many times the surface was torn down.
    #[must_use]
    pub fn teardowns(&self) -> u32 {
        self.state.borrow().teardowns
    }
}

impl Surface for FakeSurface {
    fn pane_token(&self) -> &str {
        &self.token
    }

    fn is_realized(&self) -> bool {
        self.state.borrow().realized
    }

    fn is_live_session(&self) -> bool {
        self.live_session
    }

    fn promote_to_primary(&mut self) {
        self.state.borrow_mut().promotions += 1;
    }

    fn type_text(&mut self, text: &str) -> bool {
        let mut state = self.state.borrow_mut();
        if !state.realized {
            return false;
        }
        state.typed.push(text.to_owned());
        true
    }

    fn read_text(&self, scope: TextScope) -> Option<String> {
        let state = self.state.borrow();
        if !state.realized {
            return None;
        }
        let keep = match scope {
            TextScope::Screen => self.rows,
            TextScope::All => state.lines.len(),
        };
        let start = state.lines.len().saturating_sub(keep);
        Some(state.lines[start..].join("\n"))
    }

    fn read_selection(&self) -> Option<String> {
        let state = self.state.borrow();
        if !state.realized {
            return None;
        }
        state.selection.clone().filter(|selection| !selection.is_empty())
    }

    fn cursor_column(&self) -> Option<u32> {
        let state = self.state.borrow();
        state.cursor_column.filter(|_| state.realized)
    }

    fn binding_action(&mut self, action: &str) -> bool {
        let mut state = self.state.borrow_mut();
        if !state.realized {
            return false;
        }
        if action == "select_all" {
            state.selection = Some(state.lines.join("\n"));
        }
        state.actions.push(action.to_owned());
        true
    }

    fn foreground_pid(&self) -> Option<u32> {
        let state = self.state.borrow();
        state.foreground_pid.filter(|_| state.realized)
    }

    fn font_size(&self) -> Option<f64> {
        self.state.borrow().realized.then_some(Self::FONT_SIZE)
    }

    fn cell_size(&self) -> Option<(f64, f64)> {
        let state = self.state.borrow();
        state.cell_size.filter(|_| state.realized)
    }

    fn teardown(self: Box<Self>) {
        self.state.borrow_mut().teardowns += 1;
    }
}

/// One daemon of a [`FakeSessionHost`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeLiveSession {
    pub record: LiveSessionRecord,
    pub screen: String,
    pub scrollback: String,
    /// Every byte queued by [`SessionHost::type_input`].
    pub typed: Vec<u8>,
}

impl FakeLiveSession {
    /// The grid every fake daemon reports.
    pub const COLUMNS: u32 = 80;
    pub const ROWS: u32 = 24;
}

/// A live-session host over an in-memory set of daemons.
pub struct FakeSessionHost {
    available: bool,
    sessions: Mutex<BTreeMap<String, FakeLiveSession>>,
    list_error: Mutex<Option<String>>,
    /// Every kill asked for, in order: the name and whether it was forced (a batch records each name forced).
    kills: Mutex<Vec<(String, bool)>>,
    /// The outcome a kill of a name reports instead of killing it.
    kill_outcomes: Mutex<BTreeMap<String, KillOutcome>>,
    /// The foreground job each shell pid reports.
    jobs: Mutex<BTreeMap<i32, Vec<crate::ProcessRecord>>>,
    /// The job processes still running.
    running: Mutex<Vec<crate::ProcessRecord>>,
    /// The job processes ended by [`SessionHost::end_job`].
    ended: Mutex<Vec<crate::ProcessRecord>>,
}

impl Default for FakeSessionHost {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeSessionHost {
    /// An available host with no daemons.
    #[must_use]
    pub fn new() -> Self {
        Self {
            available: true,
            sessions: Mutex::default(),
            list_error: Mutex::default(),
            kills: Mutex::default(),
            kill_outcomes: Mutex::default(),
            jobs: Mutex::default(),
            running: Mutex::default(),
            ended: Mutex::default(),
        }
    }

    /// A host whose binary is not staged: nothing can be listed or run.
    #[must_use]
    pub fn unavailable() -> Self {
        Self {
            available: false,
            ..Self::new()
        }
    }

    /// Adds (or replaces) the daemon `name` with one attached client and `screen` as its text.
    pub fn insert(&self, name: impl Into<String>, screen: impl Into<String>) {
        let name = name.into();
        let session = FakeLiveSession {
            record: LiveSessionRecord {
                name: name.clone(),
                clients: Some(1),
                leader_pid: None,
                created_at: None,
            },
            screen: screen.into(),
            scrollback: String::new(),
            typed: Vec::new(),
        };
        lock(&self.sessions).insert(name, session);
    }

    /// The daemon `name` as it stands now.
    #[must_use]
    pub fn session(&self, name: &str) -> Option<FakeLiveSession> {
        lock(&self.sessions).get(name).cloned()
    }

    /// Replaces the listing row of daemon `name` (inserting a blank daemon when there is none).
    pub fn set_record(&self, name: &str, clients: Option<u32>, leader_pid: Option<i32>, created_at: Option<u64>) {
        let mut sessions = lock(&self.sessions);
        let session = sessions.entry(name.to_owned()).or_insert_with(|| FakeLiveSession {
            record: LiveSessionRecord {
                name: name.to_owned(),
                clients: None,
                leader_pid: None,
                created_at: None,
            },
            screen: String::new(),
            scrollback: String::new(),
            typed: Vec::new(),
        });
        session.record = LiveSessionRecord {
            name: name.to_owned(),
            clients,
            leader_pid,
            created_at,
        };
    }

    /// Sets the scrollback [`SessionHost::screen`] adds before the screen when asked for everything.
    pub fn set_scrollback(&self, name: &str, scrollback: impl Into<String>) {
        if let Some(session) = lock(&self.sessions).get_mut(name) {
            session.scrollback = scrollback.into();
        }
    }

    /// Makes every later kill of `name` report `outcome` and leave the daemon alone; `None` kills it again.
    pub fn set_kill_outcome(&self, name: &str, outcome: Option<KillOutcome>) {
        let mut outcomes = lock(&self.kill_outcomes);
        match outcome {
            Some(outcome) => outcomes.insert(name.to_owned(), outcome),
            None => outcomes.remove(name),
        };
    }

    /// Every kill asked for, in order, with whether it was forced.
    #[must_use]
    pub fn kills(&self) -> Vec<(String, bool)> {
        lock(&self.kills).clone()
    }

    /// Makes `shell` report `job` as its foreground job, every process of it running until ended.
    pub fn set_job(&self, shell: i32, job: Vec<crate::ProcessRecord>) {
        lock(&self.running).extend(job.iter().copied());
        lock(&self.jobs).insert(shell, job);
    }

    /// Ends process `pid` of a job, as its exit would.
    pub fn finish_job_process(&self, pid: i32) {
        lock(&self.running).retain(|process| process.pid != pid);
    }

    /// The job processes [`SessionHost::end_job`] killed.
    #[must_use]
    pub fn ended(&self) -> Vec<crate::ProcessRecord> {
        lock(&self.ended).clone()
    }

    /// Makes every later [`SessionHost::list`] fail with `message`.
    pub fn fail_list(&self, message: impl Into<String>) {
        *lock(&self.list_error) = Some(message.into());
    }

    /// Lists again after [`FakeSessionHost::fail_list`].
    pub fn heal_list(&self) {
        *lock(&self.list_error) = None;
    }

    fn require_available(&self) -> Result<(), HostError> {
        if self.available {
            Ok(())
        } else {
            Err(HostError("the session host is not available".to_owned()))
        }
    }
}

impl SessionHost for FakeSessionHost {
    fn is_available(&self) -> bool {
        self.available
    }

    fn list(&self) -> Result<Vec<LiveSessionRecord>, HostError> {
        self.require_available()?;
        if let Some(message) = lock(&self.list_error).clone() {
            return Err(HostError(message));
        }
        Ok(lock(&self.sessions).values().map(|s| s.record.clone()).collect())
    }

    fn kill(&self, name: &str, force: bool) -> KillOutcome {
        if let Err(error) = self.require_available() {
            return KillOutcome::Failed(error.0);
        }
        lock(&self.kills).push((name.to_owned(), force));
        if let Some(outcome) = lock(&self.kill_outcomes).get(name) {
            return outcome.clone();
        }
        match lock(&self.sessions).remove(name) {
            Some(_) => KillOutcome::Killed,
            None => KillOutcome::Failed(format!("session not found: {name}")),
        }
    }

    fn screen(&self, name: &str, all: bool) -> Result<LiveScreen, HostError> {
        self.require_available()?;
        let sessions = lock(&self.sessions);
        let session = sessions
            .get(name)
            .ok_or_else(|| HostError(format!("session not found: {name}")))?;
        let text = if all {
            format!("{}{}", session.scrollback, session.screen)
        } else {
            session.screen.clone()
        };
        Ok(LiveScreen {
            columns: FakeLiveSession::COLUMNS,
            rows: FakeLiveSession::ROWS,
            text,
            ..LiveScreen::default()
        })
    }

    fn type_input(&self, name: &str, bytes: &[u8]) -> Result<(), HostError> {
        self.require_available()?;
        let mut sessions = lock(&self.sessions);
        let session = sessions
            .get_mut(name)
            .ok_or_else(|| HostError(format!("session not found: {name}")))?;
        session.typed.extend_from_slice(bytes);
        Ok(())
    }

    fn foreground_job(&self, shell: i32) -> Option<Vec<crate::ProcessRecord>> {
        Some(lock(&self.jobs).get(&shell).cloned().unwrap_or_default())
    }

    fn job_running(&self, job: &[crate::ProcessRecord]) -> bool {
        let running = lock(&self.running);
        job.iter().any(|process| running.contains(process))
    }

    fn end_job(&self, job: &[crate::ProcessRecord]) {
        let mut running = lock(&self.running);
        for process in job {
            if running.contains(process) {
                running.retain(|other| other != process);
                lock(&self.ended).push(*process);
            }
        }
    }
}

/// A launcher that records each launch and hands out increasing pids without starting anything; a test ends a
/// process with [`FakeProcessLauncher::finish`].
pub struct FakeProcessLauncher {
    launches: Mutex<Vec<LaunchSpec>>,
    exits: Mutex<BTreeMap<u32, ExitHandler>>,
    next_pid: AtomicU32,
    failure: Mutex<Option<io::ErrorKind>>,
}

impl Default for FakeProcessLauncher {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeProcessLauncher {
    /// The first pid handed out.
    pub const FIRST_PID: u32 = 1000;

    #[must_use]
    pub fn new() -> Self {
        Self {
            launches: Mutex::default(),
            exits: Mutex::default(),
            next_pid: AtomicU32::new(Self::FIRST_PID),
            failure: Mutex::default(),
        }
    }

    /// Makes every later launch fail with `kind` (`None` restores success). Failed launches are not recorded.
    pub fn fail_with(&self, kind: Option<io::ErrorKind>) {
        *lock(&self.failure) = kind;
    }

    /// Every successful launch, in order.
    #[must_use]
    pub fn launches(&self) -> Vec<LaunchSpec> {
        lock(&self.launches).clone()
    }

    /// The pids launched and not finished yet, ascending.
    #[must_use]
    pub fn running(&self) -> Vec<u32> {
        lock(&self.exits).keys().copied().collect()
    }

    /// Ends process `pid` with `exit`, calling its exit handler on this thread. False for an unknown or finished pid.
    pub fn finish(&self, pid: u32, exit: ProcessExit) -> bool {
        let handler = lock(&self.exits).remove(&pid);
        handler.map(|handler| handler(exit)).is_some()
    }
}

impl ProcessLauncher for FakeProcessLauncher {
    fn launch(&self, spec: &LaunchSpec, on_exit: ExitHandler) -> io::Result<LaunchedProcess> {
        if let Some(kind) = *lock(&self.failure) {
            return Err(io::Error::new(
                kind,
                format!("fake launch of {} failed", spec.program.display()),
            ));
        }
        lock(&self.launches).push(spec.clone());
        let pid = self.next_pid.fetch_add(1, Ordering::Relaxed);
        lock(&self.exits).insert(pid, on_exit);
        Ok(LaunchedProcess { pid })
    }
}

/// An in-memory process table: the processes a test put in it.
#[derive(Debug, Default)]
pub struct FakeProcessTable {
    processes: std::cell::RefCell<Vec<(crate::ProcessStat, Option<Vec<String>>)>>,
    /// The terminal foreground group each process reports.
    foregrounds: std::cell::RefCell<BTreeMap<i32, i32>>,
    /// The working directory each process reports.
    directories: std::cell::RefCell<BTreeMap<i32, String>>,
}

impl FakeProcessTable {
    /// Makes process `pid` report `pgrp` as its terminal's foreground group.
    pub fn set_terminal_foreground(&self, pid: i32, pgrp: i32) {
        self.foregrounds.borrow_mut().insert(pid, pgrp);
    }

    /// Makes process `pid` report `directory` as its working directory.
    pub fn set_working_directory(&self, pid: i32, directory: &str) {
        self.directories.borrow_mut().insert(pid, directory.to_owned());
    }

    /// Adds process `stat` with `argv` (`None` for one whose argv cannot be read), replacing one with its pid.
    pub fn insert(&self, stat: crate::ProcessStat, argv: Option<&[&str]>) {
        let argv = argv.map(|argv| argv.iter().map(|arg| (*arg).to_owned()).collect());
        let mut processes = self.processes.borrow_mut();
        processes.retain(|(known, _)| known.pid != stat.pid);
        processes.push((stat, argv));
    }

    /// Removes process `pid`, as when it exits.
    pub fn remove(&self, pid: i32) {
        self.processes.borrow_mut().retain(|(known, _)| known.pid != pid);
    }
}

impl crate::ProcessTable for FakeProcessTable {
    fn argv(&self, pid: i32) -> Option<Vec<String>> {
        self.processes
            .borrow()
            .iter()
            .find(|(stat, _)| stat.pid == pid)
            .and_then(|(_, argv)| argv.clone())
    }

    fn stat(&self, pid: i32) -> Option<crate::ProcessStat> {
        self.processes
            .borrow()
            .iter()
            .find(|(stat, _)| stat.pid == pid)
            .map(|(stat, _)| *stat)
    }

    fn group(&self, pgrp: i32) -> Vec<crate::ProcessStat> {
        self.processes
            .borrow()
            .iter()
            .filter(|(stat, _)| stat.pgrp == pgrp)
            .map(|(stat, _)| *stat)
            .collect()
    }

    fn terminal_foreground(&self, pid: i32) -> Option<i32> {
        self.foregrounds.borrow().get(&pid).copied()
    }

    fn working_directory(&self, pid: i32) -> Option<String> {
        self.directories.borrow().get(&pid).cloned()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::mpsc;

    use super::*;

    #[test]
    fn main_loop_runs_tasks_in_order_on_its_own_thread() {
        let main_loop = Arc::new(FakeMainLoop::spawn(Vec::<u32>::new()).unwrap());
        let (tx, rx) = mpsc::channel();
        for n in 0..5 {
            let tx = tx.clone();
            main_loop
                .post(Box::new(move |state: &mut Vec<u32>| {
                    state.push(n);
                    tx.send(thread::current().id()).unwrap();
                }))
                .unwrap();
        }
        for _ in 0..5 {
            assert_eq!(rx.recv().unwrap(), main_loop.thread_id());
        }
        assert_ne!(main_loop.thread_id(), thread::current().id());
        assert_eq!(main_loop.shutdown(), Some(vec![0, 1, 2, 3, 4]));
    }

    #[test]
    fn main_loop_runs_a_delayed_task_after_its_delay() {
        let main_loop = FakeMainLoop::spawn(Vec::<&str>::new()).unwrap();
        let (tx, rx) = mpsc::channel();
        let posted = std::time::Instant::now();
        main_loop
            .post_after(
                Duration::from_millis(50),
                Box::new(move |state: &mut Vec<&str>| {
                    state.push("late");
                    tx.send(posted.elapsed()).unwrap();
                }),
            )
            .unwrap();
        main_loop
            .post(Box::new(|state: &mut Vec<&str>| state.push("now")))
            .unwrap();
        assert!(rx.recv().unwrap() >= Duration::from_millis(50));
        assert_eq!(main_loop.shutdown(), Some(vec!["now", "late"]));
    }

    /// As `MainLoop::post_after` promises and the `GLib` loop does: a loop shut down before the delay is up drops the
    /// task, and the shutdown does not wait for the timer.
    #[test]
    fn a_delayed_task_of_a_shut_down_loop_never_runs() {
        let main_loop = FakeMainLoop::spawn(Vec::<&str>::new()).unwrap();
        let (tx, rx) = mpsc::channel::<()>();
        main_loop
            .post_after(
                Duration::from_millis(200),
                Box::new(move |state: &mut Vec<&str>| {
                    state.push("late");
                    tx.send(()).unwrap();
                }),
            )
            .unwrap();
        let started = std::time::Instant::now();
        assert_eq!(main_loop.shutdown(), Some(Vec::new()));
        assert!(
            started.elapsed() < Duration::from_millis(200),
            "shutdown waited for the timer"
        );
        // the timer thread drops the task, and with it the reply channel, without running it
        assert_eq!(rx.recv(), Err(mpsc::RecvError));
    }

    #[test]
    fn a_state_that_cannot_cross_threads_is_built_and_kept_on_the_loop() {
        let main_loop = FakeMainLoop::spawn_with(|| Rc::new(RefCell::new(0_u8))).unwrap();
        let (tx, rx) = mpsc::channel();
        main_loop
            .post(Box::new(move |state: &mut Rc<RefCell<u8>>| {
                *state.borrow_mut() += 1;
                tx.send(*state.borrow()).unwrap();
            }))
            .unwrap();
        assert_eq!(rx.recv(), Ok(1));
        assert!(main_loop.close());
        assert!(!main_loop.close());
        assert_eq!(main_loop.post(Box::new(|_| {})), Err(MainLoopClosed));
    }

    #[test]
    fn main_loop_refuses_work_after_shutdown() {
        let main_loop = FakeMainLoop::spawn(0_u8).unwrap();
        assert_eq!(main_loop.shutdown(), Some(0));
        assert_eq!(main_loop.post(Box::new(|_| {})), Err(MainLoopClosed));
        assert_eq!(
            main_loop.post_after(Duration::ZERO, Box::new(|_| {})),
            Err(MainLoopClosed)
        );
        assert_eq!(main_loop.shutdown(), None);
    }

    #[test]
    fn surface_reads_scopes_and_records_input() {
        let surface = FakeSurface::new("pane-1");
        let probe = surface.probe();
        probe.set_lines((0..30).map(|n| format!("line {n}")));
        let mut surface: Box<dyn Surface> = Box::new(surface);
        assert_eq!(surface.pane_token(), "pane-1");
        let screen = surface.read_text(TextScope::Screen).unwrap();
        assert_eq!(screen.lines().count(), FakeSurface::ROWS);
        assert!(screen.starts_with("line 6\n"));
        assert_eq!(surface.read_text(TextScope::All).unwrap().lines().count(), 30);
        assert!(surface.type_text("echo hi\n"));
        assert_eq!(surface.read_selection(), None);
        assert!(surface.binding_action("select_all"));
        assert_eq!(surface.read_selection().unwrap().lines().count(), 30);
        probe.set_cursor_column(Some(5));
        assert_eq!(surface.cursor_column(), Some(5));
        surface.promote_to_primary();
        surface.teardown();
        assert_eq!(probe.typed(), ["echo hi\n"]);
        assert_eq!(probe.actions(), ["select_all"]);
        assert_eq!(probe.promotions(), 1);
        assert_eq!(probe.teardowns(), 1);
    }

    #[test]
    fn unrealized_surface_has_no_text() {
        let surface = FakeSurface::new("").with_realized(false).with_live_session(true);
        assert!(!surface.is_realized());
        assert!(surface.is_live_session());
        assert_eq!(surface.read_text(TextScope::All), None);
        let mut surface: Box<dyn Surface> = Box::new(surface);
        assert!(!surface.type_text("x"));
        assert!(!surface.binding_action("select_all"));
        assert_eq!(surface.cursor_column(), None);
    }

    #[test]
    fn session_host_lists_types_and_kills() {
        let host = FakeSessionHost::new();
        host.insert("agterm-a", "$ ");
        host.type_input("agterm-a", b"ls\n").unwrap();
        assert_eq!(host.session("agterm-a").unwrap().typed, b"ls\n");
        let screen = host.screen("agterm-a", false).unwrap();
        assert_eq!(screen.text, "$ ");
        assert_eq!((screen.columns, screen.rows), (80, 24));
        assert_eq!(host.list().unwrap().len(), 1);
        assert_eq!(host.kill("agterm-a", true), KillOutcome::Killed);
        assert!(matches!(host.kill("agterm-a", true), KillOutcome::Failed(_)));
        assert!(host.screen("agterm-a", false).is_err());
        host.fail_list("zmx list timed out");
        assert_eq!(host.list(), Err(HostError("zmx list timed out".to_owned())));
    }

    #[test]
    fn unavailable_session_host_refuses_everything() {
        let host = FakeSessionHost::unavailable();
        assert!(!host.is_available());
        assert!(host.list().is_err());
        assert!(host.type_input("x", b"").is_err());
    }

    #[test]
    fn launcher_records_launches_and_can_fail() {
        let launcher = FakeProcessLauncher::new();
        let spec = LaunchSpec {
            program: "/bin/true".into(),
            ..LaunchSpec::default()
        };
        assert_eq!(
            launcher.launch(&spec, Box::new(|_| ())).unwrap().pid,
            FakeProcessLauncher::FIRST_PID
        );
        assert_eq!(
            launcher.launch(&spec, Box::new(|_| ())).unwrap().pid,
            FakeProcessLauncher::FIRST_PID + 1
        );
        launcher.fail_with(Some(io::ErrorKind::NotFound));
        assert_eq!(
            launcher.launch(&spec, Box::new(|_| ())).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(launcher.launches(), [spec.clone(), spec]);
    }

    #[test]
    fn launcher_finishes_a_process_once() {
        let launcher = FakeProcessLauncher::new();
        let (tx, rx) = mpsc::channel();
        let pid = launcher
            .launch(
                &LaunchSpec::default(),
                Box::new(move |exit| tx.send(exit.status).unwrap()),
            )
            .unwrap()
            .pid;
        assert_eq!(launcher.running(), [pid]);
        let exit = ProcessExit {
            status: 3,
            ..ProcessExit::default()
        };
        assert!(launcher.finish(pid, exit.clone()));
        assert!(!launcher.finish(pid, exit));
        assert_eq!(rx.try_recv(), Ok(3));
        assert!(launcher.running().is_empty());
    }
}
