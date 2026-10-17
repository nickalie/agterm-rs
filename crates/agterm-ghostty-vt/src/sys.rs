//! The library's process-global hooks (`sys.h`): a PNG decoder for Kitty graphics and a log sink.
//!
//! They are plain global variables in the library, read by every terminal on any thread, so `sys.h` asks for them to
//! be set once at startup, before any terminal uses them. The wrapper enforces that order: the first of [`install`]
//! and [`Terminal::new`](crate::Terminal::new) seals the hooks (a terminal created first seals them empty, and a later
//! [`install`] changes nothing), and either waits while the other is sealing them. No native write of a hook can
//! therefore overlap a terminal's read of it. Code that wants the hooks calls [`install`] before its first terminal.

use std::ffi::c_void;
use std::sync::OnceLock;

use crate::{ffi, raw_slice};

/// A decoded image: 8-bit RGBA, rows top to bottom, no padding (`width * height * 4` bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// The severity of a library log message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Error,
    Warning,
    Info,
    Debug,
}

/// Decodes PNG bytes; `None` on failure.
pub type PngDecoder = fn(&[u8]) -> Option<DecodedImage>;

/// Receives `(level, scope, message)`, from any thread.
pub type LogSink = fn(LogLevel, &str, &str);

/// The hooks to install. `png` decodes PNG bytes (`None` on failure); `log` receives `(level, scope, message)` from
/// any thread.
#[derive(Debug, Clone, Copy)]
pub struct Hooks {
    pub png: Option<PngDecoder>,
    pub log: Option<LogSink>,
}

static HOOKS: OnceLock<Hooks> = OnceLock::new();

/// Install `hooks` for the whole process, once. Returns whether this call installed them (`false`: an earlier call
/// did, and its hooks stay, or a terminal was created first and sealed them empty).
pub fn install(hooks: Hooks) -> bool {
    let mut installed = false;
    HOOKS.get_or_init(|| {
        if hooks.png.is_some() {
            // SAFETY: the option takes the decode function pointer itself; the trampoline reads `HOOKS`, which is set
            // when this initializer returns and before any terminal can use it (callers install first).
            unsafe { ffi::ghostty_sys_set(ffi::GHOSTTY_SYS_OPT_DECODE_PNG, decode_png as *const c_void) };
        }
        if hooks.log.is_some() {
            // SAFETY: as above, for the log function.
            unsafe { ffi::ghostty_sys_set(ffi::GHOSTTY_SYS_OPT_LOG, log as *const c_void) };
        }
        installed = true;
        hooks
    });
    installed
}

/// Seal the hooks before the first terminal exists: empty unless [`install`] ran first (waits while it runs).
pub(crate) fn seal() {
    HOOKS.get_or_init(|| Hooks { png: None, log: None });
}

/// The installed hooks, if any (empty ones when a terminal sealed them first).
#[must_use]
pub fn installed() -> Option<Hooks> {
    HOOKS.get().copied()
}

unsafe extern "C" fn decode_png(
    _userdata: *mut c_void,
    allocator: *const ffi::GhosttyAllocator,
    data: *const u8,
    data_len: usize,
    out: *mut ffi::GhosttySysImage,
) -> bool {
    let Some(decode) = HOOKS.get().and_then(|hooks| hooks.png) else {
        return false;
    };
    // SAFETY: the library passes `data_len` readable bytes for the call.
    let png = unsafe { raw_slice(data, data_len) };
    let Some(image) = decode(png) else {
        return false;
    };
    let expected = u64::from(image.width) * u64::from(image.height) * 4;
    if image.rgba.is_empty() || u64::try_from(image.rgba.len()).ok() != Some(expected) {
        return false;
    }
    // SAFETY: the allocator the library passed for this image; a null result (over its limit) is a failure.
    let buffer = unsafe { ffi::ghostty_alloc(allocator, image.rgba.len()) };
    if buffer.is_null() {
        return false;
    }
    // SAFETY: `buffer` holds `rgba.len()` writable bytes just allocated, disjoint from `rgba`.
    unsafe { std::ptr::copy_nonoverlapping(image.rgba.as_ptr(), buffer, image.rgba.len()) };
    // SAFETY: the library passes a valid, zeroed out-pointer for the call.
    let Some(out) = (unsafe { out.as_mut() }) else {
        // SAFETY: the buffer came from this allocator with this length and was never handed over.
        unsafe { ffi::ghostty_free(allocator, buffer, image.rgba.len()) };
        return false;
    };
    *out = ffi::GhosttySysImage {
        width: image.width,
        height: image.height,
        data: buffer,
        data_len: image.rgba.len(),
    };
    true
}

unsafe extern "C" fn log(
    _userdata: *mut c_void,
    level: ffi::GhosttySysLogLevel,
    scope: *const u8,
    scope_len: usize,
    message: *const u8,
    message_len: usize,
) {
    let Some(sink) = HOOKS.get().and_then(|hooks| hooks.log) else {
        return;
    };
    let level = match level {
        ffi::GHOSTTY_SYS_LOG_LEVEL_ERROR => LogLevel::Error,
        ffi::GHOSTTY_SYS_LOG_LEVEL_WARNING => LogLevel::Warning,
        ffi::GHOSTTY_SYS_LOG_LEVEL_INFO => LogLevel::Info,
        _ => LogLevel::Debug,
    };
    // SAFETY: the library passes `scope_len` readable bytes for the call.
    let scope = String::from_utf8_lossy(unsafe { raw_slice(scope, scope_len) });
    // SAFETY: as above, for the message.
    let message = String::from_utf8_lossy(unsafe { raw_slice(message, message_len) });
    sink(level, &scope, &message);
}
