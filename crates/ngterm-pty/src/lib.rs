//! ngterm's PTY layer, the same on Linux, macOS and Windows: a child on a PTY ([`spawn`]) and the PTY actor
//! ([`Actor`]) that moves its bytes, used by plain panes (`ngterm-term`) and by the live-session server's panes
//! (`ngterm-session`) alike, so neither depends on the other for it.
//!
//! - [`spawn`] starts a [`Command`] on a new PTY, sized as a [`Size`]: `openpty` on Unix, where the child leads a
//!   session and process group of its own; Windows' pseudoconsole (`ConPTY`), where the child and every process it
//!   starts run in a job object that kills them all when it closes. It returns the PTY's [`Master`] and the
//!   [`Child`].
//! - [`Child::killer`]'s [`Killer`] ends the child's tree: on Windows by terminating the job, at any time; on Unix
//!   by signalling the process group, which is never signalled once the child is reaped (its id may be reused by
//!   then), so a caller kills the tree before it waits for the child, or the group's survivors live on.
//! - [`Actor::spawn`] takes the master and runs the PTY's IO: every read goes to a callback, whose replies (the
//!   terminal's answers to queries) are written back in order with the input [`ActorHandle::write_input`] queues. On
//!   Unix one thread polls the master and a wake pipe; on Windows a reader thread and a writer thread work the
//!   pseudoconsole's two pipes.
//! - For the session server's update handoff, the actor quiesces (no reads, no writes), hands out duplicates of
//!   what the PTY is made of (the master's descriptor; on Windows the pseudoconsole's pipes and handles), rolls
//!   back, or releases the PTY after the new server took it. An actor over a handed-over PTY starts quiesced
//!   ([`ActorConfig::initially_quiesced`]).
//!
//! # Provenance
//!
//! The actor and the spawn are taken from herdr (<https://github.com/herdrdev/herdr>, Apache-2.0) at the commit
//! `build/pins.env` pins as `HERDR_REV`, as the module headers say, trimmed to what ngterm uses and reworked; herdr's
//! license is in the repository's `NOTICE`. Hence the crate's `MIT AND Apache-2.0`. The PTY itself is portable-pty
//! (`vendor/portable-pty`, with the patches `build/patches/portable-pty` names).

mod actor;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
use unix as sys;
#[cfg(windows)]
use windows as sys;

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;

pub use actor::{
    Actor, ActorConfig, ActorHandle, DEFAULT_INPUT_LIMIT, ExitCallback, ExitReason, REPLY_CAP, ReadCallback, ReadPause,
    Released, WriteError,
};
pub use portable_pty::ExitStatus;
#[cfg(windows)]
pub use portable_pty::win::conpty::ConPtyHandoff;
pub use sys::{Child, Killer, Master};

/// What a PTY runs, and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub program: OsString,
    pub args: Vec<OsString>,
    /// Arguments after `args` that Windows takes into the command line as they are written, without the quoting
    /// `args` get there (portable-pty's `ArgvQuote`, which escapes quotes in a form `cmd.exe` does not read): the text
    /// after `cmd.exe /C`. On Unix they are ordinary arguments after `args`.
    pub raw_args: Vec<OsString>,
    /// The working directory; `None` keeps the current one.
    pub cwd: Option<PathBuf>,
    /// Start from an empty environment instead of the inherited one, so `env` is the child's whole environment.
    pub env_clear: bool,
    /// Variables set on top of the inherited environment (or of none, with `env_clear`).
    pub env: Vec<(OsString, OsString)>,
    /// Inherited variables removed (before `env` is applied).
    pub env_remove: Vec<OsString>,
}

impl Command {
    #[must_use]
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            raw_args: Vec::new(),
            cwd: None,
            env_clear: false,
            env: Vec::new(),
            env_remove: Vec::new(),
        }
    }

    #[must_use]
    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    #[must_use]
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Append `arg` to [`Command::raw_args`].
    #[must_use]
    pub fn raw_arg(mut self, arg: impl Into<OsString>) -> Self {
        self.raw_args.push(arg.into());
        self
    }

    #[must_use]
    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Start the child's environment empty: only what `env` sets reaches it.
    #[must_use]
    pub fn env_clear(mut self) -> Self {
        self.env_clear = true;
        self
    }

    #[must_use]
    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    #[must_use]
    pub fn env_remove(mut self, key: impl Into<OsString>) -> Self {
        self.env_remove.push(key.into());
        self
    }

    /// portable-pty's builder: `args`, then `raw_args`; the environment inherited (none with `env_clear`), minus
    /// `env_remove`, plus `env`;
    /// the working directory `cwd`, else the current one.
    fn builder(&self) -> portable_pty::CommandBuilder {
        let mut builder = portable_pty::CommandBuilder::new(&self.program);
        builder.args(&self.args);
        #[cfg(windows)]
        for arg in &self.raw_args {
            builder.raw_arg(arg);
        }
        #[cfg(not(windows))]
        builder.args(&self.raw_args);
        if self.env_clear {
            builder.env_clear();
        }
        for key in &self.env_remove {
            builder.env_remove(key);
        }
        for (key, value) in &self.env {
            builder.env(key, value);
        }
        match &self.cwd {
            Some(cwd) => builder.cwd(cwd),
            None => {
                if let Ok(cwd) = std::env::current_dir() {
                    builder.cwd(cwd);
                }
            }
        }
        builder
    }
}

/// A PTY's size: cells, and the pixel size of one cell (the window size in pixels is the product, saturated).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Size {
    pub rows: u16,
    pub cols: u16,
    pub cell_width: u32,
    pub cell_height: u32,
}

impl Size {
    /// `rows` by `cols` cells of no known pixel size.
    #[must_use]
    pub fn cells(rows: u16, cols: u16) -> Self {
        Self {
            rows,
            cols,
            cell_width: 0,
            cell_height: 0,
        }
    }

    /// The window's width and height in pixels, each saturated at `u16::MAX`.
    #[must_use]
    pub fn pixels(self) -> (u16, u16) {
        let pixels = |cells: u16, cell: u32| u16::try_from(u32::from(cells).saturating_mul(cell)).unwrap_or(u16::MAX);
        (pixels(self.cols, self.cell_width), pixels(self.rows, self.cell_height))
    }

    fn pty(self) -> portable_pty::PtySize {
        let (pixel_width, pixel_height) = self.pixels();
        portable_pty::PtySize {
            rows: self.rows,
            cols: self.cols,
            pixel_width,
            pixel_height,
        }
    }
}

/// A child just started on its PTY.
#[derive(Debug)]
pub struct Spawned {
    /// The PTY's master, for [`Actor::spawn`].
    pub master: Master,
    pub child: Child,
}

/// Start `command` on a new PTY of `size`.
///
/// # Errors
///
/// The PTY could not be opened (or, on Windows, the child's job created), or the program could not be started.
pub fn spawn(command: &Command, size: Size) -> io::Result<Spawned> {
    sys::spawn(command.builder(), size)
}

/// portable-pty's errors (`anyhow` ones, its causes joined) as `io::Error`s.
fn pty_error(err: &impl std::fmt::Display) -> io::Error {
    io::Error::other(format!("{err:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pixel_size_saturates() {
        let size = Size {
            rows: 24,
            cols: 80,
            cell_width: 10,
            cell_height: 20,
        };
        assert_eq!(size.pixels(), (800, 480));
        let huge = Size {
            cell_width: 10_000,
            ..size
        };
        assert_eq!(huge.pixels().0, u16::MAX);
        assert_eq!(Size::cells(24, 80).pixels(), (0, 0));
    }

    #[test]
    fn the_builder_removes_then_sets_and_keeps_the_given_cwd() {
        let dir = std::env::temp_dir();
        let command = Command::new("prog")
            .args(["a", "b"])
            .raw_arg("\"c d\"")
            .cwd(&dir)
            .env_remove("NGTERM_PTY_TEST_GONE")
            .env("NGTERM_PTY_TEST_SET", "1");
        let builder = command.builder();
        let argv: Vec<_> = builder
            .get_argv()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        #[cfg(not(windows))]
        assert_eq!(argv, ["prog", "a", "b", "\"c d\""]);
        #[cfg(windows)]
        assert_eq!(argv, ["prog", "a", "b"], "a raw argument stays out of the quoted argv");
        assert_eq!(
            builder.get_env("NGTERM_PTY_TEST_SET").and_then(|v| v.to_str()),
            Some("1")
        );
        assert!(builder.get_env("NGTERM_PTY_TEST_GONE").is_none());
        assert_eq!(builder.get_cwd().map(PathBuf::from), Some(dir));
    }

    #[test]
    fn a_cleared_environment_holds_only_what_is_set() {
        let builder = Command::new("prog")
            .env_clear()
            .env("NGTERM_PTY_TEST_SET", "1")
            .builder();
        let env: Vec<_> = builder
            .iter_full_env_as_str()
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect();
        assert_eq!(env, [("NGTERM_PTY_TEST_SET".to_owned(), "1".to_owned())]);
    }
}
