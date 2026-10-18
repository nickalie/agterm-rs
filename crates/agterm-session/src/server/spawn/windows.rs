//! The spawn on Windows: a detached process in a process group of its own, outside any job of the launcher's.
//!
//! herdr: `src/platform/windows.rs` (`launch_server_daemon_command`, `launch_server_daemon_with_wmi`,
//! `windows_command_line`, `effective_command_environment`, `unicode_windows_value`, `current_process_is_in_job`,
//! `current_job_kills_processes_on_close`, `detach_server_daemon_command`, `current_process_is_detached_server_daemon`)
//! at `HERDR_REV` (`build/pins.env`), licensed under the Apache License 2.0, modified. Kept: `DETACHED_PROCESS`; WMI's
//! `Win32_Process.Create` with the command line, working directory and whole environment spelled out; the server is
//! detached when it has no console and no job of its kills it on close. Changed, after Task 2's probe
//! (`docs/spikes/20261018-session-server.md`): `CREATE_NEW_PROCESS_GROUP` too; inside a job, `CREATE_BREAKAWAY_FROM_JOB`
//! first (created suspended, and let run only once out of every job), and WMI only where a job refuses breakaway,
//! never a plain retry inside it (`super::start_in_job`); any job
//! counts, not only a kill-on-close one, since a process sees only its innermost job's limits and an enclosing job
//! may still kill on close (nested jobs); the process is created with `CreateProcessW` without handle inheritance, so
//! none of the launcher's inheritable handles (its own standard handles, say) reaches the server; WMI runs on a
//! thread of its own, so its COM initialization never touches the caller's thread; a WMI start that ended before its
//! handle could be had reads as ended, so the caller probes again (a racing start's loser) instead of failing; and
//! the client's handshake, which a pipe cannot time out, runs on a thread whose pending I/O is cancelled once the
//! handshake's timeout runs out (`exchange_within`).

use std::ffi::{OsStr, c_void};
use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::{AsHandle as _, AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::os::windows::process::ExitStatusExt as _;
use std::process::ExitStatus;
use std::ptr::{null, null_mut};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
use windows_sys::Win32::System::Console::GetConsoleWindow;
use windows_sys::Win32::System::IO::CancelIoEx;
use windows_sys::Win32::System::JobObjects::{
    IsProcessInJob, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, QueryInformationJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
    DETACHED_PROCESS, GetCurrentProcess, GetExitCodeProcess, OpenProcess, PROCESS_INFORMATION,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, ResumeThread, STARTUPINFOW,
    TerminateProcess, WaitForSingleObject,
};

use super::{Breakaway, Exit, ServerCommand, SpawnPath, quote_windows_arg, start_in_job};
use crate::endpoint::Stream;
use crate::protocol::FramingError;

/// Every start's flags: no console, and a process group of its own, so no console's Ctrl+C or Ctrl+Break reaches it.
const DETACHED: u32 = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;

/// How often a timed-out exchange's pipe is cancelled while its thread holds on: a cancel that comes before the
/// thread's read or write starts cancels nothing.
const CANCEL_RETRY: Duration = Duration::from_millis(10);
/// How long a timed-out exchange's thread is waited for once cancelled.
const CANCEL_TIMEOUT: Duration = Duration::from_secs(1);

/// `ERROR_INVALID_PARAMETER`: what `OpenProcess` gives for a pid no process has (any more).
const ERROR_INVALID_PARAMETER: i32 = 87;

/// A started server: its process handle, which keeps its pid from being reused while it is held; `None` for a
/// process that had already ended, its status lost, when its handle was asked for (a WMI start, which hands over a
/// pid only).
#[derive(Debug)]
pub(super) struct Process(Option<OwnedHandle>);

impl Process {
    pub(super) fn try_exit(&mut self) -> Option<Exit> {
        let Some(handle) = &self.0 else {
            return Some(Exit::Unknown);
        };
        // SAFETY: the handle is an open process handle with SYNCHRONIZE.
        if unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) } != WAIT_OBJECT_0 {
            return None;
        }
        let mut code = 0_u32;
        // SAFETY: the handle is an open process handle with query access, and `code` is writable.
        if unsafe { GetExitCodeProcess(handle.as_raw_handle(), &raw mut code) } == 0 {
            return Some(Exit::Unknown);
        }
        Some(Exit::Status(ExitStatus::from_raw(code)))
    }

    pub(super) fn terminate(&mut self, _pid: u32) {
        let Some(handle) = &self.0 else {
            return;
        };
        // SAFETY: the handle is an open process handle with terminate access; a process already gone fails harmlessly.
        unsafe { TerminateProcess(handle.as_raw_handle(), 1) };
    }
}

pub(super) fn spawn(command: &ServerCommand) -> io::Result<(u32, SpawnPath, Process)> {
    if !current_process_is_in_job()? {
        let (pid, process) = create_process(command, DETACHED)?;
        return Ok((pid, SpawnPath::Detached, process));
    }
    let ((pid, process), path) = start_in_job(|| create_breaking_away(command), || wmi_create(command))?;
    Ok((pid, path, process))
}

pub(super) fn current_process_is_detached() -> bool {
    // SAFETY: GetConsoleWindow takes no arguments.
    if !unsafe { GetConsoleWindow() }.is_null() {
        return false;
    }
    // membership alone does not tie the server's life to its launcher's (herdr's note)
    matches!(current_job_kills_processes_on_close(), Ok(false))
}

/// `CreateProcessW` of the command with `flags`, inheriting no handle.
fn create_process(command: &ServerCommand, flags: u32) -> io::Result<(u32, Process)> {
    let (pid, process, _thread) = create(command, flags)?;
    Ok((pid, Process(Some(process))))
}

/// A breakaway start: created suspended and let run only once out of every job. A job that refuses breakaway fails
/// `CreateProcessW` only when it is the innermost one; an enclosing one keeps the process without an error (the
/// Win11 VM's guest-tools job, Task 22), and then the process is ended unrun.
fn create_breaking_away(command: &ServerCommand) -> io::Result<Breakaway<(u32, Process)>> {
    let (pid, process, thread) = create(command, DETACHED | CREATE_BREAKAWAY_FROM_JOB | CREATE_SUSPENDED)?;
    let raw = process.as_raw_handle();
    let mut process = Process(Some(process));
    let mut in_job = 0;
    // SAFETY: the process handle, which `process` holds open, has all access, a null job asks about any job, and
    // `in_job` is writable.
    let asked = unsafe { IsProcessInJob(raw, null_mut(), &raw mut in_job) };
    if asked == 0 || in_job != 0 {
        let error = (asked == 0).then(io::Error::last_os_error);
        process.terminate(pid);
        return error.map_or(Ok(Breakaway::Kept), Err);
    }
    // SAFETY: the thread handle is the suspended primary thread's, with all access.
    if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
        let error = io::Error::last_os_error();
        process.terminate(pid);
        return Err(error);
    }
    Ok(Breakaway::Left((pid, process)))
}

/// `CreateProcessW` of the command with `flags`, inheriting no handle: its pid, process and primary thread. The
/// standard handles are left empty, which a detached process reads as the null device.
fn create(command: &ServerCommand, flags: u32) -> io::Result<(u32, OwnedHandle, OwnedHandle)> {
    let application = wide(command.executable.as_os_str())?;
    let mut command_line = wide(OsStr::new(&command_line(command)?))?;
    let environment = environment_block(&command.env)?;
    let directory = command.cwd.as_deref().map(|cwd| wide(cwd.as_os_str())).transpose()?;
    let startup = STARTUPINFOW {
        cb: u32::try_from(size_of::<STARTUPINFOW>()).map_err(io::Error::other)?,
        ..STARTUPINFOW::default()
    };
    let mut information = PROCESS_INFORMATION::default();
    // SAFETY: every string is NUL-terminated and outlives the call, the command line is writable as CreateProcessW
    // requires, the environment block is a double-NUL-terminated UTF-16 block (CREATE_UNICODE_ENVIRONMENT), and the
    // startup and process information structures are valid for the call.
    let created = unsafe {
        CreateProcessW(
            application.as_ptr(),
            command_line.as_mut_ptr(),
            null(),
            null(),
            0,
            flags | CREATE_UNICODE_ENVIRONMENT,
            environment.as_ptr().cast::<c_void>(),
            directory.as_ref().map_or(null(), Vec::as_ptr),
            &raw const startup,
            &raw mut information,
        )
    };
    if created == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateProcessW returned a fresh process handle nobody else owns, with all access.
    let process = unsafe { OwnedHandle::from_raw_handle(information.hProcess) };
    // SAFETY: CreateProcessW returned a fresh thread handle nobody else owns, with all access.
    let thread = unsafe { OwnedHandle::from_raw_handle(information.hThread) };
    Ok((information.dwProcessId, process, thread))
}

/// Runs `exchange` over `stream` on a thread of its own and gives up after `timeout` with
/// [`io::ErrorKind::TimedOut`], since a pipe's reads and writes take no timeout: the thread's pending read or write
/// is cancelled (`CancelIoEx`, through a duplicate of the pipe's handle, which names the same pipe) until the thread
/// lets go, or for [`CANCEL_TIMEOUT`], after which the thread is left to end with the pipe's peer.
///
/// # Errors
///
/// The exchange's own, or [`FramingError::Io`] when the timeout ran out or no thread could be started.
pub(super) fn exchange_within<T: Send + 'static>(
    stream: Stream,
    timeout: Duration,
    exchange: impl FnOnce(&mut Stream) -> Result<T, FramingError> + Send + 'static,
) -> Result<(Stream, T), FramingError> {
    let Stream::NamedPipe(pipe) = &stream;
    let pipe = pipe.as_handle().try_clone_to_owned().map_err(FramingError::Io)?;
    let (send, receive) = mpsc::channel();
    let worker = thread::Builder::new()
        .name("agterm-session-handshake".to_owned())
        .spawn(move || {
            let mut stream = stream;
            let result = match exchange(&mut stream) {
                Ok(value) => Ok((stream, value)),
                Err(error) => {
                    abandon(stream);
                    Err(error)
                }
            };
            let _ = send.send(result);
        })
        .map_err(FramingError::Io)?;
    match receive.recv_timeout(timeout) {
        Ok(result) => {
            let _ = worker.join();
            return result;
        }
        Err(RecvTimeoutError::Disconnected) => {
            return Err(FramingError::Io(io::Error::other("the handshake thread panicked")));
        }
        Err(RecvTimeoutError::Timeout) => {}
    }
    let deadline = Instant::now() + CANCEL_TIMEOUT;
    loop {
        // SAFETY: the handle is this function's own duplicate of the pipe's, open until it returns; a null OVERLAPPED
        // cancels every pending operation on the pipe, and with none pending the call fails harmlessly.
        unsafe { CancelIoEx(pipe.as_raw_handle(), null()) };
        match receive.recv_timeout(CANCEL_RETRY) {
            // the exchange finished before the cancel reached it
            Ok(Ok(done)) => {
                let _ = worker.join();
                return Ok(done);
            }
            Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => {
                let _ = worker.join();
                break;
            }
            Err(RecvTimeoutError::Timeout) if Instant::now() >= deadline => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
    Err(FramingError::Io(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("the peer did not answer within {}s", timeout.as_secs_f32()),
    )))
}

/// Closes a connection whose exchange failed at once. A dropped pipe stream with data written since its last flush
/// goes to interprocess's linger pool, whose one thread flushes it (`FlushFileBuffers`, which waits for the peer to
/// read it all) before closing it: a peer that never reads, the very one a timed-out exchange gives up on, would hold
/// that thread, and with it every pipe this process drops later, open for good (their servers then never see their
/// clients leave). What was written is left unread instead.
fn abandon(stream: Stream) {
    let Stream::NamedPipe(pipe) = &stream;
    pipe.inner().assume_flushed();
    drop(stream);
}

/// herdr's `launch_server_daemon_with_wmi`, on a thread of its own: `Win32_Process.Create` starts the server from
/// WMI's provider host, outside this process's job.
fn wmi_create(command: &ServerCommand) -> io::Result<(u32, Process)> {
    let input = wmi_input(command)?;
    let pid = thread::scope(|scope| {
        thread::Builder::new()
            .name("agterm-session-wmi".to_owned())
            .spawn_scoped(scope, || wmi_call(&input))?
            .join()
            .unwrap_or_else(|_| Err(io::Error::other("the WMI thread panicked")))
    })?;
    Ok((pid, track(pid)?))
}

/// The process WMI just started as `pid`. Its handle is to be had while the process runs, or while anyone holds a
/// handle to it; WMI's provider host does not, so a process that already ended (a racing start's loser, gone with
/// [`super::super::EXIT_TAKEN`] in milliseconds) reads as ended with its status lost, and the caller probes again
/// rather than failing the start.
///
/// # Errors
///
/// The process exists but could not be opened.
fn track(pid: u32) -> io::Result<Process> {
    match open_process(pid) {
        Ok(handle) => Ok(Process(Some(handle))),
        Err(error) if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER) => Ok(Process(None)),
        Err(error) => Err(error),
    }
}

#[derive(serde::Serialize)]
struct CreateInput {
    #[serde(rename = "CommandLine")]
    command_line: String,
    #[serde(rename = "CurrentDirectory")]
    current_directory: String,
    #[serde(rename = "ProcessStartupInformation")]
    startup: ProcessStartup,
}

/// WMI's embedded `Win32_ProcessStartup`: the type's name is the CIM class name WMI serializes it as.
#[derive(serde::Serialize)]
#[serde(rename = "Win32_ProcessStartup")]
struct ProcessStartup {
    #[serde(rename = "CreateFlags")]
    create_flags: u32,
    #[serde(rename = "EnvironmentVariables")]
    environment: Vec<String>,
}

/// WMI resolves the class from the type's name, CIM casing included.
#[derive(serde::Deserialize)]
#[serde(rename = "Win32_Process")]
struct Win32Process;

#[derive(serde::Deserialize)]
struct CreateOutput {
    #[serde(rename = "ProcessId")]
    process_id: Option<u32>,
    #[serde(rename = "ReturnValue")]
    return_value: u32,
}

fn wmi_input(command: &ServerCommand) -> io::Result<CreateInput> {
    let current_directory = match &command.cwd {
        Some(cwd) => cwd.clone(),
        None => std::env::current_dir()?,
    };
    Ok(CreateInput {
        command_line: command_line(command)?,
        current_directory: unicode(current_directory.as_os_str(), "the working directory")?,
        startup: ProcessStartup {
            create_flags: DETACHED,
            environment: environment_strings(&command.env)?,
        },
    })
}

fn wmi_call(input: &CreateInput) -> io::Result<u32> {
    let connection =
        wmi::WMIConnection::new().map_err(|error| io::Error::other(format!("could not connect to WMI: {error}")))?;
    let output: CreateOutput = connection
        .exec_class_method::<Win32Process, _>("Create", input)
        .map_err(|error| io::Error::other(format!("WMI Win32_Process.Create failed: {error}")))?;
    if output.return_value != 0 {
        return Err(io::Error::other(format!(
            "WMI Win32_Process.Create returned error {}",
            output.return_value
        )));
    }
    output
        .process_id
        .ok_or_else(|| io::Error::other("WMI Win32_Process.Create succeeded without a process id"))
}

fn open_process(pid: u32) -> io::Result<OwnedHandle> {
    let access = PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE;
    // SAFETY: OpenProcess takes no pointers; a null result is checked before the handle is owned.
    let handle = unsafe { OpenProcess(access, 0, pid) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: OpenProcess returned a fresh handle nobody else owns.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

/// The program and its arguments, each quoted as `CommandLineToArgvW` reads it back.
fn command_line(command: &ServerCommand) -> io::Result<String> {
    std::iter::once(command.executable.as_os_str())
        .chain(command.args.iter().map(std::ffi::OsString::as_os_str))
        .map(|value| unicode(value, "an argument of the server's command").map(|value| quote_windows_arg(&value)))
        .collect::<io::Result<Vec<_>>>()
        .map(|parts| parts.join(" "))
}

/// The environment as `name=value` strings, sorted by name ignoring case as Windows keeps an environment block.
fn environment_strings(env: &[(std::ffi::OsString, std::ffi::OsString)]) -> io::Result<Vec<String>> {
    let mut entries = env
        .iter()
        .map(|(name, value)| {
            let name = unicode(name, "an environment variable's name")?;
            if name.is_empty() || name.chars().skip(1).any(|ch| ch == '=') || name.contains('\0') {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{name:?} is not an environment variable's name"),
                ));
            }
            let value = unicode(value, "an environment variable's value")?;
            if value.contains('\0') {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("the value of {name} holds a NUL"),
                ));
            }
            Ok((name, value))
        })
        .collect::<io::Result<Vec<_>>>()?;
    entries.sort_by_cached_key(|(name, _)| name.to_uppercase());
    Ok(entries
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect())
}

/// The environment as a `CREATE_UNICODE_ENVIRONMENT` block: each `name=value` NUL-terminated, then a final NUL.
fn environment_block(env: &[(std::ffi::OsString, std::ffi::OsString)]) -> io::Result<Vec<u16>> {
    let mut block: Vec<u16> = Vec::new();
    for entry in environment_strings(env)? {
        block.extend(entry.encode_utf16());
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

/// `value` as UTF-16 with its terminating NUL.
fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
    if value.encode_wide().any(|unit| unit == 0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} holds a NUL", value.to_string_lossy()),
        ));
    }
    Ok(value.encode_wide().chain(std::iter::once(0)).collect())
}

/// herdr's `unicode_windows_value`: WMI takes strings.
fn unicode(value: &OsStr, what: &str) -> io::Result<String> {
    value
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("{what} is not valid Unicode")))
}

fn current_process_is_in_job() -> io::Result<bool> {
    let mut in_job = 0;
    // SAFETY: GetCurrentProcess returns a pseudo-handle that is always valid and needs no closing.
    let current = unsafe { GetCurrentProcess() };
    // SAFETY: the process handle is valid, a null job asks about any job, and `in_job` is writable.
    if unsafe { IsProcessInJob(current, null_mut(), &raw mut in_job) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(in_job != 0)
}

/// Whether this process's innermost job kills its processes when the job closes.
fn current_job_kills_processes_on_close() -> io::Result<bool> {
    if !current_process_is_in_job()? {
        return Ok(false);
    }
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    let size = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()).map_err(io::Error::other)?;
    // SAFETY: a null job is the calling process's own; `limits` is writable and `size` its exact size.
    let queried = unsafe {
        QueryInformationJobObject(
            null_mut(),
            JobObjectExtendedLimitInformation,
            (&raw mut limits).cast::<c_void>(),
            size,
            null_mut(),
        )
    };
    if queried == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(limits.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE != 0)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::endpoint::Endpoint;
    use crate::protocol::{Control, Hello, Role, read_frame, write_control};

    /// Set in the child the WMI and `CreateProcessW` tests start (this test binary again): where to write what it got.
    const CAPTURE: &str = "AGLS_TEST_CAPTURE";
    const VALUE: &str = "AGLS_TEST_VALUE";
    const ARGS: [&str; 6] = [
        "two words",
        "q\"uote",
        r"trailing\",
        r#"back\"slash"#,
        "",
        r"C:\with space\",
    ];

    fn os(value: &str) -> OsString {
        OsString::from(value)
    }

    #[test]
    fn the_environment_block_is_sorted_ignoring_case_and_double_nul_terminated() {
        let env = [(os("b"), os("2")), (os("A"), os("1")), (os("=C:"), os(r"C:\x"))];
        assert_eq!(environment_strings(&env).unwrap(), ["=C:=C:\\x", "A=1", "b=2"]);
        let block = environment_block(&[(os("A"), os("1"))]).unwrap();
        assert_eq!(block, "A=1\0\0".encode_utf16().collect::<Vec<_>>());
        assert_eq!(environment_block(&[]).unwrap(), [0, 0]);
    }

    #[test]
    fn a_malformed_environment_is_refused() {
        for env in [[(os("A=B"), os("1"))], [(os(""), os("1"))], [(os("A"), os("1\u{0}2"))]] {
            let error = environment_strings(&env).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{env:?}");
        }
    }

    #[test]
    fn the_command_line_quotes_every_part() {
        let command = ServerCommand {
            executable: PathBuf::from(r"C:\Program Files\agterm\agterm-session.exe"),
            args: vec![os("server"), os(r"C:\Users\a b\AppData\Local\agterm\live\0123")],
            env: Vec::new(),
            cwd: None,
        };
        assert_eq!(
            command_line(&command).unwrap(),
            r#""C:\Program Files\agterm\agterm-session.exe" server "C:\Users\a b\AppData\Local\agterm\live\0123""#
        );
    }

    /// The child: writes its arguments after the test filter, its working directory and one variable, then ends.
    fn capture_if_child() -> bool {
        let Some(capture) = std::env::var_os(CAPTURE) else {
            return false;
        };
        let args: Vec<String> = std::env::args().skip(3).collect();
        // SAFETY: GetConsoleWindow takes no arguments.
        let console = !unsafe { GetConsoleWindow() }.is_null();
        let report = serde_json::json!({
            "args": args,
            "cwd": std::env::current_dir().unwrap(),
            "value": std::env::var(VALUE).ok(),
            "console": console,
            "detached": current_process_is_detached(),
        });
        fs::write(capture, report.to_string()).unwrap();
        true
    }

    /// This test binary run as a child running only `test`, with [`ARGS`] after the filter, in `cwd`.
    fn child_command(test: &str, capture: &Path, cwd: &Path) -> ServerCommand {
        let mut args = vec![os("--exact"), os(test)];
        args.extend(ARGS.iter().map(|arg| os(arg)));
        let mut env = super::super::server_environment(std::env::vars_os());
        env.push((os(CAPTURE), capture.as_os_str().to_owned()));
        env.push((os(VALUE), os("a value = with spaces")));
        ServerCommand {
            executable: std::env::current_exe().unwrap(),
            args,
            env,
            cwd: Some(cwd.to_path_buf()),
        }
    }

    fn wait_for_capture(capture: &Path, mut process: Process) -> serde_json::Value {
        let deadline = Instant::now() + Duration::from_secs(30);
        while process.try_exit().is_none() {
            assert!(Instant::now() < deadline, "the child did not end");
            thread::sleep(Duration::from_millis(50));
        }
        serde_json::from_str(&fs::read_to_string(capture).unwrap()).unwrap()
    }

    fn assert_kept(report: &serde_json::Value, cwd: &Path) {
        assert_eq!(report["args"], serde_json::json!(ARGS));
        assert_eq!(report["value"], "a value = with spaces");
        assert_eq!(
            fs::canonicalize(report["cwd"].as_str().unwrap()).unwrap(),
            fs::canonicalize(cwd).unwrap()
        );
        assert_eq!(report["console"], false);
    }

    #[test]
    fn a_created_process_keeps_the_argv_the_working_directory_and_the_environment() {
        if capture_if_child() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let capture = root.path().join("capture.json");
        let command = child_command(
            "server::spawn::windows::tests::a_created_process_keeps_the_argv_the_working_directory_and_the_environment",
            &capture,
            root.path(),
        );
        let (_pid, process) = create_process(&command, DETACHED).unwrap();
        assert_kept(&wait_for_capture(&capture, process), root.path());
    }

    #[test]
    fn wmi_keeps_the_argv_the_working_directory_and_the_environment() {
        if capture_if_child() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let capture = root.path().join("capture.json");
        let command = child_command(
            "server::spawn::windows::tests::wmi_keeps_the_argv_the_working_directory_and_the_environment",
            &capture,
            root.path(),
        );
        let (_pid, process) = wmi_create(&command).unwrap();
        let report = wait_for_capture(&capture, process);
        assert_kept(&report, root.path());
        // outside any job of this process's, and without a console
        assert_eq!(report["detached"], true);
    }

    /// A connected pair in a fresh namespace, with the listener that keeps the pipe: the client's end and the
    /// server's.
    fn connected_pair() -> (tempfile::TempDir, crate::endpoint::Listener, Stream, Stream) {
        let root = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::new(root.path().join("live"));
        let listener = endpoint.bind().unwrap();
        let accept = thread::spawn(move || {
            let accepted = listener.accept();
            (listener, accepted)
        });
        let client = endpoint.connect().unwrap();
        let (listener, server) = accept.join().unwrap();
        (root, listener, client, server.unwrap())
    }

    #[test]
    fn a_silent_peer_ends_the_exchange_at_its_timeout_and_its_thread_lets_go() {
        let (_root, _listener, client, _server) = connected_pair();
        let (ended, thread_ended) = mpsc::channel();
        let timeout = Duration::from_millis(300);
        let started = Instant::now();
        let error = exchange_within(client, timeout, move |stream| {
            // written and never read: the peer is silent
            write_control(stream, &Control::Hello(hello()))?;
            let read = read_frame(stream).map(|_| ());
            let _ = ended.send(());
            read
        })
        .unwrap_err();
        assert!(
            matches!(&error, FramingError::Io(error) if error.kind() == io::ErrorKind::TimedOut),
            "{error}"
        );
        assert!(started.elapsed() >= timeout);
        // the pending read was cancelled, not left behind
        thread_ended.try_recv().unwrap();
        // the abandoned connection holds no thread of this process's: a connection dropped after it still closes
        let (_root, _listener, mut client, mut server) = connected_pair();
        write_control(&mut client, &Control::Hello(hello())).unwrap();
        drop(client);
        let (send, closed) = mpsc::channel();
        thread::spawn(move || {
            let _ = read_frame(&mut server);
            let _ = send.send(read_frame(&mut server).map(|_| ()));
        });
        let next = closed
            .recv_timeout(Duration::from_secs(30))
            .expect("the connection dropped later stays open");
        assert!(next.is_err(), "{next:?}");
    }

    fn hello() -> Hello {
        Hello {
            role: Role::Control,
            build: "test".to_owned(),
            capabilities: Vec::new(),
        }
    }

    #[test]
    fn an_exchange_within_its_timeout_hands_the_stream_back() {
        let (_root, _listener, client, mut server) = connected_pair();
        let hello = hello();
        let sent = Control::Hello(hello.clone());
        let answer = thread::spawn(move || write_control(&mut server, &sent).map(|()| server));
        let (mut client, frame) = exchange_within(client, Duration::from_secs(30), read_frame).unwrap();
        assert_eq!(frame.control().unwrap(), Control::Hello(hello.clone()));
        // the stream still works
        let mut server = answer.join().unwrap().unwrap();
        write_control(&mut client, &Control::Hello(hello.clone())).unwrap();
        assert_eq!(
            read_frame(&mut server).unwrap().control().unwrap(),
            Control::Hello(hello)
        );
    }

    #[test]
    fn a_wmi_start_that_ended_before_its_handle_reads_as_ended_with_its_status_lost() {
        // no process has pid 0 to open (OpenProcess gives the System Idle Process's ERROR_INVALID_PARAMETER), as with
        // a process that has already ended and that nobody holds
        let mut process = track(0).unwrap();
        assert_eq!(process.try_exit(), Some(Exit::Unknown));
        // stopping it is a no-op
        process.terminate(0);
        assert_eq!(process.try_exit(), Some(Exit::Unknown));
    }

    #[test]
    fn a_wmi_start_still_running_is_tracked_by_its_handle() {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "--list"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let mut process = track(child.id()).unwrap();
        // the child holds its own handle, so the pid stays this process's until both are closed
        let status = child.wait().unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let exit = loop {
            if let Some(exit) = process.try_exit() {
                break exit;
            }
            assert!(Instant::now() < deadline, "the child did not end");
            thread::sleep(Duration::from_millis(50));
        };
        assert_eq!(exit, Exit::Status(status));
    }

    #[test]
    fn a_wmi_failure_is_reported_with_its_return_value() {
        let root = tempfile::tempdir().unwrap();
        let command = ServerCommand {
            executable: root.path().join("missing.exe"),
            args: Vec::new(),
            env: super::super::server_environment(std::env::vars_os()),
            cwd: Some(root.path().to_path_buf()),
        };
        let error = wmi_create(&command).unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("WMI Win32_Process.Create returned error "),
            "{error}"
        );
    }
}
