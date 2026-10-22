//! The watch on `remotes.conf` (upstream's `RemotesWatcher` file sources; the GTK build's `remotes_watcher.rs` over
//! `gio::FileMonitor`), through `notify`'s recommended watcher (inotify on Linux, `FSEvents` on macOS,
//! `ReadDirectoryChangesW` on Windows): the file, its nearest existing directory and, when the file is a symlink, the
//! nearest existing directory of every link target on the way to the real file. A change that touches the file or the
//! way to it re-arms the watches, so a directory created later, a symlink target recreated or re-pointed, and an
//! editor's replace-on-save are all seen, then runs the change handler, which reads the file again in the core
//! ([`ngterm_core::Core::refresh_remotes`]).
//!
//! - **One thread** owns the watcher: it takes the paths to follow and the watcher's events from one channel.
//! - **Debounced**: a burst of events (an editor's save is several) settles for [`DEBOUNCE`] after the last one, at most
//!   [`DEBOUNCE_CAP`] after the first, then the watches are re-armed and the handler runs once. Re-arming before the
//!   handler reads means a change after the re-arm is seen by the new watches and one before it by the read; upstream
//!   re-reads on every event, which the burst's last read equals.
//! - **Only changes count**: inotify also reports opens and reads (the core's own read of the file among them), which
//!   would loop; a rescan (a queue overflow) counts as a change.
//! - **Following** a path arms the watches and then runs the handler once, so a change between the caller's own read
//!   (the core's launch read, or its re-point to a new config directory) and the watch's start is not lost; the
//!   core's report rule (once per clean-to-broken change) keeps that read from repeating a banner.
//! - Paths are made absolute and normalized first, as the watcher reports them, so a relative or unnormalized config
//!   directory still matches its events.

use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use notify::event::{AccessKind, AccessMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};

/// How long a burst of changes settles before one read.
pub const DEBOUNCE: Duration = Duration::from_millis(50);
/// The longest a steady stream of changes holds the read back.
pub const DEBOUNCE_CAP: Duration = Duration::from_millis(500);

enum Message {
    Follow(PathBuf),
    Event(notify::Result<Event>),
    Stop,
}

/// Follows one `remotes.conf`; dropping it stops the watch.
pub struct RemotesWatcher {
    control: mpsc::Sender<Message>,
    /// The file followed, as last asked.
    path: Option<PathBuf>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl RemotesWatcher {
    /// A watcher that runs `on_change` (on its own thread) once per settled burst of changes of the file it follows.
    ///
    /// # Errors
    ///
    /// The watcher's thread could not start.
    pub fn new(on_change: impl Fn() + Send + 'static) -> std::io::Result<Self> {
        let (control, messages) = mpsc::channel();
        let events = control.clone();
        let thread = std::thread::Builder::new()
            .name("ngterm-remotes-watch".to_owned())
            .spawn(move || run(&messages, &events, &on_change))?;
        Ok(Self {
            control,
            path: None,
            thread: Some(thread),
        })
    }

    /// Watches `path`, as the launch and a config directory change ask; the same path again keeps the watches.
    pub fn follow(&mut self, path: &Path) {
        if self.path.as_deref() == Some(path) {
            return;
        }
        self.path = Some(path.to_path_buf());
        if self.control.send(Message::Follow(path.to_path_buf())).is_err() {
            tracing::debug!("remotes.conf: the watcher's thread is gone");
        }
    }
}

impl Drop for RemotesWatcher {
    fn drop(&mut self) {
        let _ = self.control.send(Message::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// What the thread follows: the file, the paths whose change concerns it, and the watcher over them.
#[derive(Default)]
struct Watch {
    path: Option<PathBuf>,
    /// The file itself and every symlink target on the way to the real file.
    interest: Vec<PathBuf>,
    watcher: Option<RecommendedWatcher>,
}

impl Watch {
    /// Replaces the watcher with one over the paths the file needs now; the new one starts before the old one stops,
    /// so an edit in between is not lost.
    fn arm(&mut self, events: &mpsc::Sender<Message>) {
        let Some(path) = self.path.as_deref().map(absolute) else {
            return;
        };
        let sender = events.clone();
        let watcher = notify::recommended_watcher(move |event| {
            let _ = sender.send(Message::Event(event));
        });
        match watcher {
            Ok(mut watcher) => {
                for watched in watched_paths(&path) {
                    if let Err(error) = watcher.watch(&watched, RecursiveMode::NonRecursive) {
                        tracing::debug!(path = %watched.display(), %error, "remotes.conf: no watch");
                    }
                }
                self.watcher = Some(watcher);
            }
            Err(error) => tracing::warn!(%error, "remotes.conf: no watcher; edits are seen at the next launch"),
        }
        let mut interest = vec![path.clone()];
        interest.extend(symlink_targets(&path));
        self.interest = interest;
    }

    /// Whether `event` changed the file or the way to it.
    fn concerns(&self, event: &Event) -> bool {
        is_change(event.kind)
            && (event.need_rescan() || event.paths.iter().any(|changed| concerns(&self.interest, changed)))
    }
}

/// The thread: follows what [`RemotesWatcher::follow`] asks, debounces the changes, re-arms and reports.
fn run(messages: &mpsc::Receiver<Message>, events: &mpsc::Sender<Message>, on_change: &dyn Fn()) {
    let mut watch = Watch::default();
    let mut burst = Burst::default();
    loop {
        let message = match burst.due() {
            None => messages.recv().map_err(|_| RecvTimeoutError::Disconnected),
            Some(due) => messages.recv_timeout(due.saturating_duration_since(Instant::now())),
        };
        match message {
            Ok(Message::Follow(path)) => {
                tracing::debug!(path = %path.display(), "remotes.conf: following");
                watch.path = Some(path);
                watch.arm(events);
                // a change between the caller's own read and the watch's start is read now
                on_change();
            }
            Ok(Message::Event(Ok(event))) => {
                if watch.concerns(&event) {
                    burst.changed(Instant::now());
                }
            }
            Ok(Message::Event(Err(error))) => tracing::debug!(%error, "remotes.conf: a watch error"),
            Err(RecvTimeoutError::Timeout) => {
                burst = Burst::default();
                watch.arm(events);
                tracing::debug!("remotes.conf: changed; reading it again");
                on_change();
            }
            Ok(Message::Stop) | Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// A burst of changes waiting to settle.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Burst {
    /// The first change of the burst and the last.
    span: Option<(Instant, Instant)>,
}

impl Burst {
    fn changed(&mut self, now: Instant) {
        let first = self.span.map_or(now, |(first, _)| first);
        self.span = Some((first, now));
    }

    /// When the burst is read: [`DEBOUNCE`] after its last change, at most [`DEBOUNCE_CAP`] after its first.
    fn due(&self) -> Option<Instant> {
        self.span
            .map(|(first, last)| (last + DEBOUNCE).min(first + DEBOUNCE_CAP))
    }
}

/// Whether an event of `kind` is a change: anything but an open, a read or a close without writing.
fn is_change(kind: EventKind) -> bool {
    match kind {
        EventKind::Access(AccessKind::Close(AccessMode::Write)) => true,
        EventKind::Access(_) => false,
        _ => true,
    }
}

/// Whether a change of `changed` concerns the file: it is one of `interest` or a directory on the way to one, which a
/// directory created later or removed is.
fn concerns(interest: &[PathBuf], changed: &Path) -> bool {
    interest.iter().any(|path| path.starts_with(changed))
}

/// The file, its nearest existing directory and, for a symlink, the nearest existing directory of each target on the
/// way to the real file. Upstream's `watchedPaths` adds only the first target's directory and relies on its descriptor
/// on the file, which follows the whole chain; a directory watch does not follow one, so each hop's directory is
/// watched here, as in the GTK build.
fn watched_paths(path: &Path) -> Vec<PathBuf> {
    let mut paths = vec![path.to_path_buf(), nearest_existing_directory(path)];
    for target in symlink_targets(path) {
        let directory = nearest_existing_directory(&target);
        if !paths.contains(&directory) {
            paths.push(directory);
        }
    }
    paths
}

/// The hops a symlink chain at `path` takes, each relative to the directory of the link naming it and normalized
/// lexically (Foundation's `standardizedFileURL`); empty when `path` is no symlink. A loop or a chain longer than the
/// kernel follows ends it.
fn symlink_targets(path: &Path) -> Vec<PathBuf> {
    /// Linux's `MAXSYMLINKS`.
    const MAX_HOPS: usize = 40;
    let mut targets: Vec<PathBuf> = Vec::new();
    let mut link = path.to_path_buf();
    while targets.len() < MAX_HOPS {
        let Some(target) = symlink_target(&link) else {
            break;
        };
        if target == path || targets.contains(&target) {
            break;
        }
        targets.push(target.clone());
        link = target;
    }
    targets
}

/// Where the symlink at `path` points, relative to its directory and normalized lexically; `None` when `path` is no
/// symlink.
fn symlink_target(path: &Path) -> Option<PathBuf> {
    let destination = std::fs::read_link(path).ok()?;
    let joined = path.parent().unwrap_or(Path::new("/")).join(destination);
    Some(normalized(&joined))
}

/// `path` made absolute against the working directory and normalized lexically, so it compares equal to the paths the
/// watcher's events carry.
fn absolute(path: &Path) -> PathBuf {
    normalized(&std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()))
}

fn normalized(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other),
        }
    }
    result
}

/// The closest ancestor directory of the absolute `path` that exists, the root at worst (upstream's
/// `nearestExistingDirectory`).
fn nearest_existing_directory(path: &Path) -> PathBuf {
    let mut directory = path.parent().unwrap_or(Path::new("/"));
    while !directory.is_dir() {
        match directory.parent() {
            Some(parent) => directory = parent,
            None => break,
        }
    }
    directory.to_path_buf()
}

#[cfg(test)]
mod tests;
