//! The job table on macOS, upstream's read (`ProcessTable.read` in `agterm/Ghostty/ProcessTable.swift`):
//! `sysctl(KERN_PROC_ALL)`'s `kinfo_proc` for every process (its start time, process group, parent and the
//! controlling terminal's foreground group), and `kill(2)`. Unlike `libproc`'s `PROC_PIDTBSDINFO`, which answers only
//! for a process of the caller's effective user, `KERN_PROC` lists every process whoever runs it, so a `sudo` job
//! is in the table. The start time is upstream's: microseconds since the epoch.
//!
//! The `libc` crate defines no `kinfo_proc` for Apple, so [`KinfoProc`] declares it from xnu's `bsd/sys/sysctl.h`
//! (`struct kinfo_proc`, `struct eproc`, `struct _pcred`, `struct _ucred`), `bsd/sys/proc.h` (`struct extern_proc`)
//! and `bsd/sys/vm.h` (the user-space `struct vmspace`), with its size and the offsets read pinned at compile time.

use std::mem::{offset_of, size_of};

use agterm_host::ProcessRecord;

use super::{ProcessEntry, Signal};

pub(super) fn read_table() -> Option<Vec<ProcessEntry>> {
    // every table holds this process at least, so an empty one is a failed read
    let processes = kinfo(&[libc::KERN_PROC_ALL]).filter(|processes| !processes.is_empty())?;
    Some(processes.iter().map(KinfoProc::entry).collect())
}

pub(super) fn session_members(session: i32) -> Option<Vec<ProcessRecord>> {
    // `kinfo_proc` holds the session only as a scrubbed kernel pointer, so each listed process is asked (herdr's
    // `session_processes`); one gone meanwhile answers -1 and is passed over
    let processes = kinfo(&[libc::KERN_PROC_ALL]).filter(|processes| !processes.is_empty())?;
    Some(
        processes
            .iter()
            // SAFETY: `getsid` has no memory preconditions.
            .filter(|process| unsafe { libc::getsid(process.pid()) } == session)
            .map(|process| process.entry().record)
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

/// Every process `sysctl(CTL_KERN, KERN_PROC, <selector>)` lists (`KERN_PROC_ALL`, `KERN_PROC_PGRP` with the group,
/// `KERN_PROC_PID` with the pid); empty when none matches, `None` when the call fails. The list can grow between the
/// sizing call and the read, which answers `ENOMEM` (with nothing salvageable), so the read is retried with a wider
/// margin, as upstream's `ProcessTable.read` and `processGroup` do.
pub(crate) fn kinfo(selector: &[libc::c_int]) -> Option<Vec<KinfoProc>> {
    let mut mib = vec![libc::CTL_KERN, libc::KERN_PROC];
    mib.extend_from_slice(selector);
    let count = u32::try_from(mib.len()).ok()?;
    for margin in [16, 256, 4096] {
        let mut size = 0_usize;
        // SAFETY: `mib` holds `count` names, a null buffer only asks for the size, and `size` is writable.
        let sized = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                count,
                std::ptr::null_mut(),
                &raw mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if sized != 0 {
            return None;
        }
        if size == 0 {
            return Some(Vec::new());
        }
        let mut processes = vec![KinfoProc::default(); size / size_of::<KinfoProc>() + margin];
        size = processes.len() * size_of::<KinfoProc>();
        // SAFETY: as above, with `processes` writable for `size` bytes of `kinfo_proc`.
        let read = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                count,
                processes.as_mut_ptr().cast(),
                &raw mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if read == 0 {
            processes.truncate(size / size_of::<KinfoProc>());
            return Some(processes);
        }
        if std::io::Error::last_os_error().raw_os_error() != Some(libc::ENOMEM) {
            return None;
        }
    }
    None
}

/// xnu's `struct kinfo_proc`: a process's `extern_proc` and `eproc`. Kernel pointers are plain words here (the
/// kernel scrubs them), and the fields nothing reads are underscored.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct KinfoProc {
    kp_proc: ExternProc,
    kp_eproc: Eproc,
}

impl KinfoProc {
    pub(crate) fn pid(&self) -> i32 {
        self.kp_proc.p_pid
    }

    pub(crate) fn parent(&self) -> i32 {
        self.kp_eproc.e_ppid
    }

    pub(crate) fn group(&self) -> i32 {
        self.kp_eproc.e_pgid
    }

    /// The controlling terminal's foreground group; zero for a process without a controlling terminal.
    pub(crate) fn terminal_foreground(&self) -> i32 {
        self.kp_eproc.e_tpgid
    }

    /// Microseconds since the epoch, upstream's `started`.
    fn started(&self) -> u64 {
        let started = self.kp_proc.p_starttime;
        let seconds = u64::try_from(started.tv_sec).unwrap_or(0);
        let micros = u64::try_from(started.tv_usec).unwrap_or(0);
        seconds.saturating_mul(1_000_000).saturating_add(micros)
    }

    fn entry(&self) -> ProcessEntry {
        ProcessEntry {
            record: ProcessRecord {
                pid: self.pid(),
                started: self.started(),
                group: self.group(),
                foreground: self.terminal_foreground(),
            },
            parent: self.parent(),
        }
    }
}

/// `struct timeval` with a `Default` (`libc::timeval` has none).
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Timeval {
    tv_sec: i64,
    tv_usec: i32,
}

/// `struct extern_proc`; `p_un` is read as its `__p_starttime` arm.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ExternProc {
    p_starttime: Timeval,
    _p_vmspace: usize,
    _p_sigacts: usize,
    _p_flag: i32,
    _p_stat: i8,
    p_pid: i32,
    _p_oppid: i32,
    _p_dupfd: i32,
    _user_stack: usize,
    _exit_thread: usize,
    _p_debugger: i32,
    _sigwait: i32,
    _p_estcpu: u32,
    _p_cpticks: i32,
    _p_pctcpu: u32,
    _p_wchan: usize,
    _p_wmesg: usize,
    _p_swtime: u32,
    _p_slptime: u32,
    /// `struct itimerval`: two `timeval`s.
    _p_realtimer: [Timeval; 2],
    _p_rtime: Timeval,
    _p_uticks: u64,
    _p_sticks: u64,
    _p_iticks: u64,
    _p_traceflag: i32,
    _p_tracep: usize,
    _p_siglist: i32,
    _p_textvp: usize,
    _p_holdcnt: i32,
    _p_sigmask: u32,
    _p_sigignore: u32,
    _p_sigcatch: u32,
    _p_priority: u8,
    _p_usrpri: u8,
    _p_nice: i8,
    _p_comm: [u8; 17],
    _p_pgrp: usize,
    _p_addr: usize,
    _p_xstat: u16,
    _p_acflag: u16,
    _p_ru: usize,
}

/// `struct eproc`, with `struct _pcred`, `struct _ucred` and `struct vmspace` as opaque spans of their size and
/// alignment.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Eproc {
    _e_paddr: usize,
    _e_sess: usize,
    /// `struct _pcred`: `char pc_lock[72]`, a pointer, five 32-bit ids and counts, padded to 104 bytes.
    _e_pcred: [u64; 13],
    /// `struct _ucred`: `cr_ref`, `cr_uid`, `cr_ngroups` and `cr_groups[16]`, 76 bytes.
    _e_ucred: [u32; 19],
    /// `struct vmspace`: 64 bytes of dummies.
    _e_vm: [u64; 8],
    e_ppid: i32,
    e_pgid: i32,
    _e_jobc: i16,
    _e_tdev: i32,
    e_tpgid: i32,
    _e_tsess: usize,
    _e_wmesg: [u8; 8],
    _e_xsize: i32,
    _e_xrssize: i16,
    _e_xccount: i16,
    _e_xswrss: i16,
    _e_flag: i32,
    _e_login: [u8; 12],
    _e_spare: [i32; 4],
}

// the layout of the 64-bit kernels macOS runs (x86_64 and arm64 alike)
const _: () = {
    assert!(size_of::<KinfoProc>() == 648);
    assert!(size_of::<ExternProc>() == 296);
    assert!(offset_of!(ExternProc, p_pid) == 40);
    assert!(offset_of!(ExternProc, _p_comm) == 243);
    assert!(offset_of!(Eproc, e_ppid) == 264);
    assert!(offset_of!(Eproc, e_pgid) == 268);
    assert!(offset_of!(Eproc, e_tpgid) == 280);
    assert!(offset_of!(Eproc, _e_spare) == 332);
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_reads_its_own_group_and_start_time() {
        let pid = i32::try_from(std::process::id()).unwrap();
        let [own] = kinfo(&[libc::KERN_PROC_PID, pid]).unwrap()[..] else {
            panic!("one kinfo_proc for one pid");
        };
        let own = own.entry();
        // SAFETY: `getpgrp` and `getppid` have no preconditions.
        let group = unsafe { libc::getpgrp() };
        // SAFETY: as above.
        let parent = unsafe { libc::getppid() };
        assert_eq!((own.record.pid, own.record.group, own.parent), (pid, group, parent));
        assert!(
            own.record.started > 1_000_000_000 * 1_000_000,
            "microseconds since the epoch"
        );
        let members = kinfo(&[libc::KERN_PROC_PGRP, group]).unwrap();
        assert!(members.iter().any(|member| member.pid() == pid));
        assert!(read_table().unwrap().contains(&own));
        assert!(kinfo(&[libc::KERN_PROC_PID, -1]).unwrap().is_empty(), "no such process");
    }

    #[test]
    fn this_process_is_a_member_of_its_session() {
        // SAFETY: `getsid` has no memory preconditions.
        let session = unsafe { libc::getsid(0) };
        let pid = i32::try_from(std::process::id()).unwrap();
        assert!(session_members(session).unwrap().iter().any(|record| record.pid == pid));
    }

    #[test]
    fn another_users_process_is_in_the_table() {
        // launchd runs as root: `PROC_PIDTBSDINFO` refuses it to a non-root caller, `KERN_PROC` does not
        let table = read_table().unwrap();
        let launchd = table.iter().find(|entry| entry.record.pid == 1).unwrap();
        assert!(launchd.record.started > 0, "{launchd:?}");
    }
}
