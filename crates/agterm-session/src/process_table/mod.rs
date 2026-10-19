//! The process table a pane's foreground is read through ([`agterm_host::ProcessTable`]) on every OS, standing in for
//! upstream's `sysctl` reads in `Ghostty/ForegroundProcess.swift`: the quit-time restore capture, the `tree`'s
//! foreground fields and a live restart's working directory.
//!
//! - Linux ([`linux`]): `/proc/<pid>/cmdline` for an argv, `/proc/<pid>/stat` for the ids and the terminal's
//!   foreground group ([`crate::procfs`]), a scan of every stat line for a group's members, `/proc/<pid>/cwd`.
//! - macOS: `sysctl(KERN_PROCARGS2)` for an argv ([`parse_procargs`], upstream's), `sysctl(KERN_PROC_PID)`'s
//!   `kinfo_proc` for the ids and the terminal's foreground group, `sysctl(KERN_PROC_PGRP)` for a group's members
//!   (upstream's `processGroup`), `getsid` for the session, `proc_pidinfo(PROC_PIDVNODEPATHINFO)` for the working
//!   directory (upstream's `workingDirectory`).
//! - Windows: `NtQueryInformationProcess(ProcessCommandLineInformation)` split with `CommandLineToArgvW` for an argv
//!   (herdr's), a tool-help snapshot for the parent, the process environment block's current directory for the
//!   working directory (herdr's). Windows has neither process groups nor sessions: a process stands in for its own
//!   group, as in herdr, so a group is that one process, and the session is zero (no process leads one). A console
//!   has no foreground group either; [`console_foreground`] stands in for it.
//!
//! An argv is decoded lossily everywhere, so an undecodable byte (or an unpaired UTF-16 surrogate) becomes U+FFFD,
//! which the restore then refuses to replay, as upstream's lossy decode does.

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
pub use linux::ProcTable;
#[cfg(target_os = "macos")]
pub use macos::ProcTable;
#[cfg(windows)]
pub use windows::ProcTable;

use crate::job::ProcessEntry;

/// Upstream's cap on a `KERN_PROCARGS2` argc (`CommandRestore.maxArgCount`): anything above is a corrupt blob.
const MAX_ARG_COUNT: i32 = 4096;

/// The argv in a macOS `KERN_PROCARGS2` blob (upstream's `CommandRestore.parseProcArgs`): a host-order `i32` argc,
/// the NUL-terminated executable path, zero or more NUL padding bytes, then `argc` NUL-terminated arguments (the
/// environment follows, ignored), each decoded lossily. `None` for a truncated or implausible blob.
#[must_use]
pub fn parse_procargs(bytes: &[u8]) -> Option<Vec<String>> {
    if bytes.len() <= 4 {
        return None;
    }
    let count = i32::from_ne_bytes(bytes[..4].try_into().ok()?);
    if count <= 0 || count >= MAX_ARG_COUNT {
        return None;
    }
    let count = usize::try_from(count).ok()?;
    let mut rest = &bytes[4..];
    // skip the executable path up to its NUL, then the NUL padding before argv[0]
    let path_end = rest.iter().position(|byte| *byte == 0).unwrap_or(rest.len());
    rest = &rest[path_end..];
    let padding = rest.iter().position(|byte| *byte != 0).unwrap_or(rest.len());
    rest = &rest[padding..];
    let mut argv = Vec::with_capacity(count);
    while argv.len() < count && !rest.is_empty() {
        let end = rest.iter().position(|byte| *byte == 0).unwrap_or(rest.len());
        argv.push(String::from_utf8_lossy(&rest[..end]).into_owned());
        rest = rest.get(end + 1..).unwrap_or_default();
    }
    (argv.len() == count).then_some(argv)
}

/// What stands in on Windows for the foreground group of the console `shell` holds: the newest live direct child of
/// the shell (the program it runs; a pipeline's or a wrapper's own children sit under it), else the shell itself,
/// which then holds its own foreground as a Unix shell at its prompt does. A child that started before the shell was
/// adopted by a recycled pid and does not count. `None` when the shell is not in `table`.
#[must_use]
pub fn console_foreground(shell: i32, table: &[ProcessEntry]) -> Option<i32> {
    let root = table.iter().find(|entry| entry.record.pid == shell)?;
    let newest = table
        .iter()
        .filter(|entry| entry.parent == shell && entry.record.pid != shell)
        .filter(|entry| entry.record.started >= root.record.started)
        .max_by_key(|entry| (entry.record.started, entry.record.pid));
    Some(newest.map_or(shell, |entry| entry.record.pid))
}

#[cfg(test)]
mod tests;
