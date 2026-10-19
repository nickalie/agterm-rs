//! The process sweeper (upstream's `ProcessSweeper` in `agterm/Ghostty/ProcessTable.swift`): it reads the live
//! process table and hangs up the foreground job a killed daemon's shell leaves running, as closing a terminal would
//! have. Injected into [`crate::ZmxClient`], which has none by default: a test's fake listing names pids that are real
//! processes on the machine running it.
//!
//! Linux reads `/proc/<pid>/stat`: the process group, the controlling terminal's foreground group (`tpgid`) and the
//! start time (clock ticks since boot), which together with the pid tell a process from one that recycled its pid
//! ([`parse_record`], from [`agterm_session::procfs`]).

use std::fs;
use std::path::PathBuf;

use agterm_core::live::sweep;
use agterm_host::ProcessRecord;
pub use agterm_session::procfs::parse_record;

/// Reads the process table; `None` when it cannot be read.
pub type TableReader = Box<dyn Fn() -> Option<Vec<ProcessRecord>> + Send + Sync>;
/// Sends `signal` to `pid`.
pub type Signaller = Box<dyn Fn(i32, i32) + Send + Sync>;

/// Reads the process table and signals what is left of a job.
pub struct Sweeper {
    table: TableReader,
    signal: Signaller,
}

impl Sweeper {
    /// The live sweeper over `/proc` and `kill(2)`.
    #[must_use]
    pub fn live() -> Self {
        Self::new(Box::new(|| read_table(&PathBuf::from("/proc"))), Box::new(send_signal))
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
            .map(|(name, shell)| (name.clone(), sweep::foreground_job(*shell, &table)))
            .filter(|(_, job)| !job.is_empty())
            .collect()
    }

    /// One shell's foreground job; `None` when the table cannot be read.
    #[must_use]
    pub fn foreground_job(&self, shell: i32) -> Option<Vec<ProcessRecord>> {
        (self.table)().map(|table| sweep::foreground_job(shell, &table))
    }

    /// Whether any process of `job` still runs; an unreadable table counts as running.
    #[must_use]
    pub fn is_running(&self, job: &[ProcessRecord]) -> bool {
        (self.table)().map_or(!job.is_empty(), |table| !sweep::survivors(job, &table).is_empty())
    }

    /// Sends `signal` to every process of `job` still running.
    pub fn send(&self, signal: i32, job: &[ProcessRecord]) {
        let Some(table) = (self.table)() else {
            return;
        };
        for record in sweep::survivors(job, &table) {
            (self.signal)(signal, record.pid);
        }
    }
}

fn send_signal(signal: i32, pid: i32) {
    // SAFETY: `kill` has no memory preconditions; the sweep matched `pid` on its start time a moment ago.
    unsafe { libc::kill(pid, signal) };
}

/// Every process under the procfs mount `root` whose stat line reads.
#[must_use]
pub fn read_table(root: &std::path::Path) -> Option<Vec<ProcessRecord>> {
    let entries = fs::read_dir(root).ok()?;
    Some(
        entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_str().is_some_and(is_pid))
            .filter_map(|entry| fs::read(entry.path().join("stat")).ok())
            .filter_map(|line| parse_record(&line))
            .collect(),
    )
}

fn is_pid(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[test]
    fn the_live_table_holds_this_test_process() {
        let table = read_table(&PathBuf::from("/proc")).unwrap();
        let pid = i32::try_from(std::process::id()).unwrap();
        assert!(table.iter().any(|record| record.pid == pid));
    }

    fn record(pid: i32, started: u64, group: i32, foreground: i32) -> ProcessRecord {
        ProcessRecord {
            pid,
            started,
            group,
            foreground,
        }
    }

    #[test]
    fn a_sweep_signals_only_the_surviving_job() {
        let table = Arc::new(Mutex::new(vec![
            record(10, 1, 10, 20),
            record(20, 2, 20, 20),
            record(21, 3, 20, 20),
        ]));
        let sent = Arc::new(Mutex::new(Vec::new()));
        let (reader, log) = (Arc::clone(&table), Arc::clone(&sent));
        let sweeper = Sweeper::new(
            Box::new(move || Some(reader.lock().unwrap().clone())),
            Box::new(move |signal, pid| log.lock().unwrap().push((signal, pid))),
        );
        let jobs = sweeper.capture(&[("agterm-a".to_owned(), 10), ("agterm-b".to_owned(), 99)]);
        assert_eq!(
            jobs,
            [(
                "agterm-a".to_owned(),
                vec![record(20, 2, 20, 20), record(21, 3, 20, 20)]
            )]
        );
        // the shell is gone and 21 exited: only 20 is hung up
        *table.lock().unwrap() = vec![record(20, 2, 20, 20)];
        sweeper.send(libc::SIGHUP, &jobs[0].1);
        assert_eq!(*sent.lock().unwrap(), [(libc::SIGHUP, 20)]);
        assert!(sweeper.is_running(&jobs[0].1));
        table.lock().unwrap().clear();
        assert!(!sweeper.is_running(&jobs[0].1));
    }

    #[test]
    fn an_unreadable_table_counts_as_running_and_signals_nothing() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&sent);
        let sweeper = Sweeper::new(
            Box::new(|| None),
            Box::new(move |signal, pid| log.lock().unwrap().push((signal, pid))),
        );
        let job = [record(20, 2, 20, 20)];
        assert!(sweeper.is_running(&job));
        assert!(!sweeper.is_running(&[]));
        assert_eq!(sweeper.foreground_job(10), None);
        assert!(sweeper.capture(&[("agterm-a".to_owned(), 10)]).is_empty());
        sweeper.send(libc::SIGHUP, &job);
        assert!(sent.lock().unwrap().is_empty());
    }
}
