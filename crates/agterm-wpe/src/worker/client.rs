//! The app's side of the worker (module docs of `worker`): the start and its handshake, the writer and reader
//! threads, evaluations by id, the shutdown and the reaping.
//!
//! The worker is not trusted (`message` checks what it sends), and what the app keeps for it stays bounded: the
//! pages it knows are the ones the app made (`Command::CreateView`) and has not closed; a frame of any other page is
//! dropped where it arrives, its planes closed. Each page's dmabufs are kept (one `Arc` per buffer, as the in-process
//! engine hands them out) until they leave its set (`Event::BuffersRemoved`), the page closes (`Command::CloseView`)
//! or the worker ends, at most [`MAX_BUFFERS`] of them; at most [`MAX_UNANSWERED`] of a page's frames wait for the
//! app's answer, and one past them is answered here unseen. Events wait in a queue of [`EVENT_QUEUE`], which holds the
//! reader (and so the worker, through the socket) while the app catches up; a shutdown closes it first.
//!
//! What goes the other way is bounded too ([`Outgoing`]): the app's commands and the reader's answers wait for the
//! writer in one queue of at most [`OUTGOING_PACKETS`] packets and [`OUTGOING_BYTES`] bytes. A worker reads its socket
//! all the time (its engine takes each command without waiting), so a queue past either bound means the worker stopped
//! reading: the transport is broken then, as when the worker dies (its pages fail), and nothing that queues ever
//! waits for the worker, neither GPUI's main thread nor the reader with the pages locked.

use std::collections::{BTreeMap, HashMap};
use std::os::fd::{AsFd as _, AsRawFd as _, BorrowedFd, OwnedFd};
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use async_channel::{Receiver, Sender};
use rustix::event::{PollFd, PollFlags, Timespec};
use rustix::net::{AddressFamily, Shutdown, SocketFlags, SocketType};

use super::message::{self, FromWorker, PROTOCOL, ToWorker};
use super::wire::{self, Limits, Packet, WireError};
use super::{WORKER_FD_ENV, WORKER_IMAGE_ENV};
use crate::config::HostConfig;
use crate::event::Event;
use crate::frames::{BufferId, DmaBuf, Frame, FrameContent};
use crate::host::{Command as EngineCommand, Evaluation, StartError, ViewCommand};
use crate::launcher::{self, Provider};
use crate::spec::ViewId;

/// How long the worker may take to say hello and start its engine.
const START_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a shutdown waits for the worker's engine to close its pages.
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a worker that should end may take before its group is killed.
const REAP_GRACE: Duration = Duration::from_secs(2);
/// What an evaluation gets when the worker is gone.
const ENDED: &str = "the web engine ended";
/// The most frames of one page waiting for the app's answer (`Host::frame_presented`): the engine hands over a page's
/// next frame once the one before was answered, so two at most are in flight while it keeps to that.
const MAX_UNANSWERED: u32 = 4;
/// The most dmabufs kept for one page: `WebKit`'s set holds two or three (and the set it replaces, for a moment); past
/// them the oldest is forgotten, passed on as `Event::BuffersRemoved` (its next frame brings its planes again).
const MAX_BUFFERS: usize = 8;
/// How many events wait for the app before the reader waits for it.
const EVENT_QUEUE: usize = 256;
/// The most packets waiting for the writer: seconds of a fast pointer's input and frame answers for several pages
/// while the worker's socket buffer is full too, which a worker that reads never lets happen.
const OUTGOING_PACKETS: usize = 1024;
/// The most bytes waiting for the writer, past which nothing more is queued: as large as the largest body the app
/// reads from the worker. One larger packet (the app's own data, a scheme answer's bytes) still goes when nothing waits.
const OUTGOING_BYTES: usize = 64 << 20;

/// What waits for the writer (module docs): a bounded queue of packets and the bytes they hold, and what a broken
/// transport closes. Pushing never waits.
#[derive(Clone)]
struct Outgoing {
    packets: Sender<Packet>,
    /// The bytes of the packets in `packets` (counted down once the writer takes one).
    bytes: Arc<AtomicUsize>,
    socket: Arc<OwnedFd>,
    events: Sender<Event>,
}

/// The bytes `packet` holds in memory.
fn weight(packet: &Packet) -> usize {
    packet.body.len() + packet.blob.as_ref().map_or(0, Vec::len)
}

impl Outgoing {
    /// Queues `packet` for the writer; past either bound the transport breaks instead.
    fn push(&self, packet: Packet) {
        let weight = weight(&packet);
        let before = self.bytes.fetch_add(weight, Ordering::AcqRel);
        if before > 0 && before.saturating_add(weight) > OUTGOING_BYTES {
            self.bytes.fetch_sub(weight, Ordering::AcqRel);
            self.overflow(format_args!("{before} bytes"));
            return;
        }
        match self.packets.try_send(packet) {
            Ok(()) => {}
            Err(async_channel::TrySendError::Full(_)) => {
                self.bytes.fetch_sub(weight, Ordering::AcqRel);
                self.overflow(format_args!("{OUTGOING_PACKETS} packets"));
            }
            Err(async_channel::TrySendError::Closed(_)) => {
                self.bytes.fetch_sub(weight, Ordering::AcqRel);
                tracing::debug!("the WPE worker has ended: command dropped");
            }
        }
    }

    fn overflow(&self, waiting: std::fmt::Arguments<'_>) {
        if !self.packets.is_closed() {
            tracing::error!("the WPE worker stopped reading its commands ({waiting} wait for it): its pages are gone");
        }
        self.sever();
    }

    /// The writer's next packet, `None` once the queue closed.
    fn take(&self, queue: &Receiver<Packet>) -> Option<Packet> {
        let packet = queue.recv_blocking().ok()?;
        self.bytes.fetch_sub(weight(&packet), Ordering::AcqRel);
        Some(packet)
    }

    /// Breaks the transport without waiting for anything: the queue closes, the socket shuts (a writer stuck on it and
    /// the reader go on, the worker's sends fail), and so do the events, so the app fails the pages at once, even while
    /// a full event queue holds the reader. The reader then ends as on the worker's death and reaps it.
    fn sever(&self) {
        self.packets.close();
        let _ = rustix::net::shutdown(&*self.socket, Shutdown::Both);
        self.events.close();
    }

    /// The packets and bytes waiting.
    #[cfg(test)]
    fn queued(&self) -> (usize, usize) {
        (self.packets.len(), self.bytes.load(Ordering::Acquire))
    }
}

/// The app's pages as the reader keeps them (module docs).
#[derive(Debug, Default)]
struct Pages(HashMap<ViewId, PageFrames>);

/// One page's dmabufs, by buffer (ids only grow, so the first is the oldest), and its frames not answered yet.
#[derive(Debug, Default)]
struct PageFrames {
    buffers: BTreeMap<BufferId, Arc<DmaBuf>>,
    unanswered: u32,
    /// A frame past the unanswered ones was dropped (logged once).
    dropped: bool,
}

impl Pages {
    /// Follows a command the app sends: a page made, closed, or a frame of it answered.
    fn track(&mut self, command: &EngineCommand) {
        match command {
            EngineCommand::CreateView(view, _) => {
                self.0.entry(*view).or_default();
            }
            EngineCommand::CloseView(view) => {
                self.0.remove(view);
            }
            EngineCommand::View(view, ViewCommand::Presented(_)) => {
                if let Some(page) = self.0.get_mut(view) {
                    page.unanswered = page.unanswered.saturating_sub(1);
                }
            }
            _ => {}
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The evaluations waiting for the worker's answer.
#[derive(Default)]
struct Evaluations {
    next: u64,
    waiting: HashMap<u64, Sender<Evaluation>>,
    /// The worker is gone: nothing is answered any more.
    ended: bool,
}

/// The worker process: its `bwrap` (or itself) and the process group they lead.
struct Worker {
    child: Mutex<Option<Child>>,
    group: i32,
}

impl Worker {
    /// Waits `grace` for the worker to end, then kills its group; reaps it either way. Once: a second call (the reader
    /// and the shutdown may both get here) finds nothing to reap.
    fn reap(&self, grace: Duration) {
        // held throughout: a second caller returns only once the worker is reaped
        let mut slot = self.child.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(mut child) = slot.take() else { return };
        let deadline = Instant::now() + grace;
        loop {
            match child.try_wait() {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) if Instant::now() >= deadline => break,
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        tracing::warn!("the WPE worker did not end within {grace:?}: killing it");
        // SAFETY: `kill` only sends a signal, to the group the worker leads (it was started as its leader).
        let _ = unsafe { libc::kill(-self.group, libc::SIGKILL) };
        let _ = child.wait();
    }
}

/// The app's handle on a running worker.
pub(crate) struct Client {
    outgoing: Outgoing,
    socket: Arc<OwnedFd>,
    evaluations: Arc<Mutex<Evaluations>>,
    worker: Arc<Worker>,
    /// The pages the app made, shared with the reader.
    pages: Arc<Mutex<Pages>>,
    /// The event queue's sending side, closed by a shutdown so a reader waiting on a full queue goes on.
    events: Sender<Event>,
    /// Told once the worker's engine stopped; `None` after a shutdown.
    stopped: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    stop_timeout: Duration,
    pid: u32,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").field("pid", &self.pid).finish_non_exhaustive()
    }
}

/// This process's own image, open for the worker to run: the image the app runs even if its file was replaced since.
/// (The worker cannot reach it as `/proc/<pid>/exe` from inside the overlay's user namespace.)
fn own_image() -> Result<OwnedFd, StartError> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH)
        .open("/proc/self/exe")
        .map(OwnedFd::from)
        .map_err(|err| StartError::Worker(format!("this program cannot open its own image: {err}")))
}

/// What the worker runs: the image `image` names (inherited as that descriptor), under the overlay for a vendored WPE
/// `WebKit`.
fn worker_command(image: i32) -> Command {
    let image = PathBuf::from(format!("/proc/self/fd/{image}"));
    match launcher::provider() {
        Provider::System => Command::new(image),
        Provider::Vendored { libdir } => {
            let cwd = std::env::current_dir().ok();
            let mut command = Command::new("bwrap");
            command.args(launcher::overlay_args(&libdir, cwd.as_deref(), false, &[], &image));
            command
        }
    }
}

impl Client {
    /// Starts the worker and its engine with `config` (module docs of `worker`).
    pub(crate) fn start(config: &HostConfig) -> Result<(Self, Receiver<Event>), StartError> {
        let image = own_image()?;
        let (child, socket) = spawn(worker_command(image.as_raw_fd()), Some(image.as_raw_fd()))?;
        drop(image);
        let worker = Arc::new(Worker {
            group: i32::try_from(child.id()).unwrap_or(i32::MAX),
            child: Mutex::new(Some(child)),
        });
        let pid = match handshake(socket.as_fd(), config, START_TIMEOUT) {
            Ok(pid) => pid,
            Err(error) => {
                let _ = rustix::net::shutdown(&socket, Shutdown::Both);
                worker.reap(Duration::ZERO);
                return Err(error);
            }
        };
        Ok(Self::running(socket, worker, pid, STOP_TIMEOUT))
    }

    /// The client of a worker past its handshake: the writer and the reader started.
    fn running(socket: OwnedFd, worker: Arc<Worker>, pid: u32, stop_timeout: Duration) -> (Self, Receiver<Event>) {
        let socket = Arc::new(socket);
        let (packets, queue) = async_channel::bounded::<Packet>(OUTGOING_PACKETS);
        let (events, event_queue) = async_channel::bounded(EVENT_QUEUE);
        let outgoing = Outgoing {
            packets,
            bytes: Arc::new(AtomicUsize::new(0)),
            socket: Arc::clone(&socket),
            events: events.clone(),
        };
        let (stop, stopped) = std::sync::mpsc::sync_channel(1);
        let evaluations = Arc::new(Mutex::new(Evaluations::default()));
        let pages = Arc::new(Mutex::new(Pages::default()));
        let writer = outgoing.clone();
        let spawned = std::thread::Builder::new()
            .name("wpe-worker-out".to_owned())
            .spawn(move || {
                while let Some(packet) = writer.take(&queue) {
                    if let Err(err) = wire::send(writer.socket.as_fd(), &packet) {
                        tracing::debug!(%err, "the WPE worker's socket closed: commands dropped");
                        // nothing more reaches the worker: the transport is broken, whatever the reader still reads
                        writer.sever();
                        break;
                    }
                }
            });
        if let Err(err) = spawned {
            tracing::error!(%err, "no writer thread for the WPE worker");
        }
        let reader = Reader {
            socket: Arc::clone(&socket),
            events: events.clone(),
            stop,
            evaluations: Arc::clone(&evaluations),
            worker: Arc::clone(&worker),
            outgoing: outgoing.clone(),
            pages: Arc::clone(&pages),
        };
        let spawned = std::thread::Builder::new()
            .name("wpe-worker-in".to_owned())
            .spawn(move || reader.run());
        if let Err(err) = spawned {
            tracing::error!(%err, "no reader thread for the WPE worker");
        }
        let client = Self {
            outgoing,
            socket,
            evaluations,
            worker,
            pages,
            events,
            stopped: Mutex::new(Some(stopped)),
            stop_timeout,
            pid,
        };
        (client, event_queue)
    }

    /// The worker's pid.
    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }

    fn queue(&self, message: &ToWorker, blob: Option<Vec<u8>>) {
        match message.packet(blob) {
            Ok(packet) => self.outgoing.push(packet),
            Err(err) => tracing::error!(%err, "a command for the WPE worker could not be written"),
        }
    }

    /// Queues `command` for the worker.
    pub(crate) fn send(&self, command: EngineCommand) {
        lock(&self.pages).track(&command);
        let (message, blob) = ToWorker::command(command);
        self.queue(&message, blob);
    }

    /// Runs `script` in the worker's page `view`; the answer arrives on the returned channel.
    pub(crate) fn evaluate(&self, view: ViewId, script: String, world: Option<String>) -> Receiver<Evaluation> {
        let (reply, result) = async_channel::bounded(1);
        let id = {
            let mut evaluations = self.evaluations.lock().unwrap_or_else(PoisonError::into_inner);
            if evaluations.ended {
                let _ = reply.try_send(Err(ENDED.to_owned()));
                return result;
            }
            evaluations.next += 1;
            let id = evaluations.next;
            evaluations.waiting.insert(id, reply);
            id
        };
        self.queue(
            &ToWorker::Evaluate {
                id,
                view,
                script,
                world,
            },
            None,
        );
        result
    }

    /// Quits the worker's engine and waits for it (a bounded time), then closes the socket and reaps the worker.
    /// Once: a second call returns at once.
    pub(crate) fn shutdown(&self) {
        let taken = self.stopped.lock().unwrap_or_else(PoisonError::into_inner).take();
        let Some(stopped) = taken else { return };
        // nobody reads the events past here: a reader held by a full queue goes on to the worker's last word
        self.events.close();
        self.send(EngineCommand::Quit);
        match stopped.recv_timeout(self.stop_timeout) {
            Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                tracing::warn!("the WPE worker did not stop within {:?}", self.stop_timeout);
            }
        }
        // frees a writer stuck on a worker that stopped reading, and the reader
        let _ = rustix::net::shutdown(&*self.socket, Shutdown::Both);
        self.outgoing.packets.close();
        self.worker.reap(REAP_GRACE);
        lock(&self.pages).0.clear();
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Starts `command` as the worker: its end of a new socket pair inherited (and named in [`WORKER_FD_ENV`]), and
/// `image`, the descriptor it runs, too ([`WORKER_IMAGE_ENV`]); a process group of its own, no stdin. Our end comes
/// back.
fn spawn(mut command: Command, image: Option<i32>) -> Result<(Child, OwnedFd), StartError> {
    let (ours, theirs) =
        rustix::net::socketpair(AddressFamily::UNIX, SocketType::SEQPACKET, SocketFlags::CLOEXEC, None)
            .map_err(|err| StartError::Worker(format!("no socket for the worker: {err}")))?;
    let socket = theirs.as_raw_fd();
    command
        .env(WORKER_FD_ENV, socket.to_string())
        .process_group(0)
        .stdin(Stdio::null());
    if let Some(image) = image {
        command.env(WORKER_IMAGE_ENV, image.to_string());
    }
    let inherited = [Some(socket), image];
    let keep_open = move || {
        for fd in inherited.into_iter().flatten() {
            // SAFETY: `fcntl` only changes the flags of the child's own copy of a descriptor it inherited.
            if unsafe { libc::fcntl(fd, libc::F_SETFD, 0) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(())
    };
    // SAFETY: the closure runs in the forked child before exec and calls only `fcntl`, which is async-signal-safe.
    unsafe { command.pre_exec(keep_open) };
    let child = command
        .spawn()
        .map_err(|err| StartError::Worker(format!("the worker did not start ({command:?}): {err}")))?;
    drop(theirs);
    Ok((child, ours))
}

/// The next packet before `deadline`, the end of a start that may take `timeout`.
fn recv_before(socket: BorrowedFd<'_>, deadline: Instant, timeout: Duration) -> Result<Packet, StartError> {
    let left = deadline.saturating_duration_since(Instant::now());
    let wait = Timespec::try_from(left).unwrap_or(Timespec { tv_sec: 0, tv_nsec: 0 });
    let mut fds = [PollFd::from_borrowed_fd(socket, PollFlags::IN)];
    let ready = loop {
        match rustix::event::poll(&mut fds, Some(&wait)) {
            Ok(ready) => break ready,
            Err(rustix::io::Errno::INTR) => {}
            Err(err) => return Err(StartError::Worker(format!("the worker's socket: {err}"))),
        }
    };
    if ready == 0 {
        return Err(StartError::Worker(format!(
            "the worker did not answer within {timeout:?}"
        )));
    }
    match wire::recv(socket, Limits::FROM_WORKER) {
        Ok(Some(packet)) => Ok(packet),
        Ok(None) => Err(StartError::Worker("the worker ended while starting".to_owned())),
        Err(err) => Err(StartError::Worker(format!("the worker's socket: {err}"))),
    }
}

/// Waits for the worker's hello, hands it `config` and waits for its engine's start, all before `timeout`; the
/// worker's pid.
fn handshake(socket: BorrowedFd<'_>, config: &HostConfig, timeout: Duration) -> Result<u32, StartError> {
    let deadline = Instant::now() + timeout;
    let broken = |err: WireError| StartError::Worker(err.to_string());
    let hello = recv_before(socket, deadline, timeout)?;
    let pid = match message::body::<FromWorker>(&hello).map_err(broken)? {
        FromWorker::Hello { protocol, pid, .. } if protocol == PROTOCOL => pid,
        FromWorker::Hello { protocol, version, .. } => {
            return Err(StartError::Worker(format!(
                "the worker speaks protocol {protocol} ({version}), this build {PROTOCOL} ({}): the executable was \
                 replaced while agterm ran; restart it",
                env!("CARGO_PKG_VERSION")
            )));
        }
        other => return Err(StartError::Worker(format!("the worker said {other:?} before hello"))),
    };
    let start = ToWorker::Start(config.clone()).packet(None).map_err(broken)?;
    wire::send(socket, &start).map_err(broken)?;
    let started = recv_before(socket, deadline, timeout)?;
    match message::body::<FromWorker>(&started).map_err(broken)? {
        FromWorker::Started(Ok(())) => Ok(pid),
        FromWorker::Started(Err(failure)) => Err(failure.into()),
        other => Err(StartError::Worker(format!(
            "the worker said {other:?} instead of its start"
        ))),
    }
}

/// The reader thread: the worker's packets as events and answers, until the socket ends.
struct Reader {
    socket: Arc<OwnedFd>,
    events: Sender<Event>,
    stop: std::sync::mpsc::SyncSender<()>,
    evaluations: Arc<Mutex<Evaluations>>,
    worker: Arc<Worker>,
    outgoing: Outgoing,
    /// The app's pages, their dmabufs and unanswered frames (module docs).
    pages: Arc<Mutex<Pages>>,
}

impl Reader {
    fn run(self) {
        let mut stopped = false;
        loop {
            let packet = match wire::recv(self.socket.as_fd(), Limits::FROM_WORKER) {
                Ok(Some(packet)) => packet,
                Ok(None) => break,
                Err(err) => {
                    tracing::error!(%err, "the WPE worker's socket broke");
                    break;
                }
            };
            match self.take(packet) {
                Ok(true) => stopped = true,
                Ok(false) => {}
                Err(err) => {
                    tracing::error!(%err, "the WPE worker sent a broken message");
                    break;
                }
            }
        }
        if !stopped {
            tracing::error!("the WPE worker ended unasked: its pages are gone");
        }
        let waiting = {
            let mut evaluations = self.evaluations.lock().unwrap_or_else(PoisonError::into_inner);
            evaluations.ended = true;
            std::mem::take(&mut evaluations.waiting)
        };
        for reply in waiting.into_values() {
            let _ = reply.try_send(Err(ENDED.to_owned()));
        }
        self.outgoing.packets.close();
        let _ = rustix::net::shutdown(&*self.socket, Shutdown::Both);
        // the dmabufs go with the worker, and the consumer's events end at once, before the reaping's wait
        lock(&self.pages).0.clear();
        self.events.close();
        self.worker.reap(REAP_GRACE);
    }

    /// Hands `event` to the app, waiting while its queue is full; dropped once a shutdown closed it.
    fn forward(&self, event: Event) {
        if self.events.send_blocking(event).is_err() {
            tracing::debug!("nobody listens to the WPE engine's events any more");
        }
    }

    /// Takes one packet; whether it was the worker's last word ([`FromWorker::Stopped`]).
    fn take(&self, packet: Packet) -> Result<bool, WireError> {
        let event = match message::body::<FromWorker>(&packet)? {
            FromWorker::Event(event) => {
                if let Event::BuffersRemoved { view, buffers } = &event
                    && let Some(page) = lock(&self.pages).0.get_mut(view)
                {
                    for buffer in buffers {
                        page.buffers.remove(buffer);
                    }
                }
                message::checked_event(event)
            }
            FromWorker::DmaBufFrame { frame, dmabuf } => {
                let mut pages = lock(&self.pages);
                // a page the app closed (or never made): its frame goes, and its planes close, here
                let Some(page) = pages.0.get_mut(&frame.view()) else {
                    return Ok(false);
                };
                let known = page.buffers.get(&frame.buffer()).cloned();
                let frame = message::dmabuf_frame(frame, dmabuf, packet.fds, known)?;
                let Some(frame) = self.admit(page, frame) else {
                    return Ok(false);
                };
                let removed = keep_buffer(page, &frame);
                drop(pages);
                if let Some(removed) = removed {
                    self.forward(removed);
                }
                Event::Frame(frame)
            }
            FromWorker::ShmFrame { frame, regions } => {
                let mut pages = lock(&self.pages);
                let Some(page) = pages.0.get_mut(&frame.view()) else {
                    return Ok(false);
                };
                let frame = message::shm_frame(frame, &regions, packet.blob)?;
                let Some(frame) = self.admit(page, frame) else {
                    return Ok(false);
                };
                Event::Frame(frame)
            }
            FromWorker::Evaluation { id, result } => {
                let reply = self
                    .evaluations
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .waiting
                    .remove(&id);
                if let Some(reply) = reply {
                    let _ = reply.try_send(result);
                }
                return Ok(false);
            }
            FromWorker::Stopped => {
                let _ = self.stop.try_send(());
                return Ok(true);
            }
            other @ (FromWorker::Hello { .. } | FromWorker::Started(_)) => {
                return Err(WireError::Broken(format!("{other:?} after the start")));
            }
        };
        self.forward(event);
        Ok(false)
    }

    /// `frame` of `page` for the app, unless [`MAX_UNANSWERED`] of the page's frames already wait for its answer: then
    /// it is answered here, unseen, so the page goes on drawing. The answer joins the bounded queue of [`Outgoing`]
    /// (never waiting, the pages locked): a worker that floods frames and reads none of the answers breaks the
    /// transport.
    fn admit(&self, page: &mut PageFrames, frame: Frame) -> Option<Frame> {
        if page.unanswered < MAX_UNANSWERED {
            page.unanswered += 1;
            return Some(frame);
        }
        if !std::mem::replace(&mut page.dropped, true) {
            tracing::warn!(
                view = ?frame.view,
                "the WPE worker sends a page's frames faster than they are answered: the extra ones are dropped"
            );
        }
        let answer = EngineCommand::View(frame.view, ViewCommand::Presented(frame.buffer));
        let (message, blob) = ToWorker::command(answer);
        if let Ok(packet) = message.packet(blob) {
            self.outgoing.push(packet);
        }
        None
    }
}

/// Keeps `frame`'s dmabuf for `page` when it is a buffer not seen yet; past [`MAX_BUFFERS`] the oldest is forgotten,
/// and the event that tells the app so comes back.
fn keep_buffer(page: &mut PageFrames, frame: &Frame) -> Option<Event> {
    let FrameContent::DmaBuf(dmabuf) = &frame.content else {
        return None;
    };
    if page.buffers.contains_key(&frame.buffer) {
        return None;
    }
    page.buffers.insert(frame.buffer, Arc::clone(dmabuf));
    let mut removed = Vec::new();
    while page.buffers.len() > MAX_BUFFERS {
        if let Some((oldest, _)) = page.buffers.pop_first() {
            removed.push(oldest);
        }
    }
    (!removed.is_empty()).then_some(Event::BuffersRemoved {
        view: frame.view,
        buffers: removed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in worker: `sleep`, in a group of its own, on our end of a fresh pair; the other end comes back unread.
    fn sleeper() -> (Arc<Worker>, OwnedFd, OwnedFd) {
        let (ours, theirs) =
            rustix::net::socketpair(AddressFamily::UNIX, SocketType::SEQPACKET, SocketFlags::CLOEXEC, None).unwrap();
        let child = Command::new("sleep").arg("30").process_group(0).spawn().unwrap();
        let worker = Arc::new(Worker {
            group: i32::try_from(child.id()).unwrap(),
            child: Mutex::new(Some(child)),
        });
        (worker, ours, theirs)
    }

    fn alive(pid: i32) -> bool {
        // SAFETY: signal 0 only checks that the process exists.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    #[test]
    fn a_shutdown_ends_a_worker_that_never_reads_within_its_bounds() {
        let (worker, ours, _unread) = sleeper();
        let pid = worker.group;
        let (client, events) = Client::running(ours, Arc::clone(&worker), 0, Duration::from_millis(200));
        // more than the socket buffers: the writer blocks on the peer
        for _ in 0..400 {
            client.send(EngineCommand::Clipboard("x".repeat(30_000)));
        }
        let started = Instant::now();
        client.shutdown();
        let took = started.elapsed();
        assert!(took < Duration::from_secs(4), "the shutdown took {took:?}");
        assert!(!alive(pid), "the worker was killed and reaped");
        assert!(events.recv_blocking().is_err(), "the event channel closed");
    }

    #[test]
    fn a_worker_that_goes_away_answers_every_evaluation_and_closes_the_events() {
        let (worker, ours, theirs) = sleeper();
        let (client, events) = Client::running(ours, Arc::clone(&worker), 0, Duration::from_millis(200));
        let waiting = client.evaluate(ViewId(1), "1".to_owned(), None);
        drop(theirs);
        // SAFETY: as in `alive`.
        unsafe { libc::kill(worker.group, libc::SIGKILL) };
        assert_eq!(waiting.recv_blocking(), Ok(Err(ENDED.to_owned())));
        assert!(events.recv_blocking().is_err());
        assert_eq!(
            client.evaluate(ViewId(1), "1".to_owned(), None).recv_blocking(),
            Ok(Err(ENDED.to_owned())),
            "an evaluation after the end is answered at once"
        );
    }

    fn create_view(view: ViewId) -> EngineCommand {
        EngineCommand::CreateView(
            view,
            Box::new(crate::spec::ViewSpec {
                session: crate::spec::SessionId(1),
                settings: crate::spec::Settings::overlay(true),
                scripts: Vec::new(),
                message_handlers: Vec::new(),
                zoom: 1.0,
                background: None,
                geometry: crate::spec::Geometry {
                    width: 4,
                    height: 4,
                    scale: 1.0,
                },
            }),
        )
    }

    /// A dmabuf frame of `view` in `buffer`, its one plane a plain file whose inode comes back too.
    fn dmabuf_frame(view: ViewId, buffer: u64) -> (Frame, (u64, u64)) {
        let file = tempfile::tempfile().unwrap();
        let stat = rustix::fs::fstat(&file).unwrap();
        let dmabuf = DmaBuf {
            width: 4,
            height: 4,
            fourcc: crate::fourcc(b"XR24"),
            modifier: 0,
            planes: vec![crate::frames::Plane {
                fd: OwnedFd::from(file),
                offset: 0,
                stride: 16,
            }],
        };
        let frame = Frame {
            view,
            buffer: BufferId(buffer),
            width: 4,
            height: 4,
            damage: Vec::new(),
            content: FrameContent::DmaBuf(Arc::new(dmabuf)),
        };
        (frame, (stat.st_dev, stat.st_ino))
    }

    /// The (device, inode) pairs this process's descriptors name.
    fn open_inodes() -> Vec<(u64, u64)> {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::read_dir("/proc/self/fd")
            .unwrap()
            .flatten()
            .filter_map(|entry| std::fs::metadata(entry.path()).ok())
            .map(|metadata| (metadata.dev(), metadata.ino()))
            .collect()
    }

    #[test]
    fn a_page_s_dmabufs_go_when_it_closes_and_stay_bounded_while_it_runs() {
        let (view, other) = (ViewId(7), ViewId(8));
        let mut pages = Pages::default();
        pages.track(&create_view(view));
        pages.track(&create_view(other));
        let mut inodes = Vec::new();
        let mut removed = Vec::new();
        for buffer in 1..=10 {
            let (frame, inode) = dmabuf_frame(view, buffer);
            inodes.push(inode);
            let page = pages.0.get_mut(&view).unwrap();
            if let Some(Event::BuffersRemoved { view: id, buffers }) = keep_buffer(page, &frame) {
                assert_eq!(id, view);
                removed.extend(buffers);
            }
            // a buffer seen again keeps its first dmabuf
            assert!(keep_buffer(page, &frame).is_none());
        }
        assert_eq!(
            removed,
            [BufferId(1), BufferId(2)],
            "past the bound the oldest go, said so"
        );
        assert_eq!(pages.0[&view].buffers.len(), MAX_BUFFERS);
        let open = open_inodes();
        assert!(
            inodes[2..].iter().all(|inode| open.contains(inode)),
            "the kept planes stay open"
        );
        assert!(
            !inodes[..2].iter().any(|inode| open.contains(inode)),
            "the forgotten ones closed"
        );
        let (frame, kept) = dmabuf_frame(other, 1);
        keep_buffer(pages.0.get_mut(&other).unwrap(), &frame);
        drop(frame);
        pages.track(&EngineCommand::CloseView(view));
        assert!(!pages.0.contains_key(&view));
        let open = open_inodes();
        assert!(
            !inodes.iter().any(|inode| open.contains(inode)),
            "a closed page's planes closed"
        );
        assert!(open.contains(&kept), "another page keeps its own");
    }

    #[test]
    fn answers_count_down_a_page_s_unanswered_frames() {
        let view = ViewId(3);
        let mut pages = Pages::default();
        pages.track(&create_view(view));
        pages.0.get_mut(&view).unwrap().unanswered = 2;
        let presented = || EngineCommand::View(view, ViewCommand::Presented(BufferId(1)));
        pages.track(&presented());
        assert_eq!(pages.0[&view].unanswered, 1);
        pages.track(&presented());
        pages.track(&presented());
        assert_eq!(pages.0[&view].unanswered, 0, "never below none");
        // an answer for a page the app does not have changes nothing
        pages.track(&EngineCommand::View(ViewId(99), ViewCommand::Presented(BufferId(1))));
        assert!(!pages.0.contains_key(&ViewId(99)));
    }

    /// An SHM frame packet of `view` in `buffer`: one 1x1 region of a 4x4 frame.
    fn shm_packet(view: ViewId, buffer: u64) -> Packet {
        let rect = crate::frames::Rect {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        };
        FromWorker::event_packet(Event::Frame(Frame {
            view,
            buffer: BufferId(buffer),
            width: 4,
            height: 4,
            damage: Vec::new(),
            content: FrameContent::Shm(vec![crate::frames::ShmRegion {
                rect,
                pixels: vec![1, 2, 3, 4],
            }]),
        }))
        .unwrap()
    }

    /// The next command the client sent the stand-in worker on `theirs`.
    fn next_command(theirs: &OwnedFd) -> ToWorker {
        let packet = wire::recv(theirs.as_fd(), Limits::FROM_APP).unwrap().unwrap();
        message::body(&packet).unwrap()
    }

    /// A client of a stand-in worker on `theirs` with page `view` made: the client, its events, the worker's end.
    fn with_page(view: ViewId) -> (Client, Receiver<Event>, OwnedFd) {
        let (worker, ours, theirs) = sleeper();
        let (client, events) = Client::running(ours, worker, 0, Duration::from_millis(200));
        client.send(create_view(view));
        let made = next_command(&theirs);
        assert!(matches!(made, ToWorker::Command(EngineCommand::CreateView(id, _)) if id == view));
        (client, events, theirs)
    }

    /// An event packet that marks a place in the worker's stream.
    fn marker(view: ViewId) -> Packet {
        FromWorker::Event(Event::CloseRequested { view }).packet().unwrap()
    }

    #[test]
    fn a_frame_of_a_page_the_app_never_made_is_dropped() {
        let (client, events, theirs) = with_page(ViewId(5));
        wire::send(theirs.as_fd(), &shm_packet(ViewId(6), 1)).unwrap();
        wire::send(theirs.as_fd(), &marker(ViewId(6))).unwrap();
        let next = events.recv_blocking();
        assert!(
            matches!(next, Ok(Event::CloseRequested { view }) if view == ViewId(6)),
            "{next:?}"
        );
        drop(theirs);
        client.shutdown();
    }

    #[test]
    fn frames_past_the_unanswered_bound_are_answered_unseen() {
        let view = ViewId(5);
        let (client, events, theirs) = with_page(view);
        let over = u64::from(MAX_UNANSWERED) + 1;
        for buffer in 1..=over {
            wire::send(theirs.as_fd(), &shm_packet(view, buffer)).unwrap();
        }
        wire::send(theirs.as_fd(), &marker(view)).unwrap();
        let arrived: Vec<BufferId> = (0..=MAX_UNANSWERED)
            .map_while(|_| match events.recv_blocking() {
                Ok(Event::Frame(frame)) => Some(frame.buffer),
                _ => None,
            })
            .collect();
        let expected: Vec<BufferId> = (1..over).map(BufferId).collect();
        assert_eq!(arrived, expected, "the frames up to the bound, not the one past it");
        let answer = next_command(&theirs);
        let answered = matches!(answer, ToWorker::Command(EngineCommand::View(id, ViewCommand::Presented(buffer))) if id == view && buffer == BufferId(over));
        assert!(answered, "{answer:?}");
        // once the app answers one, the next frame reaches it again
        client.send(EngineCommand::View(view, ViewCommand::Presented(BufferId(1))));
        wire::send(theirs.as_fd(), &shm_packet(view, 9)).unwrap();
        let next = events.recv_blocking();
        assert!(
            matches!(next, Ok(Event::Frame(ref frame)) if frame.buffer == BufferId(9)),
            "{next:?}"
        );
        drop(theirs);
        client.shutdown();
    }

    #[test]
    fn a_malformed_frame_breaks_the_transport() {
        let view = ViewId(5);
        let (client, events, theirs) = with_page(view);
        let mut broken = shm_packet(view, 1);
        broken.blob = Some(vec![0; 3]);
        wire::send(theirs.as_fd(), &broken).unwrap();
        assert!(events.recv_blocking().is_err(), "the reader ended on the broken frame");
        drop(theirs);
        client.shutdown();
    }

    #[test]
    fn a_full_event_queue_holds_the_reader_and_a_shutdown_frees_it() {
        let (worker, ours, theirs) = sleeper();
        let pid = worker.group;
        let (client, events) = Client::running(ours, Arc::clone(&worker), 0, Duration::from_millis(200));
        // more events than the queue holds, nobody reading: the reader waits on the queue
        let event = FromWorker::Event(Event::CloseRequested { view: ViewId(1) })
            .packet()
            .unwrap();
        for _ in 0..EVENT_QUEUE + 10 {
            wire::send(theirs.as_fd(), &event).unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while events.len() < EVENT_QUEUE && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(events.len(), EVENT_QUEUE, "the queue filled and stays bounded");
        let started = Instant::now();
        client.shutdown();
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "the shutdown took {:?}",
            started.elapsed()
        );
        assert!(!alive(pid));
        // the reader went on past the full queue and ended: only the test and the client hold the worker now
        let deadline = Instant::now() + Duration::from_secs(5);
        while Arc::strong_count(&worker) > 2 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(Arc::strong_count(&worker), 2, "the reader thread ended");
        drop(theirs);
    }

    /// Whether `events` ends (the transport broke) within `wait`; the events taken before the end.
    fn ends_within(events: &Receiver<Event>, wait: Duration) -> (bool, usize) {
        let deadline = Instant::now() + wait;
        let mut taken = 0;
        while Instant::now() < deadline {
            match events.try_recv() {
                Ok(_) => taken += 1,
                Err(async_channel::TryRecvError::Closed) => return (true, taken),
                Err(async_channel::TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        (false, taken)
    }

    /// The client's queue for the worker within its bounds.
    fn assert_bounded(client: &Client) {
        let (packets, bytes) = client.outgoing.queued();
        assert!(packets <= OUTGOING_PACKETS, "{packets} packets wait for the worker");
        assert!(bytes <= OUTGOING_BYTES, "{bytes} bytes wait for the worker");
    }

    /// The transport of `client` broke: its events ended, an evaluation is answered at once, and a quit still ends the
    /// worker `pid` in time.
    fn assert_broken_and_quits(client: &Client, events: &Receiver<Event>, pid: i32) -> usize {
        let (ended, taken) = ends_within(events, Duration::from_secs(10));
        assert!(ended, "the transport broke and the pages' events ended");
        assert_eq!(
            client.evaluate(ViewId(5), "1".to_owned(), None).recv_blocking(),
            Ok(Err(ENDED.to_owned()))
        );
        let started = Instant::now();
        client.shutdown();
        let took = started.elapsed();
        assert!(took < Duration::from_secs(4), "the shutdown took {took:?}");
        assert!(!alive(pid), "the worker was reaped");
        taken
    }

    #[test]
    fn a_worker_that_floods_frames_and_reads_no_answer_breaks_the_transport_within_bounds() {
        let view = ViewId(5);
        let (worker, ours, theirs) = sleeper();
        let pid = worker.group;
        let (client, events) = Client::running(ours, worker, 0, Duration::from_millis(200));
        client.send(create_view(view));
        // far more frames than the queue and the socket's buffer hold answers for; `theirs` reads nothing
        let frame = shm_packet(view, 1);
        let mut sent = 0;
        while sent < 20_000 && wire::send(theirs.as_fd(), &frame).is_ok() {
            sent += 1;
            assert_bounded(&client);
        }
        assert!(sent < 20_000, "the app kept reading a worker that reads nothing");
        let frames = assert_broken_and_quits(&client, &events, pid);
        assert_eq!(
            frames, MAX_UNANSWERED as usize,
            "the frames up to the unanswered bound reached the app"
        );
    }

    /// GPUI's main thread sending `count` of the commands `command` makes to a stand-in worker that never reads: each
    /// send returns at once, the queue stays bounded, the transport breaks and a quit still completes in time.
    fn flood_a_stalled_worker(count: usize, command: impl Fn(usize) -> EngineCommand) {
        let (worker, ours, _unread) = sleeper();
        let pid = worker.group;
        let (client, events) = Client::running(ours, worker, 0, Duration::from_millis(200));
        client.send(create_view(ViewId(5)));
        let mut slowest = Duration::ZERO;
        for at in 0..count {
            let command = command(at);
            let started = Instant::now();
            client.send(command);
            slowest = slowest.max(started.elapsed());
            assert_bounded(&client);
        }
        assert!(slowest < Duration::from_millis(250), "a send took {slowest:?}");
        assert_broken_and_quits(&client, &events, pid);
    }

    #[test]
    fn a_stalled_worker_breaks_the_transport_before_many_small_commands_outgrow_the_queue() {
        flood_a_stalled_worker(20_000, |at| {
            let x = f64::from(u32::try_from(at % 1000).unwrap());
            let motion = crate::input::Pointer {
                kind: crate::input::PointerKind::Move,
                x,
                y: x,
                modifiers: crate::input::Modifiers::empty(),
            };
            EngineCommand::View(ViewId(5), ViewCommand::Input(crate::input::Input::Pointer(motion)))
        });
    }

    #[test]
    fn a_stalled_worker_breaks_the_transport_before_large_commands_outgrow_the_queue_s_bytes() {
        // a quarter MiB each, in memfds: the socket's buffer takes some, the bytes' bound trips before the count's
        flood_a_stalled_worker(1000, |at| {
            EngineCommand::Clipboard(
                char::from(b'a' + u8::try_from(at % 26).unwrap())
                    .to_string()
                    .repeat(1 << 18),
            )
        });
    }

    #[test]
    fn one_packet_larger_than_the_bytes_bound_goes_when_nothing_waits() {
        let (ours, _theirs) =
            rustix::net::socketpair(AddressFamily::UNIX, SocketType::SEQPACKET, SocketFlags::CLOEXEC, None).unwrap();
        let (packets, queue) = async_channel::bounded(OUTGOING_PACKETS);
        let (events, _event_queue) = async_channel::bounded(1);
        let outgoing = Outgoing {
            packets,
            bytes: Arc::new(AtomicUsize::new(0)),
            socket: Arc::new(ours),
            events,
        };
        let big = || Packet {
            body: b"{}".to_vec(),
            blob: Some(vec![0; OUTGOING_BYTES + 1]),
            fds: Vec::new(),
        };
        outgoing.push(big());
        assert_eq!(outgoing.queued(), (1, OUTGOING_BYTES + 3), "an empty queue takes it");
        outgoing.take(&queue).unwrap();
        assert_eq!(outgoing.queued(), (0, 0), "the writer took it");
        outgoing.push(big());
        outgoing.push(Packet {
            body: b"{}".to_vec(),
            ..Packet::default()
        });
        assert!(outgoing.packets.is_closed(), "past the bound the transport broke");
        assert!(outgoing.events.is_closed());
        assert_eq!(
            outgoing.queued(),
            (1, OUTGOING_BYTES + 3),
            "the refused packet is not counted"
        );
    }

    #[test]
    fn a_worker_that_never_says_hello_fails_the_start_in_time_and_is_reaped() {
        let mut sleep = Command::new("sleep");
        sleep.arg("30");
        let (child, socket) = spawn(sleep, None).unwrap();
        let pid = i32::try_from(child.id()).unwrap();
        let worker = Worker {
            group: pid,
            child: Mutex::new(Some(child)),
        };
        let config = HostConfig {
            frames: crate::config::FrameMode::Shm,
            schemes: Vec::new(),
        };
        let started = Instant::now();
        let error = handshake(socket.as_fd(), &config, Duration::from_millis(300)).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(error.to_string().contains("did not answer"), "{error}");
        worker.reap(Duration::ZERO);
        assert!(!alive(pid));
    }
}
