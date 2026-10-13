//! A pane's foreground program, read two ways (upstream's `Ghostty/ForegroundProcess.swift`), and the restore capture
//! that fills the sessions' slots at quit, at the last window's close and on demand (`AppDelegate.
//! captureForegroundCommands`).
//!
//! The terminal reports its foreground process GROUP; the process table answers the rest:
//!
//! - [`ForegroundReader::running`], for the `tree`: the group leader's argv, classified as a program or a recognized
//!   shell; when the leader's argv cannot be read (it exited, `cat f | less` keeps the reaped `cat`'s group), the
//!   group's surviving members are tried as upstream's descent does.
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

use agterm_host::{ProcessTable, Surface};

use crate::command_restore::{PaneForeground, ProcessGroupMember, group_descent_candidates, pane_foreground};
use crate::model::Session;

/// Reads foreground processes through the frontend's process table.
pub struct ForegroundReader<'a> {
    table: &'a dyn ProcessTable,
    /// The user's `$SHELL` basename, widening shell recognition to a non-standard login shell.
    shell: Option<&'a str>,
}

impl<'a> ForegroundReader<'a> {
    #[must_use]
    pub fn new(table: &'a dyn ProcessTable, shell: Option<&'a str>) -> Self {
        Self { table, shell }
    }

    /// The `tree`'s read of foreground group `pgid` (upstream's `ForegroundProcess.running`): the program or the shell
    /// holding the foreground; `None` when nothing in the group can be read.
    #[must_use]
    pub fn running(&self, pgid: i32) -> Option<PaneForeground> {
        if let Some(argv) = self.table.argv(pgid) {
            return pane_foreground(&argv, self.shell);
        }
        let members: Vec<ProcessGroupMember> = self
            .table
            .group(pgid)
            .into_iter()
            .map(|stat| ProcessGroupMember {
                pid: stat.pid,
                ppid: stat.ppid,
            })
            .collect();
        group_descent_candidates(pgid, &members)
            .into_iter()
            .find_map(|pid| self.table.argv(pid))
            .and_then(|argv| pane_foreground(&argv, self.shell))
    }

    /// The restore capture's read of foreground group `pgid` (upstream's `ForegroundProcess.command`): the leader's
    /// program, or `None` when the group is led by the terminal's session leader (module docs), a shell holds the
    /// foreground, or the leader cannot be read.
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
        self.running(foreground_group(surface)?)
    }

    /// [`ForegroundReader::command`] for a terminal.
    #[must_use]
    pub fn command_in(&self, surface: &dyn Surface) -> Option<Vec<String>> {
        self.command(foreground_group(surface)?)
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
    use agterm_host::ProcessStat;
    use agterm_host::fakes::{FakeProcessTable, FakeSurface};

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
