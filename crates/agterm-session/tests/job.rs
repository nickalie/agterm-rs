//! The job and process tables on real processes, on every OS: a shell on a PTY runs a job, which the live sweeper
//! finds as the shell's foreground job and the process table as the terminal's foreground, and which ends when the
//! sweeper kills it; and a process's argv read back with its spaces, quotes and backslashes.
//!
//! The shells are per OS: an interactive `/bin/sh` (job control on its PTY) on Unix, `cmd.exe` on Windows, whose job
//! is the shell's descendants. Every child gets the test's environment without the live agterm's `AGTERM_*` and
//! `ZMX_*` variables.

#[expect(
    dead_code,
    reason = "the helpers serve the pane, the lead and the PowerShell tests too"
)]
mod support;

use std::io::Read as _;
use std::process::Stdio;

use agterm_host::ProcessTable as _;
use agterm_pty::{Actor, ActorConfig, ActorHandle, Child, Command, Size, Spawned};
use agterm_session::job::{Signal, Sweeper};
use agterm_session::process_table::ProcTable;
use support::wait_for;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// What the argv helper is told to wait by.
const HELPER: &str = "AGTERM_SESSION_TEST_HELPER";

/// Arguments the argv test passes and reads back: spaces, quotes of both kinds, backslashes before a quote and at
/// the end, a tab, an empty one.
const ODD_ARGS: [&str; 6] = [
    "a b",
    "it's \"quoted\"",
    r#"back\"slash"#,
    r"C:\dir with space\",
    "tab\there",
    "",
];

fn without_live_variables(mut command: Command) -> Command {
    for (key, _) in std::env::vars_os() {
        if key
            .to_str()
            .is_some_and(|key| key.starts_with("AGTERM_") || key.starts_with("ZMX_"))
        {
            command = command.env_remove(key);
        }
    }
    command
}

/// A shell on a PTY, its tree killed when the test ends, however it ends.
struct Shell {
    handle: ActorHandle,
    child: Child,
}

impl Drop for Shell {
    fn drop(&mut self) {
        let _ = self.child.killer().kill_tree();
        let _ = self.child.wait();
        self.handle.child_exited();
    }
}

impl Shell {
    fn start() -> TestResult<Self> {
        let command = if cfg!(windows) {
            Command::new("cmd.exe").args(["/d", "/q", "/k"])
        } else {
            Command::new("/bin/sh").arg("-i").env("PS1", "$ ").env_remove("ENV")
        };
        let Spawned { master, child } = agterm_pty::spawn(&without_live_variables(command), Size::cells(24, 80))?;
        let config = ActorConfig::new("job-test", master, answer_cursor_queries());
        Ok(Self {
            handle: Actor::spawn(config)?,
            child,
        })
    }

    fn pid(&self) -> i32 {
        i32::try_from(self.child.pid()).unwrap_or_default()
    }

    fn type_line(&self, line: &str) -> TestResult {
        let enter = if cfg!(windows) { "\r" } else { "\n" };
        self.handle.write_input(format!("{line}{enter}").as_bytes())?;
        Ok(())
    }
}

/// The read callback: the output is dropped, but every cursor query `ConPTY` asks before the child runs (and the
/// shells' line editors after) gets its answer.
fn answer_cursor_queries() -> agterm_pty::ReadCallback {
    const QUERY: &[u8] = b"\x1b[6n";
    // what may hold the start of a query split across reads
    let mut tail = Vec::new();
    Box::new(move |bytes| {
        tail.extend_from_slice(bytes);
        let queries = tail.windows(QUERY.len()).filter(|window| *window == QUERY).count();
        let keep = tail.len().saturating_sub(QUERY.len() - 1);
        tail.drain(..keep);
        (0..queries).map(|_| b"\x1b[1;1R".to_vec()).collect()
    })
}

/// The job each shell runs, and the argv its first process has.
fn job_line() -> (&'static str, Vec<String>) {
    if cfg!(windows) {
        (
            "ping -n 600 127.0.0.1 >NUL",
            ["ping", "-n", "600", "127.0.0.1"].map(str::to_owned).to_vec(),
        )
    } else {
        ("sleep 600", ["sleep", "600"].map(str::to_owned).to_vec())
    }
}

#[test]
fn a_shells_foreground_job_is_found_and_ended() -> TestResult {
    let shell = Shell::start()?;
    let sweeper = Sweeper::live();
    let table = ProcTable::new();
    // the shell at its prompt holds its own foreground
    wait_for(
        "the idle shell's job",
        || sweeper.foreground_job(shell.pid()),
        |job| job.as_ref().is_some_and(Vec::is_empty),
    );
    let (line, argv) = job_line();
    shell.type_line(line)?;
    let job = wait_for(
        "the job",
        || sweeper.foreground_job(shell.pid()).unwrap_or_default(),
        |job| job.iter().any(|record| table.argv(record.pid).as_ref() == Some(&argv)),
    );
    let foreground = table.terminal_foreground(shell.pid());
    let program = job
        .iter()
        .find(|record| table.argv(record.pid).as_ref() == Some(&argv))
        .map(|record| record.group);
    assert_eq!(foreground, program, "{job:?}");
    assert!(sweeper.is_running(&job));

    sweeper.send(Signal::Kill, &job);
    wait_for("the job's end", || sweeper.is_running(&job), |running| !running);
    wait_for(
        "the shell's foreground back",
        || table.terminal_foreground(shell.pid()),
        |group| *group == Some(shell.pid()),
    );
    assert!(sweeper.foreground_job(shell.pid()).is_some_and(|job| job.is_empty()));
    Ok(())
}

#[test]
fn an_argv_with_spaces_and_quotes_reads_back_as_given() -> TestResult {
    // the helper below, run by name in a child of this test's own binary, with the odd arguments after `--`, where
    // the harness takes them for filters that match no test
    let mut child = std::process::Command::new(std::env::current_exe()?)
        .args(["--ignored", "--exact", "argv_helper_waits_for_stdin", "--"])
        .args(ODD_ARGS)
        .env(HELPER, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let pid = i32::try_from(child.id())?;
    let table = ProcTable::new();
    let argv = wait_for(
        "the helper's argv",
        || table.argv(pid),
        |argv| {
            argv.as_ref()
                .is_some_and(|argv| argv.ends_with(&ODD_ARGS.map(str::to_owned)))
        },
    );
    drop(child.stdin.take());
    assert!(child.wait()?.success());
    // the handle held no more, so not even Windows keeps the process object
    drop(child);
    assert_eq!(argv.map(|argv| argv.len()), Some(ODD_ARGS.len() + 5));
    assert_eq!(table.argv(pid), None, "a process gone has no argv");
    Ok(())
}

/// Not a test of its own: the child [`an_argv_with_spaces_and_quotes_reads_back_as_given`] reads, which waits until
/// its standard input closes.
#[test]
#[ignore = "the argv test's child"]
fn argv_helper_waits_for_stdin() {
    if std::env::var_os(HELPER).is_some() {
        let _ = std::io::stdin().read_to_end(&mut Vec::new());
    }
}
