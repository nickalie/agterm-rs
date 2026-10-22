//! A live pane's IO: its session on the session server, attached in process through `ngterm-session`'s client
//! library, in place of a PTY and a child. The pane's terminal is fed the session's snapshot and output as it is fed a
//! PTY's (the same mailbox, IO worker and VT slicing); its input, grid and reply facts go to the server.
//!
//! - **What it attaches to** ([`resolve`]): what `ngterm-session attach` would read when the pane ran it as its
//!   command — the attach contract read from the environment the attach would be started in (the app's, under the
//!   launch's variables, `ngterm_session::cli::Contract`), the working directory it would start in, and the session
//!   and creation argv of core's [`LiveAttach`], the data its shell line is made from. A session the attach creates
//!   gets that environment and directory, as the CLI gives it its own.
//! - **Threads**: `pane-live` connects (starting the server when none runs), reads the stream and hands its output to
//!   the pane's mailbox, pausing while the mailbox is past its high mark (the server then resyncs the pane, as any
//!   slow client); the other events that must follow the output fed before them (a snapshot's end, a reply ownership
//!   change, a Kitty barrier, a delegated request) go through the mailbox too, and the IO worker handles them
//!   ([`handle`]). `pane-live-out` sends what the pane queues ([`Live::input`] within the pane's write limit, never
//!   blocking the caller) in order. A connection that ends without the session's exit (a server stopping, or handing
//!   its sessions to a new build) is attached again as the CLI attaches ([`attach_again`]), under the lead the pane
//!   held, its terminal reset first ([`snapshot::RESYNC_RESET`]).
//! - **Replies**: the server answers the program's queries; the pane's terminal answers none but Kitty graphics
//!   commands, whose answers complete the server's barriers ([`crate::pty::ReplyOutlet`]). The pane is the reply owner
//!   when the server makes it one: it sends its facts (theme colours, cell size, colour scheme, Kitty graphics) with
//!   the attach and on every change, and the frontend answers a delegated clipboard read as any of the pane's own
//!   ([`crate::PaneEvent::ClipboardRead`]). The clipboard reads in the output are the server's to delegate, so the
//!   pane does not stop at them.
//! - **Lead**: a managed pane's role comes as the attach contract's role title, reported as the pane's title
//!   ([`crate::PaneEvent::Title`]) without touching its terminal's own.
//! - **End**: the session's exit ends the pane under its [`crate::ExitPolicy`], as the attach CLI's exit with the
//!   program's status did; an attach that fails (no session to reattach, the server refusing, a broken contract)
//!   writes why and ends it with the CLI's status. Dropping the pane detaches it, leaving the session running.
//!
//! The snapshot is the server's formatter snapshot, the one the CLI and remote attaches take, not libghostty-vt's
//! binary codec: the codec decodes into a new terminal of the server's settings (the pane's has the frontend's theme,
//! scrollback limits, Kitty storage and effects), both ends would have to run one pin with a wire choice for it, and
//! at the pin it carries no Kitty images either.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use ngterm_core::live::support::LiveAttach;
use ngterm_pty::ReadPause;
use ngterm_session::cli::{
    Contract, Origin, Outcome, Prepared, attach_again, prepare, process_environment, same_variable, variable,
};
use ngterm_session::client::{
    AttachEvent, AttachReader, AttachWriter, Attachment, Barrier, Client, ClientError, Delegated,
};
use ngterm_session::protocol::{
    self as wire, AttachRequest, DelegatedReply, DelegatedRequest, ExitStatus, LeadRole, Managed, ReplyMetadata,
    SessionSpec,
};
use ngterm_session::server::spawn::StartOptions;
use ngterm_session::server::{lead, snapshot};

use crate::effects::{ClipboardAnswer, ClipboardLocation, ClipboardReadRequest, ColorScheme, PaneEvent};
use crate::pane::{Shared, lock};
use crate::{GridSize, PaneCommand, PaneError, ReadScope};

/// How long the reader waits on the mailbox's resume before it looks again (a resume wakes it at once).
const PAUSE_POLL: Duration = Duration::from_millis(100);
/// The Kitty graphics answers kept for the next barrier at most (the server ends a barrier past 1 MiB held anyway).
const KITTY_ANSWERS_LIMIT: usize = 1024 * 1024;
/// What starts a Kitty graphics answer (`ESC _ G`), the one reply of the pane's terminal a live pane sends.
const KITTY_ANSWER: &[u8] = b"\x1b_G";

/// The session `attach` names under the launch's `command` (its environment over the app's `base`, its working
/// directory, else the app's `current_dir`, a relative one taken from there), prepared as `ngterm-session attach`
/// prepares its command line, environment and directory.
pub(crate) fn resolve(
    attach: &LiveAttach,
    command: &PaneCommand,
    base: impl IntoIterator<Item = (String, String)>,
    current_dir: Option<PathBuf>,
) -> Result<Prepared, Outcome> {
    let env = child_environment(command, base);
    let contract = Contract::from_env(|name| variable(&env, name).map(Into::into));
    let cwd = match (&command.cwd, current_dir) {
        (Some(cwd), Some(current_dir)) => Some(current_dir.join(cwd)),
        (cwd, current_dir) => cwd.clone().or(current_dir),
    };
    let origin = Origin {
        env,
        cwd,
        executable: PathBuf::from(&attach.executable),
    };
    prepare(&contract, &attach.session, attach.argv.clone(), origin)
}

/// The environment `command`'s child would run in over `base` (the app's, unless it clears it): its removals, then its
/// variables; where names ignore case (Windows) a key stands for every spelling of it ([`same_variable`]).
fn child_environment(command: &PaneCommand, base: impl IntoIterator<Item = (String, String)>) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = if command.env_clear {
        Vec::new()
    } else {
        base.into_iter().collect()
    };
    for removed in &command.env_remove {
        let removed = removed.to_string_lossy();
        env.retain(|(key, _)| !same_variable(key, &removed));
    }
    for (key, value) in &command.env {
        let key = key.to_string_lossy();
        env.retain(|(known, _)| !same_variable(known, &key));
        env.push((key.into_owned(), value.to_string_lossy().into_owned()));
    }
    env
}

/// What the IO worker handles in order with the output ([`handle`]); `connection` names the connection a request
/// came on, whose answer no later one takes.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LiveMail {
    /// A snapshot ended: its Kitty answers are not the server's.
    SnapshotEnd,
    /// The pane's reply ownership changed: the answers so far are nobody's.
    Owner,
    Barrier {
        barrier: Barrier,
        connection: u64,
    },
    Delegated {
        request: Delegated,
        connection: u64,
    },
}

/// What the pane sends the server, in order.
#[derive(Debug)]
enum Message {
    Input(Vec<u8>),
    Resize(wire::GridSize),
    Metadata(ReplyMetadata),
    Answer {
        request: Delegated,
        reply: DelegatedReply,
        connection: u64,
    },
    Complete {
        barrier: Barrier,
        data: Vec<u8>,
        connection: u64,
    },
}

#[derive(Debug, Default)]
struct Outbox {
    queue: VecDeque<Message>,
    /// Input bytes queued.
    input: usize,
    /// The current connection's writer and its number.
    writer: Option<(u64, AttachWriter)>,
    /// The pane left: nothing more is sent.
    closed: bool,
}

impl Outbox {
    /// The first message queued and the current connection, while there is one.
    fn take(&mut self) -> Option<(Message, u64, AttachWriter)> {
        let (connection, writer) = self.writer.clone()?;
        let message = self.queue.pop_front()?;
        if let Message::Input(data) = &message {
            self.input = self.input.saturating_sub(data.len());
        }
        Some((message, connection, writer))
    }
}

/// A live pane's attach, shared by the pane, its IO worker and its two threads.
#[derive(Debug)]
pub(crate) struct Live {
    outbox: Mutex<Outbox>,
    outbox_changed: Condvar,
    /// The mailbox's pause of the reader, and the wake of a paused reader.
    pause: ReadPause,
    resumed: (Mutex<()>, Condvar),
    leaving: AtomicBool,
    /// The most input bytes queued before [`Live::input`] refuses more.
    write_limit: usize,
    /// The delegated requests the frontend has been asked, by their read id.
    pending: Mutex<HashMap<u64, (Delegated, u64)>>,
    next_read: AtomicU64,
    /// The facts last sent, which an attach starts with.
    metadata: Mutex<ReplyMetadata>,
    /// The pane's terminal shows Kitty graphics.
    kitty_graphics: AtomicBool,
}

impl Live {
    pub(crate) fn new(pause: ReadPause, write_limit: usize, kitty_graphics: bool) -> Self {
        Self {
            outbox: Mutex::new(Outbox::default()),
            outbox_changed: Condvar::new(),
            pause,
            resumed: (Mutex::new(()), Condvar::new()),
            leaving: AtomicBool::new(false),
            write_limit,
            pending: Mutex::new(HashMap::new()),
            next_read: AtomicU64::new(0),
            metadata: Mutex::new(ReplyMetadata::default()),
            kitty_graphics: AtomicBool::new(kitty_graphics),
        }
    }

    fn post(&self, message: Message) {
        let mut outbox = lock(&self.outbox);
        if outbox.closed {
            return;
        }
        outbox.queue.push_back(message);
        drop(outbox);
        self.outbox_changed.notify_all();
    }

    /// Queue input for the session's program; never blocks.
    pub(crate) fn input(&self, data: &[u8]) -> Result<(), PaneError> {
        let mut outbox = lock(&self.outbox);
        if outbox.closed {
            return Err(PaneError::Closed);
        }
        if outbox.input > 0 && outbox.input + data.len() > self.write_limit {
            return Err(PaneError::Backpressure);
        }
        outbox.input += data.len();
        outbox.queue.push_back(Message::Input(data.to_vec()));
        drop(outbox);
        self.outbox_changed.notify_all();
        Ok(())
    }

    /// Queue bytes the pane itself sends the program (a clear's form feed), past the write limit as the PTY's replies.
    pub(crate) fn program_input(&self, data: &[u8]) {
        self.post(Message::Input(data.to_vec()));
    }

    /// Input bytes queued and not sent yet.
    pub(crate) fn queued(&self) -> usize {
        lock(&self.outbox).input
    }

    /// The pane's grid; the session takes it while the pane leads.
    pub(crate) fn resize(&self, size: GridSize) {
        self.post(Message::Resize(grid(size)));
    }

    /// The pane's facts now, sent when they changed.
    pub(crate) fn update_metadata(&self, metadata: ReplyMetadata) {
        let mut last = lock(&self.metadata);
        if *last == metadata {
            return;
        }
        last.clone_from(&metadata);
        drop(last);
        self.post(Message::Metadata(metadata));
    }

    pub(crate) fn set_kitty_graphics(&self, shown: bool) {
        self.kitty_graphics.store(shown, Ordering::Release);
    }

    /// The facts of the pane's terminal `shared` for the server's replies.
    pub(crate) fn metadata_of(&self, shared: &Shared, scheme: ColorScheme) -> ReplyMetadata {
        let vt = lock(&shared.vt);
        let colors = vt.terminal.default_colors();
        let rgb = |color: ngterm_ghostty_vt::Rgb| wire::Rgb {
            r: color.r,
            g: color.g,
            b: color.b,
        };
        ReplyMetadata {
            foreground: colors.foreground.map(rgb),
            background: colors.background.map(rgb),
            cursor: colors.cursor.map(rgb),
            palette: colors.palette.iter().copied().map(rgb).collect(),
            cell_width_px: vt.size.cell_width,
            cell_height_px: vt.size.cell_height,
            kitty_graphics: self.kitty_graphics.load(Ordering::Acquire),
            color_scheme: Some(match scheme {
                ColorScheme::Light => wire::ColorScheme::Light,
                ColorScheme::Dark => wire::ColorScheme::Dark,
            }),
        }
    }

    /// The frontend's answer to the clipboard read `id`; one for no read waiting is dropped.
    pub(crate) fn answer(&self, id: u64, answer: ClipboardAnswer) {
        let Some((request, connection)) = lock(&self.pending).remove(&id) else {
            return;
        };
        let reply = match answer {
            ClipboardAnswer::Text(text) => DelegatedReply::Clipboard { text },
            ClipboardAnswer::Denied => DelegatedReply::Denied,
        };
        self.post(Message::Answer {
            request,
            reply,
            connection,
        });
    }

    /// The mailbox resumed the reader.
    pub(crate) fn wake_reader(&self) {
        let _resumed = lock(&self.resumed.0);
        self.resumed.1.notify_all();
    }

    /// Leave the session running: detach the connection (within the client's `DETACH_WAIT`), drop what is queued, and
    /// let the threads end.
    pub(crate) fn leave(&self) {
        self.leaving.store(true, Ordering::Release);
        let writer = {
            let mut outbox = lock(&self.outbox);
            outbox.closed = true;
            outbox.queue.clear();
            outbox.input = 0;
            outbox.writer.take()
        };
        self.outbox_changed.notify_all();
        self.wake_reader();
        if let Some((_, writer)) = writer
            && let Err(error) = writer.detach()
        {
            tracing::debug!(%error, "detaching the live pane failed");
        }
    }

    fn leaving(&self) -> bool {
        self.leaving.load(Ordering::Acquire)
    }

    /// Send on `writer` (connection `connection`) from now on; a pane that left detaches it at once.
    fn install(&self, connection: u64, writer: AttachWriter) {
        let mut outbox = lock(&self.outbox);
        if outbox.closed {
            drop(outbox);
            let _ = writer.detach();
            return;
        }
        outbox.writer = Some((connection, writer));
        drop(outbox);
        self.outbox_changed.notify_all();
    }

    /// The connection ended: nothing is sent until the next one, and the requests it delegated are nobody's.
    fn lost(&self) {
        lock(&self.outbox).writer = None;
        lock(&self.pending).clear();
    }

    /// The `pane-live-out` thread: the queue in order, each message on the connection current when it is sent; an
    /// answer for an earlier connection is dropped.
    pub(crate) fn send_loop(&self) {
        while let Some((message, connection, writer)) = self.next_message() {
            if let Err(error) = send(&writer, connection, message) {
                tracing::debug!(%error, "a live pane's message was not sent");
            }
        }
    }

    /// The next message to send and the connection to send it on, waiting for both; `None` once the pane left.
    fn next_message(&self) -> Option<(Message, u64, AttachWriter)> {
        let mut outbox = lock(&self.outbox);
        loop {
            if outbox.closed {
                return None;
            }
            if let Some(next) = outbox.take() {
                return Some(next);
            }
            outbox = self
                .outbox_changed
                .wait(outbox)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    /// The reader's wait while the mailbox is past its high mark; false once the pane left.
    fn wait_resumed(&self) -> bool {
        let mut guard = lock(&self.resumed.0);
        while self.pause.is_paused() && !self.leaving() {
            guard = self
                .resumed
                .1
                .wait_timeout(guard, PAUSE_POLL)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        !self.leaving()
    }
}

fn send(writer: &AttachWriter, connection: u64, message: Message) -> Result<(), ClientError> {
    match message {
        Message::Input(data) => writer.input(&data),
        Message::Resize(size) => writer.resize(size),
        Message::Metadata(metadata) => writer.metadata(metadata),
        Message::Answer {
            request,
            reply,
            connection: of,
        } if of == connection => writer.answer(request, reply).map(drop),
        Message::Complete {
            barrier,
            data,
            connection: of,
        } if of == connection => writer.complete(barrier, data).map(drop),
        Message::Answer { .. } | Message::Complete { .. } => Ok(()),
    }
}

fn grid(size: GridSize) -> wire::GridSize {
    wire::GridSize {
        cols: size.cols,
        rows: size.rows,
    }
}

/// The IO worker's side of `mail`, after every output byte before it was fed: a barrier completed with the Kitty
/// answers since the previous one, a delegated read asked of the frontend.
pub(crate) fn handle(live: &Live, shared: &Shared, mail: LiveMail) {
    match mail {
        LiveMail::SnapshotEnd | LiveMail::Owner => drop(shared.take_kitty_answers()),
        LiveMail::Barrier { barrier, connection } => {
            let data = shared.take_kitty_answers();
            live.post(Message::Complete {
                barrier,
                data,
                connection,
            });
        }
        LiveMail::Delegated { request, connection } => {
            let DelegatedRequest::ClipboardRead { selection, list_only } = &request.request;
            let location = match selection.as_str() {
                "s" => ClipboardLocation::Selection,
                "p" => ClipboardLocation::Primary,
                _ => ClipboardLocation::Standard,
            };
            let scope = if *list_only {
                ReadScope::ListOnly
            } else {
                ReadScope::Data
            };
            let id = live.next_read.fetch_add(1, Ordering::AcqRel) + 1;
            lock(&live.pending).insert(id, (request, connection));
            shared.send(PaneEvent::ClipboardRead(ClipboardReadRequest { id, location, scope }));
            shared.wake();
        }
    }
}

/// Keep `data`, a reply of the pane's terminal, in `answers` when it is a Kitty graphics answer: the only replies a
/// live pane's terminal sends.
pub(crate) fn keep_kitty_answer(answers: &mut Vec<u8>, data: &[u8]) {
    if data.starts_with(KITTY_ANSWER) && answers.len() + data.len() <= KITTY_ANSWERS_LIMIT {
        answers.extend_from_slice(data);
    }
}

/// Whether an attach claims the lead.
#[derive(Debug, Clone, Copy)]
enum Claim {
    /// As the contract says: a pane's first attach.
    AsAsked,
    /// When the pane led with its role before: a reattach.
    IfLed(Option<LeadRole>),
}

/// How a connection's stream ended.
enum Ended {
    Exit(ExitStatus),
    /// The pane left.
    Left,
    /// The connection ended without the session's exit.
    Lost,
    Failed(String),
}

/// The `pane-live` thread's work: attach, read, attach again, end the pane.
pub(crate) struct Reader {
    pub live: Arc<Live>,
    pub shared: Arc<Shared>,
    pub session: Result<Prepared, Outcome>,
}

impl Reader {
    pub(crate) fn run(self) {
        let session = match &self.session {
            Ok(session) => session.clone(),
            Err(refusal) => return self.fail(refusal),
        };
        let client = Client::new(&session.directory).starting(StartOptions::new(&session.executable));
        let attached = client.attach(self.request(&session, Some(session.spec.clone()), Claim::AsAsked));
        let mut attachment = match attached {
            Ok(attachment) => attachment,
            Err(error) => return self.fail(&Outcome::failed(error.to_string())),
        };
        let mut role = None;
        let mut connection = 0;
        loop {
            connection += 1;
            let Attachment { reader, writer, .. } = attachment;
            self.live.install(connection, writer);
            match self.read(reader, &session, connection, &mut role) {
                Ended::Exit(ExitStatus::Exited { code, signal }) => return self.end(Some(code), signal),
                Ended::Exit(ExitStatus::Unknown) => return self.end(None, None),
                Ended::Left => return,
                Ended::Failed(message) => return self.fail(&Outcome::failed(message)),
                Ended::Lost => {
                    self.live.lost();
                    match self.reattach(&session, role) {
                        Ok(Some(next)) => attachment = next,
                        Ok(None) => return,
                        Err(refusal) => return self.fail(&refusal),
                    }
                }
            }
        }
    }

    /// The attach request for `session` at the pane's grid and facts, a managed one claiming the lead as `claim` says.
    fn request(&self, session: &Prepared, create: Option<SessionSpec>, claim: Claim) -> AttachRequest {
        let size = lock(&self.shared.vt).size;
        let managed = session.managed.as_ref().map(|managed| Managed {
            nonce: managed.nonce.clone(),
            claim: match claim {
                Claim::AsAsked => managed.claim,
                Claim::IfLed(role) => role == Some(LeadRole::Leader),
            },
        });
        AttachRequest {
            name: session.name.clone(),
            create,
            size: grid(size),
            managed,
            metadata: lock(&self.live.metadata).clone(),
        }
    }

    /// Read connection `connection`'s stream into the pane until it ends.
    fn read(
        &self,
        mut reader: AttachReader,
        session: &Prepared,
        connection: u64,
        role: &mut Option<LeadRole>,
    ) -> Ended {
        let mailbox = self.shared.mailbox();
        // a reattach's first snapshot starts over from a fresh terminal, as a resync's does
        let mut reset = connection > 1;
        loop {
            let event = match reader.recv() {
                Ok(Some(event)) => event,
                Ok(None) => return Ended::Left,
                Err(_) if self.live.leaving() => return Ended::Left,
                Err(ClientError::Disconnected) => return Ended::Lost,
                Err(error) => return Ended::Failed(error.to_string()),
            };
            match event {
                AttachEvent::SnapshotBegin { .. } => {
                    if std::mem::take(&mut reset) {
                        mailbox.deliver(snapshot::RESYNC_RESET);
                    }
                }
                AttachEvent::SnapshotChunk(data) | AttachEvent::Output { data, .. } => {
                    mailbox.deliver(&data);
                    if !self.live.wait_resumed() {
                        return Ended::Left;
                    }
                }
                AttachEvent::SnapshotEnd => mailbox.deliver_live(LiveMail::SnapshotEnd),
                AttachEvent::ReplyOwner { .. } => mailbox.deliver_live(LiveMail::Owner),
                AttachEvent::KittyBarrier(barrier) => mailbox.deliver_live(LiveMail::Barrier { barrier, connection }),
                AttachEvent::Delegated(request) => mailbox.deliver_live(LiveMail::Delegated { request, connection }),
                AttachEvent::Role { role: told, generation } => {
                    *role = Some(told);
                    if let Some(managed) = &session.managed {
                        self.shared
                            .send(PaneEvent::Title(lead::role_report(&managed.nonce, told, generation)));
                        self.shared.wake();
                    }
                }
                AttachEvent::Resized { .. } => {}
                AttachEvent::Exit(status) => return Ended::Exit(status),
                AttachEvent::Shutdown(_) => return Ended::Lost,
            }
        }
    }

    /// Attach again after a connection ended (module docs); `None` once the pane left.
    fn reattach(&self, session: &Prepared, role: Option<LeadRole>) -> Result<Option<Attachment>, Outcome> {
        attach_again(&session.directory, || {
            (!self.live.leaving()).then(|| self.request(session, None, Claim::IfLed(role)))
        })
    }

    /// End the pane with `outcome`, its complaint written after the output.
    fn fail(&self, outcome: &Outcome) {
        tracing::warn!(
            code = outcome.code,
            message = outcome.message.as_deref(),
            "a live pane's attach failed"
        );
        if let Some(message) = &outcome.message {
            self.shared.mailbox().deliver(format!("\r\n{message}\r\n").as_bytes());
        }
        self.end(Some(u32::from(outcome.code)), None);
    }

    /// The session's program (or the attach) ended with `code` (`None`: unknown) and `signal`: the pane's end.
    fn end(&self, code: Option<u32>, signal: Option<String>) {
        if self.live.leaving() {
            return;
        }
        let mailbox = self.shared.mailbox();
        mailbox.end_of_output();
        mailbox.child_exited();
        crate::pane::report_exit(&self.shared, code, signal);
    }
}

/// The app's environment as a child inherits it (as the attach CLI reads its own, [`process_environment`]), and its
/// working directory.
pub(crate) fn app_environment() -> (Vec<(String, String)>, Option<PathBuf>) {
    (process_environment(), std::env::current_dir().ok())
}

#[cfg(test)]
mod tests;
