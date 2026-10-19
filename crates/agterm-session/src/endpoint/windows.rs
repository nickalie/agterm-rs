//! The endpoint on Windows: a named pipe named after the namespace directory, owner-only.
//!
//! herdr: `src/ipc.rs` and `src/platform/windows.rs` (`bind_private_local_listener`,
//! `local_server_security_descriptor`, `connect_local_stream_with_timeout`, `local_server_connection_error`,
//! `windows_named_pipe_available`) at `HERDR_REV` (`build/pins.env`), licensed under the Apache License 2.0, modified
//! (see the parent module).

use std::ffi::c_void;
use std::fs;
use std::io;
use std::os::windows::io::{AsHandle, AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::null_mut;
use std::time::Duration;

use interprocess::local_socket::traits::{Stream as _, StreamCommon as _};
use interprocess::local_socket::{GenericNamespaced, ListenerOptions, ToNsName};
use interprocess::os::windows::local_socket::ListenerOptionsExt as _;
use interprocess::os::windows::named_pipe::{DuplexPipeStream, pipe_mode::Bytes};
use interprocess::os::windows::security_descriptor::SecurityDescriptor;
use widestring::U16CString;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Security::{
    EqualSid, GetTokenInformation, TOKEN_ELEVATION, TOKEN_INFORMATION_CLASS, TOKEN_QUERY, TOKEN_USER, TokenElevation,
    TokenUser,
};
use windows_sys::Win32::System::IO::CancelIoEx;
use windows_sys::Win32::System::Pipes::{DisconnectNamedPipe, PeekNamedPipe};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

use super::{Endpoint, Listener, Stream, is_connection_closed_error};

/// `ERROR_INVALID_HANDLE`, `ERROR_BROKEN_PIPE`, `ERROR_NO_DATA`, `ERROR_PIPE_NOT_CONNECTED`: a peer that is gone.
const PIPE_CLOSED_ERRORS: [i32; 4] = [6, 109, 232, 233];

pub(super) fn bind(endpoint: &Endpoint) -> io::Result<Listener> {
    fs::create_dir_all(endpoint.directory())?;
    let inner = ListenerOptions::new()
        .name(endpoint.pipe_name().to_ns_name::<GenericNamespaced>()?)
        .reclaim_name(false)
        .security_descriptor(owner_only_security_descriptor()?)
        .create_sync()
        .map_err(|error| name_taken_error(endpoint, error))?;
    Ok(Listener { inner })
}

pub(super) fn connect(endpoint: &Endpoint) -> io::Result<Stream> {
    let stream = Stream::connect(endpoint.pipe_name().to_ns_name::<GenericNamespaced>()?)
        .map_err(|error| connection_error(endpoint, error))?;
    check_server_user(&stream, endpoint)?;
    Ok(stream)
}

/// The local-socket wrapper ignores its wait mode on Windows (herdr's note, still so in interprocess 2.4.6), so the
/// pipe is connected through the named-pipe API.
pub(super) fn connect_timeout(endpoint: &Endpoint, timeout: Duration) -> io::Result<Stream> {
    let path = format!(r"\\.\pipe\{}", endpoint.pipe_name());
    let pipe = DuplexPipeStream::<Bytes>::connect_by_path_with_wait_mode(
        path.as_str(),
        interprocess::ConnectWaitMode::Timeout(timeout),
    )
    .map_err(|error| connection_error(endpoint, error))?;
    let stream = Stream::NamedPipe(pipe.into());
    check_server_user(&stream, endpoint)?;
    Ok(stream)
}

/// Disconnect the server's end of the pipe, which closes the client's end and fails every later read or write of this
/// one (interprocess reads `ERROR_PIPE_NOT_CONNECTED` as the end of the stream), then cancel what another thread has
/// pending on it, a read or a write (interprocess issues them overlapped, so they can run side by side).
pub(super) fn disconnect(stream: &Stream) {
    let Stream::NamedPipe(pipe) = stream;
    let handle = pipe.as_handle().as_raw_handle();
    // Failures (a pipe already disconnected, a client's end, nothing pending) leave it as closed as it gets.
    // SAFETY: the handle is the open pipe end `pipe` borrows for the call.
    unsafe { DisconnectNamedPipe(handle) };
    // SAFETY: as above; a null overlapped pointer cancels every operation pending on the handle.
    unsafe { CancelIoEx(handle, std::ptr::null()) };
}

pub(super) fn peer_closed(stream: &mut Stream) -> io::Result<bool> {
    let Stream::NamedPipe(pipe) = stream;
    let mut available = 0_u32;
    // SAFETY: the handle is the open pipe `pipe` borrows; a null buffer of size zero only peeks, and `available`
    // outlives the call.
    let peeked = unsafe {
        PeekNamedPipe(
            pipe.as_handle().as_raw_handle(),
            null_mut(),
            0,
            null_mut(),
            &raw mut available,
            null_mut(),
        )
    };
    if peeked != 0 {
        return Ok(available > 0);
    }
    let error = io::Error::last_os_error();
    if is_connection_closed_error(&error)
        || error
            .raw_os_error()
            .is_some_and(|code| PIPE_CLOSED_ERRORS.contains(&code))
    {
        return Ok(true);
    }
    Err(error)
}

/// The listener's descriptor: full access for SYSTEM and the pipe's owner only (herdr's private listener DACL), and a
/// mandatory label that keeps lower-integrity processes from reading or writing it: medium, or high for an elevated
/// server, so an unelevated client of the same account cannot reach an elevated server.
fn owner_only_security_descriptor() -> io::Result<SecurityDescriptor> {
    let integrity = if current_process_is_elevated()? { "HI" } else { "ME" };
    let sddl = U16CString::from_str(format!("D:P(A;;GA;;;SY)(A;;GA;;;OW)S:(ML;;NRNW;;;{integrity})"))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    SecurityDescriptor::deserialize(&sddl)
}

/// A pipe of another server (or another process squatting the name) makes `FILE_FLAG_FIRST_PIPE_INSTANCE` fail with
/// `ERROR_ACCESS_DENIED`.
fn name_taken_error(endpoint: &Endpoint, error: io::Error) -> io::Error {
    if error.kind() != io::ErrorKind::PermissionDenied {
        return error;
    }
    io::Error::new(
        io::ErrorKind::AddrInUse,
        format!("agterm-session server is already running (pipe busy at {endpoint}): {error}"),
    )
}

/// herdr's `local_server_connection_error`: a denied connection is an integrity mismatch, an elevated server.
fn connection_error(endpoint: &Endpoint, error: io::Error) -> io::Error {
    if error.kind() != io::ErrorKind::PermissionDenied {
        return error;
    }
    io::Error::new(
        error.kind(),
        format!("the agterm-session server at {endpoint} runs elevated; connect from an elevated process: {error}"),
    )
}

/// Refuses a pipe whose server runs as another user: the name is derived, so anyone could create it first.
fn check_server_user(stream: &Stream, endpoint: &Endpoint) -> io::Result<()> {
    let server_pid = stream
        .peer_creds()?
        .pid()
        .ok_or_else(|| io::Error::other("the pipe's server process id is unknown"))?;
    let peer = open_process(server_pid)?;
    let peer_user = token_information(&open_token(peer.as_raw_handle())?, TokenUser)?;
    // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no closing and is always valid.
    let current = unsafe { GetCurrentProcess() };
    let own_user = token_information(&open_token(current)?, TokenUser)?;
    // SAFETY: filled by GetTokenInformation(TokenUser).
    let peer_sid = unsafe { user_sid(&peer_user) };
    // SAFETY: filled by GetTokenInformation(TokenUser).
    let own_sid = unsafe { user_sid(&own_user) };
    // SAFETY: both SIDs point into their buffers, which live until the end of this function.
    let same = unsafe { EqualSid(peer_sid, own_sid) };
    if same != 0 {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("the pipe at {endpoint} belongs to a process of another user (pid {server_pid})"),
    ))
}

fn current_process_is_elevated() -> io::Result<bool> {
    // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no closing and is always valid.
    let current = unsafe { GetCurrentProcess() };
    let buffer = token_information(&open_token(current)?, TokenElevation)?;
    // SAFETY: GetTokenInformation(TokenElevation) filled the aligned buffer with a TOKEN_ELEVATION.
    let elevation = unsafe { &*buffer.as_ptr().cast::<TOKEN_ELEVATION>() };
    Ok(elevation.TokenIsElevated != 0)
}

fn open_process(pid: u32) -> io::Result<OwnedHandle> {
    // SAFETY: OpenProcess takes no pointers; a null result is checked before the handle is owned.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: OpenProcess returned a fresh handle that nothing else owns.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

fn open_token(process: HANDLE) -> io::Result<OwnedHandle> {
    let mut token: HANDLE = null_mut();
    // SAFETY: `process` is a valid process handle for the duration of the call and `token` outlives it.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &raw mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: OpenProcessToken returned a fresh handle that nothing else owns.
    Ok(unsafe { OwnedHandle::from_raw_handle(token) })
}

/// The token's information of `class`, in `usize` storage so the structure at its start is aligned and the SIDs it
/// points to stay alive with it.
fn token_information(token: &OwnedHandle, class: TOKEN_INFORMATION_CLASS) -> io::Result<Vec<usize>> {
    let mut needed = 0_u32;
    // SAFETY: a null buffer of length zero only asks for the size, written to `needed`, which outlives the call.
    unsafe { GetTokenInformation(token.as_raw_handle(), class, null_mut(), 0, &raw mut needed) };
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    let words = usize::try_from(needed)
        .map_err(io::Error::other)?
        .div_ceil(size_of::<usize>());
    let mut buffer = vec![0_usize; words];
    // SAFETY: the buffer holds at least `needed` bytes, and `needed` outlives the call.
    let filled = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            class,
            buffer.as_mut_ptr().cast::<c_void>(),
            needed,
            &raw mut needed,
        )
    };
    if filled == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(buffer)
}

/// The SID of a `TokenUser` buffer.
///
/// # Safety
///
/// `buffer` was filled by `GetTokenInformation(TokenUser)`.
unsafe fn user_sid(buffer: &[usize]) -> *mut c_void {
    // SAFETY: the caller guarantees an aligned TOKEN_USER at the buffer's start.
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    user.User.Sid
}
