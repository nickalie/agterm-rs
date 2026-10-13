//! The Linux process table over `/proc`, standing in for upstream's `sysctl` reads in
//! `Ghostty/ForegroundProcess.swift` (`KERN_PROCARGS2` for an argv, `KERN_PROC_PID` and `KERN_PROC_PGRP` for the ids).
//!
//! - An argv is `/proc/<pid>/cmdline`: NUL-terminated arguments, decoded lossily so an undecodable byte becomes U+FFFD,
//!   which the restore then refuses to replay, as upstream's lossy decode does. A zombie or a kernel thread reads
//!   empty and has no argv.
//! - The ids are `/proc/<pid>/stat`, whose command name may hold spaces and parentheses, so the fields after it are
//!   found from the last `)`.
//! - A process group's members come from a scan of every `/proc/<pid>/stat`: Linux has no per-group listing.

use std::fs;
use std::path::{Path, PathBuf};

use agterm_host::{ProcessStat, ProcessTable};

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
        parse_stat(&fs::read_to_string(path).ok()?)
    }
}

impl ProcessTable for ProcTable {
    fn argv(&self, pid: i32) -> Option<Vec<String>> {
        parse_cmdline(&fs::read(self.process_file(pid, "cmdline")).ok()?)
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
            .filter(|entry| entry.file_name().to_str().is_some_and(is_pid))
            .filter_map(|entry| Self::read_stat(&entry.path().join("stat")))
            .filter(|stat| stat.pgrp == pgrp)
            .collect();
        members.sort_unstable_by_key(|stat| stat.pid);
        members
    }
}

/// The argv in a `/proc/<pid>/cmdline`: NUL-separated arguments, the trailing NUL ending the last one, each decoded
/// lossily. `None` for empty content: a zombie or a kernel thread.
pub fn parse_cmdline(bytes: &[u8]) -> Option<Vec<String>> {
    if bytes.is_empty() {
        return None;
    }
    let args = bytes.strip_suffix(&[0]).unwrap_or(bytes);
    Some(
        args.split(|byte| *byte == 0)
            .map(|arg| String::from_utf8_lossy(arg).into_owned())
            .collect(),
    )
}

/// The ids in a `/proc/<pid>/stat` line, `pid (comm) state ppid pgrp session …`. The command name may hold spaces and
/// `)`, so the fields after it start at the last `)`. `None` for a malformed line.
pub fn parse_stat(text: &str) -> Option<ProcessStat> {
    let (head, tail) = text.rsplit_once(')')?;
    let (pid, _comm) = head.split_once('(')?;
    let mut fields = tail.split_ascii_whitespace().skip(1).map(str::parse::<i32>);
    Some(ProcessStat {
        pid: pid.trim().parse().ok()?,
        ppid: fields.next()?.ok()?,
        pgrp: fields.next()?.ok()?,
        session: fields.next()?.ok()?,
    })
}

fn is_pid(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_digit())
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
    fn a_cmdline_splits_at_nuls_and_the_trailing_one_ends_the_last_argument() {
        assert_eq!(
            parse_cmdline(b"ssh\0gate\0-t ssh inner\0"),
            Some(vec!["ssh".to_owned(), "gate".to_owned(), "-t ssh inner".to_owned()])
        );
        assert_eq!(
            parse_cmdline(b"sh\0-c\0\0"),
            Some(vec!["sh".to_owned(), "-c".to_owned(), String::new()])
        );
        // a process that rewrote its title may leave no terminator.
        assert_eq!(parse_cmdline(b"nginx: worker"), Some(vec!["nginx: worker".to_owned()]));
        assert_eq!(parse_cmdline(b""), None);
    }

    #[test]
    fn an_undecodable_byte_becomes_the_replacement_character() {
        assert_eq!(
            parse_cmdline(b"grep\0caf\xe9\0"),
            Some(vec!["grep".to_owned(), "caf\u{fffd}".to_owned()])
        );
    }

    #[test]
    fn a_stat_line_reads_past_spaces_and_parentheses_in_the_command_name() {
        assert_eq!(
            parse_stat("4242 (zsh) S 4200 4242 4242 34817 4300 4194560 0"),
            Some(stat(4242, 4200, 4242, 4242))
        );
        assert_eq!(parse_stat("17 (a (b) c) R 1 17 9 0 -1\n"), Some(stat(17, 1, 17, 9)));
        assert_eq!(parse_stat("18 ()) S 2 3 4 0"), Some(stat(18, 2, 3, 4)));
        assert_eq!(parse_stat("19 (x) S 2 3"), None);
        assert_eq!(parse_stat("(x) S 2 3 4"), None);
        assert_eq!(parse_stat(""), None);
    }

    #[test]
    fn a_fake_tree_answers_argv_stat_and_group() {
        let root = tempfile::tempdir().unwrap();
        let write = |pid: i32, cmdline: &[u8], stat: &str| {
            let dir = root.path().join(pid.to_string());
            fs::create_dir(&dir).unwrap();
            fs::write(dir.join("cmdline"), cmdline).unwrap();
            fs::write(dir.join("stat"), stat).unwrap();
        };
        write(100, b"-zsh\0", "100 (zsh) S 9 100 100 0");
        write(101, b"tail\0-f\0log\0", "101 (tail) S 100 101 100 0");
        write(102, b"grep\0x\0", "102 (grep) S 100 101 100 0");
        write(103, b"", "103 (defunct) Z 100 101 100 0");
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
    fn the_live_table_reads_this_test_process() {
        let table = ProcTable::new();
        let pid = i32::try_from(std::process::id()).unwrap();
        let own = table.stat(pid).unwrap();
        assert_eq!(own.pid, pid);
        assert!(!table.argv(pid).unwrap().is_empty());
        assert!(table.group(own.pgrp).contains(&own));
    }
}
