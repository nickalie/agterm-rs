//! [`Host`]: the handle the consumer drives the engine thread with. Every call is a command posted to the engine and
//! returns at once; what follows comes back as [`Event`]s.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use async_channel::{Receiver, Sender};

use crate::config::{ConfigError, HostConfig};
use crate::engine;
use crate::event::{Event, PolicyToken, ReplyToken, SchemeResponse, SchemeToken};
use crate::frames::BufferId;
use crate::input::Input;
use crate::spec::{Geometry, Rgba, SessionId, SessionSpec, UserScript, ViewId, ViewSpec};

/// What a page loads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Load {
    Uri(String),
    /// A document from text, resolved against `base_uri` (`about:blank` when `None`).
    Html {
        html: String,
        base_uri: Option<String>,
    },
}

/// The result of [`Host::evaluate`]: the script's value as JSON (`None` for `undefined` or a value JSON cannot
/// express), or the exception's message.
pub type Evaluation = Result<Option<String>, String>;

/// A command for the engine thread.
#[derive(Debug)]
pub(crate) enum Command {
    CreateSession(SessionId, SessionSpec),
    DropSession(SessionId),
    ClearSession(SessionId),
    CreateView(ViewId, Box<ViewSpec>),
    CloseView(ViewId),
    View(ViewId, ViewCommand),
    Policy(PolicyToken, bool),
    Reply(ReplyToken, Result<String, String>),
    Scheme(SchemeToken, SchemeResponse),
    Quit,
}

/// A command for one page.
#[derive(Debug)]
pub(crate) enum ViewCommand {
    Load(Load),
    Reload,
    Stop,
    GoBack,
    GoForward,
    Zoom(f64),
    Scripts(Vec<UserScript>),
    Background(Option<Rgba>),
    Geometry(Geometry),
    Visible(bool),
    Focused(bool),
    Input(Input),
    Presented(BufferId),
    FallBackToShm,
    Evaluate {
        script: String,
        world: Option<String>,
        reply: Sender<Evaluation>,
    },
}

/// Why the engine did not start.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("the WPE engine runs once per process: WebKit binds its main run loop to the first thread that uses it")]
    AlreadyStarted,
    #[error("the WPE engine thread did not start: {0}")]
    Thread(#[source] std::io::Error),
    #[error("the WPE display did not connect: {0}")]
    Display(String),
    #[error("the WPE engine thread ended while starting")]
    Ended,
}

static STARTED: AtomicBool = AtomicBool::new(false);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// How long a shutdown waits for the engine to close its pages.
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// Ends the process with `code` at once, after flushing stdout and stderr, without running its exit handlers. A
/// process that started the engine ends through this, after [`Host::shutdown`]: `WebKit` registers static
/// destructors that unreference its objects and assert they run on its main thread, which is the engine thread (it
/// never ends), so `exit` (and returning from `main`) runs them on the calling thread, where they abort.
pub fn exit_process(code: i32) -> ! {
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    // SAFETY: `_exit` ends the process without touching any state of it; nothing after it runs.
    unsafe { libc::_exit(code) }
}

/// The engine: WPE `WebKit` on a thread of its own, with its own `GMainContext`, rendering pages offscreen.
///
/// One per process. Dropping it shuts the engine down, as [`Host::shutdown`] does; the thread itself lives on, idle,
/// until the process ends, since it is `WebKit`'s main thread, which must never end (its thread-local teardown waits
/// forever on the run loop it tears down). The process then ends through [`exit_process`].
#[derive(Debug)]
pub struct Host {
    commands: Sender<Command>,
    /// Told once the engine has closed everything; `None` after a shutdown.
    stopped: Option<std::sync::mpsc::Receiver<()>>,
}

impl Host {
    /// Starts the engine thread; events arrive on the returned receiver until the engine ends.
    ///
    /// # Errors
    ///
    /// An invalid `config`, a second start in this process, a thread that cannot be spawned, or a display that does
    /// not connect (a render node `WebKit` cannot use).
    pub fn start(config: HostConfig) -> Result<(Self, Receiver<Event>), StartError> {
        config.validate()?;
        if STARTED.swap(true, Ordering::SeqCst) {
            return Err(StartError::AlreadyStarted);
        }
        let (commands, command_queue) = async_channel::unbounded();
        let (events, event_queue) = async_channel::unbounded();
        let (ready, started) = std::sync::mpsc::sync_channel(1);
        let (stop, stopped) = std::sync::mpsc::sync_channel(1);
        // never joined: the engine thread outlives the host (type docs)
        std::thread::Builder::new()
            .name("wpe-engine".to_owned())
            .spawn(move || engine::run(&config, &command_queue, events, &ready, stop))
            .map_err(StartError::Thread)?;
        let host = Self {
            commands,
            stopped: Some(stopped),
        };
        match started.recv() {
            Ok(Ok(())) => Ok((host, event_queue)),
            Ok(Err(message)) => Err(StartError::Display(message)),
            Err(_) => Err(StartError::Ended),
        }
    }

    fn send(&self, command: Command) {
        if self.commands.try_send(command).is_err() {
            tracing::debug!("the WPE engine has ended: command dropped");
        }
    }

    fn view(&self, view: ViewId, command: ViewCommand) {
        self.send(Command::View(view, command));
    }

    /// Creates a browser store.
    #[must_use]
    pub fn create_session(&self, spec: SessionSpec) -> SessionId {
        let id = SessionId(next_id());
        self.send(Command::CreateSession(id, spec));
        id
    }

    /// Drops the host's hold on a store; pages built on it keep it until they close.
    pub fn drop_session(&self, session: SessionId) {
        self.send(Command::DropSession(session));
    }

    /// Removes everything a store holds; [`Event::SessionCleared`] reports the outcome.
    pub fn clear_session(&self, session: SessionId) {
        self.send(Command::ClearSession(session));
    }

    /// Creates a page, mapped and visible, loading nothing yet.
    #[must_use]
    pub fn create_view(&self, spec: ViewSpec) -> ViewId {
        let id = ViewId(next_id());
        self.send(Command::CreateView(id, Box::new(spec)));
        id
    }

    /// Closes a page: its load stops, the questions it left open are answered, its buffers go.
    pub fn close_view(&self, view: ViewId) {
        self.send(Command::CloseView(view));
    }

    pub fn load(&self, view: ViewId, load: Load) {
        self.view(view, ViewCommand::Load(load));
    }

    pub fn reload(&self, view: ViewId) {
        self.view(view, ViewCommand::Reload);
    }

    pub fn stop(&self, view: ViewId) {
        self.view(view, ViewCommand::Stop);
    }

    pub fn go_back(&self, view: ViewId) {
        self.view(view, ViewCommand::GoBack);
    }

    pub fn go_forward(&self, view: ViewId) {
        self.view(view, ViewCommand::GoForward);
    }

    pub fn set_zoom(&self, view: ViewId, zoom: f64) {
        self.view(view, ViewCommand::Zoom(zoom));
    }

    /// Replaces the page's user scripts; they run from its next load on.
    pub fn set_scripts(&self, view: ViewId, scripts: Vec<UserScript>) {
        self.view(view, ViewCommand::Scripts(scripts));
    }

    pub fn set_background(&self, view: ViewId, background: Option<Rgba>) {
        self.view(view, ViewCommand::Background(background));
    }

    pub fn set_geometry(&self, view: ViewId, geometry: Geometry) {
        self.view(view, ViewCommand::Geometry(geometry));
    }

    /// A hidden page renders nothing.
    pub fn set_visible(&self, view: ViewId, visible: bool) {
        self.view(view, ViewCommand::Visible(visible));
    }

    /// Gives the page the keyboard focus, or takes it away.
    pub fn set_focused(&self, view: ViewId, focused: bool) {
        self.view(view, ViewCommand::Focused(focused));
    }

    pub fn input(&self, view: ViewId, input: Input) {
        self.view(view, ViewCommand::Input(input));
    }

    /// The frame in `buffer` is on screen: the page may render the next one, and the buffer shown before it goes
    /// back (crate `frames`).
    pub fn frame_presented(&self, view: ViewId, buffer: BufferId) {
        self.view(view, ViewCommand::Presented(buffer));
    }

    /// Switches a running page to SHM frames, for good: the consumer could not import its dmabufs (a format outside
    /// what it offered, a failed import). The page's web process is replaced and the page reloads what it shows
    /// (its history stays; the document's script state does not, and a page loaded from text should be loaded
    /// again); the frames that follow arrive as [`crate::FrameContent::Shm`], and the dmabuf shown last is released
    /// once an SHM frame replaces it. The engine's own mode cannot change after the start (`WebKit` hands its web
    /// processes the render node once per process, and a web process picks its buffers when it builds a page's
    /// surface), so this is the per-page fall-back. A page in an SHM engine is left alone.
    pub fn fall_back_to_shm(&self, view: ViewId) {
        self.view(view, ViewCommand::FallBackToShm);
    }

    /// Runs `script` in the page's main frame, in `world` (`None`: the page's own); the result arrives on the
    /// returned channel.
    #[must_use]
    pub fn evaluate(&self, view: ViewId, script: String, world: Option<String>) -> Receiver<Evaluation> {
        let (reply, result) = async_channel::bounded(1);
        self.view(view, ViewCommand::Evaluate { script, world, reply });
        result
    }

    /// Answers a [`crate::PolicyRequest`]: go on with it, or ignore it.
    pub fn decide_policy(&self, token: PolicyToken, allow: bool) {
        self.send(Command::Policy(token, allow));
    }

    /// Answers a [`crate::ScriptMessage`]: the page's promise resolves with the JSON value, or rejects with the
    /// message.
    pub fn reply_script_message(&self, token: ReplyToken, reply: Result<String, String>) {
        self.send(Command::Reply(token, reply));
    }

    /// Answers a [`crate::SchemeRequest`].
    pub fn finish_scheme_request(&self, token: SchemeToken, response: SchemeResponse) {
        self.send(Command::Scheme(token, response));
    }

    /// Closes every page and stops the engine, waiting (a bounded time) until it has. The engine cannot start again in
    /// this process.
    pub fn shutdown(mut self) {
        self.stop_engine();
    }

    fn stop_engine(&mut self) {
        let Some(stopped) = self.stopped.take() else { return };
        self.send(Command::Quit);
        match stopped.recv_timeout(STOP_TIMEOUT) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {}
            Err(RecvTimeoutError::Timeout) => {
                tracing::warn!("the WPE engine did not stop within {STOP_TIMEOUT:?}; leaving it");
            }
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.stop_engine();
    }
}
