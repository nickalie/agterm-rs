//! `agtermctl restore capture|clear|mode` (`agtermctlKit/MiscCommands.swift` `Restore`): the captured foreground
//! commands and the restore-mode policy. All three are app-global, so none takes `--window`. `capture` prints its own
//! `result.text`, `mode` the policy through [`crate::output`]'s `restore` branch.

use agterm_core::paths::Env;
use agterm_core::persist::settings::RestoreMode;
use agterm_proto::command::RestoreCmd;
use agterm_proto::{Args, Command, Request};

use crate::{BasicOptions, Failure, RequestCommand};

/// `agtermctl restore <subcommand>`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Restore-running-command commands.",
    subcommand_required = true,
    arg_required_else_help = true
)]
pub struct Restore {
    #[command(subcommand)]
    command: RestoreCommand,
}

/// The subcommands, in upstream's `Restore.configuration.subcommands` order.
#[derive(Debug, clap::Subcommand)]
enum RestoreCommand {
    Capture(Capture),
    Clear(Clear),
    Mode(Mode),
}

impl Restore {
    /// The subcommand this invocation runs.
    fn command(&self) -> &dyn RequestCommand {
        match &self.command {
            RestoreCommand::Capture(command) => command,
            RestoreCommand::Clear(command) => command,
            RestoreCommand::Mode(command) => command,
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

/// A `restore.*` request: none takes a target.
fn restore_request(cmd: RestoreCmd, args: Option<Args>) -> Request {
    Request {
        cmd: Command::Restore(cmd),
        target: None,
        args,
    }
}

/// `agtermctl restore capture`: app-global, like `restore clear`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Capture every pane's running command now, so a forced exit restores it.",
    long_about = "agterm captures the running commands when it quits, so an exit that never gets there leaves every \
                  pane restoring a plain shell: a force quit, a crash, a hard reset, a power loss. This runs the same \
                  capture on demand, app-global across every open window, and prints how many panes had a command to \
                  capture. Consumption is unchanged: the next launch arms each captured command once and clears it.\n\
                  \n\
                  A capture is only as fresh as its last run, so a pager or a build that has finished since still \
                  re-runs after a crash. \"restore clear\" drops every captured command, and restore-denylist.conf in \
                  the config directory keeps a named program from re-running at all.\n\
                  \n\
                  Typed at a prompt it records ITSELF: while it runs it is that pane's foreground process, so that \
                  pane comes back running the capture command. Bind it or run it from a scheduled job rather than by \
                  hand; \"restore clear\" is app-global, so it is no per-pane undo.\n\
                  \n\
                  This command is available only when this launch is in rerun mode. In fresh-shell or live mode it \
                  fails and names the active mode."
)]
pub struct Capture {
    #[command(flatten)]
    options: BasicOptions,
}

impl RequestCommand for Capture {
    fn options(&self) -> &BasicOptions {
        &self.options
    }

    fn make_request(&self) -> Request {
        restore_request(RestoreCmd::Capture, None)
    }
}

/// `agtermctl restore clear`: clears every open window, so no `--window`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Clear every session's saved foreground command so the next restart restores plain shells.",
    long_about = "This is app-global and CAPTURE-scoped: it drops the foreground commands agterm captured at quit, \
                  across every open window, and leaves per-session restore-command overrides alone.\n\
                  \n\
                  Not to be confused with `agtermctl session restore --clear`, which is per-session and \
                  OVERRIDE-scoped: it drops one pane's pinned command so that pane goes back to auto-capture."
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
        restore_request(RestoreCmd::Clear, None)
    }
}

/// `agtermctl restore mode [none|rerun|live]`: no mode reads the policy.
#[derive(Debug, clap::Args)]
#[command(
    about = "Read or set what a restart does with your sessions.",
    long_about = "With no argument this reports the policy: what settings hold for the next launch, what THIS launch \
                  asked for, and what it actually got. The two requested values differ once the mode has been changed \
                  since this instance started, which is exactly when a caller is confused about why nothing happened.\n\
                  \n\
                  Setting one writes it for the NEXT launch. This process keeps the mode it started with, and that is \
                  not a shortcut: a pane is wrapped in a live-session daemon or not at the moment it is created, so \
                  no setting can retrofit a shell that is already running.\n\
                  \n\
                  fresh shells (none) re-spawns each pane in its saved directory. re-run (rerun) starts the command \
                  each pane had at the last clean quit. live keeps the actual processes alive.\n\
                  \n\
                  Switching away from live and restarting ends every detached live process in this state directory. \
                  If live was requested but could not be used, the reason is reported here."
)]
pub struct Mode {
    #[arg(help = "none|rerun|live. Omit to read the current policy.")]
    mode: Option<String>,

    #[command(flatten)]
    options: BasicOptions,
}

impl RequestCommand for Mode {
    fn options(&self) -> &BasicOptions {
        &self.options
    }

    fn validate(&self) -> Result<(), String> {
        match &self.mode {
            Some(mode) if RestoreMode::from_wire(mode).is_none() => Err("mode must be none, rerun, or live".to_owned()),
            _ => Ok(()),
        }
    }

    fn make_request(&self) -> Request {
        let args = self.mode.as_ref().map(|mode| Args {
            mode: Some(mode.clone()),
            ..Args::default()
        });
        restore_request(RestoreCmd::Mode, args)
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use serde_json::{Value, json};

    use crate::{Agtermctl, Command as Cli};

    /// `agtermctl restore <argv>` parsed and validated: the request a run sends as JSON, or the refusal.
    fn outcome(argv: &[&str]) -> Result<Value, String> {
        let line = ["agtermctl", "restore"].iter().chain(argv).copied();
        let parsed = Agtermctl::try_parse_from(line).map_err(|error| error.to_string())?;
        let Cli::Restore(group) = parsed.command else {
            panic!("not a restore command")
        };
        group.check()?;
        Ok(serde_json::to_value(group.command().make_request()).unwrap())
    }

    #[test]
    fn capture_and_clear_send_no_args_and_take_no_window() {
        for (name, cmd) in [("capture", "restore.capture"), ("clear", "restore.clear")] {
            assert_eq!(outcome(&[name]).unwrap(), json!({"cmd": cmd}));
            assert!(outcome(&[name, "--window", "w1"]).is_err());
            assert!(outcome(&[name, "--target", "s1"]).is_err());
            assert!(outcome(&[name, "extra"]).is_err());
        }
    }

    #[test]
    fn mode_sends_its_mode_or_none_and_takes_no_window() {
        assert_eq!(outcome(&["mode"]).unwrap(), json!({"cmd": "restore.mode"}));
        for mode in ["none", "rerun", "live"] {
            assert_eq!(
                outcome(&["mode", mode]).unwrap(),
                json!({"cmd": "restore.mode", "args": {"mode": mode}})
            );
        }
        for bogus in ["fresh", "Live", ""] {
            assert_eq!(
                outcome(&["mode", bogus]).unwrap_err(),
                "mode must be none, rerun, or live"
            );
        }
        assert!(outcome(&["mode", "live", "--window", "w1"]).is_err());
    }

    #[test]
    fn the_group_needs_a_subcommand() {
        assert!(outcome(&[]).is_err());
        assert!(outcome(&["bogus"]).is_err());
    }
}
