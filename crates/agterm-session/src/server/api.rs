//! The api role: what `SessionHost` and `ngtermctl clipboard set` ask of the server, request by request on one
//! connection ([`ApiCall`] in, [`ApiReply`] with the same id out), answered in order until the client closes it.
//!
//! herdr: `src/api/server.rs` (`handle_connection_with_stop`: a connection's request read, dispatched and answered,
//! a client gone mid-answer being no error; `handle_request`: one request to one typed result or error) at `HERDR_REV`
//! (`build/pins.env`), licensed under the Apache License 2.0, modified: postcard calls by id on a connection that stays
//! open in place of herdr's one JSON line per connection; agterm's operations in place of herdr's methods, done on the
//! connection's own thread against the pane registry instead of being dispatched to an app loop; a reply too large for
//! a message answered as the request's error.
//!
//! # Operations
//!
//! Exactly the ones Technical Details > API of the plan names (zmx's `list`, `kill`, `screen`, `type` and `print`,
//! with upstream's sweep), so `SessionHost` keeps its semantics over them:
//!
//! - [`ApiRequest::List`]: every session of the namespace, sorted by name, each with its attached clients, its
//!   program's pid (`None` once the program ended, in the moment before the registry forgets it) and its creation time.
//! - [`ApiRequest::Kill`]: each named session ended, a batch in parallel, one [`KillResult`] per name in the request's
//!   order. Without `force` its program's process group is hung up (on Windows its job ended); with `force`, the
//!   hangup, then a kill of the group and of every other process of its session ([`Pane::kill`]). [`KillOutcome::Killed`]
//!   only once the program's end is recorded; then the foreground job its shell had left is hung up as well, read
//!   before the kill (upstream's `ProcessSweeper`, which `ZmxClient` ran on the client side). A name with no running
//!   session is [`KillOutcome::Gone`], which `SessionHost` reports as `StaleSocket`; a program that outlived the
//!   hangup, or a signal that failed, is [`KillOutcome::Failed`] with the reason.
//! - [`ApiRequest::Screen`]: the session's own terminal, which always has the leader's layout, read with its revision
//!   under the pane's lock ([`Pane::observe`]): its grid, the cursor in the active area, whether the alternate screen
//!   is active, and the text, plain, of the active area or (`all`) of the scrollback and the active area, as zmx's
//!   `serializeScreenText` reads it: soft-wrapped rows joined, trailing blanks and blank lines trimmed.
//! - [`ApiRequest::Type`]: the bytes as keystrokes, not a paste ([`encode_typed`], zmx's `encodeTyped`): each CR a
//!   Return press and release, every run between them text a key produced, encoded for the keyboard mode the program
//!   asked for (the Kitty protocol's `CSI 13 u` where a shell gets a bare CR), and queued as the program's input
//!   without taking the lead ([`Pane::write`]). Done means queued, not read by the program; a full input queue takes
//!   nothing, and the request fails.
//! - [`ApiRequest::Print`]: the bytes written into the session's output as if its program printed them
//!   ([`Pane::print`]): `clipboard set`'s OSC 52 write reaches every attached client in order with the output.
//! - [`ApiRequest::ForegroundJob`], [`ApiRequest::JobRunning`], [`ApiRequest::EndJob`]: the job rules of
//!   [`crate::job`] over this host's process table, for a caller that waits for a restarted pane's old job to end.
//!
//! # Errors
//!
//! [`ApiErrorKind::NoSuchSession`] for a screen, type or print of a name with no running session (none was there, or
//! its program ended), with `no live session named "<name>"`; [`ApiErrorKind::Failed`] for one that ran but failed,
//! and for a reply too large for a message ([`crate::protocol::MAX_MESSAGE_SIZE`]: an unlimited scrollback can hold
//! more), with the reason. A message that is not an [`ApiCall`] closes the connection.

use std::io::Write as _;
use std::thread;

use agterm_ghostty_vt as vt;
use agterm_host::ProcessRecord;

use super::Shared;
use super::pane::{Pane, Panes, PrintError};
use crate::endpoint::{self, Stream};
use crate::job::{Signal, Sweeper};
use crate::protocol::{
    ApiCall, ApiError, ApiErrorKind, ApiReply, ApiRequest, ApiResponse, FramingError, JobProcess, KillOutcome,
    KillResult, PROTOCOL_VERSION, Screen, SessionRecord, encode_message, read_frame, write_message,
};

/// The layout of a screen read (zmx's `serializeScreenText`: Ghostty's plain options, wraps joined, trimmed).
const SCREEN_TEXT: vt::FormatOptions = vt::FormatOptions {
    format: vt::Format::Plain,
    unwrap: true,
    trim: true,
    extras: vt::FormatExtras::empty(),
};

/// An api connection after its welcome: its calls answered in order until the client closes it.
pub(super) fn serve(stream: &Stream, shared: &Shared) -> Result<(), FramingError> {
    loop {
        let frame = match read_frame(&mut &*stream) {
            Ok(frame) => frame,
            Err(FramingError::Closed) => return Ok(()),
            Err(FramingError::Io(err)) if endpoint::is_connection_closed_error(&err) => return Ok(()),
            Err(err) => return Err(err),
        };
        let call = frame.message::<ApiCall>(PROTOCOL_VERSION)?;
        let reply = ApiReply {
            id: call.id,
            result: handle(&call.request, &shared.panes, &shared.jobs),
        };
        match send(stream, &reply) {
            Ok(()) => {}
            // the client went while it was answered: nothing more to do
            Err(FramingError::Io(err)) if endpoint::is_connection_closed_error(&err) => return Ok(()),
            Err(err) => return Err(err),
        }
    }
}

/// `reply`'s frames, or, for one too large for a message, its request's error in its place.
fn send(stream: &Stream, reply: &ApiReply) -> Result<(), FramingError> {
    let frames = match encode_message(reply) {
        Ok(frames) => frames,
        Err(FramingError::Oversized { claimed, max }) => {
            let refusal = ApiReply {
                id: reply.id,
                result: Err(failed(format!(
                    "the reply of {claimed} bytes exceeds the largest message the session server sends ({max} bytes)"
                ))),
            };
            return write_message(&mut &*stream, &refusal);
        }
        Err(err) => return Err(err),
    };
    let mut writer = stream;
    writer.write_all(&frames)?;
    writer.flush()?;
    Ok(())
}

/// The answer to `request` over the sessions `panes` holds and the process table `jobs` reads.
///
/// # Errors
///
/// The request's [`ApiError`] (module docs).
pub fn handle(request: &ApiRequest, panes: &Panes, jobs: &Sweeper) -> Result<ApiResponse, ApiError> {
    match request {
        ApiRequest::List => Ok(ApiResponse::Sessions(list(panes))),
        ApiRequest::Kill { names, force } => Ok(ApiResponse::Killed(kill(panes, names, *force, jobs))),
        ApiRequest::Screen { name, all } => screen(&*running(panes, name)?, name, *all).map(ApiResponse::Screen),
        ApiRequest::Type { name, data } => type_input(&*running(panes, name)?, name, data).map(|()| ApiResponse::Done),
        ApiRequest::Print { name, data } => print(&*running(panes, name)?, name, data).map(|()| ApiResponse::Done),
        ApiRequest::ForegroundJob { shell } => Ok(ApiResponse::ForegroundJob(
            jobs.foreground_job(*shell)
                .map(|job| job.into_iter().map(JobProcess::from).collect()),
        )),
        ApiRequest::JobRunning { job } => Ok(ApiResponse::JobRunning(jobs.is_running(&records(job)))),
        ApiRequest::EndJob { job } => {
            jobs.send(Signal::Kill, &records(job));
            Ok(ApiResponse::Done)
        }
    }
}

fn records(job: &[JobProcess]) -> Vec<ProcessRecord> {
    job.iter().copied().map(ProcessRecord::from).collect()
}

fn no_such_session(name: &str) -> ApiError {
    ApiError {
        kind: ApiErrorKind::NoSuchSession,
        message: format!("no live session named {name:?}"),
    }
}

fn failed(message: String) -> ApiError {
    ApiError {
        kind: ApiErrorKind::Failed,
        message,
    }
}

/// The session `name` while its program runs.
fn running(panes: &Panes, name: &str) -> Result<std::sync::Arc<Pane>, ApiError> {
    panes
        .get(name)
        .filter(|pane| pane.exit_status().is_none())
        .ok_or_else(|| no_such_session(name))
}

fn list(panes: &Panes) -> Vec<SessionRecord> {
    panes
        .names()
        .into_iter()
        .filter_map(|name| panes.get(&name))
        .map(|pane| SessionRecord {
            name: pane.name().to_owned(),
            clients: u32::try_from(pane.clients()).unwrap_or(u32::MAX),
            leader_pid: pane
                .exit_status()
                .is_none()
                .then(|| i32::try_from(pane.pid()).ok())
                .flatten(),
            created_at: pane.created_at(),
        })
        .collect()
}

/// Every name of `names` killed, in parallel; their outcomes in the same order.
fn kill(panes: &Panes, names: &[String], force: bool, jobs: &Sweeper) -> Vec<KillResult> {
    let outcomes: Vec<KillOutcome> = thread::scope(|scope| {
        let kills: Vec<_> = names
            .iter()
            .map(|name| {
                thread::Builder::new()
                    .name("agterm-session-kill".to_owned())
                    .spawn_scoped(scope, move || kill_one(panes, name, force, jobs))
            })
            .collect();
        kills
            .into_iter()
            .zip(names)
            .map(|(kill, name)| match kill {
                Ok(kill) => kill
                    .join()
                    .unwrap_or_else(|_| KillOutcome::Failed(format!("killing the live session {name:?} failed"))),
                // no thread for it: killed here, after the others started
                Err(_) => kill_one(panes, name, force, jobs),
            })
            .collect()
    });
    names
        .iter()
        .zip(outcomes)
        .map(|(name, outcome)| KillResult {
            name: name.clone(),
            outcome,
        })
        .collect()
}

/// The session `name` killed ([`Pane::kill`]); once it ended, the foreground job its shell left hung up.
fn kill_one(panes: &Panes, name: &str, force: bool, jobs: &Sweeper) -> KillOutcome {
    let Ok(pane) = running(panes, name) else {
        return KillOutcome::Gone;
    };
    // read before the kill: the shell's terminal names its foreground group only while the shell runs
    let job = i32::try_from(pane.pid())
        .ok()
        .and_then(|shell| jobs.foreground_job(shell))
        .unwrap_or_default();
    match pane.kill(force) {
        Ok(true) => {
            jobs.send(Signal::HangUp, &job);
            KillOutcome::Killed
        }
        Ok(false) => KillOutcome::Failed(format!(
            "the program of the live session {name:?} did not end after its hangup"
        )),
        Err(err) => KillOutcome::Failed(format!("the live session {name:?} could not be killed: {err}")),
    }
}

/// The session's own terminal as [`Screen`] (module docs).
fn screen(pane: &Pane, name: &str, all: bool) -> Result<Screen, ApiError> {
    pane.observe(|terminal, revision| read_screen(terminal, revision, all))
        .ok_or_else(|| no_such_session(name))?
        .map_err(|err| {
            failed(format!(
                "the screen of the live session {name:?} could not be read: {err}"
            ))
        })
}

/// `terminal`'s screen at `revision` (module docs): its grid, cursor and alternate screen, and the text of its active
/// area, or of its scrollback and active area when `all` (zmx's `handleScreen` over `serializeScreenText`).
///
/// # Errors
///
/// The library's result when the read fails.
pub fn read_screen(terminal: &mut vt::Terminal, revision: u64, all: bool) -> Result<Screen, vt::Error> {
    let (cols, rows) = (terminal.cols(), terminal.rows());
    let region = if all {
        vt::Region::Screen
    } else {
        vt::Region::Range {
            start: vt::Point::new(vt::PointTag::Active, 0, 0),
            end: vt::Point::new(
                vt::PointTag::Active,
                cols.saturating_sub(1),
                u32::from(rows.saturating_sub(1)),
            ),
            rectangle: false,
        }
    };
    let text = terminal.format(region, SCREEN_TEXT)?.unwrap_or_default();
    let (cursor_column, cursor_row) = terminal.cursor();
    Ok(Screen {
        revision,
        columns: u32::from(cols),
        rows: u32::from(rows),
        cursor_column: u32::from(cursor_column),
        cursor_row: u32::from(cursor_row),
        alternate: terminal.alternate_screen(),
        text: String::from_utf8_lossy(&text).into_owned(),
    })
}

/// `data` typed into the session (module docs).
fn type_input(pane: &Pane, name: &str, data: &[u8]) -> Result<(), ApiError> {
    let keys = pane
        .observe(|terminal, _| encode_typed(terminal, data))
        .ok_or_else(|| no_such_session(name))?
        .map_err(|err| {
            failed(format!(
                "the input for the live session {name:?} could not be encoded: {err}"
            ))
        })?;
    if keys.is_empty() {
        return Err(failed(format!("nothing to type into the live session {name:?}")));
    }
    pane.write(&keys).map_err(|err| match err {
        agterm_pty::WriteError::Backpressure => failed(format!(
            "the input queue of the live session {name:?} is full; nothing was typed"
        )),
        // the PTY takes no more input: the session is ending
        agterm_pty::WriteError::Closed => no_such_session(name),
    })
}

fn print(pane: &Pane, name: &str, data: &[u8]) -> Result<(), ApiError> {
    pane.print(data).map_err(|err| match err {
        PrintError::Ended => no_such_session(name),
        PrintError::InsideSequence => failed(format!(
            "the output of the live session {name:?} stands inside an escape sequence; nothing was printed"
        )),
    })
}

/// The bytes a terminal sends for `typed` as keystrokes rather than a paste (zmx's `encodeTyped`, `src/util.zig` of
/// its patch `0001-explicit-leadership`): each CR a Return key press and release, every run between them text a key
/// produced, encoded against `terminal`'s live keyboard mode. The encoder takes no control characters as text
/// (`key/event.h`), so a C0 control or DEL inside a run, or a byte that is not UTF-8, goes as it is.
///
/// # Errors
///
/// The library's result when encoding fails.
pub fn encode_typed(terminal: &mut vt::Terminal, typed: &[u8]) -> Result<Vec<u8>, vt::Error> {
    let mut encoder = vt::KeyEncoder::new()?;
    let mut keys = Vec::with_capacity(typed.len());
    let mut runs = typed.split(|&byte| byte == b'\r').peekable();
    while let Some(run) = runs.next() {
        for chunk in run.utf8_chunks() {
            encode_text(&mut encoder, terminal, chunk.valid(), &mut keys)?;
            keys.extend_from_slice(chunk.invalid());
        }
        if runs.peek().is_some() {
            for action in [vt::KeyAction::Press, vt::KeyAction::Release] {
                let enter = vt::KeyEvent {
                    action,
                    key: vt::Key::ENTER,
                    ..vt::KeyEvent::default()
                };
                keys.extend(encoder.encode(terminal, &enter)?);
            }
        }
    }
    Ok(keys)
}

/// `text` as the keys that typed it: its runs of printable characters through the encoder, its control characters
/// as they are.
fn encode_text(
    encoder: &mut vt::KeyEncoder,
    terminal: &mut vt::Terminal,
    text: &str,
    keys: &mut Vec<u8>,
) -> Result<(), vt::Error> {
    let mut rest = text;
    while !rest.is_empty() {
        let printable = rest.find(|c: char| c.is_ascii_control()).unwrap_or(rest.len());
        let (run, after) = rest.split_at(printable);
        if !run.is_empty() {
            let typed = vt::KeyEvent {
                text: run,
                ..vt::KeyEvent::default()
            };
            keys.extend(encoder.encode(terminal, &typed)?);
        }
        let controls = after.find(|c: char| !c.is_ascii_control()).unwrap_or(after.len());
        keys.extend_from_slice(&after.as_bytes()[..controls]);
        rest = &after[controls..];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, PoisonError};

    use super::*;
    use crate::job::ProcessEntry;

    fn terminal() -> vt::Terminal {
        vt::Terminal::new(80, 24).unwrap()
    }

    #[test]
    fn typing_in_the_legacy_mode_is_the_text_itself() {
        let mut terminal = terminal();
        let keys = encode_typed(&mut terminal, "ls -la\r\rcaf\u{e9}".as_bytes()).unwrap();
        assert_eq!(keys, "ls -la\r\rcaf\u{e9}".as_bytes());
    }

    #[test]
    fn a_program_in_the_kitty_protocol_gets_return_as_a_key_with_its_release() {
        let mut terminal = terminal();
        // every Kitty keyboard flag
        terminal.vt_write(b"\x1b[>31u");
        let keys = encode_typed(&mut terminal, b"ls\r").unwrap();
        let keys = String::from_utf8(keys).unwrap();
        assert!(keys.starts_with("ls"), "{keys:?}");
        assert!(keys.contains("\x1b[13u"), "{keys:?}");
        assert!(keys.contains("\x1b[13;1:3u"), "{keys:?}");
        assert!(!keys.contains('\r'), "{keys:?}");
    }

    #[test]
    fn control_characters_and_bytes_that_are_not_utf8_go_as_they_are() {
        let mut terminal = terminal();
        let keys = encode_typed(&mut terminal, b"a\tb\x1b[A\xffc\r").unwrap();
        assert_eq!(keys, b"a\tb\x1b[A\xffc\r");
        assert_eq!(encode_typed(&mut terminal, b"").unwrap(), b"");
    }

    #[test]
    fn a_screen_reads_its_grid_cursor_and_text_with_wraps_joined() {
        let mut terminal = vt::Terminal::new(20, 5).unwrap();
        let blank = read_screen(&mut terminal, 0, false).unwrap();
        assert_eq!(
            (blank.columns, blank.rows, blank.cursor_column, blank.cursor_row),
            (20, 5, 0, 0)
        );
        assert_eq!(blank.text, "", "a blank screen reads as empty, never a guess");
        // a line longer than the grid wraps on screen and reads back as one line (zmx's case)
        terminal.vt_write(b"0123456789abcdefghijKLMNO\r\n> ab");
        let screen = read_screen(&mut terminal, 9, false).unwrap();
        assert_eq!(
            screen,
            Screen {
                revision: 9,
                columns: 20,
                rows: 5,
                cursor_column: 4,
                cursor_row: 2,
                alternate: false,
                text: "0123456789abcdefghijKLMNO\n> ab".to_owned(),
            }
        );
    }

    #[test]
    fn all_reads_the_scrollback_before_the_screen() {
        let mut terminal = vt::Terminal::new(20, 3).unwrap();
        terminal.vt_write(b"one\r\ntwo\r\nthree\r\nfour  \r\nfive");
        assert_eq!(read_screen(&mut terminal, 1, false).unwrap().text, "three\nfour\nfive");
        assert_eq!(
            read_screen(&mut terminal, 1, true).unwrap().text,
            "one\ntwo\nthree\nfour\nfive"
        );
        terminal.vt_write(b"\x1b[?1049h\x1b[2;3Hfull");
        let alternate = read_screen(&mut terminal, 2, false).unwrap();
        assert!(alternate.alternate);
        assert_eq!((alternate.cursor_column, alternate.cursor_row), (6, 1));
        assert_eq!(alternate.text, "\n  full");
    }

    /// A sweeper over a fixed table, recording what it signals.
    fn sweeper(table: Option<Vec<ProcessEntry>>, sent: &Arc<Mutex<Vec<(Signal, i32)>>>) -> Sweeper {
        let sent = Arc::clone(sent);
        Sweeper::new(
            Box::new(move || table.clone()),
            Box::new(move |signal, record| {
                sent.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push((signal, record.pid));
            }),
        )
    }

    fn entry(pid: i32, group: i32, foreground: i32, parent: i32) -> ProcessEntry {
        ProcessEntry {
            record: ProcessRecord {
                pid,
                started: 100,
                group,
                foreground,
            },
            parent,
        }
    }

    #[test]
    fn requests_for_a_session_that_is_not_there() {
        let panes = Panes::new();
        let sent = Arc::default();
        let jobs = sweeper(Some(Vec::new()), &sent);
        assert_eq!(
            handle(&ApiRequest::List, &panes, &jobs),
            Ok(ApiResponse::Sessions(Vec::new()))
        );
        let gone = ApiError {
            kind: ApiErrorKind::NoSuchSession,
            message: "no live session named \"missing\"".to_owned(),
        };
        for request in [
            ApiRequest::Screen {
                name: "missing".to_owned(),
                all: false,
            },
            ApiRequest::Type {
                name: "missing".to_owned(),
                data: b"x".to_vec(),
            },
            ApiRequest::Print {
                name: "missing".to_owned(),
                data: b"x".to_vec(),
            },
        ] {
            assert_eq!(handle(&request, &panes, &jobs), Err(gone.clone()), "{request:?}");
        }
        let kill = ApiRequest::Kill {
            names: vec!["a".to_owned(), "b".to_owned()],
            force: true,
        };
        assert_eq!(
            handle(&kill, &panes, &jobs),
            Ok(ApiResponse::Killed(vec![
                KillResult {
                    name: "a".to_owned(),
                    outcome: KillOutcome::Gone,
                },
                KillResult {
                    name: "b".to_owned(),
                    outcome: KillOutcome::Gone,
                },
            ]))
        );
        assert!(sent.lock().unwrap_or_else(PoisonError::into_inner).is_empty());
    }

    #[test]
    fn the_job_requests_read_and_signal_the_process_table() {
        // the shell 10 in group 10, its job 11 and 12 in the terminal's foreground group 11 (on Windows, its
        // descendants)
        let table = vec![entry(10, 10, 11, 1), entry(11, 11, 11, 10), entry(12, 11, 11, 11)];
        let sent = Arc::default();
        let jobs = sweeper(Some(table), &sent);
        let panes = Panes::new();
        let Ok(ApiResponse::ForegroundJob(Some(job))) = handle(&ApiRequest::ForegroundJob { shell: 10 }, &panes, &jobs)
        else {
            panic!("no job");
        };
        assert_eq!(job.iter().map(|process| process.pid).collect::<Vec<_>>(), [11, 12]);
        assert_eq!(
            handle(&ApiRequest::JobRunning { job: job.clone() }, &panes, &jobs),
            Ok(ApiResponse::JobRunning(true))
        );
        assert_eq!(
            handle(&ApiRequest::EndJob { job }, &panes, &jobs),
            Ok(ApiResponse::Done)
        );
        assert_eq!(
            *sent.lock().unwrap_or_else(PoisonError::into_inner),
            [(Signal::Kill, 11), (Signal::Kill, 12)]
        );
    }

    #[test]
    fn an_unreadable_process_table_is_no_job_known_and_counts_as_running() {
        let sent = Arc::default();
        let jobs = sweeper(None, &sent);
        let panes = Panes::new();
        assert_eq!(
            handle(&ApiRequest::ForegroundJob { shell: 10 }, &panes, &jobs),
            Ok(ApiResponse::ForegroundJob(None))
        );
        let job = vec![JobProcess::from(entry(11, 11, 11, 10).record)];
        assert_eq!(
            handle(&ApiRequest::JobRunning { job }, &panes, &jobs),
            Ok(ApiResponse::JobRunning(true))
        );
    }
}
