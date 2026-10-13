//! `agtermctl session ...` (`agtermctlKit/SessionCommands.swift` `Session`): the group, and here its lifecycle
//! subcommands; [`crate::session_io`] drives the terminal, [`crate::session_panes`] arranges its panes,
//! [`crate::session_meta`] sets metadata, [`crate::session_restore`] pins or replaces what a pane runs and
//! [`crate::session_background`] sets backgrounds, [`crate::session_overlay`] runs overlay programs and pages and
//! [`crate::session_hud`] posts the message panel. Each takes its own options first, then `--target` where it
//! addresses a session, then [`ClientOptions`], the order its help and usage line list them in.

use agterm_proto::command::SessionCmd;
use agterm_proto::{Args, Command, Request};

use crate::{
    BasicOptions, BatchTargetOptions, ClientOptions, RequestCommand, TargetOptions, session_background, session_hud,
    session_io, session_meta, session_overlay, session_panes, session_restore,
};

/// `agtermctl session <subcommand>`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Session commands.",
    subcommand_required = true,
    arg_required_else_help = true
)]
pub struct Session {
    #[command(subcommand)]
    command: SessionCommand,
}

impl Session {
    /// The subcommand this invocation runs.
    pub fn command(&self) -> &dyn RequestCommand {
        match &self.command {
            SessionCommand::New(command) => command,
            SessionCommand::Duplicate(command) => command,
            SessionCommand::Close(command) => command,
            SessionCommand::Select(command) => command,
            SessionCommand::Go(command) => command,
            SessionCommand::Rename(command) => command,
            SessionCommand::Reveal(command) => command,
            SessionCommand::Move(command) => command,
            SessionCommand::Type(command) => command,
            SessionCommand::Split(group) => group.command(),
            SessionCommand::Swap(command) => command,
            SessionCommand::Lead(command) => command,
            SessionCommand::Scratch(command) => command,
            SessionCommand::Focus(command) => command,
            SessionCommand::Resize(command) => command,
            SessionCommand::Copy(command) => command,
            SessionCommand::Paste(command) => command,
            SessionCommand::SelectAll(command) => command,
            SessionCommand::Text(command) => command,
            SessionCommand::Status(command) => command,
            SessionCommand::Restore(command) => command,
            SessionCommand::Restart(command) => command,
            SessionCommand::Flag(command) => command,
            SessionCommand::Context(command) => command,
            SessionCommand::Seen(command) => command,
            SessionCommand::Search(command) => command,
            SessionCommand::Background(group) => group.command(),
            SessionCommand::Overlay(group) => group.command(),
            SessionCommand::Hud(group) => group.command(),
        }
    }
}

/// The subcommands, in upstream's `Session.configuration.subcommands` order.
#[derive(Debug, clap::Subcommand)]
enum SessionCommand {
    New(New),
    Duplicate(Duplicate),
    Close(Close),
    Select(Select),
    Go(Go),
    Rename(Rename),
    Reveal(Reveal),
    Move(Move),
    Type(session_io::Type),
    Split(session_panes::Split),
    Swap(session_panes::Swap),
    Lead(session_panes::Lead),
    Scratch(session_panes::Scratch),
    Focus(session_panes::Focus),
    Resize(session_panes::Resize),
    Copy(session_io::Copy),
    Paste(session_io::Paste),
    SelectAll(session_io::SelectAll),
    Text(session_io::Text),
    Status(session_meta::Status),
    Restore(session_restore::Restore),
    Restart(session_restore::Restart),
    Flag(session_meta::Flag),
    Context(session_meta::Context),
    Seen(session_meta::Seen),
    Search(session_io::Search),
    Background(session_background::Background),
    Overlay(session_overlay::Overlay),
    Hud(session_hud::Hud),
}

/// A request for `cmd` on `target` with `args` (`--window` folded in).
pub fn session_request(
    cmd: SessionCmd,
    target: Option<String>,
    args: Option<Args>,
    options: &ClientOptions,
) -> Request {
    Request {
        cmd: Command::Session(cmd),
        target,
        args: options.with_window(args),
    }
}

/// Upstream's `Session.validateSizePercent`: a `--size-percent` is a share of the pane, 1 to 100.
pub fn validate_size_percent(size_percent: Option<i64>) -> Result<(), String> {
    match size_percent {
        Some(percent) if !(1..=100).contains(&percent) => Err("--size-percent must be between 1 and 100".to_owned()),
        _ => Ok(()),
    }
}

/// `Some(true)` for a set flag, absent otherwise: the wire carries a flag only when it is on.
pub fn flag(set: bool) -> Option<bool> {
    set.then_some(true)
}

/// `agtermctl session new`: echoes the new session's id.
#[derive(Debug, clap::Args)]
#[command(about = "Create a session.")]
pub struct New {
    #[arg(long, help = "Working directory (defaults to $HOME).")]
    cwd: Option<String>,

    #[arg(
        long,
        help = "Target workspace by id/prefix/active (defaults to the current one). Mutually exclusive with \
                --workspace-name."
    )]
    workspace: Option<String>,

    #[arg(
        long,
        help = "Target workspace by name; errors if not found unless --create-workspace. Mutually exclusive with \
                --workspace."
    )]
    workspace_name: Option<String>,

    #[arg(
        long,
        help = "With --workspace-name, create the workspace when it does not exist (reuse it otherwise)."
    )]
    create_workspace: bool,

    #[arg(
        long,
        help = "Run this command as the session's process instead of the login shell (no echoed command line; the \
                session closes when it exits)."
    )]
    command: Option<String>,

    #[arg(
        long,
        help = "With --command, hold the session open after the command exits (press any key to close) instead of \
                closing immediately."
    )]
    wait: bool,

    #[arg(long, help = "Initial session name (defaults to the auto basename).")]
    name: Option<String>,

    #[arg(
        long,
        help = "Place the new session right AFTER this anchor session (id/prefix/active); the anchor carries its own \
                workspace, replacing --workspace."
    )]
    after: Option<String>,

    #[arg(
        long,
        help = "Place the new session right BEFORE this anchor session (id/prefix/active); mirror of --after."
    )]
    before: Option<String>,

    #[arg(
        long,
        help = "Create the session in the background without selecting or focusing it (leaves the current selection \
                untouched)."
    )]
    no_select: bool,

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

    fn validate(&self) -> Result<(), String> {
        let placed = self.after.is_some() || self.before.is_some();
        let addressed = self.workspace.is_some() || self.workspace_name.is_some();
        let message = if self.after.is_some() && self.before.is_some() {
            "use either --after or --before, not both"
        } else if placed && addressed {
            // the anchor already names the workspace, so placement cannot also address one
            "session.new takes --after/--before or a workspace, not both"
        } else if self.workspace.is_some() && self.workspace_name.is_some() {
            "use either --workspace or --workspace-name, not both"
        } else if self.create_workspace && self.workspace_name.is_none() {
            "--create-workspace requires --workspace-name"
        } else if self.wait && self.command.is_none() {
            "--wait requires --command"
        } else {
            return Ok(());
        };
        Err(message.to_owned())
    }

    fn make_request(&self) -> Request {
        let args = Args {
            name: self.name.clone(),
            cwd: self.cwd.clone(),
            workspace: self.workspace.clone(),
            workspace_name: self.workspace_name.clone(),
            create_workspace: flag(self.create_workspace),
            no_select: flag(self.no_select),
            command: self.command.clone(),
            wait: flag(self.wait),
            after: self.after.clone(),
            before: self.before.clone(),
            ..Args::default()
        };
        // upstream always builds the bag, so even a bare `session new` sends `"args": {}`
        session_request(SessionCmd::New, None, Some(args), &self.options)
    }
}

/// `agtermctl session duplicate`: no options of its own, since the target session names both the destination
/// workspace and the cwd. Echoes the copy's id.
#[derive(Debug, clap::Args)]
#[command(about = "Duplicate a session: a fresh shell in its directory, placed right after it.")]
pub struct Duplicate {
    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Duplicate {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn echoes_result_id(&self) -> bool {
        true
    }

    fn make_request(&self) -> Request {
        session_request(
            SessionCmd::Duplicate,
            Some(self.target.target.clone()),
            None,
            &self.options,
        )
    }
}

/// `agtermctl session close`: one session, or a batch with a repeated `--target`.
#[derive(Debug, clap::Args)]
#[command(about = "Close a session.")]
pub struct Close {
    #[command(flatten)]
    target: BatchTargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Close {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn make_request(&self) -> Request {
        let args = self.target.batch().map(|targets| Args {
            targets: Some(targets),
            ..Args::default()
        });
        session_request(SessionCmd::Close, Some(self.target.first()), args, &self.options)
    }
}

/// `agtermctl session select`.
#[derive(Debug, clap::Args)]
#[command(about = "Select a session.")]
pub struct Select {
    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Select {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn make_request(&self) -> Request {
        session_request(
            SessionCmd::Select,
            Some(self.target.target.clone()),
            None,
            &self.options,
        )
    }
}

/// `agtermctl session go --to <direction>`: relative to the selection, so it takes no `--target`.
#[derive(Debug, clap::Args)]
#[command(about = "Navigate sessions: next|prev|first|last|next-attention|prev-attention.")]
pub struct Go {
    #[arg(
        long,
        help = "Direction: next, prev, first, last, next-attention, or prev-attention (attention = \
                blocked/completed)."
    )]
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
        session_request(SessionCmd::Go, None, Some(args), &self.options)
    }
}

/// `agtermctl session rename <name>`.
#[derive(Debug, clap::Args)]
#[command(about = "Rename a session.")]
pub struct Rename {
    #[arg(help = "New session name.")]
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
        session_request(
            SessionCmd::Rename,
            Some(self.target.target.clone()),
            Some(args),
            &self.options,
        )
    }
}

/// `agtermctl session reveal`: opens the session's focused working directory in the file manager. The abstract is
/// upstream's, Finder included.
#[derive(Debug, clap::Args)]
#[command(about = "Reveal a session's focused working directory in Finder.")]
pub struct Reveal {
    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Reveal {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn make_request(&self) -> Request {
        session_request(
            SessionCmd::Reveal,
            Some(self.target.target.clone()),
            None,
            &self.options,
        )
    }
}

/// `agtermctl session move`: exactly one placement intent among a destination workspace (relocate), `--to`
/// (reorder) and `--after`/`--before` (anchor-relative), checked before anything is sent.
#[derive(Debug, clap::Args)]
#[command(
    about = "Move a session: to another workspace, reorder with --to, or place relative to an anchor with \
             --after/--before."
)]
pub struct Move {
    #[arg(help = "Destination workspace id/prefix (relocate). Omit with --to or --after/--before.")]
    workspace: Option<String>,

    #[arg(long, help = "Reorder within the workspace: up, down, top, or bottom.")]
    to: Option<String>,

    #[arg(
        long,
        help = "Place right AFTER this anchor session (id/prefix/active); the anchor carries its own workspace \
                (relocates + positions in one shot)."
    )]
    after: Option<String>,

    #[arg(
        long,
        help = "Place right BEFORE this anchor session (id/prefix/active); mirror of --after."
    )]
    before: Option<String>,

    #[command(flatten)]
    target: BatchTargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Move {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn validate(&self) -> Result<(), String> {
        let message = if self.after.is_some() && self.before.is_some() {
            "use either --after or --before, not both"
        } else if self.after.is_some() || self.before.is_some() {
            // the anchor carries its own workspace, so placement excludes --to and a workspace
            if self.to.is_some() {
                "session.move takes --after/--before or --to, not both"
            } else if self.workspace.is_some() {
                "session.move takes --after/--before or a workspace, not both"
            } else {
                return Ok(());
            }
        } else if self.target.targets.len() > 1 && self.to.is_some() {
            "session.move --target can be repeated only with a workspace or --after/--before"
        } else {
            match (&self.workspace, &self.to) {
                (None, None) => "provide a destination workspace, --to, or --after/--before",
                (Some(_), Some(_)) => "provide a destination workspace or --to, not both",
                _ => return Ok(()),
            }
        };
        Err(message.to_owned())
    }

    fn make_request(&self) -> Request {
        let mut args = if let Some(after) = &self.after {
            Args {
                after: Some(after.clone()),
                ..Args::default()
            }
        } else if let Some(before) = &self.before {
            Args {
                before: Some(before.clone()),
                ..Args::default()
            }
        } else if let Some(workspace) = &self.workspace {
            Args {
                workspace: Some(workspace.clone()),
                ..Args::default()
            }
        } else {
            Args {
                to: self.to.clone(),
                ..Args::default()
            }
        };
        args.targets = self.target.batch();
        session_request(SessionCmd::Move, Some(self.target.first()), Some(args), &self.options)
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use serde_json::{Value, json};

    use crate::Agtermctl;

    fn parsed(argv: &[&str]) -> super::Session {
        let command_line = ["agtermctl", "session"].iter().chain(argv).copied();
        let crate::Command::Session(group) = Agtermctl::try_parse_from(command_line).unwrap().command else {
            panic!("not a session command")
        };
        group
    }

    /// `agtermctl session <argv>` parsed and validated: the request it sends as JSON, or the validation refusal.
    fn outcome(argv: &[&str]) -> Result<Value, String> {
        let group = parsed(argv);
        let command = group.command();
        command.validate()?;
        Ok(serde_json::to_value(command.make_request()).unwrap())
    }

    fn request(argv: &[&str]) -> Value {
        outcome(argv).unwrap()
    }

    fn refusal(argv: &[&str]) -> String {
        outcome(argv).unwrap_err()
    }

    fn parses(argv: &[&str]) -> bool {
        let command_line = ["agtermctl", "session"].iter().chain(argv).copied();
        Agtermctl::try_parse_from(command_line).is_ok()
    }

    #[test]
    fn new_sends_its_options_and_always_an_args_bag() {
        assert_eq!(request(&["new"]), json!({"cmd": "session.new", "args": {}}));
        assert_eq!(
            request(&["new", "--cwd", "/tmp", "--workspace", "ws1"]),
            json!({"cmd": "session.new", "args": {"cwd": "/tmp", "workspace": "ws1"}})
        );
        assert_eq!(
            request(&["new", "--workspace-name", "servers", "--create-workspace"]),
            json!({"cmd": "session.new", "args": {"workspaceName": "servers", "createWorkspace": true}})
        );
        assert_eq!(
            request(&["new", "--no-select", "--name", "n"]),
            json!({"cmd": "session.new", "args": {"name": "n", "noSelect": true}})
        );
        assert_eq!(
            request(&["new", "--command", "make test", "--wait"]),
            json!({"cmd": "session.new", "args": {"command": "make test", "wait": true}})
        );
        assert_eq!(
            request(&["new", "--after", "active"]),
            json!({"cmd": "session.new", "args": {"after": "active"}})
        );
        assert_eq!(
            request(&["new", "--before", "s1", "--window", "w1"]),
            json!({"cmd": "session.new", "args": {"before": "s1", "window": "w1"}})
        );
    }

    #[test]
    fn new_refuses_conflicting_options_before_sending() {
        let cases: [(&[&str], &str); 7] = [
            (&["--wait"], "--wait requires --command"),
            (
                &["--workspace", "active", "--workspace-name", "servers"],
                "use either --workspace or --workspace-name, not both",
            ),
            (&["--create-workspace"], "--create-workspace requires --workspace-name"),
            (
                &["--after", "a", "--before", "b"],
                "use either --after or --before, not both",
            ),
            (
                &["--after", "a", "--workspace", "ws1"],
                "session.new takes --after/--before or a workspace, not both",
            ),
            (
                &["--before", "a", "--workspace-name", "servers"],
                "session.new takes --after/--before or a workspace, not both",
            ),
            // the first rule that applies wins, in upstream's order
            (
                &["--after", "a", "--before", "b", "--workspace", "w", "--wait"],
                "use either --after or --before, not both",
            ),
        ];
        for (argv, message) in cases {
            assert_eq!(refusal(&[&["new"][..], argv].concat()), message, "{argv:?}");
        }
    }

    #[test]
    fn duplicate_select_and_reveal_default_to_the_active_session() {
        for (verb, cmd) in [
            ("duplicate", "session.duplicate"),
            ("select", "session.select"),
            ("reveal", "session.reveal"),
        ] {
            assert_eq!(request(&[verb]), json!({"cmd": cmd, "target": "active"}), "{verb}");
            assert_eq!(
                request(&[verb, "--target", "9f3c", "--window", "w1"]),
                json!({"cmd": cmd, "target": "9f3c", "args": {"window": "w1"}}),
                "{verb}"
            );
            // a single --target takes its last value, as every option
            assert_eq!(
                request(&[verb, "--target", "a", "--target", "b"]),
                json!({"cmd": cmd, "target": "b"}),
                "{verb}"
            );
        }
    }

    #[test]
    fn close_sends_one_target_or_a_batch_in_command_line_order() {
        assert_eq!(request(&["close"]), json!({"cmd": "session.close", "target": "active"}));
        assert_eq!(
            request(&["close", "--target", "x", "--window", "w1"]),
            json!({"cmd": "session.close", "target": "x", "args": {"window": "w1"}})
        );
        assert_eq!(
            request(&["close", "--target", "b", "--target", "a"]),
            json!({"cmd": "session.close", "target": "b", "args": {"targets": ["b", "a"]}})
        );
    }

    #[test]
    fn go_carries_the_direction_and_requires_it() {
        assert_eq!(
            request(&["go", "--to", "next"]),
            json!({"cmd": "session.go", "args": {"to": "next"}})
        );
        assert_eq!(
            request(&["go", "--to", "last", "--window", "w1"]),
            json!({"cmd": "session.go", "args": {"to": "last", "window": "w1"}})
        );
        assert!(!parses(&["go"]));
        assert!(!parses(&["go", "--to", "next", "--target", "x"]));
    }

    #[test]
    fn rename_takes_the_name_positionally() {
        assert_eq!(
            request(&["rename", "build"]),
            json!({"cmd": "session.rename", "target": "active", "args": {"name": "build"}})
        );
        assert_eq!(
            request(&["rename", "build", "--target", "s1", "--window", "w1"]),
            json!({"cmd": "session.rename", "target": "s1", "args": {"name": "build", "window": "w1"}})
        );
        assert!(!parses(&["rename"]));
    }

    #[test]
    fn move_sends_one_placement_intent() {
        let cases: [(&[&str], Value); 7] = [
            (
                &["ws2", "--target", "s1"],
                json!({"target": "s1", "args": {"workspace": "ws2"}}),
            ),
            (
                &["ws2", "--target", "s1", "--target", "s2"],
                json!({"target": "s1", "args": {"targets": ["s1", "s2"], "workspace": "ws2"}}),
            ),
            (&["--to", "up"], json!({"target": "active", "args": {"to": "up"}})),
            (
                &["--after", "s2", "--target", "s1"],
                json!({"target": "s1", "args": {"after": "s2"}}),
            ),
            (
                &["--after", "s3", "--target", "s1", "--target", "s2"],
                json!({"target": "s1", "args": {"targets": ["s1", "s2"], "after": "s3"}}),
            ),
            (
                &["--before", "s2", "--target", "s1"],
                json!({"target": "s1", "args": {"before": "s2"}}),
            ),
            (
                &["ws2", "--target", "s1", "--window", "w1"],
                json!({"target": "s1", "args": {"workspace": "ws2", "window": "w1"}}),
            ),
        ];
        for (argv, mut expected) in cases {
            expected["cmd"] = json!("session.move");
            assert_eq!(request(&[&["move"][..], argv].concat()), expected, "{argv:?}");
        }
    }

    #[test]
    fn move_refuses_a_missing_or_conflicting_intent_before_sending() {
        let cases: [(&[&str], &str); 7] = [
            (&[], "provide a destination workspace, --to, or --after/--before"),
            (
                &["ws2", "--to", "up"],
                "provide a destination workspace or --to, not both",
            ),
            (
                &["--to", "up", "--target", "s1", "--target", "s2"],
                "session.move --target can be repeated only with a workspace or --after/--before",
            ),
            (
                &["--after", "a", "--before", "b"],
                "use either --after or --before, not both",
            ),
            (
                &["--after", "a", "--to", "up"],
                "session.move takes --after/--before or --to, not both",
            ),
            (
                &["ws2", "--after", "a"],
                "session.move takes --after/--before or a workspace, not both",
            ),
            (
                &["ws2", "--before", "a", "--to", "up"],
                "session.move takes --after/--before or --to, not both",
            ),
        ];
        for (argv, message) in cases {
            assert_eq!(refusal(&[&["move"][..], argv].concat()), message, "{argv:?}");
        }
    }

    #[test]
    fn only_the_create_commands_echo_the_result_id() {
        for (argv, echoes) in [
            (&["new"][..], true),
            (&["duplicate"], true),
            (&["close"], false),
            (&["select"], false),
            (&["go", "--to", "next"], false),
            (&["rename", "x"], false),
            (&["reveal"], false),
            (&["move", "--to", "up"], false),
        ] {
            assert_eq!(parsed(argv).command().echoes_result_id(), echoes, "{argv:?}");
        }
    }
}
