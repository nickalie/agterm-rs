//! [`Effects`]: the callbacks a terminal runs synchronously inside [`Terminal::vt_write`](crate::Terminal::vt_write)
//! (and the other mutating calls), and the trampolines that reach them from C.
//!
//! The library passes one userdata pointer to every callback. [`Terminal::set_effects`](crate::Terminal::set_effects)
//! boxes the handler on the heap and hands the library a pointer to that box; the terminal owns the box but never
//! dereferences it, so the only Rust reference to the handler during a call is the `&mut` a trampoline makes for the
//! duration of one callback (no aliased `&mut`). Callbacks must not call back into the terminal's mutating API (the C
//! API forbids re-entrant `vt_write`), which the handler cannot: it gets no terminal, only [`HeldTerminal`] in
//! [`Effects::render_hold`], which can do nothing but capture a render state, as `terminal.h` asks.

use std::ffi::c_void;
use std::marker::PhantomData;

use crate::{ffi, raw_slice, string_bytes};

/// A clipboard a program reads or writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClipboardLocation {
    Standard,
    Selection,
    Primary,
}

impl ClipboardLocation {
    fn from_raw(raw: ffi::GhosttyClipboardLocation) -> Self {
        match raw {
            ffi::GHOSTTY_CLIPBOARD_LOCATION_SELECTION => Self::Selection,
            ffi::GHOSTTY_CLIPBOARD_LOCATION_PRIMARY => Self::Primary,
            _ => Self::Standard,
        }
    }

    pub(crate) fn raw(self) -> ffi::GhosttyClipboardLocation {
        match self {
            Self::Standard => ffi::GHOSTTY_CLIPBOARD_LOCATION_STANDARD,
            Self::Selection => ffi::GHOSTTY_CLIPBOARD_LOCATION_SELECTION,
            Self::Primary => ffi::GHOSTTY_CLIPBOARD_LOCATION_PRIMARY,
        }
    }
}

/// One representation of a clipboard write, borrowed for the callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipboardData<'a> {
    pub mime: &'a [u8],
    pub data: &'a [u8],
}

/// A program's clipboard write (OSC 52, OSC 1337 Copy, OSC 5522), decoded; every entry is one representation of the
/// same value. No entries means "clear the clipboard".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardWriteRequest<'a> {
    pub location: ClipboardLocation,
    pub contents: Vec<ClipboardData<'a>>,
    /// The writing program's name, when the protocol carries one (OSC 5522).
    pub name: &'a [u8],
}

/// How a clipboard write ended; protocols with an acknowledgement (OSC 5522) report it to the program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardWriteResult {
    Success,
    Denied,
    Unsupported,
    Busy,
    InvalidData,
    IoError,
}

impl ClipboardWriteResult {
    fn to_raw(self) -> ffi::GhosttyClipboardWriteResult {
        match self {
            Self::Success => ffi::GHOSTTY_CLIPBOARD_WRITE_RESULT_SUCCESS,
            Self::Denied => ffi::GHOSTTY_CLIPBOARD_WRITE_RESULT_DENIED,
            Self::Unsupported => ffi::GHOSTTY_CLIPBOARD_WRITE_RESULT_UNSUPPORTED,
            Self::Busy => ffi::GHOSTTY_CLIPBOARD_WRITE_RESULT_BUSY,
            Self::InvalidData => ffi::GHOSTTY_CLIPBOARD_WRITE_RESULT_INVALID_DATA,
            Self::IoError => ffi::GHOSTTY_CLIPBOARD_WRITE_RESULT_IO_ERROR,
        }
    }
}

/// A program's clipboard read (OSC 52 `?`, OSC 5522 `type=read`), answered before the callback returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardReadRequest<'a> {
    pub location: ClipboardLocation,
    /// The MIME types wanted, in order of preference; OSC 52 asks for `text/plain`. Empty for a listing alone.
    pub mimes: Vec<&'a [u8]>,
    /// Whether the program also wants the list of available MIME types.
    pub list: bool,
    pub name: &'a [u8],
    /// Whether the terminal holds a session grant for the request (only after a reply set `remember`, which this
    /// wrapper never does).
    pub granted: bool,
}

/// One owned representation of a clipboard read's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardContent {
    pub mime: String,
    pub data: Vec<u8>,
}

/// The answer to a [`ClipboardReadRequest`]. Anything but `Success` answers the program with an empty clipboard
/// (OSC 52) or the protocol's status (OSC 5522).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardReadReply {
    Success {
        contents: Vec<ClipboardContent>,
        available: Vec<String>,
    },
    Denied,
    Unsupported,
    Busy,
    IoError,
}

impl ClipboardReadReply {
    fn raw_result(&self) -> ffi::GhosttyClipboardReadResult {
        match self {
            Self::Success { .. } => ffi::GHOSTTY_CLIPBOARD_READ_RESULT_SUCCESS,
            Self::Denied => ffi::GHOSTTY_CLIPBOARD_READ_RESULT_DENIED,
            Self::Unsupported => ffi::GHOSTTY_CLIPBOARD_READ_RESULT_UNSUPPORTED,
            Self::Busy => ffi::GHOSTTY_CLIPBOARD_READ_RESULT_BUSY,
            Self::IoError => ffi::GHOSTTY_CLIPBOARD_READ_RESULT_IO_ERROR,
        }
    }
}

/// The state of an OSC 9;4 progress report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressState {
    Remove,
    Set,
    Error,
    Indeterminate,
    Pause,
}

/// An OSC 9;4 progress report; `progress` is 0–100 or absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgressReport {
    pub state: ProgressState,
    pub progress: Option<u8>,
}

/// What a program says it is doing (OSC 7501).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramStatusState {
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
pub enum ProgramStatusKind {
    None,
    Permission,
    Question,
    Auth,
}

/// A program status report (OSC 7501), its texts copied (lossy UTF-8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramStatus {
    pub state: ProgramStatusState,
    pub kind: ProgramStatusKind,
    pub progress: Option<u8>,
    pub id: String,
    pub app: String,
    pub title: String,
    pub message: String,
}

/// The geometry a size report (XTWINOPS, mode 2048) answers with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeReport {
    pub rows: u16,
    pub columns: u16,
    pub cell_width: u32,
    pub cell_height: u32,
}

/// The device attributes (DA1/DA2/DA3) a terminal reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceAttributes {
    /// DA1 conformance level (62 for a VT220).
    pub conformance_level: u16,
    /// DA1 feature codes (at most 64 are sent).
    pub features: Vec<u16>,
    /// DA2 terminal type (1 for a VT220), firmware version and ROM cartridge.
    pub device_type: u16,
    pub firmware_version: u16,
    pub rom_cartridge: u16,
    /// DA3 unit id.
    pub unit_id: u32,
}

/// The terminal as [`Effects::render_hold`] sees it: only a render state can be captured from it
/// ([`RenderState::update_held`](crate::RenderState::update_held)).
#[derive(Debug)]
pub struct HeldTerminal<'a> {
    raw: ffi::GhosttyTerminal,
    call: PhantomData<&'a mut ()>,
}

impl HeldTerminal<'_> {
    pub(crate) fn as_raw(&mut self) -> ffi::GhosttyTerminal {
        self.raw
    }
}

/// The callbacks a terminal runs synchronously inside its mutating calls (`vt_write`, `resize`, `reset`), on the
/// calling thread and with whatever lock the caller holds. Each must return quickly and must not wait on anything that
/// could be waiting on the caller's lock (a lock taken in the same order as the caller's is fine, as a render-hold
/// capture needs); data passed in is borrowed for the call only. The defaults ignore the effect or decline the query.
pub trait Effects: Send {
    /// Bytes to send back to the program (query and mode reports, clipboard replies).
    fn write_pty(&mut self, data: &[u8]) {
        let _ = data;
    }

    /// BEL.
    fn bell(&mut self) {}

    /// OSC 0/2; the new title.
    fn title_changed(&mut self, title: &str) {
        let _ = title;
    }

    /// OSC 7/9/1337 `CurrentDir`; the raw value the program sent (an OSC 7 URI, or a bare path).
    fn pwd_changed(&mut self, pwd: &str) {
        let _ = pwd;
    }

    /// OSC 9 / OSC 777; the title is empty when the protocol has none.
    fn desktop_notification(&mut self, title: &str, body: &str) {
        let _ = (title, body);
    }

    /// OSC 9;4.
    fn progress_report(&mut self, report: ProgressReport) {
        let _ = report;
    }

    /// OSC 7501.
    fn program_status(&mut self, status: ProgramStatus) {
        let _ = status;
    }

    /// A program's clipboard write. The default refuses it.
    fn clipboard_write(&mut self, request: &ClipboardWriteRequest<'_>) -> ClipboardWriteResult {
        let _ = request;
        ClipboardWriteResult::Denied
    }

    /// A program's clipboard read, answered now. The default refuses it.
    fn clipboard_read(&mut self, request: &ClipboardReadRequest<'_>) -> ClipboardReadReply {
        let _ = request;
        ClipboardReadReply::Denied
    }

    /// Synchronized output (mode 2026) began (`held`) or ended. When it begins, the terminal holds exactly the frame
    /// the program wants left on screen; capture it from `terminal` now and stop updating until it ends.
    fn render_hold(&mut self, held: bool, terminal: &mut HeldTerminal<'_>) {
        let _ = (held, terminal);
    }

    /// A size report is due; `current` is the terminal's own geometry. `None` suppresses the report.
    fn size(&mut self, current: SizeReport) -> Option<SizeReport> {
        Some(current)
    }

    /// A device attributes query; `None` ignores it.
    fn device_attributes(&mut self) -> Option<DeviceAttributes> {
        None
    }

    /// The XTVERSION answer; `None` reports the library's default.
    fn xtversion(&mut self) -> Option<&str> {
        None
    }

    /// A full reset (RIS): the title and pwd are cleared without their own callbacks.
    fn reset(&mut self) {}
}

/// The heap slot the library's userdata points at, owned by the terminal and freed after it.
pub(crate) struct EffectsSlot {
    raw: std::ptr::NonNull<Box<dyn Effects>>,
}

// SAFETY: the slot owns a `Box<dyn Effects>` and `Effects: Send`; it is only reached through the terminal that owns
// it, whose calls are serialized by `&mut`.
unsafe impl Send for EffectsSlot {}

impl EffectsSlot {
    pub(crate) fn new(effects: Box<dyn Effects>) -> Self {
        let raw = std::ptr::NonNull::from(Box::leak(Box::new(effects)));
        Self { raw }
    }

    pub(crate) fn userdata(&self) -> *const c_void {
        self.raw.as_ptr().cast_const().cast()
    }
}

impl std::fmt::Debug for EffectsSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EffectsSlot").finish_non_exhaustive()
    }
}

impl Drop for EffectsSlot {
    fn drop(&mut self) {
        // SAFETY: `raw` came from `Box::leak` in `new` and is reclaimed exactly once; the terminal that could call
        // through it is freed (or its userdata replaced) before the slot drops.
        drop(unsafe { Box::from_raw(self.raw.as_ptr()) });
    }
}

/// Each option and the trampoline registered for it.
pub(crate) const CALLBACKS: &[(ffi::GhosttyTerminalOption, *const c_void)] = &[
    (ffi::GHOSTTY_TERMINAL_OPT_WRITE_PTY, write_pty as *const c_void),
    (ffi::GHOSTTY_TERMINAL_OPT_BELL, bell as *const c_void),
    (ffi::GHOSTTY_TERMINAL_OPT_TITLE_CHANGED, title_changed as *const c_void),
    (ffi::GHOSTTY_TERMINAL_OPT_PWD_CHANGED, pwd_changed as *const c_void),
    (
        ffi::GHOSTTY_TERMINAL_OPT_DESKTOP_NOTIFICATION,
        desktop_notification as *const c_void,
    ),
    (
        ffi::GHOSTTY_TERMINAL_OPT_PROGRESS_REPORT,
        progress_report as *const c_void,
    ),
    (
        ffi::GHOSTTY_TERMINAL_OPT_PROGRAM_STATUS,
        program_status as *const c_void,
    ),
    (
        ffi::GHOSTTY_TERMINAL_OPT_CLIPBOARD_WRITE,
        clipboard_write as *const c_void,
    ),
    (
        ffi::GHOSTTY_TERMINAL_OPT_CLIPBOARD_READ,
        clipboard_read as *const c_void,
    ),
    (ffi::GHOSTTY_TERMINAL_OPT_RENDER_HOLD, render_hold as *const c_void),
    (ffi::GHOSTTY_TERMINAL_OPT_SIZE, size as *const c_void),
    (
        ffi::GHOSTTY_TERMINAL_OPT_DEVICE_ATTRIBUTES,
        device_attributes as *const c_void,
    ),
    (ffi::GHOSTTY_TERMINAL_OPT_XTVERSION, xtversion as *const c_void),
    (ffi::GHOSTTY_TERMINAL_OPT_RESET, reset as *const c_void),
];

/// The handler behind a callback's userdata.
///
/// # Safety
///
/// `userdata` must be the pointer of a live [`EffectsSlot`] (the terminal sets nothing else), and no other reference
/// to its handler may exist for `'a` (true inside one callback: the terminal never dereferences the slot).
unsafe fn handler<'a>(userdata: *mut c_void) -> &'a mut dyn Effects {
    let slot = userdata.cast::<Box<dyn Effects>>();
    // SAFETY: the caller's contract: `slot` points at the live box, uniquely borrowed for `'a`.
    let boxed = unsafe { &mut *slot };
    boxed.as_mut()
}

/// A `GhosttyString` datum of the calling terminal, copied (lossy UTF-8).
///
/// # Safety
///
/// `terminal` must be the live terminal running the callback; `data` a key whose output type is `GhosttyString`.
unsafe fn terminal_string(terminal: ffi::GhosttyTerminal, data: ffi::GhosttyTerminalData) -> String {
    let mut value = ffi::GhosttyString {
        ptr: std::ptr::null(),
        len: 0,
    };
    // SAFETY: the caller's contract; `value` is a valid `GhosttyString` out-pointer. On failure it stays empty.
    unsafe { ffi::ghostty_terminal_get(terminal, data, (&raw mut value).cast()) };
    // SAFETY: the library keeps the bytes until the next mutating call, after this copy.
    String::from_utf8_lossy(unsafe { string_bytes(value) }).into_owned()
}

/// A borrowed library string as lossy UTF-8.
///
/// # Safety
///
/// As [`string_bytes`].
unsafe fn lossy<'a>(string: ffi::GhosttyString) -> std::borrow::Cow<'a, str> {
    // SAFETY: the caller's contract.
    String::from_utf8_lossy(unsafe { string_bytes(string) })
}

unsafe extern "C" fn write_pty(_terminal: ffi::GhosttyTerminal, userdata: *mut c_void, data: *const u8, len: usize) {
    // SAFETY: the library passes `len` readable bytes at `data` for the call.
    let bytes = unsafe { raw_slice(data, len) };
    // SAFETY: userdata is the terminal's effects slot (set in `Terminal::set_effects`).
    unsafe { handler(userdata) }.write_pty(bytes);
}

unsafe extern "C" fn bell(_terminal: ffi::GhosttyTerminal, userdata: *mut c_void) {
    // SAFETY: as in `write_pty`.
    unsafe { handler(userdata) }.bell();
}

unsafe extern "C" fn title_changed(terminal: ffi::GhosttyTerminal, userdata: *mut c_void) {
    // SAFETY: the calling terminal; the title is set before the callback runs (`stream_terminal.zig` windowTitle).
    let title = unsafe { terminal_string(terminal, ffi::GHOSTTY_TERMINAL_DATA_TITLE) };
    // SAFETY: as in `write_pty`.
    unsafe { handler(userdata) }.title_changed(&title);
}

unsafe extern "C" fn pwd_changed(terminal: ffi::GhosttyTerminal, userdata: *mut c_void) {
    // SAFETY: the calling terminal; `terminal.h` documents reading the pwd inside this callback.
    let pwd = unsafe { terminal_string(terminal, ffi::GHOSTTY_TERMINAL_DATA_PWD) };
    // SAFETY: as in `write_pty`.
    unsafe { handler(userdata) }.pwd_changed(&pwd);
}

unsafe extern "C" fn desktop_notification(
    _terminal: ffi::GhosttyTerminal,
    userdata: *mut c_void,
    notification: *const ffi::GhosttyTerminalDesktopNotification,
) {
    // SAFETY: the library passes a valid notification for the call; its strings live as long.
    let Some(notification) = (unsafe { notification.as_ref() }) else {
        return;
    };
    // SAFETY: as above.
    let title = unsafe { lossy(notification.title) };
    // SAFETY: as above.
    let body = unsafe { lossy(notification.body) };
    // SAFETY: as in `write_pty`.
    unsafe { handler(userdata) }.desktop_notification(&title, &body);
}

fn percent(raw: i8) -> Option<u8> {
    u8::try_from(raw).ok().filter(|value| *value <= 100)
}

unsafe extern "C" fn progress_report(
    _terminal: ffi::GhosttyTerminal,
    userdata: *mut c_void,
    report: *const ffi::GhosttyTerminalProgressReport,
) {
    // SAFETY: the library passes a valid report for the call.
    let Some(report) = (unsafe { report.as_ref() }) else {
        return;
    };
    let state = match report.state {
        ffi::GHOSTTY_TERMINAL_PROGRESS_STATE_REMOVE => ProgressState::Remove,
        ffi::GHOSTTY_TERMINAL_PROGRESS_STATE_SET => ProgressState::Set,
        ffi::GHOSTTY_TERMINAL_PROGRESS_STATE_ERROR => ProgressState::Error,
        ffi::GHOSTTY_TERMINAL_PROGRESS_STATE_PAUSE => ProgressState::Pause,
        _ => ProgressState::Indeterminate,
    };
    let report = ProgressReport {
        state,
        progress: percent(report.progress),
    };
    // SAFETY: as in `write_pty`.
    unsafe { handler(userdata) }.progress_report(report);
}

fn program_state(raw: ffi::GhosttyProgramStatusState) -> ProgramStatusState {
    match raw {
        ffi::GHOSTTY_PROGRAM_STATUS_STATE_IDLE => ProgramStatusState::Idle,
        ffi::GHOSTTY_PROGRAM_STATUS_STATE_WORKING => ProgramStatusState::Working,
        ffi::GHOSTTY_PROGRAM_STATUS_STATE_DONE => ProgramStatusState::Done,
        ffi::GHOSTTY_PROGRAM_STATUS_STATE_BLOCKED => ProgramStatusState::Blocked,
        ffi::GHOSTTY_PROGRAM_STATUS_STATE_ERROR => ProgramStatusState::Error,
        _ => ProgramStatusState::Clear,
    }
}

fn program_kind(raw: ffi::GhosttyProgramStatusKind) -> ProgramStatusKind {
    match raw {
        ffi::GHOSTTY_PROGRAM_STATUS_KIND_PERMISSION => ProgramStatusKind::Permission,
        ffi::GHOSTTY_PROGRAM_STATUS_KIND_QUESTION => ProgramStatusKind::Question,
        ffi::GHOSTTY_PROGRAM_STATUS_KIND_AUTH => ProgramStatusKind::Auth,
        _ => ProgramStatusKind::None,
    }
}

unsafe extern "C" fn program_status(
    _terminal: ffi::GhosttyTerminal,
    userdata: *mut c_void,
    report: *const ffi::GhosttyTerminalProgramStatus,
) {
    // SAFETY: the library passes a valid report for the call; its strings live as long.
    let Some(report) = (unsafe { report.as_ref() }) else {
        return;
    };
    // SAFETY: the report's strings are borrowed for the call and copied here.
    let id = unsafe { lossy(report.id) }.into_owned();
    // SAFETY: as above.
    let app = unsafe { lossy(report.app) }.into_owned();
    // SAFETY: as above.
    let title = unsafe { lossy(report.title) }.into_owned();
    // SAFETY: as above.
    let message = unsafe { lossy(report.message) }.into_owned();
    let status = ProgramStatus {
        state: program_state(report.state),
        kind: program_kind(report.kind),
        progress: percent(report.progress),
        id,
        app,
        title,
        message,
    };
    // SAFETY: as in `write_pty`.
    unsafe { handler(userdata) }.program_status(status);
}

unsafe extern "C" fn clipboard_write(
    _terminal: ffi::GhosttyTerminal,
    userdata: *mut c_void,
    write: *const ffi::GhosttyClipboardWrite,
) {
    // SAFETY: the library passes a valid request for the call; its arrays and strings live as long.
    let Some(write) = (unsafe { write.as_ref() }) else {
        return;
    };
    if write.size < std::mem::size_of::<ffi::GhosttyClipboardWrite>() {
        return;
    }
    // SAFETY: `contents` holds `contents_len` entries for the call.
    let entries = unsafe { raw_slice(write.contents, write.contents_len) };
    let contents = entries
        .iter()
        .map(|entry| ClipboardData {
            // SAFETY: each entry's strings live for the call.
            mime: unsafe { string_bytes(entry.mime) },
            // SAFETY: as above.
            data: unsafe { string_bytes(entry.data) },
        })
        .collect();
    let request = ClipboardWriteRequest {
        location: ClipboardLocation::from_raw(write.location),
        contents,
        // SAFETY: as above.
        name: unsafe { string_bytes(write.name) },
    };
    // SAFETY: as in `write_pty`.
    let result = unsafe { handler(userdata) }.clipboard_write(&request);
    let reply = ffi::GhosttyClipboardWriteReply {
        size: std::mem::size_of::<ffi::GhosttyClipboardWriteReply>(),
        result: result.to_raw(),
        remember: false,
    };
    if let Some(answer) = write.reply {
        // SAFETY: `answer` is the request's own reply function, called inside the callback with the request and a
        // reply that lives across the call.
        unsafe { answer(write, &raw const reply) };
    }
}

unsafe extern "C" fn clipboard_read(
    _terminal: ffi::GhosttyTerminal,
    userdata: *mut c_void,
    read: *const ffi::GhosttyClipboardRead,
) {
    // SAFETY: the library passes a valid request for the call; its arrays and strings live as long.
    let Some(read) = (unsafe { read.as_ref() }) else {
        return;
    };
    if read.size < std::mem::size_of::<ffi::GhosttyClipboardRead>() {
        return;
    }
    // SAFETY: `mimes` holds `mimes_len` strings for the call.
    let mimes = unsafe { raw_slice(read.mimes, read.mimes_len) };
    let request = ClipboardReadRequest {
        location: ClipboardLocation::from_raw(read.location),
        // SAFETY: each MIME string lives for the call.
        mimes: mimes.iter().map(|mime| unsafe { string_bytes(*mime) }).collect(),
        list: read.list,
        // SAFETY: as above.
        name: unsafe { string_bytes(read.name) },
        granted: read.granted,
    };
    // SAFETY: as in `write_pty`.
    let answer = unsafe { handler(userdata) }.clipboard_read(&request);
    let (contents, available): (Vec<_>, Vec<_>) = match &answer {
        ClipboardReadReply::Success { contents, available } => (
            contents
                .iter()
                .map(|content| ffi::GhosttyClipboardContent {
                    mime: library_string(content.mime.as_bytes()),
                    data: library_string(&content.data),
                })
                .collect(),
            available.iter().map(|mime| library_string(mime.as_bytes())).collect(),
        ),
        _ => (Vec::new(), Vec::new()),
    };
    let reply = ffi::GhosttyClipboardReadReply {
        size: std::mem::size_of::<ffi::GhosttyClipboardReadReply>(),
        result: answer.raw_result(),
        contents: contents.as_ptr(),
        contents_len: contents.len(),
        available: available.as_ptr(),
        available_len: available.len(),
        remember: false,
    };
    if let Some(respond) = read.reply {
        // SAFETY: the request's own reply function, called inside the callback; the reply, its arrays and the
        // strings they point at (`answer`) live across the call.
        unsafe { respond(read, &raw const reply) };
    }
}

/// A library string borrowing `bytes`.
fn library_string(bytes: &[u8]) -> ffi::GhosttyString {
    ffi::GhosttyString {
        ptr: bytes.as_ptr(),
        len: bytes.len(),
    }
}

unsafe extern "C" fn render_hold(terminal: ffi::GhosttyTerminal, userdata: *mut c_void, held: bool) {
    let mut held_terminal = HeldTerminal {
        raw: terminal,
        call: PhantomData,
    };
    // SAFETY: as in `write_pty`.
    unsafe { handler(userdata) }.render_hold(held, &mut held_terminal);
}

/// A `uint16_t`/`uint32_t` datum of the calling terminal.
///
/// # Safety
///
/// `terminal` must be live and `data` a key whose output type is `T`.
unsafe fn terminal_number<T: Default>(terminal: ffi::GhosttyTerminal, data: ffi::GhosttyTerminalData) -> T {
    let mut value = T::default();
    // SAFETY: the caller's contract; `value` is a valid out-pointer of the key's type.
    unsafe { ffi::ghostty_terminal_get(terminal, data, (&raw mut value).cast()) };
    value
}

unsafe extern "C" fn size(
    terminal: ffi::GhosttyTerminal,
    userdata: *mut c_void,
    out: *mut ffi::GhosttySizeReportSize,
) -> bool {
    // SAFETY: the calling terminal; COLS/ROWS are `uint16_t`, WIDTH_PX/HEIGHT_PX `uint32_t`.
    let columns: u16 = unsafe { terminal_number(terminal, ffi::GHOSTTY_TERMINAL_DATA_COLS) };
    // SAFETY: as above.
    let rows: u16 = unsafe { terminal_number(terminal, ffi::GHOSTTY_TERMINAL_DATA_ROWS) };
    // SAFETY: as above.
    let width: u32 = unsafe { terminal_number(terminal, ffi::GHOSTTY_TERMINAL_DATA_WIDTH_PX) };
    // SAFETY: as above.
    let height: u32 = unsafe { terminal_number(terminal, ffi::GHOSTTY_TERMINAL_DATA_HEIGHT_PX) };
    let current = SizeReport {
        rows,
        columns,
        cell_width: width.checked_div(u32::from(columns)).unwrap_or(0),
        cell_height: height.checked_div(u32::from(rows)).unwrap_or(0),
    };
    // SAFETY: as in `write_pty`.
    let Some(report) = unsafe { handler(userdata) }.size(current) else {
        return false;
    };
    // SAFETY: the library passes a valid out-pointer for the call.
    let Some(out) = (unsafe { out.as_mut() }) else {
        return false;
    };
    *out = ffi::GhosttySizeReportSize {
        rows: report.rows,
        columns: report.columns,
        cell_width: report.cell_width,
        cell_height: report.cell_height,
    };
    true
}

unsafe extern "C" fn device_attributes(
    _terminal: ffi::GhosttyTerminal,
    userdata: *mut c_void,
    out: *mut ffi::GhosttyDeviceAttributes,
) -> bool {
    // SAFETY: as in `write_pty`.
    let Some(attributes) = unsafe { handler(userdata) }.device_attributes() else {
        return false;
    };
    // SAFETY: the library passes a valid out-pointer for the call.
    let Some(out) = (unsafe { out.as_mut() }) else {
        return false;
    };
    let mut features = [0_u16; 64];
    let count = attributes.features.len().min(features.len());
    features[..count].copy_from_slice(&attributes.features[..count]);
    *out = ffi::GhosttyDeviceAttributes {
        primary: ffi::GhosttyDeviceAttributesPrimary {
            conformance_level: attributes.conformance_level,
            features,
            num_features: count,
        },
        secondary: ffi::GhosttyDeviceAttributesSecondary {
            device_type: attributes.device_type,
            firmware_version: attributes.firmware_version,
            rom_cartridge: attributes.rom_cartridge,
        },
        tertiary: ffi::GhosttyDeviceAttributesTertiary {
            unit_id: attributes.unit_id,
        },
    };
    true
}

unsafe extern "C" fn xtversion(_terminal: ffi::GhosttyTerminal, userdata: *mut c_void) -> ffi::GhosttyString {
    // SAFETY: as in `write_pty`. The returned string borrows the handler, which outlives the callback, as the C API
    // requires ("must remain valid until the callback returns").
    let version = unsafe { handler(userdata) }.xtversion().unwrap_or_default();
    library_string(version.as_bytes())
}

unsafe extern "C" fn reset(_terminal: ffi::GhosttyTerminal, userdata: *mut c_void) {
    // SAFETY: as in `write_pty`.
    unsafe { handler(userdata) }.reset();
}
