//! The macOS process table, upstream's reads in `Ghostty/ForegroundProcess.swift`: `sysctl(KERN_PROCARGS2)` for an
//! argv (`procArgs`; refused for another user's process, such as setuid `login` or `sudo`), the members of a process
//! group (`processGroup`), `proc_pidinfo(PROC_PIDVNODEPATHINFO)` for the working directory (`workingDirectory`). The
//! ids come from `libproc`'s BSD info ([`crate::job::macos`]), as upstream's `kinfo_proc` reads carry them, and the
//! session from `getsid`.

use std::ffi::c_void;
use std::mem::size_of;

use agterm_host::{ProcessStat, ProcessTable};

use super::parse_procargs;
use crate::job::macos::{PROC_PGRP_ONLY, bsdinfo, list_pids};

/// The live macOS process table.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcTable;

impl ProcTable {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl ProcessTable for ProcTable {
    fn argv(&self, pid: i32) -> Option<Vec<String>> {
        parse_procargs(&procargs(pid)?)
    }

    fn stat(&self, pid: i32) -> Option<ProcessStat> {
        stat_of(pid)
    }

    fn group(&self, pgrp: i32) -> Vec<ProcessStat> {
        let Some(pids) = u32::try_from(pgrp)
            .ok()
            .and_then(|group| list_pids(PROC_PGRP_ONLY, group))
        else {
            return Vec::new();
        };
        let mut members: Vec<ProcessStat> = pids
            .into_iter()
            .filter_map(stat_of)
            .filter(|stat| stat.pgrp == pgrp)
            .collect();
        members.sort_unstable_by_key(|stat| stat.pid);
        members
    }

    /// The BSD info's `e_tpgid`; `None` for a process without a terminal.
    fn terminal_foreground(&self, pid: i32) -> Option<i32> {
        i32::try_from(bsdinfo(pid)?.e_tpgid).ok().filter(|group| *group > 0)
    }

    fn working_directory(&self, pid: i32) -> Option<String> {
        // SAFETY: `proc_vnodepathinfo` is plain old data, for which all zeroes is a valid value.
        let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
        let size = i32::try_from(size_of::<libc::proc_vnodepathinfo>()).ok()?;
        let buffer = (&raw mut info).cast::<c_void>();
        // SAFETY: the buffer is a writable `proc_vnodepathinfo` of exactly `size` bytes.
        let read = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDVNODEPATHINFO, 0, buffer, size) };
        if read != size {
            return None;
        }
        // the path is a NUL-terminated `char[MAXPATHLEN]`, which libc declares as 32 rows of 32
        let bytes: Vec<u8> = info
            .pvi_cdir
            .vip_path
            .iter()
            .flatten()
            .map(|byte| byte.to_ne_bytes()[0])
            .take_while(|byte| *byte != 0)
            .collect();
        String::from_utf8(bytes).ok().filter(|path| !path.is_empty())
    }
}

/// The ids of `pid`; `None` when it is gone or cannot be read.
fn stat_of(pid: i32) -> Option<ProcessStat> {
    let info = bsdinfo(pid)?;
    // SAFETY: `getsid` has no memory preconditions.
    let session = unsafe { libc::getsid(pid) };
    if session < 0 {
        return None;
    }
    Some(ProcessStat {
        pid,
        ppid: i32::try_from(info.pbi_ppid).ok()?,
        pgrp: i32::try_from(info.pbi_pgid).ok()?,
        session,
    })
}

/// The raw `KERN_PROCARGS2` blob of `pid`; `None` on any syscall failure, which a process of another user gives.
fn procargs(pid: i32) -> Option<Vec<u8>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size = 0_usize;
    let count = u32::try_from(mib.len()).ok()?;
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
    if sized != 0 || size == 0 {
        return None;
    }
    let mut buffer = vec![0_u8; size];
    // SAFETY: as above, with `buffer` writable for `size` bytes.
    let read = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            count,
            buffer.as_mut_ptr().cast(),
            &raw mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if read != 0 {
        return None;
    }
    buffer.truncate(size);
    Some(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_live_table_reads_this_test_process() {
        let table = ProcTable::new();
        let pid = i32::try_from(std::process::id()).unwrap();
        let own = table.stat(pid).unwrap();
        // SAFETY: `getpgrp` has no preconditions.
        let group = unsafe { libc::getpgrp() };
        // SAFETY: `getsid` has no memory preconditions.
        let session = unsafe { libc::getsid(0) };
        assert_eq!((own.pid, own.pgrp, own.session), (pid, group, session));
        let argv = table.argv(pid).unwrap();
        let exe = std::env::current_exe().unwrap();
        assert_eq!(std::path::Path::new(&argv[0]).file_name(), exe.file_name(), "{argv:?}");
        assert!(table.group(own.pgrp).contains(&own));
        let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
        let read = table
            .working_directory(pid)
            .map(|path| std::path::PathBuf::from(path).canonicalize().unwrap());
        assert_eq!(read, Some(cwd));
        assert_eq!(table.stat(-1), None);
        assert_eq!(table.argv(-1), None);
    }
}
