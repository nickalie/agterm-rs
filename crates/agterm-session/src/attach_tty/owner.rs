//! The attach client between the session and the outer terminal: the session's output filtered on its way out
//! (`super::filter`), the client's own sequences written where they split nothing of the program's, and the reply
//! owner's part (`server::replies`): the facts the server answers the program's queries with taken from the outer
//! terminal, and the requests only the owner answers relayed to it.
//!
//! # What the client writes of its own
//!
//! - The role title of a managed attach (`server::lead::role_title`), the newest replacing one not yet written: zmx's
//!   relay (`role.zig` of `build/patches/zmx/0001-explicit-leadership.patch`). A sequence that stays unfinished for
//!   [`INJECT_WAIT`] is ended with CAN, which only a terminal whose client is changing role or relaying sees.
//! - A probe at the start, when the client runs in a terminal: OSC 10, 11 and 12, OSC 4 for the 256 colours, `CSI 16
//!   t` (the cell's pixels), `CSI ? 996 n` (dark or light), a Kitty graphics query, and DA1 last, whose answer (the
//!   fence) says every answer before it is in. Mode 2031 is turned on in the outer terminal, so a change of its
//!   appearance is reported, and the colours are asked for again then.
//! - A DA1 fence after each Kitty graphics command the client owns the answer to: the outer terminal answers the
//!   command first, then DA1, so the Kitty answers read before the fence's are that command's (and its earlier ones'),
//!   which complete its barrier. The fence goes after the command's ST (the barrier comes when the output stands at
//!   its ESC, so it is the first ground place).
//! - For a delegated clipboard read, an OSC 52 read of the outer terminal's clipboard and a DA1 fence: what the outer
//!   terminal answered before the fence's answer is the clipboard (denied without one).
//!
//! An outer terminal that answers no DA1 within [`PROBE_GIVE_UP`] gets no fences: barriers are completed empty at
//! once and clipboard reads denied, and the metadata says it shows no Kitty graphics.
//!
//! # The metadata
//!
//! The colours from the answers, the 256 colours only when all came; the cell's pixels from the window size the
//! terminal reports with the grid (`TIOCGWINSZ`), else from `CSI 16 t` (or `CSI 14 t` over the grid); the appearance;
//! Kitty graphics when the outer terminal answered the query and answers fences. It is sent when the probe's fence
//! comes, again when a refresh's does, and whenever the cell size changes.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::filter::OutputFilter;
use super::input::{Answer, Dynamic};
use crate::client::{Barrier, Delegated};
use crate::protocol::{ColorScheme, DelegatedReply, DelegatedRequest, GridSize, LeadRole, ReplyMetadata, Rgb};
use crate::server::lead::role_title;

/// How long the client's own sequences wait for the output to stand at ground before CAN ends what the program left
/// unfinished (zmx's `DEADLINE_MS`).
pub const INJECT_WAIT: Duration = Duration::from_millis(250);

/// How long the probe's fence may take before the outer terminal is taken to answer no fence.
pub const PROBE_GIVE_UP: Duration = Duration::from_secs(5);

/// The most fences awaited at once: past it, the outer terminal is taken to have stopped answering them.
pub const FENCE_LIMIT: usize = 4096;

/// The image id of the probe's Kitty graphics query.
pub const KITTY_PROBE_ID: u32 = 3_418_201;

/// DA1: the fence.
const FENCE: &[u8] = b"\x1b[c";

/// The colours asked for (OSC 10, 11, 12 and OSC 4 for every palette entry).
fn colour_queries() -> Vec<u8> {
    let mut bytes = b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b]12;?\x1b\\\x1b]4".to_vec();
    for index in 0..=255 {
        bytes.extend_from_slice(format!(";{index};?").as_bytes());
    }
    bytes.extend_from_slice(b"\x1b\\");
    bytes
}

/// The probe: the colours, the cell size, the appearance, Kitty graphics, then the fence.
fn probe_queries() -> Vec<u8> {
    let mut bytes = colour_queries();
    bytes.extend_from_slice(b"\x1b[16t\x1b[?996n");
    bytes.extend_from_slice(format!("\x1b_Gi={KITTY_PROBE_ID},s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\").as_bytes());
    bytes.extend_from_slice(FENCE);
    bytes
}

/// Turns the outer terminal's colour scheme reports on (mode 2031).
pub const SCHEME_REPORTS_ON: &[u8] = b"\x1b[?2031h";

/// What a fence's answer completes.
#[derive(Debug)]
enum Fence {
    Probe,
    /// The colours asked for again.
    Refresh,
    Kitty(Barrier),
    Clipboard(Delegated),
    /// One written for a connection that ended: its answer completes nothing.
    Stale,
}

/// Bytes the client writes of its own, and the fence they end with.
#[derive(Debug)]
struct Injection {
    bytes: Vec<u8>,
    fence: Option<Fence>,
}

/// The outer terminal's answer to a clipboard read.
#[derive(Debug)]
enum Clipboard {
    Data(Vec<u8>),
    /// Its payload was not base64.
    Unreadable,
}

/// What the client sends the server.
#[derive(Debug)]
pub enum Outgoing {
    Answer(Delegated, DelegatedReply),
    Complete(Barrier, Vec<u8>),
    Metadata(ReplyMetadata),
}

/// What handling an event produced: bytes for the outer terminal and messages for the server, in order.
#[derive(Debug, Default)]
pub struct Effects {
    pub stdout: Vec<u8>,
    pub outgoing: Vec<Outgoing>,
}

/// The outer terminal's facts as answered so far.
#[derive(Debug, Clone, Default)]
struct Draft {
    foreground: Option<Rgb>,
    background: Option<Rgb>,
    cursor: Option<Rgb>,
    palette: Vec<Option<Rgb>>,
    /// From `CSI 16 t`.
    cell: Option<(u32, u32)>,
    /// From `CSI 14 t`.
    text_area: Option<(u32, u32)>,
    scheme: Option<ColorScheme>,
}

/// The attach client's state between the session and the outer terminal (module docs).
#[derive(Debug)]
pub struct Relay {
    filter: OutputFilter,
    /// The managed attach's nonce, for its role titles.
    nonce: Option<String>,
    /// The newest role not yet written.
    role: Option<(LeadRole, u32)>,
    pending: Vec<Injection>,
    /// When the oldest of the client's own sequences waiting was queued.
    waiting_since: Option<Instant>,
    fences: VecDeque<Fence>,
    /// Whether the outer terminal answers fences; `None` while the probe's is awaited.
    fences_work: Option<bool>,
    probe_started: Option<Instant>,
    /// The outer terminal answered the Kitty graphics query.
    kitty: bool,
    /// Kitty answers read since the last Kitty fence.
    kitty_answers: Vec<u8>,
    /// The clipboard the outer terminal answered since the last clipboard fence.
    clipboard: Option<Clipboard>,
    draft: Draft,
    grid: GridSize,
    /// The cell's pixels from the window size.
    window_cell: Option<(u32, u32)>,
    /// The metadata the server holds.
    sent: ReplyMetadata,
}

impl Relay {
    /// The relay of a client attached under `nonce` (managed) or none, its outer terminal `grid` cells.
    #[must_use]
    pub fn new(nonce: Option<String>, grid: GridSize) -> Self {
        Self {
            filter: OutputFilter::new(),
            nonce,
            role: None,
            pending: Vec::new(),
            waiting_since: None,
            fences: VecDeque::new(),
            fences_work: None,
            probe_started: None,
            kitty: false,
            kitty_answers: Vec::new(),
            clipboard: None,
            draft: Draft {
                palette: vec![None; 256],
                ..Draft::default()
            },
            grid,
            window_cell: None,
            sent: ReplyMetadata::default(),
        }
    }

    /// Starts the probe of a client in a terminal (module docs); a client without one asks nothing and answers no
    /// fence.
    pub fn start(&mut self, terminal: bool, now: Instant, fx: &mut Effects) {
        if !terminal {
            self.fences_work = Some(false);
            return;
        }
        self.probe_started = Some(now);
        fx.stdout.extend_from_slice(SCHEME_REPORTS_ON);
        self.queue(probe_queries(), Some(Fence::Probe), now, fx);
    }

    /// Whether the probe's fence is still awaited.
    #[must_use]
    pub fn probing(&self) -> bool {
        self.fences_work.is_none()
    }

    /// Whether an answer of the outer terminal is due.
    #[must_use]
    pub fn awaiting(&self) -> bool {
        !self.fences.is_empty()
    }

    /// The metadata to attach with, taken as the server's from here on.
    pub fn attach_metadata(&mut self) -> ReplyMetadata {
        self.sent = self.metadata();
        self.sent.clone()
    }

    /// The metadata from what the outer terminal answered so far (module docs).
    #[must_use]
    pub fn metadata(&self) -> ReplyMetadata {
        let palette: Vec<Rgb> = self.draft.palette.iter().flatten().copied().collect();
        let (cell_width_px, cell_height_px) = self
            .window_cell
            .or(self.draft.cell)
            .or_else(|| {
                let (width, height) = self.draft.text_area?;
                let (cols, rows) = (u32::from(self.grid.cols), u32::from(self.grid.rows));
                (cols > 0 && rows > 0).then(|| (width / cols, height / rows))
            })
            .unwrap_or((0, 0));
        ReplyMetadata {
            foreground: self.draft.foreground,
            background: self.draft.background,
            cursor: self.draft.cursor,
            palette: if palette.len() == 256 { palette } else { Vec::new() },
            cell_width_px,
            cell_height_px,
            kitty_graphics: self.kitty && self.fences_work == Some(true),
            color_scheme: self.draft.scheme,
        }
    }

    fn send_metadata(&mut self, fx: &mut Effects) {
        let metadata = self.metadata();
        if metadata != self.sent {
            self.sent = metadata.clone();
            fx.outgoing.push(Outgoing::Metadata(metadata));
        }
    }

    /// The session's output (a snapshot's bytes too), on its way to the outer terminal.
    pub fn output(&mut self, bytes: &[u8], fx: &mut Effects) {
        let mut rest = bytes;
        while !rest.is_empty() {
            if !self.has_pending() {
                self.filter.feed(rest, &mut fx.stdout);
                return;
            }
            if self.filter.at_ground() {
                self.flush(fx);
                continue;
            }
            let taken = self.filter.feed_until_ground(rest, &mut fx.stdout);
            rest = &rest[taken..];
            if self.filter.at_ground() {
                self.flush(fx);
            }
        }
    }

    /// The client's role changed: its title is written at the next ground place.
    pub fn role(&mut self, role: LeadRole, generation: u32, now: Instant, fx: &mut Effects) {
        if self.nonce.is_none() {
            return;
        }
        self.role = Some((role, generation));
        self.waiting_since.get_or_insert(now);
        if self.filter.at_ground() {
            self.flush(fx);
        }
    }

    /// A Kitty graphics barrier: fenced in the outer terminal, or completed empty at once when it answers no fence.
    pub fn barrier(&mut self, barrier: Barrier, now: Instant, fx: &mut Effects) {
        if self.fences_work == Some(true) && self.kitty && self.fences.len() < FENCE_LIMIT {
            self.queue(FENCE.to_vec(), Some(Fence::Kitty(barrier)), now, fx);
        } else {
            fx.outgoing.push(Outgoing::Complete(barrier, Vec::new()));
        }
    }

    /// A delegated request: a clipboard read relayed to the outer terminal, or denied when it answers no fence.
    pub fn delegated(&mut self, request: Delegated, now: Instant, fx: &mut Effects) {
        if self.fences_work != Some(true) || self.fences.len() >= FENCE_LIMIT {
            fx.outgoing.push(Outgoing::Answer(request, DelegatedReply::Denied));
            return;
        }
        let DelegatedRequest::ClipboardRead { selection, .. } = &request.request;
        let selection = match selection.as_str() {
            "s" | "p" => selection.as_str(),
            _ => "c",
        };
        let mut bytes = format!("\x1b]52;{selection};?\x1b\\").into_bytes();
        bytes.extend_from_slice(FENCE);
        self.queue(bytes, Some(Fence::Clipboard(request)), now, fx);
    }

    /// An answer of the outer terminal.
    pub fn answer(&mut self, answer: Answer, now: Instant, fx: &mut Effects) {
        match answer {
            Answer::PrimaryAttributes => self.fence_answered(fx),
            Answer::Dynamic { which, color } => match which {
                Dynamic::Foreground => self.draft.foreground = color,
                Dynamic::Background => self.draft.background = color,
                Dynamic::Cursor => self.draft.cursor = color,
            },
            Answer::Palette { index, color } => self.draft.palette[usize::from(index)] = color,
            Answer::Clipboard(data) => self.clipboard = Some(data.map_or(Clipboard::Unreadable, Clipboard::Data)),
            Answer::ColorScheme(scheme) => {
                self.draft.scheme = Some(scheme);
                if !self.probing() && !self.fences.iter().any(|fence| matches!(fence, Fence::Refresh)) {
                    self.refresh(now, fx);
                }
            }
            Answer::CellSize { width, height } => {
                self.draft.cell = Some((width, height));
                self.settled_metadata(fx);
            }
            Answer::TextArea { width, height } => {
                self.draft.text_area = Some((width, height));
                self.settled_metadata(fx);
            }
            Answer::Kitty(bytes) => self.kitty_answer(&bytes),
        }
    }

    /// The outer terminal's grid changed, with its window's pixels when the terminal reports them.
    pub fn resize(&mut self, grid: GridSize, pixels: Option<(u32, u32)>, now: Instant, fx: &mut Effects) {
        self.grid = grid;
        let (cols, rows) = (u32::from(grid.cols), u32::from(grid.rows));
        self.window_cell = pixels
            .filter(|(width, height)| *width > 0 && *height > 0 && cols > 0 && rows > 0)
            .map(|(width, height)| (width / cols, height / rows));
        if self.window_cell.is_none() && self.fences_work == Some(true) {
            // the cell may have changed with the grid (a font size change): asked again, answered without a fence
            self.queue(b"\x1b[16t".to_vec(), None, now, fx);
        }
        self.settled_metadata(fx);
    }

    /// The metadata sent when it changed, unless the probe is still under way (its fence sends it).
    fn settled_metadata(&mut self, fx: &mut Effects) {
        if !self.probing() {
            self.send_metadata(fx);
        }
    }

    /// The client reattached on a new connection: the fences awaited complete nothing of it.
    pub fn reattached(&mut self) {
        for fence in &mut self.fences {
            if matches!(fence, Fence::Kitty(_) | Fence::Clipboard(_)) {
                *fence = Fence::Stale;
            }
        }
        self.pending
            .retain(|injection| !matches!(injection.fence, Some(Fence::Kitty(_) | Fence::Clipboard(_))));
        self.role = None;
        self.kitty_answers.clear();
        self.clipboard = None;
    }

    /// When [`Relay::tick`] is due next.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Instant> {
        let inject = self.waiting_since.map(|since| since + INJECT_WAIT);
        let probe = self
            .probe_started
            .filter(|_| self.probing())
            .map(|started| started + PROBE_GIVE_UP);
        inject.into_iter().chain(probe).min()
    }

    /// What is due at `now`: the client's own sequences written after CAN when the output stayed inside a sequence for
    /// [`INJECT_WAIT`]; the probe given up after [`PROBE_GIVE_UP`].
    pub fn tick(&mut self, now: Instant, fx: &mut Effects) {
        if self
            .waiting_since
            .is_some_and(|since| now.saturating_duration_since(since) >= INJECT_WAIT)
        {
            if !self.filter.at_ground() {
                self.filter.cancel(&mut fx.stdout);
            }
            self.flush(fx);
        }
        if self.probing()
            && self
                .probe_started
                .is_some_and(|started| now.saturating_duration_since(started) >= PROBE_GIVE_UP)
        {
            self.fences_work = Some(false);
            self.fences.clear();
            self.send_metadata(fx);
        }
    }

    fn has_pending(&self) -> bool {
        self.role.is_some() || !self.pending.is_empty()
    }

    /// Queues the client's `bytes`, written at the next ground place, with the fence they end with.
    fn queue(&mut self, bytes: Vec<u8>, fence: Option<Fence>, now: Instant, fx: &mut Effects) {
        self.pending.push(Injection { bytes, fence });
        self.waiting_since.get_or_insert(now);
        if self.filter.at_ground() {
            self.flush(fx);
        }
    }

    /// Writes the role title and the queued sequences, at ground.
    fn flush(&mut self, fx: &mut Effects) {
        if let (Some((role, generation)), Some(nonce)) = (self.role.take(), &self.nonce) {
            fx.stdout
                .extend_from_slice(role_title(nonce, role, generation).as_bytes());
        }
        for injection in std::mem::take(&mut self.pending) {
            fx.stdout.extend_from_slice(&injection.bytes);
            if let Some(fence) = injection.fence {
                self.fences.push_back(fence);
            }
        }
        self.waiting_since = None;
    }

    /// The colours asked for again (the outer terminal's appearance changed), behind a fence.
    fn refresh(&mut self, now: Instant, fx: &mut Effects) {
        if self.fences_work != Some(true) {
            return;
        }
        self.draft.foreground = None;
        self.draft.background = None;
        self.draft.cursor = None;
        self.draft.palette = vec![None; 256];
        let mut bytes = colour_queries();
        bytes.extend_from_slice(FENCE);
        self.queue(bytes, Some(Fence::Refresh), now, fx);
    }

    /// A fence's answer came: what it ends is complete.
    fn fence_answered(&mut self, fx: &mut Effects) {
        let Some(fence) = self.fences.pop_front() else {
            return;
        };
        match fence {
            Fence::Probe => {
                self.fences_work = Some(true);
                self.send_metadata(fx);
            }
            Fence::Refresh => self.send_metadata(fx),
            Fence::Kitty(barrier) => {
                let answers = std::mem::take(&mut self.kitty_answers);
                fx.outgoing.push(Outgoing::Complete(barrier, answers));
            }
            Fence::Clipboard(request) => {
                let reply = match self.clipboard.take() {
                    Some(Clipboard::Data(data)) if data.is_empty() => DelegatedReply::Clipboard { text: None },
                    Some(Clipboard::Data(data)) => DelegatedReply::Clipboard {
                        text: Some(String::from_utf8_lossy(&data).into_owned()),
                    },
                    Some(Clipboard::Unreadable) | None => DelegatedReply::Denied,
                };
                fx.outgoing.push(Outgoing::Answer(request, reply));
            }
            // the Kitty answers before it answered the ended connection's commands
            Fence::Stale => self.kitty_answers.clear(),
        }
    }

    /// A Kitty graphics answer: the probe's says the outer terminal shows Kitty graphics; another belongs to the next
    /// Kitty fence, or to nobody.
    fn kitty_answer(&mut self, bytes: &[u8]) {
        let control = bytes
            .get(3..)
            .and_then(|rest| rest.split(|byte| *byte == b';').next())
            .unwrap_or_default();
        let probe = format!("i={KITTY_PROBE_ID}");
        if control.split(|byte| *byte == b',').any(|key| key == probe.as_bytes()) {
            self.kitty = true;
        } else if self
            .fences
            .iter()
            .any(|fence| matches!(fence, Fence::Kitty(_) | Fence::Stale))
        {
            self.kitty_answers.extend_from_slice(bytes);
        }
    }
}

#[cfg(test)]
mod tests;
