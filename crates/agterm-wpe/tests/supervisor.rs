//! The overlay's supervisor (`agterm_wpe::launcher::supervise`) with the real `bwrap` and the vendored package's
//! overlay: a signal sent to the supervisor, by pid or to its process group, reaches the program inside the overlay,
//! which runs its own handler, and the program's exit status comes back as the supervisor's.
//!
//! The supervisor installs process-wide signal handlers, so each case runs it in a child process of this test binary
//! (the `supervised` test, which does nothing unless [`CHILD`] is set), in a process group of its own so a group signal
//! reaches nothing else. Skipped where the WPE `WebKit` is the system's: there is no overlay to enter.

use std::io::{BufRead as _, BufReader};
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use agterm_wpe::launcher::{self, Provider};

const CHILD: &str = "AGTERM_SUPERVISOR_TEST_CHILD";
/// The program the supervisor runs: it reports which signal reached it through its exit status.
const PROGRAM: &str = "trap 'exit 7' TERM; trap 'exit 5' INT; echo ready; while :; do sleep 0.05; done";

/// The child's body: supervise the shell program in the overlay and exit with its status.
#[test]
fn supervised() {
    let Ok(libdir) = std::env::var(CHILD) else {
        return;
    };
    let args = ["-c".into(), PROGRAM.into()];
    let status = launcher::supervise(&PathBuf::from(libdir), &PathBuf::from("/bin/sh"), &args).unwrap();
    std::process::exit(status);
}

/// How a relay case ended.
#[derive(Debug, PartialEq, Eq)]
enum Relayed {
    /// The WPE `WebKit` is the system's: no overlay to supervise.
    Skipped,
    /// The supervisor exited with this code (`None`: by a signal).
    Exited(Option<i32>),
}

/// Runs the supervisor in a child, waits until the program inside says it is ready, sends `signal` to the supervisor
/// (or, with `group`, to its process group), and returns how the supervisor ended.
fn relayed(signal: i32, group: bool) -> Result<Relayed, String> {
    let Provider::Vendored { libdir } = launcher::provider() else {
        println!("skipped: the WPE WebKit is the system's, so there is no overlay to supervise");
        return Ok(Relayed::Skipped);
    };
    let exe = std::env::current_exe().map_err(|err| err.to_string())?;
    let mut child = Command::new(exe)
        .args(["--exact", "supervised", "--nocapture", "--test-threads=1"])
        .env(CHILD, &libdir)
        .stdout(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|err| format!("the supervisor did not start: {err}"))?;
    let pid = i32::try_from(child.id()).map_err(|err| err.to_string())?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    // libtest's own `test supervised ... ` has no line end before the program's word
    let ready = BufReader::new(stdout)
        .lines()
        .map_while(Result::ok)
        .any(|line| line.ends_with("ready"));
    if !ready {
        let _ = child.kill();
        return Err("the supervised program never said it was ready".to_owned());
    }
    // SAFETY: `kill` only sends a signal, to the child or the process group it leads.
    let sent = unsafe { libc::kill(if group { -pid } else { pid }, signal) };
    if sent != 0 {
        return Err(format!("the signal was not sent: {}", std::io::Error::last_os_error()));
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = child.try_wait().map_err(|err| err.to_string())? {
            return Ok(Relayed::Exited(status.code()));
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            return Err("the supervisor did not exit".to_owned());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn sigterm_to_the_supervisor_runs_the_programs_handler() {
    let relayed = relayed(libc::SIGTERM, false).unwrap();
    if relayed != Relayed::Skipped {
        assert_eq!(
            relayed,
            Relayed::Exited(Some(7)),
            "the program's TERM trap ran and its status came back"
        );
    }
}

#[test]
fn a_group_sigint_reaches_the_program_through_the_supervisor_alone() {
    let relayed = relayed(libc::SIGINT, true).unwrap();
    if relayed != Relayed::Skipped {
        assert_eq!(
            relayed,
            Relayed::Exited(Some(5)),
            "bwrap in a group of its own: the program's INT trap ran instead of bwrap dying"
        );
    }
}
