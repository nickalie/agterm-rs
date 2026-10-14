//! The presentation bridge to an attached row's origin as a child process (upstream's `RemotePresentationProcess.swift`):
//! the ssh running `agtermctl zmx present` on the far side. Stdout is read a line at a time on a thread of its own and
//! handed to the main loop, stdin takes the client's frames, and stderr goes to the log, where an ssh failure is worth
//! reading. The core decides everything else ([`agterm_core::remote::client`]).
//!
//! The frontend's main-loop state takes the lines and the close ([`PresentationViewer`]); the network and resume
//! observer that retries the links stays with each frontend, since it rides on its platform's event sources.

use std::cell::{Cell, RefCell};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::ExitStatusExt;
use std::process::{ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};

use agterm_core::remote::frames::{MAX_FRAME_BYTES, MAX_PENDING_FRAMES};
use agterm_core::remote::{LinkKey, PresentationLink, PresentationTransport};
use agterm_host::MainLoop;

/// The main-loop state a presentation bridge reports to: a line it read, and its end with the reason.
pub trait PresentationViewer: 'static {
    /// A line a presentation bridge read.
    fn viewer_line(&mut self, key: LinkKey, line: &[u8]);

    /// A presentation bridge ended.
    fn viewer_closed(&mut self, key: LinkKey, reason: &str);
}

/// Where a bridge's reader threads post: the main loop, held weakly so a reader outliving it posts nothing.
type Poster<S> = Weak<dyn MainLoop<S>>;

/// Bounds the lines a stream's reader may have waiting for the main loop: the reader blocks in
/// [`InboundGate::acquire`] past the limit until a task releases its place (upstream's inbound semaphore).
#[derive(Clone)]
pub struct InboundGate(Arc<(Mutex<usize>, Condvar)>);

impl InboundGate {
    #[must_use]
    pub fn new(limit: usize) -> Self {
        Self(Arc::new((Mutex::new(limit), Condvar::new())))
    }

    pub fn acquire(&self) {
        let (free, wake) = &*self.0;
        let mut free = free.lock().unwrap_or_else(PoisonError::into_inner);
        while *free == 0 {
            free = wake.wait(free).unwrap_or_else(PoisonError::into_inner);
        }
        *free -= 1;
    }

    pub fn release(&self) {
        let (free, wake) = &*self.0;
        *free.lock().unwrap_or_else(PoisonError::into_inner) += 1;
        wake.notify_one();
    }
}

/// Runs each bridge through `/usr/bin/env`, so a user's own `ssh` earlier on `PATH` wins.
pub struct ProcessTransport<S> {
    poster: Poster<S>,
}

impl<S> ProcessTransport<S> {
    #[must_use]
    pub fn new(poster: Poster<S>) -> Self {
        Self { poster }
    }
}

impl<S: PresentationViewer> PresentationTransport for ProcessTransport<S> {
    fn open(&self, argv: &[String], key: LinkKey) -> Box<dyn PresentationLink> {
        Box::new(ProcessLink::spawn(argv, key, &self.poster))
    }
}

/// One child and its pipes.
struct ProcessLink {
    pid: Option<i32>,
    stdin: RefCell<Option<ChildStdin>>,
    stopped: Cell<bool>,
    /// The child was reaped, so its pid may name another process now.
    reaped: Arc<AtomicBool>,
}

impl ProcessLink {
    fn spawn<S: PresentationViewer>(argv: &[String], key: LinkKey, poster: &Poster<S>) -> Self {
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
                    stdin: RefCell::new(None),
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
            stdin: RefCell::new(stdin),
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
fn read_lines<S: PresentationViewer>(stdout: impl Read, key: LinkKey, poster: &Poster<S>) {
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
        let task = move |state: &mut S| {
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

/// Reports the bridge's end to the main loop; a closed or gone loop drops it.
fn closed<S: PresentationViewer>(poster: &Poster<S>, key: LinkKey, reason: String) {
    let Some(main_loop) = poster.upgrade() else {
        return;
    };
    let task = move |state: &mut S| state.viewer_closed(key, &reason);
    if main_loop.post(Box::new(task)).is_err() {
        tracing::debug!("main-loop task dropped: the loop is closed");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use agterm_host::fakes::FakeMainLoop;
    use uuid::Uuid;

    use super::*;

    /// What the bridge reported to the loop, in order.
    #[derive(Debug, PartialEq, Eq)]
    enum Report {
        Line(String),
        Closed(String),
    }

    struct Viewer(mpsc::Sender<Report>);

    impl PresentationViewer for Viewer {
        fn viewer_line(&mut self, _key: LinkKey, line: &[u8]) {
            self.0
                .send(Report::Line(String::from_utf8_lossy(line).into_owned()))
                .unwrap();
        }

        fn viewer_closed(&mut self, _key: LinkKey, reason: &str) {
            self.0.send(Report::Closed(reason.to_owned())).unwrap();
        }
    }

    const KEY: LinkKey = LinkKey {
        session: Uuid::nil(),
        client: 1,
        launch: 1,
    };

    /// Opens a bridge running `script` under `/bin/sh` and collects its reports until it closes.
    fn reports_of(script: &str) -> Vec<Report> {
        let (sender, receiver) = mpsc::channel();
        let main_loop = Arc::new(FakeMainLoop::spawn(Viewer(sender)).unwrap());
        let poster: Poster<Viewer> = Arc::downgrade(&main_loop) as Poster<Viewer>;
        let transport = ProcessTransport::new(poster);
        let argv = ["sh".to_owned(), "-c".to_owned(), script.to_owned()];
        let link = transport.open(&argv, KEY);
        let mut reports = Vec::new();
        loop {
            let report = receiver.recv_timeout(Duration::from_secs(10)).unwrap();
            let closed = matches!(report, Report::Closed(_));
            reports.push(report);
            if closed {
                break;
            }
        }
        drop(link);
        main_loop.close();
        reports
    }

    #[test]
    fn the_bridge_hands_each_line_to_the_loop_then_its_exit() {
        assert_eq!(
            reports_of("printf 'one\\ntwo\\n'"),
            [
                Report::Line("one".to_owned()),
                Report::Line("two".to_owned()),
                Report::Closed("exit 0".to_owned()),
            ]
        );
    }

    #[test]
    fn an_unterminated_line_is_not_handed_on() {
        assert_eq!(
            reports_of("printf 'whole\\npartial'; exit 3"),
            [Report::Line("whole".to_owned()), Report::Closed("exit 3".to_owned())]
        );
    }

    #[test]
    fn the_inbound_gate_holds_the_reader_past_its_limit_until_a_line_is_handled() {
        let gate = InboundGate::new(2);
        gate.acquire();
        gate.acquire();
        let (done, finished) = std::sync::mpsc::channel();
        let reader = gate.clone();
        let blocked = std::thread::spawn(move || {
            reader.acquire();
            done.send(()).unwrap();
        });
        assert!(
            finished.recv_timeout(Duration::from_millis(100)).is_err(),
            "a third line waits"
        );
        gate.release();
        finished.recv_timeout(Duration::from_secs(5)).unwrap();
        blocked.join().unwrap();
    }
}
