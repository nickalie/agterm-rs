//! Bridges a streaming control command between a pair of descriptors and the app's socket (upstream's
//! `agtermctlKit/StreamBridge.swift`): the request goes out, its one ordinary reply comes back, and after an `ok`
//! everything is copied both ways untouched.
//!
//! `agtermctl live present` runs it on the origin under the viewer's ssh, so the output is what the viewer parses:
//! only frames are ever written to it.

use std::fs::File;
use std::io::{self, ErrorKind, Read, Write};
use std::net::Shutdown;
use std::os::fd::{AsRawFd, BorrowedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::thread;

use agterm_proto::{MAX_REQUEST_LINE_BYTES, Request, Response};

use crate::client;

/// How much one read takes.
const CHUNK_BYTES: usize = 16 * 1024;

/// The socket to the app, and the descriptors the other side's frames come in on and go out to.
#[derive(Debug)]
pub struct StreamBridge {
    socket: UnixStream,
    input: RawFd,
    output: RawFd,
}

impl StreamBridge {
    /// A bridge over `socket`, reading the other side from `input` and writing it to `output`. The descriptors stay
    /// the caller's: the bridge works on duplicates.
    #[must_use]
    pub fn new(socket: UnixStream, input: RawFd, output: RawFd) -> Self {
        Self { socket, input, output }
    }

    /// Sends `request` and reads its reply, having written nothing to the output either way.
    ///
    /// # Errors
    ///
    /// The server's refusal, or why no reply came.
    pub fn open(&self, request: &Request) -> Result<(), String> {
        let mut line = serde_json::to_vec(request).map_err(|error| format!("could not encode the request: {error}"))?;
        line.push(b'\n');
        client::send_all(&self.socket, &line).map_err(|_| "could not send the request".to_owned())?;
        // a byte at a time: a frame can sit right behind the reply in the same packet
        let reply = client::read_socket_line(&self.socket, MAX_REQUEST_LINE_BYTES)
            .ok_or_else(|| "the app closed the connection without answering".to_owned())?;
        let response: Response =
            serde_json::from_slice(&reply).map_err(|error| format!("could not decode the reply: {error}"))?;
        if response.ok {
            Ok(())
        } else {
            Err(response
                .error
                .unwrap_or_else(|| "the app refused the stream".to_owned()))
        }
    }

    /// Copies both ways until either side ends. The input ending shuts the socket down, which is how the app learns
    /// the viewer went away; the socket ending returns. Both directions have stopped by the time this returns.
    pub fn pump(&self) {
        let (Ok(input), Ok(output)) = (duplicate(self.input), duplicate(self.output)) else {
            return;
        };
        let Ok((cancel, wake)) = io::pipe() else {
            return;
        };
        let socket = &self.socket;
        // the reader stays here, open until both directions stopped: the input side may finish first, and a wake
        // written into a pipe with no reader left raises SIGPIPE, which main restored to its default
        let cancel_fd = cancel.as_raw_fd();
        thread::scope(|scope| {
            let upstream = thread::Builder::new()
                .name("agtermctl.stream.upstream".to_owned())
                .spawn_scoped(scope, move || copy_until_cancelled(input, socket, cancel_fd));
            copy(socket, output);
            let mut wake = wake;
            let _ = wake.write_all(&[1]);
            if let Ok(upstream) = upstream {
                let _ = upstream.join();
            }
        });
    }
}

/// A duplicate of `fd`, owned, so closing it leaves the caller's descriptor alone.
fn duplicate(fd: RawFd) -> io::Result<File> {
    // SAFETY: the caller hands a descriptor that stays open for the bridge's whole life (stdio, or a test's pipe);
    // it is only borrowed long enough to duplicate it.
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    borrowed.try_clone_to_owned().map(File::from)
}

/// Copies `source` to the socket until `source` ends, which shuts the socket down, or `cancel` becomes readable, which
/// just stops.
fn copy_until_cancelled(mut source: File, socket: &UnixStream, cancel: RawFd) {
    let mut chunk = vec![0_u8; CHUNK_BYTES];
    loop {
        match wait_readable(source.as_raw_fd(), cancel) {
            Readiness::Interrupted => continue,
            Readiness::Cancelled | Readiness::Failed => return,
            Readiness::Source => {}
        }
        let count = match source.read(&mut chunk) {
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Ok(count) => count,
            Err(_) => 0,
        };
        if count == 0 || client::send_all(socket, &chunk[..count]).is_err() {
            let _ = socket.shutdown(Shutdown::Both);
            return;
        }
    }
}

/// Copies the socket to `destination` until either ends.
fn copy(mut socket: &UnixStream, mut destination: File) {
    let mut chunk = vec![0_u8; CHUNK_BYTES];
    loop {
        let count = match socket.read(&mut chunk) {
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Ok(count) => count,
            Err(_) => 0,
        };
        if count == 0 || destination.write_all(&chunk[..count]).is_err() {
            return;
        }
    }
}

enum Readiness {
    Source,
    Cancelled,
    Interrupted,
    Failed,
}

/// Blocks until `source` or `cancel` can be read; the cancel wins a tie.
fn wait_readable(source: RawFd, cancel: RawFd) -> Readiness {
    let mut pollers = [
        libc::pollfd {
            fd: source,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: cancel,
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    // SAFETY: `pollers` is a live array of two initialized `pollfd`s, and the count passed is its length.
    let ready = unsafe { libc::poll(pollers.as_mut_ptr(), 2, -1) };
    if ready < 0 {
        return if io::Error::last_os_error().kind() == ErrorKind::Interrupted {
            Readiness::Interrupted
        } else {
            Readiness::Failed
        };
    }
    if pollers[1].revents != 0 {
        Readiness::Cancelled
    } else {
        Readiness::Source
    }
}

#[cfg(test)]
mod tests {
    use std::io::BufRead;
    use std::time::Duration;

    use agterm_proto::Command;
    use agterm_proto::command::LiveCmd;

    use super::*;

    fn present() -> Request {
        Request {
            cmd: Command::Live(LiveCmd::Present),
            target: Some("S".to_owned()),
            args: None,
        }
    }

    /// A bridge between a socket pair and two pipes: the app's end, the viewer's writer and the viewer's reader.
    struct Rig {
        app: UnixStream,
        viewer_in: io::PipeWriter,
        viewer_out: io::BufReader<io::PipeReader>,
        bridge: StreamBridge,
        _ends: (io::PipeReader, io::PipeWriter),
    }

    fn rig() -> Rig {
        let (bridge_side, app) = UnixStream::pair().unwrap();
        let (input_reader, viewer_in) = io::pipe().unwrap();
        let (viewer_out, output_writer) = io::pipe().unwrap();
        let bridge = StreamBridge::new(bridge_side, input_reader.as_raw_fd(), output_writer.as_raw_fd());
        Rig {
            app,
            viewer_in,
            viewer_out: io::BufReader::new(viewer_out),
            bridge,
            _ends: (input_reader, output_writer),
        }
    }

    fn app_line(app: &UnixStream) -> String {
        let mut line = String::new();
        io::BufReader::new(app).read_line(&mut line).unwrap();
        line
    }

    #[test]
    fn the_request_goes_first_and_frames_then_pass_both_ways_unmodified() {
        let Rig {
            app,
            mut viewer_in,
            mut viewer_out,
            bridge,
            _ends,
        } = rig();
        let app_side = thread::spawn(move || {
            let request = app_line(&app);
            // the first frame rides right behind the reply in one write
            client::send_all(&app, b"{\"ok\":true,\"result\":{\"id\":\"S\"}}\n{\"kind\":\"hello\"}\n").unwrap();
            let from_viewer = app_line(&app);
            (request, from_viewer)
        });
        bridge.open(&present()).unwrap();
        let pumping = thread::spawn(move || bridge.pump());
        let mut frame = String::new();
        viewer_out.read_line(&mut frame).unwrap();
        assert_eq!(frame, "{\"kind\":\"hello\"}\n");
        viewer_in.write_all(b"{\"kind\":\"ack\"}\n").unwrap();
        let (request, from_viewer) = app_side.join().unwrap();
        let sent: Request = serde_json::from_str(request.trim_end()).unwrap();
        assert_eq!(sent, present());
        assert_eq!(from_viewer, "{\"kind\":\"ack\"}\n");
        // the app hanging up ends the bridge even though stdin stays open
        pumping.join().unwrap();
        drop(viewer_in);
    }

    #[test]
    fn stdin_closing_ends_the_bridge_and_the_app_sees_end_of_stream() {
        let Rig {
            app,
            viewer_in,
            viewer_out: _viewer_out,
            bridge,
            _ends,
        } = rig();
        client::send_all(&app, b"{\"ok\":true}\n").unwrap();
        bridge.open(&present()).unwrap();
        let pumping = thread::spawn(move || bridge.pump());
        drop(viewer_in);
        app.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut rest = Vec::new();
        (&app).read_to_end(&mut rest).unwrap();
        assert!(
            String::from_utf8_lossy(&rest).contains("live.present"),
            "the request, then end of stream"
        );
        drop(app);
        pumping.join().unwrap();
    }

    #[test]
    fn the_viewer_leaving_first_ends_the_bridge_without_sigpipe() {
        // with SIGPIPE at its default, as the real binary runs: the socket side ending after the input did wakes the
        // finished input side's cancel pipe, which must still have its reader. Repeated, as which side of the bridge
        // notices the end first is up to the scheduler.
        crate::local::default_sigpipe::run(
            "stream_bridge::tests::the_viewer_leaving_first_ends_the_bridge_without_sigpipe",
            || {
                for _ in 0..200 {
                    let Rig {
                        app,
                        viewer_in,
                        viewer_out: _viewer_out,
                        bridge,
                        _ends,
                    } = rig();
                    client::send_all(&app, b"{\"ok\":true}\n").unwrap();
                    bridge.open(&present()).unwrap();
                    drop(viewer_in);
                    bridge.pump();
                }
            },
        );
    }

    #[test]
    fn a_refusal_or_a_silent_close_is_an_error_and_writes_nothing() {
        let Rig {
            app,
            viewer_out,
            bridge,
            _ends: ends,
            ..
        } = rig();
        client::send_all(&app, b"{\"ok\":false,\"error\":\"no such session: S\"}\n").unwrap();
        assert_eq!(bridge.open(&present()), Err("no such session: S".to_owned()));
        drop(bridge);
        drop(ends);
        let mut written = Vec::new();
        viewer_out.into_inner().read_to_end(&mut written).unwrap();
        assert!(written.is_empty());
        let Rig { app, bridge, .. } = rig();
        // takes the request, then hangs up without answering
        app.shutdown(Shutdown::Write).unwrap();
        assert_eq!(
            bridge.open(&present()),
            Err("the app closed the connection without answering".to_owned())
        );
    }
}
