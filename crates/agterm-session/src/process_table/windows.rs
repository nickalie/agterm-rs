//! The Windows process table: an argv from the process's command line, the parent from a tool-help snapshot, the
//! console's stand-in foreground ([`super::console_foreground`]) and the working directory from the process
//! environment block.
//!
//! herdr: `src/platform/windows.rs` (`read_process_command_line`, `command_line_to_argv`, `process_cwd`,
//! `process_cwd_descriptor`, `read_process_parameters`, `read_process_value`, `read_unicode_string`, the `Peb`,
//! `Peb32`, `ProcessCwdParameters32` and `RtlUserProcessParameters` prefixes) at `HERDR_REV` (`build/pins.env`),
//! licensed under the Apache License 2.0, modified: the command line goes to `CommandLineToArgvW` as the UTF-16 it
//! is, never through a `String`, and each argument is decoded lossily (an unpaired surrogate becomes U+FFFD), so an
//! argv is never dropped for one bad unit; the string's offset comes from its header rather than assumed; the
//! structure prefixes name only the fields read; the working directory loses the trailing separator Windows keeps on
//! it (but a root's).

use std::ffi::c_void;
use std::mem::{MaybeUninit, size_of};
use std::os::windows::io::{AsRawHandle as _, OwnedHandle};
use std::ptr::null_mut;

use agterm_host::{ProcessStat, ProcessTable};
use windows_sys::Wdk::System::Threading::{
    NtQueryInformationProcess, ProcessBasicInformation, ProcessCommandLineInformation,
};
use windows_sys::Win32::Foundation::{
    HANDLE, LocalFree, NTSTATUS, STATUS_BUFFER_OVERFLOW, STATUS_BUFFER_TOO_SMALL, STATUS_INFO_LENGTH_MISMATCH,
    UNICODE_STRING,
};
use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows_sys::Win32::System::Threading::{
    PROCESS_BASIC_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
};
use windows_sys::Win32::UI::Shell::CommandLineToArgvW;

use super::console_foreground;
use crate::job;
use crate::job::windows::{open, snapshot};

/// The live Windows process table.
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
        let process = open(u32::try_from(pid).ok()?, PROCESS_QUERY_LIMITED_INFORMATION)?;
        command_line_to_argv(&read_command_line(&process)?)
    }

    /// The process with its parent; it stands in for its own group and leads no session.
    fn stat(&self, pid: i32) -> Option<ProcessStat> {
        let wanted = u32::try_from(pid).ok()?;
        let process = snapshot()?.into_iter().find(|process| process.pid == wanted)?;
        Some(ProcessStat {
            pid,
            ppid: i32::try_from(process.parent).ok()?,
            pgrp: pid,
            session: 0,
        })
    }

    /// The one process that stands in for group `pgrp`, while it runs.
    fn group(&self, pgrp: i32) -> Vec<ProcessStat> {
        self.stat(pgrp).into_iter().collect()
    }

    fn terminal_foreground(&self, pid: i32) -> Option<i32> {
        console_foreground(pid, &job::read_table()?)
    }

    fn working_directory(&self, pid: i32) -> Option<String> {
        let process = open(
            u32::try_from(pid).ok()?,
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ,
        )?;
        let (length, address) = current_directory(process.as_raw_handle())?;
        let mut path = read_wide(process.as_raw_handle(), length, address)?;
        if path.len() > 3 && path.ends_with('\\') {
            path.pop();
        }
        Some(path)
    }
}

/// Whether a query's status says the buffer was too small for what the process holds (now).
fn too_small(status: NTSTATUS) -> bool {
    [
        STATUS_BUFFER_OVERFLOW,
        STATUS_BUFFER_TOO_SMALL,
        STATUS_INFO_LENGTH_MISMATCH,
    ]
    .contains(&status)
}

/// The command line of `process`, as UTF-16 units; `None` when it cannot be read or is empty. It needs only
/// `PROCESS_QUERY_LIMITED_INFORMATION`, which hardened runtimes grant where they deny reading their memory.
fn read_command_line(process: &OwnedHandle) -> Option<Vec<u16>> {
    let handle = process.as_raw_handle();
    let mut required = 0_u32;
    // SAFETY: a null buffer of length zero only asks for the size, and `required` is writable.
    let status =
        unsafe { NtQueryInformationProcess(handle, ProcessCommandLineInformation, null_mut(), 0, &raw mut required) };
    if !too_small(status) {
        return None;
    }
    let mut buffer = vec![0_u8; usize::try_from(required).ok()?];
    let mut filled = false;
    // the command line may grow between the sizing call and the read: once more with the size then reported
    for _ in 0..2 {
        let length = u32::try_from(buffer.len()).ok()?;
        let target = buffer.as_mut_ptr().cast::<c_void>();
        // SAFETY: `buffer` is writable for `length` bytes, and `required` is writable.
        let status = unsafe {
            NtQueryInformationProcess(handle, ProcessCommandLineInformation, target, length, &raw mut required)
        };
        if too_small(status) {
            buffer = vec![0_u8; usize::try_from(required).ok()?];
            continue;
        }
        filled = status >= 0;
        break;
    }
    if !filled || buffer.len() < size_of::<UNICODE_STRING>() {
        return None;
    }
    // SAFETY: the kernel wrote a `UNICODE_STRING` at the start of `buffer`, which holds at least its size; a byte
    // buffer is not aligned for it, hence the unaligned read.
    let header = unsafe { buffer.as_ptr().cast::<UNICODE_STRING>().read_unaligned() };
    let length = usize::from(header.Length);
    let offset = header.Buffer.addr().checked_sub(buffer.as_ptr().addr())?;
    let units = buffer.get(offset..offset.checked_add(length)?)?;
    if units.is_empty() || !length.is_multiple_of(2) {
        return None;
    }
    Some(
        units
            .as_chunks::<2>()
            .0
            .iter()
            .map(|unit| u16::from_ne_bytes(*unit))
            .collect(),
    )
}

/// Frees what `CommandLineToArgvW` allocated.
struct LocalAllocation(*mut *mut u16);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // SAFETY: the pointer is the allocation `CommandLineToArgvW` returned, freed only here.
        unsafe { LocalFree(self.0.cast()) };
    }
}

/// `command_line` split as the C runtime splits it (`CommandLineToArgvW`), each argument decoded lossily; `None`
/// when it does not split.
fn command_line_to_argv(command_line: &[u16]) -> Option<Vec<String>> {
    let wide: Vec<u16> = command_line.iter().copied().chain(std::iter::once(0)).collect();
    let mut split = 0_i32;
    // SAFETY: `wide` is NUL-terminated, and `split` is writable.
    let pointers = unsafe { CommandLineToArgvW(wide.as_ptr(), &raw mut split) };
    if pointers.is_null() {
        return None;
    }
    let allocation = LocalAllocation(pointers);
    let count = usize::try_from(split).ok().filter(|count| *count > 0)?;
    // SAFETY: `CommandLineToArgvW` returned an array of `count` pointers, alive until `allocation` drops.
    let strings = unsafe { std::slice::from_raw_parts(allocation.0, count) };
    Some(
        strings
            .iter()
            .map(|arg| {
                // SAFETY: each pointer is a NUL-terminated string inside the same allocation.
                String::from_utf16_lossy(unsafe { nul_terminated(*arg) })
            })
            .collect(),
    )
}

/// The units of a NUL-terminated UTF-16 string, without the NUL.
///
/// # Safety
///
/// `string` points to a NUL-terminated UTF-16 string that outlives the result.
unsafe fn nul_terminated<'a>(string: *const u16) -> &'a [u16] {
    let mut length = 0;
    // SAFETY: the string is NUL-terminated, so every unit up to the NUL is readable.
    while unsafe { string.wrapping_add(length).read() } != 0 {
        length += 1;
    }
    // SAFETY: the `length` units before the NUL were just read.
    unsafe { std::slice::from_raw_parts(string, length) }
}

/// The prefix of a native process environment block, through its process parameters: four pointer-sized slots
/// (flags and padding, the mutant, the image base, the loader data), then the parameters' address.
#[repr(C)]
#[derive(Clone, Copy)]
struct Peb {
    _reserved: [*mut c_void; 4],
    process_parameters: *mut c_void,
}

/// The prefix of a native `RTL_USER_PROCESS_PARAMETERS`, through the current directory's DOS path.
#[repr(C)]
#[derive(Clone, Copy)]
struct ProcessParameters {
    _lengths_and_flags: [u32; 4],
    _console_handle: HANDLE,
    _console_flags: u32,
    _standard_handles: [HANDLE; 3],
    current_directory: UNICODE_STRING,
}

/// The current directory of `process` (its length in bytes and its address in the process); `None` when it cannot
/// be read.
fn current_directory(process: HANDLE) -> Option<(u16, *const c_void)> {
    #[cfg(target_pointer_width = "64")]
    {
        let peb32 = wow64::peb_address(process)?;
        if peb32 != 0 {
            return wow64::current_directory(process, peb32);
        }
    }
    let mut basic = PROCESS_BASIC_INFORMATION::default();
    let size = u32::try_from(size_of::<PROCESS_BASIC_INFORMATION>()).ok()?;
    // SAFETY: `basic` is a writable `PROCESS_BASIC_INFORMATION` of exactly `size` bytes.
    let status = unsafe {
        NtQueryInformationProcess(
            process,
            ProcessBasicInformation,
            (&raw mut basic).cast(),
            size,
            null_mut(),
        )
    };
    if status < 0 {
        return None;
    }
    let peb = read_remote::<Peb>(process, basic.PebBaseAddress.cast_const().cast())?;
    let parameters = read_remote::<ProcessParameters>(process, peb.process_parameters.cast_const())?;
    let directory = parameters.current_directory;
    Some((directory.Length, directory.Buffer.cast_const().cast()))
}

/// A 32-bit process under `WoW64` keeps its current directory in its own 32-bit environment block (the native one may
/// still say `C:\Windows`).
#[cfg(target_pointer_width = "64")]
mod wow64 {
    use std::ffi::c_void;
    use std::mem::size_of;
    use std::ptr::null_mut;

    use windows_sys::Wdk::System::Threading::{NtQueryInformationProcess, ProcessWow64Information};
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::Kernel::STRING32;

    use super::read_remote;

    /// The prefix of a 32-bit process environment block, through its process parameters.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Peb32 {
        _reserved: [u32; 4],
        process_parameters: u32,
    }

    /// The prefix of a 32-bit `RTL_USER_PROCESS_PARAMETERS`, through the current directory's DOS path.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct ProcessParameters32 {
        _reserved: [u32; 9],
        current_directory: STRING32,
    }

    /// The address of the 32-bit environment block of `process`, zero for a native process; `None` when the query
    /// fails.
    pub(super) fn peb_address(process: HANDLE) -> Option<usize> {
        let mut peb32 = 0_usize;
        let size = u32::try_from(size_of::<usize>()).ok()?;
        // SAFETY: `peb32` is a writable `ULONG_PTR` of exactly `size` bytes.
        let status = unsafe {
            NtQueryInformationProcess(
                process,
                ProcessWow64Information,
                (&raw mut peb32).cast(),
                size,
                null_mut(),
            )
        };
        (status >= 0).then_some(peb32)
    }

    /// The current directory of the 32-bit `process` whose environment block is at `peb32`.
    pub(super) fn current_directory(process: HANDLE, peb32: usize) -> Option<(u16, *const c_void)> {
        let peb = read_remote::<Peb32>(process, std::ptr::without_provenance(peb32))?;
        let address = usize::try_from(peb.process_parameters).ok()?;
        let parameters = read_remote::<ProcessParameters32>(process, std::ptr::without_provenance(address))?;
        let directory = parameters.current_directory;
        let buffer = usize::try_from(directory.Buffer).ok()?;
        Some((directory.Length, std::ptr::without_provenance(buffer)))
    }
}

/// A `T` read from `address` in `process`; `None` when the read fails or comes up short.
fn read_remote<T: Copy>(process: HANDLE, address: *const c_void) -> Option<T> {
    if address.is_null() {
        return None;
    }
    let mut value = MaybeUninit::<T>::uninit();
    let mut read = 0_usize;
    // SAFETY: `value` is writable for `size_of::<T>()` bytes, and `read` is writable.
    let ok = unsafe {
        ReadProcessMemory(
            process,
            address,
            value.as_mut_ptr().cast(),
            size_of::<T>(),
            &raw mut read,
        )
    };
    if ok == 0 || read != size_of::<T>() {
        return None;
    }
    // SAFETY: all of `value`'s bytes were just written, and the structures read here are plain old data.
    Some(unsafe { value.assume_init() })
}

/// The UTF-16 string of `length` bytes at `address` in `process`; `None` when it cannot be read or is not valid
/// UTF-16 (a path with an unpaired surrogate names no directory a `String` could).
fn read_wide(process: HANDLE, length: u16, address: *const c_void) -> Option<String> {
    if address.is_null() || length == 0 || !length.is_multiple_of(2) {
        return None;
    }
    let mut units = vec![0_u16; usize::from(length / 2)];
    let mut read = 0_usize;
    // SAFETY: `units` is writable for `length` bytes, and `read` is writable.
    let ok = unsafe {
        ReadProcessMemory(
            process,
            address,
            units.as_mut_ptr().cast(),
            usize::from(length),
            &raw mut read,
        )
    };
    if ok == 0 || read != usize::from(length) {
        return None;
    }
    String::from_utf16(&units).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    #[test]
    fn a_command_line_splits_as_the_c_runtime_splits_it() {
        assert_eq!(
            command_line_to_argv(&wide(r#""C:\Program Files\x.exe" "a b" it"s "q\"uoted" tail\\"#)),
            Some(vec![
                r"C:\Program Files\x.exe".to_owned(),
                "a b".to_owned(),
                "its".to_owned(),
                "q\"uoted".to_owned(),
                r"tail\\".to_owned(),
            ])
        );
    }

    #[test]
    fn an_unpaired_surrogate_becomes_the_replacement_character() {
        let mut line = wide("grep caf");
        line.push(0xd800);
        assert_eq!(
            command_line_to_argv(&line),
            Some(vec!["grep".to_owned(), "caf\u{fffd}".to_owned()])
        );
    }

    #[test]
    fn the_live_table_reads_this_test_process() {
        let table = ProcTable::new();
        let pid = i32::try_from(std::process::id()).unwrap();
        let own = table.stat(pid).unwrap();
        assert_eq!((own.pid, own.pgrp, own.session), (pid, pid, 0));
        assert_eq!(table.group(pid), [own]);
        let argv = table.argv(pid).unwrap();
        let exe = std::env::current_exe().unwrap();
        assert_eq!(std::path::Path::new(&argv[0]).file_name(), exe.file_name(), "{argv:?}");
        let cwd = std::env::current_dir().unwrap();
        let read = table.working_directory(pid).unwrap();
        assert!(
            read.eq_ignore_ascii_case(cwd.to_str().unwrap().trim_end_matches('\\')),
            "{read} {cwd:?}"
        );
        assert_eq!(table.stat(-1), None);
        assert_eq!(table.argv(0), None);
    }
}
