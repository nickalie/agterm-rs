//! Safe wrapper over libghostty's embedding API (`agterm-ghostty-sys`).
//!
//! So far: the once-per-process [`init`], and [`Config`] with file loading, typed reads and diagnostics. The app,
//! surface, input and action layers follow with the GTK frontend.

mod app;
mod config;

pub use app::init;
pub use config::{Color, Config, ConfigKey, ConfigValue, Diagnostic, keys};

/// A libghostty call that could not produce what was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// `ghostty_init` returned this non-zero status; nothing else in libghostty may be used.
    Init(i32),
    /// libghostty returned a null handle (it does on allocation failure).
    Null(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Init(status) => write!(f, "ghostty_init failed with status {status}"),
            Self::Null(function) => write!(f, "{function} returned null"),
        }
    }
}

impl std::error::Error for Error {}
