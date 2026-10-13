//! `agtermctl config reload` (`agtermctlKit/MiscCommands.swift` `Config`): app-global (one settings model), so no
//! `--window`. The diagnostic count prints through [`crate::output`]'s `count` branch.

use agterm_core::paths::Env;
use agterm_proto::command::ConfigCmd;
use agterm_proto::{Command, Request};

use crate::{BasicOptions, Failure, RequestCommand};

/// `agtermctl config <subcommand>`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Config commands.",
    subcommand_required = true,
    arg_required_else_help = true
)]
pub struct Config {
    #[command(subcommand)]
    command: ConfigCommand,
}

/// The subcommands, in upstream's `Config.configuration.subcommands` order.
#[derive(Debug, clap::Subcommand)]
enum ConfigCommand {
    Reload(Reload),
}

impl Config {
    /// The subcommand this invocation runs.
    fn command(&self) -> &dyn RequestCommand {
        match &self.command {
            ConfigCommand::Reload(command) => command,
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

/// `agtermctl config reload`: prints the diagnostic count, `ok` for none.
#[derive(Debug, clap::Args)]
#[command(about = "Re-read and apply the agterm-scoped ghostty.conf (prints the diagnostic count).")]
pub struct Reload {
    #[command(flatten)]
    options: BasicOptions,
}

impl RequestCommand for Reload {
    fn options(&self) -> &BasicOptions {
        &self.options
    }

    fn make_request(&self) -> Request {
        Request {
            cmd: Command::Config(ConfigCmd::Reload),
            target: None,
            args: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use serde_json::{Value, json};

    use crate::{Agtermctl, Command as Cli};

    /// `agtermctl config <argv>` parsed and validated: the request a run sends as JSON, or the refusal.
    fn outcome(argv: &[&str]) -> Result<Value, String> {
        let line = ["agtermctl", "config"].iter().chain(argv).copied();
        let parsed = Agtermctl::try_parse_from(line).map_err(|error| error.to_string())?;
        let Cli::Config(group) = parsed.command else {
            panic!("not a config command")
        };
        group.check()?;
        Ok(serde_json::to_value(group.command().make_request()).unwrap())
    }

    #[test]
    fn reload_sends_no_args_and_takes_no_window() {
        assert_eq!(outcome(&["reload"]).unwrap(), json!({"cmd": "config.reload"}));
        assert!(outcome(&["reload", "--window", "w1"]).is_err());
        assert!(outcome(&["reload", "--target", "s1"]).is_err());
    }
}
