//! The stream handoff seam: a handler's reply may claim the connection once its response is written, for commands
//! that keep streaming after the first `ok` (`events.read --follow`, `zmx.present`, `session.overlay.job.run`).

use std::fmt;
use std::os::unix::net::UnixStream;

use agterm_core::ControlError;
use agterm_proto::Response;

/// Takes over a connection after its `ok` response was written.
pub trait StreamOwner: Send {
    /// Owns `stream` from here on, on the connection's own thread; it may block for the stream's whole life and closes
    /// the stream by dropping it. `pending` holds bytes the client sent after the request line that the server already
    /// read.
    fn adopt(self: Box<Self>, stream: UnixStream, pending: Vec<u8>);

    /// The connection never reached the owner: the response was a failure or could not be written. The default does
    /// nothing; an owner holding a claim (an overlay job, a presentation) releases it here.
    fn abandon(self: Box<Self>) {}
}

/// A handler's answer: the response, plus an optional owner for the connection afterwards.
pub struct Reply {
    pub response: Response,
    pub stream: Option<Box<dyn StreamOwner>>,
}

impl Reply {
    /// A response after which `owner` takes the connection, provided the response is `ok` and was written.
    #[must_use]
    pub fn streaming(response: Response, owner: Box<dyn StreamOwner>) -> Self {
        Self {
            response,
            stream: Some(owner),
        }
    }
}

impl From<Response> for Reply {
    fn from(response: Response) -> Self {
        Self { response, stream: None }
    }
}

impl From<ControlError> for Reply {
    fn from(error: ControlError) -> Self {
        Response::from(error).into()
    }
}

impl fmt::Debug for Reply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Reply")
            .field("response", &self.response)
            .field("streams", &self.stream.is_some())
            .finish()
    }
}
