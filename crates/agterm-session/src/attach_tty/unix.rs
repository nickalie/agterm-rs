//! The outer terminal on Unix: termios' raw mode on standard input (zmx's `cfmakeraw`, restored with `TCSAFLUSH`),
//! the window size (`TIOCGWINSZ`, pixels included where the terminal sets them), a reader thread on standard input and
//! a `SIGWINCH` watcher.
//!
//! herdr: `src/client/terminal_setup.rs` and `src/platform/mod.rs` at `HERDR_REV` (`build/pins.env`), licensed under
//! the Apache License 2.0, modified as the parent module's docs say.

use std::io::{self, IsTerminal as _, Read as _, Write as _};
use std::sync::mpsc::Sender;
use std::thread;

use rustix::termios::{OptionalActions, Termios, tcgetattr, tcgetwinsize, tcsetattr};
use signal_hook::consts::SIGWINCH;
use signal_hook::iterator::Signals;

use super::{TtyEvent, TtySize};
use crate::protocol::GridSize;

/// The size of one read of the input.
const READ_SIZE: usize = 16 * 1024;

pub(super) fn is_terminal() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

/// Standard input in raw mode; dropped, its mode is restored and its unread input discarded.
#[derive(Debug)]
pub(super) struct RawMode {
    saved: Termios,
}

impl RawMode {
    pub(super) fn enter() -> io::Result<Self> {
        let stdin = io::stdin();
        let saved = tcgetattr(&stdin)?;
        let mut raw = saved.clone();
        raw.make_raw();
        tcsetattr(&stdin, OptionalActions::Now, &raw)?;
        Ok(Self { saved })
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = tcsetattr(io::stdin(), OptionalActions::Flush, &self.saved);
    }
}

pub(super) fn size() -> Option<TtySize> {
    let size = tcgetwinsize(io::stdout()).or_else(|_| tcgetwinsize(io::stdin())).ok()?;
    let pixels =
        (size.ws_xpixel > 0 && size.ws_ypixel > 0).then(|| (u32::from(size.ws_xpixel), u32::from(size.ws_ypixel)));
    Some(TtySize {
        grid: GridSize {
            cols: size.ws_col,
            rows: size.ws_row,
        },
        pixels,
    })
}

pub(super) fn start_threads<E: From<TtyEvent> + Send + 'static>(
    raw: Option<&RawMode>,
    events: &Sender<E>,
) -> io::Result<()> {
    let input = events.clone();
    thread::Builder::new()
        .name("attach-input".to_owned())
        .spawn(move || read_input(&input))?;
    if raw.is_some() {
        let mut signals = Signals::new([SIGWINCH])?;
        let resize = events.clone();
        thread::Builder::new().name("attach-resize".to_owned()).spawn(move || {
            for _ in signals.forever() {
                if resize.send(TtyEvent::Resize.into()).is_err() {
                    return;
                }
            }
        })?;
    }
    Ok(())
}

/// Standard input's bytes as they come, until its end.
fn read_input<E: From<TtyEvent>>(events: &Sender<E>) {
    let mut stdin = io::stdin().lock();
    let mut buf = vec![0_u8; READ_SIZE];
    loop {
        match stdin.read(&mut buf) {
            Ok(0) => break,
            Ok(read) => {
                if events.send(TtyEvent::Input(buf[..read].to_vec()).into()).is_err() {
                    return;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    let _ = events.send(TtyEvent::InputEnd.into());
}

pub(super) fn write_out(bytes: &[u8]) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(bytes)?;
    stdout.flush()
}
