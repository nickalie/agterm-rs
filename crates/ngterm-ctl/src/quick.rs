//! `ngtermctl quick` (`agtermctlKit/MiscCommands.swift` `Quick`): the app's one quick terminal. `visibility` is the
//! group's default subcommand ([`crate::usage::name_default_subcommands`]), so a bare `quick [show|hide|toggle]` keeps
//! working. None of them takes `--window`: the quick terminal belongs to no window.

use std::io::{self, Read};

use ngterm_core::paths::Env;
use ngterm_proto::command::QuickCmd;
use ngterm_proto::{Args, Command, Request};

use crate::client::os_message;
use crate::{BasicOptions, Failure, RequestCommand};

/// `ngtermctl quick <subcommand>`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Quick terminal: visibility, type into it, read its text.",
    subcommand_required = true,
    arg_required_else_help = true
)]
pub struct Quick {
    #[command(subcommand)]
    command: QuickCommand,
}

/// The subcommands, in upstream's `Quick.configuration.subcommands` order.
#[derive(Debug, clap::Subcommand)]
enum QuickCommand {
    Visibility(Visibility),
    #[command(name = "type")]
    Type(Type),
    Text(Text),
}

impl Quick {
    /// The subcommand this invocation runs.
    fn command(&self) -> &dyn RequestCommand {
        match &self.command {
            QuickCommand::Visibility(command) => command,
            QuickCommand::Type(command) => command,
            QuickCommand::Text(command) => command,
        }
    }

    /// What the parser cannot check, as a usage error's message.
    pub fn check(&self) -> Result<(), String> {
        self.command().validate()
    }

    /// Runs the command.
    pub fn execute(&self, env: &dyn Env) -> Result<(), Failure> {
        self.command().default_run(env)
    }
}

/// A `quick.*` request carrying `args`; every subcommand sends its bag.
fn quick_request(cmd: QuickCmd, args: Args) -> Request {
    Request {
        cmd: Command::Quick(cmd),
        target: None,
        args: Some(args),
    }
}

/// `ngtermctl quick [visibility] [show|hide|toggle]`: the mode is the server's to check.
#[derive(Debug, clap::Args)]
#[command(about = "Quick terminal visibility (show|hide|toggle).")]
pub struct Visibility {
    #[arg(default_value = "toggle", help = "Mode: show, hide, or toggle (default).")]
    mode: String,

    #[command(flatten)]
    options: BasicOptions,
}

impl RequestCommand for Visibility {
    fn options(&self) -> &BasicOptions {
        &self.options
    }

    fn make_request(&self) -> Request {
        quick_request(
            QuickCmd::Visibility,
            Args {
                mode: Some(self.mode.clone()),
                ..Args::default()
            },
        )
    }
}

/// `ngtermctl quick type`: literal keystrokes into the quick terminal, the twin of `session type`; the text is the
/// argument, or standard input with `--stdin`.
#[derive(Debug, clap::Args)]
#[command(about = "Inject text into the quick terminal.")]
pub struct Type {
    #[arg(help = "Text to inject (omit with --stdin).")]
    text: Option<String>,

    #[arg(long, help = "Read the text from stdin instead of an argument.")]
    stdin: bool,

    #[command(flatten)]
    options: BasicOptions,
}

impl Type {
    fn request_for(payload: String) -> Request {
        quick_request(
            QuickCmd::Type,
            Args {
                text: Some(payload),
                ..Args::default()
            },
        )
    }

    /// The request for a `--stdin` payload, whose contract is text: an invalid byte refuses it whole.
    fn request_for_input(input: Vec<u8>) -> Result<Request, Failure> {
        let payload = String::from_utf8(input).map_err(|_| Failure::Usage("stdin must be valid UTF-8".to_owned()))?;
        Ok(Self::request_for(payload))
    }
}

impl RequestCommand for Type {
    fn options(&self) -> &BasicOptions {
        &self.options
    }

    /// The request for the TEXT argument; a run goes through [`Self::run_request`], which reads `--stdin` and refuses
    /// a missing payload.
    fn make_request(&self) -> Request {
        Self::request_for(self.text.clone().unwrap_or_default())
    }

    /// Upstream refuses a missing payload only when the run builds the request.
    fn run_request(&self) -> Result<Request, Failure> {
        if self.stdin {
            let mut input = Vec::new();
            io::stdin()
                .read_to_end(&mut input)
                .map_err(|error| Failure::Local(format!("could not read stdin: {}", os_message(&error))))?;
            return Self::request_for_input(input);
        }
        match &self.text {
            Some(text) => Ok(Self::request_for(text.clone())),
            None => Err(Failure::Usage("provide TEXT or --stdin".to_owned())),
        }
    }
}

/// `ngtermctl quick text`: the quick terminal's buffer as plain text, the read-back for `quick type`; one surface, so
/// no `--pane`.
#[derive(Debug, clap::Args)]
#[command(about = "Print the quick terminal's buffer as plain text.")]
pub struct Text {
    #[arg(long, help = "Read the full screen + scrollback instead of just the visible screen.")]
    all: bool,

    #[arg(long, help = "Keep only the last N lines of the full buffer.")]
    lines: Option<i64>,

    #[command(flatten)]
    options: BasicOptions,
}

impl RequestCommand for Text {
    fn options(&self) -> &BasicOptions {
        &self.options
    }

    fn validate(&self) -> Result<(), String> {
        if self.all && self.lines.is_some() {
            return Err("use either --all or --lines, not both".to_owned());
        }
        if self.lines.is_some_and(|lines| lines <= 0) {
            return Err("--lines must be greater than 0".to_owned());
        }
        Ok(())
    }

    fn make_request(&self) -> Request {
        quick_request(
            QuickCmd::Text,
            Args {
                all: self.all.then_some(true),
                lines: self.lines,
                ..Args::default()
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use clap::{CommandFactory, Parser};
    use serde_json::{Value, json};

    use super::*;
    use crate::Ngtermctl;
    use crate::usage::name_default_subcommands;

    /// `ngtermctl quick <argv>` parsed (the default subcommand named first) and validated: the request a run sends as
    /// JSON, or the refusal.
    fn outcome(argv: &[&str]) -> Result<Value, String> {
        let command_line: Vec<OsString> = ["ngtermctl", "quick"].iter().chain(argv).map(OsString::from).collect();
        let named = name_default_subcommands(&Ngtermctl::command(), &command_line);
        let parsed = Ngtermctl::try_parse_from(named).map_err(|error| error.to_string())?;
        let crate::Command::Quick(group) = parsed.command else {
            panic!("not a quick command")
        };
        group.check()?;
        match group.command().run_request() {
            Ok(request) => Ok(serde_json::to_value(request).unwrap()),
            Err(Failure::Usage(message) | Failure::Local(message)) => Err(message),
            Err(Failure::Exit(code)) => Err(format!("{code:?}")),
        }
    }

    fn request(argv: &[&str]) -> Value {
        outcome(argv).unwrap()
    }

    #[test]
    fn visibility_is_the_default_and_defaults_to_toggle() {
        assert_eq!(request(&[]), json!({"cmd": "quick", "args": {"mode": "toggle"}}));
        assert_eq!(request(&["show"]), json!({"cmd": "quick", "args": {"mode": "show"}}));
        assert_eq!(
            request(&["visibility", "hide"]),
            json!({"cmd": "quick", "args": {"mode": "hide"}})
        );
        // the server checks the mode
        assert_eq!(request(&["maybe"]), json!({"cmd": "quick", "args": {"mode": "maybe"}}));
    }

    #[test]
    fn no_quick_command_takes_a_window() {
        for argv in [
            &["--window", "w1"][..],
            &["text", "--window", "w1"],
            &["type", "x", "--window", "w1"],
        ] {
            assert!(outcome(argv).is_err(), "{argv:?}");
        }
    }

    #[test]
    fn type_sends_its_text_and_refuses_a_missing_payload_or_invalid_utf8() {
        assert_eq!(
            request(&["type", "ls\n"]),
            json!({"cmd": "quick.type", "args": {"text": "ls\n"}})
        );
        assert_eq!(
            request(&["type", "--", "--dash"]),
            json!({"cmd": "quick.type", "args": {"text": "--dash"}})
        );
        assert_eq!(outcome(&["type"]), Err("provide TEXT or --stdin".to_owned()));
        assert!(matches!(
            Type::request_for_input(vec![0xFF]),
            Err(Failure::Usage(message)) if message == "stdin must be valid UTF-8"
        ));
        let request = Type::request_for_input(b"echo hi\n".to_vec()).ok().unwrap();
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            json!({"cmd": "quick.type", "args": {"text": "echo hi\n"}})
        );
    }

    #[test]
    fn text_sends_its_extent_in_an_args_bag_and_refuses_a_bad_one() {
        assert_eq!(request(&["text"]), json!({"cmd": "quick.text", "args": {}}));
        assert_eq!(
            request(&["text", "--all"]),
            json!({"cmd": "quick.text", "args": {"all": true}})
        );
        assert_eq!(
            request(&["text", "--lines", "50"]),
            json!({"cmd": "quick.text", "args": {"lines": 50}})
        );
        assert_eq!(
            outcome(&["text", "--all", "--lines", "10"]),
            Err("use either --all or --lines, not both".to_owned())
        );
        assert_eq!(
            outcome(&["text", "--lines", "0"]),
            Err("--lines must be greater than 0".to_owned())
        );
    }
}
