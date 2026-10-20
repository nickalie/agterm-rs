//! [`Pane`]: one terminal — a libghostty-vt terminal behind a mutex, a PTY child, and its threads: the PTY actor's
//! (`agterm-pty`: reads the PTY into the pane's [`Mailbox`] and writes the input and the terminal's replies to it; one
//! polling thread on Unix, a reader and a writer on Windows), the IO worker (feeds the mailbox's output to the VT,
//! stopping at clipboard reads for their answers), and the waiter (reports the child's exit).
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
//! the IO worker, which waits on its mailbox with that deadline while a hold is on and wakes the frontend.
//!
//! While the worker waits on a clipboard answer, the program's output waits in the mailbox until the mailbox's high
//! mark pauses the actor's reads ([`crate::mailbox`]); the actor keeps writing the program's input.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, TryLockError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use agterm_ghostty_vt::{self as vt, ReadCandidate, Scanner};
use agterm_pty::{Actor, ActorConfig, ActorHandle, Child, Killer, Master, ReadPause, Spawned};

use crate::effects::{
    ArmedRead, ArmedSlot, ClipboardAnswer, ClipboardLocation, ClipboardReadRequest, ColorScheme, DivertSlot,
    PaneEffects, PaneEvent, SchemeSlot, TerminalColors,
};
use crate::exit::{ExitPolicy, Layout};
use crate::input::InputState;
use crate::mailbox::{Answer, Mail, Mailbox};
use crate::pty::{GridSize, PaneCommand, ReplyOutlet, Tty};
use crate::render::{Capture, CursorDefaults, Frame, MouseShape, ThemeColors};
use crate::search::SearchSlot;
use crate::{PaneError, ScrollbackLimits};

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
    /// Grapheme clustering (mode 2027) as the terminal's default, which a reset restores: Ghostty's
    /// `grapheme-width-method = unicode`, its default; `false` is `legacy`.
    pub grapheme_clusters: bool,
    /// The desktop's appearance the terminal answers color scheme queries with ([`Pane::set_color_scheme`]).
    pub color_scheme: ColorScheme,
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
            grapheme_clusters: true,
            color_scheme: ColorScheme::default(),
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
            .field("grapheme_clusters", &self.grapheme_clusters)
            .field("color_scheme", &self.color_scheme)
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
    /// Chunks of output the IO worker took from the mailbox and fed (one wake each at most): one per call of the
    /// actor's read callback, which can cover several `read` calls on the PTY.
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
    /// The word boundaries of word selections (`selection-word-chars`); `None` is Ghostty's default set.
    pub(crate) word_boundaries: Option<Vec<u32>>,
    /// A `clear_screen` waits for the program's output to reach a ground point ([`crate::actions`]).
    pub(crate) pending_clear: bool,
    /// The placed Kitty images' pixels, copied on the IO thread after each batch (`kitty::Prefetch`).
    pub(crate) kitty: crate::kitty::Prefetch,
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
    /// Where the terminal's replies to the program go.
    replies: Arc<ReplyOutlet>,
    /// What reaches the IO worker.
    mailbox: Arc<Mailbox>,
    events: Sender<PaneEvent>,
    /// The color scheme the terminal answers queries with (dark when set).
    scheme: SchemeSlot,
    wake: Option<Wake>,
    wake_pending: AtomicBool,
    counters: Counters,
    /// The most input bytes the PTY's actor queues for the child.
    write_limit: usize,
    /// How the program's end is handled, and when the program started (an early failure is reported as one).
    exit: ExitPolicy,
    started: Instant,
    /// Set once the IO worker fed the last of the output (or stopped).
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

    /// Queue bytes the terminal answers the program with (never refused; see [`ReplyOutlet`]).
    pub(crate) fn push_reply(&self, bytes: &[u8]) {
        self.replies.push_reply(bytes);
    }

    /// Make the next paint convert (and re-check every row's selection); the caller holds the VT lock.
    pub(crate) fn touch_capture(&self) {
        lock(&self.capture).touch();
    }

    /// The frame last published by a paint (the bench's, which runs on Linux).
    #[cfg(all(test, target_os = "linux"))]
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
    /// The PTY's actor: the input queue, the resizes, the end of the PTY.
    actor: ActorHandle,
    tty: Tty,
    events: Mutex<Receiver<PaneEvent>>,
    pid: Option<u32>,
    killer: Killer,
    worker: Option<JoinHandle<()>>,
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
    /// [`PaneError::Spawn`] when no PTY can be opened or the program cannot be started (not found, not executable);
    /// [`PaneError::Pty`] when a thread of the pane cannot start; [`PaneError::Terminal`] when libghostty-vt refuses
    /// the size or cannot allocate.
    pub fn spawn(command: &PaneCommand, config: &PaneConfig) -> Result<Self, PaneError> {
        let Spawned { master, mut child } =
            agterm_pty::spawn(command, config.size.pty()).map_err(|err| PaneError::Spawn(err.to_string()))?;
        let pid = child.pid();
        #[cfg(test)]
        tests::LAST_CHILD.with(|last| last.set(Some(pid)));
        let killer = child.killer();
        let parts = Parts::new(&master, config);
        let parts = match parts {
            Ok(parts) => parts,
            Err(err) => {
                // nothing started yet: only the child to stop and reap
                stop_child(&killer, &mut child);
                return Err(err);
            }
        };
        let started = parts.start(master, child, &killer)?;
        Ok(Self {
            shared: started.shared,
            actor: started.actor,
            tty: started.tty,
            events: Mutex::new(started.events),
            pid: Some(pid),
            killer,
            worker: Some(started.worker),
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
    /// pid of the group's leader; `None` once the PTY cannot say (the child side closed). Always `None` on Windows,
    /// whose pseudoconsole has no foreground process group.
    #[must_use]
    pub fn foreground_pid(&self) -> Option<u32> {
        self.tty.foreground_pid()
    }

    /// Whether the program on the PTY is reading a password: canonical input with echo off, the heuristic Ghostty's
    /// termio applies to the PTY's modes (polling them every 200 ms) to set the terminal's `password_input`, which
    /// libghostty-vt leaves to its embedder. `false` once the PTY cannot say, and always on Windows: the pseudoconsole
    /// gives its parent no handle on the console's input buffer, whose mode holds the echo (a process can read it only
    /// by attaching to that console, and a process has one console at a time).
    #[must_use]
    pub fn password_input(&self) -> bool {
        self.tty.password_input()
    }

    /// Queue input for the child (keys, pastes). Never blocks.
    ///
    /// # Errors
    ///
    /// [`PaneError::Backpressure`] when the queue is full (the child is not reading), [`PaneError::Closed`] once the
    /// PTY ended or refused a write (the child side is gone).
    pub fn write(&self, bytes: &[u8]) -> Result<(), PaneError> {
        self.actor.write_input(bytes).map_err(PaneError::from)
    }

    /// Resize the VT (reflowing the primary screen) and then the PTY (the child gets `SIGWINCH`; on Windows the
    /// pseudoconsole resizes its buffer).
    ///
    /// # Errors
    ///
    /// [`PaneError::Terminal`] for a zero size or a failed reflow.
    pub fn resize(&self, size: GridSize) -> Result<(), PaneError> {
        let mut vt = lock(&self.shared.vt);
        vt.terminal
            .resize(size.cols, size.rows, size.cell_width, size.cell_height)
            .map_err(PaneError::terminal)?;
        vt.size = size;
        drop(vt);
        self.actor.resize(size.pty());
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

    /// Change the Kitty image storage per screen (`image-storage-limit` on a config reload, as Ghostty's termio applies
    /// it to a live terminal); a lower limit evicts images now, zero turns the protocol off.
    ///
    /// # Errors
    ///
    /// [`PaneError::Terminal`] when the library refuses the value.
    pub fn set_kitty_image_limit(&self, limit: u64) -> Result<(), PaneError> {
        lock(&self.shared.vt)
            .terminal
            .set_kitty_image_storage_limit(limit)
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

    /// Follow the desktop's appearance: color scheme queries are answered with `scheme` from now on, and a program
    /// that asked for color scheme reports (mode 2031) gets one when the scheme changed, as Ghostty's surface sends
    /// it. Never blocks: the report joins the replies queued for the program.
    pub fn set_color_scheme(&self, scheme: ColorScheme) {
        let dark = scheme == ColorScheme::Dark;
        if self.shared.scheme.swap(dark, Ordering::AcqRel) == dark {
            return;
        }
        let reports = lock(&self.shared.vt).terminal.mode(vt::Mode::COLOR_SCHEME_REPORT);
        if reports {
            self.shared.push_reply(scheme.engine().report());
        }
    }

    /// The color scheme the terminal answers queries with.
    #[must_use]
    pub fn color_scheme(&self) -> ColorScheme {
        crate::effects::scheme_of(&self.shared.scheme)
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
        self.shared.mailbox.answer(id, answer);
    }

    /// Bytes queued for the child and not written yet.
    #[must_use]
    pub fn queued_input(&self) -> usize {
        self.actor.queued()
    }

    #[must_use]
    pub fn stats(&self) -> PaneStats {
        let counters = &self.shared.counters;
        PaneStats {
            bytes: counters.bytes.load(Ordering::Relaxed),
            batches: counters.batches.load(Ordering::Relaxed),
            wakes: counters.wakes.load(Ordering::Relaxed),
            vt_nanos: counters.vt_nanos.load(Ordering::Relaxed),
            max_hold_nanos: counters.max_hold_nanos.load(Ordering::Relaxed),
        }
    }
}

impl Drop for Pane {
    /// Stop the worker (a clipboard wait included), hang up the child's tree (Unix: `SIGHUP` to its process group;
    /// Windows: its job ends), close the PTY (on Unix the kernel hangs up the child's session), join the worker.
    fn drop(&mut self) {
        self.shared.mailbox.shutdown();
        if let Err(err) = self.killer.hang_up() {
            tracing::debug!(%err, "hanging up the pane's child failed");
        }
        self.actor.shutdown();
        self.shared.replies.close();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        crate::search::shut_down(&self.shared);
        let mut vt = lock(&self.shared.vt);
        let Vt { terminal, gesture, .. } = &mut *vt;
        if let Some(gesture) = gesture.take() {
            gesture.close(terminal);
        }
    }
}

fn shared(config: &PaneConfig, events: Sender<PaneEvent>, mailbox: Arc<Mailbox>) -> Result<Shared, PaneError> {
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
    terminal
        .set_mode_default(vt::Mode::GRAPHEME_CLUSTER, config.grapheme_clusters)
        .map_err(PaneError::terminal)?;
    kitty_graphics(&mut terminal, config.kitty_image_limit).map_err(PaneError::terminal)?;
    let capture = Arc::new(Mutex::new(Capture::new().map_err(PaneError::terminal)?));
    let armed: ArmedSlot = Arc::default();
    let divert: DivertSlot = Arc::default();
    let replies = Arc::new(ReplyOutlet::default());
    let scheme: SchemeSlot = Arc::new(AtomicBool::new(config.color_scheme == ColorScheme::Dark));
    terminal
        .set_effects(Box::new(PaneEffects {
            events: events.clone(),
            replies: Arc::clone(&replies),
            capture: Arc::clone(&capture),
            armed: Arc::clone(&armed),
            divert: Arc::clone(&divert),
            xtversion: format!("ghostty {}", vt::version()),
            scheme: Arc::clone(&scheme),
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
            word_boundaries: None,
            pending_clear: false,
            kitty: crate::kitty::Prefetch::default(),
        }),
        capture,
        published: Mutex::new(Arc::new(Frame::default())),
        armed,
        divert,
        search: Mutex::new(SearchSlot::default()),
        search_wake: Condvar::new(),
        replies,
        mailbox,
        events,
        scheme,
        wake: config.wake.clone(),
        wake_pending: AtomicBool::new(false),
        counters: Counters::default(),
        write_limit: config.write_limit,
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
        let held = &mut *vt;
        capture.capture_kitty(held.terminal.kitty_graphics(), &held.kitty);
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

/// Kill the child's tree and reap the child, for a pane that never finished starting (the kill comes first: on Unix
/// the tree is never signalled once the child is reaped).
fn stop_child(killer: &Killer, child: &mut Child) {
    if let Err(err) = killer.kill_tree() {
        tracing::debug!(%err, "killing a pane's child that never finished starting failed");
    }
    let _ = child.wait();
}

/// What a pane needs before its threads start.
struct Parts {
    shared: Arc<Shared>,
    tty: Tty,
    read_pause: ReadPause,
    events: Receiver<PaneEvent>,
}

impl Parts {
    fn new(master: &Master, config: &PaneConfig) -> Result<Self, PaneError> {
        let tty = Tty::of(master)?;
        let (events_tx, events) = mpsc::channel();
        let read_pause = ReadPause::new();
        let mailbox = Arc::new(Mailbox::new(read_pause.clone()));
        Ok(Self {
            shared: Arc::new(shared(config, events_tx, mailbox)?),
            tty,
            read_pause,
            events,
        })
    }

    /// Start the PTY's actor, the IO worker and the waiter. On a failure every part already running is stopped (and
    /// the worker joined), and the child's tree is killed and the child reaped, so nothing outlives the error.
    fn start(self, master: Master, mut child: Child, killer: &Killer) -> Result<Started, PaneError> {
        let Self {
            shared,
            tty,
            read_pause,
            events,
        } = self;
        let stop = |err: PaneError, actor: Option<&ActorHandle>, worker: Option<JoinHandle<()>>, child: &mut Child| {
            shared.mailbox.shutdown();
            shared.replies.close();
            if let Some(actor) = actor {
                actor.shutdown();
            }
            if let Some(worker) = worker {
                let _ = worker.join();
            }
            stop_child(killer, child);
            err
        };
        let actor = match start_actor(&shared, master, read_pause) {
            Ok(actor) => actor,
            Err(err) => return Err(stop(err, None, None, &mut child)),
        };
        shared.replies.attach(&actor);
        let worker = Worker {
            shared: Arc::clone(&shared),
            actor: actor.clone(),
            scanner: Scanner::default(),
            next_read: 0,
            child_exited: false,
        };
        let worker = match spawn_thread("pane-io", move || worker.run()) {
            Ok(worker) => worker,
            Err(err) => return Err(stop(err, Some(&actor), None, &mut child)),
        };
        // the child goes to the waiter only once it runs, so a failed start still holds it to reap
        let (hand, handed) = mpsc::sync_channel::<Child>(1);
        let waiter = {
            let (shared, actor) = (Arc::clone(&shared), actor.clone());
            spawn_thread("pane-wait", move || {
                if let Ok(child) = handed.recv() {
                    wait_child(child, &shared, &actor);
                }
            })
        };
        if let Err(err) = waiter {
            return Err(stop(err, Some(&actor), Some(worker), &mut child));
        }
        if let Err(mpsc::SendError(mut child)) = hand.send(child) {
            let err = PaneError::Pty("the pane-wait thread is gone".to_owned());
            return Err(stop(err, Some(&actor), Some(worker), &mut child));
        }
        Ok(Started {
            shared,
            actor,
            tty,
            events,
            worker,
        })
    }
}

/// The PTY's actor over `master`: every read goes to the pane's mailbox (the callback never waits), the actor's end
/// ends the mailbox's output, and the input queue holds at most the pane's write limit.
fn start_actor(shared: &Shared, master: Master, read_pause: ReadPause) -> Result<ActorHandle, PaneError> {
    #[cfg(test)]
    if tests::FAIL_THREAD.with(std::cell::Cell::get) == Some("pane-pty") {
        return Err(PaneError::Pty("cannot start the pane-pty thread: injected".to_owned()));
    }
    let (reads, ends) = (Arc::clone(&shared.mailbox), Arc::clone(&shared.mailbox));
    let mut config = ActorConfig::new(
        "pane",
        master,
        Box::new(move |bytes| {
            reads.deliver(bytes);
            Vec::new()
        }),
    );
    config.input_limit = shared.write_limit;
    config.read_pause = read_pause;
    config.on_exit = Some(Box::new(move |reason| {
        tracing::debug!(?reason, "the pane's PTY ended");
        ends.end_of_output();
    }));
    Actor::spawn(config).map_err(|err| PaneError::Pty(format!("cannot start the pane's PTY actor: {err}")))
}

/// A pane's running parts.
struct Started {
    shared: Arc<Shared>,
    actor: ActorHandle,
    tty: Tty,
    events: Receiver<PaneEvent>,
    worker: JoinHandle<()>,
}

/// The waiter thread: reap the child, release a clipboard read the IO worker may wait on, end the PTY (Windows'
/// pseudoconsole never ends by itself), give the worker a moment to feed the last output, put the exit message on
/// screen ([`ExitPolicy`]), report the exit.
fn wait_child(mut child: Child, shared: &Shared, actor: &ActorHandle) {
    let (code, signal) = match child.wait() {
        Ok(status) => (status.exit_code(), status.signal().map(str::to_owned)),
        Err(err) => {
            tracing::warn!(%err, "waiting for the pane's child failed");
            (1, None)
        }
    };
    let runtime = shared.started.elapsed();
    shared.mailbox.child_exited();
    actor.child_exited();
    shared.wait_eof(EXIT_GRACE);
    let outcome = shared.exit.outcome(Some(code), runtime);
    if outcome.held() {
        let mut vt = lock(&shared.vt);
        let layout = Layout::of(&mut vt.terminal);
        let message = shared.exit.message(outcome, Some(code), runtime, layout);
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
        location: ClipboardLocation::from_engine(read.location),
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

/// The IO worker: the program's output from the mailbox into the VT.
struct Worker {
    shared: Arc<Shared>,
    /// Woken when the mailbox resumes the actor's reads.
    actor: ActorHandle,
    scanner: Scanner,
    next_read: u64,
    /// A read was denied for the child's exit: every later one is denied without asking.
    child_exited: bool,
}

impl Worker {
    fn run(mut self) {
        loop {
            match self.shared.mailbox.next(self.hold_deadline()) {
                Mail::Output(output) => {
                    let fed = self.feed(&output);
                    if self.shared.mailbox.consumed(output.len()) {
                        self.actor.wake_reader();
                    }
                    if !fed {
                        break;
                    }
                    self.after_batch(output.len());
                }
                Mail::Idle => self.expire_hold(),
                Mail::End => break,
            }
        }
        self.shared.mark_eof();
        self.shared.wake();
    }

    /// When a synchronized-output hold expires, if one is on.
    fn hold_deadline(&self) -> Option<Instant> {
        lock(&self.shared.capture).hold_deadline()
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

    /// Ask the frontend about a clipboard read and wait for its answer without the VT lock, ending expired holds
    /// meanwhile; `None` on shutdown. The child's exit denies the read (nobody is left to read the answer), and every
    /// later one without asking.
    fn park(&mut self, read: ReadCandidate) -> Option<ClipboardAnswer> {
        if self.child_exited {
            return Some(ClipboardAnswer::Denied);
        }
        self.next_read += 1;
        let id = self.next_read;
        self.shared.send(PaneEvent::ClipboardRead(ClipboardReadRequest {
            id,
            location: ClipboardLocation::from_engine(read.location),
            scope: read.scope,
        }));
        self.shared.wake();
        loop {
            match self.shared.mailbox.await_answer(id, self.hold_deadline()) {
                Answer::Given(answer) => return Some(answer),
                Answer::ChildExited => {
                    // keep processing the output instead of waiting on a prompt
                    self.child_exited = true;
                    return Some(ClipboardAnswer::Denied);
                }
                Answer::Shutdown => return None,
                Answer::Idle => self.expire_hold(),
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
            // a new Kitty image's pixels are copied here, not on the thread that paints
            let held = &mut *vt;
            held.kitty.update(held.terminal.kitty_graphics());
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
    use agterm_ghostty_vt::ReadScope;

    const SIZE: GridSize = GridSize {
        cols: 20,
        rows: 4,
        cell_width: 8,
        cell_height: 16,
    };

    thread_local! {
        /// The pid of the last child `Pane::spawn` started on this thread.
        pub(super) static LAST_CHILD: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
        /// A thread name whose start `spawn_thread` (or, for `pane-pty`, the PTY actor's) fails on this thread (fault
        /// injection).
        pub(super) static FAIL_THREAD: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
    }

    const fn assert_send_sync<T: Send + Sync>() {}
    const _: () = assert_send_sync::<Pane>();

    #[cfg(unix)]
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

    /// A mailbox for a terminal driven without a PTY.
    fn mailbox() -> Arc<Mailbox> {
        Arc::new(Mailbox::new(ReadPause::new()))
    }

    #[cfg(unix)]
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

    #[cfg(unix)]
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

    /// `Pane::spawn` of `command` on a worker thread whose thread `fail` cannot start: the error and the child's pid,
    /// or a panic when the cleanup hangs.
    fn failed_start(command: PaneCommand, fail: &'static str) -> (PaneError, u32) {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            FAIL_THREAD.with(|cell| cell.set(Some(fail)));
            let error = Pane::spawn(&command, &PaneConfig::new(SIZE)).unwrap_err();
            let pid = LAST_CHILD.with(std::cell::Cell::get).unwrap();
            let _ = tx.send((error, pid));
        });
        rx.recv_timeout(Duration::from_secs(20))
            .unwrap_or_else(|_| panic!("{fail}: the failed start hung"))
    }

    /// Whether the process `pid` is gone: killed and reaped, not even a zombie left.
    #[cfg(unix)]
    fn gone(pid: u32) -> bool {
        let pid = libc::pid_t::try_from(pid).unwrap();
        // SAFETY: `kill` with signal 0 only checks that the process exists; no memory is passed.
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        !alive && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }

    /// Whether the process `pid` is gone: no process of that id is left to open.
    #[cfg(windows)]
    fn gone(pid: u32) -> bool {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        // SAFETY: opens a handle to the process `pid`, if there is one; no memory is passed.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if process.is_null() {
            return true;
        }
        let mut code = 0;
        // SAFETY: `process` is the handle opened above; the code is written into a live local.
        let read = unsafe { GetExitCodeProcess(process, &raw mut code) } != 0;
        // SAFETY: closes the handle opened above, once.
        unsafe { CloseHandle(process) };
        read && code != STILL_ACTIVE.cast_unsigned()
    }

    /// A child that runs for 30 s unless killed, `SIGHUP` ignored too on Unix (so only the tree's kill ends it).
    fn long_child(ignore_hangup: bool) -> PaneCommand {
        if cfg!(windows) {
            PaneCommand::new("cmd.exe").args(["/D", "/C", "ping -n 30 127.0.0.1 >NUL"])
        } else if ignore_hangup {
            PaneCommand::new("/bin/sh").args(["-c", "trap '' HUP; exec sleep 30"])
        } else {
            PaneCommand::new("/bin/sh").args(["-c", "exec sleep 30"])
        }
    }

    #[test]
    fn a_pane_that_fails_to_start_a_thread_stops_its_child_and_workers() {
        for ignore_hangup in [false, true] {
            for thread in ["pane-pty", "pane-io", "pane-wait"] {
                let (error, pid) = failed_start(long_child(ignore_hangup), thread);
                assert!(matches!(error, PaneError::Pty(_)), "{thread}: {error:?}");
                assert!(
                    gone(pid),
                    "{thread} (SIGHUP ignored: {ignore_hangup}): child {pid} is left"
                );
            }
        }
    }

    #[test]
    fn the_scrollback_limits_reach_the_terminal() {
        let config = PaneConfig {
            scrollback: ScrollbackLimits {
                max_bytes: None,
                max_lines: Some(100),
            },
            ..PaneConfig::new(SIZE)
        };
        let pane = Pane::spawn(&long_child(false), &config).unwrap();
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
        let shared = shared(&PaneConfig::new(SIZE), events, mailbox()).unwrap();
        let read = ReadCandidate {
            location: vt::ClipboardLocation::Standard,
            scope: ReadScope::Data,
        };
        // a BEL outside any OSC: no read callback runs, so the armed answer is left over and must be cleared
        dispatch(&shared, 0x07, read, ClipboardAnswer::Text(Some("secret".to_owned())));
        assert!(lock(&shared.armed).is_none());
        assert_eq!(received.try_iter().collect::<Vec<_>>(), [PaneEvent::Bell]);
        // a read nothing armed: denied, the program reads an empty clipboard
        lock(&shared.vt).terminal.vt_write(b"\x1b]52;c;?\x07");
        assert_eq!(shared.replies.queued(), b"\x1b]52;c;\x07".len());
    }

    #[test]
    fn an_armed_answer_serves_the_read_it_was_armed_for() {
        let (events, _received) = mpsc::channel();
        let shared = shared(&PaneConfig::new(SIZE), events, mailbox()).unwrap();
        lock(&shared.vt).terminal.vt_write(b"\x1b]52;c;?");
        let read = ReadCandidate {
            location: vt::ClipboardLocation::Standard,
            scope: ReadScope::Data,
        };
        dispatch(&shared, 0x07, read, ClipboardAnswer::Text(Some("abc".to_owned())));
        assert!(lock(&shared.armed).is_none());
        assert_eq!(shared.replies.queued(), b"\x1b]52;c;YWJj\x07".len());
    }
}
