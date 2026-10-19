//! The job table on Linux: every `/proc/<pid>/stat` line that reads ([`crate::procfs`]), and `kill(2)`.

use std::fs;
use std::path::Path;

use agterm_host::ProcessRecord;

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

pub(super) fn signal(signal: Signal, record: &ProcessRecord) {
    let number = match signal {
        Signal::HangUp => libc::SIGHUP,
        Signal::Kill => libc::SIGKILL,
    };
    // SAFETY: `kill` has no memory preconditions; the sweep matched the pid on its start time a moment ago.
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
}
