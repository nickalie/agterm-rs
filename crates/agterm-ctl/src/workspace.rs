//! `ngtermctl workspace ...` (`agtermctlKit/WorkspaceCommands.swift` `Workspace`). Each subcommand takes its own
//! arguments first, then `--target` where it addresses a workspace, then [`ClientOptions`], the order its help and
//! usage line list them in. `go` and `filter` take no `--target`: one steps from the current workspace, the other
//! flips the whole window's filter.

use agterm_core::model::FocusMode;
use agterm_proto::command::WorkspaceCmd;
use agterm_proto::{Args, Command, Request};

use crate::{BasicOptions, ClientOptions, RequestCommand, TargetOptions};

/// `ngtermctl workspace <subcommand>`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Workspace commands.",
    subcommand_required = true,
    arg_required_else_help = true
)]
pub struct Workspace {
    #[command(subcommand)]
    command: WorkspaceCommand,
}

impl Workspace {
    /// The subcommand this invocation runs.
    pub fn command(&self) -> &dyn RequestCommand {
        match &self.command {
            WorkspaceCommand::New(command) => command,
            WorkspaceCommand::Rename(command) => command,
            WorkspaceCommand::Delete(command) => command,
            WorkspaceCommand::Select(command) => command,
            WorkspaceCommand::Go(command) => command,
            WorkspaceCommand::Move(command) => command,
            WorkspaceCommand::Focus(command) => command,
            WorkspaceCommand::Filter(command) => command,
            WorkspaceCommand::Collapse(command) => command,
            WorkspaceCommand::Expand(command) => command,
        }
    }
}

/// The subcommands, in upstream's `Workspace.configuration.subcommands` order.
#[derive(Debug, clap::Subcommand)]
enum WorkspaceCommand {
    New(New),
    Rename(Rename),
    Delete(Delete),
    Select(Select),
    Go(Go),
    Move(Move),
    Focus(Focus),
    Filter(Filter),
    Collapse(Collapse),
    Expand(Expand),
}

/// A request for `cmd` on `target` with `args` (`--window` folded in).
fn workspace_request(
    cmd: WorkspaceCmd,
    target: Option<&TargetOptions>,
    args: Option<Args>,
    options: &ClientOptions,
) -> Request {
    Request {
        cmd: Command::Workspace(cmd),
        target: target.map(|target| target.target.clone()),
        args: options.with_window(args),
    }
}

/// `ngtermctl workspace new [<name>] [--collapsed]`: echoes the new workspace's id.
#[derive(Debug, clap::Args)]
#[command(about = "Create a workspace.")]
pub struct New {
    #[arg(help = "Workspace name (defaults to the auto-generated name).")]
    name: Option<String>,

    #[arg(long, help = "Create the workspace collapsed in the sidebar.")]
    collapsed: bool,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for New {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn echoes_result_id(&self) -> bool {
        true
    }

    fn make_request(&self) -> Request {
        let args = Args {
            name: self.name.clone(),
            collapsed: self.collapsed.then_some(true),
            ..Args::default()
        };
        // upstream always builds the bag, so even a bare `workspace new` sends `"args": {}`
        workspace_request(WorkspaceCmd::New, None, Some(args), &self.options)
    }
}

/// `ngtermctl workspace rename <name>`.
#[derive(Debug, clap::Args)]
#[command(about = "Rename a workspace.")]
pub struct Rename {
    #[arg(help = "New workspace name.")]
    name: String,

    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Rename {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn make_request(&self) -> Request {
        let args = Args {
            name: Some(self.name.clone()),
            ..Args::default()
        };
        workspace_request(WorkspaceCmd::Rename, Some(&self.target), Some(args), &self.options)
    }
}

/// A subcommand addressing one workspace with no options of its own.
macro_rules! targeted_command {
    ($(#[$doc:meta])* $name:ident, $cmd:expr, $about:literal) => {
        $(#[$doc])*
        #[derive(Debug, clap::Args)]
        #[command(about = $about)]
        pub struct $name {
            #[command(flatten)]
            target: TargetOptions,

            #[command(flatten)]
            options: ClientOptions,
        }

        impl RequestCommand for $name {
            fn options(&self) -> &BasicOptions {
                &self.options.basic
            }

            fn make_request(&self) -> Request {
                workspace_request($cmd, Some(&self.target), None, &self.options)
            }
        }
    };
}

targeted_command!(
    /// `ngtermctl workspace delete`: the last workspace is refused by the app.
    Delete,
    WorkspaceCmd::Delete,
    "Delete a workspace."
);
targeted_command!(
    /// `ngtermctl workspace select`.
    Select,
    WorkspaceCmd::Select,
    "Select a workspace."
);
targeted_command!(
    /// `ngtermctl workspace collapse`.
    Collapse,
    WorkspaceCmd::Collapse,
    "Collapse a workspace in the sidebar tree."
);
targeted_command!(
    /// `ngtermctl workspace expand`.
    Expand,
    WorkspaceCmd::Expand,
    "Expand a workspace in the sidebar tree."
);

/// `ngtermctl workspace go --to next|prev`: steps the current workspace and selects its first session. Relative to
/// what is current, so it takes no `--target`, as `session go`; `move` is the verb that reorders instead.
#[derive(Debug, clap::Args)]
#[command(name = "go", about = "Navigate workspaces: next|prev.")]
pub struct Go {
    #[arg(long, help = "Direction: next or prev.")]
    to: String,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Go {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn make_request(&self) -> Request {
        let args = Args {
            to: Some(self.to.clone()),
            ..Args::default()
        };
        workspace_request(WorkspaceCmd::Go, None, Some(args), &self.options)
    }
}

/// `ngtermctl workspace move --to up|down|top|bottom`.
#[derive(Debug, clap::Args)]
#[command(about = "Reorder a workspace among its siblings.")]
pub struct Move {
    #[arg(long, help = "Direction: up, down, top, or bottom.")]
    to: String,

    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Move {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn make_request(&self) -> Request {
        let args = Args {
            to: Some(self.to.clone()),
            ..Args::default()
        };
        workspace_request(WorkspaceCmd::Move, Some(&self.target), Some(args), &self.options)
    }
}

/// `ngtermctl workspace focus [on|off|toggle|add]`: marks or unmarks one workspace in the sidebar's focus set; `add`
/// only marks, `workspace filter on` applies the set. The accepted list, the help and the refusal all come from
/// [`FocusMode::ALL`], so a new mode reaches each.
#[derive(Debug, clap::Args)]
#[command(about = format!(
    "Mark a workspace in the sidebar focus set ({}).",
    FocusMode::valid_names_list()
))]
pub struct Focus {
    #[arg(
        default_value = "toggle",
        help = format!("Mode: {}.", FocusMode::help_phrase())
    )]
    mode: String,

    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Focus {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn validate(&self) -> Result<(), String> {
        match FocusMode::from_wire(&self.mode) {
            Some(_) => Ok(()),
            None => Err(format!("mode must be one of: {}", FocusMode::valid_names_phrase())),
        }
    }

    fn make_request(&self) -> Request {
        let args = Args {
            mode: Some(self.mode.clone()),
            ..Args::default()
        };
        workspace_request(WorkspaceCmd::Focus, Some(&self.target), Some(args), &self.options)
    }
}

/// `ngtermctl workspace filter [on|off|toggle]`: turns the window's focus filter on or off, leaving the marked set
/// alone. It flips the whole window, so it takes no `--target`. The three names are spelled out, as upstream's.
#[derive(Debug, clap::Args)]
#[command(about = "Turn the sidebar workspace focus filter on or off (on|off|toggle).")]
pub struct Filter {
    #[arg(default_value = "toggle", help = "Mode: on, off, or toggle (default).")]
    mode: String,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Filter {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn validate(&self) -> Result<(), String> {
        if ["on", "off", "toggle"].contains(&self.mode.as_str()) {
            Ok(())
        } else {
            Err("mode must be on, off, or toggle".to_owned())
        }
    }

    fn make_request(&self) -> Request {
        let args = Args {
            mode: Some(self.mode.clone()),
            ..Args::default()
        };
        workspace_request(WorkspaceCmd::Filter, None, Some(args), &self.options)
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use serde_json::{Value, json};

    use crate::Agtermctl;

    fn parsed(argv: &[&str]) -> super::Workspace {
        let command_line = ["ngtermctl", "workspace"].iter().chain(argv).copied();
        let crate::Command::Workspace(group) = Agtermctl::try_parse_from(command_line).unwrap().command else {
            panic!("not a workspace command")
        };
        group
    }

    /// `ngtermctl workspace <argv>` parsed and validated: the request it sends as JSON, or the validation refusal.
    fn outcome(argv: &[&str]) -> Result<Value, String> {
        let group = parsed(argv);
        let command = group.command();
        command.validate()?;
        Ok(serde_json::to_value(command.make_request()).unwrap())
    }

    fn request(argv: &[&str]) -> Value {
        outcome(argv).unwrap()
    }

    fn parses(argv: &[&str]) -> bool {
        let command_line = ["ngtermctl", "workspace"].iter().chain(argv).copied();
        Agtermctl::try_parse_from(command_line).is_ok()
    }

    #[test]
    fn new_sends_the_name_and_flag_and_always_an_args_bag() {
        assert_eq!(request(&["new"]), json!({"cmd": "workspace.new", "args": {}}));
        assert_eq!(
            request(&["new", "build", "--collapsed", "--window", "w1"]),
            json!({"cmd": "workspace.new", "args": {"name": "build", "collapsed": true, "window": "w1"}})
        );
    }

    #[test]
    fn targeted_commands_default_to_the_active_workspace() {
        for (verb, cmd) in [
            ("delete", "workspace.delete"),
            ("select", "workspace.select"),
            ("collapse", "workspace.collapse"),
            ("expand", "workspace.expand"),
        ] {
            assert_eq!(request(&[verb]), json!({"cmd": cmd, "target": "active"}), "{verb}");
            assert_eq!(
                request(&[verb, "--target", "9f3c", "--window", "w1"]),
                json!({"cmd": cmd, "target": "9f3c", "args": {"window": "w1"}}),
                "{verb}"
            );
        }
        assert_eq!(
            request(&["rename", "api", "--target", "w"]),
            json!({"cmd": "workspace.rename", "target": "w", "args": {"name": "api"}})
        );
        assert!(!parses(&["rename"]));
    }

    #[test]
    fn go_and_move_require_a_direction_and_only_move_takes_a_target() {
        assert_eq!(
            request(&["go", "--to", "next"]),
            json!({"cmd": "workspace.go", "args": {"to": "next"}})
        );
        assert!(!parses(&["go"]));
        assert!(!parses(&["go", "--to", "next", "--target", "x"]));
        assert_eq!(
            request(&["move", "--to", "top", "--target", "w"]),
            json!({"cmd": "workspace.move", "target": "w", "args": {"to": "top"}})
        );
        assert!(!parses(&["move"]));
    }

    #[test]
    fn focus_defaults_to_toggle_and_refuses_an_unknown_mode_locally() {
        assert_eq!(
            request(&["focus"]),
            json!({"cmd": "workspace.focus", "target": "active", "args": {"mode": "toggle"}})
        );
        assert_eq!(
            request(&["focus", "add", "--target", "w"]),
            json!({"cmd": "workspace.focus", "target": "w", "args": {"mode": "add"}})
        );
        assert_eq!(
            outcome(&["focus", "pin"]),
            Err("mode must be one of: on, off, toggle, add".to_owned())
        );
    }

    #[test]
    fn filter_takes_no_target_and_refuses_an_unknown_mode_locally() {
        assert_eq!(
            request(&["filter"]),
            json!({"cmd": "workspace.filter", "args": {"mode": "toggle"}})
        );
        assert_eq!(
            request(&["filter", "off", "--window", "w1"]),
            json!({"cmd": "workspace.filter", "args": {"mode": "off", "window": "w1"}})
        );
        assert!(!parses(&["filter", "--target", "x"]));
        assert_eq!(
            outcome(&["filter", "show"]),
            Err("mode must be on, off, or toggle".to_owned())
        );
    }

    #[test]
    fn only_new_echoes_the_result_id() {
        for (argv, echoes) in [
            (&["new"][..], true),
            (&["rename", "x"], false),
            (&["delete"], false),
            (&["select"], false),
            (&["go", "--to", "next"], false),
            (&["move", "--to", "up"], false),
            (&["focus"], false),
            (&["filter"], false),
            (&["collapse"], false),
            (&["expand"], false),
        ] {
            assert_eq!(parsed(argv).command().echoes_result_id(), echoes, "{argv:?}");
        }
    }
}
