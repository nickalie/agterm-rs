//! Safe wrapper over libghostty-vt's official C API (`agterm-ghostty-vt-sys`, Ghostty's `ghostty/vt.h`).
//!
//! - [`Terminal`]: one VT (parser, screen and scrollback), fed with [`Terminal::vt_write`] and resized with
//!   [`Terminal::resize`].
//! - [`RenderState`]: the capture a renderer draws from, updated from a terminal in one call or in two phases, read
//!   row by row through [`RenderState::rows`].
//!
//! Every handle is `Send` and not `Sync`: the library creates no threads and keeps no thread-local state, and asks
//! callers to serialize access to a terminal from any thread (`render.h`, `terminal.h`). A handle may move to another
//! thread; sharing one takes a lock around it. The rest of the API (effects, encoders, selection, formatter, search,
//! kitty graphics) grows with the pane core.
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

mod render;
mod terminal;

pub use render::{Dirty, PendingUpdate, RenderState, RowRef, Rows};
pub use terminal::Terminal;

use std::ptr::NonNull;

use agterm_ghostty_vt_sys as sys;

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
    pub(crate) fn check(result: sys::GhosttyResult) -> Result<(), Self> {
        match result {
            sys::GHOSTTY_SUCCESS => Ok(()),
            sys::GHOSTTY_OUT_OF_MEMORY => Err(Self::OutOfMemory),
            sys::GHOSTTY_INVALID_VALUE => Err(Self::InvalidValue),
            sys::GHOSTTY_OUT_OF_SPACE => Err(Self::OutOfSpace),
            sys::GHOSTTY_NO_VALUE => Err(Self::NoValue),
            sys::GHOSTTY_IO_ERROR => Err(Self::Io),
            sys::GHOSTTY_LIMIT_EXCEEDED => Err(Self::LimitExceeded),
            sys::GHOSTTY_REJECTED => Err(Self::Rejected),
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
        assert_eq!(Error::check(sys::GHOSTTY_SUCCESS), Ok(()));
        for (code, error) in [
            (sys::GHOSTTY_OUT_OF_MEMORY, Error::OutOfMemory),
            (sys::GHOSTTY_INVALID_VALUE, Error::InvalidValue),
            (sys::GHOSTTY_OUT_OF_SPACE, Error::OutOfSpace),
            (sys::GHOSTTY_NO_VALUE, Error::NoValue),
            (sys::GHOSTTY_IO_ERROR, Error::Io),
            (sys::GHOSTTY_LIMIT_EXCEEDED, Error::LimitExceeded),
            (sys::GHOSTTY_REJECTED, Error::Rejected),
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
