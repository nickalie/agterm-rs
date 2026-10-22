//! The session's answers to its program's queries: one answer per query, whichever clients are attached.
//!
//! zmx answered nothing itself: every attached client's own terminal did, so two clients meant two answers and none
//! meant none. Here the server's terminal ([`super::pane`]) answers, through libghostty-vt's effects, so the session
//! has one identity:
//!
//! - **State and capability queries** (DA1/2/3, DSR and CPR, DECRQM, DECRQSS, XTVERSION, XTGETTCAP with the
//!   session's `TERM` as its terminfo name, Kitty keyboard flags, the glyph protocol's glossary, `CSI 21 t`) come
//!   from the terminal's state with Ghostty's identity, as `ngterm-term` answers a plain pane's ([`ServerEffects`]).
//! - **Frontend-dependent queries** (OSC 4/10/11/12 and Kitty's OSC 21 colours, XTWINOPS `14 t`/`16 t` pixel sizes,
//!   mode 2048 reports, `CSI ? 996 n` and mode 2031 reports) come from the reply owner's
//!   [`ReplyMetadata`], applied to the terminal as its defaults (a program's own OSC overrides stay theirs), and from
//!   stable defaults ([`Facts`]) while there is no owner or it left a value out. Kitty images placed on the server's
//!   terminal take the owner's cell size too, so the cursor moves past them as on the owner's screen.
//! - **Clipboard reads** (OSC 52 `?`, Kitty's OSC 5522 reads) need the owner's clipboard, and maybe its user's
//!   consent, which the synchronous read callback cannot wait for. The output is scanned as `ngterm-term` scans a
//!   plain pane's ([`vt::Scanner`]): it is fed to the terminal and fanned out to the clients up to the byte that would
//!   dispatch the read, which is sent to the owner alone as a [`DelegatedRequest`] in its current epoch; the
//!   dispatch byte and the output after it wait in the pane ([`Replies`]), while the PTY keeps being read (up to
//!   [`PARK_HIGH`] bytes held, then the actor's reads pause until they drop under [`PARK_LOW`]). The owner's answer,
//!   or a denial when the owner changes, the owner falls behind or leaves, the program ends or no answer comes within
//!   [`DELEGATION_WAIT`], is armed for the dispatch byte, which is fed alone, so Ghostty still parses, validates and
//!   answers the read; then the held output goes on, scanned again. A pane's helper thread ends a request whose time
//!   is up.
//! - **Kitty graphics** (APC G): the server's terminal keeps the protocol on, its storage bounded
//!   ([`KITTY_STORAGE_LIMIT`]), so it places direct images as the clients do (the cursor moves past them, which the
//!   protocol turned off would not), with its image media off, so a temporary file or shared memory is read once, by a
//!   client that shows it. An image sent that way (`t=f`, `t=t`, `t=s`) fails to load on the server's terminal, which
//!   then places nothing: its cursor stays where the owner's moved away from (with the default `C=0`), so the server
//!   answers CPR and DSR from there and later snapshots put the cursor there, until the program moves it again. Its
//!   own APC G answers are dropped, and the owner's terminal answers instead: the scanner stops at the byte that runs
//!   each command ([`vt::Stop::KittyGraphics`]), and the output up to it is followed by a barrier sent to the owner
//!   alone ([`ClientEvent::KittyBarrier`](super::clients::ClientEvent), when its [`ReplyMetadata::kitty_graphics`]
//!   says it shows them), which it completes with what its terminal answered
//!   ([`AttachClientMessage::KittyComplete`](crate::protocol::AttachClientMessage)), even nothing. The output goes on
//!   meanwhile; the server's replies to what follows wait behind the barrier, so the program reads the owner's answers
//!   before the answers to its later queries (Kitty's detection sends a query, then DA1, and takes DA1 coming first as
//!   no support). A barrier ends empty past [`KITTY_WAIT`], when the owner changes, falls behind, leaves or withdraws
//!   `kitty_graphics`, or past [`HELD_LIMIT`] or [`BARRIER_LIMIT`]. Neither the formatter snapshot nor, at this pin,
//!   the binary snapshot carries images or glyph registrations, so an image is not shown again to a client that
//!   attaches later.
//!
//! Every reply, whichever thread makes it (a read of the PTY, a client's answer or completion, a resize, a deadline),
//! is queued in the pane's [`Outlet`] under the pane's lock, in that lock's order; one thread, the pane's helper, takes
//! them and writes them to the program, so it reads them in the order of its queries.
//!
//! # The reply owner
//!
//! The leader, else the first managed client attached ([`Lead::reply_owner`](super::lead::Lead::reply_owner)). Every
//! client is told whether it owns the replies before its first snapshot, then on every change, in order with the
//! output ([`ClientEvent::ReplyOwner`](super::clients::ClientEvent)), each change counted by a generation; only the
//! owner is sent Kitty barriers, and a change ends those waiting empty, so no Kitty command is answered twice, though
//! one the leaving owner was still to answer goes unanswered. Only the owner's [`ReplyMetadata`] counts, and a
//! client's is forgotten when it detaches.
//!
//! # What herdr's pane answered itself
//!
//! Checked against the C API at `LIBGHOSTTY_VT_REV` (herdr's `src/pane/` at `HERDR_REV`, Apache-2.0): nothing of it
//! is ported. `osc.rs` tracks OSC 10/11/12 queries and sets so herdr can answer default-colour queries from its host
//! terminal's theme and restore that theme around its own drawing; libghostty-vt answers them from the terminal's
//! default colours (`stream_terminal.zig` `colorOperation`, `Terminal.colorForXterm`), which carry the owner's theme
//! here, and nothing draws in a host terminal. Its title, progress and debug trackers serve herdr's agent detection,
//! and the working directory is the terminal's own (`Terminal::pwd`). `xtgettcap.rs` answers XTGETTCAP sent with an
//! 8-bit C1 DCS, which Ghostty, the reference, leaves unanswered (its parser reads C1 bytes in the ground state as
//! UTF-8). `kitty_keyboard.rs` records the keyboard modes only to replay them across herdr's handoff; the formatter
//! snapshot's extras carry them here.
//!
//! # Windows
//!
//! The pseudoconsole (conhost) runs a terminal of its own between the program and the server: it answers some
//! queries itself, passes others on to its output pipe (re-encoded at times), and keeps others; and the answers the
//! server writes back pass through conhost's input parser, which lets DCS and CSI answers through and drops OSC and
//! APC ones. Recorded by `tests/replies.rs` `windows_conhost_record` (it prints the table) on Windows Server 2025,
//! 10.0.26100.33438 (`windows-2025`, the CI runner), the version measured; another version is recorded by running that
//! test there:
//!
//! | query | reaches the server | the program's answer |
//! |---|---|---|
//! | DA1, DA2, DECRQM | no | conhost's own: `CSI ? 61;6;7;21;22;23;24;28;32;42 c`, `CSI > 0;10;1 c`, not recognized |
//! | CPR | yes | one answer, the cursor's row and column |
//! | DA3, DSR, DECRQSS, XTVERSION, Kitty keyboard flags | yes | the server's |
//! | `CSI ? 996 n`, XTWINOPS `14 t` and `18 t` | yes | the server's |
//! | XTGETTCAP | no | none |
//! | OSC 10, 11, 12 | no | none |
//! | OSC 4 | yes | none (conhost drops the OSC answer) |
//! | OSC 52 read | no | none: no clipboard read is delegated |
//! | Kitty graphics (APC G) | no | none: no image reaches a client |
//!
//! So on that version a program in a live pane still gets one answer per query, never two, but conhost's identity
//! for DA1 and DA2, and no theme colours, clipboard reads or Kitty graphics: the pseudoconsole stands in their way,
//! as it stands in front of a plain pane's program (`ngterm-term` runs it on a pseudoconsole too).

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use ngterm_ghostty_vt as vt;
use ngterm_pty::ReadPause;
use vt::{ReadCandidate, ReadScope, Stop};

use super::clients::ClientId;
use super::pane::State;
use super::recovery::Recorder;
use crate::protocol::{ColorScheme, DelegatedReply, DelegatedRequest, ReplyMetadata, Rgb};

/// The foreground the server's terminal answers with while no owner gave one: Ghostty's default (`foreground` in its
/// config), as ngterm's.
pub const DEFAULT_FOREGROUND: Rgb = Rgb {
    r: 0xff,
    g: 0xff,
    b: 0xff,
};
/// The background the server's terminal answers with while no owner gave one: Ghostty's default.
pub const DEFAULT_BACKGROUND: Rgb = Rgb {
    r: 0x28,
    g: 0x2c,
    b: 0x34,
};
/// The appearance the server's terminal answers with while no owner gave one, as `ngterm-term`'s default.
pub const DEFAULT_COLOR_SCHEME: ColorScheme = ColorScheme::Light;
/// How long a delegated request waits for its answer before it is denied (a consent prompt included).
pub const DELEGATION_WAIT: Duration = Duration::from_secs(30);
/// The output a pane holds while a request waits before it stops reading the PTY (`ngterm-term`'s mailbox mark).
pub const PARK_HIGH: usize = 1024 * 1024;
/// The output held under which a paused PTY is read again.
pub const PARK_LOW: usize = 256 * 1024;
/// The Kitty image storage of the server's terminal, per screen: it holds the direct images programs place, for
/// their place in the grid, and nothing else reads them.
pub const KITTY_STORAGE_LIMIT: u64 = 64 * 1024 * 1024;
/// How long a Kitty barrier waits for the reply owner's completion before it ends empty (no user is asked: the owner's
/// terminal answers as it processes the output).
pub const KITTY_WAIT: Duration = Duration::from_secs(2);
/// The server's replies held behind Kitty barriers, in bytes, past which every barrier waiting ends empty.
pub const HELD_LIMIT: usize = 1024 * 1024;
/// The Kitty barriers waiting (completed ones behind a waiting one included), past which every one ends empty.
pub const BARRIER_LIMIT: usize = 4096;

/// The introducer of a Kitty graphics answer.
const KITTY_ANSWER: &[u8] = b"\x1b_G";
/// The MIME type a clipboard read is served (text only, as `ngterm-term` serves it).
const TEXT: &str = "text/plain";

/// The answer a delegated request got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// The clipboard's text; `None` when it holds none.
    Text(Option<String>),
    Denied,
}

impl From<DelegatedReply> for Answer {
    fn from(reply: DelegatedReply) -> Self {
        match reply {
            DelegatedReply::Clipboard { text } => Self::Text(text),
            DelegatedReply::Denied => Self::Denied,
        }
    }
}

/// What the server's terminal answers frontend-dependent queries with: the owner's [`ReplyMetadata`] over the
/// stable defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub foreground: vt::Rgb,
    pub background: vt::Rgb,
    /// `None`: the foreground answers for it (Ghostty's `colorForXterm`).
    pub cursor: Option<vt::Rgb>,
    /// `None`: the library's palette.
    pub palette: Option<Box<[vt::Rgb; 256]>>,
    pub cell_width_px: u32,
    pub cell_height_px: u32,
    pub color_scheme: ColorScheme,
}

impl Default for Facts {
    fn default() -> Self {
        Self::of(None)
    }
}

impl Facts {
    /// `metadata`'s facts, each value it leaves out (or a palette of another length than 256) taken from the stable
    /// defaults.
    #[must_use]
    pub fn of(metadata: Option<&ReplyMetadata>) -> Self {
        let palette = metadata
            .and_then(|metadata| <[Rgb; 256]>::try_from(metadata.palette.as_slice()).ok())
            .map(|palette| Box::new(palette.map(engine)));
        Self {
            foreground: engine(metadata.and_then(|m| m.foreground).unwrap_or(DEFAULT_FOREGROUND)),
            background: engine(metadata.and_then(|m| m.background).unwrap_or(DEFAULT_BACKGROUND)),
            cursor: metadata.and_then(|m| m.cursor).map(engine),
            palette,
            cell_width_px: metadata.map_or(0, |m| m.cell_width_px),
            cell_height_px: metadata.map_or(0, |m| m.cell_height_px),
            color_scheme: metadata.and_then(|m| m.color_scheme).unwrap_or(DEFAULT_COLOR_SCHEME),
        }
    }

    /// Apply these facts to `terminal`, a grid of `cols` x `rows`, where it holds `old` (`None`: everything).
    ///
    /// # Errors
    ///
    /// The library refused a value.
    pub fn apply(
        &self,
        terminal: &mut vt::Terminal,
        old: Option<&Self>,
        cols: u16,
        rows: u16,
    ) -> Result<(), vt::Error> {
        let colors = |facts: &Self| (facts.foreground, facts.background, facts.cursor);
        if old.is_none_or(|old| colors(old) != colors(self)) {
            terminal.set_default_colors(Some(self.foreground), Some(self.background), self.cursor)?;
        }
        if old.is_none_or(|old| old.palette != self.palette) {
            terminal.set_default_palette(self.palette.as_deref())?;
        }
        let cell = |facts: &Self| (facts.cell_width_px, facts.cell_height_px);
        if old.map_or(cell(self) != (0, 0), |old| cell(old) != cell(self)) {
            terminal.resize(cols, rows, self.cell_width_px, self.cell_height_px)?;
        }
        Ok(())
    }
}

fn engine(rgb: Rgb) -> vt::Rgb {
    vt::Rgb::new(rgb.r, rgb.g, rgb.b)
}

fn scheme(scheme: ColorScheme) -> vt::ColorScheme {
    match scheme {
        ColorScheme::Light => vt::ColorScheme::Light,
        ColorScheme::Dark => vt::ColorScheme::Dark,
    }
}

/// The device attributes Ghostty reports (`termio/stream_handler.zig`), as `ngterm-term` reports them: a VT220
/// (level 2) with colour text and clipboard access; DA2 `1;10;0`.
fn device_attributes() -> vt::DeviceAttributes {
    vt::DeviceAttributes {
        conformance_level: 62,
        features: vec![22, 52],
        device_type: 1,
        firmware_version: 10,
        rom_cartridge: 0,
        unit_id: 0,
    }
}

/// An answer armed for the next dispatch byte.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Armed {
    location: vt::ClipboardLocation,
    scope: ReadScope,
    answer: Answer,
}

/// A Kitty graphics command the reply owner's terminal answers ([`State::barrier`]): its answers go to the program in
/// its place among the server's replies, which are held behind it until it and every barrier before it ended.
#[derive(Debug)]
struct Barrier {
    id: u64,
    /// The client it was sent to, in its epoch then.
    client: ClientId,
    epoch: u64,
    deadline: Instant,
    /// Its answers are dropped: the command was printed ([`Pane::print`](super::pane::Pane::print)), not the
    /// program's.
    discard: bool,
    /// The owner's answers once it ended (empty when it ended otherwise); `None` while it waits.
    answers: Option<Vec<u8>>,
    /// The server's replies to what followed the command.
    held: Vec<Vec<u8>>,
}

/// The replies for the program in order: those ready to be written, then the barriers waiting, each with the server's
/// replies held behind it.
#[derive(Debug, Default)]
struct Sequence {
    ready: Vec<Vec<u8>>,
    barriers: VecDeque<Barrier>,
    /// The bytes held behind barriers.
    held: usize,
    /// Replies are dropped (to bytes a print fed, which the program never asked about).
    discard: bool,
}

impl Sequence {
    fn push(&mut self, data: &[u8]) {
        if self.discard {
            return;
        }
        let Some(last) = self.barriers.back_mut() else {
            self.ready.push(data.to_vec());
            return;
        };
        last.held.push(data.to_vec());
        self.held += data.len();
        if self.held > HELD_LIMIT {
            tracing::warn!(
                held = self.held,
                "too many replies wait for Kitty answers; the waiting barriers end empty"
            );
            self.end(|_| true);
        }
    }

    /// End every waiting barrier `which` picks, empty; then make ready what ended.
    fn end(&mut self, which: impl Fn(&Barrier) -> bool) {
        for barrier in &mut self.barriers {
            if barrier.answers.is_none() && which(barrier) {
                barrier.answers = Some(Vec::new());
            }
        }
        self.drain();
    }

    /// The barriers that ended, from the front up to the first one waiting, made ready with the replies they held.
    fn drain(&mut self) {
        while let Some(barrier) = self.barriers.pop_front() {
            let Some(answers) = barrier.answers else {
                self.barriers.push_front(barrier);
                return;
            };
            if !answers.is_empty() && !barrier.discard {
                self.ready.push(answers);
            }
            self.held -= barrier.held.iter().map(Vec::len).sum::<usize>();
            self.ready.extend(barrier.held);
        }
    }
}

/// What the server's terminal's effects share with its pane: the replies they queue for the program in order with the
/// reply owner's Kitty answers ([`Sequence`]), the clipboard answer armed for a dispatch byte, the appearance a
/// colour scheme query is answered with, and the clipboard writes the terminal completed, which a client that fell
/// behind gets back ([`super::recovery`]). The replies change under the pane's lock only.
#[derive(Debug, Default)]
pub struct Outlet {
    sequence: Mutex<Sequence>,
    armed: Mutex<Option<Armed>>,
    dark: AtomicBool,
    pub(super) written: Recorder,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Outlet {
    /// The replies ready to be written to the program, in order, taken. The pane's replies helper is the only one that
    /// takes them, and writes each batch before it takes the next, so they reach the program in the order they were
    /// made ready.
    pub fn take(&self) -> Vec<Vec<u8>> {
        std::mem::take(&mut lock(&self.sequence).ready)
    }

    /// The replies ready to be written, in order, left in place: what an update handoff carries to the new server,
    /// which writes them first, while this one keeps them for a rollback.
    #[must_use]
    pub fn ready_replies(&self) -> Vec<Vec<u8>> {
        lock(&self.sequence).ready.clone()
    }

    /// Whether replies are ready to be written.
    #[must_use]
    pub fn ready(&self) -> bool {
        !lock(&self.sequence).ready.is_empty()
    }

    /// Queue `data` for the program, behind every Kitty barrier waiting.
    pub fn push(&self, data: &[u8]) {
        lock(&self.sequence).push(data);
    }

    /// Drop the replies queued from now on (`true`), or queue them again (`false`).
    pub fn discard(&self, discard: bool) {
        lock(&self.sequence).discard = discard;
    }

    /// Hold the replies queued from now on behind `barrier`, until it ends.
    fn open(&self, barrier: Barrier) {
        let mut sequence = lock(&self.sequence);
        if sequence.barriers.len() >= BARRIER_LIMIT {
            tracing::warn!("too many Kitty barriers wait; they end empty");
            sequence.end(|_| true);
        }
        sequence.barriers.push_back(barrier);
    }

    /// End the barrier `id`, sent to `client` in its epoch `epoch`, with the owner's `answers`; whether it waited.
    fn complete(&self, id: u64, client: ClientId, epoch: u64, answers: Vec<u8>) -> bool {
        let mut sequence = lock(&self.sequence);
        let Some(barrier) = sequence.barriers.iter_mut().find(|barrier| {
            barrier.id == id && barrier.client == client && barrier.epoch == epoch && barrier.answers.is_none()
        }) else {
            return false;
        };
        barrier.answers = Some(answers);
        sequence.drain();
        true
    }

    /// End the waiting barriers empty: every one (`None`), or those sent to `client`.
    fn end(&self, client: Option<ClientId>) {
        lock(&self.sequence).end(|barrier| client.is_none_or(|client| barrier.client == client));
    }

    /// End the waiting barriers whose time is up at `now`, empty.
    fn expire(&self, now: Instant) {
        lock(&self.sequence).end(|barrier| barrier.deadline <= now);
    }

    /// When the next waiting barrier is to end; `None` with none waiting.
    fn deadline(&self) -> Option<Instant> {
        lock(&self.sequence)
            .barriers
            .iter()
            .filter(|barrier| barrier.answers.is_none())
            .map(|barrier| barrier.deadline)
            .min()
    }

    /// The appearance a colour scheme query is answered with from now on.
    pub fn set_scheme(&self, scheme: ColorScheme) {
        self.dark.store(scheme == ColorScheme::Dark, Ordering::Release);
    }

    fn scheme(&self) -> vt::ColorScheme {
        if self.dark.load(Ordering::Acquire) {
            vt::ColorScheme::Dark
        } else {
            vt::ColorScheme::Light
        }
    }

    /// Serve a clipboard read from the answer armed for it: only the read it was armed for (the same clipboard; a
    /// list-only answer never serves contents), once; with nothing armed (a read the scanner did not see), denied.
    fn answer(&self, request: &vt::ClipboardReadRequest<'_>) -> vt::ClipboardReadReply {
        let Some(armed) = lock(&self.armed).take() else {
            tracing::debug!("a clipboard read without an armed answer was denied");
            return vt::ClipboardReadReply::Denied;
        };
        if armed.location != request.location || (armed.scope == ReadScope::ListOnly && !request.mimes.is_empty()) {
            tracing::debug!(?armed, "a clipboard read other than the one armed for was denied");
            return vt::ClipboardReadReply::Denied;
        }
        let Answer::Text(text) = armed.answer else {
            return vt::ClipboardReadReply::Denied;
        };
        let contents = match text.as_deref() {
            Some(text) if request.mimes.contains(&TEXT.as_bytes()) => vec![vt::ClipboardContent {
                mime: TEXT.to_owned(),
                data: text.as_bytes().to_vec(),
            }],
            _ => Vec::new(),
        };
        let available = if request.list && text.is_some() {
            vec![TEXT.to_owned()]
        } else {
            Vec::new()
        };
        vt::ClipboardReadReply::Success { contents, available }
    }
}

/// The server terminal's effects: its answers go to the program (but its Kitty graphics answers, which the reply
/// owner's terminal gives), with Ghostty's identity; everything else is the clients' to show.
#[derive(Debug)]
pub struct ServerEffects {
    outlet: Arc<Outlet>,
    xtversion: String,
}

impl ServerEffects {
    #[must_use]
    pub fn new(outlet: Arc<Outlet>) -> Self {
        Self {
            outlet,
            xtversion: format!("ghostty {}", vt::version()),
        }
    }
}

impl vt::Effects for ServerEffects {
    fn write_pty(&mut self, data: &[u8]) {
        // Ghostty writes each Kitty answer alone (`stream_terminal.zig` `apcEnd`)
        if !data.starts_with(KITTY_ANSWER) {
            self.outlet.push(data);
        }
    }

    fn clipboard_read(&mut self, request: &vt::ClipboardReadRequest<'_>) -> vt::ClipboardReadReply {
        self.outlet.answer(request)
    }

    /// Recorded for the clients that fall behind on it ([`super::recovery`]), and refused as the default refuses it: the
    /// clients' terminals apply the write, each under its own policy.
    fn clipboard_write(&mut self, request: &vt::ClipboardWriteRequest<'_>) -> vt::ClipboardWriteResult {
        self.outlet.written.record(request);
        vt::ClipboardWriteResult::Denied
    }

    fn device_attributes(&mut self) -> Option<vt::DeviceAttributes> {
        Some(device_attributes())
    }

    fn xtversion(&mut self) -> Option<&str> {
        Some(&self.xtversion)
    }

    fn color_scheme(&mut self) -> Option<vt::ColorScheme> {
        Some(self.outlet.scheme())
    }
}

/// Whether `data` is one or more whole Kitty graphics answers (`ESC _ G … ESC \`), the only replies a client sends.
#[must_use]
pub fn is_kitty_reply(data: &[u8]) -> bool {
    let mut rest = data;
    while !rest.is_empty() {
        let Some(body) = rest.strip_prefix(KITTY_ANSWER) else {
            return false;
        };
        // an APC ends at its first ESC, which must be ST's
        let Some(end) = body.iter().position(|byte| *byte == 0x1b) else {
            return false;
        };
        if body.get(end + 1) != Some(&b'\\') {
            return false;
        }
        rest = &body[end + 2..];
    }
    !data.is_empty()
}

/// The selection an OSC 52 read names for `location`.
fn selection(location: vt::ClipboardLocation) -> &'static str {
    match location {
        vt::ClipboardLocation::Standard => "c",
        vt::ClipboardLocation::Selection => "s",
        vt::ClipboardLocation::Primary => "p",
    }
}

/// A clipboard read waiting for its answer.
#[derive(Debug, Clone)]
struct Pending {
    id: u64,
    /// The client it was sent to.
    client: ClientId,
    /// That client's epoch then.
    epoch: u64,
    read: ReadCandidate,
    deadline: Instant,
    /// The owner changed, or the client fell behind: denied by the helper thread.
    cancelled: bool,
}

/// The pane's replies: who owns them, its facts, and the clipboard read waiting for an answer with the output held
/// behind it.
#[derive(Debug)]
pub struct Replies {
    outlet: Arc<Outlet>,
    /// Wakes the helper thread; waited on with the pane's lock.
    timer: Arc<Condvar>,
    owner: Option<ClientId>,
    /// The session's reply-ownership changes, wrapping.
    generation: u32,
    metadata: HashMap<ClientId, ReplyMetadata>,
    /// What the terminal answers with now.
    facts: Facts,
    scanner: vt::Scanner,
    /// While a read waits: its dispatch byte, then the output read after it.
    parked: Vec<u8>,
    pending: Option<Pending>,
    next_id: u64,
    /// The program ended: every read is denied without asking, and no Kitty barrier opens.
    denied: bool,
    /// How long a clipboard read waits for the owner's answer.
    wait: Duration,
    /// How long a Kitty barrier waits for the owner's completion.
    kitty_wait: Duration,
}

impl Replies {
    /// The replies of a session whose creator `creator` owns them with `metadata`; the terminal answers with
    /// [`Replies::facts`]. A clipboard read waits `waits.0` for the owner's answer, a Kitty barrier `waits.1`.
    #[must_use]
    pub fn new(
        outlet: Arc<Outlet>,
        timer: Arc<Condvar>,
        creator: ClientId,
        metadata: ReplyMetadata,
        waits: (Duration, Duration),
    ) -> Self {
        Self::with_owner(outlet, timer, Some((creator, metadata)), waits)
    }

    /// The replies of a session no client owns yet (one an update handoff brought over, whose clients reattach): the
    /// terminal answers with the stable defaults until a client attaches and owns them. A clipboard read waits
    /// `waits.0` for the owner's answer, a Kitty barrier `waits.1`.
    #[must_use]
    pub fn ownerless(outlet: Arc<Outlet>, timer: Arc<Condvar>, waits: (Duration, Duration)) -> Self {
        Self::with_owner(outlet, timer, None, waits)
    }

    /// The replies owned by `owner` with its metadata, or by no client.
    fn with_owner(
        outlet: Arc<Outlet>,
        timer: Arc<Condvar>,
        owner: Option<(ClientId, ReplyMetadata)>,
        (wait, kitty_wait): (Duration, Duration),
    ) -> Self {
        let facts = Facts::of(owner.as_ref().map(|(_, metadata)| metadata));
        outlet.set_scheme(facts.color_scheme);
        Self {
            outlet,
            timer,
            owner: owner.as_ref().map(|(client, _)| *client),
            generation: 1,
            metadata: owner.into_iter().collect(),
            facts,
            scanner: vt::Scanner::default(),
            parked: Vec::new(),
            pending: None,
            next_id: 1,
            denied: false,
            wait,
            kitty_wait,
        }
    }

    #[must_use]
    pub fn facts(&self) -> &Facts {
        &self.facts
    }

    #[must_use]
    pub fn owner(&self) -> Option<ClientId> {
        self.owner
    }

    #[must_use]
    pub fn generation(&self) -> u32 {
        self.generation
    }

    /// The output held behind a waiting read.
    #[must_use]
    pub fn parked(&self) -> usize {
        self.parked.len()
    }

    /// When the waiting read is to be denied or the next Kitty barrier to end; `None` with neither waiting.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        let read = self.pending.as_ref().map(|pending| pending.deadline);
        match (read, self.outlet.deadline()) {
            (Some(read), Some(barrier)) => Some(read.min(barrier)),
            (read, barrier) => read.or(barrier),
        }
    }

    /// Whether the waiting read is to be denied now.
    #[must_use]
    pub fn due(&self, now: Instant) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|pending| pending.cancelled || pending.deadline <= now)
    }

    /// `bytes` were fed to the terminal outside the program's output (a print): the scanner follows them, so it
    /// stays in step with the terminal's parser; a read among them is not delegated (the terminal finds nothing
    /// armed for it and denies it, to no program). Whether they held a Kitty graphics command.
    pub fn observe(&mut self, mut bytes: &[u8]) -> bool {
        let mut kitty = false;
        while let Some((at, stop)) = self.scanner.next(bytes) {
            kitty |= stop == Stop::KittyGraphics;
            bytes = &bytes[at + 1..];
        }
        kitty
    }

    fn cancel(&mut self) {
        if let Some(pending) = &mut self.pending {
            pending.cancelled = true;
            self.timer.notify_all();
        }
    }
}

impl State {
    /// A read of the PTY: held behind a waiting read, else fed to the terminal and the clients up to the next
    /// clipboard read, which is delegated. Reads pause while [`PARK_HIGH`] bytes or more are held.
    pub(super) fn take_output(&mut self, bytes: &[u8], pause: &ReadPause) {
        if self.replies.pending.is_some() {
            self.replies.parked.extend_from_slice(bytes);
        } else {
            self.pump(bytes);
        }
        if self.replies.parked.len() >= PARK_HIGH {
            pause.pause();
        }
    }

    /// Feed `bytes` up to the next clipboard read the owner is to answer, answering the others (denied) at once;
    /// the dispatch byte of a delegated one and everything after it are held. A Kitty graphics command opens a barrier
    /// right after the byte that runs it.
    fn pump(&mut self, mut bytes: &[u8]) {
        while let Some((at, stop)) = self.replies.scanner.next(bytes) {
            let read = match stop {
                Stop::KittyGraphics => {
                    self.feed(&bytes[..=at]);
                    self.barrier(false);
                    bytes = &bytes[at + 1..];
                    continue;
                }
                Stop::Read(read) => read,
            };
            self.feed(&bytes[..at]);
            let (byte, rest) = (bytes[at], &bytes[at + 1..]);
            if let Some(answer) = self.delegate(read) {
                self.dispatch(byte, read, answer);
                bytes = rest;
            } else {
                self.replies.parked.push(byte);
                self.replies.parked.extend_from_slice(rest);
                return;
            }
        }
        self.feed(bytes);
    }

    /// The output fed so far ended with a Kitty graphics command, which the reply owner's terminal answers: a barrier
    /// sent to it, after that output, behind which the server's replies to what follows wait for its answers (dropped
    /// when `discard`). None while no owner shows Kitty graphics, the owner would not get it before its resync, or
    /// once the program ended.
    pub(super) fn barrier(&mut self, discard: bool) {
        let replies = &mut self.replies;
        let shows = |owner: &ClientId| replies.metadata.get(owner).is_some_and(|m| m.kitty_graphics);
        let Some(client) = replies.owner.filter(|owner| !replies.denied && shows(owner)) else {
            return;
        };
        let id = replies.next_id;
        replies.next_id += 1;
        let Some(epoch) = self.clients.barrier(client, id) else {
            return;
        };
        self.replies.outlet.open(Barrier {
            id,
            client,
            epoch,
            deadline: Instant::now() + self.replies.kitty_wait,
            discard,
            answers: None,
            held: Vec::new(),
        });
        self.replies.timer.notify_all();
    }

    /// The client `client`'s completion of the Kitty barrier `id`, sent in its epoch `epoch`, with its terminal's
    /// `answers`: taken, ending the barrier, only when that barrier was sent to it in that epoch, still waits, and the
    /// client has not fallen behind since; answers that are not whole Kitty answers ([`is_kitty_reply`]) end it empty
    /// and are dropped. Whether they were taken.
    pub(super) fn complete(&mut self, client: ClientId, id: u64, epoch: u64, answers: &[u8]) -> bool {
        if self.clients.epoch(client) != Some(epoch) || self.clients.overflowed(client) != Some(false) {
            return false;
        }
        if !answers.is_empty() && !is_kitty_reply(answers) {
            self.replies.outlet.complete(id, client, epoch, Vec::new());
            return false;
        }
        self.replies.outlet.complete(id, client, epoch, answers.to_vec())
    }

    /// End what is due at `now`: the waiting read (denied) and the Kitty barriers whose time is up (empty).
    pub(super) fn expire(&mut self, now: Instant) {
        if self.replies.due(now) {
            self.settle(Answer::Denied);
        }
        self.replies.outlet.expire(now);
    }

    /// The program's output `chunk` into the terminal and every client.
    fn feed(&mut self, chunk: &[u8]) {
        if chunk.is_empty() {
            return;
        }
        self.terminal.vt_write(chunk);
        self.advance(chunk);
    }

    /// `chunk`, just written to the terminal, at the next offset for every client; the revision moves, and the
    /// clipboard writes the terminal completed in it are kept for the clients that fall behind on it.
    pub(super) fn advance(&mut self, chunk: &[u8]) {
        let offset = self.offset;
        self.offset += chunk.len() as u64;
        self.revision = self.revision.wrapping_add(1);
        let written = self.replies.outlet.written.take();
        if !written.is_empty() {
            self.recovery.completed(written, self.offset);
        }
        self.prune();
        self.clients.output(offset, chunk);
    }

    /// A read's dispatch byte, fed alone with `answer` armed for it, then disarmed whatever the terminal did.
    fn dispatch(&mut self, byte: u8, read: ReadCandidate, answer: Answer) {
        *lock(&self.replies.outlet.armed) = Some(Armed {
            location: read.location,
            scope: read.scope,
            answer,
        });
        self.terminal.vt_write(&[byte]);
        if lock(&self.replies.outlet.armed).take().is_some() {
            tracing::debug!("a clipboard read candidate was not dispatched; its answer is dropped");
        }
        self.advance(&[byte]);
    }

    /// Send `read` to the reply owner, to wait for its answer (`None`), or the answer it gets at once: denied with no
    /// owner, an owner that would not get the request before its resync, or once the program ended.
    fn delegate(&mut self, read: ReadCandidate) -> Option<Answer> {
        let replies = &mut self.replies;
        let Some(client) = replies.owner.filter(|_| !replies.denied) else {
            return Some(Answer::Denied);
        };
        let id = replies.next_id;
        replies.next_id += 1;
        let request = DelegatedRequest::ClipboardRead {
            selection: selection(read.location).to_owned(),
            list_only: read.scope == ReadScope::ListOnly,
        };
        let Some(epoch) = self.clients.delegate(client, id, request) else {
            return Some(Answer::Denied);
        };
        self.replies.pending = Some(Pending {
            id,
            client,
            epoch,
            read,
            deadline: Instant::now() + self.replies.wait,
            cancelled: false,
        });
        self.replies.timer.notify_all();
        None
    }

    /// End the waiting read with `answer`: its dispatch byte goes in with it, then the output held behind it, which
    /// may stop at the next read. Nothing when none waits.
    pub(super) fn settle(&mut self, answer: Answer) {
        let Some(pending) = self.replies.pending.take() else {
            return;
        };
        let parked = std::mem::take(&mut self.replies.parked);
        if let Some((&byte, rest)) = parked.split_first() {
            self.dispatch(byte, pending.read, answer);
            self.pump(rest);
        }
        self.replies.timer.notify_all();
    }

    /// The program ended: the waiting read and every later one are denied without asking, so its last output goes
    /// through, and the Kitty barriers end.
    pub(super) fn deny_from_now(&mut self) {
        self.replies.denied = true;
        self.settle(Answer::Denied);
        self.replies.outlet.end(None);
    }

    /// Before an update handoff's cut: the waiting read denied, with the output held behind it fed (any read in it
    /// denied too, without asking), and every Kitty barrier ended empty, so nothing waits on a client and every reply
    /// is ready ([`Outlet::ready_replies`]). Later reads are asked for again as before (a rollback keeps the session).
    pub(super) fn settle_for_handoff(&mut self) {
        let denied = std::mem::replace(&mut self.replies.denied, true);
        self.settle(Answer::Denied);
        self.replies.denied = denied;
        self.replies.outlet.end(None);
    }

    /// The answer the client `client` gave to the request `id` in its epoch `epoch`: taken, ending the waiting read,
    /// only when that read was sent to it in that epoch, still waits, and the client has not fallen behind since.
    pub(super) fn answer(&mut self, client: ClientId, id: u64, epoch: u64, answer: Answer) -> bool {
        let waits = self.replies.pending.as_ref().is_some_and(|pending| {
            pending.id == id && pending.client == client && pending.epoch == epoch && !pending.cancelled
        });
        if !waits || self.clients.epoch(client) != Some(epoch) || self.clients.overflowed(client) != Some(false) {
            return false;
        }
        self.settle(answer);
        true
    }

    /// The client `client` fell behind: a read waiting for its answer is denied (by the helper thread), and its Kitty
    /// barriers end empty, since the request and the barriers went with the output it was not sent.
    pub(super) fn fell_behind(&mut self, client: ClientId) {
        if self
            .replies
            .pending
            .as_ref()
            .is_some_and(|pending| pending.client == client)
        {
            self.replies.cancel();
        }
        self.replies.outlet.end(Some(client));
    }

    /// The facts of the client `client` (attached) for the replies; they count while it owns them. An owner that no
    /// longer shows Kitty graphics ends its barriers empty.
    pub(super) fn remember(&mut self, client: ClientId, metadata: ReplyMetadata) {
        if self.lead.role(client).is_none() {
            return;
        }
        if !metadata.kitty_graphics {
            self.replies.outlet.end(Some(client));
        }
        self.replies.metadata.insert(client, metadata);
        self.apply_facts();
    }

    /// The client `id` just attached: told it does not own the replies, unless it does and was told so.
    pub(super) fn introduce(&mut self, id: ClientId) {
        if self.replies.owner != Some(id) {
            self.clients.reply_owner(id, false, self.replies.generation);
        }
    }

    /// After a change of the lead: the reply owner it makes, told (the leaving owner and the new one), its facts
    /// applied, a waiting read denied (by the helper thread) and the Kitty barriers ended empty when the owner
    /// changed; the facts of clients gone forgotten.
    pub(super) fn refresh_owner(&mut self) {
        let lead = &self.lead;
        self.replies.metadata.retain(|id, _| lead.role(*id).is_some());
        let owner = self.lead.reply_owner();
        if owner != self.replies.owner {
            let generation = self.replies.generation.wrapping_add(1);
            self.replies.generation = generation;
            if let Some(old) = self.replies.owner {
                self.clients.reply_owner(old, false, generation);
            }
            if let Some(new) = owner {
                self.clients.reply_owner(new, true, generation);
            }
            self.replies.owner = owner;
            self.replies.cancel();
            self.replies.outlet.end(None);
            self.replies.timer.notify_all();
        }
        self.apply_facts();
    }

    /// The owner's facts onto the terminal when they changed; a program that asked for colour scheme reports (mode
    /// 2031) gets one when the appearance changed, as Ghostty's surface sends it.
    fn apply_facts(&mut self) {
        let owner = self.replies.owner.and_then(|id| self.replies.metadata.get(&id));
        let facts = Facts::of(owner);
        if facts == self.replies.facts {
            return;
        }
        let old = std::mem::replace(&mut self.replies.facts, facts);
        let facts = &self.replies.facts;
        if let Err(err) = facts.apply(&mut self.terminal, Some(&old), self.size.cols, self.size.rows) {
            tracing::warn!(%err, "the reply owner's facts were not all applied to the session's terminal");
        }
        if old.color_scheme != facts.color_scheme {
            self.replies.outlet.set_scheme(facts.color_scheme);
            if self.terminal.mode(vt::Mode::COLOR_SCHEME_REPORT) {
                self.replies.outlet.push(scheme(facts.color_scheme).report());
            }
        }
        // what the terminal answered (a mode 2048 report too) is written by the helper thread
        self.replies.timer.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use vt::Effects as _;

    use super::*;

    #[test]
    fn a_reply_is_whole_kitty_answers_and_nothing_else() {
        assert!(is_kitty_reply(b"\x1b_Gi=1;OK\x1b\\"));
        assert!(is_kitty_reply(b"\x1b_Gi=1;OK\x1b\\\x1b_Gi=2;ENOENT:gone\x1b\\"));
        for refused in [
            &b""[..],
            b"\x1b_Gi=1;OK",
            b"\x1b_Gi=1;OK\x1b",
            b"\x1b_Gi=1;O\x1bK\x1b\\",
            b"\x1b_Gi=1;OK\x1b\\typed",
            b"\x1b]52;c;?\x07",
            b"\x1b[c",
            b"\x1b_25a1;s\x1b\\",
        ] {
            assert!(!is_kitty_reply(refused), "{refused:?}");
        }
    }

    #[test]
    fn the_facts_are_the_owners_over_the_defaults() {
        let defaults = Facts::of(None);
        assert_eq!(defaults, Facts::default());
        assert_eq!(defaults.foreground, engine(DEFAULT_FOREGROUND));
        assert_eq!(defaults.background, engine(DEFAULT_BACKGROUND));
        assert_eq!((defaults.cursor, defaults.palette.is_none()), (None, true));
        assert_eq!((defaults.cell_width_px, defaults.cell_height_px), (0, 0));
        assert_eq!(defaults.color_scheme, DEFAULT_COLOR_SCHEME);
        let red = Rgb { r: 0xff, g: 0, b: 0 };
        let mut metadata = ReplyMetadata {
            background: Some(red),
            palette: vec![red; 255],
            cell_width_px: 8,
            cell_height_px: 17,
            color_scheme: Some(ColorScheme::Dark),
            ..ReplyMetadata::default()
        };
        let facts = Facts::of(Some(&metadata));
        assert_eq!(facts.foreground, engine(DEFAULT_FOREGROUND), "left out: the default");
        assert_eq!(facts.background, engine(red));
        assert!(facts.palette.is_none(), "a palette of 255 entries counts as none");
        assert_eq!((facts.cell_width_px, facts.cell_height_px), (8, 17));
        assert_eq!(facts.color_scheme, ColorScheme::Dark);
        metadata.palette.push(red);
        assert_eq!(
            Facts::of(Some(&metadata)).palette.map(|palette| palette[255]),
            Some(engine(red))
        );
    }

    #[test]
    fn the_terminal_answers_with_the_facts_applied() {
        let mut terminal = vt::Terminal::new(80, 24).unwrap();
        let facts = Facts::of(Some(&ReplyMetadata {
            foreground: Some(Rgb { r: 1, g: 2, b: 3 }),
            cell_width_px: 9,
            cell_height_px: 18,
            ..ReplyMetadata::default()
        }));
        facts.apply(&mut terminal, None, 80, 24).unwrap();
        let colors = terminal.default_colors();
        assert_eq!(colors.foreground, Some(vt::Rgb::new(1, 2, 3)));
        assert_eq!(colors.background, Some(engine(DEFAULT_BACKGROUND)));
        assert_eq!(colors.cursor, None);
        let outlet = Arc::new(Outlet::default());
        terminal
            .set_effects(Box::new(ServerEffects::new(Arc::clone(&outlet))))
            .unwrap();
        terminal.vt_write(b"\x1b[14t");
        assert_eq!(outlet.take(), [b"\x1b[4;432;720t".to_vec()]);
        // back to the defaults: no pixel size
        Facts::default().apply(&mut terminal, Some(&facts), 80, 24).unwrap();
        terminal.vt_write(b"\x1b[14t");
        assert_eq!(outlet.take(), [b"\x1b[4;0;0t".to_vec()]);
    }

    #[test]
    fn the_server_drops_its_own_kitty_answers_and_keeps_the_rest() {
        let outlet = Arc::new(Outlet::default());
        let mut effects = ServerEffects::new(Arc::clone(&outlet));
        effects.write_pty(b"\x1b_Gi=1;OK\x1b\\");
        effects.write_pty(b"\x1b[?62;22;52c");
        effects.write_pty(b"\x1b_25a1;s;fmt=\x1b\\");
        assert_eq!(
            outlet.take(),
            [b"\x1b[?62;22;52c".to_vec(), b"\x1b_25a1;s;fmt=\x1b\\".to_vec()]
        );
        assert_eq!(effects.device_attributes().map(|da| da.conformance_level), Some(62));
        assert_eq!(effects.color_scheme(), Some(vt::ColorScheme::Light));
        outlet.set_scheme(ColorScheme::Dark);
        assert_eq!(effects.color_scheme(), Some(vt::ColorScheme::Dark));
        assert!(
            effects
                .xtversion()
                .is_some_and(|version| version.starts_with("ghostty "))
        );
    }

    fn barrier(id: u64, client: ClientId, deadline: Instant) -> Barrier {
        Barrier {
            id,
            client,
            epoch: 1,
            deadline,
            discard: false,
            answers: None,
            held: Vec::new(),
        }
    }

    fn chunks(list: &[&[u8]]) -> Vec<Vec<u8>> {
        list.iter().map(|chunk| chunk.to_vec()).collect()
    }

    #[test]
    fn replies_wait_behind_barriers_and_go_in_order_once_those_before_them_ended() {
        let outlet = Outlet::default();
        let later = Instant::now() + Duration::from_secs(60);
        outlet.push(b"a");
        outlet.open(barrier(1, 7, later));
        outlet.push(b"b");
        outlet.open(barrier(2, 7, later));
        outlet.push(b"c");
        assert_eq!(outlet.take(), chunks(&[b"a"]));
        assert_eq!(outlet.deadline(), Some(later));
        // the second ends first: kept until the first ends
        assert!(outlet.complete(2, 7, 1, b"K2".to_vec()));
        assert!(!outlet.ready());
        // another client's, another epoch's, an unknown barrier's, a second completion: refused
        assert!(!outlet.complete(1, 8, 1, Vec::new()));
        assert!(!outlet.complete(1, 7, 2, Vec::new()));
        assert!(!outlet.complete(3, 7, 1, Vec::new()));
        assert!(!outlet.complete(2, 7, 1, Vec::new()));
        assert!(outlet.complete(1, 7, 1, b"K1".to_vec()));
        assert_eq!(outlet.take(), chunks(&[b"K1", b"b", b"K2", b"c"]));
        assert_eq!(outlet.deadline(), None);
    }

    #[test]
    fn a_printed_commands_answers_and_a_prints_replies_are_dropped() {
        let outlet = Outlet::default();
        let later = Instant::now() + Duration::from_secs(60);
        // an empty completion adds nothing; a printed command's answers are dropped
        let mut printed = barrier(3, 7, later);
        printed.discard = true;
        outlet.open(printed);
        outlet.push(b"d");
        outlet.open(barrier(4, 7, later));
        assert!(outlet.complete(3, 7, 1, b"K3".to_vec()));
        assert!(outlet.complete(4, 7, 1, Vec::new()));
        assert_eq!(outlet.take(), chunks(&[b"d"]));
        // replies discarded meanwhile are never queued
        outlet.discard(true);
        outlet.push(b"printed");
        outlet.discard(false);
        assert!(!outlet.ready());
    }

    #[test]
    fn barriers_end_empty_at_their_deadline_for_their_client_or_past_the_limits() {
        let outlet = Outlet::default();
        let now = Instant::now();
        let later = now + Duration::from_secs(60);
        outlet.open(barrier(1, 7, now));
        outlet.push(b"a");
        outlet.open(barrier(2, 7, later));
        outlet.push(b"b");
        assert_eq!(outlet.deadline(), Some(now));
        outlet.expire(now);
        assert_eq!(outlet.take(), chunks(&[b"a"]));
        assert!(!outlet.complete(1, 7, 1, b"late".to_vec()), "a late completion");
        // another client's end leaves it; its own ends it
        outlet.end(Some(8));
        assert!(!outlet.ready());
        outlet.end(Some(7));
        assert_eq!(outlet.take(), chunks(&[b"b"]));
        // past the held bytes
        outlet.open(barrier(3, 7, later));
        outlet.push(&vec![b'x'; HELD_LIMIT]);
        assert!(!outlet.ready());
        outlet.push(b"y");
        assert_eq!(outlet.take().len(), 2);
        assert_eq!(outlet.deadline(), None);
        // past the barriers: those waiting end, the new one waits
        for id in 0..BARRIER_LIMIT as u64 {
            outlet.open(barrier(10 + id, 7, later));
        }
        outlet.push(b"z");
        outlet.open(barrier(1, 7, later));
        assert_eq!(outlet.take(), chunks(&[b"z"]));
        outlet.push(b"behind");
        assert!(!outlet.ready());
        outlet.end(None);
        assert_eq!(outlet.take(), chunks(&[b"behind"]));
    }

    fn read(location: vt::ClipboardLocation, mimes: Vec<&[u8]>, list: bool) -> vt::ClipboardReadRequest<'_> {
        vt::ClipboardReadRequest {
            location,
            mimes,
            list,
            name: b"",
            granted: false,
        }
    }

    fn arm(outlet: &Outlet, location: vt::ClipboardLocation, scope: ReadScope, answer: Answer) {
        *lock(&outlet.armed) = Some(Armed {
            location,
            scope,
            answer,
        });
    }

    #[test]
    fn an_armed_answer_serves_the_read_it_was_armed_for_once() {
        let outlet = Outlet::default();
        let standard = vt::ClipboardLocation::Standard;
        let text = Answer::Text(Some("hi".to_owned()));
        assert_eq!(
            outlet.answer(&read(standard, vec![b"text/plain"], false)),
            vt::ClipboardReadReply::Denied
        );
        arm(&outlet, standard, ReadScope::Data, text.clone());
        let served = vt::ClipboardReadReply::Success {
            contents: vec![vt::ClipboardContent {
                mime: TEXT.to_owned(),
                data: b"hi".to_vec(),
            }],
            available: Vec::new(),
        };
        assert_eq!(outlet.answer(&read(standard, vec![b"text/plain"], false)), served);
        assert_eq!(
            outlet.answer(&read(standard, vec![b"text/plain"], false)),
            vt::ClipboardReadReply::Denied,
            "once"
        );
        // another clipboard, a list-only answer asked for contents, a denial
        arm(&outlet, vt::ClipboardLocation::Primary, ReadScope::Data, text.clone());
        assert_eq!(
            outlet.answer(&read(standard, vec![b"text/plain"], false)),
            vt::ClipboardReadReply::Denied
        );
        arm(&outlet, standard, ReadScope::ListOnly, text.clone());
        assert_eq!(
            outlet.answer(&read(standard, vec![b"text/plain"], true)),
            vt::ClipboardReadReply::Denied
        );
        arm(&outlet, standard, ReadScope::ListOnly, text);
        let listed = vt::ClipboardReadReply::Success {
            contents: Vec::new(),
            available: vec![TEXT.to_owned()],
        };
        assert_eq!(outlet.answer(&read(standard, Vec::new(), true)), listed);
        arm(&outlet, standard, ReadScope::Data, Answer::Denied);
        assert_eq!(
            outlet.answer(&read(standard, vec![b"text/plain"], false)),
            vt::ClipboardReadReply::Denied
        );
    }

    #[test]
    fn a_read_names_its_selection_and_an_answer_its_text() {
        assert_eq!(selection(vt::ClipboardLocation::Standard), "c");
        assert_eq!(selection(vt::ClipboardLocation::Selection), "s");
        assert_eq!(selection(vt::ClipboardLocation::Primary), "p");
        let text = DelegatedReply::Clipboard { text: None };
        assert_eq!(Answer::from(text), Answer::Text(None));
        assert_eq!(Answer::from(DelegatedReply::Denied), Answer::Denied);
    }
}
