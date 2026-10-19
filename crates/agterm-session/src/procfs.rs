//! The Linux process table's files, read as bytes: `/proc/<pid>/stat` (the parent, the process group, the session,
//! the controlling terminal's foreground group `tpgid` and the start time in clock ticks since boot, which together
//! with the pid tell a process from one that recycled its pid) and `/proc/<pid>/cmdline` (the argv). Pure parsers,
//! defined on every OS so their rules are tested everywhere; only Linux has the files.
//!
//! A stat line's command name may hold spaces, parentheses and any byte (the kernel cuts it at 15 bytes, even inside
//! a UTF-8 character), so the pid is read before the first `(` and the other fields after the last `)`.

use agterm_host::{ProcessRecord, ProcessStat};

use crate::job::ProcessEntry;

/// The pid before a stat line's command name and the fields after it: `state(0) ppid(1) pgrp(2) session(3) tty_nr(4)
/// tpgid(5) ... starttime(19)`. `None` for a malformed line.
fn split_stat(line: &[u8]) -> Option<(i32, Vec<&str>)> {
    let close = line.iter().rposition(|byte| *byte == b')')?;
    let open = line[..close].iter().position(|byte| *byte == b'(')?;
    let pid = std::str::from_utf8(&line[..open]).ok()?.trim().parse().ok()?;
    let fields = std::str::from_utf8(&line[close + 1..])
        .ok()?
        .split_ascii_whitespace()
        .collect();
    Some((pid, fields))
}

fn field<T: std::str::FromStr>(fields: &[&str], index: usize) -> Option<T> {
    fields.get(index)?.parse().ok()
}

/// The sweep's record in a `/proc/<pid>/stat` line, `pid (comm) state ppid pgrp session tty_nr tpgid ... starttime
/// ...`. `None` for a malformed line.
#[must_use]
pub fn parse_record(line: &[u8]) -> Option<ProcessRecord> {
    let (pid, fields) = split_stat(line)?;
    Some(ProcessRecord {
        pid,
        started: field(&fields, 19)?,
        group: field(&fields, 2)?,
        foreground: field(&fields, 5)?,
    })
}

/// The job table's entry in a stat line: [`parse_record`]'s record and the parent. `None` for a malformed line.
#[must_use]
pub fn parse_entry(line: &[u8]) -> Option<ProcessEntry> {
    let (_, fields) = split_stat(line)?;
    Some(ProcessEntry {
        record: parse_record(line)?,
        parent: field(&fields, 1)?,
    })
}

/// The process table's ids in a stat line: the pid, the parent's, the process group's and the session's. Only the
/// fields through the session must be there. `None` for a malformed line.
#[must_use]
pub fn parse_stat(line: &[u8]) -> Option<ProcessStat> {
    let (pid, fields) = split_stat(line)?;
    Some(ProcessStat {
        pid,
        ppid: field(&fields, 1)?,
        pgrp: field(&fields, 2)?,
        session: field(&fields, 3)?,
    })
}

/// The argv in a `/proc/<pid>/cmdline`: NUL-separated arguments, the trailing NUL ending the last one, each decoded
/// lossily, so an undecodable byte becomes U+FFFD, which the restore then refuses to replay, as upstream's lossy decode
/// does. `None` for empty content: a zombie or a kernel thread.
#[must_use]
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

/// Whether a `/proc` directory entry's name is a pid.
#[must_use]
pub fn is_pid(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE: &[u8] = b"4242 (a (b) c) S 4200 4250 4242 34817 4260 4194560 0 0 0 0 1 2 0 0 20 0 1 0 987654 0 0";

    fn stat(pid: i32, parent: i32, pgrp: i32, session: i32) -> ProcessStat {
        ProcessStat {
            pid,
            ppid: parent,
            pgrp,
            session,
        }
    }

    #[test]
    fn a_stat_line_gives_the_group_the_terminal_foreground_and_the_start_time() {
        let record = ProcessRecord {
            pid: 4242,
            started: 987_654,
            group: 4250,
            foreground: 4260,
        };
        assert_eq!(parse_record(LINE), Some(record));
        assert_eq!(parse_entry(LINE), Some(ProcessEntry { record, parent: 4200 }));
        assert_eq!(parse_record(b"17 (x) S 1 17 17 0 -1"), None);
        assert_eq!(parse_entry(b"17 (x) S 1 17 17 0 -1"), None);
        assert_eq!(parse_record(b""), None);
    }

    #[test]
    fn a_name_with_bytes_that_are_not_utf8_still_parses_and_a_terminalless_process_reads_its_minus_one() {
        let line = b"7 (\xff\xfe) S 1 7 7 0 -1 4194560 0 0 0 0 1 2 0 0 20 0 1 0 55 0 0";
        assert_eq!(
            parse_record(line),
            Some(ProcessRecord {
                pid: 7,
                started: 55,
                group: 7,
                foreground: -1,
            })
        );
        assert_eq!(
            parse_record(b"x (a) S 1 7 7 0 -1 0 0 0 0 0 0 0 0 0 0 0 0 0 55"),
            None,
            "a pid that is no number"
        );
        assert_eq!(parse_record(b"7 a) S"), None, "no opening parenthesis");
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
            parse_stat(b"4242 (zsh) S 4200 4242 4242 34817 4300 4194560 0"),
            Some(stat(4242, 4200, 4242, 4242))
        );
        assert_eq!(parse_stat(b"17 (a (b) c) R 1 17 9 0 -1\n"), Some(stat(17, 1, 17, 9)));
        assert_eq!(parse_stat(b"18 ()) S 2 3 4 0"), Some(stat(18, 2, 3, 4)));
        assert_eq!(parse_stat(b"19 (x) S 2 3"), None);
        assert_eq!(parse_stat(b"(x) S 2 3 4"), None);
        assert_eq!(parse_stat(b""), None);
    }

    #[test]
    fn a_command_name_that_is_not_utf8_still_reads() {
        // fourteen ASCII bytes and an `é` the kernel cut after its first byte
        assert_eq!(
            parse_stat(b"20 (abcdefghijklmn\xc3) S 1 20 20 0"),
            Some(stat(20, 1, 20, 20))
        );
        assert_eq!(parse_stat(b"21 (\xff\xfe) S 1 21 9 0"), Some(stat(21, 1, 21, 9)));
    }

    #[test]
    fn a_pid_directory_is_all_digits() {
        assert!(is_pid("4242"));
        assert!(!is_pid("self"));
        assert!(!is_pid("42a"));
        assert!(!is_pid(""));
    }
}
