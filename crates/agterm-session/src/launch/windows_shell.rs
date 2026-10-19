//! The Windows login shell. Windows has no password database and no `$SHELL`, so the shell a pane runs is named here:
//! the first launchable `pwsh.exe` (PowerShell 7) on `PATH`, else the first launchable `powershell.exe` (Windows
//! PowerShell 5.1, inbox) on `PATH`, else the inbox one in the system directory (`System32\WindowsPowerShell\v1.0`),
//! each as a full path, so the pane runs exactly the binary that was validated. None of them is no shell: a plain
//! pane then runs `cmd.exe` (`agterm-term`), and live sessions are refused with the unsupported-shell reason.
//!
//! herdr: `src/pane.rs` `default_windows_pane_shell`, `is_windows_executable_file_for_host` and
//! `windows_executable_machine_is_compatible`, and `src/platform/windows.rs` `native_machine_type`, at `HERDR_REV`
//! (`build/pins.env`), licensed under the Apache License 2.0, modified: the image check reads the headers into a value
//! first and judges it apart; `powershell.exe` is validated and looked for like `pwsh.exe` (herdr names it bare); the
//! image rules are herdr's. A candidate is launchable when it is a PE image (an `MZ` header whose `e_lfanew` points
//! at a `PE\0\0` signature) that is an executable, not a DLL, for a machine this host runs, with a section table and a
//! PE32 or PE32+ optional header inside the file: `CreateProcessW` does not fall back to later `PATH` entries, so a
//! malformed, DLL or foreign-architecture `pwsh.exe` is skipped here. Defined on every OS for its tests, which model
//! an x64 host where the host is not Windows.

#[cfg(test)]
mod tests;

use std::ffi::OsStr;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Windows PowerShell 5.1, the shell without a launchable `pwsh.exe`.
pub(super) const INBOX_POWERSHELL: &str = "powershell.exe";
/// The PowerShell 7 executable looked up on `PATH`.
pub(super) const PWSH: &str = "pwsh.exe";
/// Where the inbox PowerShell lives under the system directory (`%SystemRoot%\System32`).
const INBOX_DIRECTORY: [&str; 2] = ["WindowsPowerShell", "v1.0"];

const MACHINE_I386: u16 = 0x014c;
const MACHINE_AMD64: u16 = 0x8664;
const MACHINE_ARM64: u16 = 0xaa64;

const DOS_HEADER_LEN: usize = 0x40;
/// Where the DOS header keeps the PE header's offset (`e_lfanew`).
const PE_OFFSET_FIELD: usize = 0x3c;
const PE_SIGNATURE: &[u8; 4] = b"PE\0\0";
const COFF_HEADER_LEN: usize = 20;
/// The signature and the COFF header.
const PE_HEADERS_LEN: usize = PE_SIGNATURE.len() + COFF_HEADER_LEN;
/// [`DOS_HEADER_LEN`] and [`PE_HEADERS_LEN`] as file offsets.
const DOS_HEADER_END: u64 = DOS_HEADER_LEN as u64;
const PE_HEADERS_SIZE: u64 = PE_HEADERS_LEN as u64;
const SECTION_HEADER_LEN: u64 = 40;
const OPTIONAL_HEADER_MAGIC_LEN: u16 = 2;
const IMAGE_FILE_EXECUTABLE_IMAGE: u16 = 0x0002;
const IMAGE_FILE_DLL: u16 = 0x2000;
const PE32_MAGIC: u16 = 0x010b;
const PE32_PLUS_MAGIC: u16 = 0x020b;

/// This process's shell: the first launchable `pwsh.exe` on its `PATH`, else `powershell.exe` there or in the
/// system directory; `None` without any.
#[cfg(windows)]
pub(super) fn login_shell() -> Option<String> {
    shell_on_path(
        std::env::var_os("PATH").as_deref(),
        system_directory().as_deref(),
        native_machine(),
    )
}

/// The shell of a host of `native_machine` whose `PATH` is `path` and whose system directory is `system`: the first
/// launchable `pwsh.exe` in the absolute directories of `path`, else the first launchable [`INBOX_POWERSHELL`] there,
/// else the one under `system` (when absolute), as a full path; `None` when there is none.
pub(super) fn shell_on_path(path: Option<&OsStr>, system: Option<&Path>, native_machine: u16) -> Option<String> {
    let dirs: Vec<PathBuf> = path
        .into_iter()
        .flat_map(std::env::split_paths)
        .filter(|dir| dir.is_absolute())
        .collect();
    let inbox = system.filter(|system| system.is_absolute()).map(|system| {
        INBOX_DIRECTORY
            .iter()
            .fold(system.to_path_buf(), |dir, part| dir.join(part))
    });
    [PWSH, INBOX_POWERSHELL]
        .into_iter()
        .flat_map(|name| dirs.iter().map(move |dir| dir.join(name)))
        .chain(inbox.map(|dir| dir.join(INBOX_POWERSHELL)))
        .find(|candidate| is_launchable(candidate, native_machine))
        .and_then(|candidate| candidate.into_os_string().into_string().ok())
}

/// The system directory (`%SystemRoot%\System32`), as `GetSystemDirectoryW` names it; `None` when the call fails.
#[cfg(windows)]
fn system_directory() -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt as _;

    use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;

    let mut buffer = [0_u16; 260];
    let capacity = u32::try_from(buffer.len()).ok()?;
    // SAFETY: the buffer is valid for `capacity` UTF-16 units; the call writes at most that many and returns the
    // length written without the NUL, or the size needed (larger than the buffer) when it does not fit, or 0.
    let len = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), capacity) };
    let len = usize::try_from(len)
        .ok()
        .filter(|len| *len > 0 && *len < buffer.len())?;
    Some(PathBuf::from(std::ffi::OsString::from_wide(&buffer[..len])))
}

/// Whether `path` is a PE image a host of `native_machine` can launch.
pub(super) fn is_launchable(path: &Path, native_machine: u16) -> bool {
    Image::read(path).is_some_and(|image| image.is_launchable(native_machine))
}

/// Whether a host of `native_machine` (`IMAGE_FILE_MACHINE_*`) runs an image built for `machine`: a host runs its own
/// architecture; ARM64 Windows also runs x64 and x86 through emulation, x64 Windows runs x86 through WOW64, and an
/// unknown host (detection failed) counts as x64.
pub(super) fn machine_is_compatible(machine: u16, native_machine: u16) -> bool {
    match native_machine {
        MACHINE_ARM64 => matches!(machine, MACHINE_I386 | MACHINE_AMD64 | MACHINE_ARM64),
        MACHINE_I386 => machine == MACHINE_I386,
        _ => matches!(machine, MACHINE_I386 | MACHINE_AMD64),
    }
}

/// The header fields of a file that starts like a PE image.
struct Image {
    /// The file's length.
    len: u64,
    /// Where the PE signature starts.
    pe_offset: u64,
    machine: u16,
    sections: u16,
    optional_header_len: u16,
    characteristics: u16,
    /// The optional header's magic; `None` when the file ends before it.
    magic: Option<u16>,
}

impl Image {
    /// The headers of `path`; `None` unless it is a file with a DOS header pointing at a PE signature and a whole
    /// COFF header.
    fn read(path: &Path) -> Option<Self> {
        let mut file = File::open(path).ok()?;
        let len = file.metadata().ok()?.len();
        let mut dos = [0u8; DOS_HEADER_LEN];
        file.read_exact(&mut dos).ok()?;
        if !dos.starts_with(b"MZ") {
            return None;
        }
        let offset: [u8; 4] = dos[PE_OFFSET_FIELD..PE_OFFSET_FIELD + 4].try_into().ok()?;
        let pe_offset = u64::from(u32::from_le_bytes(offset));
        if pe_offset < DOS_HEADER_END || pe_offset.saturating_add(PE_HEADERS_SIZE) > len {
            return None;
        }
        file.seek(SeekFrom::Start(pe_offset)).ok()?;
        let mut headers = [0u8; PE_HEADERS_LEN];
        file.read_exact(&mut headers).ok()?;
        if !headers.starts_with(PE_SIGNATURE) {
            return None;
        }
        // the COFF header's fields, at their offsets after the signature
        let coff = &headers[PE_SIGNATURE.len()..];
        let field = |at: usize| u16::from_le_bytes([coff[at], coff[at + 1]]);
        let mut magic = [0u8; 2];
        let magic = file.read_exact(&mut magic).ok().map(|()| u16::from_le_bytes(magic));
        Some(Self {
            len,
            pe_offset,
            machine: field(0),
            sections: field(2),
            optional_header_len: field(16),
            characteristics: field(18),
            magic,
        })
    }

    /// Whether a host of `native_machine` launches this image: an executable, not a DLL, for a compatible machine,
    /// with at least one section, its optional header and section table inside the file, PE32 or PE32+.
    fn is_launchable(&self, native_machine: u16) -> bool {
        let tables = u64::from(self.optional_header_len) + u64::from(self.sections) * SECTION_HEADER_LEN;
        let tables_end = self.pe_offset.saturating_add(PE_HEADERS_SIZE).saturating_add(tables);
        let executable = self.characteristics & IMAGE_FILE_EXECUTABLE_IMAGE != 0;
        let dll = self.characteristics & IMAGE_FILE_DLL != 0;
        machine_is_compatible(self.machine, native_machine)
            && self.sections > 0
            && executable
            && !dll
            && self.optional_header_len >= OPTIONAL_HEADER_MAGIC_LEN
            && tables_end <= self.len
            && matches!(self.magic, Some(PE32_MAGIC | PE32_PLUS_MAGIC))
    }
}

/// The host's native machine (`IMAGE_FILE_MACHINE_*`), which an x86 or x64 process under emulation does not share;
/// the process's own when the query fails.
#[cfg(windows)]
fn native_machine() -> u16 {
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, IsWow64Process2};

    let (mut process_machine, mut native_machine) = (0u16, 0u16);
    // SAFETY: `GetCurrentProcess` has no preconditions; it returns a pseudo-handle that needs no closing.
    let process = unsafe { GetCurrentProcess() };
    // SAFETY: `process` is this process's pseudo-handle and both out-pointers are valid for the call.
    let ok = unsafe { IsWow64Process2(process, &raw mut process_machine, &raw mut native_machine) };
    if ok != 0 && native_machine != 0 {
        native_machine
    } else {
        process_machine
    }
}
