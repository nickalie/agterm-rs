//! The app's [`ProcessLauncher`]: hooks and custom commands as detached `/bin/sh -c` children (upstream's
//! `HookProcessRunner.swift` and the spawn half of `CustomCommandRunner.swift`).
//!
//! stdout goes to `/dev/null`; stdin is the bytes the spec carries (written on a thread of its own, closed after, a
//! child leaving without reading them is no failure) or `/dev/null`; stderr is `/dev/null` or, when captured, a temp
//! file rather than a pipe, so a background descendant the command started keeps running after the app quits instead
//! of dying of a broken pipe. A waiter thread reaps the child and reports its [`ProcessExit`] once the stdin write is
//! over, so no input outlives the run it belongs to.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::thread::{self, JoinHandle};

use agterm_host::{ExitHandler, LaunchSpec, LaunchedProcess, ProcessExit, ProcessLauncher, STDERR_TAIL_LIMIT};

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
        let mut child = command.spawn().inspect_err(|_| {
            if let Some(capture) = &capture {
                capture.remove();
            }
        })?;
        let pid = child.id();
        let writer = child
            .stdin
            .take()
            .zip(spec.stdin.clone())
            .map(|(stdin, bytes)| write_stdin(stdin, bytes));
        thread::Builder::new()
            .name(format!("agterm-reap-{pid}"))
            .spawn(move || reap(child, writer, capture, on_exit))?;
        Ok(LaunchedProcess { pid })
    }
}

/// Writes `bytes` to the child's stdin and closes it; why it failed, a broken pipe aside.
fn write_stdin(mut stdin: ChildStdin, bytes: Vec<u8>) -> JoinHandle<Option<String>> {
    thread::spawn(move || match stdin.write_all(&bytes) {
        Ok(()) => None,
        Err(err) if err.kind() == io::ErrorKind::BrokenPipe => None,
        Err(err) => Some(err.to_string()),
    })
}

/// Waits for the child, then for its stdin write, then reads the stderr tail and reports.
fn reap(
    mut child: Child,
    writer: Option<JoinHandle<Option<String>>>,
    capture: Option<StderrFile>,
    on_exit: ExitHandler,
) {
    let status = child.wait().map_or(-1, status_code);
    let stdin_failure = writer.and_then(|writer| writer.join().unwrap_or(Some("the stdin writer panicked".to_owned())));
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

/// A temp file the child's stderr goes to.
struct StderrFile {
    path: PathBuf,
}

impl StderrFile {
    /// `None` when the file cannot be created; stderr then goes to `/dev/null`.
    fn create() -> Option<Self> {
        let path = std::env::temp_dir().join(format!("agterm-command-{}.err", uuid::Uuid::new_v4()));
        match File::create(&path) {
            Ok(_) => Some(Self { path }),
            Err(err) => {
                tracing::warn!(%err, "stderr capture unavailable");
                None
            }
        }
    }

    fn writer(&self) -> io::Result<File> {
        fs::OpenOptions::new().append(true).open(&self.path)
    }

    /// The last [`STDERR_TAIL_LIMIT`] bytes written so far, the file removed after. The cap is on the read: a
    /// descendant still holding the file keeps growing the unlinked inode until it exits.
    fn consume(self) -> Vec<u8> {
        let tail = read_tail(&self.path).unwrap_or_default();
        self.remove();
        tail
    }

    fn remove(&self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn read_tail(path: &std::path::Path) -> io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let size = file.seek(SeekFrom::End(0))?;
    let wanted = size.min(STDERR_TAIL_LIMIT as u64);
    file.seek(SeekFrom::Start(size - wanted))?;
    let mut tail = Vec::new();
    file.take(wanted).read_to_end(&mut tail)?;
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
    fn a_missing_program_fails_to_launch() {
        let spec = LaunchSpec {
            program: "/nonexistent/agterm-test".into(),
            capture_stderr: true,
            ..LaunchSpec::default()
        };
        assert!(SystemLauncher.launch(&spec, Box::new(|_| ())).is_err());
    }
}
