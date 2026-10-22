//! `ngtermctl theme set|list` (`agtermctlKit/MiscCommands.swift` `Theme`): the app-global terminal theme, so neither
//! takes `--window`. `list` prints through [`crate::output`]'s theme listing.

use ngterm_core::paths::Env;
use ngterm_proto::command::ThemeCmd;
use ngterm_proto::{Args, Command, Request};

use crate::{BasicOptions, Failure, RequestCommand};

/// `ngtermctl theme <subcommand>`.
#[derive(Debug, clap::Args)]
#[command(about = "Theme commands.", subcommand_required = true, arg_required_else_help = true)]
pub struct Theme {
    #[command(subcommand)]
    command: ThemeCommand,
}

/// The subcommands, in upstream's `Theme.configuration.subcommands` order.
#[derive(Debug, clap::Subcommand)]
enum ThemeCommand {
    Set(Set),
    List(List),
}

impl Theme {
    /// The subcommand this invocation runs.
    fn command(&self) -> &dyn RequestCommand {
        match &self.command {
            ThemeCommand::Set(command) => command,
            ThemeCommand::List(command) => command,
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

/// `ngtermctl theme set [NAME] [--light L] [--dark D]`: the names are the server's to check.
#[derive(Debug, clap::Args)]
#[command(
    about = "Set + persist the terminal theme, per slot.",
    long_about = "theme set NAME            set the light/single theme (a dark theme, if set, is kept)\n\
                  theme set --dark NAME     set the dark theme — the terminal then tracks the macOS Light/Dark \
                  appearance (the light side seeds from the current theme)\n\
                  theme set --dark none     clear the dark theme (stop tracking the appearance)\n\
                  theme set                 ghostty's built-in default (clears everything)"
)]
pub struct Set {
    #[arg(help = "Light/single theme name (a bundled theme); omit for ghostty's built-in default.")]
    name: Option<String>,

    #[arg(long, help = "Light-appearance theme (same slot as NAME).")]
    light: Option<String>,

    #[arg(long, help = "Dark-appearance theme, or 'none' to clear it.")]
    dark: Option<String>,

    #[command(flatten)]
    options: BasicOptions,
}

impl RequestCommand for Set {
    fn options(&self) -> &BasicOptions {
        &self.options
    }

    fn validate(&self) -> Result<(), String> {
        if self.name.is_some() && self.light.is_some() {
            Err("Pass either a NAME or --light, not both.".to_owned())
        } else {
            Ok(())
        }
    }

    /// Always carries its args bag, empty for a bare `theme set` (ghostty's built-in default), as upstream.
    fn make_request(&self) -> Request {
        let args = Args {
            name: self.name.clone(),
            light: self.light.clone(),
            dark: self.dark.clone(),
            ..Args::default()
        };
        Request {
            cmd: Command::Theme(ThemeCmd::Set),
            target: None,
            args: Some(args),
        }
    }
}

/// `ngtermctl theme list`.
#[derive(Debug, clap::Args)]
#[command(about = "List bundled themes (the current one marked).")]
pub struct List {
    #[command(flatten)]
    options: BasicOptions,
}

impl RequestCommand for List {
    fn options(&self) -> &BasicOptions {
        &self.options
    }

    fn make_request(&self) -> Request {
        Request {
            cmd: Command::Theme(ThemeCmd::List),
            target: None,
            args: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use serde_json::{Value, json};

    use crate::{Command, Ngtermctl};

    /// `ngtermctl theme <argv>` parsed and validated: the request a run sends as JSON, or the refusal.
    fn outcome(argv: &[&str]) -> Result<Value, String> {
        let line = ["ngtermctl", "theme"].iter().chain(argv).copied();
        let parsed = Ngtermctl::try_parse_from(line).map_err(|error| error.to_string())?;
        let Command::Theme(group) = parsed.command else {
            panic!("not a theme command")
        };
        group.check()?;
        Ok(serde_json::to_value(group.command().make_request()).unwrap())
    }

    fn request(argv: &[&str]) -> Value {
        outcome(argv).unwrap()
    }

    #[test]
    fn set_sends_the_name_or_an_empty_bag_for_the_default() {
        assert_eq!(
            request(&["set", "Dracula"]),
            json!({"cmd": "theme.set", "args": {"name": "Dracula"}})
        );
        assert_eq!(request(&["set"]), json!({"cmd": "theme.set", "args": {}}));
    }

    #[test]
    fn set_sends_each_slot_as_typed() {
        assert_eq!(
            request(&["set", "--light", "Builtin Light", "--dark", "Gruvbox Dark"]),
            json!({"cmd": "theme.set", "args": {"light": "Builtin Light", "dark": "Gruvbox Dark"}})
        );
        assert_eq!(
            request(&["set", "--dark", "none"]),
            json!({"cmd": "theme.set", "args": {"dark": "none"}})
        );
        assert_eq!(
            request(&["set", "Nord", "--dark", "Gruvbox Dark"]),
            json!({"cmd": "theme.set", "args": {"name": "Nord", "dark": "Gruvbox Dark"}})
        );
    }

    #[test]
    fn set_refuses_a_name_beside_light() {
        assert_eq!(
            outcome(&["set", "Dracula", "--light", "Builtin Light"]),
            Err("Pass either a NAME or --light, not both.".to_owned())
        );
    }

    #[test]
    fn list_sends_no_args() {
        assert_eq!(request(&["list"]), json!({"cmd": "theme.list"}));
    }

    #[test]
    fn neither_takes_a_window_or_target() {
        assert!(outcome(&["set", "Nord", "--window", "w1"]).is_err());
        assert!(outcome(&["list", "--window", "w1"]).is_err());
        assert!(outcome(&["list", "--target", "s1"]).is_err());
    }
}
