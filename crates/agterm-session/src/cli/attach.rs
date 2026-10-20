//! `agterm-session attach`: the attach contract's client (`super` module docs) as a terminal client of the outer
//! terminal it runs in (`crate::attach_tty`), and the session's reply owner when the server makes it one.
//!
//! One loop owns everything: the outer terminal's input and size changes and the server's stream come to it as events
//! from threads of their own, and it alone writes to the outer terminal and to the server. Before it attaches, the loop
//! gives the outer terminal's probe up to [`PROBE_WAIT`] to be answered, so the attach carries its facts; what the user
//! typed meanwhile follows the attach. The server's output reaches the loop within a byte budget ([`OUTPUT_BUDGET`]),
//! so a slow outer terminal holds the stream back and the server resyncs this client, as it would any slow one.
//!
//! A connection that ends without the session's exit (the server stopped, or handed its sessions to a new build and
//! disconnected its clients) is attached again, without creating the session, for up to [`REATTACH_WAIT`]: the outer
//! terminal is reset (`server::snapshot::RESYNC_RESET`) and the new snapshot written, under the lead the client held.
//! The first attach clears the outer terminal first, as zmx's did; leaving it, the client resets it (RIS and the
//! default colours, zmx's `restore_seq`).

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use super::{Contract, EXIT_OK, default_argv};
use crate::attach_tty::input::{Held, InputParser, Token};
use crate::attach_tty::owner::{Effects, Outgoing, Relay};
use crate::attach_tty::{Tty, TtyEvent, write_out};
use crate::client::{AttachEvent, AttachReader, AttachWriter, Attachment, Client, ClientError};
use crate::protocol::{AttachRefusal, AttachRequest, ExitStatus, LeadRole, Managed, SessionSpec};
use crate::server::spawn::StartOptions;
use crate::server::{EXIT_FAILED, snapshot};

/// How long the attach waits for the outer terminal to answer the probe.
pub const PROBE_WAIT: Duration = Duration::from_millis(500);

/// How long a cut sequence of the input that may be a key waits for its end before it is taken as typed.
pub const INPUT_QUIET: Duration = Duration::from_millis(50);

/// How long, while an answer of the outer terminal is due, a cut sequence that may be one waits for its end before it
/// is dropped, and a cut CSI before it is taken as typed: a pause inside an answer (a slow link) is no key. With no
/// answer due, both wait [`INPUT_QUIET`] and are taken as typed, and so they do once the relay gives the answer up.
pub const ANSWER_QUIET: Duration = Duration::from_secs(2);

/// How long a client whose connection ended tries to attach again.
pub const REATTACH_WAIT: Duration = Duration::from_secs(5);

/// How often it tries.
const REATTACH_POLL: Duration = Duration::from_millis(100);

/// The session's output read from the server but not yet written to the outer terminal, past which the reader waits.
pub const OUTPUT_BUDGET: usize = 4 * 1024 * 1024;

/// What the first attach writes ahead of its snapshot (zmx's `clear_seq`).
const CLEAR: &[u8] = b"\x1b[2J\x1b[H";

/// What leaving writes (zmx's `restore_seq`): RIS and the default foreground, background and cursor colours.
const RESTORE: &[u8] = b"\x1bc\x1b]110\x1b\\\x1b]111\x1b\\\x1b]112\x1b\\";

/// `agterm-session attach <name> [argv]` under `contract`: its exit status.
pub(super) fn run(contract: &Contract, name: &str, argv: Vec<String>) -> u8 {
    let outcome = prepare(contract, name, argv).map(|session| {
        let tty = Tty::open();
        let outcome = Loop::new(&tty, session).run();
        if tty.is_terminal() {
            let _ = write_out(RESTORE);
        }
        outcome
        // the terminal's mode is restored here, before the message
    });
    let outcome = outcome.unwrap_or_else(|failure| failure);
    if let Some(message) = &outcome.message {
        eprintln!("agterm-session attach: {message}");
    }
    outcome.code
}

/// How an attach ended.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Outcome {
    code: u8,
    message: Option<String>,
}

impl Outcome {
    fn ok(code: u8) -> Self {
        Self { code, message: None }
    }

    fn failed(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_FAILED,
            message: Some(message.into()),
        }
    }
}

/// The exit status of a client whose session's program ended with `status`.
fn exit_code(status: &ExitStatus) -> u8 {
    match status {
        ExitStatus::Exited { code, .. } => u8::try_from(*code).unwrap_or(EXIT_FAILED),
        ExitStatus::Unknown => EXIT_OK,
    }
}

/// What an attach needs before it touches the terminal.
struct Prepared {
    directory: PathBuf,
    name: String,
    spec: SessionSpec,
    managed: Option<Managed>,
    detach_key: bool,
    executable: PathBuf,
}

fn prepare(contract: &Contract, name: &str, argv: Vec<String>) -> Result<Prepared, Outcome> {
    if let Some(inside) = &contract.session {
        return Err(Outcome::failed(format!(
            "this runs inside the live session {inside} (AGTERM_LIVE_SESSION); set it empty to attach from here"
        )));
    }
    let usage = |message: String| Outcome {
        code: crate::server::EXIT_USAGE,
        message: Some(message),
    };
    let directory = contract.directory.clone().ok_or_else(|| {
        usage("AGTERM_LIVE_DIR is not set: it names the live sessions' namespace directory".to_owned())
    })?;
    let directory = std::path::absolute(&directory).map_err(|error| Outcome::failed(error.to_string()))?;
    let name = contract.session_name(name).map_err(usage)?;
    let argv = if argv.is_empty() {
        default_argv(std::env::var_os("SHELL").as_deref())
    } else {
        argv
    };
    let env = std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .collect();
    let cwd = std::env::current_dir()
        .ok()
        .map(|cwd| cwd.to_string_lossy().into_owned());
    let executable = std::env::current_exe().map_err(|error| Outcome::failed(error.to_string()))?;
    Ok(Prepared {
        directory,
        name,
        spec: SessionSpec {
            argv,
            env,
            cwd,
            tracked_env: Vec::new(),
        },
        managed: contract.managed.clone(),
        detach_key: contract.detach_key,
        executable,
    })
}

/// What the loop is told.
enum Event {
    Tty(TtyEvent),
    /// The server's stream of connection `connection`, and the output bytes the event carries.
    Server {
        connection: u64,
        bytes: usize,
        event: Result<Option<AttachEvent>, ClientError>,
    },
}

impl From<TtyEvent> for Event {
    fn from(event: TtyEvent) -> Self {
        Self::Tty(event)
    }
}

/// The output bytes on their way from the server's reader to the outer terminal ([`OUTPUT_BUDGET`]).
#[derive(Debug, Default)]
struct Budget {
    used: Mutex<usize>,
    freed: Condvar,
}

impl Budget {
    fn take(&self, bytes: usize) {
        let mut used = self.used.lock().unwrap_or_else(PoisonError::into_inner);
        while bytes > 0 && *used > OUTPUT_BUDGET {
            used = self.freed.wait(used).unwrap_or_else(PoisonError::into_inner);
        }
        *used += bytes;
    }

    fn give(&self, bytes: usize) {
        if bytes == 0 {
            return;
        }
        let mut used = self.used.lock().unwrap_or_else(PoisonError::into_inner);
        *used = used.saturating_sub(bytes);
        self.freed.notify_all();
    }
}

/// The attach loop (module docs).
struct Loop<'a> {
    tty: &'a Tty,
    session: Prepared,
    relay: Relay,
    parser: InputParser,
    sender: Sender<Event>,
    events: Receiver<Event>,
    budget: Arc<Budget>,
    writer: Option<AttachWriter>,
    /// The current connection; events of an earlier one are dropped.
    connection: u64,
    /// The first snapshot of the current connection is still to come.
    first_snapshot: bool,
    /// What was typed before the attach.
    typed: Vec<u8>,
    /// When the input last came, which a cut sequence held waits from (`Loop::quiet`).
    input_at: Option<Instant>,
    /// The role the server last told this client.
    role: Option<LeadRole>,
}

impl<'a> Loop<'a> {
    fn new(tty: &'a Tty, session: Prepared) -> Self {
        let (sender, events) = mpsc::channel();
        let nonce = session.managed.as_ref().map(|managed| managed.nonce.clone());
        let detach_key = session.detach_key;
        let size = tty.size();
        Self {
            tty,
            relay: Relay::new(nonce, size.grid, size.pixels),
            parser: InputParser::new(detach_key),
            session,
            sender,
            events,
            budget: Arc::new(Budget::default()),
            writer: None,
            connection: 0,
            first_snapshot: true,
            typed: Vec::new(),
            input_at: None,
            role: None,
        }
    }

    fn run(mut self) -> Outcome {
        if let Err(error) = self.tty.start_threads(&self.sender) {
            return Outcome::failed(format!("could not read the terminal: {error}"));
        }
        let mut fx = Effects::default();
        self.relay.start(self.tty.is_terminal(), Instant::now(), &mut fx);
        if let Err(outcome) = self.emit(fx) {
            return outcome;
        }
        if let Err(outcome) = self.await_probe() {
            return outcome;
        }
        if let Err(outcome) = self.first_attach() {
            return outcome;
        }
        loop {
            if let Err(outcome) = self.turn() {
                return outcome;
            }
        }
    }

    /// Handles the outer terminal's events until the probe is answered or [`PROBE_WAIT`] is over.
    fn await_probe(&mut self) -> Result<(), Outcome> {
        let deadline = Instant::now() + PROBE_WAIT;
        while self.relay.probing() {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            match self.events.recv_timeout(deadline - now) {
                Ok(event) => self.handle(event)?,
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return Err(Outcome::failed("the terminal's input ended")),
            }
        }
        Ok(())
    }

    fn first_attach(&mut self) -> Result<(), Outcome> {
        let client = Client::new(&self.session.directory).starting(StartOptions::new(&self.session.executable));
        let request = AttachRequest {
            name: self.session.name.clone(),
            create: Some(self.session.spec.clone()),
            size: self.tty.size().grid,
            managed: self.session.managed.clone(),
            metadata: self.relay.attach_metadata(),
        };
        let attachment = client
            .attach(request)
            .map_err(|error| Outcome::failed(error.to_string()))?;
        self.connect(attachment);
        let typed = std::mem::take(&mut self.typed);
        if !typed.is_empty() {
            self.input(&typed);
        }
        Ok(())
    }

    /// Takes `attachment` as the current connection: its reader on a thread of its own, its writer here.
    fn connect(&mut self, attachment: Attachment) {
        if let Some(old) = self.writer.take() {
            old.close();
        }
        self.connection += 1;
        self.first_snapshot = true;
        self.writer = Some(attachment.writer);
        spawn_reader(
            attachment.reader,
            self.connection,
            self.sender.clone(),
            Arc::clone(&self.budget),
        );
    }

    /// One event, or what is due.
    fn turn(&mut self) -> Result<(), Outcome> {
        let deadline = [self.relay.next_deadline(), self.quiet()].into_iter().flatten().min();
        let event = match deadline {
            Some(deadline) => match self
                .events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(event) => Some(event),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return Err(Outcome::ok(EXIT_OK)),
            },
            None => Some(self.events.recv().map_err(|_| Outcome::ok(EXIT_OK))?),
        };
        if let Some(event) = event {
            self.handle(event)?;
        }
        self.due(Instant::now())
    }

    /// What is due at `now`: the relay's deadlines, then a cut input sequence given up (taken as typed, or dropped as a
    /// cut answer while one is still due).
    fn due(&mut self, now: Instant) -> Result<(), Outcome> {
        let mut fx = Effects::default();
        self.relay.tick(now, &mut fx);
        self.emit(fx)?;
        if self.quiet().is_some_and(|quiet| now >= quiet) {
            let tokens = self.parser.flush(self.relay.awaiting());
            self.tokens(tokens)?;
        }
        Ok(())
    }

    fn handle(&mut self, event: Event) -> Result<(), Outcome> {
        match event {
            Event::Tty(TtyEvent::Input(bytes)) => {
                self.input_at = Some(Instant::now());
                let tokens = self.parser.feed(&bytes);
                self.tokens(tokens)
            }
            // zmx detaches at the end of its input
            Event::Tty(TtyEvent::InputEnd) => Err(self.detach()),
            Event::Tty(TtyEvent::Resize) => {
                let size = self.tty.size();
                if let Some(writer) = &self.writer {
                    let _ = writer.resize(size.grid);
                }
                let mut fx = Effects::default();
                self.relay.resize(size.grid, size.pixels, Instant::now(), &mut fx);
                self.emit(fx)
            }
            Event::Server {
                connection,
                bytes,
                event,
            } => {
                let result = if connection == self.connection {
                    self.server(event)
                } else {
                    Ok(())
                };
                self.budget.give(bytes);
                result
            }
        }
    }

    /// When the cut sequence the input holds is given up (`attach_tty::input`): [`ANSWER_QUIET`] after the last input
    /// for what may be an answer while one is due, [`INPUT_QUIET`] otherwise, so a hold outlives no answer the relay
    /// gave up. `None` with none held, or one that waits for good.
    fn quiet(&self) -> Option<Instant> {
        let wait = match self.parser.held()? {
            Held::Csi | Held::Answer if self.relay.awaiting() => ANSWER_QUIET,
            Held::Key | Held::Csi | Held::Answer => INPUT_QUIET,
        };
        Some(self.input_at? + wait)
    }

    fn tokens(&mut self, tokens: Vec<Token>) -> Result<(), Outcome> {
        let mut fx = Effects::default();
        for token in tokens {
            match token {
                Token::Input(bytes) => self.input(&bytes),
                Token::Answer(answer) => self.relay.answer(answer, Instant::now(), &mut fx),
                Token::Detach => {
                    self.emit(fx)?;
                    return Err(self.detach());
                }
            }
        }
        self.emit(fx)
    }

    fn input(&mut self, bytes: &[u8]) {
        match &self.writer {
            Some(writer) => {
                let _ = writer.input(bytes);
            }
            None => self.typed.extend_from_slice(bytes),
        }
    }

    /// Leaves the session running.
    fn detach(&mut self) -> Outcome {
        if let Some(writer) = &self.writer {
            let _ = writer.detach();
        }
        Outcome::ok(EXIT_OK)
    }

    /// One event of the current connection.
    fn server(&mut self, event: Result<Option<AttachEvent>, ClientError>) -> Result<(), Outcome> {
        let mut fx = Effects::default();
        let now = Instant::now();
        match event {
            Ok(Some(AttachEvent::SnapshotBegin { .. })) => {
                if self.first_snapshot {
                    self.first_snapshot = false;
                    let reset = if self.connection == 1 {
                        CLEAR
                    } else {
                        snapshot::RESYNC_RESET
                    };
                    self.relay.output(reset, &mut fx);
                }
            }
            Ok(Some(AttachEvent::SnapshotChunk(data) | AttachEvent::Output { data, .. })) => {
                self.relay.output(&data, &mut fx);
            }
            Ok(Some(AttachEvent::Role { role, generation })) => {
                self.role = Some(role);
                self.relay.role(role, generation, now, &mut fx);
            }
            Ok(Some(AttachEvent::KittyBarrier(barrier))) => self.relay.barrier(barrier, now, &mut fx),
            Ok(Some(AttachEvent::Delegated(request))) => self.relay.delegated(request, now, &mut fx),
            Ok(Some(AttachEvent::Exit(status))) => {
                self.emit(fx)?;
                return Err(Outcome::ok(exit_code(&status)));
            }
            Ok(Some(AttachEvent::SnapshotEnd | AttachEvent::Resized { .. } | AttachEvent::ReplyOwner { .. })) => {}
            // the client's own detach or close
            Ok(None) => return Err(Outcome::ok(EXIT_OK)),
            Ok(Some(AttachEvent::Shutdown(_))) | Err(ClientError::Disconnected) => {
                self.emit(fx)?;
                return self.reattach();
            }
            Err(error) => {
                self.emit(fx)?;
                return Err(Outcome::failed(error.to_string()));
            }
        }
        self.emit(fx)
    }

    /// Attaches again to the session after its connection ended (module docs).
    fn reattach(&mut self) -> Result<(), Outcome> {
        let client = Client::new(&self.session.directory);
        let deadline = Instant::now() + REATTACH_WAIT;
        loop {
            let request = AttachRequest {
                name: self.session.name.clone(),
                create: None,
                size: self.tty.size().grid,
                managed: self.session.managed.as_ref().map(|managed| Managed {
                    nonce: managed.nonce.clone(),
                    claim: self.role == Some(LeadRole::Leader),
                }),
                metadata: self.relay.attach_metadata(),
            };
            match client.attach(request) {
                Ok(attachment) => {
                    self.relay.reattached();
                    self.connect(attachment);
                    return Ok(());
                }
                Err(ClientError::Refused {
                    reason: AttachRefusal::Gone,
                    ..
                }) => {
                    return Err(Outcome::failed(format!(
                        "the live session {} is gone",
                        self.session.name
                    )));
                }
                Err(error) if Instant::now() >= deadline => {
                    return Err(Outcome::failed(format!(
                        "lost the connection to the live session server: {error}"
                    )));
                }
                Err(_) => thread::sleep(REATTACH_POLL),
            }
        }
    }

    /// Writes what handling an event produced: bytes to the outer terminal, then messages to the server.
    fn emit(&mut self, fx: Effects) -> Result<(), Outcome> {
        if !fx.stdout.is_empty()
            && let Err(error) = write_out(&fx.stdout)
        {
            let _ = self.detach();
            return Err(Outcome::failed(format!("could not write to the terminal: {error}")));
        }
        let Some(writer) = &self.writer else {
            return Ok(());
        };
        for outgoing in fx.outgoing {
            let _ = match outgoing {
                Outgoing::Answer(request, reply) => writer.answer(request, reply).map(drop),
                Outgoing::Complete(barrier, data) => writer.complete(barrier, data).map(drop),
                Outgoing::Metadata(metadata) => writer.metadata(metadata),
            };
        }
        Ok(())
    }
}

/// Reads connection `connection`'s stream on a thread of its own, into `events`, within the output budget.
fn spawn_reader(mut reader: AttachReader, connection: u64, events: Sender<Event>, budget: Arc<Budget>) {
    let spawned = thread::Builder::new().name("attach-server".to_owned()).spawn(move || {
        loop {
            let event = reader.recv();
            let bytes = match &event {
                Ok(Some(AttachEvent::Output { data, .. } | AttachEvent::SnapshotChunk(data))) => data.len(),
                _ => 0,
            };
            let last = !matches!(
                &event,
                Ok(Some(event)) if !matches!(event, AttachEvent::Exit(_) | AttachEvent::Shutdown(_))
            );
            budget.take(bytes);
            let sent = events.send(Event::Server {
                connection,
                bytes,
                event,
            });
            if last || sent.is_err() {
                return;
            }
        }
    });
    if let Err(error) = spawned {
        tracing::error!(%error, "the attach stream's reader could not be started");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_exit_status_is_the_programs_and_unknown_is_success() {
        let exited = |code| ExitStatus::Exited { code, signal: None };
        assert_eq!(exit_code(&exited(0)), 0);
        assert_eq!(exit_code(&exited(7)), 7);
        assert_eq!(exit_code(&exited(0xC000_013A)), EXIT_FAILED);
        assert_eq!(exit_code(&ExitStatus::Unknown), EXIT_OK);
    }

    #[test]
    fn the_budget_holds_the_reader_back_until_output_is_written() {
        let budget = Arc::new(Budget::default());
        budget.take(OUTPUT_BUDGET + 1);
        let (sender, received) = mpsc::channel();
        let waiting = Arc::clone(&budget);
        let reader = thread::spawn(move || {
            waiting.take(10);
            let _ = sender.send(());
        });
        assert!(received.recv_timeout(Duration::from_millis(100)).is_err());
        budget.give(OUTPUT_BUDGET + 1);
        assert!(received.recv_timeout(Duration::from_secs(10)).is_ok());
        let _ = reader.join();
    }
}
