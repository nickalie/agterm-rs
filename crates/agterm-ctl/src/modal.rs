//! The open → poll flow `pick` and `ask` share (upstream's `ModalCommandRunner` in `agtermctlKit/MiscCommands.swift`
//! and the pick/ask helpers of `SocketClient.swift`). A blocking open polls the result by the dialog's global id until
//! it resolves, prints the nested result object and exits with its outcome's status; `--no-block` prints `{"id":...}`
//! instead. These lines are the CLI's own JSON, not the server's response, so `--json` only changes how a refusal is
//! reported.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use agterm_proto::command::{AskCmd, PickCmd};
use agterm_proto::{
    Command, ControlAskOutcome, ControlAskResult, ControlPickOutcome, ControlPickResult, ControlResult, Request,
    Response,
};

use crate::Failure;
use crate::client::{self, ClientError, Reply};
use crate::output;

/// Which modal a runner drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Pick,
    Ask,
}

impl Family {
    fn name(self) -> &'static str {
        match self {
            Self::Pick => "pick",
            Self::Ask => "ask",
        }
    }

    fn result_command(self) -> Command {
        match self {
            Self::Pick => Command::Pick(PickCmd::Result),
            Self::Ask => Command::Ask(AskCmd::Result),
        }
    }

    fn cancel_command(self) -> Command {
        match self {
            Self::Pick => Command::Pick(PickCmd::Cancel),
            Self::Ask => Command::Ask(AskCmd::Cancel),
        }
    }
}

/// The runner's side effects, injectable so the unbounded blocking flow is testable without a socket or real delays.
pub trait ModalIo {
    /// Sends one request on its own connection.
    fn send(&mut self, request: &Request) -> Result<Reply, ClientError>;
    fn sleep(&mut self, delay: Duration);
    /// One line on stdout.
    fn output(&mut self, line: &str);
    /// One line on stderr.
    fn error_output(&mut self, line: &str);
}

/// The socket, the thread's sleep, stdout and stderr.
pub struct LiveIo {
    pub socket: PathBuf,
}

impl ModalIo for LiveIo {
    fn send(&mut self, request: &Request) -> Result<Reply, ClientError> {
        client::send(&self.socket, request)
    }

    fn sleep(&mut self, delay: Duration) {
        std::thread::sleep(delay);
    }

    fn output(&mut self, line: &str) {
        println!("{line}");
    }

    fn error_output(&mut self, line: &str) {
        eprintln!("{line}");
    }
}

/// The delay before the next poll after `poll` pending answers: a human choice may take minutes, so poll quickly only
/// for the first second (ten 100 ms waits), then back off to 500 ms rather than hammer the serial accept loop.
#[must_use]
pub fn poll_delay(poll: u32) -> Duration {
    Duration::from_millis(if poll <= 10 { 100 } else { 500 })
}

/// The process status of a picker outcome: `pending` is a failure when a one-shot read sees it.
#[must_use]
pub fn pick_exit_code(outcome: ControlPickOutcome) -> u8 {
    match outcome {
        ControlPickOutcome::Picked | ControlPickOutcome::Custom => 0,
        ControlPickOutcome::Pending => 1,
        ControlPickOutcome::Cancelled => 2,
    }
}

/// The process status of a dialog outcome: a user dismissal (3) is told apart from an administrative cancel (2).
#[must_use]
pub fn ask_exit_code(outcome: ControlAskOutcome) -> u8 {
    match outcome {
        ControlAskOutcome::Answered => 0,
        ControlAskOutcome::Pending => 1,
        ControlAskOutcome::Cancelled => 2,
        ControlAskOutcome::Escaped => 3,
    }
}

fn encode_failure(error: &serde_json::Error) -> Failure {
    Failure::Local(format!("could not encode result: {error}"))
}

/// The `--no-block` line: `{"id":"..."}`.
pub fn format_id(id: &str) -> Result<String, Failure> {
    serde_json::to_string(&ControlResult::id(id)).map_err(|error| encode_failure(&error))
}

/// The nested `pick.result` payload, without the response around it.
pub fn format_pick_result(result: &ControlPickResult) -> Result<String, Failure> {
    serde_json::to_string(result).map_err(|error| encode_failure(&error))
}

/// The nested `ask.result` payload, without the response around it.
pub fn format_ask_result(result: &ControlAskResult) -> Result<String, Failure> {
    serde_json::to_string(result).map_err(|error| encode_failure(&error))
}

/// A result read off a poll.
struct Outcome {
    pending: bool,
    line: String,
    code: u8,
}

impl Outcome {
    fn exit(&self) -> Result<(), Failure> {
        if self.code == 0 {
            Ok(())
        } else {
            Err(Failure::Exit(ExitCode::from(self.code)))
        }
    }
}

/// The reply's result object, `None` when absent or not one.
fn control_result(response: &Response) -> Option<ControlResult> {
    let result = response.result.as_ref().filter(|result| !result.is_null())?;
    serde_json::from_value(result.clone()).ok()
}

/// Drives one open or one read of a `family` modal.
pub struct ModalRunner<'a> {
    pub family: Family,
    /// `--json`: a refusal prints the server's line on stdout instead of `error:` on stderr.
    pub json: bool,
    pub io: &'a mut dyn ModalIo,
}

impl ModalRunner<'_> {
    /// Sends the open request, then prints the id (`no_block`) or polls until the modal resolves.
    pub fn open(&mut self, request: &Request, no_block: bool) -> Result<(), Failure> {
        let opened = self.io.send(request)?;
        self.require_success(&opened)?;
        let Some(id) = control_result(&opened.response).and_then(|result| result.id) else {
            let message = format!("error: {}.open result missing id", self.family.name());
            self.io.error_output(&message);
            return Err(Failure::Exit(ExitCode::FAILURE));
        };
        if no_block {
            let line = format_id(&id)?;
            self.io.output(&line);
            return Ok(());
        }
        self.wait(&id)
    }

    /// Polls `id` until it leaves `pending`, prints its result and exits with its status.
    fn wait(&mut self, id: &str) -> Result<(), Failure> {
        let poll = Request {
            cmd: self.family.result_command(),
            target: Some(id.to_owned()),
            args: None,
        };
        let mut pending_polls: u32 = 0;
        loop {
            let polled = match self.io.send(&poll) {
                Ok(polled) => polled,
                Err(error) => {
                    // each request opens its own connection, so the cancel can still reach the host after a failed poll
                    self.abandon(id);
                    return Err(error.into());
                }
            };
            // the id-only poll cannot resolve to another window: a refusal means the host no longer holds the modal
            self.require_success(&polled)?;
            let Some(outcome) = self.outcome(&polled.response)? else {
                self.missing_result();
                self.abandon(id);
                return Err(Failure::Exit(ExitCode::FAILURE));
            };
            if !outcome.pending {
                self.io.output(&outcome.line);
                return outcome.exit();
            }
            pending_polls = pending_polls.saturating_add(1);
            self.io.sleep(poll_delay(pending_polls));
        }
    }

    /// Sends a one-shot result request and prints what it reports, `pending` included.
    pub fn read(&mut self, request: &Request) -> Result<(), Failure> {
        let polled = self.io.send(request)?;
        self.require_success(&polled)?;
        let Some(outcome) = self.outcome(&polled.response)? else {
            self.missing_result();
            return Err(Failure::Exit(ExitCode::FAILURE));
        };
        self.io.output(&outcome.line);
        outcome.exit()
    }

    fn missing_result(&mut self) {
        let message = format!("error: {}.result missing result", self.family.name());
        self.io.error_output(&message);
    }

    /// The family's nested result, `None` when the reply carries none.
    fn outcome(&self, response: &Response) -> Result<Option<Outcome>, Failure> {
        let Some(result) = control_result(response) else {
            return Ok(None);
        };
        match self.family {
            Family::Pick => result
                .pick
                .map(|pick| {
                    Ok(Outcome {
                        pending: pick.result == ControlPickOutcome::Pending,
                        line: format_pick_result(&pick)?,
                        code: pick_exit_code(pick.result),
                    })
                })
                .transpose(),
            Family::Ask => result
                .ask
                .map(|ask| {
                    Ok(Outcome {
                        pending: ask.result == ControlAskOutcome::Pending,
                        line: format_ask_result(&ask)?,
                        code: ask_exit_code(ask.result),
                    })
                })
                .transpose(),
        }
    }

    /// A refusal exits 1 after the server's line (`--json`) or its `error:` line.
    fn require_success(&mut self, reply: &Reply) -> Result<(), Failure> {
        if reply.response.ok {
            return Ok(());
        }
        if self.json {
            self.io.output(&reply.line());
        } else {
            self.io.error_output(&output::format_response(&reply.response, false));
        }
        Err(Failure::Exit(ExitCode::FAILURE))
    }

    /// Best-effort cancel of a modal this run gives up on, so it does not keep the shared modal slot.
    fn abandon(&mut self, id: &str) {
        let cancel = Request {
            cmd: self.family.cancel_command(),
            target: Some(id.to_owned()),
            args: None,
        };
        // best effort: the failure being reported is the one that made the run give up
        let _ = self.io.send(&cancel);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use serde_json::{Value, json};

    use super::*;

    /// A scripted transport: `answer` replies to each request, which is recorded with every sleep and line.
    pub struct FakeIo<F> {
        pub answer: F,
        pub requests: Vec<Value>,
        pub delays: Vec<Duration>,
        pub lines: Vec<String>,
        pub errors: Vec<String>,
    }

    impl<F: FnMut(&Request) -> Result<Reply, ClientError>> FakeIo<F> {
        pub fn new(answer: F) -> Self {
            Self {
                answer,
                requests: Vec::new(),
                delays: Vec::new(),
                lines: Vec::new(),
                errors: Vec::new(),
            }
        }

        /// The commands sent, in order.
        pub fn commands(&self) -> Vec<&str> {
            self.requests
                .iter()
                .map(|request| request["cmd"].as_str().unwrap())
                .collect()
        }
    }

    impl<F: FnMut(&Request) -> Result<Reply, ClientError>> ModalIo for FakeIo<F> {
        fn send(&mut self, request: &Request) -> Result<Reply, ClientError> {
            self.requests.push(serde_json::to_value(request).unwrap());
            (self.answer)(request)
        }

        fn sleep(&mut self, delay: Duration) {
            self.delays.push(delay);
        }

        fn output(&mut self, line: &str) {
            self.lines.push(line.to_owned());
        }

        fn error_output(&mut self, line: &str) {
            self.errors.push(line.to_owned());
        }
    }

    /// The reply a server sends as `line`.
    pub fn reply(line: &str) -> Reply {
        Reply {
            response: serde_json::from_str(line).unwrap(),
            raw: line.as_bytes().to_vec(),
        }
    }

    /// An ok reply carrying `result`.
    pub fn answer(result: &Value) -> Reply {
        reply(&json!({"ok": true, "result": result}).to_string())
    }

    fn exit_status(outcome: Result<(), Failure>) -> Option<ExitCode> {
        match outcome {
            Ok(()) => None,
            Err(Failure::Exit(code)) => Some(code),
            Err(other) => panic!("unexpected failure: {other:?}"),
        }
    }

    fn open_request() -> Request {
        Request::new(Command::Pick(PickCmd::Open))
    }

    #[test]
    fn the_poll_backs_off_after_one_second() {
        assert_eq!(poll_delay(1), Duration::from_millis(100));
        assert_eq!(poll_delay(10), Duration::from_millis(100));
        assert_eq!(poll_delay(11), Duration::from_millis(500));
    }

    #[test]
    fn outcomes_map_to_exit_statuses() {
        assert_eq!(pick_exit_code(ControlPickOutcome::Picked), 0);
        assert_eq!(pick_exit_code(ControlPickOutcome::Custom), 0);
        assert_eq!(pick_exit_code(ControlPickOutcome::Pending), 1);
        assert_eq!(pick_exit_code(ControlPickOutcome::Cancelled), 2);
        assert_eq!(ask_exit_code(ControlAskOutcome::Answered), 0);
        assert_eq!(ask_exit_code(ControlAskOutcome::Pending), 1);
        assert_eq!(ask_exit_code(ControlAskOutcome::Cancelled), 2);
        assert_eq!(ask_exit_code(ControlAskOutcome::Escaped), 3);
    }

    #[test]
    fn results_print_as_their_nested_object() {
        assert_eq!(format_id("P1").ok().unwrap(), r#"{"id":"P1"}"#);
        let picked = ControlPickResult {
            id: Some("a".to_owned()),
            label: Some("Alpha".to_owned()),
            index: Some(0),
            ..ControlPickResult::outcome(ControlPickOutcome::Picked)
        };
        assert_eq!(
            format_pick_result(&picked).ok().unwrap(),
            r#"{"result":"picked","id":"a","label":"Alpha","index":0}"#
        );
        let custom = ControlPickResult {
            query: Some("typed".to_owned()),
            ..ControlPickResult::outcome(ControlPickOutcome::Custom)
        };
        assert_eq!(
            format_pick_result(&custom).ok().unwrap(),
            r#"{"result":"custom","query":"typed"}"#
        );
        let answered = ControlAskResult {
            id: Some("yes".to_owned()),
            label: Some("Yes".to_owned()),
            index: Some(0),
            ..ControlAskResult::outcome(ControlAskOutcome::Answered)
        };
        assert_eq!(
            format_ask_result(&answered).ok().unwrap(),
            r#"{"result":"answered","id":"yes","label":"Yes","index":0}"#
        );
    }

    #[test]
    fn a_blocking_pick_polls_by_id_with_backoff_and_prints_the_answer() {
        let mut polls = 0;
        let mut io = FakeIo::new(|request: &Request| {
            if request.cmd == Command::Pick(PickCmd::Open) {
                return Ok(answer(&json!({"id": "P1"})));
            }
            polls += 1;
            if polls <= 11 {
                Ok(answer(&json!({"pick": {"result": "pending"}})))
            } else {
                Ok(answer(
                    &json!({"pick": {"result": "picked", "id": "a", "label": "Alpha", "index": 0}}),
                ))
            }
        });
        let mut runner = ModalRunner {
            family: Family::Pick,
            json: false,
            io: &mut io,
        };
        assert_eq!(exit_status(runner.open(&open_request(), false)), None);
        assert!(
            io.requests[1..]
                .iter()
                .all(|request| *request == json!({"cmd": "pick.result", "target": "P1"}))
        );
        let expected: Vec<Duration> = [100; 10]
            .iter()
            .chain(&[500])
            .map(|ms| Duration::from_millis(*ms))
            .collect();
        assert_eq!(io.delays, expected);
        assert_eq!(io.lines, [r#"{"result":"picked","id":"a","label":"Alpha","index":0}"#]);
    }

    #[test]
    fn a_cancelled_pick_prints_before_exiting_2() {
        let mut io = FakeIo::new(|request: &Request| {
            if request.cmd == Command::Pick(PickCmd::Open) {
                Ok(answer(&json!({"id": "P1"})))
            } else {
                Ok(answer(&json!({"pick": {"result": "cancelled"}})))
            }
        });
        let mut runner = ModalRunner {
            family: Family::Pick,
            json: false,
            io: &mut io,
        };
        assert_eq!(
            exit_status(runner.open(&open_request(), false)),
            Some(ExitCode::from(2))
        );
        assert_eq!(io.lines, [r#"{"result":"cancelled"}"#]);
    }

    #[test]
    fn no_block_prints_only_the_id() {
        let mut io = FakeIo::new(|_: &Request| Ok(answer(&json!({"id": "P1", "pane": "right"}))));
        let mut runner = ModalRunner {
            family: Family::Pick,
            json: true,
            io: &mut io,
        };
        assert_eq!(exit_status(runner.open(&open_request(), true)), None);
        assert_eq!(io.commands(), ["pick.open"]);
        assert!(io.delays.is_empty());
        assert_eq!(io.lines, [r#"{"id":"P1"}"#]);
    }

    #[test]
    fn a_refused_open_follows_the_json_flag() {
        let raw = r#"{"ok": false, "error": "pick already pending", "hint": "unmodeled"}"#;
        for json in [false, true] {
            let mut io = FakeIo::new(|_: &Request| Ok(reply(raw)));
            let mut runner = ModalRunner {
                family: Family::Pick,
                json,
                io: &mut io,
            };
            assert_eq!(
                exit_status(runner.open(&open_request(), false)),
                Some(ExitCode::FAILURE)
            );
            let (lines, errors): (&[&str], &[&str]) = if json {
                (&[raw], &[])
            } else {
                (&[], &["error: pick already pending"])
            };
            assert_eq!(io.lines, lines, "{json}");
            assert_eq!(io.errors, errors, "{json}");
        }
    }

    #[test]
    fn an_open_without_an_id_fails_before_polling() {
        let mut io = FakeIo::new(|_: &Request| Ok(answer(&json!({}))));
        let mut runner = ModalRunner {
            family: Family::Pick,
            json: false,
            io: &mut io,
        };
        assert_eq!(
            exit_status(runner.open(&open_request(), false)),
            Some(ExitCode::FAILURE)
        );
        assert_eq!(io.commands(), ["pick.open"]);
        assert_eq!(io.errors, ["error: pick.open result missing id"]);
        assert!(io.lines.is_empty());
    }

    #[test]
    fn a_poll_without_a_result_cancels_the_exact_modal() {
        let mut io = FakeIo::new(|request: &Request| {
            if request.cmd == Command::Pick(PickCmd::Open) {
                Ok(answer(&json!({"id": "P1"})))
            } else {
                Ok(reply(r#"{"ok":true}"#))
            }
        });
        let mut runner = ModalRunner {
            family: Family::Pick,
            json: false,
            io: &mut io,
        };
        assert_eq!(
            exit_status(runner.open(&open_request(), false)),
            Some(ExitCode::FAILURE)
        );
        assert_eq!(io.commands(), ["pick.open", "pick.result", "pick.cancel"]);
        assert_eq!(io.requests[2], json!({"cmd": "pick.cancel", "target": "P1"}));
        assert_eq!(io.errors, ["error: pick.result missing result"]);
    }

    #[test]
    fn a_failed_poll_cancels_and_reports_the_poll_failure() {
        let mut io = FakeIo::new(|request: &Request| match request.cmd {
            Command::Pick(PickCmd::Open) => Ok(answer(&json!({"id": "P1"}))),
            Command::Pick(PickCmd::Result) => Err(ClientError::new("poll failed")),
            _ => Err(ClientError::new("cancel failed")),
        });
        let mut runner = ModalRunner {
            family: Family::Pick,
            json: false,
            io: &mut io,
        };
        assert!(matches!(
            runner.open(&open_request(), false),
            Err(Failure::Local(message)) if message == "poll failed"
        ));
        assert_eq!(io.commands(), ["pick.open", "pick.result", "pick.cancel"]);
    }

    #[test]
    fn a_refused_poll_does_not_cancel() {
        let mut io = FakeIo::new(|request: &Request| {
            if request.cmd == Command::Pick(PickCmd::Open) {
                Ok(answer(&json!({"id": "P1"})))
            } else {
                Ok(reply(r#"{"ok":false,"error":"unknown pick: P1"}"#))
            }
        });
        let mut runner = ModalRunner {
            family: Family::Pick,
            json: false,
            io: &mut io,
        };
        assert_eq!(
            exit_status(runner.open(&open_request(), false)),
            Some(ExitCode::FAILURE)
        );
        assert_eq!(io.commands(), ["pick.open", "pick.result"]);
        assert_eq!(io.errors, ["error: unknown pick: P1"]);
    }

    #[test]
    fn a_one_shot_read_prints_every_outcome_and_maps_its_exit() {
        let cases = [
            (json!({"result": "pending"}), Some(1)),
            (
                json!({"result": "picked", "id": "a", "label": "Alpha", "index": 0}),
                None,
            ),
            (json!({"result": "custom", "query": "typed"}), None),
            (json!({"result": "cancelled"}), Some(2)),
        ];
        for (pick, status) in cases {
            let mut io = FakeIo::new(|_: &Request| Ok(answer(&json!({"pick": pick}))));
            let mut runner = ModalRunner {
                family: Family::Pick,
                json: false,
                io: &mut io,
            };
            let request = Request {
                cmd: Command::Pick(PickCmd::Result),
                target: Some("P1".to_owned()),
                args: None,
            };
            assert_eq!(exit_status(runner.read(&request)), status.map(ExitCode::from), "{pick}");
            assert_eq!(io.commands(), ["pick.result"]);
            let printed: Value = serde_json::from_str(&io.lines[0]).unwrap();
            assert_eq!((io.lines.len(), printed), (1, pick));
        }
    }

    #[test]
    fn a_one_shot_read_without_a_result_does_not_cancel() {
        let mut io = FakeIo::new(|_: &Request| Ok(reply(r#"{"ok":true}"#)));
        let mut runner = ModalRunner {
            family: Family::Ask,
            json: false,
            io: &mut io,
        };
        let request = Request {
            cmd: Command::Ask(AskCmd::Result),
            target: Some("A1".to_owned()),
            args: None,
        };
        assert_eq!(exit_status(runner.read(&request)), Some(ExitCode::FAILURE));
        assert_eq!(io.commands(), ["ask.result"]);
        assert_eq!(io.errors, ["error: ask.result missing result"]);
    }
}
