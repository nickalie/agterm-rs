//! The PTY layer on real PTYs, on every OS: a child spawned with its argv, environment and working directory, its
//! output read and its input written through the actor, a resize reaching it, its tree killed, its exit code, and
//! the handoff's quiesce, rollback and release with a second actor taking the PTY over.
//!
//! The commands are per OS: `/bin/sh` on Unix, `cmd.exe` and Windows PowerShell on Windows. Every child gets the
//! test's environment without the live agterm's `AGTERM_*` and `ZMX_*` variables.

use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use agterm_pty::{
    Actor, ActorConfig, ActorHandle, Child, Command, ExitReason, ExitStatus, Master, ReadCallback, Size, Spawned,
    WriteError,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// How long a test waits for output (Windows PowerShell can take seconds to start on a CI runner).
const PATIENCE: Duration = Duration::from_secs(60);

/// `ConPTY` asks its terminal for the cursor position before the child runs (`PSEUDOCONSOLE_INHERIT_CURSOR`).
const CURSOR_QUERY: &[u8] = b"\x1b[6n";

/// `program` with the live agterm's variables removed from the inherited environment.
fn command(program: &str) -> Command {
    std::env::vars_os()
        .map(|(key, _)| key)
        .filter(|key| {
            key.to_str()
                .is_some_and(|key| key.starts_with("AGTERM_") || key.starts_with("ZMX_"))
        })
        .fold(Command::new(program), Command::env_remove)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A child on a PTY with an actor gathering its output (and answering the cursor queries).
struct Session {
    handle: ActorHandle,
    child: Child,
    output: Arc<Mutex<Vec<u8>>>,
    exit: Receiver<ExitReason>,
}

/// The read callback of a session: gather the output, answer every new cursor query.
fn gather(output: &Arc<Mutex<Vec<u8>>>) -> ReadCallback {
    let output = Arc::clone(output);
    let mut answered = 0;
    Box::new(move |bytes| {
        let mut output = lock(&output);
        output.extend_from_slice(bytes);
        let queries = output
            .windows(CURSOR_QUERY.len())
            .filter(|window| *window == CURSOR_QUERY)
            .count();
        let replies = (answered..queries).map(|_| b"\x1b[1;1R".to_vec()).collect();
        answered = queries;
        replies
    })
}

fn start(command: &Command, size: Size) -> TestResult<Session> {
    let Spawned { master, child } = agterm_pty::spawn(command, size)?;
    let output = Arc::new(Mutex::new(Vec::new()));
    let (exits, exit) = mpsc::channel();
    let mut config = ActorConfig::new("test", master, gather(&output));
    config.on_exit = Some(Box::new(move |reason| {
        let _ = exits.send(reason);
    }));
    Ok(Session {
        handle: Actor::spawn(config)?,
        child,
        output,
        exit,
    })
}

impl Session {
    fn text(&self) -> String {
        String::from_utf8_lossy(&lock(&self.output)).into_owned()
    }

    /// Wait for `needle` in the output; the output so far when it never comes.
    fn expect(&self, needle: &str) {
        let deadline = Instant::now() + PATIENCE;
        while !self.text().contains(needle) {
            assert!(
                Instant::now() < deadline,
                "no {needle:?} in the output: {:?}",
                self.text()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn type_line(&self, line: &str) -> TestResult {
        let enter = if cfg!(windows) { "\r" } else { "\n" };
        self.handle.write_input(format!("{line}{enter}").as_bytes())?;
        Ok(())
    }

    /// End [`shell`]: `exit` for cmd, the end of input for the read loop.
    fn quit(&self) -> TestResult {
        if cfg!(windows) {
            self.type_line("exit")
        } else {
            Ok(self.handle.write_input(b"\x04")?)
        }
    }

    /// Wait for the child, then for the actor to stop with the PTY's end.
    fn finish(mut self) -> TestResult<ExitStatus> {
        let status = self.child.wait()?;
        self.handle.child_exited();
        let reason = self.exit.recv_timeout(PATIENCE)?;
        assert_eq!(reason, ExitReason::Eof, "{:?}", self.text());
        Ok(status)
    }
}

/// A shell that runs each line it reads: `cmd /q /k` (no prompt noise), or `sh` evaluating its input lines (no
/// prompt at all). The terminal echoes each typed line, so the lines below print what they type in another form.
fn shell() -> Command {
    if cfg!(windows) {
        command("cmd.exe").args(["/d", "/q", "/k"])
    } else {
        command("/bin/sh").args(["-c", "while read line; do eval \"$line\"; done"])
    }
}

/// The line that makes [`shell`] print `got:<word>` (the typed line, `got^:` or `got"":`, does not match it).
fn tagged(word: &str) -> String {
    if cfg!(windows) {
        format!("echo got^:{word}")
    } else {
        format!("echo got\"\":{word}")
    }
}

/// The line that makes [`shell`] print `SIZE=<rows>x<cols>` as the child sees its terminal.
fn size_line() -> String {
    if cfg!(windows) {
        "powershell.exe -NoLogo -NoProfile -NonInteractive -Command \"'SIZE=' + [Console]::WindowHeight + 'x' + \
         [Console]::WindowWidth\""
            .to_owned()
    } else {
        "echo \"SIZE=$(stty size | tr ' ' x)\"".to_owned()
    }
}

#[test]
fn input_round_trips_through_the_child() -> TestResult {
    let session = start(&shell(), Size::cells(24, 80))?;
    session.type_line(&tagged("ping"))?;
    session.expect("got:ping");
    session.type_line(&tagged("pong"))?;
    session.expect("got:pong");
    session.quit()?;
    assert!(session.finish()?.success());
    Ok(())
}

#[test]
fn a_resize_reaches_the_child() -> TestResult {
    let session = start(&shell(), Size::cells(24, 80))?;
    session.handle.resize(Size {
        rows: 30,
        cols: 100,
        cell_width: 8,
        cell_height: 16,
    });
    session.type_line(&size_line())?;
    session.expect("SIZE=30x100");
    session.quit()?;
    assert!(session.finish()?.success());
    Ok(())
}

#[test]
fn the_exit_code_is_the_childs() -> TestResult {
    let exit = |code| {
        if cfg!(windows) {
            command("cmd.exe").args(["/d", "/c", &format!("exit {code}")])
        } else {
            command("/bin/sh").args(["-c", &format!("exit {code}")])
        }
    };
    let status = start(&exit(7), Size::cells(24, 80))?.finish()?;
    assert_eq!(status.exit_code(), 7);
    assert!(!status.success());
    assert!(start(&exit(0), Size::cells(24, 80))?.finish()?.success());
    Ok(())
}

/// Once `try_wait` has the status, `try_wait` and `wait` keep returning it (on Unix without waiting by the released
/// id again).
#[test]
fn the_status_stays_readable_after_the_child_is_reaped() -> TestResult {
    let program = if cfg!(windows) {
        command("cmd.exe").args(["/d", "/c", "exit 5"])
    } else {
        command("/bin/sh").args(["-c", "exit 5"])
    };
    let mut session = start(&program, Size::cells(24, 80))?;
    let deadline = Instant::now() + PATIENCE;
    let status = loop {
        if let Some(status) = session.child.try_wait()? {
            break status;
        }
        assert!(Instant::now() < deadline, "the child never exited");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status.exit_code(), 5);
    assert_eq!(session.child.try_wait()?.map(|status| status.exit_code()), Some(5));
    assert_eq!(session.child.wait()?.exit_code(), 5);
    assert_eq!(session.finish()?.exit_code(), 5);
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_signal_ending_the_child_is_reported() -> TestResult {
    let session = start(&command("/bin/sh").args(["-c", "kill -TERM $$"]), Size::cells(24, 80))?;
    let status = session.finish()?;
    assert!(!status.success());
    assert!(status.signal().is_some(), "{status:?}");
    Ok(())
}

#[test]
fn the_environment_and_the_working_directory_are_as_given() -> TestResult {
    let dir = tempfile::tempdir()?;
    // the real path, as `pwd -P` prints it (macOS' /var is a link); on Windows without the `\\?\` prefix
    // canonicalize adds, which cmd refuses as a working directory
    let shown = dir
        .path()
        .canonicalize()?
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .to_owned();
    let cwd = std::path::PathBuf::from(&shown);
    let (command, removed) = if cfg!(windows) {
        let command = command("cmd.exe").args(["/d", "/c", "echo [%AGTERM_PTY_TEST%][%USERNAME%][%CD%]"]);
        (command, "USERNAME")
    } else {
        let command = command("/bin/sh").args(["-c", "echo \"[$AGTERM_PTY_TEST][${HOME-unset}][$(pwd -P)]\""]);
        (command, "HOME")
    };
    let command = command.env("AGTERM_PTY_TEST", "a value").env_remove(removed).cwd(&cwd);
    let session = start(&command, Size::cells(24, 200))?;
    let unset = if cfg!(windows) { "%USERNAME%" } else { "unset" };
    session.expect(&format!("[a value][{unset}][{shown}]"));
    assert!(session.finish()?.success());
    Ok(())
}

#[test]
fn killing_the_tree_ends_the_child_and_its_descendants() -> TestResult {
    let command = if cfg!(windows) {
        command("cmd.exe").args([
            "/d",
            "/c",
            "start /b ping -n 300 127.0.0.1 >nul & echo started & ping -n 300 127.0.0.1 >nul",
        ])
    } else {
        command("/bin/sh").args(["-c", "sleep 300 & echo \"started:$!:\"; wait"])
    };
    let mut session = start(&command, Size::cells(24, 80))?;
    session.expect("started");
    let descendants = descendants(&session)?;
    session.child.killer().kill_tree()?;
    let status = session.child.wait()?;
    assert!(!status.success());
    descendants.assert_gone()?;
    session.handle.child_exited();
    assert_eq!(session.exit.recv_timeout(PATIENCE)?, ExitReason::Eof);
    // signalling a tree that is gone is no error
    session.child.killer().kill_tree()?;
    Ok(())
}

/// The child's descendants, to check they ended.
#[cfg(unix)]
struct Descendants(libc::pid_t);

#[cfg(unix)]
fn descendants(session: &Session) -> TestResult<Descendants> {
    let text = session.text();
    let pid = text
        .split("started:")
        .nth(1)
        .and_then(|rest| rest.split(':').next())
        .ok_or("no background pid in the output")?;
    Ok(Descendants(pid.parse()?))
}

#[cfg(unix)]
impl Descendants {
    /// The background `sleep` is gone (or a zombie its new parent has yet to reap).
    fn assert_gone(&self) -> TestResult {
        let deadline = Instant::now() + PATIENCE;
        loop {
            // SAFETY: probes a process id with signal 0; no memory is passed.
            let alive = unsafe { libc::kill(self.0, 0) } == 0;
            if !alive || is_zombie(self.0) {
                return Ok(());
            }
            assert!(Instant::now() < deadline, "the background process {} survived", self.0);
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

#[cfg(target_os = "linux")]
fn is_zombie(pid: libc::pid_t) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| stat.rsplit_once(") ").map(|(_, rest)| rest.starts_with('Z')))
        .unwrap_or(true)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn is_zombie(pid: libc::pid_t) -> bool {
    std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .map_or(true, |out| String::from_utf8_lossy(&out.stdout).trim().starts_with('Z'))
}

/// The child's job, to count the processes left in it.
#[cfg(windows)]
struct Descendants(std::os::windows::io::OwnedHandle);

#[cfg(windows)]
fn descendants(session: &Session) -> TestResult<Descendants> {
    let descendants = Descendants(session.child.job().try_clone_to_owned()?);
    let deadline = Instant::now() + PATIENCE;
    // cmd and its two pings
    while descendants.active()? < 3 {
        assert!(Instant::now() < deadline, "the job never held the child's tree");
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(descendants)
}

#[cfg(windows)]
impl Descendants {
    fn active(&self) -> TestResult<u32> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::{
            JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JobObjectBasicAccountingInformation, QueryInformationJobObject,
        };
        let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        let len = u32::try_from(size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>())?;
        // SAFETY: fills a live local of the class's struct type and its exact size, for a job handle owned here.
        let ok = unsafe {
            QueryInformationJobObject(
                self.0.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                (&raw mut info).cast(),
                len,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(info.ActiveProcesses)
    }

    fn assert_gone(&self) -> TestResult {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let active = self.active()?;
            if active == 0 {
                return Ok(());
            }
            assert!(Instant::now() < deadline, "{active} processes survived in the job");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

#[test]
fn a_quiesced_actor_refuses_input_until_rolled_back() -> TestResult {
    let session = start(&shell(), Size::cells(24, 80))?;
    session.type_line(&tagged("before"))?;
    session.expect("got:before");
    session.handle.begin_handoff(PATIENCE)?;
    assert_eq!(session.handle.write_input(b"x"), Err(WriteError::Closed));
    session.handle.rollback_handoff()?;
    session.type_line(&tagged("after"))?;
    session.expect("got:after");
    session.quit()?;
    assert!(session.finish()?.success());
    Ok(())
}

#[test]
fn a_released_pty_passes_to_a_second_actor_with_the_child_alive() -> TestResult {
    let mut first = start(&shell(), Size::cells(24, 80))?;
    first.type_line(&tagged("one"))?;
    first.expect("got:one");
    first.handle.begin_handoff(PATIENCE)?;
    let master = take_over(&first.handle)?;
    let released = first.handle.release_after_commit()?;
    assert_eq!(first.exit.recv_timeout(PATIENCE)?, ExitReason::Released);
    assert!(
        first.child.try_wait()?.is_none(),
        "the child ended with the first actor"
    );

    let output = Arc::new(Mutex::new(released.pending_output));
    let mut config = ActorConfig::new("taken-over", master, gather(&output));
    config.initially_quiesced = true;
    let second = Actor::spawn(config)?;
    assert_eq!(second.write_input(b"x"), Err(WriteError::Closed));
    second.rollback_handoff()?;
    let session = Session {
        handle: second,
        child: first.child,
        output,
        exit: mpsc::channel().1,
    };
    session.type_line(&tagged("two"))?;
    session.expect("got:two");
    // a resize through the taken-over master reaches the child (on Windows, through the rebuilt pseudoconsole)
    session.handle.resize(Size::cells(30, 100));
    session.type_line(&size_line())?;
    session.expect("SIZE=30x100");
    session.quit()?;
    let mut child = session.child;
    assert!(child.wait()?.success());
    Ok(())
}

/// A handoff aborted before its commit: the actor over the taken-over master, started quiesced, is dropped without
/// owning the PTY, so it reads none of the output, and the first actor rolls back with all of it.
#[test]
fn an_aborted_takeover_leaves_the_output_to_the_first_actor() -> TestResult {
    let first = start(&shell(), Size::cells(24, 80))?;
    first.type_line(&tagged("one"))?;
    first.expect("got:one");
    first.handle.begin_handoff(PATIENCE)?;
    let master = take_over(&first.handle)?;
    let taken = Arc::new(Mutex::new(Vec::new()));
    let (exits, exit) = mpsc::channel();
    let mut config = ActorConfig::new("aborted", master, gather(&taken));
    config.initially_quiesced = true;
    config.on_exit = Some(Box::new(move |reason| {
        let _ = exits.send(reason);
    }));
    drop(Actor::spawn(config)?);
    assert_eq!(exit.recv_timeout(PATIENCE)?, ExitReason::Shutdown);

    first.handle.rollback_handoff()?;
    for round in 0..5 {
        let word = format!("again{round}");
        first.type_line(&tagged(&word))?;
        first.expect(&format!("got:{word}"));
    }
    assert!(lock(&taken).is_empty(), "the dropped actor read {:?}", lock(&taken));
    first.quit()?;
    assert!(first.finish()?.success());
    Ok(())
}

/// The master the second actor takes over: a duplicate of the first one's.
#[cfg(unix)]
fn take_over(handle: &ActorHandle) -> TestResult<Master> {
    Ok(Master::from_fd(handle.duplicate_for_handoff()?))
}

/// The master the second actor takes over: duplicates of the pseudoconsole's handles, rebuilt.
#[cfg(windows)]
fn take_over(handle: &ActorHandle) -> TestResult<Master> {
    // SAFETY: the HPCON layout this test reads is the one the session server's Task 2 probe found on the Windows
    // versions CI runs (docs/spikes/20261018-session-server.md, Windows: ConPTY handoff).
    let handoff = unsafe { handle.duplicate_for_handoff() }?;
    Ok(Master::from_handoff(handoff, Size::cells(24, 80))?)
}
