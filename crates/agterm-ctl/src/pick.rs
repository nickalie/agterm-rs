//! `agtermctl pick` (`agtermctlKit/MiscCommands.swift` `Pick`): the native fuzzy picker. `open` is the group's default
//! subcommand, so the shell-friendly common case is `printf 'one\ntwo\n' | agtermctl pick`; `result` and `cancel` make
//! `--no-block` usable without speaking the protocol.

use std::io::{self, Read};

use agterm_core::paths::Env;
use agterm_proto::command::PickCmd;
use agterm_proto::{Args, Command, PickItem, Request};

use crate::client::os_message;
use crate::modal::{Family, LiveIo, ModalIo, ModalRunner};
use crate::{BasicOptions, ClientOptions, Failure, RequestCommand};

/// `agtermctl pick <subcommand>`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Open, poll, or cancel a native fuzzy picker.",
    subcommand_required = true,
    arg_required_else_help = true
)]
pub struct Pick {
    #[command(subcommand)]
    command: PickCommand,
}

/// The subcommands, in upstream's `Pick.configuration.subcommands` order.
#[derive(Debug, clap::Subcommand)]
enum PickCommand {
    Open(Open),
    Result(ResultCommand),
    Cancel(Cancel),
}

impl Pick {
    /// Runs the command.
    pub fn execute(&self, env: &dyn Env) -> Result<(), Failure> {
        match &self.command {
            PickCommand::Open(command) => {
                let input = read_stdin()?;
                let mut io = LiveIo {
                    socket: command.options.basic.socket_path(env),
                };
                command.execute(&input, &mut io)
            }
            PickCommand::Result(command) => {
                let mut io = LiveIo {
                    socket: command.options.basic.socket_path(env),
                };
                command.execute(&mut io)
            }
            PickCommand::Cancel(command) => command.default_run(env),
        }
    }
}

/// Standard input, whole.
fn read_stdin() -> Result<Vec<u8>, Failure> {
    let mut input = Vec::new();
    io::stdin()
        .read_to_end(&mut input)
        .map_err(|error| Failure::Local(format!("could not read stdin: {}", os_message(&error))))?;
    Ok(input)
}

/// What separates bare lines: Foundation's `CharacterSet.newlines`.
fn is_newline(c: char) -> bool {
    matches!(
        c,
        '\n' | '\u{0B}' | '\u{0C}' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}'
    )
}

/// The items on stdin. Its first non-whitespace byte decides: `[` is a JSON item array, keeping caller-supplied ids
/// and subtitles; anything else is lines, each its own id and label, blank ones dropped.
pub fn parse_items(input: &[u8]) -> Result<Vec<PickItem>, Failure> {
    let first = input.iter().find(|byte| !matches!(byte, b' ' | b'\t' | b'\n' | b'\r'));
    if first == Some(&b'[') {
        return serde_json::from_slice(input)
            .map_err(|error| Failure::Local(format!("stdin is not a valid JSON item array: {error}")));
    }
    let text = std::str::from_utf8(input)
        .map_err(|_| Failure::Usage("stdin must be UTF-8 text or a JSON item array".to_owned()))?;
    Ok(text
        .split(is_newline)
        .filter(|line| !line.trim().is_empty())
        .map(|line| PickItem {
            id: line.to_owned(),
            label: line.to_owned(),
            subtitle: None,
        })
        .collect())
}

/// `agtermctl pick [open]`: reads the choices from stdin, then waits for the answer unless `--no-block`.
#[derive(Debug, clap::Args)]
#[command(about = "Read choices from stdin and open a native fuzzy picker.")]
pub struct Open {
    #[arg(long, help = "Placeholder text shown in the picker query field.")]
    prompt: Option<String>,

    #[arg(long, help = "Initial text for the picker query field; it opens already filtered.")]
    query: Option<String>,

    #[arg(long, help = "Accept the current query as a custom result.")]
    allow_custom: bool,

    #[arg(
        long,
        help = "Item id to open highlighted; a --query that hides it falls back to the first row."
    )]
    select: Option<String>,

    #[arg(long, help = "Raise the target window when the picker opens.")]
    follow: bool,

    #[arg(long, help = "Print the picker id and return without waiting for a result.")]
    no_block: bool,

    #[command(flatten)]
    options: ClientOptions,
}

impl Open {
    /// The open request for the items in `input`.
    pub fn request(&self, input: &[u8]) -> Result<Request, Failure> {
        let args = Args {
            follow: self.follow.then_some(true),
            items: Some(parse_items(input)?),
            prompt: self.prompt.clone(),
            query: self.query.clone(),
            allow_custom: self.allow_custom.then_some(true),
            selection: self.select.clone(),
            ..Args::default()
        };
        Ok(Request {
            cmd: Command::Pick(PickCmd::Open),
            target: None,
            args: self.options.with_window(Some(args)),
        })
    }

    /// Opens the picker over `io` and prints its id or, once it resolves, its result.
    pub fn execute(&self, input: &[u8], io: &mut dyn ModalIo) -> Result<(), Failure> {
        let request = self.request(input)?;
        ModalRunner {
            family: Family::Pick,
            json: self.options.basic.json,
            io,
        }
        .open(&request, self.no_block)
    }
}

/// `agtermctl pick result <id>`: one read, printed as JSON, `pending` included.
#[derive(Debug, clap::Args)]
#[command(about = "Print a picker's current or terminal result as JSON.")]
pub struct ResultCommand {
    #[arg(help = "Exact picker id returned by pick open.")]
    id: String,

    #[command(flatten)]
    options: ClientOptions,
}

impl ResultCommand {
    fn request(&self) -> Request {
        Request {
            cmd: Command::Pick(PickCmd::Result),
            target: Some(self.id.clone()),
            args: self.options.with_window(None),
        }
    }

    /// Reads the result over `io` and exits with its outcome's status.
    pub fn execute(&self, io: &mut dyn ModalIo) -> Result<(), Failure> {
        ModalRunner {
            family: Family::Pick,
            json: self.options.basic.json,
            io,
        }
        .read(&self.request())
    }
}

/// `agtermctl pick cancel <id>`.
#[derive(Debug, clap::Args)]
#[command(about = "Cancel a pending picker.")]
pub struct Cancel {
    #[arg(help = "Exact picker id returned by pick open.")]
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
            cmd: Command::Pick(PickCmd::Cancel),
            target: Some(self.id.clone()),
            args: self.options.with_window(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use clap::{CommandFactory, Parser};
    use serde_json::{Value, json};

    use super::*;
    use crate::Agtermctl;
    use crate::modal::tests::{FakeIo, answer};
    use crate::usage::name_default_subcommands;

    fn parse(argv: &[&str]) -> Result<PickCommand, String> {
        let command_line: Vec<OsString> = ["agtermctl", "pick"].iter().chain(argv).map(OsString::from).collect();
        let named = name_default_subcommands(&Agtermctl::command(), &command_line);
        let parsed = Agtermctl::try_parse_from(named).map_err(|error| error.to_string())?;
        let crate::Command::Pick(group) = parsed.command else {
            panic!("not a pick command")
        };
        Ok(group.command)
    }

    fn open(argv: &[&str]) -> Open {
        let Ok(PickCommand::Open(open)) = parse(argv) else {
            panic!("not pick open")
        };
        open
    }

    fn open_request(argv: &[&str], input: &str) -> Value {
        serde_json::to_value(open(argv).request(input.as_bytes()).ok().unwrap()).unwrap()
    }

    fn items(input: &[u8]) -> Vec<PickItem> {
        parse_items(input).ok().unwrap()
    }

    fn item(id: &str, label: &str) -> PickItem {
        PickItem {
            id: id.to_owned(),
            label: label.to_owned(),
            subtitle: None,
        }
    }

    #[test]
    fn open_is_the_default_and_has_no_timeout() {
        assert!(matches!(parse(&[]), Ok(PickCommand::Open(_))));
        assert!(matches!(parse(&["--no-block"]), Ok(PickCommand::Open(_))));
        assert!(parse(&["--timeout", "60"]).is_err());
    }

    #[test]
    fn a_json_array_keeps_ids_and_subtitles() {
        let input =
            b"  [\n {\"id\":\"one\",\"label\":\"One\",\"subtitle\":\"First\"},\n {\"id\":\"two\",\"label\":\"Two\"}\n]";
        let first = PickItem {
            subtitle: Some("First".to_owned()),
            ..item("one", "One")
        };
        assert_eq!(items(input), [first, item("two", "Two")]);
        assert!(matches!(parse_items(b"[{\"id\":\"one\""), Err(Failure::Local(_))));
    }

    #[test]
    fn bare_lines_are_their_own_ids_and_blank_ones_are_dropped() {
        assert_eq!(items(b"One\nTwo\n"), [item("One", "One"), item("Two", "Two")]);
        assert_eq!(
            items(b"\nOne\n \t \n\nTwo\r\n"),
            [item("One", "One"), item("Two", "Two")]
        );
        // the whole line is the id: `=` is not a separator
        assert_eq!(items(b"a=Alpha\n"), [item("a=Alpha", "a=Alpha")]);
        assert!(items(b"").is_empty());
        assert!(matches!(
            parse_items(&[0xFF, b'\n']),
            Err(Failure::Usage(message)) if message == "stdin must be UTF-8 text or a JSON item array"
        ));
    }

    #[test]
    fn open_maps_every_option() {
        let argv = [
            "--prompt",
            "Choose one",
            "--query",
            "on",
            "--allow-custom",
            "--select",
            "One",
            "--follow",
            "--window",
            "w1",
            "--no-block",
        ];
        assert_eq!(
            open_request(&argv, "One\n"),
            json!({"cmd": "pick.open", "args": {
                "items": [{"id": "One", "label": "One"}], "prompt": "Choose one", "query": "on",
                "allowCustom": true, "selection": "One", "follow": true, "window": "w1"}})
        );
        assert!(open(&argv).no_block);
        assert!(!open(&[]).no_block);
        assert_eq!(
            open_request(&[], "One\n"),
            json!({"cmd": "pick.open", "args": {"items": [{"id": "One", "label": "One"}]}})
        );
        assert_eq!(
            open_request(&["--allow-custom", "--query", "old name"], ""),
            json!({"cmd": "pick.open", "args": {"items": [], "query": "old name", "allowCustom": true}})
        );
    }

    #[test]
    fn result_and_cancel_send_the_id_and_window() {
        let Ok(PickCommand::Result(result)) = parse(&["result", "pick-1", "--window", "w1"]) else {
            panic!("not pick result")
        };
        assert_eq!(
            serde_json::to_value(result.request()).unwrap(),
            json!({"cmd": "pick.result", "target": "pick-1", "args": {"window": "w1"}})
        );
        let Ok(PickCommand::Cancel(cancel)) = parse(&["cancel", "pick-1", "--window", "w1"]) else {
            panic!("not pick cancel")
        };
        assert_eq!(
            serde_json::to_value(cancel.make_request()).unwrap(),
            json!({"cmd": "pick.cancel", "target": "pick-1", "args": {"window": "w1"}})
        );
    }

    #[test]
    fn a_blocking_open_sends_the_items_then_polls_without_the_window() {
        let mut io = FakeIo::new(|request: &Request| {
            if request.cmd == Command::Pick(PickCmd::Open) {
                Ok(answer(&json!({"id": "P1"})))
            } else {
                Ok(answer(&json!({"pick": {"result": "custom", "query": "typed"}})))
            }
        });
        let outcome = open(&["--window", "w1"]).execute(b"a\nb\n", &mut io);
        assert!(outcome.is_ok());
        assert_eq!(
            io.requests,
            [
                json!({"cmd": "pick.open", "args": {"items": [{"id": "a", "label": "a"}, {"id": "b", "label": "b"}],
                    "window": "w1"}}),
                json!({"cmd": "pick.result", "target": "P1"}),
            ]
        );
        assert_eq!(io.lines, [r#"{"result":"custom","query":"typed"}"#]);
    }
}
