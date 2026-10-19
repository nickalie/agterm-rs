//! `ngtermctl ask` (`agtermctlKit/AskCommands.swift` `Ask`): a themed question dialog answered with one of the caller's
//! buttons. `open` is the group's default subcommand, so `ngtermctl ask TITLE --button ...` opens one; a title that is
//! itself `open`, `result` or `cancel` needs the explicit `ask open TITLE`.

use agterm_core::model::OverlayPane;
use agterm_core::paths::Env;
use agterm_proto::command::AskCmd;
use agterm_proto::{Args, AskButton, Command, Request};

use crate::modal::{Family, LiveIo, ModalIo, ModalRunner};
use crate::{BasicOptions, ClientOptions, Failure, RequestCommand};

/// `ngtermctl ask <subcommand>`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Open, poll, or cancel a themed question dialog.",
    long_about = "Use ask open TITLE when the title is 'open', 'result', or 'cancel'.",
    subcommand_required = true,
    arg_required_else_help = true
)]
pub struct Ask {
    #[command(subcommand)]
    command: AskCommand,
}

/// The subcommands, in upstream's `Ask.configuration.subcommands` order. `open` is boxed: its many options would
/// otherwise make every parsed command as large.
#[derive(Debug, clap::Subcommand)]
enum AskCommand {
    Open(Box<Open>),
    Result(ResultCommand),
    Cancel(Cancel),
}

impl Ask {
    /// What the parser cannot check, as a usage error's message.
    pub fn check(&self) -> Result<(), String> {
        match &self.command {
            AskCommand::Open(command) => command.validate(),
            AskCommand::Result(_) | AskCommand::Cancel(_) => Ok(()),
        }
    }

    /// Runs the command.
    pub fn execute(&self, env: &dyn Env) -> Result<(), Failure> {
        match &self.command {
            AskCommand::Open(command) => {
                let mut io = LiveIo {
                    socket: command.options.basic.socket_path(env),
                };
                command.execute(&mut io)
            }
            AskCommand::Result(command) => {
                let mut io = LiveIo {
                    socket: command.options.basic.socket_path(env),
                };
                command.execute(&mut io)
            }
            AskCommand::Cancel(command) => command.default_run(env),
        }
    }
}

/// `ask.open`'s `style` values.
const STYLES: [&str; 2] = ["terminal", "gui"];
/// `ask.open`'s `align` values.
const ALIGNMENTS: [&str; 3] = ["left", "center", "right"];

/// `token` split at its first `=`, `None` without one.
fn split_pair(token: &str) -> Option<(&str, &str)> {
    token.split_once('=')
}

/// `ngtermctl ask [open] TITLE --button ...`: opens the dialog, then waits for the answer unless `--no-block`.
#[derive(Debug, clap::Args)]
#[command(about = "Ask a question and wait for its button answer.")]
pub struct Open {
    #[arg(help = "Dialog title.")]
    title: String,

    #[arg(long, help = "Optional message below the title.")]
    message: Option<String>,

    #[arg(
        long = "button",
        action = clap::ArgAction::Append,
        help = "Button ID=LABEL, or one token for both. Repeat for each button (max 6)."
    )]
    buttons: Vec<String>,

    #[arg(
        long = "hotkey",
        action = clap::ArgAction::Append,
        help = "Button ID=LETTER. Repeat for more buttons."
    )]
    hotkeys: Vec<String>,

    #[arg(long = "default", help = "ID of the initially highlighted button.")]
    default_button: Option<String>,

    #[arg(long, default_value = "terminal", help = "Dialog style: terminal or gui.")]
    style: String,

    #[arg(
        long,
        default_value = "right",
        help = "Button block alignment: left, center, or right."
    )]
    align: String,

    #[arg(
        long,
        allow_hyphen_values = true,
        help = "Panel width as an integer percent of the anchor (10...100); omit for automatic sizing."
    )]
    width: Option<String>,

    #[arg(long = "destructive", help = "ID of the destructive button.")]
    destructive_button: Option<String>,

    #[arg(
        long,
        help = "Session id, prefix, or 'active'. Default: selected session for terminal, window-centered for gui."
    )]
    target: Option<String>,

    #[arg(
        long,
        help = "Anchor to the session's left or right pane. GUI style requires --target."
    )]
    pane: Option<String>,

    #[arg(
        long = "pane-id",
        help = "Stable pane token; overrides --pane. GUI style requires --target."
    )]
    pane_id: Option<String>,

    #[arg(long, help = "Raise the owning window.")]
    follow: bool,

    #[arg(long, help = "Print the dialog id and return without waiting.")]
    no_block: bool,

    #[command(flatten)]
    options: ClientOptions,
}

impl Open {
    /// Upstream's `validate()`, in its order.
    fn validate(&self) -> Result<(), String> {
        if self.title.trim().is_empty() {
            return Err("ask.open requires a title".to_owned());
        }
        if self.style == "gui" && (self.pane.is_some() || self.pane_id.is_some()) && self.target.is_none() {
            return Err("--pane requires a session".to_owned());
        }
        if self
            .pane
            .as_deref()
            .is_some_and(|pane| OverlayPane::from_control_name(pane).is_none())
        {
            return Err("--pane must be left or right".to_owned());
        }
        if !STYLES.contains(&self.style.as_str()) {
            return Err("unknown style".to_owned());
        }
        if !ALIGNMENTS.contains(&self.align.as_str()) {
            return Err("unknown align".to_owned());
        }
        self.parsed_width()?;
        self.parsed_buttons()?;
        Ok(())
    }

    /// `--width` as a percent, refused outside 10...100 or when it is not an integer.
    fn parsed_width(&self) -> Result<Option<i64>, String> {
        let Some(width) = &self.width else {
            return Ok(None);
        };
        match width.parse::<i64>() {
            Ok(value) if (10..=100).contains(&value) => Ok(Some(value)),
            _ => Err("width must be 10 to 100".to_owned()),
        }
    }

    /// The buttons in command-line order, each `--hotkey` assigned to the button it names.
    fn parsed_buttons(&self) -> Result<Vec<AskButton>, String> {
        if self.buttons.is_empty() {
            return Err("ask requires at least one --button".to_owned());
        }
        let mut choices: Vec<AskButton> = self
            .buttons
            .iter()
            .map(|token| {
                let (id, label) = split_pair(token).unwrap_or((token, token));
                AskButton {
                    id: id.to_owned(),
                    label: label.to_owned(),
                    hotkey: None,
                }
            })
            .collect();
        for token in &self.hotkeys {
            let (id, letter) = split_pair(token).ok_or_else(|| "--hotkey requires ID=LETTER".to_owned())?;
            let button = choices
                .iter_mut()
                .find(|button| button.id == id)
                .ok_or_else(|| format!("unknown hotkey button: {id}"))?;
            if button.hotkey.is_some() {
                return Err(format!("hotkey already assigned to button: {id}"));
            }
            button.hotkey = Some(letter.to_owned());
        }
        Ok(choices)
    }

    /// The open request; validation already refused what would fail here.
    fn request(&self) -> Result<Request, Failure> {
        let args = Args {
            follow: self.follow.then_some(true),
            message: self.message.clone(),
            buttons: Some(self.parsed_buttons().map_err(Failure::Usage)?),
            default_button: self.default_button.clone(),
            destructive_button: self.destructive_button.clone(),
            style: Some(self.style.clone()),
            align: Some(self.align.clone()),
            pane: self.pane.clone(),
            pane_id: self.pane_id.clone(),
            title: Some(self.title.clone()),
            width: self.parsed_width().map_err(Failure::Usage)?,
            ..Args::default()
        };
        Ok(Request {
            cmd: Command::Ask(AskCmd::Open),
            target: self.target.clone(),
            args: self.options.with_window(Some(args)),
        })
    }

    /// Opens the dialog over `io` and prints its id or, once it resolves, its result.
    fn execute(&self, io: &mut dyn ModalIo) -> Result<(), Failure> {
        let request = self.request()?;
        ModalRunner {
            family: Family::Ask,
            json: self.options.basic.json,
            io,
        }
        .open(&request, self.no_block)
    }
}

/// `ngtermctl ask result <id>`: one read, printed as JSON, `pending` included.
#[derive(Debug, clap::Args)]
#[command(about = "Print a dialog's current or terminal result as JSON.")]
pub struct ResultCommand {
    #[arg(help = "Exact dialog id returned by ask open.")]
    id: String,

    #[command(flatten)]
    options: ClientOptions,
}

impl ResultCommand {
    fn request(&self) -> Request {
        Request {
            cmd: Command::Ask(AskCmd::Result),
            target: Some(self.id.clone()),
            args: self.options.with_window(None),
        }
    }

    /// Reads the result over `io` and exits with its outcome's status.
    fn execute(&self, io: &mut dyn ModalIo) -> Result<(), Failure> {
        ModalRunner {
            family: Family::Ask,
            json: self.options.basic.json,
            io,
        }
        .read(&self.request())
    }
}

/// `ngtermctl ask cancel <id>`.
#[derive(Debug, clap::Args)]
#[command(about = "Cancel a dialog without answering a button.")]
pub struct Cancel {
    #[arg(help = "Exact dialog id returned by ask open.")]
    id: String,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Cancel {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn make_request(&self) -> Request {
        Request {
            cmd: Command::Ask(AskCmd::Cancel),
            target: Some(self.id.clone()),
            args: self.options.with_window(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::process::ExitCode;

    use clap::{CommandFactory, Parser};
    use serde_json::{Value, json};

    use super::*;
    use crate::Agtermctl;
    use crate::modal::tests::{FakeIo, answer, reply};
    use crate::usage::name_default_subcommands;

    /// `ngtermctl ask <argv>` parsed (the default subcommand named first) and validated.
    fn parse(argv: &[&str]) -> Result<AskCommand, String> {
        let command_line: Vec<OsString> = ["ngtermctl", "ask"].iter().chain(argv).map(OsString::from).collect();
        let named = name_default_subcommands(&Agtermctl::command(), &command_line);
        let parsed = Agtermctl::try_parse_from(named).map_err(|error| error.to_string())?;
        let crate::Command::Ask(group) = parsed.command else {
            panic!("not an ask command")
        };
        group.check()?;
        Ok(group.command)
    }

    fn open(argv: &[&str]) -> Box<Open> {
        match parse(argv) {
            Ok(AskCommand::Open(open)) => open,
            other => panic!("not ask open: {other:?}"),
        }
    }

    fn open_request(argv: &[&str]) -> Value {
        serde_json::to_value(open(argv).request().ok().unwrap()).unwrap()
    }

    #[test]
    fn open_is_the_default_and_blocks_without_a_target() {
        assert_eq!(
            open_request(&["Continue?", "--button", "yes=Yes"]),
            json!({"cmd": "ask.open", "args": {
                "buttons": [{"id": "yes", "label": "Yes"}], "style": "terminal", "align": "right",
                "title": "Continue?"}})
        );
        assert!(!open(&["Continue?", "--button", "yes=Yes"]).no_block);
    }

    #[test]
    fn terminal_pane_placement_needs_no_target() {
        let placements: [(Option<&str>, Option<&str>); 3] = [
            (Some("right"), None),
            (None, Some("pane-token")),
            (Some("left"), Some("pane-token")),
        ];
        for (pane, pane_id) in placements {
            for explicit_style in [false, true] {
                let mut argv = vec!["Continue?", "--button", "yes=Yes", "--window", "window-id"];
                argv.extend(pane.iter().flat_map(|pane| ["--pane", pane]));
                argv.extend(pane_id.iter().flat_map(|token| ["--pane-id", token]));
                if explicit_style {
                    argv.extend(["--style", "terminal"]);
                }
                let mut sent = json!({
                    "buttons": [{"id": "yes", "label": "Yes"}], "style": "terminal", "align": "right",
                    "window": "window-id", "title": "Continue?"});
                if let Some(pane) = pane {
                    sent["pane"] = json!(pane);
                }
                if let Some(token) = pane_id {
                    sent["paneID"] = json!(token);
                }
                assert_eq!(
                    open_request(&argv),
                    json!({"cmd": "ask.open", "args": sent}),
                    "{argv:?}"
                );
            }
        }
    }

    #[test]
    fn open_maps_every_option() {
        let argv = [
            "Keep changes?",
            "--message",
            "Choose an action.",
            "--button",
            "save=Save=keep",
            "--button",
            "no",
            "--button",
            "discard=Discard",
            "--hotkey",
            "save=S",
            "--hotkey",
            "no=n",
            "--default",
            "save",
            "--destructive",
            "discard",
            "--style",
            "gui",
            "--align",
            "left",
            "--target",
            "session-id",
            "--pane",
            "right",
            "--pane-id",
            "pane-token",
            "--window",
            "window-id",
            "--follow",
            "--no-block",
            "--socket",
            "/tmp/ask-cli.sock",
            "--json",
        ];
        assert_eq!(
            open_request(&argv),
            json!({"cmd": "ask.open", "target": "session-id", "args": {
                "follow": true, "message": "Choose an action.",
                "buttons": [
                    {"id": "save", "label": "Save=keep", "hotkey": "S"},
                    {"id": "no", "label": "no", "hotkey": "n"},
                    {"id": "discard", "label": "Discard"}],
                "defaultButton": "save", "destructiveButton": "discard", "style": "gui", "align": "left",
                "window": "window-id", "pane": "right", "paneID": "pane-token", "title": "Keep changes?"}})
        );
        let parsed = open(&argv);
        assert!(parsed.no_block && parsed.options.basic.json);
    }

    #[test]
    fn an_explicit_open_takes_reserved_words_as_titles() {
        for title in ["open", "result", "cancel"] {
            assert_eq!(
                open_request(&["open", title, "--button", "ok"])["args"]["title"],
                json!(title)
            );
        }
    }

    #[test]
    fn width_must_be_an_integer_from_10_to_100() {
        for width in [10, 50, 100] {
            let request = open_request(&["Choose", "--button", "ok", "--width", &width.to_string()]);
            assert_eq!(request["args"]["width"], json!(width));
        }
        for width in ["-1", "0", "9", "101", "50.5", "wide", "", "999999999999999999999"] {
            assert_eq!(
                parse(&["Choose", "--button", "ok", "--width", width]).err(),
                Some("width must be 10 to 100".to_owned()),
                "{width}"
            );
        }
    }

    #[test]
    fn invalid_forms_are_refused_before_sending() {
        let cases: [(&[&str], &str); 11] = [
            (&["Question", "--button", "ok", "--style", "other"], "unknown style"),
            (&["Question", "--button", "ok", "--align", "other"], "unknown align"),
            (&["Question"], "ask requires at least one --button"),
            (&[" ", "--button", "ok"], "ask.open requires a title"),
            (
                &["Question", "--button", "ok", "--hotkey", "o"],
                "--hotkey requires ID=LETTER",
            ),
            (
                &["Question", "--button", "ok", "--hotkey", "missing=x"],
                "unknown hotkey button: missing",
            ),
            (
                &["Question", "--button", "ok", "--hotkey", "ok=o", "--hotkey", "ok=k"],
                "hotkey already assigned to button: ok",
            ),
            (
                &["Question", "--button", "ok", "--style", "gui", "--pane", "right"],
                "--pane requires a session",
            ),
            (
                &["Question", "--button", "ok", "--style", "gui", "--pane-id", "token"],
                "--pane requires a session",
            ),
            (
                &[
                    "Question",
                    "--button",
                    "ok",
                    "--style",
                    "gui",
                    "--pane",
                    "left",
                    "--pane-id",
                    "token",
                ],
                "--pane requires a session",
            ),
            (
                &["Question", "--button", "ok", "--target", "active", "--pane", "scratch"],
                "--pane must be left or right",
            ),
        ];
        for (argv, message) in cases {
            assert_eq!(parse(argv).err(), Some(message.to_owned()), "{argv:?}");
        }
    }

    #[test]
    fn result_and_cancel_send_the_id_and_window() {
        let Ok(AskCommand::Result(result)) = parse(&["result", "ask-id", "--window", "w"]) else {
            panic!("not ask result")
        };
        assert_eq!(
            serde_json::to_value(result.request()).unwrap(),
            json!({"cmd": "ask.result", "target": "ask-id", "args": {"window": "w"}})
        );
        let Ok(AskCommand::Cancel(cancel)) = parse(&["cancel", "ask-id", "--window", "w"]) else {
            panic!("not ask cancel")
        };
        assert_eq!(
            serde_json::to_value(cancel.make_request()).unwrap(),
            json!({"cmd": "ask.cancel", "target": "ask-id", "args": {"window": "w"}})
        );
    }

    #[test]
    fn a_blocking_open_polls_the_global_id_and_prints_one_answer() {
        let mut polls = 0;
        let mut io = FakeIo::new(|request: &Request| {
            if request.cmd == Command::Ask(AskCmd::Open) {
                return Ok(answer(&json!({"id": "ask-id"})));
            }
            polls += 1;
            if polls <= 11 {
                Ok(answer(&json!({"ask": {"result": "pending"}})))
            } else {
                Ok(answer(
                    &json!({"ask": {"result": "answered", "id": "yes", "label": "Yes", "index": 0}}),
                ))
            }
        });
        let outcome = open(&["Continue?", "--button", "yes=Yes", "--window", "w"]).execute(&mut io);
        assert!(outcome.is_ok());
        assert_eq!(io.requests[0]["args"]["window"], json!("w"));
        assert!(
            io.requests[1..]
                .iter()
                .all(|request| *request == json!({"cmd": "ask.result", "target": "ask-id"}))
        );
        assert_eq!(io.delays.len(), 11);
        assert_eq!(
            io.lines,
            [r#"{"result":"answered","id":"yes","label":"Yes","index":0}"#]
        );
    }

    #[test]
    fn a_dismissal_prints_before_its_exit() {
        for (outcome, code) in [("cancelled", 2), ("escaped", 3)] {
            let mut io = FakeIo::new(|request: &Request| {
                if request.cmd == Command::Ask(AskCmd::Open) {
                    Ok(answer(&json!({"id": "ask-id"})))
                } else {
                    Ok(answer(&json!({"ask": {"result": outcome}})))
                }
            });
            let result = open(&["Continue?", "--button", "yes=Yes"]).execute(&mut io);
            assert!(
                matches!(result, Err(Failure::Exit(exit)) if exit == ExitCode::from(code)),
                "{outcome}"
            );
            assert_eq!(io.lines, [format!(r#"{{"result":"{outcome}"}}"#)]);
        }
    }

    #[test]
    fn no_block_prints_only_the_id() {
        let mut io = FakeIo::new(|_: &Request| Ok(answer(&json!({"id": "ask-id", "pane": "right"}))));
        let outcome = open(&["Continue?", "--button", "yes=Yes", "--no-block", "--json"]).execute(&mut io);
        assert!(outcome.is_ok());
        assert_eq!(io.commands(), ["ask.open"]);
        assert_eq!(io.lines, [r#"{"id":"ask-id"}"#]);
    }

    #[test]
    fn a_one_shot_result_prints_every_outcome_and_maps_its_exit() {
        let cases = [
            (json!({"result": "pending"}), Some(1)),
            (
                json!({"result": "answered", "id": "yes", "label": "Yes", "index": 0}),
                None,
            ),
            (json!({"result": "cancelled"}), Some(2)),
            (json!({"result": "cancelled", "reason": "presentation-lost"}), Some(2)),
            (json!({"result": "escaped"}), Some(3)),
        ];
        for (ask, status) in cases {
            let mut io = FakeIo::new(|_: &Request| Ok(answer(&json!({"ask": ask}))));
            let Ok(AskCommand::Result(command)) = parse(&["result", "ask-id", "--window", "w"]) else {
                panic!("not ask result")
            };
            let result = command.execute(&mut io);
            match status {
                None => assert!(result.is_ok(), "{ask}"),
                Some(code) => assert!(
                    matches!(result, Err(Failure::Exit(exit)) if exit == ExitCode::from(code)),
                    "{ask}"
                ),
            }
            assert_eq!(
                io.requests,
                [json!({"cmd": "ask.result", "target": "ask-id", "args": {"window": "w"}})]
            );
            let printed: Value = serde_json::from_str(&io.lines[0]).unwrap();
            assert_eq!((io.lines.len(), printed), (1, ask));
        }
    }

    #[test]
    fn a_refused_ask_follows_the_json_flag() {
        let raw = r#"{"ok": false, "error": "ask already pending", "hint": "unmodeled"}"#;
        for json in [false, true] {
            let mut io = FakeIo::new(|_: &Request| Ok(reply(raw)));
            let mut argv = vec!["Continue?", "--button", "yes=Yes"];
            if json {
                argv.push("--json");
            }
            let result = open(&argv).execute(&mut io);
            assert!(matches!(result, Err(Failure::Exit(exit)) if exit == ExitCode::FAILURE));
            let (lines, errors): (&[&str], &[&str]) = if json {
                (&[raw], &[])
            } else {
                (&[], &["error: ask already pending"])
            };
            assert_eq!(io.lines, lines, "{json}");
            assert_eq!(io.errors, errors, "{json}");
        }
    }
}
