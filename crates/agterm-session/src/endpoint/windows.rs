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
use interprocess::local_socket::{GenericNamespaced, ToNsName};
use interprocess::os::windows::named_pipe::{DuplexPipeStream, PipeListenerOptions, pipe_mode::Bytes};
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

/// The listener of a namespace's pipe: interprocess's named-pipe listener, whose accepts make each further instance
/// (without the first-instance flag) from its options.
pub(super) type PipeListener = interprocess::os::windows::named_pipe::PipeListener<Bytes, Bytes>;

/// The options every instance of a namespace's pipe is created with: what interprocess's local-socket listener sets
/// (byte mode both ways, blocking, remote clients refused, its default buffer sizes and instance limit) and the
/// owner-only descriptor.
fn pipe_options(endpoint: &Endpoint) -> io::Result<PipeListenerOptions<'static>> {
    let mut options = PipeListenerOptions::new().path(pipe_path(endpoint).as_str());
    options.security_descriptor = Some(owner_only_security_descriptor()?);
    options.to_owned()
}

fn pipe_path(endpoint: &Endpoint) -> String {
    format!(r"\\.\pipe\{}", endpoint.pipe_name())
}

pub(super) fn bind(endpoint: &Endpoint) -> io::Result<Listener> {
    fs::create_dir_all(endpoint.directory())?;
    let inner = pipe_options(endpoint)?
        .create_duplex::<Bytes>()
        .map_err(|error| name_taken_error(endpoint, error))?;
    Ok(Listener { inner })
}

pub(super) fn bind_beside(endpoint: &Endpoint) -> io::Result<Listener> {
    let options = pipe_options(endpoint)?;
    // a first instance created here means no server holds the pipe: nothing to take over
    if let Ok(first) = create_instance(&options, true) {
        drop(first);
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no agterm-session server holds {endpoint} to take it over from"),
        ));
    }
    let instance = create_instance(&options, false)?;
    Ok(Listener {
        inner: PipeListener::from_handle_and_options(instance, options),
    })
}

/// One instance of the pipe `options` name, as interprocess creates its listener's instances
/// (`named_pipe/listener/create_instance.rs` at interprocess 2.4.6: overlapped, duplex, byte mode, remote clients
/// refused, the options' buffer sizes and default timeout, the instance limit unlimited), with the first-instance
/// flag when `first`.
fn create_instance(options: &PipeListenerOptions<'_>, first: bool) -> io::Result<OwnedHandle> {
    use interprocess::os::windows::security_descriptor::AsSecurityDescriptor as _;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX,
    };
    use windows_sys::Win32::System::Pipes::{
        CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES,
    };

    let mut open_mode = FILE_FLAG_OVERLAPPED | PIPE_ACCESS_DUPLEX;
    if first {
        open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    let pipe_mode = PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_REJECT_REMOTE_CLIENTS;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).map_err(io::Error::other)?,
        lpSecurityDescriptor: options
            .security_descriptor
            .as_ref()
            .map_or(null_mut(), |descriptor| descriptor.as_sd().cast_mut()),
        bInheritHandle: 0,
    };
    // SAFETY: the path is a NUL-terminated wide string the options own, the attributes and the descriptor they point
    // to live across the call; the returned handle is checked before it is owned.
    let handle = unsafe {
        CreateNamedPipeW(
            options.path.as_ptr(),
            open_mode,
            pipe_mode,
            PIPE_UNLIMITED_INSTANCES,
            options.output_buffer_size_hint,
            options.input_buffer_size_hint,
            options.wait_timeout.to_raw(),
            &raw const attributes,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateNamedPipeW returned a fresh handle that nothing else owns.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

pub(super) fn accept(listener: &Listener) -> io::Result<Stream> {
    let pipe = listener.inner.accept()?;
    Ok(Stream::NamedPipe(pipe.into()))
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

/// Wait until the client has read everything written to the pipe, then [`disconnect`] it, which would discard what is
/// unread. The flush is the pipe stream's (`FlushFileBuffers`, skipped when nothing was written since the last one):
/// the local-socket wrapper's is a no-op. It returns once the client has read it all, or with an error once the
/// client's end is closed or the pipe is disconnected (a client gone has nothing left to read).
pub(super) fn close(stream: &Stream) {
    let Stream::NamedPipe(pipe) = stream;
    let _ = pipe.inner().flush();
    disconnect(stream);
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
