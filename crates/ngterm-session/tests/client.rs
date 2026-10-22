//! The client library's attach stream, on every OS: against a scripted peer for the relay both ways, the epochs
//! through a resync, the answers' tokens, the refusals and the stream's ends; and end to end against a real server run
//! in the test process: a session created, typed into, resized, detached from and reattached to through the library,
//! and (on Unix, where the pseudoconsole does not answer the queries itself) a delegated OSC 52 read and the reply
//! metadata, changed mid-stream, answering the program's queries.
//!
//! Every session gets the test's environment without the live ngterm's `NGTERM_*`, `AGTERM_*` and `ZMX_*` variables, in
//! a private namespace.

#[expect(
    dead_code,
    reason = "the helpers serve the pane, lead, snapshot, attach, api, client and host tests alike"
)]
mod support;

#[expect(dead_code, reason = "the helpers serve the client and host tests alike")]
mod peer;

use std::thread;
use std::time::{Duration, Instant};

use ngterm_session::client::{
    AttachEvent, AttachReader, Attachment, Barrier, Client, ClientError, DETACH_WAIT, Delegated,
};
use ngterm_session::protocol::{
    AttachClientMessage, AttachRefusal, AttachRequest, AttachServerMessage, ColorScheme, DelegatedReply,
    DelegatedRequest, ExitStatus, FramingError, GridSize, LeadRole, MAGIC, Managed, ReplyMetadata, Rgb, SessionSpec,
};
use ngterm_session::server::lifecycle::StopReason;
use peer::{Server, Step, attach_peer, next_message};
use support::{PATIENCE, SIZE, TestResult, shell, shown, tagged};

const OTHER: GridSize = GridSize { cols: 100, rows: 40 };

fn request(name: &str, create: Option<SessionSpec>) -> AttachRequest {
    AttachRequest {
        name: name.to_owned(),
        create,
        size: SIZE,
        managed: None,
        metadata: ReplyMetadata::default(),
    }
}

fn begin(epoch: u64, offset: u64) -> Step {
    Step::Send(AttachServerMessage::SnapshotBegin {
        epoch,
        offset,
        size: SIZE,
    })
}

fn chunk(epoch: u64, data: &[u8]) -> Step {
    Step::Send(AttachServerMessage::SnapshotChunk {
        epoch,
        data: data.to_vec(),
    })
}

fn end(epoch: u64) -> Step {
    Step::Send(AttachServerMessage::SnapshotEnd { epoch })
}

fn output(offset: u64, data: &[u8]) -> Step {
    Step::Send(AttachServerMessage::Output {
        offset,
        data: data.to_vec(),
    })
}

fn owner() -> Step {
    Step::Send(AttachServerMessage::ReplyOwner {
        owner: true,
        generation: 1,
    })
}

fn clipboard() -> DelegatedRequest {
    DelegatedRequest::ClipboardRead {
        selection: "c".to_owned(),
        list_only: false,
    }
}

/// The reader's next event, which must come.
fn event(reader: &mut AttachReader) -> TestResult<AttachEvent> {
    reader.recv()?.ok_or_else(|| "the stream ended".into())
}

/// The relay test's server side: the start of a stream, input expected, more output, metadata and a resize expected,
/// the exit.
fn relay_script() -> Vec<Step> {
    vec![
        owner(),
        Step::Send(AttachServerMessage::Role {
            role: LeadRole::Leader,
            generation: 1,
        }),
        begin(1, 10),
        chunk(1, b"snap"),
        end(1),
        output(10, b"hello"),
        Step::Send(AttachServerMessage::Resized {
            offset: 15,
            size: OTHER,
        }),
        Step::Receive,
        output(15, b"echo"),
        Step::Receive,
        Step::Receive,
        Step::Send(AttachServerMessage::Exit {
            status: ExitStatus::Exited { code: 3, signal: None },
        }),
    ]
}

#[test]
fn the_stream_relays_output_input_size_and_metadata_both_ways() -> TestResult {
    let (peer, received) = attach_peer(relay_script())?;
    let sent = request("relay", None);
    let Attachment { mut reader, writer, .. } = Client::new(peer.directory()).attach(sent.clone())?;
    assert_eq!(next_message(&received)?, AttachClientMessage::Attach(sent));
    let events = (0..7).map(|_| event(&mut reader)).collect::<TestResult<Vec<_>>>()?;
    assert_eq!(
        events,
        [
            AttachEvent::ReplyOwner {
                owner: true,
                generation: 1
            },
            AttachEvent::Role {
                role: LeadRole::Leader,
                generation: 1
            },
            AttachEvent::SnapshotBegin {
                epoch: 1,
                offset: 10,
                size: SIZE
            },
            AttachEvent::SnapshotChunk(b"snap".to_vec()),
            AttachEvent::SnapshotEnd,
            AttachEvent::Output {
                offset: 10,
                data: b"hello".to_vec()
            },
            AttachEvent::Resized {
                offset: 15,
                size: OTHER
            }
        ]
    );
    assert_eq!((reader.epoch(), writer.epoch()), (1, 1));
    writer.input(b"typed\r")?;
    assert_eq!(
        next_message(&received)?,
        AttachClientMessage::Input {
            data: b"typed\r".to_vec()
        }
    );
    let echo = AttachEvent::Output {
        offset: 15,
        data: b"echo".to_vec(),
    };
    assert_eq!(event(&mut reader)?, echo);
    let metadata = ReplyMetadata {
        background: Some(Rgb { r: 1, g: 2, b: 3 }),
        color_scheme: Some(ColorScheme::Dark),
        ..ReplyMetadata::default()
    };
    writer.metadata(metadata.clone())?;
    writer.resize(OTHER)?;
    assert_eq!(next_message(&received)?, AttachClientMessage::Metadata(metadata));
    assert_eq!(next_message(&received)?, AttachClientMessage::Resize { size: OTHER });
    assert_eq!(
        event(&mut reader)?,
        AttachEvent::Exit(ExitStatus::Exited { code: 3, signal: None })
    );
    assert_eq!(reader.recv()?, None, "the stream ends after the exit");
    Ok(())
}

#[test]
fn answers_go_back_only_in_the_current_epoch() -> TestResult {
    let (peer, received) = attach_peer(vec![
        owner(),
        begin(1, 0),
        end(1),
        Step::Send(AttachServerMessage::DelegatedRequest {
            id: 5,
            epoch: 1,
            request: clipboard(),
        }),
        Step::Receive,
        Step::Send(AttachServerMessage::KittyBarrier { id: 9, epoch: 1 }),
        // a resync: the barrier above is superseded
        begin(2, 100),
        chunk(2, b"again"),
        end(2),
        output(100, b"after"),
        Step::Receive,
    ])?;
    let Attachment { mut reader, writer, .. } = Client::new(peer.directory()).attach(request("epochs", None))?;
    next_message(&received)?;
    let read = loop {
        if let AttachEvent::Delegated(read) = event(&mut reader)? {
            break read;
        }
    };
    assert_eq!(read.request, clipboard());
    let reply = DelegatedReply::Clipboard {
        text: Some("hi".to_owned()),
    };
    assert!(writer.answer(read, reply.clone())?);
    assert_eq!(
        next_message(&received)?,
        AttachClientMessage::DelegatedReply { id: 5, epoch: 1, reply }
    );
    let AttachEvent::KittyBarrier(barrier) = event(&mut reader)? else {
        return Err("no Kitty barrier".into());
    };
    assert_eq!(barrier, Barrier { id: 9, epoch: 1 });
    let resync = AttachEvent::SnapshotBegin {
        epoch: 2,
        offset: 100,
        size: SIZE,
    };
    assert_eq!(event(&mut reader)?, resync);
    assert_eq!(writer.epoch(), 2);
    assert!(
        !writer.complete(barrier, Vec::new())?,
        "a barrier of a superseded epoch is not completed"
    );
    let stale = Delegated {
        id: 5,
        epoch: 1,
        request: clipboard(),
    };
    assert!(!writer.answer(stale, DelegatedReply::Denied)?);
    writer.detach()?;
    // nothing was sent for the stale answers: the detach comes next
    assert_eq!(next_message(&received)?, AttachClientMessage::Detach);
    assert_eq!(event(&mut reader)?, AttachEvent::SnapshotChunk(b"again".to_vec()));
    assert_eq!(event(&mut reader)?, AttachEvent::SnapshotEnd);
    let after = AttachEvent::Output {
        offset: 100,
        data: b"after".to_vec(),
    };
    assert_eq!(event(&mut reader)?, after);
    assert_eq!(reader.recv()?, None, "the stream ends after the detach");
    Ok(())
}

#[test]
fn a_barrier_of_the_current_epoch_is_completed_even_empty() -> TestResult {
    let (peer, received) = attach_peer(vec![
        owner(),
        begin(1, 0),
        end(1),
        output(0, b"\x1b_Gi=1,a=q;\x1b"),
        Step::Send(AttachServerMessage::KittyBarrier { id: 2, epoch: 1 }),
        Step::Receive,
        output(12, b"\\"),
        Step::Send(AttachServerMessage::KittyBarrier { id: 3, epoch: 1 }),
        Step::Receive,
        Step::Receive,
    ])?;
    let Attachment { mut reader, writer, .. } = Client::new(peer.directory()).attach(request("kitty", None))?;
    next_message(&received)?;
    let next_barrier = |reader: &mut AttachReader| -> TestResult<Barrier> {
        loop {
            if let AttachEvent::KittyBarrier(barrier) = event(reader)? {
                return Ok(barrier);
            }
        }
    };
    // each completed once the output before it was taken, the second with no answer at all
    let answer = b"\x1b_Gi=1;OK\x1b\\".to_vec();
    assert!(writer.complete(next_barrier(&mut reader)?, answer.clone())?);
    assert!(writer.complete(next_barrier(&mut reader)?, Vec::new())?);
    assert_eq!(
        next_message(&received)?,
        AttachClientMessage::KittyComplete {
            id: 2,
            epoch: 1,
            data: answer
        }
    );
    assert_eq!(
        next_message(&received)?,
        AttachClientMessage::KittyComplete {
            id: 3,
            epoch: 1,
            data: Vec::new()
        }
    );
    writer.detach()?;
    assert_eq!(next_message(&received)?, AttachClientMessage::Detach);
    while reader.recv()?.is_some() {}
    Ok(())
}

#[test]
fn a_refused_attach_is_an_error_with_the_servers_reason() -> TestResult {
    let (peer, received) = attach_peer(vec![Step::Send(AttachServerMessage::Refused {
        reason: AttachRefusal::Gone,
        message: "no live session named \"x\"".to_owned(),
    })])?;
    match Client::new(peer.directory()).attach(request("x", None)) {
        Err(ClientError::Refused { reason, message }) => {
            assert_eq!(
                (reason, message.as_str()),
                (AttachRefusal::Gone, "no live session named \"x\"")
            );
        }
        other => return Err(format!("not refused: {:?}", other.map(|attachment| attachment.welcome)).into()),
    }
    assert!(matches!(next_message(&received)?, AttachClientMessage::Attach(_)));
    Ok(())
}

#[test]
fn an_attach_without_a_server_starts_none() -> TestResult {
    let root = tempfile::tempdir()?;
    let directory = root.path().join("live");
    let result = Client::new(&directory).attach(request("x", Some(shell())));
    assert!(matches!(result, Err(ClientError::NoServer(_))), "{:?}", result.err());
    assert!(!directory.exists(), "nothing was started in the namespace");
    Ok(())
}

#[test]
fn a_detach_ends_the_stream_in_order() -> TestResult {
    let (peer, received) = attach_peer(vec![owner(), begin(1, 0), end(1), Step::Receive])?;
    let Attachment { mut reader, writer, .. } = Client::new(peer.directory()).attach(request("detach", None))?;
    while event(&mut reader)? != AttachEvent::SnapshotEnd {}
    writer.detach()?;
    assert_eq!(reader.recv()?, None);
    assert!(matches!(next_message(&received)?, AttachClientMessage::Attach(_)));
    assert_eq!(next_message(&received)?, AttachClientMessage::Detach);
    Ok(())
}

#[test]
fn a_connection_that_ends_before_the_stream_is_a_disconnect() -> TestResult {
    let (peer, _received) = attach_peer(vec![owner(), begin(1, 0), end(1), output(0, b"cut")])?;
    let mut attachment = Client::new(peer.directory()).attach(request("cut", None))?;
    let mut events = Vec::new();
    let error = loop {
        match attachment.reader.recv() {
            Ok(Some(event)) => events.push(event),
            Ok(None) => return Err("the stream ended in order".into()),
            Err(error) => break error,
        }
    };
    assert!(matches!(error, ClientError::Disconnected), "{error}");
    assert_eq!(events.len(), 4, "{events:?}");
    Ok(())
}

#[test]
fn output_off_its_offset_breaks_the_stream() -> TestResult {
    let (peer, _received) = attach_peer(vec![owner(), begin(1, 0), end(1), output(5, b"gap"), Step::Receive])?;
    let mut attachment = Client::new(peer.directory()).attach(request("gap", None))?;
    let error = loop {
        match attachment.reader.recv() {
            Ok(Some(_)) => {}
            Ok(None) => return Err("the stream ended in order".into()),
            Err(error) => break error,
        }
    };
    assert!(
        matches!(&error, ClientError::Protocol(what) if what.contains("at 5")),
        "{error}"
    );
    assert_eq!(attachment.reader.recv()?, None, "nothing is read past a broken stream");
    attachment.writer.close();
    Ok(())
}

#[test]
fn closing_wakes_a_blocked_reader() -> TestResult {
    let (peer, _received) = attach_peer(vec![owner(), begin(1, 0), end(1), Step::Receive])?;
    let Attachment { mut reader, writer, .. } = Client::new(peer.directory()).attach(request("close", None))?;
    let reading = thread::spawn(move || {
        let mut events = 0;
        loop {
            match reader.recv() {
                Ok(Some(_)) => events += 1,
                Ok(None) => return Ok(events),
                Err(error) => return Err(error.to_string()),
            }
        }
    });
    let deadline = Instant::now() + PATIENCE;
    while !reading.is_finished() {
        assert!(Instant::now() < deadline, "the reader was never woken");
        writer.close();
        thread::sleep(Duration::from_millis(20));
    }
    let read = reading.join().map_err(|_| "the reader panicked")?;
    assert!(
        matches!(read, Ok(events) if events <= 3),
        "the stream ended in order: {read:?}"
    );
    Ok(())
}

/// The reader's error after the exit the peer sends before `trailing`.
fn error_after_the_exit(trailing: Vec<u8>) -> TestResult<ClientError> {
    let exit = Step::Send(AttachServerMessage::Exit {
        status: ExitStatus::Exited { code: 0, signal: None },
    });
    let (peer, _received) = attach_peer(vec![owner(), begin(1, 0), end(1), exit, Step::Raw(trailing)])?;
    let mut attachment = Client::new(peer.directory()).attach(request("trailing", None))?;
    while !matches!(event(&mut attachment.reader)?, AttachEvent::Exit(_)) {}
    let result = attachment.reader.recv();
    attachment.writer.close();
    match result {
        Err(error) => Ok(error),
        Ok(event) => Err(format!("the stream went on after the exit: {event:?}").into()),
    }
}

#[test]
fn only_an_orderly_close_ends_the_stream_after_the_exit() -> TestResult {
    let malformed = error_after_the_exit(b"NOPE\0\0\0\0\0\0\0\0\0\0\0\0".to_vec())?;
    assert!(
        matches!(malformed, ClientError::Framing(FramingError::BadMagic(_))),
        "{malformed}"
    );
    let cut = error_after_the_exit(MAGIC[..3].to_vec())?;
    assert!(
        matches!(cut, ClientError::Framing(FramingError::UnexpectedEof)),
        "{cut}"
    );
    Ok(())
}

#[test]
fn a_detach_behind_a_write_a_stopped_server_never_reads_closes_the_connection() -> TestResult {
    let (peer, received) = attach_peer(vec![owner(), begin(1, 0), end(1), Step::Hold(PATIENCE)])?;
    let Attachment { mut reader, writer, .. } = Client::new(peer.directory()).attach(request("stuck", None))?;
    next_message(&received)?;
    while event(&mut reader)? != AttachEvent::SnapshotEnd {}
    // far more than a socket's or a pipe's buffer: the write waits for a reader that never comes
    let typing = writer.clone();
    let input = thread::spawn(move || typing.input(&vec![b'x'; 64 * 1024 * 1024]));
    thread::sleep(Duration::from_millis(300));
    assert!(!input.is_finished(), "the write was taken: {:?}", input.join());

    let started = Instant::now();
    let detached = writer.detach();
    assert!(detached.is_err(), "a detach that could not be sent is an error");
    let waited = started.elapsed();
    assert!(
        waited >= DETACH_WAIT && waited < PATIENCE,
        "the detach waited {waited:?} for the write"
    );
    // the close wakes the write (on Windows only one pending when it comes)
    let deadline = Instant::now() + PATIENCE;
    while !input.is_finished() {
        assert!(Instant::now() < deadline, "the write was never woken");
        writer.close();
        thread::sleep(Duration::from_millis(20));
    }
    assert!(input.join().map_err(|_| "the writer panicked")?.is_err());
    assert!(
        matches!(writer.input(b"late"), Err(ClientError::Disconnected)),
        "a closed connection takes no writes"
    );
    assert_eq!(reader.recv()?, None, "the reader's stream ended with the close");
    Ok(())
}

// ---------------------------------------------------------------------------------------------------------------------
// Against a real server
// ---------------------------------------------------------------------------------------------------------------------

/// The text a terminal shows of what the reader sends until `done` holds for it, and of the snapshots on the way.
fn read_until(reader: &mut AttachReader, done: impl Fn(&str) -> bool) -> TestResult<String> {
    let mut bytes = Vec::new();
    while !done(&shown(&bytes)) {
        match reader.recv()? {
            Some(AttachEvent::Output { data, .. } | AttachEvent::SnapshotChunk(data)) => bytes.extend_from_slice(&data),
            Some(AttachEvent::Exit(status)) => return Err(format!("the session ended: {status:?}").into()),
            Some(_) => {}
            None => return Err(format!("the stream ended: {:?}", shown(&bytes)).into()),
        }
    }
    Ok(shown(&bytes))
}

fn enter() -> &'static str {
    if cfg!(windows) { "\r" } else { "\n" }
}

#[test]
fn a_session_is_created_typed_into_resized_and_reattached_through_the_library() -> TestResult {
    let server = Server::start()?;
    let client = Client::new(server.directory());
    let managed = Managed {
        nonce: "nonce".to_owned(),
        claim: true,
    };
    let mut first = client.attach(AttachRequest {
        managed: Some(managed),
        ..request("library", Some(shell()))
    })?;
    assert_eq!(
        event(&mut first.reader)?,
        AttachEvent::Role {
            role: LeadRole::Leader,
            generation: 1
        }
    );
    first
        .writer
        .input(format!("{}{}", tagged("relayed"), enter()).as_bytes())?;
    read_until(&mut first.reader, |text| text.contains("got:relayed"))?;
    first.writer.resize(OTHER)?;
    loop {
        match event(&mut first.reader)? {
            AttachEvent::Resized { size, .. } => {
                assert_eq!(size, OTHER);
                break;
            }
            AttachEvent::Output { .. } => {}
            other => return Err(format!("no resize before {other:?}").into()),
        }
    }
    first.writer.detach()?;
    while first.reader.recv()?.is_some() {}

    // the reattach leads the session the leader left unowned, at its own size, and starts from a snapshot that shows
    // what was typed
    let mut second = client.attach(request("library", None))?;
    let size = loop {
        if let AttachEvent::SnapshotBegin { size, epoch, .. } = event(&mut second.reader)? {
            assert_eq!(epoch, 1);
            break size;
        }
    };
    assert_eq!(size, SIZE);
    read_until(&mut second.reader, |text| text.contains("got:relayed"))?;
    let quit = if cfg!(windows) { "exit\r" } else { "\x04" };
    second.writer.input(quit.as_bytes())?;
    loop {
        match second.reader.recv()? {
            Some(AttachEvent::Exit(_)) => break,
            Some(_) => {}
            None => return Err("the stream ended without the exit".into()),
        }
    }
    assert_eq!(second.reader.recv()?, None);
    drop((first, second));
    assert_eq!(server.stopped()?, StopReason::Idle);
    Ok(())
}

#[cfg(unix)]
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

/// The hex the program printed after `label:`.
#[cfg(unix)]
fn printed<'a>(text: &'a str, label: &str) -> &'a str {
    let Some((_, rest)) = text.split_once(&format!("{label}:")) else {
        return "";
    };
    let end = rest.find(|c: char| !c.is_ascii_hexdigit()).unwrap_or(rest.len());
    &rest[..end]
}

/// A session's program reads its clipboard and asks its background colour and the colour scheme, before and after
/// the reply owner changes its metadata: the read is delegated to the library's client and answered through it, and
/// the server answers the queries with the metadata the client sent, as it stands when each query is parsed.
#[cfg(unix)]
#[test]
fn a_delegated_clipboard_read_and_the_reply_metadata_go_end_to_end() -> TestResult {
    let server = Server::start()?;
    let program = "stty raw -echo min 0 time 50; \
                   ask() { printf \"$1\"; r=$(dd bs=256 count=1 2>/dev/null | od -An -tx1 | tr -d ' \\n'); \
                   printf '%s:%s\\r\\n' \"$2\" \"$r\"; }; \
                   ask '\\033]11;?\\007' bg1; ask '\\033[?996n' scheme1; ask '\\033]52;c;?\\007' clip; \
                   ask '\\033]11;?\\007' bg2; ask '\\033[?996n' scheme2; stty sane";
    let first = ReplyMetadata {
        background: Some(Rgb {
            r: 0x12,
            g: 0x34,
            b: 0x56,
        }),
        color_scheme: Some(ColorScheme::Light),
        ..ReplyMetadata::default()
    };
    let changed = ReplyMetadata {
        background: Some(Rgb {
            r: 0xfe,
            g: 0xdc,
            b: 0xba,
        }),
        color_scheme: Some(ColorScheme::Dark),
        ..ReplyMetadata::default()
    };
    let Attachment { mut reader, writer, .. } = Client::new(server.directory()).attach(AttachRequest {
        metadata: first,
        ..request("delegate", Some(support::script(program)))
    })?;
    let mut bytes = Vec::new();
    let mut owned = false;
    let mut delegated = 0;
    loop {
        match reader.recv()? {
            Some(AttachEvent::ReplyOwner { owner, .. }) => owned = owner,
            Some(AttachEvent::Output { data, .. }) => bytes.extend_from_slice(&data),
            Some(AttachEvent::Delegated(read)) => {
                assert_eq!(read.request, clipboard());
                assert!(
                    shown(&bytes).contains("scheme1:"),
                    "the output before the read came first"
                );
                delegated += 1;
                // the metadata first: the queries after the read are answered with it
                writer.metadata(changed.clone())?;
                let reply = DelegatedReply::Clipboard {
                    text: Some("hi".to_owned()),
                };
                assert!(writer.answer(read, reply)?);
            }
            Some(AttachEvent::Exit(_)) => break,
            Some(_) => {}
            None => return Err("the stream ended without the exit".into()),
        }
    }
    assert_eq!(reader.recv()?, None);
    assert!(owned, "the creator owns the replies");
    assert_eq!(delegated, 1);
    let text = shown(&bytes);
    assert!(printed(&text, "bg1").contains(&hex(b"rgb:1212/3434/5656")), "{text:?}");
    assert_eq!(printed(&text, "scheme1"), hex(b"\x1b[?997;2n"), "{text:?}");
    // `ESC ] 5 2 ; c ; aGk= BEL`
    assert_eq!(printed(&text, "clip"), hex(b"\x1b]52;c;aGk=\x07"), "{text:?}");
    assert!(printed(&text, "bg2").contains(&hex(b"rgb:fefe/dcdc/baba")), "{text:?}");
    assert_eq!(printed(&text, "scheme2"), hex(b"\x1b[?997;1n"), "{text:?}");
    drop(writer);
    assert_eq!(server.stopped()?, StopReason::Idle);
    Ok(())
}
