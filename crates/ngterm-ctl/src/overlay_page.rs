//! The HTML page side of `ngtermctl session overlay` (`agtermctlKit/OverlayPageCommands.swift`): `reload`, `submit`,
//! `navigate`, and the runner behind `open --html --block` and `result --page`, which polls by the page id the open
//! reply names, never by session, so a page reopened in the same slot cannot answer for this one.

use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;

use ngterm_proto::command::SessionCmd;
use ngterm_proto::{Args, Command, ControlHtmlPageOutcome, ControlHtmlPageOutcomeState, ControlResult, Request};

use crate::client::Reply;
use crate::modal::{ModalIo, poll_delay};
use crate::session::{flag, session_request};
use crate::session_hud::validate_overlay_pane;
use crate::{BasicOptions, ClientOptions, Failure, RequestCommand, TargetOptions, output};

/// The steps `navigate` takes (upstream's `HtmlNavigation`).
const NAVIGATION_STEPS: [&str; 4] = ["back", "forward", "browser", "finder"];

/// `path` as the caller's shell sees it (upstream's `absolutePath`), since the app never learns the caller's
/// directory: relative to the current directory, `.` and `..` resolved by name, a trailing slash dropped. Symlinks are
/// kept, so a page and its `--cwd` compare as typed.
pub fn absolute_path(path: &str) -> String {
    let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    absolute_path_from(&base, path)
}

/// [`absolute_path`] against `base`.
fn absolute_path_from(base: &Path, path: &str) -> String {
    let mut resolved = PathBuf::from("/");
    for component in base.join(path).components() {
        match component {
            Component::Normal(name) => resolved.push(name),
            Component::ParentDir => {
                resolved.pop();
            }
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
        }
    }
    resolved.to_string_lossy().into_owned()
}

/// The read of a page's outcome: by its globally unique id alone, no session and no window.
pub fn page_result_request(page: &str) -> Request {
    Request {
        cmd: Command::Session(SessionCmd::OverlayResult),
        target: None,
        args: Some(Args {
            page: Some(page.to_owned()),
            ..Args::default()
        }),
    }
}

/// The process status of a page outcome, as pick's: 0 answered, 2 dismissed; a one-shot read of a pending page fails.
pub fn page_exit_code(outcome: ControlHtmlPageOutcomeState) -> u8 {
    match outcome {
        ControlHtmlPageOutcomeState::Submitted => 0,
        ControlHtmlPageOutcomeState::Pending => 1,
        ControlHtmlPageOutcomeState::Dismissed => 2,
    }
}

/// A page outcome read off a poll, beside the reply it came in.
struct Polled {
    reply: Reply,
    outcome: ControlHtmlPageOutcome,
}

/// The page side of `open --html --block` and `result --page` (upstream's `HtmlPageRunner`), its transport, sleep and
/// output injected so every outcome and exit status is testable without a socket.
pub struct PageRunner<'a> {
    /// `--json`: print the server's reply line instead of the outcome object, and a refusal's line on stdout.
    pub json: bool,
    pub io: &'a mut dyn ModalIo,
}

impl PageRunner<'_> {
    /// Opens the page with `open`, then polls its outcome until it leaves `pending`, prints it and exits with its
    /// status.
    pub fn block(&mut self, open: &Request) -> Result<(), Failure> {
        let opened = self.io.send(open)?;
        self.require_success(&opened)?;
        let page_id = control_result(&opened).and_then(|result| result.page_id);
        let Some(page_id) = page_id else {
            self.io
                .error_output("error: session.overlay.open result missing pageID");
            return Err(Failure::Exit(ExitCode::FAILURE));
        };
        let mut pending_polls: u32 = 0;
        loop {
            let polled = self.poll(&page_id)?;
            if polled.outcome.outcome != ControlHtmlPageOutcomeState::Pending {
                return self.finish(&polled);
            }
            pending_polls = pending_polls.saturating_add(1);
            self.io.sleep(poll_delay(pending_polls));
        }
    }

    /// Reads the outcome of `page_id` once and prints it, `pending` included.
    pub fn read(&mut self, page_id: &str) -> Result<(), Failure> {
        let polled = self.poll(page_id)?;
        self.finish(&polled)
    }

    fn poll(&mut self, page_id: &str) -> Result<Polled, Failure> {
        let reply = self.io.send(&page_result_request(page_id))?;
        self.require_success(&reply)?;
        let Some(outcome) = control_result(&reply).and_then(|result| result.page_outcome) else {
            self.io
                .error_output("error: session.overlay.result missing the page outcome");
            return Err(Failure::Exit(ExitCode::FAILURE));
        };
        Ok(Polled { reply, outcome })
    }

    fn finish(&mut self, polled: &Polled) -> Result<(), Failure> {
        let line = if self.json {
            polled.reply.line()
        } else {
            serde_json::to_string(&polled.outcome)
                .map_err(|error| Failure::Local(format!("could not encode result: {error}")))?
        };
        self.io.output(&line);
        match page_exit_code(polled.outcome.outcome) {
            0 => Ok(()),
            code => Err(Failure::Exit(ExitCode::from(code))),
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
}

/// The reply's result object, `None` when absent or not one.
pub fn control_result(reply: &Reply) -> Option<ControlResult> {
    let result = reply.response.result.as_ref().filter(|result| !result.is_null())?;
    serde_json::from_value(result.clone()).ok()
}

/// `ngtermctl session overlay reload`.
#[derive(Debug, clap::Args)]
#[command(about = "Reload an HTML overlay: the file or URL it was opened with, or with --current the page shown now.")]
pub struct Reload {
    #[arg(
        long,
        help = "Reload the page the overlay shows now instead of the original file or URL."
    )]
    current: bool,

    #[arg(
        long,
        help = "Reload that split pane's page (primary/left/top or split/right/bottom); omit for the session-wide \
                overlay."
    )]
    pane: Option<String>,

    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Reload {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn validate(&self) -> Result<(), String> {
        validate_overlay_pane(self.pane.as_deref())
    }

    fn make_request(&self) -> Request {
        let args = Args {
            pane: self.pane.clone(),
            current: flag(self.current),
            ..Args::default()
        };
        session_request(
            SessionCmd::OverlayReload,
            Some(self.target.target.clone()),
            Some(args),
            &self.options,
        )
    }
}

/// `ngtermctl session overlay submit --value V`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Answer an HTML overlay with a value and close it, as its page would; a waiting --block prints the value."
)]
pub struct Submit {
    #[arg(long, help = "The answer handed back to the caller; empty is a real answer.")]
    value: String,

    #[arg(
        long,
        help = "Answer that split pane's page (primary/left/top or split/right/bottom); omit for the session-wide \
                overlay."
    )]
    pane: Option<String>,

    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Submit {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn validate(&self) -> Result<(), String> {
        validate_overlay_pane(self.pane.as_deref())
    }

    fn make_request(&self) -> Request {
        let args = Args {
            pane: self.pane.clone(),
            value: Some(self.value.clone()),
            ..Args::default()
        };
        session_request(
            SessionCmd::OverlaySubmit,
            Some(self.target.target.clone()),
            Some(args),
            &self.options,
        )
    }
}

/// `ngtermctl session overlay navigate <step>`: the step rides the wire as `to`.
#[derive(Debug, clap::Args)]
#[command(about = "Step an HTML overlay's history, open it in the browser, or reveal its current file in Finder.")]
pub struct Navigate {
    #[arg(help = "back, forward, browser, or finder (file pages only).")]
    step: String,

    #[arg(
        long,
        help = "Navigate that split pane's page (primary/left/top or split/right/bottom); omit for the session-wide \
                overlay."
    )]
    pane: Option<String>,

    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Navigate {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn validate(&self) -> Result<(), String> {
        if !NAVIGATION_STEPS.contains(&self.step.as_str()) {
            return Err("step must be back, forward, browser, or finder".to_owned());
        }
        validate_overlay_pane(self.pane.as_deref())
    }

    fn make_request(&self) -> Request {
        let args = Args {
            pane: self.pane.clone(),
            to: Some(self.step.clone()),
            ..Args::default()
        };
        session_request(
            SessionCmd::OverlayNavigate,
            Some(self.target.target.clone()),
            Some(args),
            &self.options,
        )
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::modal::tests::{FakeIo, answer, reply};

    fn exit_status(outcome: Result<(), Failure>) -> Option<ExitCode> {
        match outcome {
            Ok(()) => None,
            Err(Failure::Exit(code)) => Some(code),
            Err(other) => panic!("unexpected failure: {other:?}"),
        }
    }

    fn outcome(state: &str, value: Option<&str>) -> Reply {
        let mut page = json!({"pageID": "P1", "outcome": state});
        if let Some(value) = value {
            page["value"] = json!(value);
        }
        answer(&json!({ "pageOutcome": page }))
    }

    fn open_request() -> Request {
        Request {
            cmd: Command::Session(SessionCmd::OverlayOpen),
            target: Some("active".to_owned()),
            args: Some(Args {
                html: Some("/tmp/r.html".to_owned()),
                ..Args::default()
            }),
        }
    }

    fn opened() -> Reply {
        answer(&json!({"id": "S1", "pageID": "P1"}))
    }

    /// A transport answering the open with `opened`, then each poll with the next of `polls`.
    fn script(
        opened: Reply,
        polls: Vec<Reply>,
    ) -> FakeIo<impl FnMut(&Request) -> Result<Reply, crate::client::ClientError>> {
        let mut polls = polls.into_iter();
        let mut opened = Some(opened);
        FakeIo::new(move |request: &Request| {
            if request.cmd == Command::Session(SessionCmd::OverlayOpen) {
                return Ok(opened.take().unwrap());
            }
            Ok(polls.next().unwrap())
        })
    }

    #[test]
    fn a_path_resolves_as_the_callers_shell_sees_it() {
        let base = Path::new("/home/u/work");
        assert_eq!(absolute_path_from(base, "/tmp/a/../a/./r.html"), "/tmp/a/r.html");
        assert_eq!(absolute_path_from(base, "/tmp/a/"), "/tmp/a");
        assert_eq!(absolute_path_from(base, "out/r.html"), "/home/u/work/out/r.html");
        assert_eq!(absolute_path_from(base, "../x"), "/home/u/x");
        assert_eq!(absolute_path_from(base, "/../.."), "/");
        assert_eq!(absolute_path_from(base, "."), "/home/u/work");
    }

    #[test]
    fn a_blocked_selector_polls_by_page_and_prints_the_submitted_value() {
        let polls = vec![outcome("pending", None), outcome("submitted", Some("a\nb"))];
        let mut io = script(opened(), polls);
        let status = exit_status(
            PageRunner {
                json: false,
                io: &mut io,
            }
            .block(&open_request()),
        );
        assert_eq!(status, None);
        assert_eq!(io.delays, vec![poll_delay(1)]);
        let poll = json!({"cmd": "session.overlay.result", "args": {"page": "P1"}});
        assert_eq!(io.requests[1..], [poll.clone(), poll]);
        let printed: Value = serde_json::from_str(io.lines.last().unwrap()).unwrap();
        assert_eq!(
            printed,
            json!({"pageID": "P1", "outcome": "submitted", "value": "a\nb"})
        );
    }

    #[test]
    fn an_empty_submitted_value_is_printed_intact() {
        let mut io = script(opened(), vec![outcome("submitted", Some(""))]);
        assert_eq!(
            exit_status(
                PageRunner {
                    json: false,
                    io: &mut io
                }
                .block(&open_request())
            ),
            None
        );
        assert!(io.lines.last().unwrap().contains(r#""value":"""#), "{:?}", io.lines);
    }

    #[test]
    fn a_dismissed_selector_exits_2() {
        let mut io = script(opened(), vec![outcome("dismissed", None)]);
        let status = exit_status(
            PageRunner {
                json: false,
                io: &mut io,
            }
            .block(&open_request()),
        );
        assert_eq!(status, Some(ExitCode::from(2)));
        assert_eq!(io.lines, vec![r#"{"pageID":"P1","outcome":"dismissed"}"#.to_owned()]);
    }

    #[test]
    fn an_open_reply_without_a_page_id_fails() {
        let mut io = script(answer(&json!({"id": "S1"})), Vec::new());
        let status = exit_status(
            PageRunner {
                json: false,
                io: &mut io,
            }
            .block(&open_request()),
        );
        assert_eq!(status, Some(ExitCode::FAILURE));
        assert_eq!(
            io.errors,
            vec!["error: session.overlay.open result missing pageID".to_owned()]
        );
    }

    #[test]
    fn a_failed_open_or_poll_exits_1() {
        let mut refused = script(reply(r#"{"ok":false,"error":"overlay already open"}"#), Vec::new());
        let status = exit_status(
            PageRunner {
                json: false,
                io: &mut refused,
            }
            .block(&open_request()),
        );
        assert_eq!(status, Some(ExitCode::FAILURE));
        assert_eq!(refused.errors, vec!["error: overlay already open".to_owned()]);
        let lost_line = r#"{"ok":false,"error":"unknown page"}"#;
        let mut lost = script(opened(), vec![reply(lost_line)]);
        let status = exit_status(
            PageRunner {
                json: true,
                io: &mut lost,
            }
            .block(&open_request()),
        );
        assert_eq!(status, Some(ExitCode::FAILURE));
        assert_eq!(lost.lines, vec![lost_line.to_owned()]);
    }

    #[test]
    fn a_poll_without_an_outcome_fails() {
        let mut io = script(opened(), vec![answer(&json!({"id": "S1"}))]);
        let status = exit_status(
            PageRunner {
                json: false,
                io: &mut io,
            }
            .block(&open_request()),
        );
        assert_eq!(status, Some(ExitCode::FAILURE));
        assert_eq!(
            io.errors,
            vec!["error: session.overlay.result missing the page outcome".to_owned()]
        );
    }

    #[test]
    fn json_prints_the_raw_reply() {
        let line = r#"{"ok":true,"result":{"pageOutcome":{"pageID":"P1","outcome":"submitted","value":"x"}}}"#;
        let mut io = script(opened(), vec![reply(line)]);
        assert_eq!(
            exit_status(
                PageRunner {
                    json: true,
                    io: &mut io
                }
                .block(&open_request())
            ),
            None
        );
        assert_eq!(io.lines, vec![line.to_owned()]);
    }

    #[test]
    fn a_one_shot_read_of_a_pending_page_exits_1() {
        let mut io = script(opened(), vec![outcome("pending", None)]);
        let status = exit_status(
            PageRunner {
                json: false,
                io: &mut io,
            }
            .read("P1"),
        );
        assert_eq!(status, Some(ExitCode::FAILURE));
        assert_eq!(io.lines, vec![r#"{"pageID":"P1","outcome":"pending"}"#.to_owned()]);
        assert_eq!(
            io.requests,
            vec![json!({"cmd": "session.overlay.result", "args": {"page": "P1"}})]
        );
    }
}
