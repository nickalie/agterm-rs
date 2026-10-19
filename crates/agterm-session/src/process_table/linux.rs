//! The Linux process table over `/proc` ([`crate::procfs`]'s parsers): `cmdline` for an argv (zombies and kernel
//! threads read empty and have none), `stat` for the ids and the terminal's foreground group, a scan of every `stat`
//! for a process group's members (Linux has no per-group listing), the `cwd` link for the working directory.

use std::fs;
use std::path::{Path, PathBuf};

use agterm_host::{ProcessStat, ProcessTable};

use crate::procfs;

/// The process table read from a procfs mount: `/proc`, or a fake tree in tests.
#[derive(Debug, Clone)]
pub struct ProcTable {
    root: PathBuf,
}

impl Default for ProcTable {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcTable {
    /// The live process table at `/proc`.
    #[must_use]
    pub fn new() -> Self {
        Self::at("/proc")
    }

    /// A process table laid out like `/proc` under `root`.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn process_file(&self, pid: i32, name: &str) -> PathBuf {
        self.root.join(pid.to_string()).join(name)
    }

    fn read_stat(path: &Path) -> Option<ProcessStat> {
        procfs::parse_stat(&fs::read(path).ok()?)
    }
}

impl ProcessTable for ProcTable {
    fn argv(&self, pid: i32) -> Option<Vec<String>> {
        procfs::parse_cmdline(&fs::read(self.process_file(pid, "cmdline")).ok()?)
    }

    fn stat(&self, pid: i32) -> Option<ProcessStat> {
        Self::read_stat(&self.process_file(pid, "stat"))
    }

    fn group(&self, pgrp: i32) -> Vec<ProcessStat> {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut members: Vec<ProcessStat> = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_str().is_some_and(procfs::is_pid))
            .filter_map(|entry| Self::read_stat(&entry.path().join("stat")))
            .filter(|stat| stat.pgrp == pgrp)
            .collect();
        members.sort_unstable_by_key(|stat| stat.pid);
        members
    }

    /// The `tpgid` of the stat line; `None` for a process without a terminal.
    fn terminal_foreground(&self, pid: i32) -> Option<i32> {
        procfs::parse_record(&fs::read(self.process_file(pid, "stat")).ok()?)
            .map(|record| record.foreground)
            .filter(|group| *group > 0)
    }

    fn working_directory(&self, pid: i32) -> Option<String> {
        let link = fs::read_link(self.process_file(pid, "cwd")).ok()?;
        link.into_os_string().into_string().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stat(pid: i32, parent: i32, pgrp: i32, session: i32) -> ProcessStat {
        ProcessStat {
            pid,
            ppid: parent,
            pgrp,
            session,
        }
    }

    #[test]
    fn a_fake_tree_answers_argv_stat_and_group() {
        let root = tempfile::tempdir().unwrap();
        let write = |pid: i32, cmdline: &[u8], stat: &[u8]| {
            let dir = root.path().join(pid.to_string());
            fs::create_dir(&dir).unwrap();
            fs::write(dir.join("cmdline"), cmdline).unwrap();
            fs::write(dir.join("stat"), stat).unwrap();
        };
        write(100, b"-zsh\0", b"100 (zsh) S 9 100 100 0");
        write(101, b"tail\0-f\0log\0", b"101 (tail) S 100 101 100 0");
        write(102, b"grep\0x\0", b"102 (gr\xc3) S 100 101 100 0");
        write(103, b"", b"103 (defunct) Z 100 101 100 0");
        fs::create_dir(root.path().join("self")).unwrap();
        let table = ProcTable::at(root.path());

        assert_eq!(
            table.argv(101),
            Some(vec!["tail".to_owned(), "-f".to_owned(), "log".to_owned()])
        );
        assert_eq!(table.argv(103), None);
        assert_eq!(table.argv(999), None);
        assert_eq!(table.stat(102), Some(stat(102, 100, 101, 100)));
        assert_eq!(table.stat(999), None);
        assert_eq!(
            table.group(101),
            [
                stat(101, 100, 101, 100),
                stat(102, 100, 101, 100),
                stat(103, 100, 101, 100)
            ]
        );
        assert!(table.group(555).is_empty());
        assert!(ProcTable::at(root.path().join("missing")).group(101).is_empty());
    }

    #[test]
    fn a_stat_line_gives_the_terminal_s_foreground_group() {
        let root = tempfile::tempdir().unwrap();
        let write = |pid: i32, stat: &[u8]| {
            let dir = root.path().join(pid.to_string());
            fs::create_dir(&dir).unwrap();
            fs::write(dir.join("stat"), stat).unwrap();
        };
        let tail = b" 0 0 0 0 1 2 0 0 20 0 1 0 987654 0 0";
        write(
            4242,
            &[&b"4242 (z (s) h) S 4200 4242 4242 34817 4300 4194560"[..], tail].concat(),
        );
        write(7, &[&b"7 (daemon) S 1 7 7 0 -1 0"[..], tail].concat());
        write(8, b"8 (short) S 1 8");
        let table = ProcTable::at(root.path());
        assert_eq!(table.terminal_foreground(4242), Some(4300));
        assert_eq!(table.terminal_foreground(7), None, "no controlling terminal");
        assert_eq!(table.terminal_foreground(8), None);
        assert_eq!(table.terminal_foreground(9), None);
    }

    #[test]
    fn the_live_table_reads_this_test_process() {
        let table = ProcTable::new();
        let pid = i32::try_from(std::process::id()).unwrap();
        let own = table.stat(pid).unwrap();
        assert_eq!(own.pid, pid);
        assert!(!table.argv(pid).unwrap().is_empty());
        assert!(table.group(own.pgrp).contains(&own));
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(table.working_directory(pid).as_deref(), cwd.to_str());
    }
}
