//! The `session` subcommands that drive a session's terminal (`agtermctlKit/SessionCommands.swift`): `type`, `copy`,
//! `paste`, `select-all`, `text` and `search`. Each takes its own options first, then `--target`, then
//! [`ClientOptions`], in upstream's declaration order, which is what its help and usage line list.

use std::io::{self, Read};

use ngterm_core::pane_env::StatusPane;
use ngterm_proto::command::SessionCmd;
use ngterm_proto::{Args, Request};

use crate::client::os_message;
use crate::session::session_request;
use crate::{BasicOptions, ClientOptions, Failure, RequestCommand, TargetOptions};

/// Refuses a `--pane` that names no pane (upstream's `validatePaneArgument`); the role and position aliases pass, and
/// go on the wire as typed.
pub fn validate_pane(pane: Option<&str>) -> Result<(), String> {
    match pane {
        Some(pane) if StatusPane::from_control_name(pane).is_none() => {
            Err("--pane must be left, right, or scratch".to_owned())
        }
        _ => Ok(()),
    }
}

/// `ngtermctl session type`: the text is the argument, or standard input with `--stdin`. Its `--pane-id` follows the
/// connection options, as upstream declares it.
#[derive(Debug, clap::Args)]
#[command(about = "Inject text into a session.")]
pub struct Type {
    #[arg(help = "Text to inject (omit with --stdin).")]
    text: Option<String>,

    #[arg(long, help = "Read the text from stdin instead of an argument.")]
    stdin: bool,

    #[arg(
        long,
        help = "Select the session first if its surface is not ready (main pane only; a split pane must already \
                exist)."
    )]
    select: bool,

    #[arg(
        long,
        help = "Which pane to type into: primary/left/top, split/right/bottom, or scratch (even when hidden). \
                Defaults to primary."
    )]
    pane: Option<String>,

    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,

    #[arg(
        long = "pane-id",
        help = "Stable pane token ($NGTERM_PANE_ID); overrides --pane, and unknown without --pane is an error."
    )]
    pane_id: Option<String>,
}

impl Type {
    /// The request carrying `payload`; `select` always goes on the wire, as upstream's.
    fn request_for(&self, payload: String) -> Request {
        let args = Args {
            text: Some(payload),
            select: Some(self.select),
            pane: self.pane.clone(),
            pane_id: self.pane_id.clone(),
            ..Args::default()
        };
        session_request(
            SessionCmd::Type,
            Some(self.target.target.clone()),
            Some(args),
            &self.options,
        )
    }

    /// The request for a `--stdin` payload, whose contract is text: an invalid byte refuses it whole.
    fn request_for_input(&self, input: Vec<u8>) -> Result<Request, Failure> {
        let payload = String::from_utf8(input).map_err(|_| Failure::Usage("stdin must be valid UTF-8".to_owned()))?;
        Ok(self.request_for(payload))
    }
}

impl RequestCommand for Type {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn validate(&self) -> Result<(), String> {
        validate_pane(self.pane.as_deref())
    }

    /// The request for the TEXT argument; a run goes through [`Self::run_request`], which reads `--stdin` and refuses
    /// a missing payload.
    fn make_request(&self) -> Request {
        self.request_for(self.text.clone().unwrap_or_default())
    }

    /// Upstream refuses a missing payload only when the run builds the request, so the usage error names the root
    /// command, not `session type`.
    fn run_request(&self) -> Result<Request, Failure> {
        if self.stdin {
            let mut input = Vec::new();
            io::stdin()
                .read_to_end(&mut input)
                .map_err(|error| Failure::Local(format!("could not read stdin: {}", os_message(&error))))?;
            return self.request_for_input(input);
        }
        match &self.text {
            Some(text) => Ok(self.request_for(text.clone())),
            None => Err(Failure::Usage("provide TEXT or --stdin".to_owned())),
        }
    }
}

/// `ngtermctl session copy`: prints the selection the server returns.
#[derive(Debug, clap::Args)]
#[command(about = "Print a session's selected text (does not touch the system clipboard).")]
pub struct Copy {
    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Copy {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn make_request(&self) -> Request {
        session_request(SessionCmd::Copy, Some(self.target.target.clone()), None, &self.options)
    }
}

/// `ngtermctl session paste`. The abstract is upstream's, `⌘V` included.
#[derive(Debug, clap::Args)]
#[command(about = "Paste the system clipboard into a session (like ⌘V).")]
pub struct Paste {
    #[arg(
        long,
        help = "Which pane to paste into: primary/left/top, split/right/bottom, or scratch (even when hidden). \
                Defaults to primary."
    )]
    pane: Option<String>,

    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Paste {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn validate(&self) -> Result<(), String> {
        validate_pane(self.pane.as_deref())
    }

    fn make_request(&self) -> Request {
        let args = self.pane.clone().map(|pane| Args {
            pane: Some(pane),
            ..Args::default()
        });
        session_request(SessionCmd::Paste, Some(self.target.target.clone()), args, &self.options)
    }
}

/// `ngtermctl session select-all` (wire `session.selectall`).
#[derive(Debug, clap::Args)]
#[command(about = "Select a session's entire terminal buffer (like ⌘A).")]
pub struct SelectAll {
    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for SelectAll {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn make_request(&self) -> Request {
        session_request(
            SessionCmd::SelectAll,
            Some(self.target.target.clone()),
            None,
            &self.options,
        )
    }
}

/// `ngtermctl session text`: prints the text the server reads.
#[derive(Debug, clap::Args)]
#[command(about = "Print a session's terminal buffer as plain text (does not touch the system clipboard).")]
pub struct Text {
    #[arg(long, help = "Read the full screen + scrollback instead of just the visible screen.")]
    all: bool,

    #[arg(long, help = "Keep only the last N lines of the full buffer.")]
    lines: Option<i64>,

    #[arg(
        long,
        help = "Which pane to read: primary/left/top, split/right/bottom, or scratch (even when hidden). Defaults \
                to the on-screen pane."
    )]
    pane: Option<String>,

    #[arg(
        long = "pane-id",
        help = "Stable pane token ($NGTERM_PANE_ID); overrides --pane when it resolves, and an unknown token without \
                --pane is an error."
    )]
    pane_id: Option<String>,

    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Text {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn validate(&self) -> Result<(), String> {
        if self.all && self.lines.is_some() {
            return Err("use either --all or --lines, not both".to_owned());
        }
        if self.lines.is_some_and(|lines| lines <= 0) {
            return Err("--lines must be greater than 0".to_owned());
        }
        validate_pane(self.pane.as_deref())
    }

    fn make_request(&self) -> Request {
        let args = Args {
            pane: self.pane.clone(),
            pane_id: self.pane_id.clone(),
            all: self.all.then_some(true),
            lines: self.lines,
            ..Args::default()
        };
        // upstream always builds the bag, so a bare `session text` sends `"args": {}`
        session_request(
            SessionCmd::Text,
            Some(self.target.target.clone()),
            Some(args),
            &self.options,
        )
    }
}

/// `ngtermctl session search`: at most one of the three steps, and no needle beside `--close`.
#[derive(Debug, clap::Args)]
#[command(about = "Search a session's terminal output (open the bar, set a needle, or step matches).")]
pub struct Search {
    #[arg(help = "Needle to search for (omit to just open the bar).")]
    needle: Option<String>,

    #[arg(long, help = "Step to the next match.")]
    next: bool,

    #[arg(long, help = "Step to the previous match.")]
    prev: bool,

    #[arg(long, help = "Close the search bar.")]
    close: bool,

    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl Search {
    /// `args.to`: the step asked for, if any.
    fn step(&self) -> Option<&'static str> {
        [(self.next, "next"), (self.prev, "prev"), (self.close, "close")]
            .into_iter()
            .find_map(|(set, step)| set.then_some(step))
    }
}

impl RequestCommand for Search {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn validate(&self) -> Result<(), String> {
        if [self.next, self.prev, self.close]
            .into_iter()
            .filter(|set| *set)
            .count()
            > 1
        {
            return Err("--next, --prev, and --close are mutually exclusive".to_owned());
        }
        if self.close && self.needle.is_some() {
            return Err("--close cannot be combined with a needle".to_owned());
        }
        Ok(())
    }

    fn make_request(&self) -> Request {
        let args = Args {
            text: self.needle.clone(),
            to: self.step().map(str::to_owned),
            ..Args::default()
        };
        session_request(
            SessionCmd::Search,
            Some(self.target.target.clone()),
            Some(args),
            &self.options,
        )
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use serde_json::{Value, json};

    use super::*;
    use crate::Ngtermctl;

    fn parsed(argv: &[&str]) -> crate::session::Session {
        let command_line = ["ngtermctl", "session"].iter().chain(argv).copied();
        let crate::Command::Session(group) = Ngtermctl::try_parse_from(command_line).unwrap().command else {
            panic!("not a session command")
        };
        group
    }

    /// `ngtermctl session <argv>` parsed and validated: the request a run sends as JSON, or the refusal.
    fn outcome(argv: &[&str]) -> Result<Value, String> {
        let group = parsed(argv);
        let command = group.command();
        command.validate()?;
        match command.run_request() {
            Ok(request) => Ok(serde_json::to_value(request).unwrap()),
            Err(Failure::Usage(message) | Failure::Local(message)) => Err(message),
            Err(Failure::Exit(code)) => Err(format!("{code:?}")),
        }
    }

    fn request(argv: &[&str]) -> Value {
        outcome(argv).unwrap()
    }

    fn refusal(argv: &[&str]) -> String {
        outcome(argv).unwrap_err()
    }

    fn parses(argv: &[&str]) -> bool {
        let command_line = ["ngtermctl", "session"].iter().chain(argv).copied();
        Ngtermctl::try_parse_from(command_line).is_ok()
    }

    #[derive(Debug, Parser)]
    struct TypeOnly {
        #[command(flatten)]
        command: Type,
    }

    fn type_command(argv: &[&str]) -> Type {
        let command_line = ["type"].iter().chain(argv).copied();
        TypeOnly::try_parse_from(command_line).unwrap().command
    }

    #[test]
    fn type_always_sends_select_and_passes_the_pane_through() {
        assert_eq!(
            request(&["type", "ls\n"]),
            json!({"cmd": "session.type", "target": "active", "args": {"text": "ls\n", "select": false}})
        );
        assert_eq!(
            request(&["type", "hi", "--target", "s1", "--select"]),
            json!({"cmd": "session.type", "target": "s1", "args": {"text": "hi", "select": true}})
        );
        assert_eq!(
            request(&["type", "ls\n", "--pane", "split", "--pane-id", "tok", "--window", "w1"]),
            json!({"cmd": "session.type", "target": "active",
                   "args": {"text": "ls\n", "select": false, "pane": "split", "paneID": "tok", "window": "w1"}})
        );
        assert_eq!(
            request(&["type", "--target", "s1", "--", "--dash"]),
            json!({"cmd": "session.type", "target": "s1", "args": {"text": "--dash", "select": false}})
        );
    }

    #[test]
    fn type_refuses_a_bad_pane_a_missing_payload_and_invalid_utf8() {
        // `other` is a session.focus mode, not a typable pane
        assert_eq!(
            parsed(&["type", "x", "--pane", "other"]).command().validate(),
            Err("--pane must be left, right, or scratch".to_owned())
        );
        assert_eq!(refusal(&["type"]), "provide TEXT or --stdin");
        let stdin = type_command(&["--stdin", "--target", "s1"]);
        assert!(matches!(
            stdin.request_for_input(vec![0xFF]),
            Err(Failure::Usage(message)) if message == "stdin must be valid UTF-8"
        ));
        let request = stdin.request_for_input(b"echo hi\n".to_vec()).ok().unwrap();
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            json!({"cmd": "session.type", "target": "s1", "args": {"text": "echo hi\n", "select": false}})
        );
    }

    #[test]
    fn copy_select_all_and_paste_carry_args_only_when_given() {
        for (verb, cmd) in [("copy", "session.copy"), ("select-all", "session.selectall")] {
            assert_eq!(request(&[verb]), json!({"cmd": cmd, "target": "active"}), "{verb}");
            assert_eq!(
                request(&[verb, "--target", "9f3c", "--window", "w1"]),
                json!({"cmd": cmd, "target": "9f3c", "args": {"window": "w1"}}),
                "{verb}"
            );
            assert!(!parses(&[verb, "--pane", "left"]), "{verb}");
        }
        assert_eq!(request(&["paste"]), json!({"cmd": "session.paste", "target": "active"}));
        assert_eq!(
            request(&["paste", "--pane", "split", "--target", "9f3c"]),
            json!({"cmd": "session.paste", "target": "9f3c", "args": {"pane": "split"}})
        );
        assert_eq!(
            refusal(&["paste", "--pane", "other"]),
            "--pane must be left, right, or scratch"
        );
        assert!(!parses(&["selectall"]));
    }

    #[test]
    fn text_always_sends_an_args_bag_with_its_extent_and_pane() {
        let cases: [(&[&str], Value); 5] = [
            (&[], json!({"target": "active", "args": {}})),
            (&["--all"], json!({"target": "active", "args": {"all": true}})),
            (&["--lines", "50"], json!({"target": "active", "args": {"lines": 50}})),
            (
                &["--pane", "right", "--target", "9f3c"],
                json!({"target": "9f3c", "args": {"pane": "right"}}),
            ),
            (
                &["--pane-id", "stable-token", "--pane", "right", "--window", "w1"],
                json!({"target": "active", "args": {"pane": "right", "paneID": "stable-token", "window": "w1"}}),
            ),
        ];
        for (argv, mut expected) in cases {
            expected["cmd"] = json!("session.text");
            assert_eq!(request(&[&["text"][..], argv].concat()), expected, "{argv:?}");
        }
    }

    #[test]
    fn text_refuses_a_conflicting_or_empty_extent_and_a_bad_pane() {
        let cases: [(&[&str], &str); 3] = [
            (&["--all", "--lines", "10"], "use either --all or --lines, not both"),
            (&["--lines", "0"], "--lines must be greater than 0"),
            (&["--pane", "other"], "--pane must be left, right, or scratch"),
        ];
        for (argv, message) in cases {
            assert_eq!(refusal(&[&["text"][..], argv].concat()), message, "{argv:?}");
        }
        assert!(!parses(&["text", "--lines", "many"]));
    }

    #[test]
    fn search_sends_the_needle_and_one_step() {
        let cases: [(&[&str], Value); 7] = [
            (&["error"], json!({"target": "active", "args": {"text": "error"}})),
            (&[], json!({"target": "active", "args": {}})),
            (&["--next"], json!({"target": "active", "args": {"to": "next"}})),
            (&["--prev"], json!({"target": "active", "args": {"to": "prev"}})),
            (
                &["--close", "--target", "s1"],
                json!({"target": "s1", "args": {"to": "close"}}),
            ),
            (
                &["foo", "--next"],
                json!({"target": "active", "args": {"text": "foo", "to": "next"}}),
            ),
            (
                &["foo", "--window", "w1"],
                json!({"target": "active", "args": {"text": "foo", "window": "w1"}}),
            ),
        ];
        for (argv, mut expected) in cases {
            expected["cmd"] = json!("session.search");
            assert_eq!(request(&[&["search"][..], argv].concat()), expected, "{argv:?}");
        }
    }

    #[test]
    fn search_refuses_two_steps_and_a_needle_with_close() {
        for flags in [["--next", "--close"], ["--next", "--prev"], ["--prev", "--close"]] {
            assert_eq!(
                refusal(&[&["search"][..], &flags].concat()),
                "--next, --prev, and --close are mutually exclusive"
            );
        }
        assert_eq!(
            refusal(&["search", "foo", "--close"]),
            "--close cannot be combined with a needle"
        );
    }
}
