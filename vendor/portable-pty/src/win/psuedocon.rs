use super::WinChild;
use crate::cmdbuilder::CommandBuilder;
use crate::win::procthreadattr::ProcThreadAttributeList;
use anyhow::{bail, ensure, Error};
use filedescriptor::{FileDescriptor, OwnedHandle};
use lazy_static::lazy_static;
use std::ffi::{OsStr, OsString};
use std::io::Error as IoError;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::sync::Mutex;
use std::{mem, ptr};
use winapi::shared::minwindef::{DWORD, HMODULE};
use winapi::shared::winerror::{HRESULT, S_OK};
use winapi::um::handleapi::*;
use winapi::um::libloaderapi::{GetModuleHandleW, GetProcAddress};
use winapi::um::processthreadsapi::*;
use winapi::um::winbase::{
    CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};
use winapi::um::wincon::COORD;
use winapi::um::winnt::HANDLE;

pub type HPCON = HANDLE;

pub const PSUEDOCONSOLE_INHERIT_CURSOR: DWORD = 0x1;
pub const PSEUDOCONSOLE_RESIZE_QUIRK: DWORD = 0x2;
pub const PSEUDOCONSOLE_WIN32_INPUT_MODE: DWORD = 0x4;
#[allow(dead_code)]
pub const PSEUDOCONSOLE_PASSTHROUGH_MODE: DWORD = 0x8;

type CreatePseudoConsoleFn = unsafe extern "system" fn(
    size: COORD,
    h_input: HANDLE,
    h_output: HANDLE,
    flags: DWORD,
    hpc: *mut HPCON,
) -> HRESULT;
type ResizePseudoConsoleFn = unsafe extern "system" fn(hpc: HPCON, size: COORD) -> HRESULT;
type ClosePseudoConsoleFn = unsafe extern "system" fn(hpc: HPCON);

// The fields mirror the exported Win32 symbol names.
#[allow(non_snake_case)]
struct ConPtyFuncs {
    CreatePseudoConsole: CreatePseudoConsoleFn,
    ResizePseudoConsole: ResizePseudoConsoleFn,
    ClosePseudoConsole: ClosePseudoConsoleFn,
}

unsafe fn load_symbol<T: Copy>(module: HMODULE, name: &'static [u8]) -> Result<T, String> {
    let symbol = GetProcAddress(module, name.as_ptr().cast());
    if symbol.is_null() {
        return Err(format!(
            "missing symbol {}",
            String::from_utf8_lossy(&name[..name.len() - 1])
        ));
    }
    Ok(mem::transmute_copy(&symbol))
}

fn load_system_conpty() -> Result<ConPtyFuncs, String> {
    let name: Vec<u16> = OsStr::new("kernel32.dll")
        .encode_wide()
        .chain(Some(0))
        .collect();
    // kernel32 stays mapped for the whole life of every process: its module
    // handle needs no reference of its own.
    let module = unsafe { GetModuleHandleW(name.as_ptr()) };
    if module.is_null() {
        return Err(IoError::last_os_error().to_string());
    }
    unsafe {
        Ok(ConPtyFuncs {
            CreatePseudoConsole: load_symbol(module, b"CreatePseudoConsole\0")?,
            ResizePseudoConsole: load_symbol(module, b"ResizePseudoConsole\0")?,
            ClosePseudoConsole: load_symbol(module, b"ClosePseudoConsole\0")?,
        })
    }
}

fn load_conpty() -> ConPtyFuncs {
    // Only the ConPTY that the already loaded kernel32 exports: never a bare
    // `conpty.dll` through the DLL search path, which would load whatever
    // library of that name another application put on it. If the kernel
    // doesn't export these functions then the system is too old and we
    // cannot run.
    load_system_conpty().unwrap_or_else(|error| {
        panic!(
            "this system does not support conpty. Windows 10 October 2018 or newer is required: {}",
            error
        )
    })
}

lazy_static! {
    static ref CONPTY: ConPtyFuncs = load_conpty();
}

/// What kernelbase's HPCON points at: { hSignal, hPtyReference,
/// hConPtyProcess }. Undocumented; read only after the caller's gate.
type ConsoleHandles = [HANDLE; 3];

pub struct PsuedoCon {
    con: HPCON,
    /// `con` is a `Box<ConsoleHandles>` rebuilt from handed-over handles,
    /// owned here, not an HPCON CreatePseudoConsole returned.
    imported: bool,
}

unsafe impl Send for PsuedoCon {}
unsafe impl Sync for PsuedoCon {}

impl Drop for PsuedoCon {
    fn drop(&mut self) {
        if self.imported {
            // never ClosePseudoConsole: it frees the HPCON as kernelbase
            // allocated it, and this one was not
            let handles = unsafe { Box::from_raw(self.con as *mut ConsoleHandles) };
            for handle in handles.iter() {
                unsafe { CloseHandle(*handle) };
            }
        } else {
            unsafe { (CONPTY.ClosePseudoConsole)(self.con) };
        }
    }
}

impl PsuedoCon {
    /// A console over handles another process handed over, in the order
    /// [`PsuedoCon::console_handles`] returns them; it owns them.
    pub fn from_console_handles(handles: [std::os::windows::io::OwnedHandle; 3]) -> Self {
        use std::os::windows::io::IntoRawHandle;
        let handles: Box<ConsoleHandles> =
            Box::new(handles.map(|handle| handle.into_raw_handle() as HANDLE));
        Self {
            con: Box::into_raw(handles) as HPCON,
            imported: true,
        }
    }

    /// The signal, reference and console host process handles behind this
    /// console, borrowed.
    ///
    /// # Safety
    ///
    /// The HPCON layout is undocumented: the caller has checked that the
    /// ConPTY implementation this process runs on keeps those three handles
    /// at the start of the struct an HPCON points at.
    pub unsafe fn console_handles(&self) -> [HANDLE; 3] {
        *(self.con as *const ConsoleHandles)
    }

    pub fn new(size: COORD, input: FileDescriptor, output: FileDescriptor) -> Result<Self, Error> {
        let mut con: HPCON = INVALID_HANDLE_VALUE;
        let result = unsafe {
            (CONPTY.CreatePseudoConsole)(
                size,
                input.as_raw_handle() as _,
                output.as_raw_handle() as _,
                PSUEDOCONSOLE_INHERIT_CURSOR
                    | PSEUDOCONSOLE_RESIZE_QUIRK
                    | PSEUDOCONSOLE_WIN32_INPUT_MODE,
                &mut con,
            )
        };
        ensure!(
            result == S_OK,
            "failed to create psuedo console: HRESULT {}",
            result
        );
        Ok(Self {
            con,
            imported: false,
        })
    }

    pub fn resize(&self, size: COORD) -> Result<(), Error> {
        let result = unsafe { (CONPTY.ResizePseudoConsole)(self.con, size) };
        ensure!(
            result == S_OK,
            "failed to resize console to {}x{}: HRESULT: {}",
            size.X,
            size.Y,
            result
        );
        Ok(())
    }

    pub fn spawn_command(&self, cmd: CommandBuilder) -> anyhow::Result<WinChild> {
        self.spawn_command_in_job(cmd, None)
    }

    /// Spawn `cmd` attached to this console and, with `job`, inside that job
    /// object from its first instruction on.
    pub fn spawn_command_in_job(
        &self,
        cmd: CommandBuilder,
        job: Option<HANDLE>,
    ) -> anyhow::Result<WinChild> {
        let mut si: STARTUPINFOEXW = unsafe { mem::zeroed() };
        si.StartupInfo.cb = mem::size_of::<STARTUPINFOEXW>() as u32;
        // Explicitly set the stdio handles as invalid handles otherwise
        // we can end up with a weird state where the spawned process can
        // inherit the explicitly redirected output handles from its parent.
        // For example, when daemonizing wezterm-mux-server, the stdio handles
        // are redirected to a log file and the spawned process would end up
        // writing its output there instead of to the pty we just created.
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
        si.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
        si.StartupInfo.hStdError = INVALID_HANDLE_VALUE;

        // UpdateProcThreadAttribute keeps a pointer to the job list: it lives
        // until CreateProcessW below has returned.
        let jobs: Vec<HANDLE> = job.into_iter().collect();
        let mut attrs = ProcThreadAttributeList::with_capacity(1 + jobs.len() as DWORD)?;
        attrs.set_pty(self.con)?;
        if !jobs.is_empty() {
            attrs.set_job_list(&jobs)?;
        }
        si.lpAttributeList = attrs.as_mut_ptr();

        let mut pi: PROCESS_INFORMATION = unsafe { mem::zeroed() };

        let (mut exe, mut cmdline) = cmd.cmdline()?;
        let cmd_os = OsString::from_wide(&cmdline);

        let cwd = cmd.current_directory();

        let res = unsafe {
            CreateProcessW(
                exe.as_mut_slice().as_mut_ptr(),
                cmdline.as_mut_slice().as_mut_ptr(),
                ptr::null_mut(),
                ptr::null_mut(),
                0,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
                cmd.environment_block().as_mut_slice().as_mut_ptr() as *mut _,
                cwd.as_ref()
                    .map(|c| c.as_slice().as_ptr())
                    .unwrap_or(ptr::null()),
                &mut si.StartupInfo,
                &mut pi,
            )
        };
        if res == 0 {
            let err = IoError::last_os_error();
            let msg = format!(
                "CreateProcessW `{:?}` in cwd `{:?}` failed: {}",
                cmd_os,
                cwd.as_ref().map(|c| OsString::from_wide(c)),
                err
            );
            log::error!("{}", msg);
            bail!("{}", msg);
        }

        // Make sure we close out the thread handle so we don't leak it;
        // we do this simply by making it owned
        let _main_thread = unsafe { OwnedHandle::from_raw_handle(pi.hThread as _) };
        let proc = unsafe { OwnedHandle::from_raw_handle(pi.hProcess as _) };

        Ok(WinChild {
            proc: Mutex::new(proc),
        })
    }
}
