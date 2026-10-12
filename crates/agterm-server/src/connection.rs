//! One control connection: read one request line, hop to the main loop for the answer, write it back, and hand the
//! connection to a stream owner when the reply claims it. Upstream's `handleConnection`, `readLine` and
//! `writeResponse` (`agterm/Control/ControlServer.swift`).

use std::any::Any;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use agterm_core::{ControlError, decode_request};
use agterm_host::{MainLoop, MainTask};
use agterm_proto::{MAX_REQUEST_LINE_BYTES, Request, Response};

use crate::stream::Reply;

/// How long a connection may take to deliver its request line and to take its response, as upstream's
/// `readTimeoutSeconds`, `readDeadlineSeconds`, `writeTimeoutSeconds` and `writeDeadlineSeconds`.
///
/// A per-call timeout bounds one `read` or `write` only, so a client trickling a byte per interval, or draining a
/// large response a little at a time, never trips it; the overall deadline caps the whole line. Each call waits at
/// most the smaller of its timeout and what is left of the deadline. A zero duration fails at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionDeadlines {
    /// One `read` of the request line (`SO_RCVTIMEO`): 5 s.
    pub read_timeout: Duration,
    /// The whole request line: 10 s.
    pub read_deadline: Duration,
    /// One `write` of the response (`SO_SNDTIMEO`): 5 s.
    pub write_timeout: Duration,
    /// The whole response: 10 s.
    pub write_deadline: Duration,
}

impl Default for ConnectionDeadlines {
    fn default() -> Self {
        Self {
            read_timeout: Duration::from_secs(5),
            read_deadline: Duration::from_secs(10),
            write_timeout: Duration::from_secs(5),
            write_deadline: Duration::from_secs(10),
        }
    }
}

/// The most one `write` call sends, a fraction of the default `AF_UNIX` send buffer (see [`write_all_by`]).
const WRITE_CHUNK_BYTES: usize = 64 * 1024;

/// What is left until `deadline`, capped at `timeout`; `None` once nothing is.
fn next_wait(deadline: Instant, timeout: Duration) -> Option<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .map(|left| left.min(timeout))
        .filter(|wait| !wait.is_zero())
}

fn deadline_passed() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "connection deadline passed")
}

/// The request handler, run on the main thread with the state it owns.
pub(crate) type Handler<S> = dyn Fn(&mut S, Request) -> Reply + Send + Sync;

/// The route from a connection thread to the main thread and back.
pub(crate) struct Hop<S> {
    pub(crate) main_loop: Arc<dyn MainLoop<S>>,
    pub(crate) handler: Arc<Handler<S>>,
    pub(crate) deadlines: ConnectionDeadlines,
}

impl<S: 'static> Hop<S> {
    /// Runs the handler on the main thread and waits for its reply.
    fn call(&self, request: Request) -> Reply {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let handler = Arc::clone(&self.handler);
        let task: MainTask<S> = Box::new(move |state: &mut S| {
            let reply = run_guarded(handler.as_ref(), state, request);
            if reply_tx.send(reply).is_err() {
                tracing::debug!("control connection gone before its reply");
            }
        });
        if self.main_loop.post(task).is_err() {
            return ControlError::ShuttingDown.into();
        }
        // a task dropped unrun by a shutting-down loop drops the sender with it
        reply_rx.recv().unwrap_or_else(|_| ControlError::ShuttingDown.into())
    }
}

/// Runs the handler, turning a panic into `ok:false` plus a log entry so it never takes the main loop down.
fn run_guarded<S>(handler: &Handler<S>, state: &mut S, request: Request) -> Reply {
    let cmd = request.cmd;
    panic::catch_unwind(AssertUnwindSafe(|| handler(state, request))).unwrap_or_else(|payload| {
        tracing::error!(%cmd, panic = panic_message(payload.as_ref()), "control handler panicked");
        ControlError::HandlerPanicked(cmd).into()
    })
}

fn panic_message(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload")
}

/// Serves one accepted connection to completion (or until a stream owner takes it). A line that cannot be read is
/// answered too, before the connection closes.
pub(crate) fn serve<S: 'static>(stream: UnixStream, hop: &Hop<S>) {
    let mut reader = BufReader::new(stream);
    let reply = match read_line(&mut reader, &hop.deadlines) {
        Ok(line) => match decode_request(&line) {
            Ok(request) => hop.call(request),
            Err(error) => error.into(),
        },
        Err(error) => {
            tracing::debug!(%error, "control connection: request not read");
            ControlError::UnreadableRequest.into()
        }
    };
    // bytes after the newline the buffered read already took belong to a stream owner
    let pending = reader.buffer().to_vec();
    deliver(reader.into_inner(), reply, pending, &hop.deadlines);
}

/// Reads the first line, newline excluded. A line the client ends by closing its side without a newline counts.
///
/// # Errors
///
/// EOF before any byte, a read error or timeout, the overall read deadline, or a line over
/// [`MAX_REQUEST_LINE_BYTES`].
fn read_line(reader: &mut BufReader<UnixStream>, deadlines: &ConnectionDeadlines) -> io::Result<Vec<u8>> {
    let deadline = Instant::now() + deadlines.read_deadline;
    let mut line = Vec::new();
    loop {
        let wait = next_wait(deadline, deadlines.read_timeout).ok_or_else(deadline_passed)?;
        reader.get_ref().set_read_timeout(Some(wait))?;
        let chunk = match reader.fill_buf() {
            Ok(chunk) => chunk,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if chunk.is_empty() {
            return if line.is_empty() {
                Err(io::ErrorKind::UnexpectedEof.into())
            } else {
                Ok(line)
            };
        }
        let newline = chunk.iter().position(|&byte| byte == b'\n');
        let end = newline.unwrap_or(chunk.len());
        line.extend_from_slice(chunk.get(..end).unwrap_or_default());
        reader.consume(newline.map_or(end, |at| at + 1));
        if line.len() > MAX_REQUEST_LINE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("request line over {MAX_REQUEST_LINE_BYTES} bytes"),
            ));
        }
        if newline.is_some() {
            return Ok(line);
        }
    }
}

/// Writes the response; on a written `ok`, a claiming owner takes the connection, otherwise it is told it never will.
fn deliver(mut stream: UnixStream, reply: Reply, pending: Vec<u8>, deadlines: &ConnectionDeadlines) {
    let Reply {
        response,
        stream: owner,
    } = reply;
    let written = write_response(&mut stream, &response, deadlines);
    let Some(owner) = owner else {
        return;
    };
    if response.ok && written && clear_timeouts(&stream) {
        owner.adopt(stream, pending);
    } else {
        owner.abandon();
    }
}

/// An adopted stream starts with no timeouts; its owner sets its own.
fn clear_timeouts(stream: &UnixStream) -> bool {
    match stream
        .set_read_timeout(None)
        .and_then(|()| stream.set_write_timeout(None))
    {
        Ok(()) => true,
        Err(error) => {
            tracing::debug!(%error, "control connection: cannot clear the timeouts before the handoff");
            false
        }
    }
}

fn write_response(stream: &mut UnixStream, response: &Response, deadlines: &ConnectionDeadlines) -> bool {
    let mut bytes = match serde_json::to_vec(response) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::error!(%error, "control response not encodable");
            return false;
        }
    };
    bytes.push(b'\n');
    match write_all_by(stream, &bytes, deadlines) {
        Ok(()) => true,
        Err(error) => {
            tracing::debug!(%error, "control response not written");
            false
        }
    }
}

/// Writes all of `bytes` within the write deadline.
///
/// Linux applies `SO_SNDTIMEO` to each wait for socket buffer space, not to the whole `write`, so one `write` of a
/// large response to a client draining it slowly would run past any deadline. Writing at most [`WRITE_CHUNK_BYTES`]
/// per call lets the deadline be checked between chunks, as upstream checks it between `write` calls.
fn write_all_by(stream: &mut UnixStream, mut bytes: &[u8], deadlines: &ConnectionDeadlines) -> io::Result<()> {
    let deadline = Instant::now() + deadlines.write_deadline;
    while !bytes.is_empty() {
        let wait = next_wait(deadline, deadlines.write_timeout).ok_or_else(deadline_passed)?;
        stream.set_write_timeout(Some(wait))?;
        let chunk = bytes.get(..WRITE_CHUNK_BYTES).unwrap_or(bytes);
        match stream.write(chunk) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(written) => bytes = bytes.get(written..).unwrap_or_default(),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
