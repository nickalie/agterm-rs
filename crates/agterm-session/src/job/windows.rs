//! The job table on Windows: every process a tool-help snapshot lists, with its creation time and parent read through
//! one handle to it, and the termination that stands in for both signals.
//!
//! herdr: `src/platform/windows.rs` (`snapshot_processes`, `ProcessHandle`, `process_creation_time`,
//! `process_start_token`, `signal_processes`) at `HERDR_REV` (`build/pins.env`), licensed under the Apache License
//! 2.0, modified: a process whose creation time cannot be read (one of another session or a protected one) is left
//! out, since it could be neither matched again nor ended; the parent is read through the handle the creation time
//! is (`NtQueryInformationProcess(ProcessBasicInformation)`), not taken from the snapshot, so a pid that a new process
//! took after the snapshot never pairs that process with the old one's parent; a failed snapshot reads as an
//! unreadable table, not an empty one; the termination opens the process with `PROCESS_TERMINATE`, ends it only when
//! that handle's creation time is the record's (a pid recycled since the table was read is left alone), and ends it
//! on a hangup too.

use std::mem::size_of;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};

use agterm_host::ProcessRecord;
use windows_sys::Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation};
use windows_sys::Win32::Foundation::{FILETIME, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_ACCESS_RIGHTS, PROCESS_BASIC_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_TERMINATE, TerminateProcess,
};

use super::{ProcessEntry, Signal};

/// The exit code a terminated job process gets, herdr's.
const TERMINATED: u32 = 1;

/// One process of a tool-help snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Snapshotted {
    pub(crate) pid: u32,
    pub(crate) parent: u32,
}

pub(super) fn read_table() -> Option<Vec<ProcessEntry>> {
    Some(snapshot()?.into_iter().filter_map(entry).collect())
}

/// Ends the process of `record`: Windows has no hangup to send, and a console that closes ends what it ran. A
/// process that now holds the record's pid but was created at another time is a later one, and is left alone; the
/// handle checked is the handle ended, so the pid cannot change hands in between.
pub(super) fn signal(_signal: Signal, record: &ProcessRecord) {
    let Some(process) = u32::try_from(record.pid)
        .ok()
        .and_then(|pid| open(pid, PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION))
    else {
        return;
    };
    if creation_time(&process) != Some(record.started) {
        return;
    }
    // SAFETY: the handle is an open process handle with `PROCESS_TERMINATE`.
    unsafe { TerminateProcess(process.as_raw_handle(), TERMINATED) };
}

/// The table entry of a snapshotted pid, read through one handle: the process holding the pid now, with its own
/// creation time and parent; `None` when it is gone or may not be queried.
fn entry(process: Snapshotted) -> Option<ProcessEntry> {
    let pid = i32::try_from(process.pid).ok()?;
    let handle = open(process.pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let started = creation_time(&handle)?;
    Some(ProcessEntry {
        record: ProcessRecord {
            pid,
            started,
            group: pid,
            foreground: 0,
        },
        parent: parent(&handle)?,
    })
}

/// The pid of the process that created `process` (`InheritedFromUniqueProcessId`); `None` when it cannot be read.
fn parent(process: &OwnedHandle) -> Option<i32> {
    let mut basic = PROCESS_BASIC_INFORMATION::default();
    let size = u32::try_from(size_of::<PROCESS_BASIC_INFORMATION>()).ok()?;
    // SAFETY: the handle is an open process handle with query access, and `basic` a writable
    // `PROCESS_BASIC_INFORMATION` of exactly `size` bytes.
    let status = unsafe {
        NtQueryInformationProcess(
            process.as_raw_handle(),
            ProcessBasicInformation,
            (&raw mut basic).cast(),
            size,
            std::ptr::null_mut(),
        )
    };
    if status < 0 {
        return None;
    }
    i32::try_from(basic.InheritedFromUniqueProcessId).ok()
}

/// Every process of a fresh tool-help snapshot; `None` when no snapshot could be taken.
pub(crate) fn snapshot() -> Option<Vec<Snapshotted>> {
    // SAFETY: no pointer arguments; the result is checked before use.
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if raw == INVALID_HANDLE_VALUE {
        return None;
    }
    // SAFETY: `raw` is a snapshot handle this call owns from here on.
    let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut entry = PROCESSENTRY32W {
        dwSize: u32::try_from(size_of::<PROCESSENTRY32W>()).ok()?,
        ..PROCESSENTRY32W::default()
    };
    let mut processes = Vec::new();
    // SAFETY: the handle is an open snapshot and `entry` a writable `PROCESSENTRY32W` with its size set.
    let mut more = unsafe { Process32FirstW(handle.as_raw_handle(), &raw mut entry) } != 0;
    while more {
        processes.push(Snapshotted {
            pid: entry.th32ProcessID,
            parent: entry.th32ParentProcessID,
        });
        // SAFETY: as above.
        more = unsafe { Process32NextW(handle.as_raw_handle(), &raw mut entry) } != 0;
    }
    Some(processes)
}

/// `pid` opened with `access`; `None` when it is gone or may not be opened so.
pub(crate) fn open(pid: u32, access: PROCESS_ACCESS_RIGHTS) -> Option<OwnedHandle> {
    if pid == 0 {
        return None;
    }
    // SAFETY: no pointer arguments; the result is checked before use.
    let raw = unsafe { OpenProcess(access, 0, pid) };
    if raw.is_null() {
        return None;
    }
    // SAFETY: `raw` is an open process handle this call owns from here on.
    Some(unsafe { OwnedHandle::from_raw_handle(raw) })
}

/// When `process` was created, as a `FILETIME` (100 ns since 1601): what tells it from a later process with its pid.
pub(crate) fn creation_time(process: &OwnedHandle) -> Option<u64> {
    let mut times = [FILETIME::default(); 4];
    let [created, exited, kernel, user] = &mut times;
    // SAFETY: the handle is an open process handle with query access, and the four `FILETIME`s are writable.
    let read = unsafe { GetProcessTimes(process.as_raw_handle(), created, exited, kernel, user) };
    (read != 0).then(|| (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_snapshot_holds_this_process_and_its_parent() {
        let pid = std::process::id();
        let processes = snapshot().unwrap();
        let own = processes.iter().find(|process| process.pid == pid).unwrap();
        assert!(processes.iter().any(|process| process.pid == own.parent));
        let entry = entry(*own).unwrap();
        assert_eq!(
            entry.record.group, entry.record.pid,
            "a process stands in for its own group"
        );
        assert_eq!(
            u32::try_from(entry.parent).unwrap(),
            own.parent,
            "the handle names the parent the snapshot does"
        );
        assert_eq!(open(0, PROCESS_QUERY_LIMITED_INFORMATION).map(|_| ()), None);
    }

    fn record_of(child: &std::process::Child) -> ProcessRecord {
        let pid = i32::try_from(child.id()).unwrap();
        read_table()
            .unwrap()
            .into_iter()
            .find(|entry| entry.record.pid == pid)
            .unwrap()
            .record
    }

    #[test]
    fn a_terminated_child_is_gone() {
        let mut child = std::process::Command::new("ping.exe")
            .args(["-n", "600", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        signal(Signal::HangUp, &record_of(&child));
        assert_eq!(child.wait().unwrap().code(), Some(i32::try_from(TERMINATED).unwrap()));
    }

    #[test]
    fn a_pid_now_held_by_another_process_is_left_alone() {
        let mut child = std::process::Command::new("ping.exe")
            .args(["-n", "600", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let record = record_of(&child);
        // the record of an earlier process that held this pid
        signal(
            Signal::Kill,
            &ProcessRecord {
                started: record.started - 1,
                ..record
            },
        );
        assert_eq!(child.try_wait().unwrap(), None, "still running");
        signal(Signal::Kill, &record);
        assert_eq!(child.wait().unwrap().code(), Some(i32::try_from(TERMINATED).unwrap()));
    }
}
