//! [`Pane`]: one terminal — a libghostty-vt terminal behind a mutex, a PTY child, and three threads: the IO thread
//! (reads the PTY and feeds the VT), the writer (drains the bounded write queue into the PTY), and the waiter (reports
//! the child's exit).
//!
//! Locks (Technical Details > Threads): `vt` → `capture` → leaves (`armed`, `published`), never the reverse. The IO
//! thread holds `vt` for one bounded slice of `vt_write` at a time. Painting ([`Pane::frame`]) `try_lock`s `vt` and
//! then `capture`, keeps `vt` only for the begin phase of the capture, and draws the previous frame when either is
//! busy. Effect callbacks run under `vt` and only copy into channels.

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
    ArmedRead, ArmedSlot, ClipboardAnswer, ClipboardReadRequest, PaneEffects, PaneEvent, TerminalColors,
};
use crate::pty::{self, GridSize, PaneCommand, Ready, WakePipe, WriteQueue};
use crate::render::{Capture, Frame, MouseShape, ThemeColors};
use crate::scan::{ReadCandidate, Scanner};
use crate::{PaneError, ScrollbackLimits};

/// The most bytes one read batch gathers before the VT sees them.
const READ_BATCH: usize = 256 * 1024;
/// The most bytes fed to `vt_write` under one hold of the VT lock.
const VT_SLICE: usize = 64 * 1024;
/// How long the exit report waits for the PTY's end, so output written before the exit is processed first.
const EXIT_GRACE: Duration = Duration::from_secs(1);
/// The default limit of the write queue (input waiting for the child).
pub const DEFAULT_WRITE_LIMIT: usize = 1024 * 1024;

/// Called from the pane's threads when it has news (a frame to draw or events to drain); coalesced until the frontend
/// calls [`Pane::frame`] or [`Pane::take_events`].
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// How a pane starts.
#[derive(Clone)]
pub struct PaneConfig {
    pub size: GridSize,
    pub scrollback: ScrollbackLimits,
    pub colors: ThemeColors,
    /// The most input bytes queued for the child before [`Pane::write`] refuses more.
    pub write_limit: usize,
    pub wake: Option<Wake>,
}

impl PaneConfig {
    #[must_use]
    pub fn new(size: GridSize) -> Self {
        Self {
            size,
            scrollback: ScrollbackLimits::default(),
            colors: ThemeColors::default(),
            write_limit: DEFAULT_WRITE_LIMIT,
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
            .field("write_limit", &self.write_limit)
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
    /// The colors last reported with [`PaneEvent::Colors`].
    colors: vt::TerminalColors,
}

/// Messages to the IO thread while it waits on a clipboard answer.
enum Control {
    Answer(u64, ClipboardAnswer),
    Shutdown,
}

/// State shared by the pane and its threads.
pub(crate) struct Shared {
    pub(crate) vt: Mutex<Vt>,
    capture: Arc<Mutex<Capture>>,
    published: Mutex<Arc<Frame>>,
    armed: ArmedSlot,
    writer: Arc<WriteQueue>,
    events: Sender<PaneEvent>,
    wake: Option<Wake>,
    wake_pending: AtomicBool,
    counters: Counters,
    /// Set once the IO thread saw the PTY's end (or stopped).
    eof: (Mutex<bool>, Condvar),
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
    fn wake(&self) {
        if !self.wake_pending.swap(true, Ordering::AcqRel)
            && let Some(wake) = &self.wake
        {
            self.counters.wakes.fetch_add(1, Ordering::Relaxed);
            wake();
        }
    }

    fn send(&self, event: PaneEvent) {
        let _ = self.events.send(event);
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
        crate::init();
        let pair = native_pty_system()
            .openpty(config.size.pty())
            .map_err(|err| PaneError::Pty(err.to_string()))?;
        let child = pair
            .slave
            .spawn_command(command.builder())
            .map_err(|err| PaneError::Spawn(err.to_string()))?;
        drop(pair.slave);
        let pid = child.process_id();
        let reader = pty::reader(pair.master.as_ref())?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|err| PaneError::Pty(err.to_string()))?;
        let (events_tx, events) = mpsc::channel();
        let shared = Arc::new(shared(config, events_tx)?);
        let (control, control_rx) = mpsc::channel();
        let wake_pipe = Arc::new(WakePipe::new()?);
        let signal = Signaller::new(pid);
        {
            let queue = Arc::clone(&shared.writer);
            spawn_thread("pane-write", move || pty::write_loop(&queue, writer))?;
        }
        let io = {
            let io = Io {
                shared: Arc::clone(&shared),
                reader,
                wake: Arc::clone(&wake_pipe),
                control: control_rx,
                scanner: Scanner::default(),
                next_read: 0,
            };
            spawn_thread("pane-io", move || io.run())?
        };
        {
            let shared = Arc::clone(&shared);
            spawn_thread("pane-wait", move || wait_child(child, &shared))?;
        }
        Ok(Self {
            shared,
            master: Mutex::new(pair.master),
            events: Mutex::new(events),
            control,
            wake_pipe,
            pid,
            signal,
            io: Some(io),
        })
    }

    #[cfg(test)]
    pub(crate) fn shared(&self) -> &Shared {
        &self.shared
    }

    /// The child's process id.
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.pid
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
        lock(&self.shared.vt)
            .terminal
            .resize(size.cols, size.rows, size.cell_width, size.cell_height)
            .map_err(PaneError::terminal)?;
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

    /// The frame to draw now. Captures the terminal when both locks are free (`try_lock`: paint never waits on the
    /// IO thread), otherwise returns the previous frame marked stale. Every view of the pane shares the result.
    pub fn frame(&self) -> FrameSnapshot {
        self.shared.wake_pending.store(false, Ordering::Release);
        let previous = Arc::clone(&lock(&self.shared.published));
        match capture(&self.shared, &previous) {
            Captured::New(frame) => {
                let frame = Arc::new(frame);
                *lock(&self.shared.published) = Arc::clone(&frame);
                FrameSnapshot { frame, stale: false }
            }
            Captured::Unchanged => FrameSnapshot {
                frame: previous,
                stale: false,
            },
            Captured::Busy => FrameSnapshot {
                frame: previous,
                stale: true,
            },
        }
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
    }
}

fn shared(config: &PaneConfig, events: Sender<PaneEvent>) -> Result<Shared, PaneError> {
    let size = config.size;
    let mut terminal = vt::Terminal::new(size.cols, size.rows).map_err(PaneError::terminal)?;
    terminal
        .resize(size.cols, size.rows, size.cell_width, size.cell_height)
        .map_err(PaneError::terminal)?;
    terminal
        .set_scrollback_limits(config.scrollback.engine())
        .map_err(PaneError::terminal)?;
    config.colors.apply(&mut terminal).map_err(PaneError::terminal)?;
    let capture = Arc::new(Mutex::new(Capture::new().map_err(PaneError::terminal)?));
    let armed: ArmedSlot = Arc::default();
    let writer = Arc::new(WriteQueue::new(config.write_limit));
    terminal
        .set_effects(Box::new(PaneEffects {
            events: events.clone(),
            writer: Arc::clone(&writer),
            capture: Arc::clone(&capture),
            armed: Arc::clone(&armed),
            xtversion: format!("ghostty {}", vt::version()),
        }))
        .map_err(PaneError::terminal)?;
    let colors = terminal.colors();
    Ok(Shared {
        vt: Mutex::new(Vt { terminal, colors }),
        capture,
        published: Mutex::new(Arc::new(Frame::default())),
        armed,
        writer,
        events,
        wake: config.wake.clone(),
        wake_pending: AtomicBool::new(false),
        counters: Counters::default(),
        eof: (Mutex::new(false), Condvar::new()),
    })
}

fn spawn_thread(name: &str, body: impl FnOnce() + Send + 'static) -> Result<JoinHandle<()>, PaneError> {
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(body)
        .map_err(|err| PaneError::Pty(format!("cannot start the {name} thread: {err}")))
}

/// What a paint-side capture produced.
enum Captured {
    /// A lock was busy: draw the previous frame and ask again.
    Busy,
    Unchanged,
    New(Frame),
}

/// The paint-side capture.
fn capture(shared: &Shared, previous: &Frame) -> Captured {
    let Some(mut vt) = try_lock(&shared.vt) else {
        return Captured::Busy;
    };
    // VT → capture; on a busy capture the VT guard drops on return
    let Some(mut capture) = try_lock(&shared.capture) else {
        return Captured::Busy;
    };
    if capture.hold_expired(Instant::now()) {
        // a program that never ends its hold must not freeze the pane; setting the mode runs no callback
        if let Err(err) = vt.terminal.set_mode(vt::Mode::SYNC_OUTPUT, false) {
            tracing::warn!(%err, "ending a synchronized-output hold failed");
        }
        capture.end_hold();
    }
    let mouse_shape = MouseShape::from_engine(vt.terminal.mouse_shape());
    if capture.held() {
        drop(vt);
    } else {
        let pending = capture.state.begin_update(&mut vt.terminal);
        // only the begin phase reads the terminal
        drop(vt);
        if let Err(err) = pending.and_then(vt::PendingUpdate::end) {
            tracing::warn!(%err, "capturing the terminal failed");
        }
    }
    match capture.convert(previous, mouse_shape) {
        Ok(Some(frame)) => Captured::New(frame),
        Ok(None) => Captured::Unchanged,
        Err(err) => {
            tracing::warn!(%err, "converting the capture failed");
            Captured::Unchanged
        }
    }
}

/// Sends `SIGHUP` to the child on drop, race-free against its reaping where the platform allows (a pidfd on Linux).
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
    fn hang_up(&self) {
        use std::os::fd::AsRawFd;
        if let Some(pidfd) = &self.pidfd {
            // SAFETY: signals the process the pidfd refers to (ESRCH once it is gone, never another process); no
            // memory is passed.
            unsafe { libc::syscall(libc::SYS_pidfd_send_signal, pidfd.as_raw_fd(), libc::SIGHUP, 0, 0) };
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn new(pid: Option<u32>) -> Self {
        Self { pid }
    }

    #[cfg(not(target_os = "linux"))]
    fn hang_up(&self) {
        if let Some(pid) = self.pid.and_then(|pid| libc::pid_t::try_from(pid).ok()) {
            // SAFETY: plain `kill(2)`; no memory is passed.
            unsafe { libc::kill(pid, libc::SIGHUP) };
        }
    }
}

/// The waiter thread: reap the child, give the IO thread a moment to process its last output, report the exit.
fn wait_child(mut child: Box<dyn Child + Send + Sync>, shared: &Shared) {
    let status = match child.wait() {
        Ok(status) => crate::ExitStatus {
            code: status.exit_code(),
            signal: status.signal().map(str::to_owned),
        },
        Err(err) => {
            tracing::warn!(%err, "waiting for the pane's child failed");
            crate::ExitStatus { code: 1, signal: None }
        }
    };
    shared.wait_eof(EXIT_GRACE);
    shared.send(PaneEvent::Exited(status));
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
}

impl Io {
    fn run(mut self) {
        let mut buffer = vec![0_u8; READ_BATCH];
        loop {
            match pty::poll(&self.reader, &self.wake, -1) {
                Ok(Ready::Pty) => {}
                Ok(Ready::Idle) => continue,
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
            vt.terminal.vt_write(slice);
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
        pane.write(b"\n").unwrap();
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
            // the VT lock was released on the way out
            assert!(pane.shared.vt.try_lock().is_ok());
        }
        wait_for(&pane, "second");
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
