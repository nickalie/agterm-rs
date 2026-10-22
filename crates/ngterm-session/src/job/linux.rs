//! The job table on Linux: every `/proc/<pid>/stat` line that reads ([`crate::procfs`]), and `kill(2)`.

use std::fs;
use std::path::Path;

use ngterm_host::ProcessRecord;

use super::{ProcessEntry, Signal};
use crate::procfs;

pub(super) fn read_table() -> Option<Vec<ProcessEntry>> {
    read_table_at(Path::new("/proc"))
}

/// Every process under the procfs mount `root` whose stat line reads; `None` when `root` cannot be listed.
pub(super) fn read_table_at(root: &Path) -> Option<Vec<ProcessEntry>> {
    let entries = fs::read_dir(root).ok()?;
    Some(
        entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_str().is_some_and(procfs::is_pid))
            .filter_map(|entry| fs::read(entry.path().join("stat")).ok())
            .filter_map(|line| procfs::parse_entry(&line))
            .collect(),
    )
}

pub(super) fn session_members(session: i32) -> Option<Vec<ProcessRecord>> {
    session_members_at(Path::new("/proc"), session)
}

/// Every process under the procfs mount `root` whose stat line reads and names `session` as its session; `None` when
/// `root` cannot be listed.
pub(super) fn session_members_at(root: &Path, session: i32) -> Option<Vec<ProcessRecord>> {
    let entries = fs::read_dir(root).ok()?;
    Some(
        entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_str().is_some_and(procfs::is_pid))
            .filter_map(|entry| fs::read(entry.path().join("stat")).ok())
            .filter(|line| procfs::parse_stat(line).is_some_and(|stat| stat.session == session))
            .filter_map(|line| procfs::parse_record(&line))
            .collect(),
    )
}

pub(super) fn signal(signal: Signal, record: &ProcessRecord) {
    let number = match signal {
        Signal::HangUp => libc::SIGHUP,
        Signal::Kill => libc::SIGKILL,
    };
    // SAFETY: `kill` has no memory preconditions. The pid comes from a table read a moment ago (the sweep's match on
    // its start time, or the session scan's on its session id), so this is best effort against the pid's reuse.
    unsafe { libc::kill(record.pid, number) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fake_tree_reads_its_stat_lines_and_skips_what_does_not_parse() {
        let root = tempfile::tempdir().unwrap();
        let write = |name: &str, stat: &[u8]| {
            let dir = root.path().join(name);
            fs::create_dir(&dir).unwrap();
            fs::write(dir.join("stat"), stat).unwrap();
        };
        let tail = b" 0 0 0 0 1 2 0 0 20 0 1 0 987654 0 0";
        write("42", &[&b"42 (a b) S 7 42 42 34817 50 4194560"[..], tail].concat());
        write("43", b"43 (short) S 1 43");
        write("self", b"not a pid directory");
        let mut table = read_table_at(root.path()).unwrap();
        table.sort_unstable_by_key(|entry| entry.record.pid);
        assert_eq!(table.len(), 1);
        assert_eq!(table[0].parent, 7);
        assert_eq!(
            (table[0].record.pid, table[0].record.started, table[0].record.foreground),
            (42, 987_654, 50)
        );
        assert_eq!(
            read_table_at(&root.path().join("missing")),
            None,
            "unreadable, never empty"
        );
    }

    #[test]
    fn a_sessions_members_are_the_stat_lines_naming_it_whatever_their_group() {
        let root = tempfile::tempdir().unwrap();
        let write = |pid: i32, group: i32, session: i32| {
            let dir = root.path().join(pid.to_string());
            fs::create_dir(&dir).unwrap();
            let line = format!("{pid} (p) S 1 {group} {session} 34817 50 4194560 0 0 0 0 1 2 0 0 20 0 1 0 {pid}0 0 0");
            fs::write(dir.join("stat"), line).unwrap();
        };
        write(40, 40, 40);
        write(41, 41, 40);
        write(42, 40, 40);
        write(43, 43, 43);
        let mut members = session_members_at(root.path(), 40).unwrap();
        members.sort_unstable_by_key(|record| record.pid);
        let pids: Vec<(i32, i32, u64)> = members
            .iter()
            .map(|record| (record.pid, record.group, record.started))
            .collect();
        assert_eq!(pids, [(40, 40, 400), (41, 41, 410), (42, 40, 420)]);
        assert_eq!(session_members_at(&root.path().join("missing"), 40), None);
    }

    #[test]
    fn this_process_is_a_member_of_its_session() {
        // SAFETY: `getsid` has no memory preconditions.
        let session = unsafe { libc::getsid(0) };
        let pid = i32::try_from(std::process::id()).unwrap();
        assert!(session_members(session).unwrap().iter().any(|record| record.pid == pid));
    }
}
