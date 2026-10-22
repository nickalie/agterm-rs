//! Which processes a killed daemon's shell leaves running in its terminal's foreground, and which of a remembered job
//! still run (upstream's `ProcessSweep.swift`). A host reads the process table before a kill and signals what survives
//! after it, as closing a terminal would have hung them up.

use std::collections::HashSet;

use ngterm_host::ProcessRecord;

/// The members of the terminal's foreground group when `shell` is not in it: the job the shell started. Empty when the
/// shell holds its own foreground, has no terminal, or is not in `table`.
#[must_use]
pub fn foreground_job(shell: i32, table: &[ProcessRecord]) -> Vec<ProcessRecord> {
    let Some(leader) = table.iter().find(|record| record.pid == shell) else {
        return Vec::new();
    };
    if leader.foreground <= 1 || leader.foreground == leader.group {
        return Vec::new();
    }
    table
        .iter()
        .filter(|record| record.group == leader.foreground && record.pid != shell)
        .copied()
        .collect()
}

/// The entries of `snapshot` still running in `table`, matched on pid AND start time, so a recycled pid is never
/// taken for the process it replaced. Pid 1 and below never qualify.
#[must_use]
pub fn survivors(snapshot: &[ProcessRecord], table: &[ProcessRecord]) -> Vec<ProcessRecord> {
    let running: HashSet<(i32, u64)> = table.iter().map(|record| (record.pid, record.started)).collect();
    snapshot
        .iter()
        .filter(|record| record.pid > 1 && running.contains(&(record.pid, record.started)))
        .copied()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(pid: i32, started: u64, group: i32, foreground: i32) -> ProcessRecord {
        ProcessRecord {
            pid,
            started,
            group,
            foreground,
        }
    }

    #[test]
    fn the_job_is_the_foreground_group_without_the_shell() {
        let table = [
            record(10, 1, 10, 20),
            record(20, 2, 20, 20),
            record(21, 3, 20, 20),
            record(30, 4, 30, 30),
        ];
        assert_eq!(
            foreground_job(10, &table),
            [record(20, 2, 20, 20), record(21, 3, 20, 20)]
        );
    }

    #[test]
    fn a_shell_holding_its_own_foreground_has_no_job() {
        let table = [record(10, 1, 10, 10), record(20, 2, 20, 10)];
        assert!(foreground_job(10, &table).is_empty());
        // no terminal, or init's group
        assert!(foreground_job(11, &[record(11, 1, 11, -1)]).is_empty());
        assert!(foreground_job(12, &[record(12, 1, 12, 1)]).is_empty());
        // a shell the table does not hold
        assert!(foreground_job(99, &table).is_empty());
    }

    #[test]
    fn a_survivor_matches_on_pid_and_start_time() {
        let job = [record(20, 2, 20, 20), record(21, 3, 20, 20), record(1, 0, 1, 1)];
        let table = [record(20, 2, 20, 20), record(21, 9, 20, 20), record(1, 0, 1, 1)];
        // 21 was recycled by another process, and pid 1 never qualifies
        assert_eq!(survivors(&job, &table), [record(20, 2, 20, 20)]);
        assert!(survivors(&job, &[]).is_empty());
    }
}
