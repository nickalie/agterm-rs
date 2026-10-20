//! Helpers for the pane tests on real PTYs, on every OS: the programs per OS, a session's spec with the test's
//! environment without the live agterm's `AGTERM_*` and `ZMX_*` variables, and an attached client reading its events.

use std::path::Path;
use std::time::{Duration, Instant};

use agterm_ghostty_vt::Terminal;
use agterm_session::protocol::{ExitStatus, GridSize, LeadRole, Managed, SessionSpec};
use agterm_session::server::clients::{ClientEvent, RecvError, Subscription};
use agterm_session::server::pane::{Attached, Pane, PaneConfig};

pub type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// How long a test waits for output (Windows PowerShell can take seconds to start on a CI runner).
pub const PATIENCE: Duration = Duration::from_secs(60);

pub const SIZE: GridSize = GridSize { cols: 120, rows: 30 };

/// The test's environment without the live agterm's variables.
pub fn environment() -> Vec<(String, String)> {
    std::env::vars()
        .filter(|(key, _)| {
            let key = key.to_ascii_uppercase();
            !key.starts_with("AGTERM_") && !key.starts_with("ZMX_")
        })
        .collect()
}

pub fn spec(argv: &[&str]) -> SessionSpec {
    SessionSpec {
        argv: argv.iter().map(|arg| (*arg).to_owned()).collect(),
        env: environment(),
        cwd: None,
        tracked_env: Vec::new(),
    }
}

pub fn config(name: &str, namespace: &Path, spec: SessionSpec) -> PaneConfig {
    PaneConfig::new(name, namespace, spec, SIZE)
}

/// A shell that runs each line it reads: `cmd /q /k` (no prompt noise), or `sh` evaluating its input lines (no
/// prompt at all).
pub fn shell() -> SessionSpec {
    if cfg!(windows) {
        spec(&["cmd.exe", "/d", "/q", "/k"])
    } else {
        spec(&["/bin/sh", "-c", "while read line; do eval \"$line\"; done"])
    }
}

/// `script` run by the OS's command shell.
pub fn script(script: &str) -> SessionSpec {
    if cfg!(windows) {
        spec(&["cmd.exe", "/d", "/c", script])
    } else {
        spec(&["/bin/sh", "-c", script])
    }
}

/// The line that makes [`shell`] print `got:<word>` (the echoed typed line, `got^:` or `got"":`, does not match; cmd's
/// console title, which names the command parsed, does, so the needle is looked for in [`shown`] text).
pub fn tagged(word: &str) -> String {
    if cfg!(windows) {
        format!("echo got^:{word}")
    } else {
        format!("echo got\"\":{word}")
    }
}

/// The line that makes [`shell`] print `SIZE=<rows>x<cols>` as the child sees its terminal.
pub fn size_line() -> String {
    if cfg!(windows) {
        "powershell.exe -NoLogo -NoProfile -NonInteractive -Command \"'SIZE=' + [Console]::WindowHeight + 'x' + \
         [Console]::WindowWidth\""
            .to_owned()
    } else {
        "echo \"SIZE=$(stty size | tr ' ' x)\"".to_owned()
    }
}

pub fn type_line(pane: &Pane, line: &str) -> TestResult {
    let enter = if cfg!(windows) { "\r" } else { "\n" };
    pane.write(format!("{line}{enter}").as_bytes())?;
    Ok(())
}

/// End [`shell`]: `exit` for cmd, the end of input for the read loop.
pub fn quit(pane: &Pane) -> TestResult {
    if cfg!(windows) {
        type_line(pane, "exit")
    } else {
        Ok(pane.write(b"\x04")?)
    }
}

/// An attached client reading its events, with the terminal they build, as a client's own would.
pub struct Client {
    pub subscription: Subscription,
    /// The output offset the next output must start at.
    pub next: u64,
    /// The output received, snapshots left out.
    pub output: Vec<u8>,
    /// The events received but the reply-ownership reports, which are in `owners`.
    pub events: Vec<ClientEvent>,
    /// The reply-ownership reports received: owner, generation.
    pub owners: Vec<(bool, u32)>,
    pub exit: Option<ExitStatus>,
    /// The epoch of the last snapshot received.
    pub epoch: u64,
    /// Fed every snapshot and output, from the first snapshot on, at its size.
    pub terminal: Option<Terminal>,
}

impl Client {
    pub fn new(attached: Attached) -> Self {
        Self {
            subscription: attached.subscription,
            next: attached.offset,
            output: Vec::new(),
            events: Vec::new(),
            owners: Vec::new(),
            exit: None,
            epoch: 0,
            terminal: None,
        }
    }

    /// Another client, attached now, unmanaged, at the session's first size.
    pub fn attach(pane: &Pane) -> TestResult<Self> {
        Self::join(pane, SIZE, None)
    }

    /// Another client, attached now with the grid `size`, under explicit leadership when `managed` is given.
    pub fn join(pane: &Pane, size: GridSize, managed: Option<&Managed>) -> TestResult<Self> {
        Ok(Self::new(pane.try_attach(size, managed)?))
    }

    /// The client's id at its session.
    pub fn id(&self) -> u64 {
        self.subscription.id()
    }

    /// Receive every event queued now.
    pub fn drain(&mut self) {
        while self.receive(Duration::ZERO) {}
    }

    /// The roles it was told so far, in order.
    pub fn roles(&self) -> Vec<(LeadRole, u32)> {
        self.events
            .iter()
            .filter_map(|event| match event {
                ClientEvent::Role { role, generation } => Some((*role, *generation)),
                _ => None,
            })
            .collect()
    }

    /// The output as a terminal shows it: without its control strings.
    pub fn text(&self) -> String {
        shown(&self.output)
    }

    /// A snapshot opening `epoch`: the client's terminal (made at `size` by the first) takes it, and the output resumes
    /// at `offset`.
    fn take_snapshot(&mut self, epoch: u64, offset: u64, size: GridSize, data: &[u8]) {
        assert!(epoch > self.epoch, "epoch {epoch} after {}", self.epoch);
        self.epoch = epoch;
        self.next = offset;
        if self.terminal.is_none() {
            self.terminal = Terminal::new(size.cols, size.rows).ok();
            assert!(self.terminal.is_some(), "no client terminal");
        }
        self.resize_terminal(size);
        if let Some(terminal) = &mut self.terminal {
            terminal.vt_write(data);
        }
    }

    /// The client's terminal, once it has one, takes the grid `size`.
    fn resize_terminal(&mut self, size: GridSize) {
        if let Some(terminal) = &mut self.terminal
            && (terminal.cols(), terminal.rows()) != (size.cols, size.rows)
        {
            let resized = terminal.resize(size.cols, size.rows, 0, 0);
            assert!(resized.is_ok(), "the client terminal took no {size:?}");
        }
    }

    /// Receive one event within `timeout`; false once the client is closed or none came.
    pub fn receive(&mut self, timeout: Duration) -> bool {
        match self.subscription.recv_timeout(timeout) {
            Ok(event) => {
                match &event {
                    ClientEvent::Snapshot {
                        epoch,
                        offset,
                        size,
                        data,
                    } => self.take_snapshot(*epoch, *offset, *size, data),
                    ClientEvent::Output { offset, data } => {
                        assert_eq!(*offset, self.next, "the output is not contiguous");
                        self.next += data.len() as u64;
                        self.output.extend_from_slice(data);
                        if let Some(terminal) = &mut self.terminal {
                            terminal.vt_write(data);
                        }
                    }
                    ClientEvent::Resized { size, .. } => self.resize_terminal(*size),
                    ClientEvent::Exit(status) => self.exit = Some(status.clone()),
                    ClientEvent::ReplyOwner { owner, generation } => {
                        // kept aside: the event received is the one after it
                        self.owners.push((*owner, *generation));
                        return self.receive(timeout);
                    }
                    ClientEvent::Role { .. } | ClientEvent::Delegated { .. } | ClientEvent::Overflowed { .. } => {}
                }
                self.events.push(event);
                true
            }
            Err(RecvError::Timeout | RecvError::Closed) => false,
        }
    }

    /// Wait for `needle` in the output.
    pub fn expect(&mut self, needle: &str) {
        let deadline = Instant::now() + PATIENCE;
        while !self.text().contains(needle) {
            assert!(
                Instant::now() < deadline && self.exit.is_none(),
                "no {needle:?} in the output: {:?}",
                self.text()
            );
            self.receive(Duration::from_millis(100));
        }
    }

    /// Wait for the whole line `prefix` starts in the output (up to its line feed), so what follows the prefix on
    /// its line is not read half-delivered.
    pub fn expect_line(&mut self, prefix: &str) {
        let deadline = Instant::now() + PATIENCE;
        while !self
            .text()
            .split_once(prefix)
            .is_some_and(|(_, rest)| rest.contains('\n'))
        {
            assert!(
                Instant::now() < deadline && self.exit.is_none(),
                "no whole {prefix:?} line in the output: {:?}",
                self.text()
            );
            self.receive(Duration::from_millis(100));
        }
    }

    /// Read up to the session's exit, and the closed queue after it.
    pub fn finish(&mut self) -> ExitStatus {
        let deadline = Instant::now() + PATIENCE;
        while self.exit.is_none() {
            assert!(Instant::now() < deadline, "the session never ended: {:?}", self.text());
            self.receive(Duration::from_millis(100));
        }
        assert_eq!(self.subscription.try_recv(), Err(RecvError::Closed));
        self.exit.clone().unwrap_or(ExitStatus::Unknown)
    }
}

/// `output` without its control strings (OSC, DCS, SOS, PM, APC), whose text a terminal does not show. cmd.exe titles
/// its console with the command it runs, as parsed (`echo got:pong` for the typed `echo got^:pong`), and `ConPTY`
/// passes the title on as an OSC before the command's output: a needle found there was never printed.
pub fn shown(output: &[u8]) -> String {
    let mut shown = Vec::with_capacity(output.len());
    let mut rest = output;
    while let Some((&byte, after)) = rest.split_first() {
        if let (0x1b, Some(&kind @ (b']' | b'P' | b'X' | b'^' | b'_'))) = (byte, after.first()) {
            rest = past_string(&after[1..], kind == b']');
        } else {
            shown.push(byte);
            rest = after;
        }
    }
    String::from_utf8_lossy(&shown).into_owned()
}

/// What follows a control string's `body`: past its ST (or BEL, for an OSC); nothing while it is unterminated.
pub fn past_string(body: &[u8], osc: bool) -> &[u8] {
    for (at, &byte) in body.iter().enumerate() {
        if byte == 0x07 && osc {
            return &body[at + 1..];
        }
        if byte == 0x1b && body.get(at + 1) == Some(&b'\\') {
            return &body[at + 2..];
        }
    }
    &[]
}
