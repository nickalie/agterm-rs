//! What a pane reports to its frontend ([`PaneEvent`]) and the [`vt::Effects`] that produce it.
//!
//! Every callback runs on the IO thread inside `vt_write`, with the pane's VT lock held. They copy what they need into
//! the event channel (unbounded: a send never blocks) and return; replies to the program go to the PTY writer's queue,
//! never a blocking write under the lock.
//!
//! ## Clipboard reads
//!
//! libghostty-vt asks for a clipboard read synchronously and takes "no answer" for an empty clipboard (OSC 52) or
//! `EPERM` (OSC 5522), while the prompt and the system clipboard live on the frontend's main thread, which takes the
//! VT lock too. So the read is answered **before** the callback runs: the IO thread scans the stream ([`crate::scan`]),
//! stops right before the byte that makes Ghostty dispatch a read (BEL, or the ESC of ST), releases the VT lock, sends
//! [`PaneEvent::ClipboardRead`] and waits for [`Pane::answer_clipboard_read`](crate::Pane::answer_clipboard_read). The
//! answer is armed here, the dispatch byte is fed alone, and the callback replies from it — so Ghostty still parses,
//! validates and serializes the request and its reply. While the pane waits, its output is not read (the program
//! waits on the reply anyway); the GTK build's full libghostty keeps streaming instead.
//!
//! The callback serves an armed answer only to a request it matches (same clipboard; a list-only answer never serves
//! data) and clears it after that one byte; with nothing armed (a read the scanner did not see) it denies.

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, PoisonError};

use agterm_ghostty_vt as vt;

use crate::pty::WriteQueue;
use crate::render::{Capture, Rgb, rgb};
use crate::scan::ReadScope;

/// A clipboard a program reads or writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClipboardLocation {
    Standard,
    Selection,
    Primary,
}

impl ClipboardLocation {
    fn from_engine(location: vt::ClipboardLocation) -> Self {
        match location {
            vt::ClipboardLocation::Standard => Self::Standard,
            vt::ClipboardLocation::Selection => Self::Selection,
            vt::ClipboardLocation::Primary => Self::Primary,
        }
    }
}

/// One representation of a clipboard value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardContent {
    pub mime: String,
    pub data: Vec<u8>,
}

/// A program's clipboard write; no contents means "clear".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardWrite {
    pub location: ClipboardLocation,
    pub contents: Vec<ClipboardContent>,
}

/// A program's clipboard read, waiting for [`Pane::answer_clipboard_read`](crate::Pane::answer_clipboard_read).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipboardReadRequest {
    /// Pass back with the answer; answers to any other id are ignored.
    pub id: u64,
    pub location: ClipboardLocation,
    /// [`ReadScope::ListOnly`] asks only which types are available (no consent needed).
    pub scope: ReadScope,
}

/// The frontend's answer to a [`ClipboardReadRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardAnswer {
    /// The clipboard's text (`None`: the clipboard holds no text; the program reads it as empty).
    Text(Option<String>),
    /// Refused by policy or the user: the program gets an empty clipboard (OSC 52) or `EPERM` (OSC 5522).
    Denied,
}

/// An OSC 9;4 progress report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    Remove,
    /// 0–100.
    Set(u8),
    Error(Option<u8>),
    Indeterminate,
    Pause(Option<u8>),
}

/// What a program says it is doing (OSC 7501).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramState {
    Idle,
    Working,
    Done,
    Blocked,
    Error,
    /// Remove the record with this id and every record beneath it (every record for an empty id).
    Clear,
}

/// What a blocked program needs from the user (OSC 7501).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramNeed {
    None,
    Permission,
    Question,
    Auth,
}

/// A program status report (OSC 7501); texts are untrusted program output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramStatus {
    pub state: ProgramState,
    pub need: ProgramNeed,
    pub progress: Option<u8>,
    pub id: String,
    pub app: String,
    pub title: String,
    pub message: String,
}

/// The terminal's effective colors after a program changed them (OSC 4/10/11/12 and their resets).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalColors {
    pub foreground: Option<Rgb>,
    pub background: Option<Rgb>,
    pub cursor: Option<Rgb>,
    pub palette: Box<[Rgb; 256]>,
}

impl TerminalColors {
    pub(crate) fn from_engine(colors: &vt::TerminalColors) -> Self {
        Self {
            foreground: colors.foreground.map(rgb),
            background: colors.background.map(rgb),
            cursor: colors.cursor.map(rgb),
            palette: Box::new(colors.palette.map(rgb)),
        }
    }
}

/// How the pane's program ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitStatus {
    /// The exit code; 1 when a signal ended it.
    pub code: u32,
    /// The signal's name when a signal ended it.
    pub signal: Option<String>,
}

/// What a pane reports to its frontend, in stream order (drained on the main thread).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneEvent {
    /// OSC 0/2 (empty after a full reset).
    Title(String),
    /// OSC 7/9/1337 `CurrentDir`, raw (an OSC 7 `file://` URI or a bare path; empty after a full reset).
    Pwd(String),
    Bell,
    /// OSC 9 / OSC 777; the title is empty when the protocol has none.
    Notification {
        title: String,
        body: String,
    },
    Progress(Progress),
    ProgramStatus(ProgramStatus),
    ClipboardWrite(ClipboardWrite),
    ClipboardRead(ClipboardReadRequest),
    Colors(TerminalColors),
    /// The program exited; its output up to the exit has been processed.
    Exited(ExitStatus),
}

/// An answer armed for the next dispatch byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArmedRead {
    pub location: ClipboardLocation,
    pub scope: ReadScope,
    pub answer: ClipboardAnswer,
}

/// The armed answer, shared by the IO thread (arms and clears it around the dispatch byte, under the VT lock) and
/// the callback (inside that same `vt_write`).
pub(crate) type ArmedSlot = Arc<Mutex<Option<ArmedRead>>>;

/// The MIME type served (as the GTK build serves it: text only).
const TEXT: &str = "text/plain";

/// The device attributes Ghostty reports (`termio/stream_handler.zig`): a VT220 (level 2) with color text and
/// clipboard access; DA2 `1;10;0`.
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

/// The pane's [`vt::Effects`].
pub(crate) struct PaneEffects {
    pub events: Sender<PaneEvent>,
    pub writer: Arc<WriteQueue>,
    pub capture: Arc<Mutex<Capture>>,
    pub armed: ArmedSlot,
    pub xtversion: String,
}

impl PaneEffects {
    fn send(&self, event: PaneEvent) {
        // a closed channel means the pane is going away; nothing is waiting for the event
        let _ = self.events.send(event);
    }

    fn answer(&self, request: &vt::ClipboardReadRequest<'_>) -> vt::ClipboardReadReply {
        let armed = self.armed.lock().unwrap_or_else(PoisonError::into_inner).take();
        let Some(armed) = armed else {
            tracing::debug!("clipboard read without an armed answer; denied");
            return vt::ClipboardReadReply::Denied;
        };
        let location = ClipboardLocation::from_engine(request.location);
        if armed.location != location || (armed.scope == ReadScope::ListOnly && !request.mimes.is_empty()) {
            tracing::debug!(
                ?armed,
                ?location,
                "clipboard read does not match the armed answer; denied"
            );
            return vt::ClipboardReadReply::Denied;
        }
        let ClipboardAnswer::Text(text) = armed.answer else {
            return vt::ClipboardReadReply::Denied;
        };
        let contents = match &text {
            Some(text) if request.mimes.iter().any(|mime| *mime == TEXT.as_bytes()) => vec![vt::ClipboardContent {
                mime: TEXT.to_owned(),
                data: text.clone().into_bytes(),
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

fn progress(report: vt::ProgressReport) -> Progress {
    match report.state {
        vt::ProgressState::Remove => Progress::Remove,
        vt::ProgressState::Set => Progress::Set(report.progress.unwrap_or(0)),
        vt::ProgressState::Error => Progress::Error(report.progress),
        vt::ProgressState::Indeterminate => Progress::Indeterminate,
        vt::ProgressState::Pause => Progress::Pause(report.progress),
    }
}

fn program_status(status: vt::ProgramStatus) -> ProgramStatus {
    ProgramStatus {
        state: match status.state {
            vt::ProgramStatusState::Idle => ProgramState::Idle,
            vt::ProgramStatusState::Working => ProgramState::Working,
            vt::ProgramStatusState::Done => ProgramState::Done,
            vt::ProgramStatusState::Blocked => ProgramState::Blocked,
            vt::ProgramStatusState::Error => ProgramState::Error,
            vt::ProgramStatusState::Clear => ProgramState::Clear,
        },
        need: match status.kind {
            vt::ProgramStatusKind::None => ProgramNeed::None,
            vt::ProgramStatusKind::Permission => ProgramNeed::Permission,
            vt::ProgramStatusKind::Question => ProgramNeed::Question,
            vt::ProgramStatusKind::Auth => ProgramNeed::Auth,
        },
        progress: status.progress,
        id: status.id,
        app: status.app,
        title: status.title,
        message: status.message,
    }
}

impl vt::Effects for PaneEffects {
    fn write_pty(&mut self, data: &[u8]) {
        self.writer.push_reply(data);
    }

    fn bell(&mut self) {
        self.send(PaneEvent::Bell);
    }

    fn title_changed(&mut self, title: &str) {
        self.send(PaneEvent::Title(title.to_owned()));
    }

    fn pwd_changed(&mut self, pwd: &str) {
        self.send(PaneEvent::Pwd(pwd.to_owned()));
    }

    fn desktop_notification(&mut self, title: &str, body: &str) {
        self.send(PaneEvent::Notification {
            title: title.to_owned(),
            body: body.to_owned(),
        });
    }

    fn progress_report(&mut self, report: vt::ProgressReport) {
        self.send(PaneEvent::Progress(progress(report)));
    }

    fn program_status(&mut self, status: vt::ProgramStatus) {
        self.send(PaneEvent::ProgramStatus(program_status(status)));
    }

    /// Accepted at once and handed to the frontend, which applies `clipboard-write` and its prompt: OSC 52 has no
    /// acknowledgement, and an OSC 5522 write is acknowledged before the frontend decides.
    fn clipboard_write(&mut self, request: &vt::ClipboardWriteRequest<'_>) -> vt::ClipboardWriteResult {
        let contents = request
            .contents
            .iter()
            .map(|content| ClipboardContent {
                mime: String::from_utf8_lossy(content.mime).into_owned(),
                data: content.data.to_vec(),
            })
            .collect();
        self.send(PaneEvent::ClipboardWrite(ClipboardWrite {
            location: ClipboardLocation::from_engine(request.location),
            contents,
        }));
        vt::ClipboardWriteResult::Success
    }

    fn clipboard_read(&mut self, request: &vt::ClipboardReadRequest<'_>) -> vt::ClipboardReadReply {
        self.answer(request)
    }

    fn render_hold(&mut self, held: bool, terminal: &mut vt::HeldTerminal<'_>) {
        // lock order VT (held by our caller) → capture; paint never takes VT while holding capture
        let mut capture = self.capture.lock().unwrap_or_else(PoisonError::into_inner);
        if held {
            capture.begin_hold(terminal);
        } else {
            capture.end_hold();
        }
    }

    fn device_attributes(&mut self) -> Option<vt::DeviceAttributes> {
        Some(device_attributes())
    }

    fn xtversion(&mut self) -> Option<&str> {
        Some(&self.xtversion)
    }

    fn reset(&mut self) {
        // a full reset clears the title and pwd without their own callbacks
        self.send(PaneEvent::Title(String::new()));
        self.send(PaneEvent::Pwd(String::new()));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    fn pane_effects(armed: Option<ArmedRead>) -> PaneEffects {
        let (events, _) = mpsc::channel();
        PaneEffects {
            events,
            writer: Arc::new(WriteQueue::new(1024)),
            capture: Arc::new(Mutex::new(Capture::new().unwrap())),
            armed: Arc::new(Mutex::new(armed)),
            xtversion: String::new(),
        }
    }

    fn armed(location: ClipboardLocation, scope: ReadScope, answer: ClipboardAnswer) -> ArmedRead {
        ArmedRead {
            location,
            scope,
            answer,
        }
    }

    fn request(
        location: vt::ClipboardLocation,
        mimes: &[&'static [u8]],
        list: bool,
    ) -> vt::ClipboardReadRequest<'static> {
        vt::ClipboardReadRequest {
            location,
            mimes: mimes.to_vec(),
            list,
            name: b"",
            granted: false,
        }
    }

    fn text(text: &str) -> ClipboardAnswer {
        ClipboardAnswer::Text(Some(text.to_owned()))
    }

    #[test]
    fn nothing_armed_denies() {
        let effects = pane_effects(None);
        let reply = effects.answer(&request(vt::ClipboardLocation::Standard, &[b"text/plain"], false));
        assert_eq!(reply, vt::ClipboardReadReply::Denied);
    }

    #[test]
    fn an_answer_for_another_clipboard_denies_and_is_spent() {
        let effects = pane_effects(Some(armed(ClipboardLocation::Primary, ReadScope::Data, text("x"))));
        let standard = request(vt::ClipboardLocation::Standard, &[b"text/plain"], false);
        assert_eq!(effects.answer(&standard), vt::ClipboardReadReply::Denied);
        assert!(effects.armed.lock().unwrap().is_none());
    }

    #[test]
    fn a_listing_answer_never_serves_data() {
        let effects = pane_effects(Some(armed(
            ClipboardLocation::Standard,
            ReadScope::ListOnly,
            text("secret"),
        )));
        let data = request(vt::ClipboardLocation::Standard, &[b"text/plain"], true);
        assert_eq!(effects.answer(&data), vt::ClipboardReadReply::Denied);

        let effects = pane_effects(Some(armed(
            ClipboardLocation::Standard,
            ReadScope::ListOnly,
            text("secret"),
        )));
        let listing = request(vt::ClipboardLocation::Standard, &[], true);
        assert_eq!(
            effects.answer(&listing),
            vt::ClipboardReadReply::Success {
                contents: Vec::new(),
                available: vec!["text/plain".to_owned()],
            }
        );
    }

    #[test]
    fn text_is_served_for_text_plain_only_and_listed_only_when_present() {
        let effects = pane_effects(Some(armed(ClipboardLocation::Standard, ReadScope::Data, text("abc"))));
        let both = request(vt::ClipboardLocation::Standard, &[b"image/png", b"text/plain"], true);
        assert_eq!(
            effects.answer(&both),
            vt::ClipboardReadReply::Success {
                contents: vec![vt::ClipboardContent {
                    mime: "text/plain".to_owned(),
                    data: b"abc".to_vec(),
                }],
                available: vec!["text/plain".to_owned()],
            }
        );
        let effects = pane_effects(Some(armed(
            ClipboardLocation::Standard,
            ReadScope::Data,
            ClipboardAnswer::Text(None),
        )));
        assert_eq!(
            effects.answer(&request(vt::ClipboardLocation::Standard, &[b"text/plain"], true)),
            vt::ClipboardReadReply::Success {
                contents: Vec::new(),
                available: Vec::new(),
            }
        );
        let effects = pane_effects(Some(armed(
            ClipboardLocation::Standard,
            ReadScope::Data,
            ClipboardAnswer::Denied,
        )));
        assert_eq!(
            effects.answer(&request(vt::ClipboardLocation::Standard, &[b"text/plain"], false)),
            vt::ClipboardReadReply::Denied
        );
    }
}
