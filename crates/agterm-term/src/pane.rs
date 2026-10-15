//! [`Pane`]: one terminal — a libghostty-vt terminal behind a mutex, a PTY child, and three threads: the IO thread
//! (reads the PTY and feeds the VT), the writer (drains the bounded write queue into the PTY), and the waiter (reports
//! the child's exit).
//!
//! Locks (Technical Details > Threads): `vt` → `capture` → leaves (`armed`, `published`, `divert`), and `vt` →
//! `search`, never the reverse. The IO thread holds `vt` for one bounded slice of `vt_write` at a time; input
//! ([`crate::input`]), selection and read-back hold it for one encode or format; the search worker
//! ([`crate::search`]) for one feed, and ticks with `search` alone. Painting ([`Pane::frame`]) `try_lock`s `vt` and
//! then `capture`, keeps `vt` only for the begin phase of the capture, and draws the previous frame when either is
//! busy. Effect callbacks run under `vt` and copy into channels, except the render-hold callback, which takes `capture`
//! (VT → capture, the one order) and captures the held frame there; nothing may take `vt` while holding `capture`.
//!
//! A synchronized-output hold the program never ends is ended after a second by whichever comes first: a paint, or
//! the IO thread, which `poll`s with that deadline while a hold is on and wakes the frontend.

use std::fs::File;
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, TryLockError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use agterm_ghostty_vt as vt;
use portable_pty::{Child, MasterPty, native_pty_system};

use crate::effects::{
    ArmedRead, ArmedSlot, ClipboardAnswer, ClipboardReadRequest, DivertSlot, PaneEffects, PaneEvent, TerminalColors,
};
use crate::exit::{ExitPolicy, Layout};
use crate::input::InputState;
use crate::pty::{self, GridSize, PaneCommand, Ready, WakePipe, WriteQueue};
use crate::render::{Capture, CursorDefaults, Frame, MouseShape, ThemeColors};
use crate::scan::{ReadCandidate, Scanner};
use crate::search::SearchSlot;
use crate::{PaneError, ScrollbackLimits};

/// The most bytes one read batch gathers before the VT sees them.
const READ_BATCH: usize = 256 * 1024;
/// The most bytes fed to `vt_write` under one hold of the VT lock.
const VT_SLICE: usize = 64 * 1024;
/// How long the exit report waits for the PTY's end, so output written before the exit is processed first.
const EXIT_GRACE: Duration = Duration::from_secs(1);
/// The default limit of the write queue (input waiting for the child).
pub const DEFAULT_WRITE_LIMIT: usize = 1024 * 1024;
/// Ghostty's default Kitty image storage per screen (`image-storage-limit = 320000000`).
pub const DEFAULT_KITTY_IMAGE_LIMIT: u64 = 320_000_000;

/// Called from the pane's threads when it has news (a frame to draw or events to drain); coalesced until the frontend
/// calls [`Pane::frame`] or [`Pane::take_events`].
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// How a pane starts.
#[derive(Clone)]
pub struct PaneConfig {
    pub size: GridSize,
    pub scrollback: ScrollbackLimits,
    pub colors: ThemeColors,
    /// The cursor the terminal starts with and returns to.
    pub cursor: CursorDefaults,
    /// The most input bytes queued for the child before [`Pane::write`] refuses more.
    pub write_limit: usize,
    /// The Kitty image storage per screen in bytes; zero turns the Kitty graphics protocol off.
    pub kitty_image_limit: u64,
    /// What the end of the program leaves on screen.
    pub exit: ExitPolicy,
    pub wake: Option<Wake>,
}

impl PaneConfig {
    #[must_use]
    pub fn new(size: GridSize) -> Self {
        Self {
            size,
            scrollback: ScrollbackLimits::default(),
            colors: ThemeColors::default(),
            cursor: CursorDefaults::default(),
            write_limit: DEFAULT_WRITE_LIMIT,
            kitty_image_limit: DEFAULT_KITTY_IMAGE_LIMIT,
            exit: ExitPolicy::default(),
            wake: None,
        }
    }
}

impl std::fmt::Debug for PaneConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PaneConfig")
            .field("size", &self.size)
            .field("scrollback", &self.scrollback)
            .field("colors", &self.colors)
            .field("cursor", &self.cursor)
            .field("write_limit", &self.write_limit)
            .field("kitty_image_limit", &self.kitty_image_limit)
            .field("exit", &self.exit)
            .field("wake", &self.wake.is_some())
            .finish()
    }
}

/// What a paint got: the frame to draw, and whether it is older than the terminal because a lock was busy (draw it,
/// and ask for another frame).
#[derive(Debug, Clone)]
pub struct FrameSnapshot {
    pub frame: Arc<Frame>,
    pub stale: bool,
}

/// Counters of the read path and the VT lock (profiling and the lock-latency check).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PaneStats {
    /// Bytes fed to the VT.
    pub bytes: u64,
    /// `read` calls that returned data.
    pub reads: u64,
    /// Read batches (one wake each at most).
    pub batches: u64,
    /// Frontend wakes actually fired (after coalescing).
    pub wakes: u64,
    /// Time spent in `vt_write`.
    pub vt_nanos: u64,
    /// The longest single hold of the VT lock by the IO thread.
    pub max_hold_nanos: u64,
}

#[derive(Debug, Default)]
struct Counters {
    bytes: AtomicU64,
    reads: AtomicU64,
    batches: AtomicU64,
    wakes: AtomicU64,
    vt_nanos: AtomicU64,
    max_hold_nanos: AtomicU64,
}

/// What the VT lock guards.
pub(crate) struct Vt {
    pub(crate) terminal: vt::Terminal,
    /// The key and mouse encoders.
    pub(crate) input: InputState,
    /// The grid and cell size the terminal was last resized to.
    pub(crate) size: GridSize,
    /// An input path cleared the selection (taken before the lock is let go, to wake the frontend).
    pub(crate) cleared_selection: bool,
    /// The colors last reported with [`PaneEvent::Colors`].
    colors: vt::TerminalColors,
    /// The pointer's selection gesture, created at the first press ([`crate::gesture`]).
    pub(crate) gesture: Option<vt::SelectionGesture>,
    /// A `clear_screen` waits for the program's output to reach a ground point ([`crate::actions`]).
    pub(crate) pending_clear: bool,
}

impl Vt {
    /// Whether an input path cleared the selection under this lock (taking the mark).
    pub(crate) fn take_cleared_selection(&mut self) -> bool {
        std::mem::take(&mut self.cleared_selection)
    }

    /// Scroll the viewport to the bottom; whether it moved.
    pub(crate) fn scroll_to_bottom(&mut self) -> bool {
        if self.terminal.viewport_at_bottom() {
            return false;
        }
        self.terminal.scroll_viewport(vt::Scroll::Bottom);
        true
    }
}

/// `vt_write` of one slice of the program's output; a pending `clear_screen` goes in at the slice's first ground
/// point ([`crate::actions`]).
fn write_slice(shared: &Shared, vt: &mut Vt, slice: &[u8]) {
    if !vt.pending_clear {
        vt.terminal.vt_write(slice);
        return;
    }
    // without a ground point the whole slice was in the middle of something: the clear waits for the next one
    if let Some(consumed) = vt.terminal.vt_write_until_ground(slice) {
        crate::actions::apply_clear(shared, vt);
        vt.terminal.vt_write(&slice[consumed..]);
    }
}

/// Messages to the IO thread while it waits on a clipboard answer.
enum Control {
    Answer(u64, ClipboardAnswer),
    /// The child exited: a read it asked for can no longer be answered to anyone; deny it and every later one.
    ChildExited,
    Shutdown,
}

/// State shared by the pane and its threads.
pub(crate) struct Shared {
    pub(crate) vt: Mutex<Vt>,
    capture: Arc<Mutex<Capture>>,
    published: Mutex<Arc<Frame>>,
    armed: ArmedSlot,
    /// Where the terminal's PTY writes go instead of the reply queue while a paste runs.
    pub(crate) divert: DivertSlot,
    pub(crate) search: Mutex<SearchSlot>,
    /// Wakes the search worker early (a new needle, the pane going away).
    pub(crate) search_wake: Condvar,
    writer: Arc<WriteQueue>,
    events: Sender<PaneEvent>,
    wake: Option<Wake>,
    wake_pending: AtomicBool,
    counters: Counters,
    /// How the program's end is handled, and when the program started (an early failure is reported as one).
    exit: ExitPolicy,
    started: Instant,
    /// Set once the IO thread saw the PTY's end (or stopped).
    eof: (Mutex<bool>, Condvar),
    /// Runs once at the start of the next paint, before it takes any lock (tests interleave a second view there).
    #[cfg(test)]
    before_paint: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `Some` guard, or `None` when another thread holds the lock.
fn try_lock<T>(mutex: &Mutex<T>) -> Option<MutexGuard<'_, T>> {
    match mutex.try_lock() {
        Ok(guard) => Some(guard),
        Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
        Err(TryLockError::WouldBlock) => None,
    }
}

impl Shared {
    /// Tell the frontend there is news, once until it looks.
    pub(crate) fn wake(&self) {
        if !self.wake_pending.swap(true, Ordering::AcqRel)
            && let Some(wake) = &self.wake
        {
            self.counters.wakes.fetch_add(1, Ordering::Relaxed);
            wake();
        }
    }

    pub(crate) fn send(&self, event: PaneEvent) {
        let _ = self.events.send(event);
    }

    /// Queue bytes the terminal answers the program with (never refused; see [`WriteQueue`]).
    pub(crate) fn push_reply(&self, bytes: &[u8]) {
        self.writer.push_reply(bytes);
    }

    /// Make the next paint convert (and re-check every row's selection); the caller holds the VT lock.
    pub(crate) fn touch_capture(&self) {
        lock(&self.capture).touch();
    }

    /// The frame last published by a paint.
    #[cfg(test)]
    pub(crate) fn published_frame(&self) -> Arc<Frame> {
        Arc::clone(&lock(&self.published))
    }

    fn mark_eof(&self) {
        *lock(&self.eof.0) = true;
        self.eof.1.notify_all();
    }

    fn wait_eof(&self, timeout: Duration) {
        let guard = lock(&self.eof.0);
        let _ = self
            .eof
            .1
            .wait_timeout_while(guard, timeout, |eof| !*eof)
            .unwrap_or_else(PoisonError::into_inner);
    }
}

/// One terminal pane.
pub struct Pane {
    shared: Arc<Shared>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    events: Mutex<Receiver<PaneEvent>>,
    control: Sender<Control>,
    wake_pipe: Arc<WakePipe>,
    pid: Option<u32>,
    signal: Signaller,
    io: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Pane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pane").field("pid", &self.pid).finish_non_exhaustive()
    }
}

impl Pane {
    /// Start `command` on a new PTY of `config.size`, with the pane's threads.
    ///
    /// # Errors
    ///
    /// [`PaneError::Spawn`] when the program cannot be started (not found, not executable); [`PaneError::Pty`] when
    /// no PTY can be opened; [`PaneError::Terminal`] when libghostty-vt refuses the size or cannot allocate.
    pub fn spawn(command: &PaneCommand, config: &PaneConfig) -> Result<Self, PaneError> {
        let pair = native_pty_system()
            .openpty(config.size.pty())
            .map_err(|err| PaneError::Pty(err.to_string()))?;
        let mut child = pair
            .slave
            .spawn_command(command.builder())
            .map_err(|err| PaneError::Spawn(err.to_string()))?;
        drop(pair.slave);
        let pid = child.process_id();
        #[cfg(test)]
        tests::LAST_CHILD.with(|last| last.set(pid));
        let signal = Signaller::new(pid);
        let parts = Parts::new(pair.master.as_ref(), config);
        let parts = match parts {
            Ok(parts) => parts,
            Err(err) => {
                // nothing started yet: only the child to stop and reap. Killed through the owned child too, which
                // needs no pidfd (portable-pty sends SIGHUP and escalates to SIGKILL), so the wait cannot hang.
                signal.kill();
                let _ = child.kill();
                let _ = child.wait();
                return Err(err);
            }
        };
        let started = parts.start(child, &signal)?;
        Ok(Self {
            shared: started.shared,
            master: Mutex::new(pair.master),
            events: Mutex::new(started.events),
            control: started.control,
            wake_pipe: started.wake,
            pid,
            signal,
            io: Some(started.io),
        })
    }

    pub(crate) fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    /// The child's process id.
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// The terminal's foreground process group (`tcgetpgrp` on the PTY, as libghostty's `foreground_pid` read it): the
    /// pid of the group's leader; `None` once the PTY cannot say (the child side closed).
    #[must_use]
    pub fn foreground_pid(&self) -> Option<u32> {
        lock(&self.master)
            .process_group_leader()
            .and_then(|pgrp| u32::try_from(pgrp).ok())
            .filter(|pgrp| *pgrp > 0)
    }

    /// Queue input for the child (keys, pastes). Never blocks.
    ///
    /// # Errors
    ///
    /// [`PaneError::Backpressure`] when the queue is full (the child is not reading), [`PaneError::Closed`] once the
    /// PTY refused a write (the child side is gone).
    pub fn write(&self, bytes: &[u8]) -> Result<(), PaneError> {
        self.shared.writer.push_input(bytes)
    }

    /// Resize the VT (reflowing the primary screen) and then the PTY (the child gets `SIGWINCH`).
    ///
    /// # Errors
    ///
    /// [`PaneError::Terminal`] for a zero size or a failed reflow, [`PaneError::Pty`] when the PTY refuses it.
    pub fn resize(&self, size: GridSize) -> Result<(), PaneError> {
        let mut vt = lock(&self.shared.vt);
        vt.terminal
            .resize(size.cols, size.rows, size.cell_width, size.cell_height)
            .map_err(PaneError::terminal)?;
        vt.size = size;
        drop(vt);
        lock(&self.master)
            .resize(size.pty())
            .map_err(|err| PaneError::Pty(err.to_string()))?;
        self.shared.wake();
        Ok(())
    }

    /// Change the scrollback limits (lowering one prunes history now).
    ///
    /// # Errors
    ///
    /// [`PaneError::Terminal`] when the library refuses a value.
    pub fn set_scrollback(&self, limits: ScrollbackLimits) -> Result<(), PaneError> {
        lock(&self.shared.vt)
            .terminal
            .set_scrollback_limits(limits.engine())
            .map_err(PaneError::terminal)
    }

    /// Switch the theme (a light/dark change, a config reload); colors programs set stay overridden.
    ///
    /// # Errors
    ///
    /// [`PaneError::Terminal`] when the library refuses a color.
    pub fn set_colors(&self, colors: &ThemeColors) -> Result<(), PaneError> {
        colors
            .apply(&mut lock(&self.shared.vt).terminal)
            .map_err(PaneError::terminal)?;
        self.shared.wake();
        Ok(())
    }

    /// Change the cursor the terminal returns to (a config reload); the current cursor follows while no program chose
    /// one.
    ///
    /// # Errors
    ///
    /// [`PaneError::Terminal`] when the library refuses a value.
    pub fn set_cursor_defaults(&self, cursor: CursorDefaults) -> Result<(), PaneError> {
        cursor
            .apply(&mut lock(&self.shared.vt).terminal)
            .map_err(PaneError::terminal)?;
        self.shared.wake();
        Ok(())
    }

    /// The frame to draw now. Captures the terminal when both locks are free (`try_lock`: paint never waits on the
    /// IO thread), otherwise returns the previous frame marked stale. Every view of the pane shares the result.
    pub fn frame(&self) -> FrameSnapshot {
        frame(&self.shared)
    }

    /// Every event reported since the last call, in order.
    pub fn take_events(&self) -> Vec<PaneEvent> {
        self.shared.wake_pending.store(false, Ordering::Release);
        lock(&self.events).try_iter().collect()
    }

    /// Answer the clipboard read `id` ([`PaneEvent::ClipboardRead`]); an answer for any other id is ignored.
    pub fn answer_clipboard_read(&self, id: u64, answer: ClipboardAnswer) {
        let _ = self.control.send(Control::Answer(id, answer));
    }

    /// Bytes queued for the child and not written yet.
    #[must_use]
    pub fn queued_input(&self) -> usize {
        self.shared.writer.queued()
    }

    #[must_use]
    pub fn stats(&self) -> PaneStats {
        let counters = &self.shared.counters;
        PaneStats {
            bytes: counters.bytes.load(Ordering::Relaxed),
            reads: counters.reads.load(Ordering::Relaxed),
            batches: counters.batches.load(Ordering::Relaxed),
            wakes: counters.wakes.load(Ordering::Relaxed),
            vt_nanos: counters.vt_nanos.load(Ordering::Relaxed),
            max_hold_nanos: counters.max_hold_nanos.load(Ordering::Relaxed),
        }
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        let _ = self.control.send(Control::Shutdown);
        self.wake_pipe.wake();
        self.shared.writer.close();
        self.signal.hang_up();
        if let Some(io) = self.io.take() {
            let _ = io.join();
        }
        crate::search::shut_down(&self.shared);
        let mut vt = lock(&self.shared.vt);
        let Vt { terminal, gesture, .. } = &mut *vt;
        if let Some(gesture) = gesture.take() {
            gesture.close(terminal);
        }
    }
}

fn shared(config: &PaneConfig, events: Sender<PaneEvent>) -> Result<Shared, PaneError> {
    let size = config.size;
    // the process-global hooks go in before the first terminal seals them
    crate::init();
    let mut terminal = vt::Terminal::new(size.cols, size.rows).map_err(PaneError::terminal)?;
    terminal
        .resize(size.cols, size.rows, size.cell_width, size.cell_height)
        .map_err(PaneError::terminal)?;
    terminal
        .set_scrollback_limits(config.scrollback.engine())
        .map_err(PaneError::terminal)?;
    config.colors.apply(&mut terminal).map_err(PaneError::terminal)?;
    config.cursor.apply(&mut terminal).map_err(PaneError::terminal)?;
    kitty_graphics(&mut terminal, config.kitty_image_limit).map_err(PaneError::terminal)?;
    let capture = Arc::new(Mutex::new(Capture::new().map_err(PaneError::terminal)?));
    let armed: ArmedSlot = Arc::default();
    let divert: DivertSlot = Arc::default();
    let writer = Arc::new(WriteQueue::new(config.write_limit));
    terminal
        .set_effects(Box::new(PaneEffects {
            events: events.clone(),
            writer: Arc::clone(&writer),
            capture: Arc::clone(&capture),
            armed: Arc::clone(&armed),
            divert: Arc::clone(&divert),
            xtversion: format!("ghostty {}", vt::version()),
        }))
        .map_err(PaneError::terminal)?;
    let colors = terminal.colors();
    let input = InputState::new().map_err(PaneError::terminal)?;
    Ok(Shared {
        vt: Mutex::new(Vt {
            terminal,
            input,
            size,
            cleared_selection: false,
            colors,
            gesture: None,
            pending_clear: false,
        }),
        capture,
        published: Mutex::new(Arc::new(Frame::default())),
        armed,
        divert,
        search: Mutex::new(SearchSlot::default()),
        search_wake: Condvar::new(),
        writer,
        events,
        wake: config.wake.clone(),
        wake_pending: AtomicBool::new(false),
        counters: Counters::default(),
        exit: config.exit.clone(),
        started: Instant::now(),
        eof: (Mutex::new(false), Condvar::new()),
        #[cfg(test)]
        before_paint: Mutex::new(None),
    })
}

/// Kitty graphics as Ghostty sets them up: `limit` bytes of storage, and images loadable from files, temporary files
/// (in the system's temporary directory) and shared memory besides the escape sequence itself.
fn kitty_graphics(terminal: &mut vt::Terminal, limit: u64) -> Result<(), vt::Error> {
    terminal.set_kitty_image_storage_limit(limit)?;
    let temp = std::env::temp_dir();
    terminal.set_kitty_image_media(true, temp.to_str(), true)
}

fn spawn_thread(name: &str, body: impl FnOnce() + Send + 'static) -> Result<JoinHandle<()>, PaneError> {
    #[cfg(test)]
    if tests::FAIL_THREAD.with(std::cell::Cell::get) == Some(name) {
        return Err(PaneError::Pty(format!("cannot start the {name} thread: injected")));
    }
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(body)
        .map_err(|err| PaneError::Pty(format!("cannot start the {name} thread: {err}")))
}

/// One paint. The frame it starts from is read, and its successor published, under the capture lock (VT → capture →
/// published): two views painting at once can then never convert against an older frame than the state was last
/// cleaned for, which would publish rows the capture already reported clean.
fn frame(shared: &Shared) -> FrameSnapshot {
    shared.wake_pending.store(false, Ordering::Release);
    #[cfg(test)]
    {
        let hook = lock(&shared.before_paint).take();
        if let Some(hook) = hook {
            hook();
        }
    }
    match capture(shared) {
        Some(frame) => FrameSnapshot { frame, stale: false },
        None => FrameSnapshot {
            frame: Arc::clone(&lock(&shared.published)),
            stale: true,
        },
    }
}

/// The paint-side capture: the frame to draw (new or unchanged), or `None` when a lock was busy.
fn capture(shared: &Shared) -> Option<Arc<Frame>> {
    let mut vt = try_lock(&shared.vt)?;
    // VT → capture; on a busy capture the VT guard drops on return
    let mut capture = try_lock(&shared.capture)?;
    end_expired_hold(&mut vt, &mut capture);
    let mouse_shape = MouseShape::from_engine(vt.terminal.mouse_shape());
    if capture.held() {
        drop(vt);
    } else {
        // the Kitty view is read while the terminal is held, with the state
        capture.capture_kitty(vt.terminal.kitty_graphics());
        let pending = capture.state.begin_update(&mut vt.terminal);
        // only the begin phase reads the terminal
        drop(vt);
        if let Err(err) = pending.and_then(vt::PendingUpdate::end) {
            tracing::warn!(%err, "capturing the terminal failed");
        }
    }
    let mut published = lock(&shared.published);
    match capture.convert(&published, mouse_shape) {
        Ok(Some(frame)) => *published = Arc::new(frame),
        Ok(None) => {}
        Err(err) => tracing::warn!(%err, "converting the capture failed"),
    }
    Some(Arc::clone(&published))
}

/// End a synchronized-output hold that lasted past its timeout (a program that never ends its hold must not freeze
/// the pane); setting the mode runs no callback. Whether one ended.
fn end_expired_hold(vt: &mut Vt, capture: &mut Capture) -> bool {
    if !capture.hold_expired(Instant::now()) {
        return false;
    }
    if let Err(err) = vt.terminal.set_mode(vt::Mode::SYNC_OUTPUT, false) {
        tracing::warn!(%err, "ending a synchronized-output hold failed");
    }
    capture.end_hold();
    true
}

/// Signals the child (`SIGHUP` on drop), race-free against its reaping where the platform allows (a pidfd on Linux).
struct Signaller {
    #[cfg(target_os = "linux")]
    pidfd: Option<std::os::fd::OwnedFd>,
    #[cfg(not(target_os = "linux"))]
    pid: Option<u32>,
}

impl Signaller {
    #[cfg(target_os = "linux")]
    fn new(pid: Option<u32>) -> Self {
        use std::os::fd::FromRawFd;
        #[cfg(test)]
        if tests::NO_PIDFD.with(std::cell::Cell::get) {
            return Self { pidfd: None };
        }
        let pidfd = pid.and_then(|pid| {
            // SAFETY: `pidfd_open` takes a pid and flags and returns a new descriptor or -1; no memory is touched. The
            // child is not reaped yet (its waiter starts after this), so the pid is still the child's.
            let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, libc::pid_t::try_from(pid).ok()?, 0) };
            let fd = std::os::fd::RawFd::try_from(fd).ok().filter(|fd| *fd >= 0)?;
            // SAFETY: a fresh descriptor nothing else owns.
            Some(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
        });
        Self { pidfd }
    }

    #[cfg(target_os = "linux")]
    fn send(&self, signal: libc::c_int) {
        use std::os::fd::AsRawFd;
        if let Some(pidfd) = &self.pidfd {
            // SAFETY: signals the process the pidfd refers to (ESRCH once it is gone, never another process); no
            // memory is passed.
            unsafe { libc::syscall(libc::SYS_pidfd_send_signal, pidfd.as_raw_fd(), signal, 0, 0) };
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn new(pid: Option<u32>) -> Self {
        Self { pid }
    }

    #[cfg(not(target_os = "linux"))]
    fn send(&self, signal: libc::c_int) {
        if let Some(pid) = self.pid.and_then(|pid| libc::pid_t::try_from(pid).ok()) {
            // SAFETY: plain `kill(2)`; no memory is passed.
            unsafe { libc::kill(pid, signal) };
        }
    }

    /// `SIGHUP`, as a terminal closing does.
    fn hang_up(&self) {
        self.send(libc::SIGHUP);
    }

    /// `SIGKILL`, for a pane that never finished starting (the child is reaped right after).
    fn kill(&self) {
        self.send(libc::SIGKILL);
    }
}

/// What a pane needs before its threads start.
struct Parts {
    shared: Arc<Shared>,
    wake: Arc<WakePipe>,
    reader: File,
    writer: Box<dyn std::io::Write + Send>,
    events: Receiver<PaneEvent>,
}

impl Parts {
    fn new(master: &dyn MasterPty, config: &PaneConfig) -> Result<Self, PaneError> {
        let reader = pty::reader(master)?;
        let writer = master.take_writer().map_err(|err| PaneError::Pty(err.to_string()))?;
        let (events_tx, events) = mpsc::channel();
        Ok(Self {
            shared: Arc::new(shared(config, events_tx)?),
            wake: Arc::new(WakePipe::new()?),
            reader,
            writer,
            events,
        })
    }

    /// Start the writer, IO and waiter threads. On a failure every thread already running is stopped (and the IO
    /// thread joined), and the child is killed and reaped, so nothing outlives the error.
    fn start(self, mut child: Box<dyn Child + Send + Sync>, signal: &Signaller) -> Result<Started, PaneError> {
        let Self {
            shared,
            wake,
            reader,
            writer,
            events,
        } = self;
        let (control, control_rx) = mpsc::channel();
        let stop = |err: PaneError, io: Option<JoinHandle<()>>, child: &mut Box<dyn Child + Send + Sync>| {
            let _ = control.send(Control::Shutdown);
            wake.wake();
            shared.writer.close();
            signal.kill();
            // the pidfd may be missing (pidfd_open failed): the owned child kills without one
            let _ = child.kill();
            if let Some(io) = io {
                let _ = io.join();
            }
            let _ = child.wait();
            err
        };
        let queue = Arc::clone(&shared.writer);
        if let Err(err) = spawn_thread("pane-write", move || pty::write_loop(&queue, writer)) {
            return Err(stop(err, None, &mut child));
        }
        let io = Io {
            shared: Arc::clone(&shared),
            reader,
            wake: Arc::clone(&wake),
            control: control_rx,
            scanner: Scanner::default(),
            next_read: 0,
            child_exited: false,
        };
        let io = match spawn_thread("pane-io", move || io.run()) {
            Ok(io) => io,
            Err(err) => return Err(stop(err, None, &mut child)),
        };
        // the child goes to the waiter only once it runs, so a failed start still holds it to reap
        let (hand, handed) = mpsc::sync_channel::<Box<dyn Child + Send + Sync>>(1);
        let waiter = {
            let shared = Arc::clone(&shared);
            let control = control.clone();
            spawn_thread("pane-wait", move || {
                if let Ok(child) = handed.recv() {
                    wait_child(child, &shared, &control);
                }
            })
        };
        if let Err(err) = waiter {
            return Err(stop(err, Some(io), &mut child));
        }
        if let Err(mpsc::SendError(mut child)) = hand.send(child) {
            let err = PaneError::Pty("the pane-wait thread is gone".to_owned());
            return Err(stop(err, Some(io), &mut child));
        }
        Ok(Started {
            shared,
            wake,
            events,
            io,
            control,
        })
    }
}

/// A pane's running parts.
struct Started {
    shared: Arc<Shared>,
    wake: Arc<WakePipe>,
    events: Receiver<PaneEvent>,
    io: JoinHandle<()>,
    control: Sender<Control>,
}

/// The waiter thread: reap the child, release a clipboard read the IO thread may be parked on, give it a moment to
/// process the last output, put the exit message on screen ([`ExitPolicy`]), report the exit.
fn wait_child(mut child: Box<dyn Child + Send + Sync>, shared: &Shared, control: &Sender<Control>) {
    let (code, signal) = match child.wait() {
        Ok(status) => (status.exit_code(), status.signal().map(str::to_owned)),
        Err(err) => {
            tracing::warn!(%err, "waiting for the pane's child failed");
            (1, None)
        }
    };
    let runtime = shared.started.elapsed();
    let _ = control.send(Control::ChildExited);
    shared.wait_eof(EXIT_GRACE);
    let outcome = shared.exit.outcome(code, runtime);
    if outcome.held() {
        let mut vt = lock(&shared.vt);
        let layout = Layout::of(&mut vt.terminal);
        let message = shared.exit.message(outcome, code, runtime, layout);
        vt.terminal.vt_write(&message);
    }
    shared.send(PaneEvent::Exited(crate::ExitStatus {
        code,
        signal,
        held: outcome.held(),
    }));
    shared.wake();
}

/// Feed a read's dispatch byte alone with its answer armed, then disarm whatever the callback left, so an answer the
/// terminal did not take can never serve a later read.
fn dispatch(shared: &Shared, byte: u8, read: ReadCandidate, answer: ClipboardAnswer) {
    let mut vt = lock(&shared.vt);
    *lock(&shared.armed) = Some(ArmedRead {
        location: read.location,
        scope: read.scope,
        answer,
    });
    vt.terminal.vt_write(&[byte]);
    if lock(&shared.armed).take().is_some() {
        tracing::debug!("a clipboard read candidate was not dispatched by the terminal; its answer is dropped");
    }
    // the dispatch byte ends a sequence: a clear waiting for ground goes in now, not with the program's next output
    if vt.pending_clear && vt.terminal.at_ground() {
        crate::actions::apply_clear(shared, &mut vt);
    }
    shared.counters.bytes.fetch_add(1, Ordering::Relaxed);
}

/// The IO thread.
struct Io {
    shared: Arc<Shared>,
    reader: File,
    wake: Arc<WakePipe>,
    control: Receiver<Control>,
    scanner: Scanner,
    next_read: u64,
    /// The waiter reported the child's exit: clipboard reads are denied without asking.
    child_exited: bool,
}

impl Io {
    fn run(mut self) {
        let mut buffer = vec![0_u8; READ_BATCH];
        loop {
            match pty::poll(&self.reader, &self.wake, self.hold_timeout_ms()) {
                Ok(Ready::Pty) => {}
                Ok(Ready::Idle) => {
                    self.expire_hold();
                    continue;
                }
                Ok(Ready::Woken) | Err(_) => break,
            }
            let (filled, eof) = self.read_batch(&mut buffer);
            if filled > 0 && !self.feed(&buffer[..filled]) {
                break;
            }
            self.after_batch(filled);
            if eof {
                // every descriptor of the child side is closed: nothing will read input any more
                self.shared.writer.close();
                break;
            }
        }
        self.shared.mark_eof();
        self.shared.wake();
    }

    /// How long `poll` may wait: until a synchronized-output hold expires, else forever (-1).
    fn hold_timeout_ms(&self) -> i32 {
        let deadline = lock(&self.shared.capture).hold_deadline();
        deadline.map_or(-1, |deadline| {
            let left = deadline.saturating_duration_since(Instant::now());
            // round up, so the wake lands after the deadline, not just before it
            i32::try_from(left.as_millis() + 1).unwrap_or(i32::MAX)
        })
    }

    /// End a hold that outlived its deadline and wake the frontend to draw the live screen.
    fn expire_hold(&self) {
        let mut vt = lock(&self.shared.vt);
        let mut capture = lock(&self.shared.capture);
        if end_expired_hold(&mut vt, &mut capture) {
            drop(capture);
            drop(vt);
            self.shared.wake();
        }
    }

    /// Read what the PTY has, up to the buffer: one read, then more while `poll` says more is there right away.
    /// Returns the bytes gathered and whether the PTY ended.
    fn read_batch(&mut self, buffer: &mut [u8]) -> (usize, bool) {
        let mut filled = 0;
        while filled < buffer.len() {
            match self.reader.read(&mut buffer[filled..]) {
                Ok(0) => return (filled, true),
                Ok(read) => {
                    self.shared.counters.reads.fetch_add(1, Ordering::Relaxed);
                    filled += read;
                }
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                // EIO: the child side closed
                Err(_) => return (filled, true),
            }
            if !matches!(pty::poll(&self.reader, &self.wake, 0), Ok(Ready::Pty)) {
                break;
            }
        }
        (filled, false)
    }

    /// Feed `bytes` to the VT, stopping at each clipboard read for its answer. `false` when the pane shut down.
    fn feed(&mut self, bytes: &[u8]) -> bool {
        let mut start = 0;
        while let Some((offset, read)) = self.scanner.find(&bytes[start..]) {
            let dispatch = start + offset;
            self.write_vt(&bytes[start..dispatch]);
            let Some(answer) = self.park(read) else {
                return false;
            };
            self.dispatch(bytes[dispatch], read, answer);
            start = dispatch + 1;
        }
        self.write_vt(&bytes[start..]);
        true
    }

    /// `vt_write` in slices, the VT lock taken per slice so paint and input get in between.
    fn write_vt(&self, bytes: &[u8]) {
        for slice in bytes.chunks(VT_SLICE) {
            let mut vt = lock(&self.shared.vt);
            let started = Instant::now();
            write_slice(&self.shared, &mut vt, slice);
            let held = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
            drop(vt);
            let counters = &self.shared.counters;
            counters.vt_nanos.fetch_add(held, Ordering::Relaxed);
            counters.max_hold_nanos.fetch_max(held, Ordering::Relaxed);
            counters.bytes.fetch_add(slice.len() as u64, Ordering::Relaxed);
        }
    }

    /// Ask the frontend about a clipboard read and wait for its answer without the VT lock; `None` on shutdown.
    fn park(&mut self, read: ReadCandidate) -> Option<ClipboardAnswer> {
        if self.child_exited {
            return Some(ClipboardAnswer::Denied);
        }
        self.next_read += 1;
        let id = self.next_read;
        self.shared.send(PaneEvent::ClipboardRead(ClipboardReadRequest {
            id,
            location: read.location,
            scope: read.scope,
        }));
        self.shared.wake();
        loop {
            match self.control.recv() {
                Ok(Control::Answer(answered, answer)) if answered == id => return Some(answer),
                Ok(Control::Answer(..)) => {}
                Ok(Control::ChildExited) => {
                    // nobody is left to read the answer; keep processing the output instead of waiting on a prompt
                    self.child_exited = true;
                    return Some(ClipboardAnswer::Denied);
                }
                Ok(Control::Shutdown) | Err(_) => return None,
            }
        }
    }

    fn dispatch(&self, byte: u8, read: ReadCandidate, answer: ClipboardAnswer) {
        dispatch(&self.shared, byte, read, answer);
    }

    /// After a batch: report color changes, count it, wake the frontend.
    fn after_batch(&self, filled: usize) {
        if filled > 0 {
            let mut vt = lock(&self.shared.vt);
            let colors = vt.terminal.colors();
            if colors != vt.colors {
                vt.colors = colors.clone();
                drop(vt);
                self.shared
                    .send(PaneEvent::Colors(TerminalColors::from_engine(&colors)));
            }
        }
        self.shared.counters.batches.fetch_add(1, Ordering::Relaxed);
        self.shared.wake();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::ClipboardLocation;
    use crate::scan::ReadScope;

    const SIZE: GridSize = GridSize {
        cols: 20,
        rows: 4,
        cell_width: 8,
        cell_height: 16,
    };

    thread_local! {
        /// The pid of the last child `Pane::spawn` started on this thread.
        pub(super) static LAST_CHILD: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
        /// A thread name whose start `spawn_thread` fails on this thread (fault injection).
        pub(super) static FAIL_THREAD: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
        /// Whether `Signaller::new` acts as if `pidfd_open` failed, on this thread.
        pub(super) static NO_PIDFD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    const fn assert_send_sync<T: Send + Sync>() {}
    const _: () = assert_send_sync::<Pane>();

    fn wait_for(pane: &Pane, text: &str) -> Arc<Frame> {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let frame = pane.frame().frame;
            if frame.lines().iter().any(|line| line.contains(text)) {
                return frame;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {text:?}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_busy_lock_keeps_the_previous_frame_and_marks_it_stale() {
        let dir = tempfile::tempdir().unwrap();
        let command = PaneCommand::new("/bin/sh")
            .arg("-c")
            .arg("printf first; read _; printf ' second'; exec sleep 5")
            .cwd(dir.path());
        let pane = Pane::spawn(&command, &PaneConfig::new(SIZE)).unwrap();
        let first = wait_for(&pane, "first");
        // the child now waits in `read`: nothing else takes the locks while this test holds them
        {
            let _vt = lock(&pane.shared.vt);
            let snapshot = pane.frame();
            assert!(snapshot.stale);
            assert!(Arc::ptr_eq(&snapshot.frame, &first));
        }
        {
            let _capture = lock(&pane.shared.capture);
            let snapshot = pane.frame();
            assert!(snapshot.stale);
            assert!(Arc::ptr_eq(&snapshot.frame, &first));
        }
        // with both free again a paint is fresh (once the IO thread's post-batch check, if it was waiting on the
        // lock, is done), and new output reaches the screen
        let deadline = Instant::now() + Duration::from_secs(20);
        while pane.frame().stale {
            assert!(Instant::now() < deadline, "paints stayed stale");
            std::thread::sleep(Duration::from_millis(5));
        }
        pane.write(b"\n").unwrap();
        wait_for(&pane, "second");
    }

    #[test]
    fn a_view_that_paints_while_another_does_never_publishes_older_rows() {
        let dir = tempfile::tempdir().unwrap();
        let command = PaneCommand::new("/bin/sh")
            .arg("-c")
            .arg("exec sleep 30")
            .cwd(dir.path());
        let pane = Pane::spawn(&command, &PaneConfig::new(SIZE)).unwrap();
        lock(&pane.shared.vt).terminal.vt_write(b"abc");
        let first = pane.frame().frame;
        assert_eq!(first.lines()[0], "abc");
        // view B starts painting; before it looks at the terminal, the row changes in place (the cursor moves too)
        // and view A paints the change completely
        let shared = Arc::clone(&pane.shared);
        *lock(&pane.shared.before_paint) = Some(Box::new(move || {
            lock(&shared.vt).terminal.vt_write(b"\x1b[1;1HXYZ\x1b[3;5H");
            // view A paints until it gets the terminal (the IO thread may hold a lock for a moment on a loaded machine)
            let deadline = Instant::now() + Duration::from_secs(20);
            let a = loop {
                let a = frame(&shared);
                if !a.stale {
                    break a;
                }
                assert!(Instant::now() < deadline, "view A never got the terminal");
                std::thread::sleep(Duration::from_millis(1));
            };
            assert_eq!(a.frame.lines()[0], "XYZ");
        }));
        let b = pane.frame();
        assert_eq!(b.frame.lines()[0], "XYZ", "view B drew older rows");
        assert_eq!(pane.frame().frame.lines()[0], "XYZ", "older rows were published");
    }

    /// `Pane::spawn` of `script` on a worker thread whose thread `fail` cannot start (and, with `no_pidfd`, whose
    /// pidfd could not be opened): the error and the child's pid, or a panic when the cleanup hangs.
    fn failed_start(script: &'static str, fail: &'static str, no_pidfd: bool) -> (PaneError, u32) {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let dir = tempfile::tempdir().unwrap();
            let command = PaneCommand::new("/bin/sh").arg("-c").arg(script).cwd(dir.path());
            FAIL_THREAD.with(|cell| cell.set(Some(fail)));
            NO_PIDFD.with(|cell| cell.set(no_pidfd));
            let error = Pane::spawn(&command, &PaneConfig::new(SIZE)).unwrap_err();
            let pid = LAST_CHILD.with(std::cell::Cell::get).unwrap();
            let _ = tx.send((error, pid));
        });
        rx.recv_timeout(Duration::from_secs(20))
            .unwrap_or_else(|_| panic!("{fail} (no pidfd: {no_pidfd}): the failed start hung"))
    }

    #[test]
    fn a_pane_that_fails_to_start_a_thread_stops_its_child_and_workers() {
        // a child that ignores SIGHUP too: without a pidfd only the owned child's SIGKILL escalation ends it
        for (script, no_pidfd) in [("exec sleep 30", false), ("trap '' HUP; exec sleep 30", true)] {
            for thread in ["pane-write", "pane-io", "pane-wait"] {
                let (error, pid) = failed_start(script, thread, no_pidfd);
                assert!(matches!(error, PaneError::Pty(_)), "{thread}: {error:?}");
                // killed and reaped: not even a zombie is left
                assert!(
                    !std::path::Path::new(&format!("/proc/{pid}")).exists(),
                    "{thread} (no pidfd: {no_pidfd}): child {pid} is left"
                );
            }
        }
    }

    #[test]
    fn the_scrollback_limits_reach_the_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let command = PaneCommand::new("/bin/sh")
            .arg("-c")
            .arg("exec sleep 30")
            .cwd(dir.path());
        let config = PaneConfig {
            scrollback: ScrollbackLimits {
                max_bytes: None,
                max_lines: Some(100),
            },
            ..PaneConfig::new(SIZE)
        };
        let pane = Pane::spawn(&command, &config).unwrap();
        let limits = || lock(&pane.shared.vt).terminal.scrollback_limits();
        assert_eq!(
            limits(),
            vt::ScrollbackLimits {
                max_bytes: None,
                max_lines: Some(100),
            }
        );
        pane.set_scrollback(ScrollbackLimits::default()).unwrap();
        assert_eq!(
            limits(),
            vt::ScrollbackLimits {
                max_bytes: Some(50_000_000),
                max_lines: None,
            }
        );
    }

    #[test]
    fn an_answer_the_terminal_does_not_take_is_dropped_and_cannot_serve_the_next_read() {
        let (events, received) = mpsc::channel();
        let shared = shared(&PaneConfig::new(SIZE), events).unwrap();
        let read = ReadCandidate {
            location: ClipboardLocation::Standard,
            scope: ReadScope::Data,
        };
        // a BEL outside any OSC: no read callback runs, so the armed answer is left over and must be cleared
        dispatch(&shared, 0x07, read, ClipboardAnswer::Text(Some("secret".to_owned())));
        assert!(lock(&shared.armed).is_none());
        assert_eq!(received.try_iter().collect::<Vec<_>>(), [PaneEvent::Bell]);
        // a read nothing armed: denied, the program reads an empty clipboard
        lock(&shared.vt).terminal.vt_write(b"\x1b]52;c;?\x07");
        assert_eq!(shared.writer.queued(), b"\x1b]52;c;\x07".len());
    }

    #[test]
    fn an_armed_answer_serves_the_read_it_was_armed_for() {
        let (events, _received) = mpsc::channel();
        let shared = shared(&PaneConfig::new(SIZE), events).unwrap();
        lock(&shared.vt).terminal.vt_write(b"\x1b]52;c;?");
        let read = ReadCandidate {
            location: ClipboardLocation::Standard,
            scope: ReadScope::Data,
        };
        dispatch(&shared, 0x07, read, ClipboardAnswer::Text(Some("abc".to_owned())));
        assert!(lock(&shared.armed).is_none());
        assert_eq!(shared.writer.queued(), b"\x1b]52;c;YWJj\x07".len());
    }
}
