//! `agtermctl events` (`agtermctlKit/EventCommands.swift` `Events`): `events.read` is one request per page, so the
//! command polls. The first read carries no cursor (or the one given with `--run`/`--after`), every later one the
//! cursor of the page before; each event prints as it arrives, as one JSON line or `EventFormatter.human`'s line.

use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

use agterm_core::events::{EventRequestError, MAX_LIMIT};
use agterm_core::paths::Env;
use agterm_core::persist::parse_uuid_string;
use agterm_proto::command::EventsCmd;
use agterm_proto::{Args, Command, ControlEvent, ControlResult, EventKind, Request, Response};
use strum::IntoEnumIterator;

use crate::client::{self, ClientError};
use crate::{BasicOptions, Failure};

/// The wait before reading again after an empty page.
const EMPTY_PAGE_WAIT: Duration = Duration::from_millis(250);
/// The first wait after a refused connect, doubled per refusal up to [`MAX_RETRY_DELAY`].
const FIRST_RETRY_DELAY: Duration = Duration::from_millis(250);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(4);
/// How long consecutive refused connects may last before the stream gives up, so it never waits forever on an app
/// that is gone.
const RETRY_BUDGET: Duration = Duration::from_secs(30);

/// `agtermctl events [--socket <socket>] [--json] [--kind <kind> ...] [--run <run>] [--after <after>]
/// [--limit <limit>]`.
#[derive(Debug, clap::Args)]
#[command(about = "Continuously print control events.")]
pub struct Events {
    #[command(flatten)]
    options: BasicOptions,

    #[arg(
        long = "kind",
        action = clap::ArgAction::Append,
        help = "Event kind; repeat or comma-separate values."
    )]
    kind_fields: Vec<String>,

    #[arg(long = "run", help = "App-run UUID paired with --after.")]
    run_id: Option<String>,

    #[arg(long, help = "Sequence cursor paired with --run.")]
    after: Option<String>,

    #[arg(long, help = "Events per read (1...1000).")]
    limit: Option<i64>,
}

impl Events {
    /// Every `--kind` value, split at commas and trimmed; an empty one stays, to be refused.
    fn raw_kinds(&self) -> impl Iterator<Item = &str> {
        self.kind_fields
            .iter()
            .flat_map(|field| field.split(',').map(str::trim))
    }

    /// What the parser cannot check (upstream's `validate()`), as the message of a usage error: the server's own
    /// refusals of the same arguments.
    pub fn validate(&self) -> Result<(), String> {
        self.refusal().map_or(Ok(()), |error| Err(error.to_string()))
    }

    fn refusal(&self) -> Option<EventRequestError> {
        if self.run_id.is_some() != self.after.is_some() {
            return Some(EventRequestError::CursorPair);
        }
        if self
            .run_id
            .as_deref()
            .is_some_and(|run| parse_uuid_string(run).is_none())
        {
            return Some(EventRequestError::InvalidRun);
        }
        if self.after.as_deref().is_some_and(|after| after.parse::<u64>().is_err()) {
            return Some(EventRequestError::InvalidCursor);
        }
        let in_range = |limit: i64| usize::try_from(limit).is_ok_and(|limit| (1..=MAX_LIMIT).contains(&limit));
        if self.limit.is_some_and(|limit| !in_range(limit)) {
            return Some(EventRequestError::InvalidLimit);
        }
        self.raw_kinds()
            .find(|raw| raw.parse::<EventKind>().is_err())
            .map(|raw| EventRequestError::InvalidKind(raw.to_owned()))
    }

    /// The stream's starting point: the validated cursor, kinds and limit.
    fn make_state(&self) -> StreamState {
        let run = self.run_id.as_deref().and_then(parse_uuid_string);
        let after = self.after.as_deref().and_then(|after| after.parse::<u64>().ok());
        let cursor = run.zip(after).map(|(run, after)| Cursor {
            run: run.hyphenated().to_string().to_ascii_uppercase(),
            after,
        });
        let selected: Vec<EventKind> = self.raw_kinds().filter_map(|raw| raw.parse().ok()).collect();
        let kinds = (!selected.is_empty()).then_some(selected);
        StreamState::new(cursor, kinds.as_deref(), self.limit)
    }

    /// Polls until a failure ends the stream; it never ends otherwise.
    pub fn run(&self, env: &dyn Env) -> Result<(), Failure> {
        let mut state = self.make_state();
        let mut live = LiveStream {
            socket: self.options.socket_path(env),
        };
        loop {
            self.poll(&mut state, &mut live)?;
        }
    }

    /// One read: prints its events, or waits after an empty page or a refused connect the stream may retry.
    fn poll(&self, state: &mut StreamState, dependencies: &mut dyn StreamDependencies) -> Result<(), Failure> {
        let response = match dependencies.send(&state.make_request()) {
            Ok(response) => response,
            Err(error) => {
                let delay = state.retry_delay(&error).ok_or(error)?;
                dependencies.sleep(delay);
                return Ok(());
            }
        };
        let events = state.consume(response).map_err(Failure::Local)?;
        for event in &events {
            let line = if self.options.json {
                format_json(event)?
            } else {
                format_human(event, Zone::Local)
            };
            dependencies
                .write_line(&line)
                .map_err(|error| Failure::Local(format!("stdout write failed: {}", client::os_message(&error))))?;
        }
        if events.is_empty() {
            dependencies.sleep(EMPTY_PAGE_WAIT);
        }
        Ok(())
    }
}

/// The stream's side effects, injectable so the loop is testable (upstream's `EventStreamDependencies`).
trait StreamDependencies {
    /// Sends one request and decodes its reply.
    fn send(&mut self, request: &Request) -> Result<Response<ControlResult>, ClientError>;
    fn sleep(&mut self, delay: Duration);
    /// Writes one line and flushes it, so a reader on a pipe sees each event as it arrives.
    fn write_line(&mut self, line: &str) -> io::Result<()>;
}

/// The socket, the thread's sleep and stdout.
struct LiveStream {
    socket: PathBuf,
}

impl StreamDependencies for LiveStream {
    fn send(&mut self, request: &Request) -> Result<Response<ControlResult>, ClientError> {
        let response = client::send(&self.socket, request)?.response;
        // `client::send` already decoded the result as a `ControlResult`, so this cannot fail
        let result = response
            .result
            .filter(|result| !result.is_null())
            .map(serde_json::from_value::<ControlResult>)
            .transpose()
            .map_err(|error| ClientError::new(format!("could not decode response: {error}")))?;
        Ok(Response {
            ok: response.ok,
            result,
            error: response.error,
        })
    }

    fn sleep(&mut self, delay: Duration) {
        std::thread::sleep(delay);
    }

    fn write_line(&mut self, line: &str) -> io::Result<()> {
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{line}")?;
        stdout.flush()
    }
}

/// A position within one app run (upstream's `ControlEventCursor`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cursor {
    /// The run id, upper-case.
    run: String,
    /// The last sequence seen.
    after: u64,
}

/// The cursor, the filter and the retry bookkeeping of one stream (upstream's `EventStreamState`).
#[derive(Debug)]
struct StreamState {
    cursor: Option<Cursor>,
    /// The kinds asked for, in declaration order without repeats; `None` asks for every kind.
    kinds: Option<Vec<EventKind>>,
    limit: Option<i64>,
    next_retry_delay: Duration,
    /// How long the current run of refused connects has waited so far.
    retried_for: Duration,
}

impl StreamState {
    fn new(cursor: Option<Cursor>, kinds: Option<&[EventKind]>, limit: Option<i64>) -> Self {
        Self {
            cursor,
            kinds: kinds.map(|selected| EventKind::iter().filter(|kind| selected.contains(kind)).collect()),
            limit,
            next_retry_delay: FIRST_RETRY_DELAY,
            retried_for: Duration::ZERO,
        }
    }

    /// The next read: a bare `events.read` until there is a cursor, a filter or a limit to send.
    fn make_request(&self) -> Request {
        let request = Request::new(Command::Events(EventsCmd::Read));
        if self.cursor.is_none() && self.kinds.is_none() && self.limit.is_none() {
            return request;
        }
        let args = Args {
            after: self.cursor.as_ref().map(|cursor| cursor.after.to_string()),
            run: self.cursor.as_ref().map(|cursor| cursor.run.clone()),
            kinds: self
                .kinds
                .as_ref()
                .map(|kinds| kinds.iter().map(|kind| kind.wire_name().to_owned()).collect()),
            limit: self.limit,
            ..Args::default()
        };
        Request {
            args: Some(args),
            ..request
        }
    }

    /// The wait before reading again after `error`, `None` when the error ends the stream. Only a refused connect is
    /// retried: a live server whose backlog is full refuses, while a missing socket means the app quit, and a relaunch
    /// is a new run the cursor cannot resume. Without a cursor there is nothing to keep, so a first read fails fast.
    fn retry_delay(&mut self, error: &ClientError) -> Option<Duration> {
        let refused = error.connect_errno == Some(libc::ECONNREFUSED);
        if self.cursor.is_none() || !refused || self.retried_for >= RETRY_BUDGET {
            return None;
        }
        let delay = self.next_retry_delay;
        self.retried_for += delay;
        self.next_retry_delay = (delay * 2).min(MAX_RETRY_DELAY);
        Some(delay)
    }

    /// Takes a reply's page and moves the cursor past it; any reply renews the retry budget. A refusal is the
    /// server's error text.
    fn consume(&mut self, response: Response<ControlResult>) -> Result<Vec<ControlEvent>, String> {
        self.next_retry_delay = FIRST_RETRY_DELAY;
        self.retried_for = Duration::ZERO;
        if !response.ok {
            return Err(response.error.unwrap_or_else(|| "events.read failed".to_owned()));
        }
        let batch = response
            .result
            .and_then(|result| result.events)
            .ok_or_else(|| "events.read response missing events".to_owned())?;
        self.cursor = Some(Cursor {
            run: batch.run,
            after: batch.next,
        });
        Ok(batch.items)
    }
}

/// One event as one compact JSON line.
fn format_json(event: &ControlEvent) -> Result<String, Failure> {
    serde_json::to_string(event).map_err(|error| Failure::Local(format!("could not encode event: {error}")))
}

/// The time zone an event's clock reads in.
#[derive(Debug, Clone, Copy)]
enum Zone {
    /// The process's local zone (`TZ`, else the system's).
    Local,
    /// A fixed offset east of UTC, in seconds.
    #[cfg_attr(not(test), expect(dead_code, reason = "the tests' seam: they format at UTC"))]
    Offset(i64),
}

impl Zone {
    /// The offset east of UTC at `seconds` since the epoch.
    fn offset_at(self, seconds: i64) -> i64 {
        match self {
            Self::Local => local_offset(seconds),
            Self::Offset(offset) => offset,
        }
    }
}

/// The local zone's offset east of UTC at `seconds` since the epoch, 0 when the C library cannot say.
fn local_offset(seconds: i64) -> i64 {
    let time: libc::time_t = seconds;
    // SAFETY: `tm` is plain integers and a `tm_zone` pointer, for which all-zero is a valid (null) value.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers are to live locals; the reentrant form writes only `tm`.
    let converted = unsafe { libc::localtime_r(&raw const time, &raw mut tm) };
    if converted.is_null() { 0 } else { tm.tm_gmtoff }
}

/// Whole seconds since the epoch, rounded down.
#[expect(
    clippy::cast_possible_truncation,
    reason = "a clock reading fits i64; `as` saturates the absurd ones and maps NaN to 0"
)]
fn whole_seconds(ts: f64) -> i64 {
    ts.floor() as i64
}

/// `HH:MM:SS` of `ts` in `zone`.
fn clock(ts: f64, zone: Zone) -> String {
    let seconds = whole_seconds(ts);
    let of_day = seconds.saturating_add(zone.offset_at(seconds)).rem_euclid(86_400);
    format!("{:02}:{:02}:{:02}", of_day / 3_600, of_day / 60 % 60, of_day % 60)
}

/// One event as upstream's `EventFormatter.human`: the clock, the kind, the session's name (else the window, else
/// `-`), then the kind's arguments.
fn format_human(event: &ControlEvent, zone: Zone) -> String {
    let time = clock(event.ts, zone);
    let kind = event.kind.wire_name();
    let payload = &event.payload;
    let name = payload.name.as_deref().or(event.window.as_deref()).unwrap_or("-");
    match event.kind {
        EventKind::Status => {
            let mut parts = vec![
                time,
                kind.to_owned(),
                name.to_owned(),
                payload.status.clone().unwrap_or_else(|| "idle".to_owned()),
            ];
            parts.extend(payload.previous.as_ref().map(|previous| format!("previous={previous}")));
            parts.extend(payload.pane.as_ref().map(|pane| format!("pane={pane}")));
            parts.extend((payload.blink == Some(true)).then(|| "blink".to_owned()));
            parts.extend(payload.color.as_ref().map(|color| format!("color={color}")));
            parts.extend(payload.shape.as_ref().map(|shape| format!("shape={shape}")));
            parts.join(" ")
        }
        EventKind::Notify => format!(
            "{time} {kind} {name} {}: {}",
            payload.title.as_deref().unwrap_or(name),
            payload.body.as_deref().unwrap_or_default()
        ),
        EventKind::SessionCreated | EventKind::SessionClosed | EventKind::TreeChanged => {
            format!("{time} {kind} {name}")
        }
        EventKind::SessionSelected => format!(
            "{time} {kind} {name} previous={}",
            payload.previous.as_deref().unwrap_or("-")
        ),
        EventKind::PaneSplit | EventKind::PaneScratch => {
            format!("{time} {kind} {name} {}", payload.status.as_deref().unwrap_or_default())
        }
        EventKind::RemoteOpened | EventKind::RemoteClosed => {
            format!(
                "{time} {kind} {name} host={}",
                payload.host.as_deref().unwrap_or_default()
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use agterm_proto::{EventBatch, EventPayload};
    use serde_json::json;

    use super::*;

    const RUN: &str = "CBB5E3D0-7A9B-4C96-9EA2-18B14380DDB1";

    fn parsed(argv: &[&str]) -> Result<Events, String> {
        use clap::Parser;
        let command_line = ["agtermctl", "events"].iter().chain(argv).copied();
        let parsed = crate::Agtermctl::try_parse_from(command_line).map_err(|error| error.to_string())?;
        let crate::Command::Events(events) = parsed.command else {
            panic!("not events")
        };
        events.validate()?;
        Ok(events)
    }

    fn wire(request: &Request) -> serde_json::Value {
        serde_json::to_value(request).unwrap()
    }

    fn event(seq: u64, kind: EventKind, payload: EventPayload) -> ControlEvent {
        ControlEvent {
            seq,
            ts: 0.0,
            kind,
            window: None,
            workspace: None,
            session: None,
            payload,
        }
    }

    fn page(next: u64, items: Vec<ControlEvent>) -> Response<ControlResult> {
        Response::with_result(ControlResult::events(EventBatch {
            run: RUN.to_owned(),
            next,
            items,
        }))
    }

    fn refused() -> ClientError {
        ClientError {
            message: "connect failed: Connection refused".to_owned(),
            connect_errno: Some(libc::ECONNREFUSED),
        }
    }

    /// Scripted replies (a refused connect once they run out) and a record of what the loop did.
    #[derive(Default)]
    struct Fake {
        outcomes: VecDeque<Result<Response<ControlResult>, ClientError>>,
        requests: Vec<Request>,
        sleeps: Vec<Duration>,
        lines: Vec<String>,
        broken_stdout: bool,
    }

    impl Fake {
        fn new(outcomes: impl IntoIterator<Item = Result<Response<ControlResult>, ClientError>>) -> Self {
            Self {
                outcomes: outcomes.into_iter().collect(),
                ..Self::default()
            }
        }

        fn slept(&self) -> Duration {
            self.sleeps.iter().sum()
        }
    }

    impl StreamDependencies for Fake {
        fn send(&mut self, request: &Request) -> Result<Response<ControlResult>, ClientError> {
            self.requests.push(request.clone());
            self.outcomes.pop_front().unwrap_or_else(|| Err(refused()))
        }

        fn sleep(&mut self, delay: Duration) {
            self.sleeps.push(delay);
        }

        fn write_line(&mut self, line: &str) -> io::Result<()> {
            if self.broken_stdout {
                return Err(io::Error::from_raw_os_error(libc::EIO));
            }
            self.lines.push(line.to_owned());
            Ok(())
        }
    }

    fn millis(values: &[u64]) -> Vec<Duration> {
        values.iter().copied().map(Duration::from_millis).collect()
    }

    fn cursored() -> Events {
        parsed(&["--run", RUN, "--after", "9"]).unwrap()
    }

    fn cursor_of(request: &Request) -> (Option<&str>, Option<&str>) {
        let args = request.args.as_ref();
        (
            args.and_then(|args| args.run.as_deref()),
            args.and_then(|args| args.after.as_deref()),
        )
    }

    #[test]
    fn the_parser_builds_bootstrap_and_cursored_requests() {
        let plain = parsed(&[]).unwrap();
        assert_eq!(wire(&plain.make_state().make_request()), json!({"cmd": "events.read"}));

        let resumed = parsed(&[
            "--json",
            "--kind",
            "tree.changed, status",
            "--kind",
            "pane.split,pane.scratch,status",
            "--kind",
            "notify",
            "--run",
            &RUN.to_ascii_lowercase(),
            "--after",
            "42",
            "--limit",
            "1000",
        ])
        .unwrap();
        assert!(resumed.options.json);
        assert_eq!(
            wire(&resumed.make_state().make_request()),
            json!({"cmd": "events.read", "args": {"after": "42", "run": RUN,
                   "kinds": ["status", "notify", "tree.changed", "pane.split", "pane.scratch"], "limit": 1000}})
        );
        let signed = parsed(&["--run", RUN, "--after", "+5"]).unwrap();
        assert_eq!(cursor_of(&signed.make_state().make_request()), (Some(RUN), Some("5")));
    }

    #[test]
    fn the_parser_refuses_invalid_pairs_kinds_and_limits() {
        let cases: [(&[&str], &str); 13] = [
            (&["--run", RUN], "events.read requires --run and --after together"),
            (&["--after", "1"], "events.read requires --run and --after together"),
            (&["--run", "bad", "--after", "1"], "invalid event run id"),
            (&["--run", &RUN[1..], "--after", "1"], "invalid event run id"),
            (&["--run", RUN, "--after", "1.5"], "invalid event cursor"),
            (&["--run", RUN, "--after", "x"], "invalid event cursor"),
            (&["--kind", "future"], "invalid event kind: future"),
            (&["--kind", ""], "invalid event kind: "),
            (&["--kind", ","], "invalid event kind: "),
            (&["--kind", "status,"], "invalid event kind: "),
            (&["--limit", "0"], "event limit must be between 1 and 1000"),
            (&["--limit", "1001"], "event limit must be between 1 and 1000"),
            // the cursor pair is checked before the limit and the kinds
            (
                &["--after", "1", "--limit", "0", "--kind", "x"],
                "events.read requires --run and --after together",
            ),
        ];
        for (argv, message) in cases {
            assert_eq!(parsed(argv).err().as_deref(), Some(message), "{argv:?}");
        }
        // a negative value reads as an option, as the argument parser's: a usage error either way
        assert!(parsed(&["--run", RUN, "--after", "-1"]).is_err());
    }

    #[test]
    fn the_stream_state_bootstraps_and_advances_its_cursor() {
        let mut state = StreamState::new(None, Some(&[EventKind::Status]), Some(2));
        assert_eq!(
            wire(&state.make_request()),
            json!({"cmd": "events.read", "args": {"kinds": ["status"], "limit": 2}})
        );
        assert!(state.consume(page(5, vec![])).unwrap().is_empty());
        assert_eq!(cursor_of(&state.make_request()), (Some(RUN), Some("5")));

        let status = event(
            6,
            EventKind::Status,
            EventPayload {
                name: Some("api".to_owned()),
                status: Some("active".to_owned()),
                ..EventPayload::default()
            },
        );
        assert_eq!(state.consume(page(6, vec![status.clone()])).unwrap(), vec![status]);
        assert_eq!(cursor_of(&state.make_request()), (Some(RUN), Some("6")));
    }

    #[test]
    fn the_stream_state_keeps_a_supplied_cursor_and_refuses_server_failures() {
        let mut state = cursored().make_state();
        assert_eq!(cursor_of(&state.make_request()), (Some(RUN), Some("9")));
        assert_eq!(
            state.consume(Response::failure("event cursor expired")),
            Err("event cursor expired".to_owned())
        );
        let bare = Response {
            ok: false,
            result: None,
            error: None,
        };
        assert_eq!(state.consume(bare), Err("events.read failed".to_owned()));
        assert_eq!(
            state.consume(Response::success()),
            Err("events.read response missing events".to_owned())
        );
        assert_eq!(cursor_of(&state.make_request()), (Some(RUN), Some("9")));
    }

    #[test]
    fn polling_waits_on_an_empty_page_writes_ndjson_and_resumes_from_the_cursor() {
        let status = event(
            6,
            EventKind::Status,
            EventPayload {
                name: Some("api".to_owned()),
                status: Some("active".to_owned()),
                ..EventPayload::default()
            },
        );
        let mut fake = Fake::new([Ok(page(5, vec![])), Ok(page(6, vec![status.clone()]))]);
        let command = parsed(&["--json"]).unwrap();
        let mut state = command.make_state();
        for _ in 0..2 {
            command.poll(&mut state, &mut fake).unwrap();
        }
        assert_eq!(wire(&fake.requests[0]), json!({"cmd": "events.read"}));
        assert_eq!(cursor_of(&fake.requests[1]), (Some(RUN), Some("5")));
        assert_eq!(fake.sleeps, millis(&[250]));
        assert_eq!(fake.lines.len(), 1);
        assert!(!fake.lines[0].contains('\n'));
        assert_eq!(serde_json::from_str::<ControlEvent>(&fake.lines[0]).unwrap(), status);
    }

    fn local_failure(result: Result<(), Failure>) -> String {
        match result {
            Err(Failure::Local(message)) => message,
            other => panic!("not a local failure: {other:?}"),
        }
    }

    #[test]
    fn transport_server_and_output_failures_end_the_stream_at_once() {
        let command = parsed(&[]).unwrap();
        let mut transport = Fake::new([Err(ClientError::new("no response from /tmp/s"))]);
        let failure = command.poll(&mut command.make_state(), &mut transport);
        assert_eq!(local_failure(failure), "no response from /tmp/s");
        assert_eq!((transport.requests.len(), transport.sleeps.len()), (1, 0));

        let mut server = Fake::new([Ok(Response::failure("event cursor expired"))]);
        let failure = command.poll(&mut command.make_state(), &mut server);
        assert_eq!(local_failure(failure), "event cursor expired");
        assert_eq!(server.requests.len(), 1);

        let mut output = Fake::new([Ok(page(
            1,
            vec![event(1, EventKind::TreeChanged, EventPayload::default())],
        ))]);
        output.broken_stdout = true;
        let failure = command.poll(&mut command.make_state(), &mut output);
        assert_eq!(local_failure(failure), "stdout write failed: Input/output error");
        assert_eq!(output.requests.len(), 1);
    }

    #[test]
    fn a_refused_connect_backs_off_and_resumes_from_the_same_cursor() {
        let outcomes = std::iter::repeat_with(|| Err(refused()))
            .take(6)
            .chain([Ok(page(9, vec![])), Err(refused())]);
        let mut fake = Fake::new(outcomes);
        let command = cursored();
        let mut state = command.make_state();
        for _ in 0..8 {
            command.poll(&mut state, &mut fake).unwrap();
        }
        assert_eq!(fake.sleeps, millis(&[250, 500, 1_000, 2_000, 4_000, 4_000, 250, 250]));
        assert!(
            fake.requests
                .iter()
                .all(|request| cursor_of(request) == (Some(RUN), Some("9")))
        );
    }

    #[test]
    fn a_refused_connect_gives_up_once_the_retry_budget_is_spent() {
        let mut fake = Fake::default();
        let command = cursored();
        let mut state = command.make_state();
        let failure = (0..100)
            .map(|_| command.poll(&mut state, &mut fake))
            .find(Result::is_err);
        assert_eq!(local_failure(failure.unwrap()), "connect failed: Connection refused");
        assert!(fake.slept() >= RETRY_BUDGET, "{:?}", fake.slept());
        assert!(fake.slept() < RETRY_BUDGET + MAX_RETRY_DELAY, "{:?}", fake.slept());
    }

    #[test]
    fn a_response_renews_the_retry_budget() {
        let nearly_spent = 9;
        let outcomes = std::iter::repeat_with(|| Err(refused()))
            .take(nearly_spent)
            .chain([Ok(page(9, vec![]))]);
        let mut fake = Fake::new(outcomes);
        let command = cursored();
        let mut state = command.make_state();
        for _ in 0..=2 * nearly_spent {
            command.poll(&mut state, &mut fake).unwrap();
        }
        let episode = &fake.sleeps[..nearly_spent];
        assert!(episode.iter().sum::<Duration>() < RETRY_BUDGET);
        assert!(fake.slept() > RETRY_BUDGET + MAX_RETRY_DELAY);
        assert_eq!(&fake.sleeps[fake.sleeps.len() - nearly_spent..], episode);
    }

    #[test]
    fn only_a_refused_connect_with_a_cursor_is_retried() {
        let missing = ClientError {
            message: "connect failed".to_owned(),
            connect_errno: Some(libc::ENOENT),
        };
        let fatal = [
            (parsed(&[]).unwrap(), refused()),
            (cursored(), missing),
            (cursored(), ClientError::new("no response from /tmp/agterm.sock")),
        ];
        for (command, error) in fatal {
            let mut fake = Fake::new([Err(error.clone())]);
            let failure = command.poll(&mut command.make_state(), &mut fake);
            assert_eq!(local_failure(failure), error.message);
            assert_eq!((fake.requests.len(), fake.sleeps.len()), (1, 0));
        }
    }

    /// An event at the epoch from its wire `kind`, `window` and `payload`.
    fn wire_event(seq: u64, fields: serde_json::Value) -> ControlEvent {
        let mut event = json!({"seq": seq, "ts": 0, "payload": {}});
        if let (Some(event), serde_json::Value::Object(fields)) = (event.as_object_mut(), fields) {
            event.extend(fields);
        }
        serde_json::from_value(event).unwrap()
    }

    #[test]
    fn the_human_form_covers_every_kind_with_its_arguments() {
        let cases = [
            (
                json!({"kind": "status", "payload": {"name": "api", "status": "blocked", "pane": "right", "blink": true}}),
                "00:00:00 status api blocked pane=right blink",
            ),
            (
                json!({"kind": "notify", "payload": {"name": "api", "title": "Done", "body": "ok"}}),
                "00:00:00 notify api Done: ok",
            ),
            (
                json!({"kind": "session.created", "payload": {"name": "new"}}),
                "00:00:00 session.created new",
            ),
            (
                json!({"kind": "session.closed", "payload": {"name": "old"}}),
                "00:00:00 session.closed old",
            ),
            (
                json!({"kind": "tree.changed", "window": "win"}),
                "00:00:00 tree.changed win",
            ),
            (
                json!({"kind": "status", "payload": {"name": "api", "status": "active", "color": "#ff8800",
                                                     "shape": "triangle"}}),
                "00:00:00 status api active color=#ff8800 shape=triangle",
            ),
            (
                json!({"kind": "status", "payload": {"name": "api", "status": "completed", "shape": "star"}}),
                "00:00:00 status api completed shape=star",
            ),
            (
                json!({"kind": "pane.split", "payload": {"name": "api", "status": "shown"}}),
                "00:00:00 pane.split api shown",
            ),
            (
                json!({"kind": "pane.scratch", "payload": {"name": "api", "status": "hidden"}}),
                "00:00:00 pane.scratch api hidden",
            ),
            (
                json!({"kind": "status", "payload": {"name": "api", "status": "blocked", "pane": "left",
                                                     "previous": "active", "blink": false}}),
                "00:00:00 status api blocked previous=active pane=left",
            ),
            (
                json!({"kind": "remote.opened", "payload": {"name": "far", "host": "buildbox"}}),
                "00:00:00 remote.opened far host=buildbox",
            ),
            (
                json!({"kind": "remote.closed", "payload": {"name": "far", "host": "buildbox"}}),
                "00:00:00 remote.closed far host=buildbox",
            ),
            (
                json!({"kind": "session.selected", "payload": {"name": "api", "previous": "before"}}),
                "00:00:00 session.selected api previous=before",
            ),
            (
                json!({"kind": "session.selected", "window": "win"}),
                "00:00:00 session.selected win previous=-",
            ),
            (json!({"kind": "status"}), "00:00:00 status - idle"),
            (
                json!({"kind": "notify", "payload": {"name": "api"}}),
                "00:00:00 notify api api: ",
            ),
        ];
        for (seq, (fields, expected)) in (1..).zip(cases) {
            let event = wire_event(seq, fields);
            assert_eq!(format_human(&event, Zone::Offset(0)), expected, "{event:?}");
            let line = format_json(&event).unwrap();
            assert!(!line.contains('\n'));
            assert_eq!(serde_json::from_str::<ControlEvent>(&line).unwrap(), event);
        }
    }

    #[test]
    fn the_clock_reads_whole_seconds_in_the_given_zone() {
        let at = 3_600.0 * 13.0 + 60.0 * 4.0 + 5.75;
        assert_eq!(clock(at, Zone::Offset(0)), "13:04:05");
        assert_eq!(clock(at, Zone::Offset(3 * 3_600 + 1_800)), "16:34:05");
        assert_eq!(clock(at, Zone::Offset(-14 * 3_600)), "23:04:05");
        assert_eq!(clock(-0.5, Zone::Offset(0)), "23:59:59");
        assert_eq!(clock(f64::NAN, Zone::Offset(0)), "00:00:00");
    }
}
