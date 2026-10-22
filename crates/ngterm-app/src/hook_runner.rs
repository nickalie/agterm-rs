//! The app's [`ProcessLauncher`]: hooks and custom commands as detached `/bin/sh -c` children (upstream's
//! `HookProcessRunner.swift` and the spawn half of `CustomCommandRunner.swift`).
//!
//! stdout goes to `/dev/null`; stdin is the bytes the spec carries (written on a thread of its own, closed after, a
//! child leaving without reading them is no failure) or `/dev/null`; stderr is `/dev/null` or, when captured, an
//! owner-only temp file unlinked as soon as it is open, rather than a pipe, so a background descendant the command
//! started keeps running after the app quits instead of dying of a broken pipe. A waiter thread reaps the child,
//! stops a stdin write still pending (a descendant may hold the pipe without ever reading it; upstream closes the
//! channel with `.stop` on exit), and reports its [`ProcessExit`], so no input outlives the run it belongs to.

use std::fs::{self, File};
use std::io::{self, PipeReader, PipeWriter, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::thread::{self, JoinHandle};

use ngterm_host::{ExitHandler, LaunchSpec, LaunchedProcess, ProcessExit, ProcessLauncher, STDERR_TAIL_LIMIT};

/// Starts processes on the host.
#[derive(Debug, Default)]
pub struct SystemLauncher;

impl ProcessLauncher for SystemLauncher {
    fn launch(&self, spec: &LaunchSpec, on_exit: ExitHandler) -> io::Result<LaunchedProcess> {
        let capture = if spec.capture_stderr {
            StderrFile::create()
        } else {
            None
        };
        // made before the spawn, so nothing can fail between the spawn and the reaper taking the child
        let stop = spec.stdin.as_ref().map(|_| io::pipe()).transpose()?;
        let mut command = Command::new(&spec.program);
        command
            .args(&spec.args)
            .envs(spec.env.iter().map(|(key, value)| (key, value)))
            .stdout(Stdio::null())
            .stdin(if spec.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stderr(match capture.as_ref().map(StderrFile::writer) {
                Some(Ok(file)) => Stdio::from(file),
                _ => Stdio::null(),
            })
            // its own process group: a terminal's ^C aimed at the app does not reach it
            .process_group(0);
        if let Some(cwd) = &spec.cwd {
            command.current_dir(cwd);
        }
        let mut child = command.spawn()?;
        let pid = child.id();
        let writer = child
            .stdin
            .take()
            .zip(spec.stdin.clone())
            .zip(stop)
            .map(|((stdin, bytes), (stop_reader, stop))| StdinWriter::start(stdin, bytes, stop_reader, stop));
        thread::Builder::new()
            .name(format!("ngterm-reap-{pid}"))
            .spawn(move || reap(child, writer, capture, on_exit))?;
        Ok(LaunchedProcess { pid })
    }
}

/// The thread writing a child's stdin, and the pipe that stops it.
struct StdinWriter {
    thread: JoinHandle<Option<String>>,
    /// Dropped to stop the write: the thread's end of the pipe then polls as hung up.
    stop: PipeWriter,
}

impl StdinWriter {
    /// Writes `bytes` to `stdin` on a thread of its own, then closes it.
    fn start(stdin: ChildStdin, bytes: Vec<u8>, stop_reader: PipeReader, stop: PipeWriter) -> Self {
        let thread = thread::spawn(move || write_until_stopped(stdin, &bytes, &stop_reader));
        Self { thread, stop }
    }

    /// The child exited: a write still pending is stopped (only a descendant could still read it, and upstream
    /// drops it too), then why the delivery failed, a broken pipe or the stop aside.
    fn finish(self) -> Option<String> {
        drop(self.stop);
        self.thread
            .join()
            .unwrap_or_else(|_| Some("the stdin writer panicked".to_owned()))
    }
}

/// Writes `bytes` to `stdin` without blocking past a stop: the pipe is non-blocking, and a full one is waited on
/// together with `stop`. Why it failed, a broken pipe (the reader left) or a stop aside.
fn write_until_stopped(mut stdin: ChildStdin, bytes: &[u8], stop: &PipeReader) -> Option<String> {
    if let Err(err) = set_nonblocking(stdin.as_fd()) {
        return Some(err.to_string());
    }
    let mut rest = bytes;
    while !rest.is_empty() {
        match stdin.write(rest) {
            Ok(0) => return Some(io::Error::from(io::ErrorKind::WriteZero).to_string()),
            Ok(written) => rest = &rest[written..],
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => match wait_writable(stdin.as_fd(), stop.as_fd()) {
                Ok(true) => {}
                Ok(false) => return None,
                Err(err) => return Some(err.to_string()),
            },
            Err(err) if err.kind() == io::ErrorKind::BrokenPipe => return None,
            Err(err) => return Some(err.to_string()),
        }
    }
    None
}

/// Puts the app's end of the stdin pipe in non-blocking mode (the child's end is a separate open file, untouched).
fn set_nonblocking(fd: BorrowedFd<'_>) -> io::Result<()> {
    // SAFETY: `fd` is an open descriptor for the borrow's lifetime; F_GETFL only reads its status flags.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: as above; F_SETFL only changes the status flags of this open file.
    let set = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if set < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Waits until `pipe` takes more bytes, or its reader left (`true`: the next write tells which), or `stop` is
/// written to or closed (`false`).
fn wait_writable(pipe: BorrowedFd<'_>, stop: BorrowedFd<'_>) -> io::Result<bool> {
    let mut fds = [
        libc::pollfd {
            fd: pipe.as_raw_fd(),
            events: libc::POLLOUT,
            revents: 0,
        },
        libc::pollfd {
            fd: stop.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    loop {
        // SAFETY: `fds` is a live array of two initialized pollfd entries whose descriptors stay open for the call.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
        if ready >= 0 {
            return Ok(fds[1].revents == 0);
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

/// Waits for the child, then stops and joins its stdin write, then reads the stderr tail and reports.
fn reap(mut child: Child, writer: Option<StdinWriter>, capture: Option<StderrFile>, on_exit: ExitHandler) {
    let status = child.wait().map_or(-1, status_code);
    let stdin_failure = writer.and_then(StdinWriter::finish);
    let stderr_tail = capture.map(StderrFile::consume).unwrap_or_default();
    on_exit(ProcessExit {
        status,
        stderr_tail,
        stdin_failure,
    });
}

/// The exit code, or the signal that ended the process (Foundation's `terminationStatus` for an uncaught signal).
fn status_code(status: ExitStatus) -> i32 {
    status.code().or_else(|| status.signal()).unwrap_or(-1)
}

/// The file the child's stderr goes to: created owner-only (0600) under a fresh name and unlinked at once, so no
/// other user can open it and nothing is left behind; the app reads the tail through its own descriptor.
struct StderrFile {
    file: File,
}

impl StderrFile {
    /// `None` when the file cannot be created; stderr then goes to `/dev/null`.
    fn create() -> Option<Self> {
        let path = std::env::temp_dir().join(format!("ngterm-command-{}.err", uuid::Uuid::new_v4()));
        let opened = fs::OpenOptions::new()
            .read(true)
            .append(true)
            .create_new(true)
            .mode(0o600)
            .open(&path);
        match opened {
            Ok(file) => {
                let _ = fs::remove_file(&path);
                Some(Self { file })
            }
            Err(err) => {
                tracing::warn!(%err, "stderr capture unavailable");
                None
            }
        }
    }

    /// The child's descriptor: the same open file, appending.
    fn writer(&self) -> io::Result<File> {
        self.file.try_clone()
    }

    /// The last [`STDERR_TAIL_LIMIT`] bytes written so far. The cap is on the read: a descendant still holding the
    /// file keeps growing the unlinked inode until it exits.
    fn consume(self) -> Vec<u8> {
        read_tail(&self.file).unwrap_or_default()
    }
}

fn read_tail(file: &File) -> io::Result<Vec<u8>> {
    let size = file.metadata()?.len();
    let start = size.saturating_sub(STDERR_TAIL_LIMIT as u64);
    let mut tail = Vec::new();
    let mut chunk = [0; 4096];
    let mut offset = start;
    while offset < size {
        match file.read_at(&mut chunk, offset) {
            Ok(0) => break,
            Ok(read) => {
                let wanted = usize::try_from(size - offset).map_or(read, |left| read.min(left));
                tail.extend_from_slice(&chunk[..wanted]);
                offset += wanted as u64;
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    Ok(tail)
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    fn run(spec: &LaunchSpec) -> ProcessExit {
        let (tx, rx) = mpsc::channel();
        SystemLauncher
            .launch(spec, Box::new(move |exit| tx.send(exit).unwrap()))
            .unwrap();
        rx.recv_timeout(Duration::from_secs(10)).unwrap()
    }

    fn sh(line: &str) -> LaunchSpec {
        LaunchSpec {
            program: "/bin/sh".into(),
            args: vec!["-c".to_owned(), line.to_owned()],
            ..LaunchSpec::default()
        }
    }

    #[test]
    fn reports_the_exit_status_and_the_stderr_tail() {
        let exit = run(&LaunchSpec {
            capture_stderr: true,
            ..sh("echo oops >&2; exit 3")
        });
        assert_eq!(exit.status, 3);
        assert_eq!(exit.stderr_tail, b"oops\n");
        assert_eq!(exit.stdin_failure, None);
    }

    #[test]
    fn stdin_env_and_cwd_reach_the_child() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        let exit = run(&LaunchSpec {
            env: vec![("AGT_X".to_owned(), "x".to_owned())],
            cwd: Some(dir.path().to_owned()),
            stdin: Some(b"payload\n".to_vec()),
            ..sh("read line; printf '%s|%s|%s' \"$line\" \"$AGT_X\" \"$(basename \"$PWD\")\" > out")
        });
        assert_eq!(exit.status, 0);
        let name = dir.path().file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(fs::read_to_string(out).unwrap(), format!("payload|x|{name}"));
    }

    #[test]
    fn a_child_leaving_without_reading_is_no_delivery_failure() {
        let exit = run(&LaunchSpec {
            stdin: Some(vec![b'x'; 1 << 20]),
            ..sh("exit 0")
        });
        assert_eq!((exit.status, exit.stdin_failure), (0, None));
    }

    #[test]
    fn a_descendant_holding_stdin_unread_does_not_hold_back_the_exit() {
        // upstream's testAGrandchildHoldingStdinDoesNotDelayExitOrCleanup: more than a pipe buffer of input, and a
        // background descendant that keeps the pipe open without reading it
        let (tx, rx) = mpsc::channel();
        let launched = SystemLauncher
            .launch(
                &LaunchSpec {
                    stdin: Some(vec![b'x'; 300_000]),
                    ..sh("exec 3<&0; (sleep 30 <&3 3<&-) & exec 3<&-; exit 0")
                },
                Box::new(move |exit| tx.send(exit).unwrap()),
            )
            .unwrap();
        let exit = rx.recv_timeout(Duration::from_secs(5));
        // the descendant is in the child's process group
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{}", launched.pid)])
            .status();
        let exit = exit.expect("the exit is reported while the descendant still holds stdin");
        assert_eq!((exit.status, exit.stdin_failure), (0, None));
    }

    #[test]
    fn the_stderr_capture_is_owner_only_and_already_unlinked() {
        let exit = run(&LaunchSpec {
            capture_stderr: true,
            ..sh("stat -L -c %a /proc/$$/fd/2 >&2; readlink /proc/$$/fd/2 >&2")
        });
        let text = String::from_utf8(exit.stderr_tail).unwrap();
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some("600"));
        assert!(lines.next().is_some_and(|link| link.ends_with(" (deleted)")), "{text}");
    }

    #[test]
    fn only_the_stderr_tail_comes_back() {
        let exit = run(&LaunchSpec {
            capture_stderr: true,
            ..sh(&format!(
                "head -c {} /dev/zero >&2; printf end >&2",
                STDERR_TAIL_LIMIT * 2
            ))
        });
        assert_eq!(exit.stderr_tail.len(), STDERR_TAIL_LIMIT);
        assert!(exit.stderr_tail.ends_with(b"end"));
    }

    #[test]
    fn a_missing_program_fails_to_launch() {
        let spec = LaunchSpec {
            program: "/nonexistent/ngterm-test".into(),
            capture_stderr: true,
            ..LaunchSpec::default()
        };
        assert!(SystemLauncher.launch(&spec, Box::new(|_| ())).is_err());
    }
}
