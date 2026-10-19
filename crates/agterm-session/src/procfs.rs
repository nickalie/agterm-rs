//! The Linux process table's stat line (`/proc/<pid>/stat`), read as bytes: the process group, the controlling
//! terminal's foreground group (`tpgid`) and the start time (clock ticks since boot), which together with the pid tell
//! a process from one that recycled its pid. A pure parser, defined on every OS so its rules are tested everywhere;
//! only Linux has the file.

use agterm_host::ProcessRecord;

/// The record in a `/proc/<pid>/stat` line, `pid (comm) state ppid pgrp session tty_nr tpgid ... starttime ...`: the
/// pid before the first `(`, the rest after the last `)`, since the command name may hold spaces, `)` and bytes that
/// are not UTF-8. `None` for a malformed line.
#[must_use]
pub fn parse_record(line: &[u8]) -> Option<ProcessRecord> {
    let close = line.iter().rposition(|byte| *byte == b')')?;
    let open = line[..close].iter().position(|byte| *byte == b'(')?;
    let pid = std::str::from_utf8(&line[..open]).ok()?.trim().parse().ok()?;
    let fields: Vec<&str> = std::str::from_utf8(&line[close + 1..])
        .ok()?
        .split_ascii_whitespace()
        .collect();
    // after the name: state(0) ppid(1) pgrp(2) session(3) tty_nr(4) tpgid(5) ... starttime(19)
    Some(ProcessRecord {
        pid,
        started: fields.get(19)?.parse().ok()?,
        group: fields.get(2)?.parse().ok()?,
        foreground: fields.get(5)?.parse().ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE: &[u8] = b"4242 (a (b) c) S 4200 4250 4242 34817 4260 4194560 0 0 0 0 1 2 0 0 20 0 1 0 987654 0 0";

    #[test]
    fn a_stat_line_gives_the_group_the_terminal_foreground_and_the_start_time() {
        assert_eq!(
            parse_record(LINE),
            Some(ProcessRecord {
                pid: 4242,
                started: 987_654,
                group: 4250,
                foreground: 4260,
            })
        );
        assert_eq!(parse_record(b"17 (x) S 1 17 17 0 -1"), None);
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
}
