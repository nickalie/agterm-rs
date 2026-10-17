//! The app's side of the worker (module docs of `worker`): the start and its handshake, the writer and reader
//! threads, evaluations by id, the shutdown and the reaping.

use std::collections::HashMap;
use std::os::fd::{AsFd as _, AsRawFd as _, BorrowedFd, OwnedFd};
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use async_channel::{Receiver, Sender};
use rustix::event::{PollFd, PollFlags, Timespec};
use rustix::net::{AddressFamily, Shutdown, SocketFlags, SocketType};

use super::message::{self, FromWorker, PROTOCOL, ToWorker};
use super::wire::{self, Packet, WireError};
use super::{WORKER_FD_ENV, WORKER_IMAGE_ENV};
use crate::config::HostConfig;
use crate::event::Event;
use crate::frames::{BufferId, DmaBuf};
use crate::host::{Command as EngineCommand, Evaluation, StartError};
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
    outgoing: Sender<Packet>,
    socket: Arc<OwnedFd>,
    evaluations: Arc<Mutex<Evaluations>>,
    worker: Arc<Worker>,
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
        let (outgoing, queue) = async_channel::unbounded::<Packet>();
        let (events, event_queue) = async_channel::unbounded();
        let (stop, stopped) = std::sync::mpsc::sync_channel(1);
        let evaluations = Arc::new(Mutex::new(Evaluations::default()));
        let writer_socket = Arc::clone(&socket);
        let spawned = std::thread::Builder::new()
            .name("wpe-worker-out".to_owned())
            .spawn(move || {
                while let Ok(packet) = queue.recv_blocking() {
                    if let Err(err) = wire::send(writer_socket.as_fd(), &packet) {
                        tracing::debug!(%err, "the WPE worker's socket closed: commands dropped");
                        break;
                    }
                }
            });
        if let Err(err) = spawned {
            tracing::error!(%err, "no writer thread for the WPE worker");
        }
        let reader = Reader {
            socket: Arc::clone(&socket),
            events,
            stop,
            evaluations: Arc::clone(&evaluations),
            worker: Arc::clone(&worker),
            outgoing: outgoing.clone(),
            buffers: HashMap::new(),
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
            Ok(packet) => {
                if self.outgoing.try_send(packet).is_err() {
                    tracing::debug!("the WPE worker has ended: command dropped");
                }
            }
            Err(err) => tracing::error!(%err, "a command for the WPE worker could not be written"),
        }
    }

    /// Queues `command` for the worker.
    pub(crate) fn send(&self, command: EngineCommand) {
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
        self.send(EngineCommand::Quit);
        match stopped.recv_timeout(self.stop_timeout) {
            Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                tracing::warn!("the WPE worker did not stop within {:?}", self.stop_timeout);
            }
        }
        // frees a writer stuck on a worker that stopped reading, and the reader
        let _ = rustix::net::shutdown(&*self.socket, Shutdown::Both);
        self.outgoing.close();
        self.worker.reap(REAP_GRACE);
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
    match wire::recv(socket) {
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
    outgoing: Sender<Packet>,
    /// Each buffer's dmabuf as its first frame brought it.
    buffers: HashMap<BufferId, Arc<DmaBuf>>,
}

impl Reader {
    fn run(mut self) {
        let mut stopped = false;
        loop {
            let packet = match wire::recv(self.socket.as_fd()) {
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
        self.outgoing.close();
        let _ = rustix::net::shutdown(&*self.socket, Shutdown::Both);
        self.worker.reap(REAP_GRACE);
        // the event channel closes with `self.events`
    }

    /// Takes one packet; whether it was the worker's last word ([`FromWorker::Stopped`]).
    fn take(&mut self, packet: Packet) -> Result<bool, WireError> {
        let event = match message::body::<FromWorker>(&packet)? {
            FromWorker::Event(event) => {
                if let Event::BuffersRemoved { buffers, .. } = &event {
                    for buffer in buffers {
                        self.buffers.remove(buffer);
                    }
                }
                event
            }
            FromWorker::DmaBufFrame { frame, dmabuf } => {
                let known = self.buffers.get(&frame.buffer()).cloned();
                let frame = message::dmabuf_frame(frame, dmabuf, packet.fds, known)?;
                if let crate::frames::FrameContent::DmaBuf(dmabuf) = &frame.content {
                    self.buffers.insert(frame.buffer, Arc::clone(dmabuf));
                }
                Event::Frame(frame)
            }
            FromWorker::ShmFrame { frame, regions } => Event::Frame(message::shm_frame(frame, regions, packet.blob)?),
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
        if self.events.try_send(event).is_err() {
            tracing::debug!("nobody listens to the WPE engine's events any more");
        }
        Ok(false)
    }
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
