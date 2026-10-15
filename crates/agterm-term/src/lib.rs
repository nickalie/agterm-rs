//! The terminal pane core, toolkit-neutral: a libghostty-vt terminal fed from a PTY by an IO thread per pane, its
//! effects as [`PaneEvent`]s for the frontend's main thread, and its screen as [`Frame`]s in this crate's own types.
//!
//! - [`Pane::spawn`] starts a [`PaneCommand`] on a PTY; [`Pane::write`] queues input, [`Pane::resize`] resizes the VT
//!   and the PTY, [`Pane::frame`] captures the screen for painting, [`Pane::take_events`] drains the effects.
//! - Input: [`Pane::key`] (a toolkit-neutral [`KeyInput`], encoded for the terminal's keyboard modes),
//!   [`Pane::send`] (a binding's text), [`Pane::mouse`], [`Pane::wheel`] (routed as Ghostty routes the wheel),
//!   [`Pane::focus`], [`Pane::paste`], [`Pane::scroll`].
//! - Selection and read-back: [`Pane::select`], [`Pane::select_word`],
//!   [`Pane::select_line`], [`Pane::select_all`], [`Pane::selection_text`], [`Pane::read_text`],
//!   [`Pane::cursor_column`]; a frame's rows carry their selected columns.
//! - Search: [`Pane::search`], [`Pane::navigate_search`], [`Pane::search_state`], with
//!   [`PaneEvent::SearchTotal`] and [`PaneEvent::SearchSelected`] as libghostty reported them.
//! - Kitty graphics: every [`Frame`] carries a [`KittyView`] of its images and placements.
//! - Launch: [`Pane::launch`] runs a [`Launch`] (a session pane's seed: a live pane's `zmx attach`, a plain pane's
//!   command or login shell) under a [`Host`] (the terminal identity and shell integration libghostty gave a surface),
//!   with the [`ExitPolicy`] of a launched pane; [`Pane::foreground_pid`] is the terminal's foreground process group.
//! - Threads and locks: Technical Details > Threads of the GPUI plan; the details are in [`pane`]'s and
//!   [`effects`]'s docs. The IO thread holds the VT lock for one bounded slice of `vt_write` at a time; painting only
//!   ever `try_lock`s it.
//! - The engine boundary: no libghostty-vt type appears in this crate's public API, so the VT engine can be swapped
//!   without touching the frontend.
//!
//! [`init`] installs libghostty-vt's process-global hooks (the PNG decoder for Kitty graphics, the log sink into
//! `tracing`); [`Pane::spawn`] calls it, so a frontend need not.

mod actions;
#[cfg(test)]
mod bench;
mod effects;
mod exit;
mod input;
mod kitty;
mod launch;
mod pane;
mod placeholder;
mod png;
mod pty;
mod render;
mod scan;
mod search;
mod selection;
mod text;
mod wheel;

pub use actions::{FormatRegion, SelectionAdjust, TextFormat};
pub use effects::{
    ClipboardAnswer, ClipboardContent, ClipboardLocation, ClipboardReadRequest, ClipboardWrite, ExitStatus, PaneEvent,
    ProgramNeed, ProgramState, ProgramStatus, Progress, TerminalColors,
};
pub use exit::{ABNORMAL_EXIT_RUNTIME, ExitPolicy};
pub use input::{
    KeyInput, ModSides, MouseAction, MouseButton, MouseInput, Padding, Paste, PasteSource, Scroll, Typing,
};
pub use kitty::{
    KittyFormat, KittyImage, KittyImageKey, KittyLayer, KittyPlacement, KittyView, PlacementRect, SourceRect,
};
pub use launch::{COMMAND_SHELL, FALLBACK_TERM, Host, Launch, Prepared, resolve_resources, resource_candidates};
pub use pane::{DEFAULT_KITTY_IMAGE_LIMIT, DEFAULT_WRITE_LIMIT, FrameSnapshot, Pane, PaneConfig, PaneStats, Wake};
pub use placeholder::{KittyPlaceholder, PLACEHOLDER, VirtualPlacement, virtual_placements};
pub use pty::{GridSize, PaneCommand};
pub use render::{
    Cell, CellFlags, CellWidth, Colors, Cursor, CursorDefaults, CursorPosition, CursorStyle, Frame, MouseShape, Rgb,
    Row, ThemeColors, Underline,
};
pub use scan::ReadScope;
pub use search::{Highlight, SearchState, SearchStep};
pub use selection::{GridPoint, PointSpace, SelectionRange, SelectionText};
pub use text::TextRegion;
pub use wheel::{Wheel, WheelInput};

use agterm_ghostty_vt as vt;

/// Why a pane operation failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PaneError {
    /// The program could not be started.
    #[error("cannot start the program: {0}")]
    Spawn(String),
    /// The PTY could not be opened, resized or polled.
    #[error("pty: {0}")]
    Pty(String),
    /// libghostty-vt refused the operation.
    #[error("terminal: {0}")]
    Terminal(String),
    /// The write queue is full: the child is not reading its input.
    #[error("the pane's input queue is full")]
    Backpressure,
    /// The PTY refused a write: the child side is gone.
    #[error("the pane's pty is closed")]
    Closed,
}

impl PaneError {
    pub(crate) fn terminal(err: vt::Error) -> Self {
        Self::Terminal(err.to_string())
    }
}

/// The scrollback limits (`None`: unlimited); the first reached wins, both at page granularity (~400 KB).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollbackLimits {
    pub max_bytes: Option<u64>,
    pub max_lines: Option<u64>,
}

impl Default for ScrollbackLimits {
    /// Ghostty's defaults (`scrollback-limit-bytes = 50000000`, no line limit), as the config parser has them.
    fn default() -> Self {
        agterm_core::ghostty_config::Scrollback::default().into()
    }
}

impl From<agterm_core::ghostty_config::Scrollback> for ScrollbackLimits {
    fn from(scrollback: agterm_core::ghostty_config::Scrollback) -> Self {
        Self {
            max_bytes: scrollback.limit_bytes,
            max_lines: scrollback.limit_lines,
        }
    }
}

impl ScrollbackLimits {
    pub(crate) fn engine(self) -> vt::ScrollbackLimits {
        let size = |limit: Option<u64>| limit.map(|limit| usize::try_from(limit).unwrap_or(usize::MAX));
        vt::ScrollbackLimits {
            max_bytes: size(self.max_bytes),
            max_lines: size(self.max_lines),
        }
    }
}

/// Install libghostty-vt's process-global hooks once: the PNG decoder (Kitty graphics) and the log sink (`tracing`,
/// target `agterm_term::ghostty`).
pub fn init() {
    vt::sys::install(vt::sys::Hooks {
        png: Some(png::decode),
        log: Some(log),
    });
}

fn log(level: vt::sys::LogLevel, scope: &str, message: &str) {
    match level {
        vt::sys::LogLevel::Error => tracing::error!(target: "agterm_term::ghostty", scope, "{message}"),
        vt::sys::LogLevel::Warning => tracing::warn!(target: "agterm_term::ghostty", scope, "{message}"),
        vt::sys::LogLevel::Info => tracing::info!(target: "agterm_term::ghostty", scope, "{message}"),
        vt::sys::LogLevel::Debug => tracing::debug!(target: "agterm_term::ghostty", scope, "{message}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrollback_limits_follow_the_config_and_saturate_into_the_library() {
        let limits = ScrollbackLimits::default();
        assert_eq!(limits.max_bytes, Some(50_000_000));
        assert_eq!(limits.max_lines, None);
        let library = ScrollbackLimits {
            max_bytes: Some(u64::MAX),
            max_lines: Some(10),
        }
        .engine();
        assert_eq!(library.max_bytes, Some(usize::MAX));
        assert_eq!(library.max_lines, Some(10));
    }
}
