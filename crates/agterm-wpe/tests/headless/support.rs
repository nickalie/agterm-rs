//! What the cases share: the engine started once per process, its events waited on with a deadline, and a local HTTP
//! server for pages that need a real origin (cookies, local storage, downloads).

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use agterm_wpe::{
    Event, Frame, FrameContent, Geometry, Host, HostConfig, Load, LoadEvent, MessageHandler, PageState, PolicyKind,
    PolicyToken, SchemeError, SchemeRequest, SchemeResponse, ScriptMessage, SessionId, SessionSpec, Settings,
    UserScript, ViewId, ViewSpec,
};

/// How long one wait may take: the first page of a process starts the web and network processes.
pub const WAIT: Duration = Duration::from_secs(30);

/// The scheme the cases serve pages through.
pub const SCHEME: &str = "agterm-test";

pub type Outcome = Result<(), String>;

/// Fails the case with `message` unless `condition` holds.
pub fn ensure(condition: bool, message: impl FnOnce() -> String) -> Outcome {
    if condition { Ok(()) } else { Err(message()) }
}

/// The engine and its events.
pub struct Harness {
    pub host: Host,
    events: mpsc::Receiver<Event>,
    pub shm: bool,
    /// What each page last reported showing.
    pages: RefCell<HashMap<ViewId, PageState>>,
    /// Each page's latest frame.
    frames: RefCell<HashMap<ViewId, Frame>>,
    /// Events that arrived while a script ran, kept for the next wait.
    pending: RefCell<VecDeque<Event>>,
}

impl Harness {
    pub fn start(config: HostConfig) -> Result<Self, String> {
        let shm = matches!(config.frames, agterm_wpe::FrameMode::Shm);
        let (host, queue) = Host::start(config).map_err(|err| format!("the engine did not start: {err}"))?;
        let (forward, events) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(event) = queue.recv_blocking() {
                if forward.send(event).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            host,
            events,
            shm,
            pages: RefCell::default(),
            frames: RefCell::default(),
            pending: RefCell::default(),
        })
    }

    /// The next event before `deadline`: one kept while a script ran, else a new one, noted in the caches.
    fn next(&self, deadline: Instant) -> Result<Event, mpsc::RecvTimeoutError> {
        if let Some(event) = self.pending.borrow_mut().pop_front() {
            return Ok(event);
        }
        self.receive(deadline)
    }

    fn receive(&self, deadline: Instant) -> Result<Event, mpsc::RecvTimeoutError> {
        let event = self
            .events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))?;
        match &event {
            Event::Page { view, page } => {
                self.pages.borrow_mut().insert(*view, page.clone());
            }
            Event::Frame(frame) => {
                self.frames.borrow_mut().insert(frame.view, frame.clone());
            }
            _ => {}
        }
        Ok(event)
    }

    /// The title `view` last reported.
    pub fn current_title(&self, view: ViewId) -> Option<String> {
        self.pages.borrow().get(&view).and_then(|page| page.title.clone())
    }

    /// An ephemeral store.
    pub fn ephemeral(&self) -> SessionId {
        self.host.create_session(SessionSpec::Ephemeral)
    }

    /// A page on `session` at 320x240, scale 1, with `settings` and nothing else.
    pub fn view(&self, session: SessionId, settings: Settings) -> ViewId {
        self.view_with(ViewSpec {
            session,
            settings,
            scripts: Vec::new(),
            message_handlers: Vec::new(),
            zoom: 1.0,
            background: None,
            geometry: Geometry {
                width: 320,
                height: 240,
                scale: 1.0,
            },
        })
    }

    pub fn view_with(&self, spec: ViewSpec) -> ViewId {
        self.host.create_view(spec)
    }

    /// A page with one message handler and user scripts.
    pub fn bridged_view(
        &self,
        session: SessionId,
        settings: Settings,
        handler: MessageHandler,
        scripts: Vec<UserScript>,
    ) -> ViewId {
        self.view_with(ViewSpec {
            session,
            settings,
            scripts,
            message_handlers: vec![handler],
            zoom: 1.0,
            background: None,
            geometry: Geometry {
                width: 320,
                height: 240,
                scale: 1.0,
            },
        })
    }

    /// Waits for the first event `pick` takes; every other one gets the default treatment ([`Harness::settle`]).
    pub fn wait_for<T>(&self, what: &str, mut pick: impl FnMut(&Event) -> Option<T>) -> Result<T, String> {
        let deadline = Instant::now() + WAIT;
        loop {
            let event = self
                .next(deadline)
                .map_err(|_| format!("timed out waiting for {what}"))?;
            if let Some(found) = pick(&event) {
                return Ok(found);
            }
            self.settle(&event);
        }
    }

    /// Lets events run for `period`, each with the default treatment, failing on one `refuse` matches.
    pub fn idle(&self, period: Duration, mut refuse: impl FnMut(&Event) -> Option<String>) -> Outcome {
        let deadline = Instant::now() + period;
        loop {
            match self.next(deadline) {
                Ok(event) => {
                    if let Some(message) = refuse(&event) {
                        return Err(message);
                    }
                    self.settle(&event);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => return Ok(()),
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err("the engine's events ended".to_owned()),
            }
        }
    }

    /// The default treatment: frames are presented at once, policy questions allowed, scheme reads and script
    /// messages nobody waits for refused.
    pub fn settle(&self, event: &Event) {
        match event {
            Event::Frame(frame) => self.host.frame_presented(frame.view, frame.buffer),
            Event::Policy(request) => self.host.decide_policy(request.token, true),
            Event::SchemeRequest(request) => self.host.finish_scheme_request(
                request.token,
                SchemeResponse::Error {
                    kind: SchemeError::NotFound,
                    message: format!("not served: {}", request.uri),
                },
            ),
            Event::ScriptMessage(message) => self
                .host
                .reply_script_message(message.token, Err("unexpected message".to_owned())),
            _ => {}
        }
    }

    /// Loads `load` into `view` and waits for it to finish.
    pub fn load(&self, view: ViewId, load: Load) -> Outcome {
        self.host.load(view, load);
        self.finished(view)
    }

    /// Waits for `view`'s load to finish; a failed load fails the case.
    pub fn finished(&self, view: ViewId) -> Outcome {
        self.wait_for("the load to finish", |event| match event {
            Event::Load {
                view: id,
                event: LoadEvent::Finished,
            } if *id == view => Some(Ok(())),
            Event::LoadFailed { view: id, uri, error } if *id == view => {
                Some(Err(format!("the load of {uri} failed: {error:?}")))
            }
            _ => None,
        })?
    }

    /// Waits until `view`'s title is `title` (at once when it already is).
    pub fn title(&self, view: ViewId, title: &str) -> Outcome {
        if self.current_title(view).as_deref() == Some(title) {
            return Ok(());
        }
        self.wait_for(&format!("the title {title:?}"), |event| match event {
            Event::Page { view: id, page } if *id == view && page.title.as_deref() == Some(title) => Some(()),
            _ => None,
        })
        .map_err(|err| format!("{err} (the title is {:?})", self.current_title(view)))
    }

    /// Waits for a read of the cases' scheme that `matches` picks.
    pub fn scheme_request(&self, matches: impl Fn(&SchemeRequest) -> bool) -> Result<SchemeRequest, String> {
        self.wait_for("a scheme read", |event| match event {
            Event::SchemeRequest(request) if matches(request) => Some(request.clone()),
            _ => None,
        })
    }

    /// Waits for a script message from `view`.
    pub fn script_message(&self, view: ViewId) -> Result<ScriptMessage, String> {
        self.wait_for("a script message", |event| match event {
            Event::ScriptMessage(message) if message.view == view => Some(message.clone()),
            _ => None,
        })
    }

    /// Waits for a policy question from `view` that `matches` picks.
    pub fn policy(
        &self,
        view: ViewId,
        matches: impl Fn(&PolicyKind) -> bool,
    ) -> Result<(PolicyToken, PolicyKind), String> {
        self.wait_for("a policy question", |event| match event {
            Event::Policy(request) if request.view == view && matches(&request.kind) => {
                Some((request.token, request.kind.clone()))
            }
            _ => None,
        })
    }

    /// Waits for a frame of `view` whose pixel at (`x`, `y`) is `color`, presenting every frame of it on the way.
    pub fn pixel_frame(&self, view: ViewId, x: u32, y: u32, color: (u8, u8, u8)) -> Result<Frame, String> {
        let latest = self.frames.borrow().get(&view).cloned();
        if let Some(frame) = latest.filter(|frame| Self::pixel(frame, x, y).as_ref() == Ok(&color)) {
            return Ok(frame);
        }
        let mut seen = None;
        self.wait_for(&format!("a frame showing {color:02x?}"), |event| {
            let Event::Frame(frame) = event else { return None };
            if frame.view != view {
                return None;
            }
            let pixel = Self::pixel(frame, x, y);
            self.host.frame_presented(frame.view, frame.buffer);
            if pixel.as_ref() == Ok(&color) {
                return Some(frame.clone());
            }
            seen = Some(pixel);
            None
        })
        .map_err(|err| {
            let latest = self.frames.borrow().get(&view).map(|frame| {
                let pixel = Self::pixel(frame, x, y);
                format!(
                    "{}x{} damage {:?} reads {pixel:02x?}",
                    frame.width, frame.height, frame.damage
                )
            });
            format!("{err} (last read: {seen:02x?}; the latest frame: {latest:?})")
        })
    }

    /// Runs `script` in `view` and waits for its JSON value. Frames go on being presented meanwhile; every other
    /// event is kept for the next wait.
    pub fn evaluate(&self, view: ViewId, script: &str, world: Option<&str>) -> Result<Option<String>, String> {
        let result = self.host.evaluate(view, script.to_owned(), world.map(str::to_owned));
        let deadline = Instant::now() + WAIT;
        loop {
            if let Ok(value) = result.try_recv() {
                return value;
            }
            if Instant::now() > deadline {
                return Err(format!("timed out evaluating {script:?}"));
            }
            match self.receive(Instant::now() + Duration::from_millis(10)) {
                Ok(Event::Frame(frame)) => self.host.frame_presented(frame.view, frame.buffer),
                Ok(event) => self.pending.borrow_mut().push_back(event),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err("the engine's events ended".to_owned()),
            }
        }
    }

    /// Waits until `view`'s title satisfies `matches` (at once when it already does); returns it.
    pub fn title_where(&self, view: ViewId, matches: impl Fn(&str) -> bool) -> Result<String, String> {
        if let Some(title) = self.current_title(view).filter(|title| matches(title)) {
            return Ok(title);
        }
        self.wait_for("a title", |event| match event {
            Event::Page { view: id, page } if *id == view => page.title.clone().filter(|title| matches(title)),
            _ => None,
        })
        .map_err(|err| format!("{err} (the title is {:?})", self.current_title(view)))
    }

    /// The color at (`x`, `y`) of `frame` as (red, green, blue), from SHM pixels or a linear dmabuf mapped read-only.
    pub fn pixel(frame: &agterm_wpe::Frame, x: u32, y: u32) -> Result<(u8, u8, u8), String> {
        match &frame.content {
            FrameContent::Shm(regions) => {
                let region = regions
                    .iter()
                    .find(|region| {
                        let rect = region.rect;
                        (rect.x..rect.x + rect.width).contains(&x) && (rect.y..rect.y + rect.height).contains(&y)
                    })
                    .ok_or("the pixel is not in the frame's damage")?;
                let at = (((y - region.rect.y) * region.rect.width + (x - region.rect.x)) * 4) as usize;
                let bgra = region.pixels.get(at..at + 4).ok_or("short SHM region")?;
                Ok((bgra[2], bgra[1], bgra[0]))
            }
            FrameContent::DmaBuf(buffer) => crate::dmabuf::pixel(buffer, x, y),
        }
    }

    pub fn shutdown(self) {
        self.host.shutdown();
    }
}

/// An HTML page as a `data:` URL.
pub fn data_url(html: &str) -> String {
    let mut url = String::from("data:text/html,");
    for byte in html.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~ =:;/'(),!".contains(&byte) {
            url.push(char::from(byte));
        } else {
            const HEX: &[u8; 16] = b"0123456789ABCDEF";
            url.push('%');
            url.push(char::from(HEX[usize::from(byte >> 4)]));
            url.push(char::from(HEX[usize::from(byte & 0xf)]));
        }
    }
    url
}

/// A local HTTP server for the cases' pages: `/set` sets a cookie and local storage, `/get` shows both in the
/// title, `/download` clicks a download link to `/file.bin`.
pub struct Server {
    pub origin: String,
}

impl Server {
    pub fn start() -> Result<Self, String> {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|err| format!("no local port: {err}"))?;
        let port = listener.local_addr().map_err(|err| err.to_string())?.port();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let _ = serve(stream);
            }
        });
        Ok(Self {
            origin: format!("http://127.0.0.1:{port}"),
        })
    }
}

fn serve(stream: TcpStream) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request = String::new();
    reader.read_line(&mut request)?;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 || header.trim().is_empty() {
            break;
        }
    }
    let path = request.split_whitespace().nth(1).unwrap_or("/");
    let (headers, body): (&str, &str) = match path {
        "/set" => (
            "Content-Type: text/html\r\nSet-Cookie: saved=1; Max-Age=3600; Path=/\r\n",
            "<script>localStorage.setItem('k', 'v'); document.title = 'set';</script>",
        ),
        "/get" => (
            "Content-Type: text/html\r\n",
            "<script>document.title = 'cookie=' + document.cookie + ';storage=' + localStorage.getItem('k');</script>",
        ),
        "/download" => (
            "Content-Type: text/html\r\n",
            "<a id=d download href=/file.bin>file</a><script>document.getElementById('d').click();</script>",
        ),
        "/file.bin" => ("Content-Type: application/octet-stream\r\n", "0123456789"),
        _ => ("Content-Type: text/plain\r\n", "not here"),
    };
    let mut stream = stream;
    write!(
        stream,
        "HTTP/1.1 200 OK\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}
