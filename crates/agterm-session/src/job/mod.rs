//! A terminal's foreground job on every OS, and the sweeper that hangs up what a killed session's shell leaves of it
//! running, as closing a terminal would have (upstream's `ProcessSweeper` in `agterm/Ghostty/ProcessTable.swift`,
//! over the host-free policy of [`agterm_core::live::sweep`]).
//!
//! - [`read_table`] reads every process this user can see as a [`ProcessEntry`]: Linux's `/proc` stat lines
//!   ([`crate::procfs`]), macOS's `sysctl(KERN_PROC_ALL)` (upstream's read), Windows' tool-help snapshot with each
//!   process's creation time and parent read through one handle. `None` when the table cannot be read: a caller waiting for a job to
//!   end must never take that for "no job".
//! - [`foreground_job`] picks a shell's job from a table: on Unix the members of its terminal's foreground process
//!   group when the shell is not in it ([`group_job`]); on Windows, which has no job control, the shell's live
//!   descendants ([`descendant_job`], herdr's process snapshot). Both rules are pure and tested on every OS.
//! - [`session_members`] reads, on Unix, every process of a session, which a live pane's forced end signals.
//! - [`signal`] sends a [`Signal`] to a [`ProcessRecord`]: `SIGHUP` or `SIGKILL` to its pid on Unix; on Windows both
//!   terminate the process, since a console that goes away ends what it ran as well, and only once the handle opened
//!   to end it names the process the record was taken of (its creation time).
//! - [`Sweeper`] puts them together for the session hosts: the job captured before a kill, signalled after it, and
//!   matched on pid AND start time, so a recycled pid is never signalled.
//!
//! herdr: `src/platform/{linux,macos,windows}.rs` (`foreground_job`, `process_start_token`, `snapshot_processes`,
//! `descendant_entries`, `process_creation_time`, `session_processes`, `signal_processes`) at `HERDR_REV` (`build/pins.env`), licensed under
//! the Apache License 2.0, modified: one table read for a whole sweep in place of per-pid reads, the descendant walk
//! guarded by creation times (a child never predates its parent, so a recycled parent pid adopts nothing), and a
//! Windows termination opened with `PROCESS_TERMINATE`.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
pub(crate) mod macos;
#[cfg(windows)]
pub(crate) mod windows;

#[cfg(target_os = "linux")]
use linux as sys;
#[cfg(target_os = "macos")]
use macos as sys;
#[cfg(windows)]
use windows as sys;

use std::collections::{HashMap, HashSet, VecDeque};

use agterm_core::live::sweep;
use agterm_host::ProcessRecord;

/// One process of the table: what a sweep matches it by, and its parent, which Windows' job rule walks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProcessEntry {
    /// Its pid, start time, process group and terminal foreground group. On Windows a process stands in for its own
    /// group (herdr's rule; Windows has no job control) and has no terminal foreground (zero).
    pub record: ProcessRecord,
    /// Its parent's pid; zero when it has none.
    pub parent: i32,
}

/// What [`signal`] sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// A terminal's hangup: `SIGHUP` on Unix; on Windows the process is terminated.
    HangUp,
    /// `SIGKILL` on Unix; on Windows the process is terminated.
    Kill,
}

/// Every process this user can see; `None` when the table cannot be read.
#[must_use]
pub fn read_table() -> Option<Vec<ProcessEntry>> {
    sys::read_table()
}

/// Every process of the session `session` (a session leader's pid), on Unix: what [`signal`] reaches a session's
/// processes through when they sit in process groups of their own (a job-controlling shell's jobs), as herdr's
/// `session_processes` does for its pane shutdown. Linux reads every `/proc` stat line's session, macOS asks `getsid`
/// of every process `sysctl(KERN_PROC_ALL)` lists. `None` when the table cannot be read. Windows has no sessions:
/// a pane's tree is its job there.
#[cfg(unix)]
#[must_use]
pub fn session_members(session: i32) -> Option<Vec<ProcessRecord>> {
    sys::session_members(session)
}

/// Sends `signal` to the process of `record`, best effort: a process gone meanwhile, or one this user may not signal,
/// is left alone.
pub fn signal(signal: Signal, record: &ProcessRecord) {
    sys::signal(signal, record);
}

/// The foreground job of the terminal `shell` leads, this OS's rule: [`descendant_job`] on Windows, [`group_job`]
/// elsewhere. Empty when the shell holds its own foreground or is not in `table`.
#[must_use]
pub fn foreground_job(shell: i32, table: &[ProcessEntry]) -> Vec<ProcessRecord> {
    if cfg!(windows) {
        descendant_job(shell, table)
    } else {
        group_job(shell, table)
    }
}

/// The Unix rule: the members of the terminal's foreground process group when `shell` is not in it, the job the
/// shell started ([`agterm_core::live::sweep::foreground_job`]).
#[must_use]
pub fn group_job(shell: i32, table: &[ProcessEntry]) -> Vec<ProcessRecord> {
    let records: Vec<ProcessRecord> = table.iter().map(|entry| entry.record).collect();
    sweep::foreground_job(shell, &records)
}

/// The Windows rule (herdr's process snapshot): every live descendant of `shell`, in breadth-first order. A child
/// that started before its parent was adopted by a recycled pid, so it and its own tree are not the shell's.
#[must_use]
pub fn descendant_job(shell: i32, table: &[ProcessEntry]) -> Vec<ProcessRecord> {
    let Some(root) = table.iter().find(|entry| entry.record.pid == shell) else {
        return Vec::new();
    };
    let mut children: HashMap<i32, Vec<&ProcessEntry>> = HashMap::new();
    for entry in table.iter().filter(|entry| entry.record.pid != entry.parent) {
        children.entry(entry.parent).or_default().push(entry);
    }
    let mut job = Vec::new();
    let mut seen = HashSet::from([shell]);
    let mut queue = VecDeque::from([root]);
    while let Some(parent) = queue.pop_front() {
        let born_after = |child: &&&ProcessEntry| child.record.started >= parent.record.started;
        for child in children
            .get(&parent.record.pid)
            .into_iter()
            .flatten()
            .filter(born_after)
        {
            if seen.insert(child.record.pid) {
                job.push(child.record);
                queue.push_back(child);
            }
        }
    }
    job
}

/// Reads the process table; `None` when it cannot be read.
pub type TableReader = Box<dyn Fn() -> Option<Vec<ProcessEntry>> + Send + Sync>;
/// Sends a signal to the process of a record.
pub type Signaller = Box<dyn Fn(Signal, &ProcessRecord) + Send + Sync>;

/// Reads the process table and signals what is left of a job (upstream's `ProcessSweeper`).
pub struct Sweeper {
    table: TableReader,
    signal: Signaller,
}

impl Sweeper {
    /// The live sweeper over this OS's process table and its signals.
    #[must_use]
    pub fn live() -> Self {
        Self::new(Box::new(read_table), Box::new(signal))
    }

    #[must_use]
    pub fn new(table: TableReader, signal: Signaller) -> Self {
        Self { table, signal }
    }

    /// Each named shell's foreground job, omitting shells without one; empty when the table cannot be read.
    #[must_use]
    pub fn capture(&self, shells: &[(String, i32)]) -> Vec<(String, Vec<ProcessRecord>)> {
        let Some(table) = (self.table)() else {
            return Vec::new();
        };
        shells
            .iter()
            .map(|(name, shell)| (name.clone(), foreground_job(*shell, &table)))
            .filter(|(_, job)| !job.is_empty())
            .collect()
    }

    /// One shell's foreground job; `None` when the table cannot be read.
    #[must_use]
    pub fn foreground_job(&self, shell: i32) -> Option<Vec<ProcessRecord>> {
        (self.table)().map(|table| foreground_job(shell, &table))
    }

    /// Whether any process of `job` still runs; an unreadable table counts as running.
    #[must_use]
    pub fn is_running(&self, job: &[ProcessRecord]) -> bool {
        (self.table)().map_or(!job.is_empty(), |table| !survivors(job, &table).is_empty())
    }

    /// Sends `signal` to every process of `job` still running; nothing when the table cannot be read.
    pub fn send(&self, signal: Signal, job: &[ProcessRecord]) {
        let Some(table) = (self.table)() else {
            return;
        };
        for record in survivors(job, &table) {
            (self.signal)(signal, &record);
        }
    }
}

fn survivors(job: &[ProcessRecord], table: &[ProcessEntry]) -> Vec<ProcessRecord> {
    let records: Vec<ProcessRecord> = table.iter().map(|entry| entry.record).collect();
    sweep::survivors(job, &records)
}

#[cfg(test)]
mod tests;
