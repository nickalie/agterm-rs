//! What the client library's tests run against, on every OS: a scripted peer (a fake server on a real endpoint of a
//! private namespace, its answers given by the test) and a real server run in the test process (`server::run`).

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use agterm_session::client::{Api, Client};
use agterm_session::endpoint::{self, Endpoint, Stream};
use agterm_session::protocol::{
    ApiCall, ApiError, ApiReply, ApiRequest, ApiResponse, AttachClientMessage, AttachServerMessage, Control,
    FramingError, PROTOCOL_VERSION, Role, read_frame, welcome, write_control, write_message,
};
use agterm_session::server::lifecycle::{Graces, StopReason};
use agterm_session::server::{self, ServerConfig};

use crate::support::{PATIENCE, TestResult};

/// A private namespace directory under a temporary root: short on Unix, where a socket path holds about a hundred
/// bytes.
fn namespace() -> TestResult<(tempfile::TempDir, PathBuf)> {
    #[cfg(unix)]
    let root = tempfile::Builder::new().prefix("agcl-").tempdir_in("/tmp")?;
    #[cfg(windows)]
    let root = tempfile::tempdir()?;
    let directory = root.path().join("live");
    Ok((root, directory))
}

/// A fake server: it welcomes every connection and hands it, with its role, to the test's `serve` on a thread of its
/// own.
pub struct Peer {
    _root: tempfile::TempDir,
    directory: PathBuf,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

impl Peer {
    pub fn start(serve: impl Fn(Stream, Role) + Send + Sync + 'static) -> TestResult<Self> {
        let (root, directory) = namespace()?;
        let listener = Endpoint::new(&directory).bind()?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let serve: Arc<Serve> = Arc::new(serve);
        let accept = thread::spawn(move || {
            while let Ok(stream) = listener.accept() {
                if stopping.load(Ordering::SeqCst) {
                    return;
                }
                let serve = Arc::clone(&serve);
                thread::spawn(move || serve_connection(stream, &*serve));
            }
        });
        Ok(Self {
            _root: root,
            directory,
            stop,
            accept: Some(accept),
        })
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // the accept loop sees the stop with its next connection
        let _wake = Endpoint::new(&self.directory).connect_timeout(Duration::from_secs(1));
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}

/// What a peer does with a welcomed connection.
type Serve = dyn Fn(Stream, Role) + Send + Sync;

fn serve_connection(stream: Stream, serve: &Serve) {
    if let Some((stream, role)) = welcomed(stream) {
        serve(stream, role);
    }
}

/// The hello read and welcomed; the connection and its role.
fn welcomed(mut stream: Stream) -> Option<(Stream, Role)> {
    let frame = read_frame(&mut stream).ok()?;
    let Control::Hello(hello) = frame.control().ok()? else {
        return None;
    };
    let welcome = welcome(&hello, frame.protocol, "peer", &[]);
    write_control(&mut stream, &Control::Welcome(welcome)).ok()?;
    Some((stream, hello.role))
}

/// How a scripted api peer answers a call.
pub enum Answer {
    Reply(Result<ApiResponse, ApiError>),
    /// A reply carrying another call's id.
    WrongId(ApiResponse),
    /// The connection closed without a reply.
    Close,
    /// No reply: the peer reads on until the client goes.
    Silent,
    /// The reply written a byte at a time, `pause` after each.
    Trickle {
        response: ApiResponse,
        pause: Duration,
    },
}

/// The requests an api peer was sent, in order.
pub type Calls = Arc<Mutex<Vec<ApiRequest>>>;

pub fn calls(log: &Calls) -> Vec<ApiRequest> {
    log.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// A peer answering every api call as `answer` says, logging each request.
pub fn api_peer(answer: impl Fn(&ApiRequest) -> Answer + Send + Sync + 'static) -> TestResult<(Peer, Calls)> {
    let log = Calls::default();
    let sink = Arc::clone(&log);
    let peer = Peer::start(move |mut stream, role| {
        if role != Role::Api {
            return;
        }
        while let Ok(frame) = read_frame(&mut stream) {
            let Ok(call) = frame.message::<ApiCall>(PROTOCOL_VERSION) else {
                return;
            };
            sink.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(call.request.clone());
            let (id, result) = match answer(&call.request) {
                Answer::Reply(result) => (call.id, result),
                Answer::WrongId(response) => (call.id + 100, Ok(response)),
                Answer::Close => return,
                Answer::Silent => continue,
                Answer::Trickle { response, pause } => {
                    if trickle(&mut stream, call.id, response, pause).is_err() {
                        return;
                    }
                    continue;
                }
            };
            if write_message(&mut stream, &ApiReply { id, result }).is_err() {
                return;
            }
        }
    })?;
    Ok((peer, log))
}

/// The reply `response` to the call `id`, written a byte at a time with `pause` after each.
fn trickle(stream: &mut Stream, id: u64, response: ApiResponse, pause: Duration) -> Result<(), FramingError> {
    let mut frame = Vec::new();
    write_message(
        &mut frame,
        &ApiReply {
            id,
            result: Ok(response),
        },
    )?;
    for byte in frame {
        stream.write_all(&[byte])?;
        thread::sleep(pause);
    }
    Ok(())
}

/// One step of a scripted attach peer.
pub enum Step {
    Send(AttachServerMessage),
    /// Bytes written as they are, frames or not.
    Raw(Vec<u8>),
    /// Nothing read or written for this long, as a server that stopped.
    Hold(Duration),
    /// Wait for the client's next message (it is forwarded to the test as every one is).
    Receive,
}

/// A peer serving one attach connection: it forwards the attach request and every message the client sends to the
/// test, plays `steps` (stopping early once the client detached or went), and closes the connection in order.
pub fn attach_peer(steps: Vec<Step>) -> TestResult<(Peer, Receiver<AttachClientMessage>)> {
    let (sender, received) = mpsc::channel();
    let script = Mutex::new(Some((steps, sender)));
    let peer = Peer::start(move |stream, role| {
        let Some((steps, sender)) = script.lock().unwrap_or_else(PoisonError::into_inner).take() else {
            return;
        };
        if role == Role::Attach {
            play(&stream, steps, &sender);
            endpoint::close(&stream);
        }
    })?;
    Ok((peer, received))
}

fn play(stream: &Stream, steps: Vec<Step>, sender: &Sender<AttachClientMessage>) {
    let receive = || -> Result<bool, FramingError> {
        let message = read_frame(&mut &*stream)?.message::<AttachClientMessage>(PROTOCOL_VERSION)?;
        let detached = message == AttachClientMessage::Detach;
        let _ = sender.send(message);
        Ok(!detached)
    };
    if !matches!(receive(), Ok(true)) {
        return;
    }
    for step in steps {
        let done = match step {
            Step::Send(message) => write_message(&mut &*stream, &message).is_err(),
            Step::Raw(bytes) => (&*stream).write_all(&bytes).is_err(),
            Step::Hold(time) => {
                thread::sleep(time);
                false
            }
            Step::Receive => !matches!(receive(), Ok(true)),
        };
        if done {
            return;
        }
    }
}

/// The test's next message from the client of an attach peer.
pub fn next_message(received: &Receiver<AttachClientMessage>) -> TestResult<AttachClientMessage> {
    Ok(received.recv_timeout(PATIENCE)?)
}

/// A real server of a private namespace, run on a thread of the test until it stops by itself.
pub struct Server {
    _root: tempfile::TempDir,
    directory: PathBuf,
    thread: Option<JoinHandle<std::io::Result<StopReason>>>,
    /// An api connection that holds the server until the test lets it stop ([`Server::stopped`]): without it the
    /// readiness probe would be the server's first client, whose leaving starts the short idle grace, and a test
    /// thread descheduled for longer would find no server.
    keepalive: Option<Api>,
}

impl Server {
    /// A server that stops 100 ms after its last session ended and its last client left, once the test lets it
    /// ([`Server::stopped`]); returned once it answers.
    pub fn start() -> TestResult<Self> {
        let (root, directory) = namespace()?;
        let config = ServerConfig {
            build: "client-test".to_owned(),
            graces: Graces {
                startup: PATIENCE,
                idle: Duration::from_millis(100),
            },
            ..ServerConfig::default()
        };
        let serving = directory.clone();
        let thread = thread::spawn(move || server::run(&serving, &config));
        let client = Client::new(&directory).with_timeout(PATIENCE);
        let deadline = Instant::now() + PATIENCE;
        let keepalive = loop {
            if let Ok(Some(api)) = client.api() {
                break api;
            }
            assert!(Instant::now() < deadline, "the server never answered");
            thread::sleep(Duration::from_millis(20));
        };
        Ok(Self {
            _root: root,
            directory,
            thread: Some(thread),
            keepalive: Some(keepalive),
        })
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Let the server stop by itself and wait for it; why it did.
    pub fn stopped(mut self) -> TestResult<StopReason> {
        drop(self.keepalive.take());
        let thread = self.thread.take().ok_or("no server thread")?;
        let deadline = Instant::now() + PATIENCE;
        while !thread.is_finished() {
            assert!(Instant::now() < deadline, "the server never stopped");
            thread::sleep(Duration::from_millis(20));
        }
        Ok(thread.join().map_err(|_| "the server panicked")??)
    }
}
