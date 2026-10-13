//! Safe wrapper over libghostty's embedding API (`agterm-ghostty-sys`).
//!
//! - [`init`] once per process, then [`Config`] (load files or text, read typed values, diagnostics).
//! - [`App`] with the host's [`Runtime`]: actions, clipboard and close callbacks, wakeups and the tick.
//! - [`Surface`]: one terminal with its input ([`input`]), drawing, text reads ([`Selection`]) and clipboard
//!   completion.
//!
//! Everything but [`init`] and the wakeup closure is bound to the thread that owns the app.

mod action;
mod app;
mod clipboard;
mod config;
pub mod input;
mod surface;
mod text;

pub use action::{Action, ColorKind, MouseShape, OpenUrlKind, SurfaceKey, Target};
pub use app::{App, Runtime, Wakeup, init};
pub use clipboard::{
    ClipboardConfirm, ClipboardContent, ClipboardKind, ClipboardRead, ClipboardReadResult, ClipboardRequestKind,
    PendingRequest,
};
pub use config::{Color, Config, ConfigKey, ConfigValue, Diagnostic, Palette, StringKey, keys};
pub use surface::{ImePoint, Surface, SurfaceContext, SurfaceOptions, SurfaceSize};
pub use text::{Point, PointCoord, PointTag, Selection, SurfaceText, solve_cursor_column};

use agterm_ghostty_sys as sys;

/// A libghostty call that could not produce what was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// `ghostty_init` returned this non-zero status; nothing else in libghostty may be used.
    Init(i32),
    /// libghostty returned a null handle (it does on allocation failure).
    Null(&'static str),
    /// This thread already owns an [`App`].
    AppExists,
    /// A string handed to libghostty holds a NUL byte.
    Nul,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Init(status) => write!(f, "ghostty_init failed with status {status}"),
            Self::Null(function) => write!(f, "{function} returned null"),
            Self::AppExists => f.write_str("this thread already owns a libghostty app"),
            Self::Nul => f.write_str("a string for libghostty contains a NUL byte"),
        }
    }
}

impl std::error::Error for Error {}

/// Which side of a `theme = light:…,dark:…` pair applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorScheme {
    Light,
    Dark,
}

impl ColorScheme {
    pub(crate) fn raw(self) -> sys::ghostty_color_scheme_e {
        match self {
            Self::Light => sys::GHOSTTY_COLOR_SCHEME_LIGHT,
            Self::Dark => sys::GHOSTTY_COLOR_SCHEME_DARK,
        }
    }
}
