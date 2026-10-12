//! One control connection: read one request line, hop to the main loop for the answer, write it back, and hand the
//! connection to a stream owner when the reply claims it.

use std::any::Any;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use agterm_core::{ControlError, decode_request};
use agterm_host::{MainLoop, MainTask};
use agterm_proto::{MAX_REQUEST_LINE_BYTES, Request, Response};

use crate::stream::Reply;

/// How long a connection may sit idle before its request line is complete, as the fork's per-read poll.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// The request handler, run on the main thread with the state it owns.
pub(crate) type Handler<S> = dyn Fn(&mut S, Request) -> Reply + Send + Sync;

/// The route from a connection thread to the main thread and back.
pub(crate) struct Hop<S> {
    pub(crate) main_loop: Arc<dyn MainLoop<S>>,
    pub(crate) handler: Arc<Handler<S>>,
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

/// Serves one accepted connection to completion (or until a stream owner takes it).
pub(crate) fn serve<S: 'static>(stream: UnixStream, hop: &Hop<S>) {
    let Some(request) = read_request(stream) else {
        return;
    };
    let reply = match decode_request(&request.line) {
        Ok(decoded) => hop.call(decoded),
        Err(error) => error.into(),
    };
    deliver(request.stream, reply, request.pending);
}

/// A complete request line and the connection it came on.
struct RequestLine {
    line: Vec<u8>,
    /// Bytes after the newline the buffered read already took; they belong to a stream owner.
    pending: Vec<u8>,
    stream: UnixStream,
}

/// Reads the first line. `None` closes the connection without a reply: an idle timeout, a read error, an empty
/// connection, or a line over [`MAX_REQUEST_LINE_BYTES`].
fn read_request(stream: UnixStream) -> Option<RequestLine> {
    if let Err(error) = stream.set_read_timeout(Some(READ_TIMEOUT)) {
        tracing::debug!(%error, "control connection: cannot set the read timeout");
        return None;
    }
    let mut reader = BufReader::new(stream);
    let mut line = Vec::new();
    // one byte over the cap, so a line of exactly the cap still finds its newline
    let limit = u64::try_from(MAX_REQUEST_LINE_BYTES).map_or(u64::MAX, |cap| cap + 1);
    match (&mut reader).take(limit).read_until(b'\n', &mut line) {
        Ok(0) => return None,
        Ok(_) => {}
        Err(error) => {
            tracing::debug!(%error, "control connection: request not read");
            return None;
        }
    }
    if line.last() == Some(&b'\n') {
        line.pop();
    } else if line.len() > MAX_REQUEST_LINE_BYTES {
        tracing::warn!("control connection: request line over {MAX_REQUEST_LINE_BYTES} bytes, closing");
        return None;
    }
    let pending = reader.buffer().to_vec();
    let stream = reader.into_inner();
    if let Err(error) = stream.set_read_timeout(None) {
        tracing::debug!(%error, "control connection: cannot clear the read timeout");
    }
    Some(RequestLine { line, pending, stream })
}

/// Writes the response; on a written `ok`, a claiming owner takes the connection, otherwise it is told it never will.
fn deliver(mut stream: UnixStream, reply: Reply, pending: Vec<u8>) {
    let Reply {
        response,
        stream: owner,
    } = reply;
    let written = write_response(&mut stream, &response);
    let Some(owner) = owner else {
        return;
    };
    if response.ok && written {
        owner.adopt(stream, pending);
    } else {
        owner.abandon();
    }
}

fn write_response(stream: &mut UnixStream, response: &Response) -> bool {
    let mut bytes = match serde_json::to_vec(response) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::error!(%error, "control response not encodable");
            return false;
        }
    };
    bytes.push(b'\n');
    match stream.write_all(&bytes).and_then(|()| stream.flush()) {
        Ok(()) => true,
        Err(error) => {
            tracing::debug!(%error, "control response not written");
            false
        }
    }
}
