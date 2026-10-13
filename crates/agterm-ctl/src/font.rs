//! `agtermctl font inc|dec|reset` (`agtermctlKit/MiscCommands.swift` `Font`): step or reset one pane's font size. Each
//! takes `--target`, then [`ClientOptions`], then `--pane`, in upstream's declaration order.

use agterm_core::paths::Env;
use agterm_proto::command::FontCmd;
use agterm_proto::{Args, Command, Request};

use crate::session_io::validate_pane;
use crate::{BasicOptions, ClientOptions, Failure, RequestCommand, TargetOptions};

/// Help for the shared `--pane` option (upstream's `Font.paneHelp`).
const PANE_HELP: &str = "Which pane's font to change: left (main), right (split), or scratch (the session's scratch \
                         terminal, even when hidden). primary/left/top and split/right/bottom are aliases. Defaults to \
                         the left pane.";

/// `agtermctl font <subcommand>`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Font size commands.",
    subcommand_required = true,
    arg_required_else_help = true
)]
pub struct Font {
    #[command(subcommand)]
    command: FontCommand,
}

/// The subcommands, in upstream's `Font.configuration.subcommands` order.
#[derive(Debug, clap::Subcommand)]
enum FontCommand {
    #[command(about = "Increase font size.")]
    Inc(Step),
    #[command(about = "Decrease font size.")]
    Dec(Step),
    #[command(about = "Reset font size.")]
    Reset(Step),
}

impl Font {
    /// The subcommand this invocation runs, paired with its wire command.
    fn command(&self) -> FontRequest<'_> {
        match &self.command {
            FontCommand::Inc(step) => FontRequest {
                cmd: FontCmd::Inc,
                step,
            },
            FontCommand::Dec(step) => FontRequest {
                cmd: FontCmd::Dec,
                step,
            },
            FontCommand::Reset(step) => FontRequest {
                cmd: FontCmd::Reset,
                step,
            },
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

/// The options every font subcommand takes.
#[derive(Debug, clap::Args)]
pub struct Step {
    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,

    #[arg(long, help = PANE_HELP)]
    pane: Option<String>,
}

/// One font subcommand: its wire command and its options.
struct FontRequest<'a> {
    cmd: FontCmd,
    step: &'a Step,
}

impl RequestCommand for FontRequest<'_> {
    fn options(&self) -> &BasicOptions {
        &self.step.options.basic
    }

    fn validate(&self) -> Result<(), String> {
        validate_pane(self.step.pane.as_deref())
    }

    /// The pane, if given, as the only arg besides `--window`; no args at all without either.
    fn make_request(&self) -> Request {
        let pane = self.step.pane.as_ref().map(|pane| Args {
            pane: Some(pane.clone()),
            ..Args::default()
        });
        Request {
            cmd: Command::Font(self.cmd),
            target: Some(self.step.target.target.clone()),
            args: self.step.options.with_window(pane),
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use serde_json::{Value, json};

    use crate::{Agtermctl, Command};

    /// `agtermctl font <argv>` parsed and validated: the request a run sends as JSON, or the refusal.
    fn outcome(argv: &[&str]) -> Result<Value, String> {
        let line = ["agtermctl", "font"].iter().chain(argv).copied();
        let parsed = Agtermctl::try_parse_from(line).map_err(|error| error.to_string())?;
        let Command::Font(group) = parsed.command else {
            panic!("not a font command")
        };
        group.check()?;
        Ok(serde_json::to_value(crate::RequestCommand::make_request(&group.command())).unwrap())
    }

    fn request(argv: &[&str]) -> Value {
        outcome(argv).unwrap()
    }

    #[test]
    fn each_step_targets_the_active_session_without_args() {
        for (verb, cmd) in [("inc", "font.inc"), ("dec", "font.dec"), ("reset", "font.reset")] {
            assert_eq!(request(&[verb]), json!({"cmd": cmd, "target": "active"}), "{verb}");
        }
        assert_eq!(
            request(&["inc", "--target", "s1"]),
            json!({"cmd": "font.inc", "target": "s1"})
        );
    }

    #[test]
    fn the_pane_and_window_go_in_args_as_typed() {
        assert_eq!(
            request(&["inc", "--pane", "right", "--target", "s1"]),
            json!({"cmd": "font.inc", "target": "s1", "args": {"pane": "right"}})
        );
        assert_eq!(
            request(&["dec", "--pane", "scratch"]),
            json!({"cmd": "font.dec", "target": "active", "args": {"pane": "scratch"}})
        );
        assert_eq!(
            request(&["reset", "--pane", "left", "--window", "w1"]),
            json!({"cmd": "font.reset", "target": "active", "args": {"pane": "left", "window": "w1"}})
        );
        assert_eq!(
            request(&["inc", "--window", "w1"]),
            json!({"cmd": "font.inc", "target": "active", "args": {"window": "w1"}})
        );
        assert_eq!(
            request(&["dec", "--pane", "bottom"]),
            json!({"cmd": "font.dec", "target": "active", "args": {"pane": "bottom"}})
        );
    }

    #[test]
    fn an_unknown_pane_is_refused() {
        for pane in ["middle", "other"] {
            assert_eq!(
                outcome(&["inc", "--pane", pane]),
                Err("--pane must be left, right, or scratch".to_owned())
            );
        }
    }

    #[test]
    fn a_bare_group_or_unknown_verb_does_not_parse() {
        assert!(outcome(&[]).is_err());
        assert!(outcome(&["bigger"]).is_err());
    }
}
