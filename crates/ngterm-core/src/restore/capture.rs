//! A pane's foreground program, read two ways (upstream's `Ghostty/ForegroundProcess.swift`), and the restore capture
//! that fills the sessions' slots at quit, at the last window's close and on demand (`AppDelegate.
//! captureForegroundCommands`).
//!
//! The terminal reports its foreground process GROUP; the process table answers the rest:
//!
//! - [`ForegroundReader::running`], for the `tree`: the group leader's argv, classified as a program or a recognized
//!   shell; when the leader's argv cannot be read (it exited, `cat f | less` keeps the reaped `cat`'s group), the
//!   group's surviving members are tried as upstream's descent does. A `--command` pane's program is run by
//!   libghostty as `/bin/sh -c <command>` on Linux, where macOS's libghostty has bash `exec -l <command>`; a `/bin/sh`
//!   that does not exec its last command (dash, Debian's and Ubuntu's) stays as the group's leader over the program,
//!   so that wrapper is passed over for its child, the process upstream's `exec` leaves.
//! - [`ForegroundReader::command`], for the restore capture: the leader's program only, and never the pane's own
//!   creation command. Upstream gets that from macOS, where a `--command` pane's program runs under setuid `login`,
//!   whose argv is unreadable, so a leader-only read finds nothing there. On Linux the foreground group of a pane with
//!   no job-control shell IS the process libghostty spawned, the terminal's session leader, readable. So the capture
//!   refuses a group led by the session leader: a `--command` program (exec'd by `sh -c` or not), a shell wrapper
//!   (`--command "sh -c 'a; b'"`: no job control, so every child stays in the leader's group), a shell sitting at its
//!   prompt, or one that `exec`ed a program. A job an interactive shell starts gets its own group, led by the job,
//!   whatever started that shell, so a program typed into a shell pane (also inside `--command /bin/sh`) and the
//!   first element of a typed pipeline are captured. A captured argv then preempts the creation command on the next
//!   launch, while a refused one leaves the creation command to the exec path, with `--wait` and close-on-exit.
//!
//! PowerShell (a Rust-only extension, the shell of a live PowerShell pane) has no job control on Linux and macOS:
//! every native program it starts, in the foreground or in the background (`Start-Process -NoNewWindow`, a thread
//! job's, a `Start-Job` worker, a prompt helper such as `git status`), stays in the shell's own group, led by the
//! shell, and nothing in the process table tells its foreground program from the others. So native children sharing
//! an interactive PowerShell leader's group are not inferred: the group reads as that shell, the capture finds
//! nothing, the `tree` shows the shell and a restart replays nothing, so a Unix PowerShell pane restores to its prompt
//! rather than replaying a program that was not in its foreground. A PowerShell running a script or a command
//! (`-File`, `-Command` without `-NoExit`) is a program like any other, and a descendant that leads a group of its
//! own reads as any job does. On Windows the console's foreground is read instead (`ngterm-session`'s process table).

use ngterm_host::{ProcessTable, Surface};

use crate::command_restore::{PaneForeground, ProcessGroupMember, group_descent_candidates, pane_foreground};
use crate::model::Session;

/// A live pane's foreground group by its pane token.
pub type LiveGroup<'a> = &'a dyn Fn(&str) -> Option<i32>;

/// Reads foreground processes through the frontend's process table.
pub struct ForegroundReader<'a> {
    table: &'a dyn ProcessTable,
    /// The user's `$SHELL` basename, widening shell recognition to a non-standard login shell.
    shell: Option<&'a str>,
    /// A live pane's foreground group by its pane token: its daemon's shell holds the terminal, not the pane's own
    /// surface, whose foreground is the attach client.
    live: Option<LiveGroup<'a>>,
}

impl<'a> ForegroundReader<'a> {
    #[must_use]
    pub fn new(table: &'a dyn ProcessTable, shell: Option<&'a str>) -> Self {
        Self {
            table,
            shell,
            live: None,
        }
    }

    /// This reader with `live` reading a live pane's foreground group from its pane token (upstream's
    /// `ZmxForegroundResolver`); without it a live pane reads nothing.
    #[must_use]
    pub fn with_live(mut self, live: LiveGroup<'a>) -> Self {
        self.live = Some(live);
        self
    }

    /// The foreground group of `surface`'s terminal: a live pane's through its daemon, any other's as libghostty
    /// reports it; `None` while it is not realized or cannot be read.
    #[must_use]
    pub fn group_of(&self, surface: &dyn Surface) -> Option<i32> {
        if surface.is_live_session() {
            return self.live.and_then(|live| live(surface.pane_token()));
        }
        foreground_group(surface)
    }

    /// The process that holds group `pgid`'s foreground and what it is (upstream's `ForegroundProcess.observed`):
    /// the leader, else the first readable member of its descent. A PowerShell leader holding its prompt is that
    /// shell, whatever runs in its group (module docs).
    #[must_use]
    pub fn observed(&self, pgid: i32) -> Option<(i32, PaneForeground)> {
        if let Some(argv) = self.table.argv(pgid) {
            return pane_foreground(&argv, self.shell).map(|foreground| (pgid, foreground));
        }
        self.descent(pgid)
    }

    /// The `tree`'s read of foreground group `pgid` (upstream's `ForegroundProcess.running`): the program or the shell
    /// holding the foreground, past libghostty's `/bin/sh -c` wrapper of a `--command` pane (module docs); `None` when
    /// nothing in the group can be read.
    #[must_use]
    pub fn running(&self, pgid: i32) -> Option<PaneForeground> {
        self.past_command_wrapper(pgid)
            .or_else(|| self.observed(pgid))
            .map(|(_, foreground)| foreground)
    }

    /// The first readable member of group `pgid`'s descent (upstream's `groupDescentCandidates`).
    fn descent(&self, pgid: i32) -> Option<(i32, PaneForeground)> {
        let members: Vec<ProcessGroupMember> = self
            .table
            .group(pgid)
            .into_iter()
            .map(|stat| ProcessGroupMember {
                pid: stat.pid,
                ppid: stat.ppid,
            })
            .collect();
        let (member, argv) = group_descent_candidates(pgid, &members)
            .into_iter()
            .find_map(|candidate| self.table.argv(candidate).map(|argv| (candidate, argv)))?;
        pane_foreground(&argv, self.shell).map(|foreground| (member, foreground))
    }

    /// The child running the command when group `pgid` is led by the terminal's own process and that is libghostty's
    /// `/bin/sh -c` wrapper, which a `/bin/sh` that execs its last command never leaves; `None` otherwise, and while
    /// the wrapper runs no child (a builtin).
    fn past_command_wrapper(&self, pgid: i32) -> Option<(i32, PaneForeground)> {
        let leader = self.table.stat(pgid)?;
        if leader.session != leader.pid || !is_command_wrapper(&self.table.argv(pgid)?) {
            return None;
        }
        self.descent(pgid)
    }

    /// The restore capture's read of foreground group `pgid` (upstream's `ForegroundProcess.command`): the leader's
    /// program, or `None` when the group is led by the terminal's session leader (module docs), a shell holds the
    /// foreground, or the leader cannot be read. A PowerShell leader holding its prompt is a shell, so no program in
    /// its group is captured (module docs).
    #[must_use]
    pub fn command(&self, pgid: i32) -> Option<Vec<String>> {
        let leader = self.table.stat(pgid)?;
        if leader.session == leader.pid {
            return None;
        }
        match pane_foreground(&self.table.argv(pgid)?, self.shell)? {
            PaneForeground::Program(argv) => Some(argv),
            PaneForeground::ForegroundShell(_) => None,
        }
    }

    /// [`ForegroundReader::running`] for a terminal: its foreground group as libghostty reports it.
    #[must_use]
    pub fn running_in(&self, surface: &dyn Surface) -> Option<PaneForeground> {
        self.running(self.group_of(surface)?)
    }

    /// [`ForegroundReader::command`] for a terminal.
    #[must_use]
    pub fn command_in(&self, surface: &dyn Surface) -> Option<Vec<String>> {
        self.command(self.group_of(surface)?)
    }
}

/// Whether `argv` is the shell libghostty runs a command through on Linux: `/bin/sh -c <command>` (`-l` first inside
/// Flatpak).
fn is_command_wrapper(argv: &[String]) -> bool {
    match argv {
        [shell, flag, _] => shell == "/bin/sh" && flag == "-c",
        [shell, login, flag, _] => shell == "/bin/sh" && login == "-l" && flag == "-c",
        _ => false,
    }
}

/// The terminal's foreground process group, `None` while it is not realized.
fn foreground_group(surface: &dyn Surface) -> Option<i32> {
    surface.foreground_pid().and_then(|pid| i32::try_from(pid).ok())
}

/// How a capture treats the slots of panes it cannot read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureKind {
    /// The quit and last-window-close captures: a slot no surface consumed keeps its armed argv, which would otherwise
    /// be destroyed by writing `None` over it (a fallback launch, a restored hidden split never shown).
    Exit,
    /// `restore.capture`: an unconsumed slot stays out of the persisted field, or a later show would replay it and a
    /// crash before the next capture replay it again.
    OnDemand,
}

/// Captures the foreground program of every pane of `sessions` into its persisted slot for the next save (upstream's
/// `captureForegroundCommands`): the primary, and the split while it is shown. Remote sessions, which are never
/// saved, are skipped. `time_remaining` bounds an exit capture; once it answers false, the rest read nothing. How many
/// slots a read actually filled, which is what `restore.capture` reports.
pub fn capture_foreground_commands<'s>(
    sessions: impl IntoIterator<Item = &'s mut Session>,
    reader: &ForegroundReader<'_>,
    kind: CaptureKind,
    time_remaining: &dyn Fn() -> bool,
) -> usize {
    let mut captured = 0;
    for session in sessions.into_iter().filter(|session| session.is_persistable()) {
        let (pending, pending_split) = match kind {
            CaptureKind::Exit => (
                session.restore.pending_foreground.clone(),
                session.split_restore.pending_foreground.clone(),
            ),
            CaptureKind::OnDemand => (None, None),
        };
        let read = session
            .surface
            .get()
            .and_then(|surface| time_remaining().then(|| reader.command_in(surface)).flatten());
        captured += usize::from(read.is_some());
        session.restore.foreground = read.or(pending);
        let shown_split = session
            .split_surface
            .get()
            .filter(|split| session.split.shown || split.is_live_session());
        let read = shown_split.and_then(|split| time_remaining().then(|| reader.command_in(split)).flatten());
        captured += usize::from(read.is_some());
        session.split_restore.foreground = read.or(pending_split);
    }
    captured
}

#[cfg(test)]
mod tests {
    use ngterm_host::ProcessStat;
    use ngterm_host::fakes::{FakeProcessTable, FakeSurface};

    use super::*;
    use crate::model::SessionSeed;

    fn stat(pid: i32, parent: i32, pgrp: i32, session: i32) -> ProcessStat {
        ProcessStat {
            pid,
            ppid: parent,
            pgrp,
            session,
        }
    }

    /// A pane's process tree: the shell libghostty spawned (pid 100, its session's leader), optionally running a job.
    fn shell_pane(table: &FakeProcessTable) {
        table.insert(stat(100, 1, 100, 100), Some(&["sh"]));
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_owned()).collect()
    }

    #[test]
    fn a_job_typed_into_a_shell_is_captured_and_the_shell_itself_is_not() {
        let table = FakeProcessTable::default();
        shell_pane(&table);
        let reader = ForegroundReader::new(&table, Some("sh"));
        assert_eq!(reader.command(100), None, "a shell at its prompt");
        assert_eq!(
            reader.running(100),
            Some(PaneForeground::ForegroundShell("sh".to_owned()))
        );
        table.insert(stat(200, 100, 200, 100), Some(&["sleep", "600"]));
        assert_eq!(reader.command(200), Some(argv(&["sleep", "600"])));
        assert_eq!(
            reader.running(200),
            Some(PaneForeground::Program(argv(&["sleep", "600"])))
        );
    }

    #[test]
    fn a_command_panes_own_program_is_reported_but_never_captured() {
        let table = FakeProcessTable::default();
        // `--command cat`: `sh -c cat` exec'd cat, the session leader itself
        table.insert(stat(100, 1, 100, 100), Some(&["cat"]));
        let reader = ForegroundReader::new(&table, None);
        assert_eq!(reader.running(100), Some(PaneForeground::Program(argv(&["cat"]))));
        assert_eq!(reader.command(100), None);
        // a shell wrapper with no job control: its children share the leader's group
        table.insert(stat(100, 1, 100, 100), Some(&["/bin/sh", "-c", "cat; echo done"]));
        table.insert(stat(101, 100, 100, 100), Some(&["cat"]));
        assert_eq!(reader.command(100), None);
        // `exec sleep 600` typed into a shell pane: the leader became the program
        table.remove(101);
        table.insert(stat(100, 1, 100, 100), Some(&["sleep", "600"]));
        assert_eq!(reader.command(100), None);
    }

    #[test]
    fn the_tree_reads_past_a_dash_wrapper_to_the_command_it_runs() {
        let table = FakeProcessTable::default();
        // `--command cat` under a /bin/sh that does not exec its last command: `/bin/sh -c cat` leads, cat is its child
        table.insert(stat(100, 1, 100, 100), Some(&["/bin/sh", "-c", "cat"]));
        let reader = ForegroundReader::new(&table, Some("zsh"));
        assert_eq!(
            reader.running(100),
            Some(PaneForeground::Program(argv(&["/bin/sh", "-c", "cat"]))),
            "a wrapper running no child is what holds the foreground"
        );
        table.insert(stat(101, 100, 100, 100), Some(&["cat"]));
        assert_eq!(reader.running(100), Some(PaneForeground::Program(argv(&["cat"]))));
        assert_eq!(
            reader.observed(100).map(|(pid, _)| pid),
            Some(100),
            "only the tree's read passes it"
        );
        assert_eq!(
            reader.command(100),
            None,
            "still the pane's own program, never captured"
        );
        // Flatpak's libghostty adds `-l`
        table.insert(stat(100, 1, 100, 100), Some(&["/bin/sh", "-l", "-c", "cat"]));
        assert_eq!(reader.running(100), Some(PaneForeground::Program(argv(&["cat"]))));
        // a `sh -c` the user runs is reported as it is: a job of a shell pane, or not libghostty's spelling
        table.insert(stat(100, 1, 100, 100), Some(&["sh", "-c", "cat"]));
        assert_eq!(
            reader.running(100),
            Some(PaneForeground::Program(argv(&["sh", "-c", "cat"])))
        );
        table.insert(stat(100, 1, 100, 100), Some(&["zsh"]));
        table.insert(stat(200, 100, 200, 100), Some(&["/bin/sh", "-c", "cat"]));
        table.insert(stat(201, 200, 200, 100), Some(&["cat"]));
        assert_eq!(
            reader.running(200),
            Some(PaneForeground::Program(argv(&["/bin/sh", "-c", "cat"])))
        );
    }

    #[test]
    fn a_job_inside_a_command_shell_and_a_pipelines_first_element_are_captured() {
        let table = FakeProcessTable::default();
        // `--command /bin/sh`, then `tail -f x | grep y` typed into it: the job's group is led by tail
        table.insert(stat(100, 1, 100, 100), Some(&["/bin/sh"]));
        table.insert(stat(300, 100, 300, 100), Some(&["tail", "-f", "x"]));
        table.insert(stat(301, 100, 300, 100), Some(&["grep", "y"]));
        let reader = ForegroundReader::new(&table, None);
        assert_eq!(reader.command(300), Some(argv(&["tail", "-f", "x"])));
    }

    #[test]
    fn a_reaped_leader_reads_through_its_surviving_group_for_the_tree_only() {
        let table = FakeProcessTable::default();
        shell_pane(&table);
        // `cat f | less`: cat was reaped, less keeps its group
        table.insert(stat(401, 100, 400, 100), Some(&["less"]));
        let reader = ForegroundReader::new(&table, None);
        assert_eq!(reader.running(400), Some(PaneForeground::Program(argv(&["less"]))));
        assert_eq!(reader.command(400), None);
    }

    /// What a PowerShell's group holds while it sits at its prompt or runs a native program: on Linux and macOS it has
    /// no job control, so every child it starts shares its group, foreground or not.
    fn powershell_children() -> [(i32, &'static [&'static str]); 5] {
        [
            // `Start-Process -NoNewWindow server`, still running in the background
            (101, &["server", "--port", "8080"]),
            // a `Start-Job` worker, as PowerShellProcessInstance starts it (StartJob defaults its working directory)
            (
                102,
                &["/usr/bin/pwsh", "-s", "-NoLogo", "-NoProfile", "-wd", "/home/me"],
            ),
            // a prompt helper (posh-git's `git status`) while the prompt renders
            (103, &["git", "status", "--porcelain"]),
            // `vim notes | cat`, typed at the prompt and running in the foreground
            (104, &["vim", "notes"]),
            (105, &["cat"]),
        ]
    }

    #[test]
    fn a_powershell_without_job_control_reads_as_its_shell_whatever_runs_in_its_group() {
        let table = FakeProcessTable::default();
        // a live pane's PowerShell, its session's leader
        let live: &[&str] = &[
            "-pwsh",
            "-Login",
            "-NoLogo",
            "-NoExit",
            "-Command",
            crate::live::shell_integration::PWSH_LOAD,
        ];
        table.insert(stat(100, 1, 100, 100), Some(live));
        // a PowerShell that is a job of another shell, in a group of its own
        table.insert(stat(200, 1, 200, 200), Some(&["zsh"]));
        table.insert(stat(300, 200, 300, 200), Some(&["pwsh", "-NoLogo"]));
        let reader = ForegroundReader::new(&table, None);
        let shell = Some(PaneForeground::ForegroundShell("pwsh".to_owned()));
        let at_prompt = |pgid: i32| {
            assert_eq!(reader.command(pgid), None, "nothing captured under {pgid}");
            assert_eq!(reader.running(pgid), shell, "the shell in the tree under {pgid}");
            assert_eq!(
                reader.observed(pgid),
                shell.clone().map(|shell| (pgid, shell)),
                "the shell for a restart under {pgid}"
            );
        };
        at_prompt(100);
        at_prompt(300);
        // nothing tells the foreground program from the background ones, so none of them is inferred
        for (pid, child) in powershell_children() {
            table.insert(stat(pid, 100, 100, 100), Some(child));
            table.insert(stat(pid + 500, 300, 300, 200), Some(child));
            at_prompt(100);
            at_prompt(300);
        }
        // a PowerShell running a script or a command (no -NoExit) is the program itself
        table.insert(stat(400, 200, 400, 200), Some(&["pwsh", "-File", "build.ps1"]));
        table.insert(stat(401, 400, 400, 200), Some(&["cargo", "build"]));
        assert_eq!(reader.command(400), Some(argv(&["pwsh", "-File", "build.ps1"])));
        // a program leading a group of its own (a job of a job-control shell it started) is a job like any other
        table.insert(stat(500, 100, 500, 100), Some(&["htop"]));
        assert_eq!(reader.command(500), Some(argv(&["htop"])));
    }

    fn session_with(primary: Option<u32>, split: Option<u32>, split_shown: bool) -> Session {
        let mut session = Session::new(SessionSeed {
            initial_cwd: "/tmp".to_owned(),
            ..SessionSeed::default()
        });
        let main = FakeSurface::new("main");
        main.probe().set_realized(true);
        main.probe().set_foreground_pid(primary);
        let _ = session.surface.replace(Box::new(main));
        let right = FakeSurface::new("right");
        right.probe().set_realized(true);
        right.probe().set_foreground_pid(split);
        let _ = session.split_surface.replace(Box::new(right));
        session.split.exists = true;
        session.split.shown = split_shown;
        session
    }

    #[test]
    fn the_capture_counts_what_it_read_and_an_exit_keeps_unconsumed_slots() {
        let table = FakeProcessTable::default();
        shell_pane(&table);
        table.insert(stat(200, 100, 200, 100), Some(&["sleep", "600"]));
        let reader = ForegroundReader::new(&table, None);
        let mut sessions = [session_with(Some(200), Some(100), true)];
        sessions[0].split_restore.pending_foreground = Some(argv(&["htop"]));
        let count = capture_foreground_commands(sessions.iter_mut(), &reader, CaptureKind::Exit, &|| true);
        assert_eq!(count, 1);
        assert_eq!(sessions[0].restore.foreground, Some(argv(&["sleep", "600"])));
        assert_eq!(sessions[0].split_restore.foreground, Some(argv(&["htop"])));
        let count = capture_foreground_commands(sessions.iter_mut(), &reader, CaptureKind::OnDemand, &|| true);
        assert_eq!(count, 1);
        assert_eq!(sessions[0].split_restore.foreground, None);
    }

    #[test]
    fn a_hidden_split_and_an_expired_budget_read_nothing() {
        let table = FakeProcessTable::default();
        shell_pane(&table);
        table.insert(stat(200, 100, 200, 100), Some(&["sleep", "600"]));
        let reader = ForegroundReader::new(&table, None);
        let mut sessions = [session_with(Some(100), Some(200), false)];
        assert_eq!(
            capture_foreground_commands(sessions.iter_mut(), &reader, CaptureKind::OnDemand, &|| true),
            0
        );
        let mut sessions = [session_with(Some(200), None, false)];
        assert_eq!(
            capture_foreground_commands(sessions.iter_mut(), &reader, CaptureKind::OnDemand, &|| false),
            0
        );
        assert_eq!(sessions[0].restore.foreground, None);
    }
}
