//! The job table on macOS: every pid `proc_listpids` lists and its BSD info (`proc_pidinfo(PROC_PIDTBSDINFO)`: the
//! parent, the process group, the controlling terminal's foreground group and the start time), and `kill(2)`.
//! Upstream reads the same fields from `sysctl(KERN_PROC_ALL)`'s `kinfo_proc`, which the `libc` crate does not define
//! for Apple; herdr reads them through `libproc` as here (`src/platform/macos.rs`, `process_bsdinfo`,
//! `process_group_pids`, `bsdinfo_start_token`, at `HERDR_REV`, Apache License 2.0, modified). The start time is
//! upstream's: microseconds since the epoch.

use std::ffi::c_void;
use std::mem::size_of;

use agterm_host::ProcessRecord;

use super::{ProcessEntry, Signal};

/// `libproc.h`'s `PROC_ALL_PIDS`.
const PROC_ALL_PIDS: u32 = 1;
/// `libproc.h`'s `PROC_PGRP_ONLY`.
pub(crate) const PROC_PGRP_ONLY: u32 = 2;

pub(super) fn read_table() -> Option<Vec<ProcessEntry>> {
    Some(list_pids(PROC_ALL_PIDS, 0)?.into_iter().filter_map(entry).collect())
}

pub(super) fn signal(signal: Signal, pid: i32) {
    let number = match signal {
        Signal::HangUp => libc::SIGHUP,
        Signal::Kill => libc::SIGKILL,
    };
    // SAFETY: `kill` has no memory preconditions; the sweep matched `pid` on its start time a moment ago.
    unsafe { libc::kill(pid, number) };
}

/// The table entry of `pid`; `None` when it is gone or its BSD info cannot be read.
fn entry(pid: i32) -> Option<ProcessEntry> {
    let info = bsdinfo(pid)?;
    Some(ProcessEntry {
        record: ProcessRecord {
            pid: i32::try_from(info.pbi_pid).ok()?,
            started: info
                .pbi_start_tvsec
                .saturating_mul(1_000_000)
                .saturating_add(info.pbi_start_tvusec),
            group: i32::try_from(info.pbi_pgid).ok()?,
            // zero for a process without a controlling terminal
            foreground: i32::try_from(info.e_tpgid).unwrap_or(0),
        },
        parent: i32::try_from(info.pbi_ppid).ok()?,
    })
}

/// The BSD info of `pid`; `None` when it is gone or cannot be read.
pub(crate) fn bsdinfo(pid: i32) -> Option<libc::proc_bsdinfo> {
    if pid <= 0 {
        return None;
    }
    // SAFETY: `proc_bsdinfo` is plain old data, for which all zeroes is a valid value.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = i32::try_from(size_of::<libc::proc_bsdinfo>()).ok()?;
    // SAFETY: the buffer is a writable `proc_bsdinfo` of exactly `size` bytes.
    let read = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) };
    (read == size).then_some(info)
}

/// The pids `proc_listpids` lists for `kind` (`PROC_ALL_PIDS`, `PROC_PGRP_ONLY` with the group as `info`); `None`
/// when the call fails. The list can grow between the sizing call and the read, so a full buffer is read again with
/// room to spare.
pub(crate) fn list_pids(kind: u32, info: u32) -> Option<Vec<i32>> {
    // SAFETY: a null buffer of size zero only asks for the size the list needs.
    let needed = unsafe { libc::proc_listpids(kind, info, std::ptr::null_mut(), 0) };
    let mut capacity = usize::try_from(needed).ok()? / size_of::<i32>() + 64;
    for _ in 0..8 {
        let mut pids = vec![0_i32; capacity];
        let bytes = i32::try_from(pids.len() * size_of::<i32>()).ok()?;
        // SAFETY: `pids` is a writable buffer of `bytes` bytes.
        let read = unsafe { libc::proc_listpids(kind, info, pids.as_mut_ptr().cast::<c_void>(), bytes) };
        let read = usize::try_from(read).ok()?;
        if read < pids.len() * size_of::<i32>() {
            pids.truncate(read / size_of::<i32>());
            pids.retain(|pid| *pid > 0);
            return Some(pids);
        }
        capacity = capacity.saturating_mul(2);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_reads_its_own_group_and_start_time() {
        let pid = i32::try_from(std::process::id()).unwrap();
        let own = entry(pid).unwrap();
        // SAFETY: `getpgrp` and `getppid` have no preconditions.
        let group = unsafe { libc::getpgrp() };
        // SAFETY: as above.
        let parent = unsafe { libc::getppid() };
        assert_eq!((own.record.pid, own.record.group, own.parent), (pid, group, parent));
        assert!(
            own.record.started > 1_000_000_000 * 1_000_000,
            "microseconds since the epoch"
        );
        assert!(
            list_pids(PROC_PGRP_ONLY, u32::try_from(group).unwrap())
                .unwrap()
                .contains(&pid)
        );
        assert_eq!(entry(-1), None);
    }
}
