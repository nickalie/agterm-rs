//! The presentation bridge to an attached row's origin as a child process (upstream's `RemotePresentationProcess.swift`):
//! the ssh running `agtermctl zmx present` on the far side. Stdout is read a line at a time on a thread of its own and
//! handed to the main loop, stdin takes the client's frames, and stderr goes to the log, where an ssh failure is worth
//! reading. The core decides everything else ([`agterm_core::remote::client`]).

use std::cell::Cell;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::ExitStatusExt;
use std::process::{ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use agterm_core::remote::frames::{MAX_FRAME_BYTES, MAX_PENDING_FRAMES};
use agterm_core::remote::reconnect::NetworkReports;
use agterm_core::remote::{LinkKey, PresentationLink, PresentationTransport};
use gtk::gio;
use gtk::gio::prelude::*;

use crate::state::{self, AppState, InboundGate, Poster};

/// Runs each bridge through `/usr/bin/env`, so a user's own `ssh` earlier on `PATH` wins.
pub struct ProcessTransport {
    poster: Poster,
}

impl ProcessTransport {
    #[must_use]
    pub fn new(poster: Poster) -> Self {
        Self { poster }
    }
}

impl PresentationTransport for ProcessTransport {
    fn open(&self, argv: &[String], key: LinkKey) -> Box<dyn PresentationLink> {
        Box::new(ProcessLink::spawn(argv, key, &self.poster))
    }
}

/// One child and its pipes.
struct ProcessLink {
    pid: Option<i32>,
    stdin: std::cell::RefCell<Option<ChildStdin>>,
    stopped: Cell<bool>,
    /// The child was reaped, so its pid may name another process now.
    reaped: Arc<AtomicBool>,
}

impl ProcessLink {
    fn spawn(argv: &[String], key: LinkKey, poster: &Poster) -> Self {
        let spawned = Command::new("/usr/bin/env")
            .args(argv)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let reaped = Arc::new(AtomicBool::new(false));
        let mut child = match spawned {
            Ok(child) => child,
            Err(error) => {
                let reason = format!(
                    "could not run {}: {error}",
                    argv.first().map_or("the bridge", String::as_str)
                );
                closed(poster, key, reason);
                return Self {
                    pid: None,
                    stdin: std::cell::RefCell::new(None),
                    stopped: Cell::new(true),
                    reaped,
                };
            }
        };
        let stdin = child.stdin.take();
        if let Some(stdin) = stdin.as_ref() {
            // a frame is a few hundred bytes and the pipe holds 64 KiB: a write that would block means the child
            // stopped reading, which fails the link rather than stalling the main loop
            let fd = stdin.as_raw_fd();
            // SAFETY: `fd` is the child's stdin, owned by `stdin` for this call; F_GETFL/F_SETFL touch only its flags.
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            // SAFETY: as above.
            unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
        }
        if let Some(stderr) = child.stderr.take() {
            let spawned = std::thread::Builder::new()
                .name("agterm.presentation-stderr".to_owned())
                .spawn(move || log_lines(stderr));
            if spawned.is_err() {
                tracing::warn!("presentation bridge stderr is not read");
            }
        }
        let pid = i32::try_from(child.id()).ok();
        let (posting, done) = (poster.clone(), Arc::clone(&reaped));
        let stdout = child.stdout.take();
        let spawned = std::thread::Builder::new()
            .name("agterm.presentation-read".to_owned())
            .spawn(move || {
                if let Some(stdout) = stdout {
                    read_lines(stdout, key, &posting);
                }
                let reason = match child.wait() {
                    Ok(status) => match (status.code(), status.signal()) {
                        (Some(code), _) => format!("exit {code}"),
                        (None, Some(signal)) => format!("signal {signal}"),
                        (None, None) => "ended".to_owned(),
                    },
                    Err(error) => format!("wait failed: {error}"),
                };
                done.store(true, Ordering::SeqCst);
                closed(&posting, key, reason);
            });
        if spawned.is_err() {
            tracing::error!("presentation bridge cannot be read; ending it");
        }
        Self {
            pid,
            stdin: std::cell::RefCell::new(stdin),
            stopped: Cell::new(false),
            reaped,
        }
    }
}

impl PresentationLink for ProcessLink {
    fn send(&self, line: &[u8]) {
        if self.stopped.get() {
            return;
        }
        let written = self
            .stdin
            .borrow_mut()
            .as_mut()
            .is_some_and(|stdin| stdin.write_all(line).is_ok());
        if !written {
            tracing::info!("presentation bridge stopped taking input; ending it");
            self.stop();
        }
    }

    fn stop(&self) {
        if self.stopped.replace(true) {
            return;
        }
        self.stdin.borrow_mut().take();
        if let Some(pid) = self.pid
            && !self.reaped.load(Ordering::SeqCst)
        {
            // SAFETY: plain kill(2) of the bridge's own child, not yet reaped so its pid is still its own.
            unsafe { libc::kill(pid, libc::SIGTERM) };
        }
    }
}

impl Drop for ProcessLink {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Reads stdout one line at a time until its end, handing each to the main loop; a line over the frame limit ends the
/// read, which the child sees as a closed pipe. The reader stops reading while [`MAX_PENDING_FRAMES`] lines wait for
/// the loop, so a busy loop backs the child up into its own pipe.
fn read_lines(stdout: impl Read, key: LinkKey, poster: &Poster) {
    let gate = InboundGate::new(MAX_PENDING_FRAMES);
    let mut reader = BufReader::new(stdout);
    loop {
        let mut line = Vec::new();
        let limit = u64::try_from(MAX_FRAME_BYTES + 1).unwrap_or(u64::MAX);
        match (&mut reader).take(limit).read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if line.last() != Some(&b'\n') {
            tracing::warn!("presentation bridge sent an oversize or unterminated line; ending the read");
            return;
        }
        line.pop();
        gate.acquire();
        let released = gate.clone();
        let task = move |state: &mut AppState| {
            state.viewer_line(key, &line);
            released.release();
        };
        let delivered = poster
            .upgrade()
            .is_some_and(|main_loop| main_loop.post(Box::new(task)).is_ok());
        if !delivered {
            return;
        }
    }
}

fn log_lines(stderr: impl Read) {
    for line in BufReader::new(stderr).lines() {
        match line {
            Ok(line) => tracing::info!(line = %line, "presentation bridge"),
            Err(_) => return,
        }
    }
}

fn closed(poster: &Poster, key: LinkKey, reason: String) {
    state::post(poster, move |state: &mut AppState| state.viewer_closed(key, &reason));
}

thread_local! {
    /// Observing started (once per app).
    static OBSERVING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The logind subscription the wake retry rides on, kept for the app's life.
    static WAKE: std::cell::RefCell<Option<gio::SignalSubscription>> = const { std::cell::RefCell::new(None) };
}

/// Retries the remote links when the network comes back or the machine wakes (upstream's `RemoteLinkObserver`, its
/// path monitor and its screens-did-wake bridge): GIO's network monitor reports every change, the first one being
/// the state at start ([`NetworkReports`]), and logind's `PrepareForSleep(false)` says the machine resumed. A
/// backoff measured on the monotonic clock does not advance across a suspend, so without this a pane could keep
/// waiting for up to five minutes after a resume. Idempotent.
pub fn observe_links(poster: &Poster) {
    if OBSERVING.with(|observing| observing.replace(true)) {
        return;
    }
    let reports = std::rc::Rc::new(std::cell::RefCell::new(NetworkReports::default()));
    let posting = poster.clone();
    gio::NetworkMonitor::default().connect_network_changed(move |_, available| {
        if reports.borrow_mut().changed(available) {
            retry_links(&posting);
        }
    });
    let posting = poster.clone();
    gio::bus_get(gio::BusType::System, None::<&gio::Cancellable>, move |bus| {
        let bus = match bus {
            Ok(bus) => bus,
            Err(error) => {
                tracing::debug!(%error, "no system bus: remote links are not retried on wake");
                return;
            }
        };
        let subscription = bus.subscribe_to_signal(
            Some("org.freedesktop.login1"),
            Some("org.freedesktop.login1.Manager"),
            Some("PrepareForSleep"),
            Some("/org/freedesktop/login1"),
            None,
            gio::DBusSignalFlags::NONE,
            move |signal| {
                if resumed(signal.parameters) {
                    retry_links(&posting);
                }
            },
        );
        WAKE.with(|wake| *wake.borrow_mut() = Some(subscription));
    });
}

/// Whether a `PrepareForSleep` signal's parameters say the machine woke (`false`) rather than goes to sleep.
fn resumed(parameters: &gtk::glib::Variant) -> bool {
    parameters.get::<(bool,)>() == Some((false,))
}

fn retry_links(poster: &Poster) {
    tracing::info!("network back or machine awake: retrying the remote links now");
    state::post(poster, AppState::retry_remote_links);
}

#[cfg(test)]
mod tests {
    use gtk::glib::variant::ToVariant;

    use super::*;

    #[test]
    fn only_a_wake_retries_the_links() {
        assert!(resumed(&(false,).to_variant()));
        assert!(!resumed(&(true,).to_variant()), "going to sleep");
        assert!(!resumed(&("nope",).to_variant()));
    }
}
