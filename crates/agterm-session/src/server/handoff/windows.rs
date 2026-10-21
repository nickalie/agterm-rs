//! The update handoff on Windows, per Task 2's conditional GO (`docs/spikes/20261018-session-server.md`, Windows:
//! `ConPTY` handoff): the pseudoconsoles pass by their handles, duplicated straight into the replacement's process,
//! behind a layout gate.
//!
//! # The layout gate
//!
//! A pseudoconsole is handed over through the three handles its HPCON points at (`{hSignal, hPtyReference,
//! hConPtyProcess}`), a layout Windows does not document. Microsoft's own source of the inbox `ConPTY` declares that
//! struct an ABI the rest of the OS shares (`src/winconpty/winconpty.h` of microsoft/terminal at
//! `c62bb193d326ef2d3828f382d61ead064222b896`: "This structure is part of an ABI shared with the rest of the operating
//! system"), and the Task 2 probe proved the handoff on the branches below. So the gate ([`gate`]) admits only:
//!
//! - the implementation: the system `ConPTY` that `kernel32.dll`'s exports reach (`kernelbase.dll` or `kernel32.dll` in
//!   the system directory, which the address of `CreatePseudoConsole` must lie in): the only one the vendored loader
//!   takes (`build/patches/portable-pty`);
//! - the architecture: `x86_64`, the one probed;
//! - the Windows build, read from that module's version: one of [`SupportMatrix::PROVEN`] (`20348`, Windows Server
//!   2022; `26100`, Windows 11 24H2 and Windows Server 2025).
//!
//! Anything else refuses the handoff before any HPCON is read; the implementation's version is named in the refusal
//! and in the handoff's log, so a host the gate does not know says exactly what it runs. After the read, the handles
//! are checked (open, the signal a pipe, the console host a live `conhost.exe`), by the old server before it passes
//! them and by the replacement before it rebuilds the pseudoconsole from them. The replacement does not drive the
//! rebuilt pseudoconsole before it reports `restored`: every `ResizePseudoConsole`, to the size it has too, makes
//! conhost repaint its viewport from its own buffer, over what `print` wrote into the session's terminal alone
//! (`server::pane::handoff`), so the first real resize is the first to reach conhost through it.
//!
//! # The handles
//!
//! Per pane: the console's input write end and output read end, the three console handles, the program's process and
//! the job of its tree (`KILL_ON_JOB_CLOSE`: the replacement's copy keeps the tree alive once the old server exits),
//! each duplicated into the replacement's process through the handle its start returned, so the handoff is bound to
//! that process; their values travel as one JSON line. The replacement owns them from then on; if it is ended in a
//! rollback, they close with it, and the old server's own handles never change. The endpoint is not passed: the
//! replacement creates further instances of the namespace's pipe beside the old server's ([`Endpoint::bind_beside`]).

use std::io;
use std::os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agterm_pty::{Child, ConPtyHandoff, Master, Size};
use serde::{Deserialize, Serialize};
use windows_sys::Win32::Foundation::{
    DUPLICATE_SAME_ACCESS, DuplicateHandle, GetHandleInformation, HANDLE, MAX_PATH, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_TYPE_PIPE, GetFileType, GetFileVersionInfoSizeW, GetFileVersionInfoW, VS_FIXEDFILEINFO, VerQueryValueW,
};
use windows_sys::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, GetModuleFileNameW,
    GetModuleHandleExW, GetModuleHandleW, GetProcAddress,
};
use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetProcessId, PROCESS_NAME_WIN32, QueryFullProcessImageNameW, WaitForSingleObject,
};

use super::{PaneState, Support, read_line, write_line};
use crate::endpoint::{Endpoint, Listener, Stream};
use crate::server::pane::Pane;
use crate::server::spawn::Spawned;

/// The `ConPTY` implementations the handoff was proven on, and the gate over them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupportMatrix;

impl SupportMatrix {
    /// The (architecture, Windows build) pairs the Task 2 probe proved the handoff on, with the system `ConPTY`.
    pub const PROVEN: [(&'static str, u32); 2] = [("x86_64", 20_348), ("x86_64", 26_100)];
}

/// The implementation of `ConPTY` this process would hand over: where `CreatePseudoConsole` lives and its version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Implementation {
    /// The module `CreatePseudoConsole`'s address lies in.
    pub module: PathBuf,
    /// The system directory.
    pub system: PathBuf,
    /// The module's file version: major, minor, build, revision.
    pub version: [u16; 4],
}

/// The layout gate (module docs) for a host of architecture `arch` whose `ConPTY` is `implementation`.
///
/// # Errors
///
/// Why the handoff is refused, naming the implementation.
pub fn gate(arch: &str, implementation: &Implementation) -> Result<(), String> {
    let [major, minor, build, revision] = implementation.version;
    let named = format!(
        "{} {major}.{minor}.{build}.{revision} on {arch}",
        implementation.module.display()
    );
    let file = implementation
        .module
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase());
    let system_module = matches!(file.as_deref(), Some("kernelbase.dll" | "kernel32.dll"))
        && implementation
            .module
            .parent()
            .is_some_and(|parent| same_path(parent, &implementation.system));
    if !system_module {
        return Err(format!(
            "the update handoff takes over only the system ConPTY, and this process's is {named}"
        ));
    }
    if !SupportMatrix::PROVEN.contains(&(arch, u32::from(build))) {
        return Err(format!(
            "the update handoff was not proven on the ConPTY of {named}: its pseudoconsole layout is not known here"
        ));
    }
    Ok(())
}

fn same_path(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
}

/// This host's support: the gate over this process's `ConPTY`.
pub(super) fn support() -> Support {
    match Implementation::of_this_process() {
        Ok(implementation) => match gate(std::env::consts::ARCH, &implementation) {
            Ok(()) => {
                tracing::debug!(?implementation, "the ConPTY layout gate passes");
                Support::Supported
            }
            Err(reason) => Support::Refused(reason),
        },
        Err(err) => Support::Refused(format!(
            "the update handoff cannot tell which ConPTY this process runs: {err}"
        )),
    }
}

impl Implementation {
    /// This process's.
    ///
    /// # Errors
    ///
    /// `kernel32.dll` exports no `CreatePseudoConsole`, or its module or version could not be read.
    pub fn of_this_process() -> io::Result<Self> {
        let kernel32 = wide("kernel32.dll");
        // SAFETY: a NUL-terminated wide name; kernel32 is mapped in every process, so the handle needs no reference.
        let module = unsafe { GetModuleHandleW(kernel32.as_ptr()) };
        if module.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a NUL-terminated name of an export of the module above.
        let address = unsafe { GetProcAddress(module, c"CreatePseudoConsole".as_ptr().cast()) }
            .ok_or_else(|| io::Error::other("kernel32.dll exports no CreatePseudoConsole"))?;
        let mut owner = std::ptr::null_mut();
        // SAFETY: looks the module up by an address inside it without taking a reference; `owner` outlives the call.
        let found = unsafe {
            GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                (address as *const ()).cast(),
                &raw mut owner,
            )
        };
        if found == 0 {
            return Err(io::Error::last_os_error());
        }
        let path = module_path(owner)?;
        Ok(Self {
            version: file_version(&path)?,
            module: path,
            system: system_directory()?,
        })
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn from_wide(buffer: &[u16]) -> PathBuf {
    use std::os::windows::ffi::OsStringExt as _;
    PathBuf::from(std::ffi::OsString::from_wide(buffer))
}

fn module_path(module: windows_sys::Win32::Foundation::HMODULE) -> io::Result<PathBuf> {
    let mut buffer = vec![0_u16; 32 * 1024];
    let capacity = u32::try_from(buffer.len()).map_err(io::Error::other)?;
    // SAFETY: the buffer holds `capacity` wide characters.
    let length = unsafe { GetModuleFileNameW(module, buffer.as_mut_ptr(), capacity) };
    if length == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(from_wide(&buffer[..usize::try_from(length).map_err(io::Error::other)?]))
}

fn system_directory() -> io::Result<PathBuf> {
    let mut buffer = vec![0_u16; usize::try_from(MAX_PATH).map_err(io::Error::other)? * 4];
    let capacity = u32::try_from(buffer.len()).map_err(io::Error::other)?;
    // SAFETY: the buffer holds `capacity` wide characters.
    let length = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), capacity) };
    if length == 0 || length >= capacity {
        return Err(io::Error::last_os_error());
    }
    Ok(from_wide(&buffer[..usize::try_from(length).map_err(io::Error::other)?]))
}

/// The file version (major, minor, build, revision) of the module at `path`.
fn file_version(path: &Path) -> io::Result<[u16; 4]> {
    let name = wide(&path.to_string_lossy());
    // SAFETY: a NUL-terminated wide path; the handle argument is unused.
    let size = unsafe { GetFileVersionInfoSizeW(name.as_ptr(), std::ptr::null_mut()) };
    if size == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut data = vec![0_u8; usize::try_from(size).map_err(io::Error::other)?];
    // SAFETY: the buffer holds `size` bytes.
    if unsafe { GetFileVersionInfoW(name.as_ptr(), 0, size, data.as_mut_ptr().cast()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut fixed: *mut std::ffi::c_void = std::ptr::null_mut();
    let mut length = 0_u32;
    let root = wide("\\");
    // SAFETY: queries the root block of the version data just read; the out-pointers outlive the call.
    let found = unsafe { VerQueryValueW(data.as_ptr().cast(), root.as_ptr(), &raw mut fixed, &raw mut length) };
    if found == 0 || fixed.is_null() || (length as usize) < size_of::<VS_FIXEDFILEINFO>() {
        return Err(io::Error::other("the module has no fixed file version"));
    }
    // SAFETY: VerQueryValueW pointed `fixed` at a VS_FIXEDFILEINFO inside `data`, which lives here; it may be
    // unaligned.
    let info = unsafe { fixed.cast::<VS_FIXEDFILEINFO>().read_unaligned() };
    let half = |word: u32, high: bool| u16::try_from(if high { word >> 16 } else { word & 0xffff }).unwrap_or(0);
    Ok([
        half(info.dwFileVersionMS, true),
        half(info.dwFileVersionMS, false),
        half(info.dwFileVersionLS, true),
        half(info.dwFileVersionLS, false),
    ])
}

/// The pid of the channel's peer.
pub(super) fn peer_pid(stream: &Stream) -> Option<u32> {
    use interprocess::local_socket::traits::StreamCommon as _;
    stream.peer_creds().ok()?.pid()
}

/// One pane's handles as their values in the replacement's process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneHandles {
    pub input: u64,
    pub output: u64,
    pub signal: u64,
    pub reference: u64,
    pub conhost: u64,
    pub process: u64,
    pub job: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct HandlesLine {
    panes: Vec<PaneHandles>,
}

/// What the importer receives: nothing for the endpoint (it binds beside), every pane's PTY.
pub(super) struct Handles {
    pub(super) endpoint: (),
    pub(super) ptys: Vec<(Master, Child)>,
}

/// Step 6 of the exporter: every pane's handles duplicated into the replacement, their values as one line.
pub(super) fn send_handles(
    stream: &Stream,
    _listener: &Listener,
    panes: &[Arc<Pane>],
    replacement: &Spawned,
) -> io::Result<()> {
    if let Support::Refused(reason) = support() {
        return Err(io::Error::other(reason));
    }
    let target = replacement
        .process_handle()
        .ok_or_else(|| io::Error::other("no handle on the replacement's process to pass the sessions into"))?;
    let mut line = HandlesLine { panes: Vec::new() };
    for pane in panes {
        // SAFETY: the layout gate passed for this process's ConPTY just above.
        let console = unsafe { pane.console() }?;
        let (process, job) = pane.origin();
        check_console(&console)?;
        line.panes.push(PaneHandles {
            input: duplicate_into(console.input.as_raw_handle(), target)?,
            output: duplicate_into(console.output.as_raw_handle(), target)?,
            signal: duplicate_into(console.console[0].as_raw_handle(), target)?,
            reference: duplicate_into(console.console[1].as_raw_handle(), target)?,
            conhost: duplicate_into(console.console[2].as_raw_handle(), target)?,
            process: duplicate_into(process.as_raw_handle(), target)?,
            job: duplicate_into(job.as_raw_handle(), target)?,
        });
        // the local duplicates close here
    }
    write_line(stream, &serde_json::to_string(&line).map_err(io::Error::other)?)
}

/// The checks after the read (module docs): the handles open, the signal a pipe, the console host a live
/// `conhost.exe`.
fn check_console(console: &ConPtyHandoff) -> io::Result<()> {
    let handles = [
        console.input.as_raw_handle(),
        console.output.as_raw_handle(),
        console.console[0].as_raw_handle(),
        console.console[1].as_raw_handle(),
        console.console[2].as_raw_handle(),
    ];
    for handle in handles {
        let mut flags = 0_u32;
        // SAFETY: queries a handle owned by `console`; `flags` outlives the call.
        if handle.is_null() || unsafe { GetHandleInformation(handle, &raw mut flags) } == 0 {
            return Err(io::Error::other("a pseudoconsole handle is not open"));
        }
    }
    // SAFETY: queries a handle owned by `console`.
    if unsafe { GetFileType(console.console[0].as_raw_handle()) } != FILE_TYPE_PIPE {
        return Err(io::Error::other("the pseudoconsole's signal handle is not a pipe"));
    }
    let conhost = console.console[2].as_raw_handle();
    // SAFETY: queries and polls a process handle owned by `console`.
    let alive = unsafe { GetProcessId(conhost) } != 0 && unsafe { WaitForSingleObject(conhost, 0) } == WAIT_TIMEOUT;
    if !alive {
        return Err(io::Error::other(
            "the pseudoconsole's console host is not a live process",
        ));
    }
    let image = process_image(conhost)?;
    let named = image
        .file_name()
        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("conhost.exe"));
    if !named {
        return Err(io::Error::other(format!(
            "the pseudoconsole's console host is {}, not conhost.exe",
            image.display()
        )));
    }
    Ok(())
}

fn process_image(process: RawHandle) -> io::Result<PathBuf> {
    let mut buffer = vec![0_u16; 32 * 1024];
    let mut length = u32::try_from(buffer.len()).map_err(io::Error::other)?;
    // SAFETY: the buffer holds `length` wide characters; `length` outlives the call.
    if unsafe { QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &raw mut length) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(from_wide(&buffer[..usize::try_from(length).map_err(io::Error::other)?]))
}

/// `handle` duplicated into the process `target`, with the same access; its value there.
fn duplicate_into(handle: RawHandle, target: BorrowedHandle<'_>) -> io::Result<u64> {
    let mut out: HANDLE = std::ptr::null_mut();
    // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no closing and is always valid.
    let current = unsafe { GetCurrentProcess() };
    // SAFETY: duplicates a handle of this process into `target`, whose handle (with PROCESS_DUP_HANDLE, from its
    // start) is open; `out` outlives the call and holds a value in `target`'s table, never used here.
    let duplicated = unsafe {
        DuplicateHandle(
            current,
            handle,
            target.as_raw_handle(),
            &raw mut out,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if duplicated == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(out as usize as u64)
}

/// Step 6 of the importer: the handles' line, each pane's PTY rebuilt from them at its size.
pub(super) fn receive_handles(stream: &Arc<Stream>, panes: &[PaneState], timeout: Duration) -> io::Result<Handles> {
    let line = read_line(stream, timeout).map_err(io::Error::other)?;
    let line: HandlesLine = serde_json::from_str(&line).map_err(io::Error::other)?;
    if line.panes.len() != panes.len() {
        return Err(io::Error::other(format!(
            "the handoff sent the handles of {} panes for {}",
            line.panes.len(),
            panes.len()
        )));
    }
    let ptys = line
        .panes
        .iter()
        .zip(panes)
        .map(|(handles, state)| rebuild(handles, state))
        .collect::<io::Result<_>>()?;
    Ok(Handles { endpoint: (), ptys })
}

/// A handle the exporter duplicated into this process, owned from here on.
fn take(value: u64) -> io::Result<OwnedHandle> {
    let raw = usize::try_from(value).map_err(io::Error::other)? as RawHandle;
    if raw.is_null() {
        return Err(io::Error::other("the handoff sent a null handle"));
    }
    // SAFETY: the exporter duplicated this handle into this process for it alone; nothing else here owns it.
    Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
}

fn rebuild(handles: &PaneHandles, state: &PaneState) -> io::Result<(Master, Child)> {
    let handoff = ConPtyHandoff {
        input: take(handles.input)?,
        output: take(handles.output)?,
        console: [take(handles.signal)?, take(handles.reference)?, take(handles.conhost)?],
    };
    let (process, job) = (take(handles.process)?, take(handles.job)?);
    check_console(&handoff)?;
    let child = Child::from_handoff(process, job)?;
    let master = Master::from_handoff(handoff, Size::cells(state.size.rows, state.size.cols))?;
    Ok((master, child))
}

/// The importer's endpoint: further instances of the pipe, beside the old server's.
pub(super) fn take_endpoint(endpoint: &Endpoint, (): ()) -> io::Result<Listener> {
    endpoint.bind_beside()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn implementation(module: &str, build: u16) -> Implementation {
        Implementation {
            module: PathBuf::from(module),
            system: PathBuf::from(r"C:\Windows\system32"),
            version: [10, 0, build, 1],
        }
    }

    #[test]
    fn the_gate_admits_the_proven_system_conpty_only() {
        assert_eq!(
            gate("x86_64", &implementation(r"C:\Windows\System32\KERNELBASE.dll", 26_100)),
            Ok(())
        );
        assert_eq!(
            gate("x86_64", &implementation(r"C:\Windows\System32\kernel32.dll", 20_348)),
            Ok(())
        );
        let unknown = gate("x86_64", &implementation(r"C:\Windows\System32\KERNELBASE.dll", 22_631)).unwrap_err();
        assert!(unknown.contains("10.0.22631.1 on x86_64"), "{unknown}");
        let arm = gate(
            "aarch64",
            &implementation(r"C:\Windows\System32\KERNELBASE.dll", 26_100),
        )
        .unwrap_err();
        assert!(arm.contains("on aarch64"), "{arm}");
        let other = gate("x86_64", &implementation(r"C:\app\conpty.dll", 26_100)).unwrap_err();
        assert!(other.contains("only the system ConPTY"), "{other}");
        let elsewhere = gate("x86_64", &implementation(r"C:\app\kernelbase.dll", 26_100)).unwrap_err();
        assert!(elsewhere.contains("only the system ConPTY"), "{elsewhere}");
    }

    #[test]
    fn this_process_names_its_conpty() {
        let implementation = Implementation::of_this_process().unwrap();
        assert!(implementation.module.is_absolute(), "{implementation:?}");
        // kernel32's file version reads 6.2 or 10.0 by the module, but its build is the system's
        assert!(implementation.version[2] >= 17_763, "{implementation:?}");
    }
}
