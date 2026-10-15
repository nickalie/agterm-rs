//! Safe wrapper over libghostty-vt's official C API (`agterm-ghostty-vt-sys`, Ghostty's `ghostty/vt.h`).
//!
//! - [`Terminal`]: one VT (parser, screen and scrollback), fed with [`Terminal::vt_write`] and resized with
//!   [`Terminal::resize`].
//! - [`RenderState`]: the capture a renderer draws from, updated from a terminal in one call or in two phases, read
//!   row by row through [`RenderState::rows`].
//!
//! - [`Effects`]: the callbacks a terminal runs synchronously inside [`Terminal::vt_write`] (replies to the PTY,
//!   title, pwd, bell, notifications, progress, program status, clipboard, synchronized output), installed with
//!   [`Terminal::set_effects`].
//! - [`sys`]: the process-global hooks (PNG decoder, log), installed once at startup.
//! - Input: [`KeyEncoder`] and [`MouseEncoder`] turn key and pointer events into what the program asked for
//!   (legacy, Kitty keyboard protocol, the mouse tracking modes), [`encode_focus`] the focus reports,
//!   [`Terminal::paste`] frames a paste (bracketed or not, unsafe text refused until confirmed).
//! - Selection and read-back: [`Point`]s, the terminal's [`Selection`] (cell ranges, word, line, everything) and
//!   [`Terminal::format`], which reads a region or the selection back as text.
//! - [`SelectionGesture`]: Ghostty's pointer selection state machine (click counts, drags, autoscroll).
//! - [`Search`]: scrollback search, its terminal-touching calls serialized with the terminal, its ticks free of it.
//! - [`KittyGraphics`]: the stored Kitty images and their placements.
//!
//! Every handle is `Send` and not `Sync`: the library creates no threads and keeps no thread-local state, and asks
//! callers to serialize access to a terminal from any thread (`render.h`, `terminal.h`). A handle may move to another
//! thread; sharing one takes a lock around it. The rest of the API (encoders, selection, formatter, search, kitty
//! graphics) follows the same rule: a call that reads or writes the terminal takes it, and nothing the library lends
//! outlives the borrow it was read under.
//!
//! ```
//! fn moved<T: Send>() {}
//! moved::<agterm_ghostty_vt::Terminal>();
//! moved::<agterm_ghostty_vt::RenderState>();
//! ```
//!
//! ```compile_fail,E0277
//! fn shared<T: Sync>() {}
//! shared::<agterm_ghostty_vt::Terminal>();
//! ```
//!
//! ```compile_fail,E0277
//! fn shared<T: Sync>() {}
//! shared::<agterm_ghostty_vt::RenderState>();
//! ```
//!
//! ```
//! fn moved<T: Send>() {}
//! moved::<agterm_ghostty_vt::KeyEncoder>();
//! moved::<agterm_ghostty_vt::MouseEncoder>();
//! moved::<agterm_ghostty_vt::Search>();
//! ```
//!
//! ```compile_fail,E0277
//! fn shared<T: Sync>() {}
//! shared::<agterm_ghostty_vt::Search>();
//! ```

mod cell;
mod effects;
mod format;
mod gesture;
mod key;
mod kitty;
mod mouse;
mod paste;
mod render;
mod search;
mod selection;
mod shape;
pub mod sys;
mod terminal;

pub use cell::{CellRef, CellWidth, Cells, Style, StyleColor, StyleFlags, Underline};
pub use effects::{
    ClipboardContent, ClipboardData, ClipboardLocation, ClipboardReadReply, ClipboardReadRequest,
    ClipboardWriteRequest, ClipboardWriteResult, ColorScheme, DeviceAttributes, Effects, HeldTerminal, ProgramStatus,
    ProgramStatusKind, ProgramStatusState, ProgressReport, ProgressState, SizeReport,
};
pub use format::{Format, FormatOptions, Region};
pub use gesture::{
    Autoscroll, GestureBehavior, GestureDrag, GestureGeometry, GestureInput, GesturePress, GestureState, GestureTick,
    SelectionGesture,
};
pub use key::{Key, KeyAction, KeyEncoder, KeyEvent, KeyMods, encode_focus};
pub use kitty::{KittyFormat, KittyGraphics, KittyImage, KittyLayer, KittyPlacement, KittyRenderInfo, PixelRect};
pub use mouse::{MouseAction, MouseButton, MouseEncoder, MouseEvent, MouseGeometry};
pub use paste::{PasteOutcome, PasteSource, paste_is_safe};
pub use render::{
    CursorPosition, CursorStyle, Dirty, PendingUpdate, RenderColors, RenderCursor, RenderState, RowId, RowRef, Rows,
};
pub use search::{Match, Search, SearchDirection, SearchResults, SearchStatus};
pub use selection::{Point, PointTag, Selection, SelectionAdjust};
pub use shape::{Mode, MouseShape, ScrollbackLimits, TerminalColors};
pub use terminal::{Scroll, Terminal};

use std::ptr::NonNull;

use agterm_ghostty_vt_sys as ffi;

/// A 24-bit color.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

impl From<ffi::GhosttyColorRgb> for Rgb {
    fn from(color: ffi::GhosttyColorRgb) -> Self {
        Self::new(color.r, color.g, color.b)
    }
}

/// The library's version string (e.g. `1.2.3` or `1.2.3-dev+abcdef`).
#[must_use]
pub fn version() -> String {
    let mut value = ffi::GhosttyString {
        ptr: std::ptr::null(),
        len: 0,
    };
    // SAFETY: `VERSION_STRING` writes a `GhosttyString` pointing at static library data; on failure it stays empty.
    unsafe { ffi::ghostty_build_info(ffi::GHOSTTY_BUILD_INFO_VERSION_STRING, (&raw mut value).cast()) };
    // SAFETY: the string is static in the library.
    String::from_utf8_lossy(unsafe { string_bytes(value) }).into_owned()
}

/// The bytes a library string points at, borrowed for as long as the library keeps them (a callback's duration, or
/// until the next mutating call on the owner).
///
/// # Safety
///
/// `string.ptr` must be valid for `string.len` bytes of reads for `'a`, or `string.len` must be zero.
unsafe fn string_bytes<'a>(string: ffi::GhosttyString) -> &'a [u8] {
    if string.ptr.is_null() || string.len == 0 {
        return &[];
    }
    // SAFETY: non-null, and the caller guarantees `len` readable bytes for `'a`.
    unsafe { std::slice::from_raw_parts(string.ptr, string.len) }
}

/// A slice of `len` values at `ptr`, empty for a null pointer or a zero length.
///
/// # Safety
///
/// `ptr` must be valid for `len` reads of `T` for `'a`, or `len` must be zero or `ptr` null.
unsafe fn raw_slice<'a, T>(ptr: *const T, len: usize) -> &'a [T] {
    if ptr.is_null() || len == 0 {
        return &[];
    }
    // SAFETY: non-null, and the caller guarantees `len` readable values for `'a`.
    unsafe { std::slice::from_raw_parts(ptr, len) }
}

/// A libghostty-vt call that returned a `GhosttyResult` other than `GHOSTTY_SUCCESS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// `GHOSTTY_OUT_OF_MEMORY`: an allocation failed.
    OutOfMemory,
    /// `GHOSTTY_INVALID_VALUE`: an argument was out of range (a zero size, a null handle).
    InvalidValue,
    /// `GHOSTTY_OUT_OF_SPACE`: a caller buffer was too small.
    OutOfSpace,
    /// `GHOSTTY_NO_VALUE`: the requested value is unset.
    NoValue,
    /// `GHOSTTY_IO_ERROR`: reading or writing external I/O failed.
    Io,
    /// `GHOSTTY_LIMIT_EXCEEDED`: encoded input exceeded a configured limit.
    LimitExceeded,
    /// `GHOSTTY_REJECTED`: a safety check refused the operation.
    Rejected,
    /// A result code this binding does not know (a newer library).
    Unknown(i32),
}

impl Error {
    /// `Ok` for `GHOSTTY_SUCCESS`, the matching error otherwise.
    pub(crate) fn check(result: ffi::GhosttyResult) -> Result<(), Self> {
        match result {
            ffi::GHOSTTY_SUCCESS => Ok(()),
            ffi::GHOSTTY_OUT_OF_MEMORY => Err(Self::OutOfMemory),
            ffi::GHOSTTY_INVALID_VALUE => Err(Self::InvalidValue),
            ffi::GHOSTTY_OUT_OF_SPACE => Err(Self::OutOfSpace),
            ffi::GHOSTTY_NO_VALUE => Err(Self::NoValue),
            ffi::GHOSTTY_IO_ERROR => Err(Self::Io),
            ffi::GHOSTTY_LIMIT_EXCEEDED => Err(Self::LimitExceeded),
            ffi::GHOSTTY_REJECTED => Err(Self::Rejected),
            other => Err(Self::Unknown(other)),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutOfMemory => f.write_str("libghostty-vt ran out of memory"),
            Self::InvalidValue => f.write_str("libghostty-vt refused an invalid value"),
            Self::OutOfSpace => f.write_str("a buffer for libghostty-vt was too small"),
            Self::NoValue => f.write_str("libghostty-vt has no value for the request"),
            Self::Io => f.write_str("libghostty-vt failed on external I/O"),
            Self::LimitExceeded => f.write_str("input for libghostty-vt exceeded a configured limit"),
            Self::Rejected => f.write_str("libghostty-vt rejected the operation on a safety check"),
            Self::Unknown(code) => write!(f, "libghostty-vt failed with unknown result {code}"),
        }
    }
}

impl std::error::Error for Error {}

/// A library-allocated object owned by one wrapper value and released with its `*_free` function on drop.
///
/// Not `Send` or `Sync` by itself (it holds a raw pointer); each public type that owns handles states why it may
/// move between threads.
struct Handle<T> {
    raw: NonNull<T>,
    free: unsafe extern "C" fn(*mut T),
}

impl<T> Handle<T> {
    /// Take ownership of what a `*_new` call wrote, or [`Error::OutOfMemory`] for a null pointer.
    fn new(raw: *mut T, free: unsafe extern "C" fn(*mut T)) -> Result<Self, Error> {
        NonNull::new(raw)
            .map(|raw| Self { raw, free })
            .ok_or(Error::OutOfMemory)
    }

    fn ptr(&self) -> *mut T {
        self.raw.as_ptr()
    }

    fn non_null(&self) -> NonNull<T> {
        self.raw
    }
}

impl<T> std::fmt::Debug for Handle<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Handle").field(&self.raw).finish()
    }
}

impl<T> Drop for Handle<T> {
    fn drop(&mut self) {
        // SAFETY: `raw` came from the `*_new` call whose matching `*_free` is `free`, is owned by this handle alone
        // (nothing copies it out past the owner's borrows) and is never used after this.
        unsafe { (self.free)(self.raw.as_ptr()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_is_ok_and_every_known_code_maps_to_its_error() {
        assert_eq!(Error::check(ffi::GHOSTTY_SUCCESS), Ok(()));
        for (code, error) in [
            (ffi::GHOSTTY_OUT_OF_MEMORY, Error::OutOfMemory),
            (ffi::GHOSTTY_INVALID_VALUE, Error::InvalidValue),
            (ffi::GHOSTTY_OUT_OF_SPACE, Error::OutOfSpace),
            (ffi::GHOSTTY_NO_VALUE, Error::NoValue),
            (ffi::GHOSTTY_IO_ERROR, Error::Io),
            (ffi::GHOSTTY_LIMIT_EXCEEDED, Error::LimitExceeded),
            (ffi::GHOSTTY_REJECTED, Error::Rejected),
        ] {
            assert_eq!(Error::check(code), Err(error));
        }
    }

    #[test]
    fn an_unknown_code_is_kept() {
        assert_eq!(Error::check(-99), Err(Error::Unknown(-99)));
        assert_eq!(
            Error::Unknown(-99).to_string(),
            "libghostty-vt failed with unknown result -99"
        );
    }
}
