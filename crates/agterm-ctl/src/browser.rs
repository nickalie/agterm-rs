//! `ngtermctl browser clear|links` (`agtermctlKit/MiscCommands.swift` `Browser`): the saved browser store of
//! `--persistent` URL overlays and where a clicked terminal link opens. Both are app-wide, so neither takes `--window`;
//! `links` prints the effective mode through [`crate::output`]'s `text` branch.

use agterm_core::paths::Env;
use agterm_core::persist::settings::LinkOpenMode;
use agterm_proto::command::BrowserCmd;
use agterm_proto::{Args, Command, Request};

use crate::{BasicOptions, Failure, RequestCommand};

/// `ngtermctl browser <subcommand>`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Saved browser storage of --persistent URL overlays, and where clicked links open.",
    subcommand_required = true,
    arg_required_else_help = true
)]
pub struct Browser {
    #[command(subcommand)]
    command: BrowserCommand,
}

/// The subcommands, in upstream's `Browser.configuration.subcommands` order.
#[derive(Debug, clap::Subcommand)]
enum BrowserCommand {
    Clear(Clear),
    Links(Links),
}

impl Browser {
    /// The subcommand this invocation runs.
    fn command(&self) -> &dyn RequestCommand {
        match &self.command {
            BrowserCommand::Clear(command) => command,
            BrowserCommand::Links(command) => command,
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

/// A `browser.*` request: neither takes a target.
fn browser_request(cmd: BrowserCmd, args: Option<Args>) -> Request {
    Request {
        cmd: Command::Browser(cmd),
        target: None,
        args,
    }
}

/// `ngtermctl browser clear`: one store serves every window, so no `--window`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Remove every cookie and all site data the saved browser store holds.",
    long_about = "Refused while a --persistent page is open, one in a just-closed session that can still be restored \
                  included: an open page writes its login back. Removing the local data does not sign you out on the \
                  server."
)]
pub struct Clear {
    #[command(flatten)]
    options: BasicOptions,
}

impl RequestCommand for Clear {
    fn options(&self) -> &BasicOptions {
        &self.options
    }

    fn make_request(&self) -> Request {
        browser_request(BrowserCmd::Clear, None)
    }
}

/// `ngtermctl browser links [browser|overlay]`: no mode reads the setting.
#[derive(Debug, clap::Args)]
#[command(
    about = "Set or read where a clicked web link in a terminal opens (browser|overlay).",
    long_about = "overlay opens http and https links in a full session web overlay with JavaScript, navigation \
                  buttons and the saved browser store; with no MODE the current one is printed. A link in a HUD, a \
                  link clicked while a HUD is up or the terminal is zoomed, and a link the overlay cannot show open \
                  in the browser."
)]
pub struct Links {
    #[arg(help = "Mode: browser or overlay. Omit to read the current mode.")]
    mode: Option<String>,

    #[command(flatten)]
    options: BasicOptions,
}

impl RequestCommand for Links {
    fn options(&self) -> &BasicOptions {
        &self.options
    }

    fn validate(&self) -> Result<(), String> {
        match &self.mode {
            Some(mode) if LinkOpenMode::from_raw(mode).is_none() => Err("mode must be browser or overlay".to_owned()),
            _ => Ok(()),
        }
    }

    fn make_request(&self) -> Request {
        let args = self.mode.as_ref().map(|mode| Args {
            mode: Some(mode.clone()),
            ..Args::default()
        });
        browser_request(BrowserCmd::Links, args)
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use serde_json::{Value, json};

    use crate::{Agtermctl, Command as Cli};

    /// `ngtermctl browser <argv>` parsed and validated: the request a run sends as JSON, or the refusal.
    fn outcome(argv: &[&str]) -> Result<Value, String> {
        let line = ["ngtermctl", "browser"].iter().chain(argv).copied();
        let parsed = Agtermctl::try_parse_from(line).map_err(|error| error.to_string())?;
        let Cli::Browser(group) = parsed.command else {
            panic!("not a browser command")
        };
        group.check()?;
        Ok(serde_json::to_value(group.command().make_request()).unwrap())
    }

    #[test]
    fn clear_sends_no_args_and_takes_no_window() {
        assert_eq!(outcome(&["clear"]).unwrap(), json!({"cmd": "browser.clear"}));
        assert!(outcome(&["clear", "--window", "w1"]).is_err());
        assert!(outcome(&["clear", "--target", "s1"]).is_err());
    }

    #[test]
    fn links_sends_its_mode_or_none_and_takes_no_window() {
        assert_eq!(outcome(&["links"]).unwrap(), json!({"cmd": "browser.links"}));
        for mode in ["browser", "overlay"] {
            assert_eq!(
                outcome(&["links", mode]).unwrap(),
                json!({"cmd": "browser.links", "args": {"mode": mode}})
            );
        }
        assert_eq!(
            outcome(&["links", "tab"]).unwrap_err(),
            "mode must be browser or overlay"
        );
        assert!(outcome(&["links", "overlay", "--window", "w1"]).is_err());
    }
}
