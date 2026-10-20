//! `agterm-session attach` as a terminal client: the terminal it runs in (the outer terminal) in raw mode while it is
//! attached, its size and size changes, its input, and what passes between the session and it.
//!
//! - [`Tty`]: standard input and output; when both are a terminal, raw mode (Unix: termios' raw mode; Windows: console
//!   VT input, as herdr's `client/input/windows_vti.rs` reads it, with `ENABLE_WINDOW_INPUT` for size changes, and VT
//!   processing of the output in UTF-8), restored when it is dropped; the grid and, where the terminal reports them,
//!   its pixels; a reader thread for the input and a watcher for size changes (Unix: `SIGWINCH`; Windows: the
//!   console's window-size events), which report to the attach loop ([`TtyEvent`]).
//! - [`filter`]: the session's output with the queries the server answers taken out.
//! - [`input`]: the outer terminal's input split into the user's input, its answers, and the detach key.
//! - [`owner`]: the client's own sequences (role titles, its probe, fences) and its part as the session's reply owner.
//!
//! herdr: `src/client/terminal_setup.rs` (raw mode, the Windows console's VT input and its restore),
//! `src/client/input/windows_vti.rs` (`console_input_handle`, `windows_console_input_items`: console input records
//! read with `ReadConsoleInputW`) and `src/platform/mod.rs` (`watch_terminal_resize_signal`) at `HERDR_REV`
//! (`build/pins.env`), licensed under the Apache License 2.0, modified: the key records' characters are taken as the
//! VT bytes the console encodes (herdr maps them to its own key events), a window-size record is a size change, and
//! `SIGWINCH` wakes a watcher thread instead of setting a flag a render loop polls.

use std::io;
use std::sync::mpsc::Sender;

use crate::protocol::GridSize;

pub mod filter;
pub mod input;
pub mod owner;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as sys;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as sys;

/// The grid taken when the outer terminal reports none (zmx's `getTerminalSize` default is 120x24; agterm's panes
/// start at 80x24).
pub const DEFAULT_GRID: GridSize = GridSize { cols: 80, rows: 24 };

/// What the outer terminal's threads report.
#[derive(Debug, PartialEq, Eq)]
pub enum TtyEvent {
    /// Bytes read from it.
    Input(Vec<u8>),
    /// Its input ended (or failed).
    InputEnd,
    /// Its size may have changed.
    Resize,
}

/// The outer terminal's size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TtySize {
    pub grid: GridSize,
    /// The window's pixels, width and height, when the terminal reports them.
    pub pixels: Option<(u32, u32)>,
}

/// Standard input and output, in raw mode while this lives when both are a terminal.
#[derive(Debug)]
pub struct Tty {
    raw: Option<sys::RawMode>,
}

impl Tty {
    /// Standard input and output; raw mode when both are a terminal and it could be set.
    #[must_use]
    pub fn open() -> Self {
        let raw = if sys::is_terminal() {
            sys::RawMode::enter()
                .map_err(|error| tracing::warn!(%error, "the terminal could not be put in raw mode"))
                .ok()
        } else {
            None
        };
        Self { raw }
    }

    /// Whether the client runs in a terminal it put in raw mode.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.raw.is_some()
    }

    /// The outer terminal's size; [`DEFAULT_GRID`] when it reports none.
    #[must_use]
    pub fn size(&self) -> TtySize {
        sys::size()
            .filter(|size| size.grid.cols > 0 && size.grid.rows > 0)
            .unwrap_or(TtySize {
                grid: DEFAULT_GRID,
                pixels: None,
            })
    }

    /// Starts the reader of the input and the watcher of size changes, which report to `events` until the process
    /// ends.
    ///
    /// # Errors
    ///
    /// A thread or the signal watch could not be started.
    pub fn start_threads<E: From<TtyEvent> + Send + 'static>(&self, events: &Sender<E>) -> io::Result<()> {
        sys::start_threads(self.raw.as_ref(), events)
    }
}

/// Writes `bytes` to standard output, whole, and flushes: raw bytes (on Windows not through the console's UTF-8
/// check, which refuses a program's invalid UTF-8).
///
/// # Errors
///
/// The write failed (the outer terminal went away).
pub fn write_out(bytes: &[u8]) -> io::Result<()> {
    sys::write_out(bytes)
}
