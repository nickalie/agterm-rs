//! A blocking, one-request-per-connection client for the control socket, ported from `SocketClient.swift`: connect,
//! write the request line, read the single response line, decode it.

use std::fmt;
use std::io::{self, ErrorKind, Read};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::Path;

use agterm_proto::{ControlResult, ControlTree, MAX_REQUEST_LINE_BYTES, Request, Response};

use crate::socket;

/// `sizeof(sockaddr_un.sun_path)` on Linux: a path must be shorter, its NUL included.
const SUN_PATH_CAPACITY: usize = 108;
/// Cap on a response line. A `session.text --all` response carries the whole scrollback and can reach several MiB;
/// the cap only guards against a runaway read.
const MAX_LINE_BYTES: usize = 64 << 20;
const READ_CHUNK: usize = 64 * 1024;

/// A failure talking to the socket (connect, write, read, decode), as opposed to a server-side `{"ok":false}`, which
/// is a valid [`Reply`]. Its text is what follows `Error: ` on stderr.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientError(pub String);

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A decoded response beside the bytes it arrived as. `--json` prints `raw` unchanged, so a field this build does
/// not model still reaches the caller.
#[derive(Debug, Clone)]
pub struct Reply {
    pub response: Response,
    /// The response line without its trailing newline.
    pub raw: Vec<u8>,
}

impl Reply {
    /// The line `--json` prints.
    #[must_use]
    pub fn line(&self) -> String {
        String::from_utf8_lossy(&self.raw).into_owned()
    }
}

/// The OS's own text for an I/O error (`strerror`), without the `(os error N)` suffix std appends.
fn os_message(error: &io::Error) -> String {
    let text = error.to_string();
    match (error.raw_os_error(), text.rfind(" (os error ")) {
        (Some(_), Some(at)) => text[..at].to_owned(),
        _ => text,
    }
}

/// The sentence after a failed connect. A refusal and a missing socket are the two the ownership lock narrows, and
/// only to an owner being there: the server keeps the lock after a failed bind, so a held lock never says how the
/// socket came to be unreachable.
fn connect_hint(error: &io::Error, path: &Path) -> &'static str {
    if !matches!(error.kind(), ErrorKind::ConnectionRefused | ErrorKind::NotFound) {
        return "is agterm running?";
    }
    if socket::ownership_lock_held(path) == Some(true) {
        "the socket owner is present but not accepting connections"
    } else {
        "agterm may be stopped or unable to accept connections"
    }
}

/// Connects to `path`. The length check comes first, as upstream's, so an over-long path reports itself rather than
/// a generic connect failure.
pub fn connect(path: &Path) -> Result<UnixStream, ClientError> {
    let len = path.as_os_str().as_bytes().len();
    if len >= SUN_PATH_CAPACITY {
        return Err(ClientError(format!(
            "socket path too long ({len} bytes): {}",
            path.display()
        )));
    }
    UnixStream::connect(path).map_err(|error| {
        ClientError(format!(
            "connect({}) failed: {} — {}",
            path.display(),
            os_message(&error),
            connect_hint(&error, path)
        ))
    })
}

/// Reads up to (and excluding) the first newline. `None` on EOF before any byte, a read error, or a line over
/// [`MAX_LINE_BYTES`]; a final line without a newline still counts.
fn read_line(stream: &mut impl Read) -> Option<Vec<u8>> {
    let mut line = Vec::new();
    let mut chunk = vec![0; READ_CHUNK];
    loop {
        let n = match stream.read(&mut chunk) {
            Ok(0) => return (!line.is_empty()).then_some(line),
            Ok(n) => n,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return None,
        };
        if let Some(end) = chunk[..n].iter().position(|&byte| byte == b'\n') {
            line.extend_from_slice(&chunk[..end]);
            return Some(line);
        }
        line.extend_from_slice(&chunk[..n]);
        if line.len() > MAX_LINE_BYTES {
            return None;
        }
    }
}

/// Writes all of `data` to `stream` with `MSG_NOSIGNAL`, looping over short writes: a peer that hung up yields
/// `EPIPE` rather than SIGPIPE, whatever the process's SIGPIPE disposition (upstream's `SO_NOSIGPIPE`).
fn send_all(stream: &UnixStream, mut data: &[u8]) -> io::Result<()> {
    let fd = stream.as_raw_fd();
    while !data.is_empty() {
        let (buf, len) = (data.as_ptr().cast::<libc::c_void>(), data.len());
        // SAFETY: `fd` is the stream's open socket, borrowed for the call; `buf` and `len` describe the live slice
        // `data`, which `send` only reads.
        let sent = unsafe { libc::send(fd, buf, len, libc::MSG_NOSIGNAL) };
        match usize::try_from(sent) {
            Ok(0) => return Err(io::Error::from(ErrorKind::WriteZero)),
            Ok(n) => data = &data[n..],
            Err(_) => {
                let error = io::Error::last_os_error();
                if error.kind() != ErrorKind::Interrupted {
                    return Err(error);
                }
            }
        }
    }
    Ok(())
}

/// Sends `request` as one newline-terminated JSON line to `path` and decodes the one response line.
pub fn send(path: &Path, request: &Request) -> Result<Reply, ClientError> {
    let mut data =
        serde_json::to_vec(request).map_err(|error| ClientError(format!("could not encode request: {error}")))?;
    // the server rejects a line over the shared cap and closes the connection: say so before writing instead of
    // failing the write against the closing peer
    if data.len() > MAX_REQUEST_LINE_BYTES {
        return Err(ClientError(format!(
            "request too large ({} bytes, cap {MAX_REQUEST_LINE_BYTES}) — split the input into smaller requests",
            data.len()
        )));
    }
    data.push(b'\n');
    let mut stream = connect(path)?;
    // main restores SIGPIPE's default action for stdout, so the request goes out with MSG_NOSIGNAL: a write to a
    // hung-up peer (one that rejected the line) is an EPIPE error here, not a silent death
    send_all(&stream, &data).map_err(|error| ClientError(format!("write failed: {}", os_message(&error))))?;
    let raw = read_line(&mut stream).ok_or_else(|| ClientError(format!("no response from {}", path.display())))?;
    let response = decode_response(&raw).map_err(|error| ClientError(format!("could not decode response: {error}")))?;
    Ok(Reply { response, raw })
}

/// Decodes a response line as upstream's `JSONDecoder().decode(ControlResponse.self, …)`: a result field this build
/// models typed must decode as its type when present (a field it does not model is ignored), so a malformed one fails
/// the whole reply, `--json` included, instead of reading as absent.
fn decode_response(raw: &[u8]) -> serde_json::Result<Response> {
    let response: Response = serde_json::from_slice(raw)?;
    let Some(result) = response.result.as_ref().filter(|result| !result.is_null()) else {
        return Ok(response);
    };
    // the shared result object: `id`, `windows`, `affected` and the fields later commands add to it
    serde_json::from_value::<ControlResult>(result.clone())?;
    if let Some(tree) = result.get("tree").filter(|tree| !tree.is_null()) {
        serde_json::from_value::<ControlTree>(tree.clone())?;
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn a_modelled_result_field_that_does_not_decode_fails_the_reply() {
        let tree =
            br#"{"ok":true,"result":{"tree":{"workspaces":[{"id":"W","name":"w","active":true,"sessions":[]}]}}}"#;
        assert!(decode_response(tree).is_ok());
        assert!(decode_response(br#"{"ok":true,"result":{"tree":null,"other":5}}"#).is_ok());
        assert!(decode_response(br#"{"ok":true,"result":{"tree":{"workspaces":5}}}"#).is_err());
        let unflagged = br#"{"ok":true,"result":{"tree":{"workspaces":[{"id":"W","name":"w","active":true,
            "sessions":[{"id":"S","name":"s","cwd":"/","active":true,"scratch":false}]}]}}}"#;
        assert!(
            decode_response(unflagged).is_err(),
            "a session node without its required flagged"
        );
    }

    #[test]
    fn a_result_field_of_the_wrong_type_fails_the_reply() {
        let windows = br#"{"ok":true,"result":{"windows":[{"id":"W","name":"w","open":true,"active":false}]}}"#;
        assert!(decode_response(windows).is_ok());
        assert!(decode_response(br#"{"ok":true,"result":{"id":"S","affected":2,"unmodelled":[1]}}"#).is_ok());
        assert!(decode_response(br#"{"ok":true,"result":null}"#).is_ok());
        for malformed in [
            &br#"{"ok":true,"result":{"windows":[{"id":"W","name":"w"}]}}"#[..],
            br#"{"ok":true,"result":{"windows":{}}}"#,
            br#"{"ok":true,"result":{"affected":"two"}}"#,
            br#"{"ok":true,"result":{"id":5}}"#,
        ] {
            assert!(
                decode_response(malformed).is_err(),
                "{}",
                String::from_utf8_lossy(malformed)
            );
        }
    }

    #[test]
    fn os_message_drops_the_errno_suffix() {
        let error = io::Error::from_raw_os_error(2);
        assert_eq!(os_message(&error), "No such file or directory");
        assert_eq!(os_message(&io::Error::other("plain")), "plain");
    }

    #[test]
    fn read_line_stops_at_the_first_newline() {
        let mut input = Cursor::new(b"{\"ok\":true}\nrest".to_vec());
        assert_eq!(read_line(&mut input), Some(b"{\"ok\":true}".to_vec()));
    }

    #[test]
    fn read_line_takes_a_final_line_without_newline_and_refuses_nothing() {
        assert_eq!(read_line(&mut Cursor::new(b"abc".to_vec())), Some(b"abc".to_vec()));
        assert_eq!(read_line(&mut Cursor::new(Vec::new())), None);
    }

    #[test]
    fn read_line_caps_a_runaway_line() {
        let mut input = Cursor::new(vec![b'x'; MAX_LINE_BYTES + READ_CHUNK + 1]);
        assert_eq!(read_line(&mut input), None);
    }

    /// SIGPIPE in this thread's pending set. A blocked signal is queued even while its action is "ignore", so a
    /// write that raised it shows up here.
    fn sigpipe_pending() -> bool {
        // SAFETY: an all-zero `sigset_t` is a valid (empty) set on Linux, which `sigpending` then overwrites.
        let mut pending: libc::sigset_t = unsafe { std::mem::zeroed() };
        // SAFETY: `pending` is a valid, writable `sigset_t`.
        assert_eq!(unsafe { libc::sigpending(&raw mut pending) }, 0);
        // SAFETY: `pending` was filled by `sigpending`.
        (unsafe { libc::sigismember(&raw const pending, libc::SIGPIPE) }) == 1
    }

    #[test]
    fn a_hung_up_peer_is_an_error_without_sigpipe() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        drop(theirs);
        // SAFETY: an all-zero `sigset_t` is a valid (empty) set on Linux.
        let mut block: libc::sigset_t = unsafe { std::mem::zeroed() };
        // SAFETY: `block` is a valid set and SIGPIPE a valid signal.
        assert_eq!(unsafe { libc::sigaddset(&raw mut block, libc::SIGPIPE) }, 0);
        // SAFETY: changes only this test thread's mask; a null old set is allowed.
        let blocked = unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &raw const block, std::ptr::null_mut()) };
        assert_eq!(blocked, 0);
        let result = send_all(&ours, b"{\"cmd\":\"version\"}\n");
        let raised = sigpipe_pending();
        // SAFETY: as above; a SIGPIPE left pending is discarded on unblock, its action being "ignore" under libtest.
        let unblocked = unsafe { libc::pthread_sigmask(libc::SIG_UNBLOCK, &raw const block, std::ptr::null_mut()) };
        assert_eq!(unblocked, 0);
        assert_eq!(result.unwrap_err().kind(), ErrorKind::BrokenPipe);
        assert!(!raised, "the write raised SIGPIPE");
    }

    #[test]
    fn an_over_long_path_is_refused_before_connecting() {
        let path = format!("/tmp/{}", "x".repeat(SUN_PATH_CAPACITY - 5));
        let error = connect(Path::new(&path)).unwrap_err();
        assert_eq!(
            error.0,
            format!("socket path too long ({SUN_PATH_CAPACITY} bytes): {path}")
        );
    }
}
