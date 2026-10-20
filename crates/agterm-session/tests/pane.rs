//! The server's pane runtime on real PTYs, on every OS: a session started from its spec, typed into and read back
//! through attached clients, resized, killed with its tree, and ended with its program's status; its output fanned
//! out to every client alike, and a client that stops reading holding up neither the PTY nor the others.
//!
//! The programs are per OS: `/bin/sh` on Unix, `cmd.exe` and Windows PowerShell on Windows. Every session gets the
//! test's environment without the live agterm's `AGTERM_*` and `ZMX_*` variables, and a namespace directory of its
//! own (only named in its environment: no server runs).

#[expect(dead_code, reason = "the helpers serve the pane and the lead tests alike")]
mod support;

use std::time::{Duration, Instant};

use agterm_session::protocol::{ExitStatus, GridSize, SessionSpec};
use agterm_session::server::clients::{ClientEvent, RecvError};
use agterm_session::server::pane::{
    AttachError, DIRECTORY_VARIABLE, FALLBACK_TERM, Pane, Panes, SESSION_VARIABLE, SpawnError,
};
use support::{
    Client, PATIENCE, SIZE, TestResult, config, quit, script, shell, shown, size_line, spec, tagged, type_line,
};

#[test]
fn shown_text_leaves_out_control_strings() {
    let output = b"a\x1b]0;cmd.exe - echo got:pong\x07b\x1b]2;t\x1b\\c\x1bPq#0\x1b\\d\x1b[1me\x1b]0;open";
    assert_eq!(shown(output), "abcd\x1b[1me");
}

fn exit_code(status: &ExitStatus) -> Option<u32> {
    match status {
        ExitStatus::Exited { code, .. } => Some(*code),
        ExitStatus::Unknown => None,
    }
}

/// The screen of the pane's own terminal, as text.
fn screen(pane: &Pane) -> String {
    pane.with_terminal(|terminal| {
        let options = agterm_ghostty_vt::FormatOptions {
            format: agterm_ghostty_vt::Format::Plain,
            unwrap: true,
            trim: true,
            extras: agterm_ghostty_vt::FormatExtras::empty(),
        };
        let text = terminal
            .format(agterm_ghostty_vt::Region::Screen, options)
            .ok()
            .flatten()
            .unwrap_or_default();
        String::from_utf8_lossy(&text).into_owned()
    })
}

#[test]
fn input_round_trips_through_a_client_and_the_servers_terminal() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (pane, creator) = Pane::spawn(config("echo", dir.path(), shell()))?;
    let mut client = Client::new(creator);
    type_line(&pane, &tagged("ping"))?;
    client.expect("got:ping");
    type_line(&pane, &tagged("pong"))?;
    client.expect("got:pong");
    assert!(screen(&pane).contains("got:pong"), "{:?}", screen(&pane));
    assert_eq!(pane.clients(), 1);
    quit(&pane)?;
    assert_eq!(exit_code(&client.finish()), Some(0));
    assert_eq!(pane.clients(), 0);
    assert_eq!(
        client.next,
        pane.offset(),
        "the client got every byte the session wrote"
    );
    Ok(())
}

#[test]
fn a_resize_reaches_the_child_and_every_client_in_order() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (pane, creator) = Pane::spawn(config("resize", dir.path(), shell()))?;
    let mut client = Client::new(creator);
    let size = GridSize { cols: 100, rows: 40 };
    pane.resize(size);
    assert_eq!(pane.size(), size);
    // the same size again changes nothing
    pane.resize(size);
    type_line(&pane, &size_line())?;
    client.expect("SIZE=40x100");
    let resized: Vec<_> = client
        .events
        .iter()
        .filter(|event| matches!(event, ClientEvent::Resized { .. }))
        .collect();
    assert!(
        matches!(resized.as_slice(), [ClientEvent::Resized { size: got, .. }] if *got == size),
        "{resized:?}"
    );
    assert_eq!(
        pane.with_terminal(|terminal| (terminal.cols(), terminal.rows())),
        (100, 40)
    );
    quit(&pane)?;
    client.finish();
    Ok(())
}

#[test]
fn the_environment_and_the_working_directory_are_the_specs() -> TestResult {
    let dir = tempfile::tempdir()?;
    let cwd = tempfile::tempdir()?;
    // the real path, as `pwd -P` prints it (macOS' /var is a link); on Windows without the `\\?\` prefix
    // canonicalize adds, which cmd refuses as a working directory
    let shown = cwd
        .path()
        .canonicalize()?
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .to_owned();
    let mut spec = if cfg!(windows) {
        script(&format!(
            "echo [%AGTERM_SESSION_TEST%][%{SESSION_VARIABLE}%][%{DIRECTORY_VARIABLE}%][%TERM%][%CD%]"
        ))
    } else {
        script(&format!(
            "echo \"[$AGTERM_SESSION_TEST][${SESSION_VARIABLE}][${DIRECTORY_VARIABLE}][$TERM][$(pwd -P)]\""
        ))
    };
    spec.env.retain(|(key, _)| !key.eq_ignore_ascii_case("TERM"));
    spec.env.push(("AGTERM_SESSION_TEST".into(), "a value".into()));
    spec.env.push(("AGTERM_LIVE_MANAGED_CLAIM".into(), "1".into()));
    spec.cwd = Some(shown.clone());
    spec.tracked_env = vec!["AGTERM_SESSION_TEST".into(), "NOT_SET_ANYWHERE".into()];
    // wide enough for the line, which a console would otherwise wrap
    let mut config = config("env", dir.path(), spec);
    config.size = GridSize { cols: 400, rows: 30 };
    let (pane, creator) = Pane::spawn(config)?;
    assert_eq!(
        pane.tracked_env(),
        [("AGTERM_SESSION_TEST".to_owned(), "a value".to_owned())]
    );
    let mut client = Client::new(creator);
    let namespace = dir.path().to_string_lossy().into_owned();
    client.expect(&format!("[a value][env][{namespace}][{FALLBACK_TERM}][{shown}]"));
    assert_eq!(exit_code(&client.finish()), Some(0));
    Ok(())
}

#[test]
fn the_environment_holds_only_what_the_spec_gives() -> TestResult {
    let dir = tempfile::tempdir()?;
    // a variable this process has, left out of the spec
    let (mut spec, removed) = if cfg!(windows) {
        (script("echo [%USERNAME%]"), "USERNAME")
    } else {
        (script("echo \"[${HOME-unset}]\""), "HOME")
    };
    assert!(std::env::var_os(removed).is_some(), "the test needs {removed} set");
    spec.env.retain(|(key, _)| !key.eq_ignore_ascii_case(removed));
    let (_pane, creator) = Pane::spawn(config("clear", dir.path(), spec))?;
    let mut client = Client::new(creator);
    let unset = if cfg!(windows) { "%USERNAME%" } else { "unset" };
    client.expect(&format!("[{unset}]"));
    client.finish();
    Ok(())
}

#[test]
fn the_end_reaches_every_client_with_the_programs_status() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (pane, creator) = Pane::spawn(config("exit", dir.path(), shell()))?;
    let mut one = Client::new(creator);
    let mut two = Client::attach(&pane)?;
    type_line(&pane, &tagged("bye"))?;
    one.expect("got:bye");
    type_line(&pane, "exit 7")?;
    assert_eq!(exit_code(&one.finish()), Some(7));
    assert_eq!(exit_code(&two.finish()), Some(7));
    assert!(
        two.text().contains("got:bye"),
        "the exit came before the output: {:?}",
        two.text()
    );
    assert_eq!(exit_code(&pane.wait_exit(PATIENCE).ok_or("no exit")?), Some(7));
    assert_eq!(
        pane.try_attach(SIZE, None).err(),
        Some(AttachError::Ended),
        "an ended session takes no client"
    );
    // killing an ended session is done at once
    assert!(pane.kill(false)?);
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_signal_ending_the_program_is_reported() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (_pane, creator) = Pane::spawn(config("signal", dir.path(), script("sleep 1; kill -TERM $$")))?;
    let mut client = Client::new(creator);
    match client.finish() {
        ExitStatus::Exited { signal, .. } => assert!(signal.is_some(), "no signal"),
        ExitStatus::Unknown => panic!("the status is unknown"),
    }
    Ok(())
}

#[test]
fn two_clients_receive_the_same_bytes() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (pane, creator) = Pane::spawn(config("fanout", dir.path(), shell()))?;
    let mut one = Client::new(creator);
    let mut two = Client::attach(&pane)?;
    assert_eq!(pane.clients(), 2);
    for round in 0..20 {
        type_line(&pane, &tagged(&format!("line{round}")))?;
    }
    one.expect("got:line19");
    quit(&pane)?;
    one.finish();
    two.finish();
    assert_eq!(one.output, two.output);
    assert_eq!(one.next, two.next);
    assert!(!one.output.is_empty());
    Ok(())
}

#[test]
fn a_detached_client_leaves_the_others_reading() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (pane, leaving) = Pane::spawn(config("detach", dir.path(), shell()))?;
    let mut staying = Client::attach(&pane)?;
    assert!(pane.detach(leaving.subscription.id()));
    assert!(!pane.detach(leaving.subscription.id()));
    assert_eq!(pane.clients(), 1);
    type_line(&pane, &tagged("still"))?;
    staying.expect("got:still");
    // what was queued before the detach (its first, empty snapshot), then nothing
    assert!(matches!(
        leaving.subscription.try_recv(),
        Ok(ClientEvent::Snapshot { epoch: 1, .. })
    ));
    assert_eq!(
        leaving.subscription.recv_timeout(Duration::from_secs(1)),
        Err(RecvError::Closed)
    );
    quit(&pane)?;
    staying.finish();
    Ok(())
}

/// The chunks the flood comes in, each [`FLOOD_LINES`] lines of about 60 bytes: one fits in the test's client
/// queue, all of them do not.
const FLOOD_CHUNKS: usize = 16;
const FLOOD_LINES: usize = 150;

/// The line that makes [`shell`] print [`FLOOD_LINES`] numbered lines, then [`flood_end`] of `chunk` (which the
/// echoed line does not hold).
fn flood(chunk: usize) -> String {
    if cfg!(windows) {
        format!(
            "(for /l %i in (1,1,{FLOOD_LINES}) do @echo line-%i-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx) \
             & echo chunk-{chunk}^-end"
        )
    } else {
        format!(
            "i=0; while [ $i -lt {FLOOD_LINES} ]; do echo line-$i-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx; \
             i=$((i+1)); done; echo chunk-{chunk}\"\"-end"
        )
    }
}

fn flood_end(chunk: usize) -> String {
    format!("chunk-{chunk}-end")
}

#[test]
fn a_stalled_client_blocks_neither_the_pty_nor_the_others() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut config = config("stall", dir.path(), shell());
    config.client_queue_limit = 16 * 1024;
    // never read until the program ended
    let (pane, stalled) = Pane::spawn(config)?;
    let mut reading = Client::attach(&pane)?;
    // a chunk at a time, each read before the next is asked for, so the reading client never falls a queue behind
    // however slowly it is scheduled
    for chunk in 1..=FLOOD_CHUNKS {
        type_line(&pane, &flood(chunk))?;
        reading.expect(&flood_end(chunk));
    }
    quit(&pane)?;
    let status = reading.finish();
    assert_eq!(exit_code(&status), Some(0));
    assert_eq!(reading.next, pane.offset());
    assert!(screen(&pane).contains(&flood_end(FLOOD_CHUNKS)));
    // the stalled client's queue dropped what it held (none of it was sent: a snapshot replaces it), it learnt where
    // its copy stops, and got the session's last snapshot, then the end
    let mut events = Vec::new();
    while let Ok(event) = stalled.subscription.try_recv() {
        events.push(event);
    }
    let [
        ClientEvent::Overflowed { offset: 0 },
        ClientEvent::Snapshot { epoch: 2, offset, .. },
        ClientEvent::Exit(exit),
    ] = events.as_slice()
    else {
        panic!("not an overflow, the last snapshot and the end: {events:?}");
    };
    assert_eq!((*offset, exit), (pane.offset(), &status));
    Ok(())
}

#[test]
fn killing_ends_the_program_and_its_tree() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (pane, creator) = Pane::spawn(config("kill", dir.path(), tree()))?;
    let mut client = Client::new(creator);
    client.expect("started:");
    let descendant = descendant(&client.text())?;
    // whether the end came within the grace is a race with the runner's load: the end itself is what is asserted
    pane.kill(false)?;
    let status = client.finish();
    assert_ne!(exit_code(&status), Some(0), "{status:?}");
    assert_gone(descendant);
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_forced_kill_ends_a_program_that_ignores_the_hangup() -> TestResult {
    let dir = tempfile::tempdir()?;
    let spec = script("trap '' HUP; echo ready; while :; do sleep 1; done");
    let (pane, creator) = Pane::spawn(config("stubborn", dir.path(), spec))?;
    let mut client = Client::new(creator);
    client.expect("ready");
    assert!(!pane.kill(false)?, "the hangup ended a program that ignores it");
    assert!(pane.exit_status().is_none());
    pane.kill(true)?;
    client.finish();
    Ok(())
}

/// A shell the hangup ends, with a background child that ignores it: holding the PTY, or with it let go of
/// (`nohup`'s way). The child says `started:<pid>:` itself, once it ignores the hangup, so a hangup sent on that
/// word never finds it still taking the default action.
#[cfg(unix)]
fn stubborn_tree(holds_pty: bool) -> SessionSpec {
    let redirect = if holds_pty { "" } else { " </dev/null >/dev/null 2>&1" };
    script(&format!(
        "(trap '' HUP; exec sh -c 'echo \"started:$$:\"; exec sleep 300{redirect}') & wait"
    ))
}

#[cfg(unix)]
#[test]
fn a_forced_kill_ends_a_descendant_that_ignores_the_hangup() -> TestResult {
    for holds_pty in [true, false] {
        let dir = tempfile::tempdir()?;
        let (pane, creator) = Pane::spawn(config("stubborn-tree", dir.path(), stubborn_tree(holds_pty)))?;
        let mut client = Client::new(creator);
        client.expect("started:");
        let descendant = descendant(&client.text())?;
        pane.kill(true)?;
        client.finish();
        assert_gone(descendant);
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn dropping_a_pane_ends_a_descendant_that_ignores_the_hangup() -> TestResult {
    for holds_pty in [true, false] {
        let dir = tempfile::tempdir()?;
        let (pane, creator) = Pane::spawn(config("dropped-tree", dir.path(), stubborn_tree(holds_pty)))?;
        let mut client = Client::new(creator);
        client.expect("started:");
        let descendant = descendant(&client.text())?;
        drop(pane);
        client.finish();
        assert_gone(descendant);
    }
    Ok(())
}

/// A job-controlling shell (`set -m`) whose background job, in a process group of its own, ignores the hangup and
/// let go of the PTY: `nohup`'s way, out of the reach of the shell's group. The job says `started:<pid>:` itself, once
/// it ignores the hangup.
#[cfg(unix)]
fn job_controlling_shell() -> SessionSpec {
    script("set -m; (trap '' HUP; exec sh -c 'echo \"started:$$:\"; exec sleep 300 </dev/null >/dev/null 2>&1') & wait")
}

/// The descendant `pid` sits in a process group other than the program `program`'s.
#[cfg(unix)]
fn assert_own_group(program: u32, pid: u32) -> TestResult {
    let group_of = |pid: u32| -> TestResult<libc::pid_t> {
        // SAFETY: `getpgid` has no memory preconditions.
        Ok(unsafe { libc::getpgid(libc::pid_t::try_from(pid)?) })
    };
    let groups = (group_of(program)?, group_of(pid)?);
    assert!(groups.0 > 0 && groups.1 > 0, "{groups:?}");
    assert_ne!(groups.0, groups.1, "the job shares the shell's group");
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_forced_kill_ends_a_job_in_a_group_of_its_own() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (pane, creator) = Pane::spawn(config("job-control", dir.path(), job_controlling_shell()))?;
    let mut client = Client::new(creator);
    client.expect("started:");
    let job = descendant(&client.text())?;
    assert_own_group(pane.pid(), job)?;
    pane.kill(true)?;
    client.finish();
    assert_gone(job);
    Ok(())
}

#[cfg(unix)]
#[test]
fn dropping_a_pane_ends_a_job_in_a_group_of_its_own() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (pane, creator) = Pane::spawn(config("job-control-dropped", dir.path(), job_controlling_shell()))?;
    let mut client = Client::new(creator);
    client.expect("started:");
    let job = descendant(&client.text())?;
    assert_own_group(pane.pid(), job)?;
    drop(pane);
    client.finish();
    assert_gone(job);
    Ok(())
}

/// The recorder of [`a_forced_kill_hangs_up_a_job_in_a_group_of_its_own`], in Perl: no shell leaves its session, and
/// `setsid(1)` is util-linux's, which macOS lacks. It starts two jobs of the session, each a `sleep 300` without the
/// PTY in a process group of its own, the first taking the hangup's default action and the second ignoring it, both
/// from their fork on. It then leaves the session, so neither the hangup nor the kill reaches it, says
/// `started:<first>:<second>:`, lets go of the PTY, and once it reaped the first job writes the signal that ended it
/// (`signal:0` for none) to the file its argument names.
#[cfg(unix)]
const RECORDER: &str = r#"
use strict;
use warnings;
use POSIX qw(setpgid setsid);

sub job {
    my ($hangup) = @_;
    local $SIG{HUP} = $hangup;
    my $pid = fork() // die "fork: $!\n";
    if ($pid == 0) {
        setpgid(0, 0) or die "setpgid: $!\n";
        open STDIN, '<', '/dev/null' or die;
        open STDOUT, '>', '/dev/null' or die;
        open STDERR, '>&', \*STDOUT or die;
        exec 'sleep', '300' or die "exec: $!\n";
    }
    # settles the group before the jobs are named, whichever of the two calls comes first
    setpgid($pid, $pid);
    return $pid;
}

my ($record) = @ARGV;
my $ends = job('DEFAULT');
my $stays = job('IGNORE');
defined setsid() or die "setsid: $!\n";
$| = 1;
print "started:$ends:$stays:\n";
open STDIN, '<', '/dev/null' or die;
open STDOUT, '>', '/dev/null' or die;
open STDERR, '>&', \*STDOUT or die;
waitpid($ends, 0) == $ends or die;
my $signal = $? & 127;
open my $out, '>', "$record.part" or die;
print $out "signal:$signal\n";
close $out or die;
rename "$record.part", $record or die;
"#;

/// A forced kill hangs up a job in a group of its own, and holds the kill for the grace while a process of the session
/// lives on. The kernel's hangup on the shell's exit reaches only the foreground group, so only the session-wide
/// hangup reaches the jobs, and since they let go of the PTY (as one holding it does on macOS, which revokes the tty
/// once the shell exits) its end tells nothing of them: only the grace's wait on the session's processes holds the
/// kill. Neither is read off a race with the clock. The job that takes the hangup's default action ends on the
/// hangup itself, and [`RECORDER`], its parent out of the session's reach, writes down the signal that ended it,
/// whenever that was; the job that ignores the hangup lives on until the kill, which so comes no sooner than
/// [`KILL_GRACE`](agterm_session::server::pane::KILL_GRACE) after the hangup.
#[cfg(unix)]
#[test]
fn a_forced_kill_hangs_up_a_job_in_a_group_of_its_own() -> TestResult {
    let dir = tempfile::tempdir()?;
    let recorder = dir.path().join("recorder.pl");
    std::fs::write(&recorder, RECORDER)?;
    let record = dir.path().join("ended-by");
    let spec = script(&format!("perl '{}' '{}' & wait", recorder.display(), record.display()));
    let (pane, creator) = Pane::spawn(config("job-hangup", dir.path(), spec))?;
    let mut client = Client::new(creator);
    client.expect("started:");
    let (ends, stays) = two_jobs(&client.text())?;
    assert_own_group(pane.pid(), ends)?;
    assert_own_group(pane.pid(), stays)?;
    let start = Instant::now();
    pane.kill(true)?;
    let held = start.elapsed();
    client.finish();
    assert_gone(ends);
    assert_gone(stays);
    assert_eq!(
        recorded(&record),
        format!("signal:{}", libc::SIGHUP),
        "the job was killed without a hangup"
    );
    assert!(
        held >= agterm_session::server::pane::KILL_GRACE,
        "the kill came {held:?} after the hangup, while a job of the session lived on"
    );
    Ok(())
}

/// The two job ids of `started:<first>:<second>:`.
#[cfg(unix)]
fn two_jobs(text: &str) -> TestResult<(u32, u32)> {
    let mut ids = text
        .split("started:")
        .nth(1)
        .ok_or("no job ids in the output")?
        .split(':');
    let mut next = || -> TestResult<u32> { Ok(ids.next().ok_or("a job id is missing")?.trim().parse()?) };
    Ok((next()?, next()?))
}

/// What [`RECORDER`] wrote to `path`, once it did.
#[cfg(unix)]
fn recorded(path: &std::path::Path) -> String {
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            return text.trim().to_owned();
        }
        assert!(Instant::now() < deadline, "the recorder wrote nothing");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn dropping_a_pane_ends_its_program() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (pane, creator) = Pane::spawn(config("dropped", dir.path(), tree()))?;
    let mut client = Client::new(creator);
    client.expect("started:");
    let descendant = descendant(&client.text())?;
    drop(pane);
    client.finish();
    assert_gone(descendant);
    Ok(())
}

#[test]
fn the_registry_holds_a_session_until_it_ends() -> TestResult {
    let dir = tempfile::tempdir()?;
    let panes = Panes::new();
    let (pane, creator) = panes.spawn(config("held", dir.path(), shell()))?;
    assert!(matches!(
        panes.spawn(config("held", dir.path(), shell())),
        Err(SpawnError::Exists(name)) if name == "held"
    ));
    assert_eq!(panes.names(), ["held"]);
    assert_eq!(panes.get("held").map(|held| held.pid()), Some(pane.pid()));
    assert!(pane.created_at() > 0);
    let mut client = Client::new(creator);
    quit(&pane)?;
    client.finish();
    let deadline = Instant::now() + PATIENCE;
    while !panes.is_empty() {
        assert!(
            Instant::now() < deadline,
            "the ended session stayed: {:?}",
            panes.names()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(panes.get("held").is_none());
    // the name is free again
    let (again, _creator) = panes.spawn(config("held", dir.path(), shell()))?;
    assert_eq!(panes.len(), 1);
    assert!(panes.remove("held").is_some());
    drop(again);
    assert!(panes.is_empty());
    Ok(())
}

#[test]
fn an_unstartable_program_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let panes = Panes::new();
    let missing = spec(&["agterm-session-test-no-such-program"]);
    assert!(matches!(
        panes.spawn(config("missing", dir.path(), missing)),
        Err(SpawnError::Io(_))
    ));
    assert!(panes.is_empty());
}

/// A program with a descendant in its tree that prints `started:<descendant pid>:` and then waits for it.
fn tree() -> SessionSpec {
    if cfg!(windows) {
        spec(&[
            "powershell.exe",
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$p = Start-Process -FilePath ping.exe -ArgumentList '-n','300','127.0.0.1' -NoNewWindow -PassThru; \
             Write-Output ('started:' + $p.Id + ':'); Wait-Process -Id $p.Id",
        ])
    } else {
        script("sleep 300 & echo \"started:$!:\"; wait")
    }
}

fn descendant(text: &str) -> TestResult<u32> {
    let pid = text
        .split("started:")
        .nth(1)
        .and_then(|rest| rest.split(':').next())
        .ok_or("no descendant pid in the output")?;
    Ok(pid.trim().parse()?)
}

/// The descendant `pid` ends (or is a zombie its new parent has yet to reap).
fn assert_gone(pid: u32) {
    let deadline = Instant::now() + PATIENCE;
    while alive(pid) {
        assert!(Instant::now() < deadline, "the descendant {pid} survived");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(unix)]
fn alive(pid: u32) -> bool {
    let pid = libc::pid_t::try_from(pid).unwrap_or(libc::pid_t::MAX);
    // SAFETY: probes a process id with signal 0; no memory is passed.
    let exists = unsafe { libc::kill(pid, 0) } == 0;
    exists && !is_zombie(pid)
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

#[cfg(windows)]
fn alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };
    // SAFETY: opens a process by id with query and wait rights; a null handle means it is gone.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE, 0, pid) };
    if handle.is_null() {
        return false;
    }
    // SAFETY: waits zero milliseconds on the handle opened above, then closes it.
    let running = unsafe { WaitForSingleObject(handle, 0) } == WAIT_TIMEOUT;
    // SAFETY: the handle was opened above and is closed once.
    unsafe { CloseHandle(handle) };
    running
}
