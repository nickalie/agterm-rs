//! `agtermctl sidebar` (`agtermctlKit/MiscCommands.swift` `Sidebar`): the sidebar's visibility, view mode and flagged
//! layout, expanding and collapsing its tree, and its width. `visibility` is the group's default subcommand
//! ([`crate::usage::name_default_subcommands`]), so a bare `sidebar [show|hide|toggle]` keeps working. Visibility,
//! mode and flagged layout take no `--window`: they act on the frontmost window, or app-wide.

use std::process::ExitCode;

use agterm_core::paths::Env;
use agterm_proto::command::SidebarCmd;
use agterm_proto::{Args, Command, Request, Response};
use serde_json::Value;

use crate::{BasicOptions, ClientOptions, Failure, RequestCommand, client, output};

/// `agtermctl sidebar <subcommand>`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Sidebar visibility and view mode.",
    subcommand_required = true,
    arg_required_else_help = true
)]
pub struct Sidebar {
    #[command(subcommand)]
    command: SidebarCommand,
}

/// The subcommands, in upstream's `Sidebar.configuration.subcommands` order.
#[derive(Debug, clap::Subcommand)]
enum SidebarCommand {
    Visibility(Visibility),
    Mode(Mode),
    FlaggedLayout(FlaggedLayout),
    Expand(Expand),
    Collapse(Collapse),
    Width(Width),
}

impl Sidebar {
    /// The subcommand this invocation runs.
    fn command(&self) -> &dyn RequestCommand {
        match &self.command {
            SidebarCommand::Visibility(command) => command,
            SidebarCommand::Mode(command) => command,
            SidebarCommand::FlaggedLayout(command) => command,
            SidebarCommand::Expand(command) => command,
            SidebarCommand::Collapse(command) => command,
            SidebarCommand::Width(command) => command,
        }
    }

    /// What the parser cannot check, as a usage error's message.
    pub fn check(&self) -> Result<(), String> {
        self.command().validate()
    }

    /// Runs the command.
    pub fn execute(&self, env: &dyn Env) -> Result<(), Failure> {
        self.command().invoke(env)
    }
}

/// A `sidebar*` request carrying `args`.
fn sidebar_request(cmd: SidebarCmd, args: Option<Args>) -> Request {
    Request {
        cmd: Command::Sidebar(cmd),
        target: None,
        args,
    }
}

/// A `sidebar*` request whose args carry only `mode`.
fn mode_request(cmd: SidebarCmd, mode: &str) -> Request {
    let args = Args {
        mode: Some(mode.to_owned()),
        ..Args::default()
    };
    sidebar_request(cmd, Some(args))
}

/// `agtermctl sidebar [visibility] [show|hide|toggle]`: the mode is the server's to check. The sidebar is always the
/// frontmost window's, so this carries no `--window`.
#[derive(Debug, clap::Args)]
#[command(about = "Sidebar visibility (show|hide|toggle).")]
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
        mode_request(SidebarCmd::Visibility, &self.mode)
    }
}

/// `agtermctl sidebar mode [tree|flagged|toggle]`: flips the frontmost window's sidebar between the workspace tree and
/// the flagged working-set view.
#[derive(Debug, clap::Args)]
#[command(about = "Sidebar view mode (tree|flagged|toggle).")]
pub struct Mode {
    #[arg(default_value = "toggle", help = "Mode: tree, flagged, or toggle (default).")]
    mode: String,

    #[command(flatten)]
    options: BasicOptions,
}

impl RequestCommand for Mode {
    fn options(&self) -> &BasicOptions {
        &self.options
    }

    fn validate(&self) -> Result<(), String> {
        if ["tree", "flagged", "toggle"].contains(&self.mode.as_str()) {
            Ok(())
        } else {
            Err("mode must be tree, flagged, or toggle".to_owned())
        }
    }

    fn make_request(&self) -> Request {
        mode_request(SidebarCmd::Mode, &self.mode)
    }
}

/// `agtermctl sidebar flagged-layout [flat|plain|tree|toggle]`: how every window's flagged view arranges its sessions;
/// app-wide, so no `--window`. The reply echoes the applied layout as `text`.
#[derive(Debug, clap::Args)]
#[command(about = "Flagged view layout (flat|plain|tree|toggle).")]
pub struct FlaggedLayout {
    #[arg(
        default_value = "toggle",
        help = "Layout: flat, plain (flat without workspace names), tree, or toggle (default)."
    )]
    layout: String,

    #[command(flatten)]
    options: BasicOptions,
}

impl RequestCommand for FlaggedLayout {
    fn options(&self) -> &BasicOptions {
        &self.options
    }

    fn validate(&self) -> Result<(), String> {
        if ["flat", "plain", "tree", "toggle"].contains(&self.layout.as_str()) {
            Ok(())
        } else {
            Err("layout must be flat, plain, tree, or toggle".to_owned())
        }
    }

    fn make_request(&self) -> Request {
        mode_request(SidebarCmd::FlaggedLayout, &self.layout)
    }
}

/// `agtermctl sidebar expand [--window W]`: expands every workspace in a window's sidebar tree (default frontmost).
#[derive(Debug, clap::Args)]
#[command(about = "Expand every workspace in the sidebar.")]
pub struct Expand {
    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Expand {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn make_request(&self) -> Request {
        sidebar_request(SidebarCmd::Expand, self.options.with_window(None))
    }
}

/// `agtermctl sidebar collapse [--window W]`: collapses every workspace but the active one in a window's sidebar.
#[derive(Debug, clap::Args)]
#[command(about = "Collapse all workspaces except the active one.")]
pub struct Collapse {
    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Collapse {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn make_request(&self) -> Request {
        sidebar_request(SidebarCmd::Collapse, self.options.with_window(None))
    }
}

/// `agtermctl sidebar width <points> [--window W]`: moves the divider a drag would move and prints the APPLIED width,
/// so a value outside the bounds reads back as the clamped one. The range is the server's to check.
#[derive(Debug, clap::Args)]
#[command(about = "Set the sidebar width in points.")]
pub struct Width {
    #[arg(help = "Sidebar width in points, clamped to the drag range.")]
    points: f64,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Width {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    // nan and inf parse as numbers but are no width
    fn validate(&self) -> Result<(), String> {
        if self.points.is_finite() {
            Ok(())
        } else {
            Err("points must be a number".to_owned())
        }
    }

    fn make_request(&self) -> Request {
        let args = Args {
            sidebar_width: Some(self.points),
            ..Args::default()
        };
        sidebar_request(SidebarCmd::Width, self.options.with_window(Some(args)))
    }

    /// [`RequestCommand::default_run`], with the echoed `sidebarWidth` rendered as upstream's `formatResponse` does.
    fn invoke(&self, env: &dyn Env) -> Result<(), Failure> {
        let options = self.options();
        let reply = client::send(&options.socket_path(env), &self.make_request())?;
        if options.json || !reply.response.ok {
            output::print_response(&reply, options.json, false);
        } else {
            println!("{}", format_width_reply(&reply.response));
        }
        if reply.response.ok {
            Ok(())
        } else {
            Err(Failure::Exit(ExitCode::FAILURE))
        }
    }
}

/// The human summary of a successful `sidebar.width` reply. Upstream's `formatResponse` reads `sidebarWidth` last,
/// after every other field (the echoed id is never asked for here), so it applies only where the shared rendering
/// falls through to `ok`.
fn format_width_reply(response: &Response) -> String {
    let rendered = output::format_response(response, false);
    let width = response
        .result
        .as_ref()
        .and_then(|result| result.get("sidebarWidth"))
        .and_then(Value::as_f64);
    match width {
        Some(width) if rendered == "ok" => format_sidebar_width(width),
        _ => rendered,
    }
}

/// The stored (clamped) width as the Double's own description, without fixed-decimal rounding: `300.0`, `271.34`. A
/// caller compares it against its request numerically to detect a clamp. Rust's `Debug` spelling matches Swift's
/// `description` across the drag range; they differ only in the exponent form of extreme magnitudes.
fn format_sidebar_width(width: f64) -> String {
    format!("{width:?}")
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use clap::{CommandFactory, Parser};
    use serde_json::json;

    use super::*;
    use crate::Agtermctl;
    use crate::usage::name_default_subcommands;

    /// `agtermctl sidebar <argv>` parsed (the default subcommand named first) and validated: the request a run sends
    /// as JSON, or the refusal.
    fn outcome(argv: &[&str]) -> Result<Value, String> {
        let command_line: Vec<OsString> = ["agtermctl", "sidebar"]
            .iter()
            .chain(argv)
            .map(OsString::from)
            .collect();
        let named = name_default_subcommands(&Agtermctl::command(), &command_line);
        let parsed = Agtermctl::try_parse_from(named).map_err(|error| error.to_string())?;
        let crate::Command::Sidebar(group) = parsed.command else {
            panic!("not a sidebar command")
        };
        group.check()?;
        Ok(serde_json::to_value(group.command().make_request()).unwrap())
    }

    fn request(argv: &[&str]) -> Value {
        outcome(argv).unwrap()
    }

    #[test]
    fn visibility_is_the_default_and_defaults_to_toggle() {
        assert_eq!(request(&[]), json!({"cmd": "sidebar", "args": {"mode": "toggle"}}));
        assert_eq!(request(&["hide"]), json!({"cmd": "sidebar", "args": {"mode": "hide"}}));
        assert_eq!(
            request(&["visibility", "show"]),
            json!({"cmd": "sidebar", "args": {"mode": "show"}})
        );
        // the server checks the visibility mode
        assert_eq!(
            request(&["maybe"]),
            json!({"cmd": "sidebar", "args": {"mode": "maybe"}})
        );
    }

    #[test]
    fn mode_defaults_to_toggle_and_refuses_an_unknown_mode() {
        assert_eq!(
            request(&["mode"]),
            json!({"cmd": "sidebar.mode", "args": {"mode": "toggle"}})
        );
        assert_eq!(
            request(&["mode", "flagged"]),
            json!({"cmd": "sidebar.mode", "args": {"mode": "flagged"}})
        );
        for bad in ["sideways", "grid"] {
            assert_eq!(
                outcome(&["mode", bad]),
                Err("mode must be tree, flagged, or toggle".to_owned())
            );
        }
    }

    #[test]
    fn flagged_layout_sends_its_layout_as_the_mode() {
        assert_eq!(
            request(&["flagged-layout"]),
            json!({"cmd": "sidebar.flagged-layout", "args": {"mode": "toggle"}})
        );
        for layout in ["flat", "plain", "tree"] {
            assert_eq!(
                request(&["flagged-layout", layout]),
                json!({"cmd": "sidebar.flagged-layout", "args": {"mode": layout}})
            );
        }
        assert_eq!(
            outcome(&["flagged-layout", "grid"]),
            Err("layout must be flat, plain, tree, or toggle".to_owned())
        );
    }

    #[test]
    fn only_expand_collapse_and_width_take_a_window() {
        assert_eq!(request(&["expand"]), json!({"cmd": "sidebar.expand"}));
        assert_eq!(request(&["collapse"]), json!({"cmd": "sidebar.collapse"}));
        assert_eq!(
            request(&["expand", "--window", "abc"]),
            json!({"cmd": "sidebar.expand", "args": {"window": "abc"}})
        );
        assert_eq!(
            request(&["collapse", "--window", "abc"]),
            json!({"cmd": "sidebar.collapse", "args": {"window": "abc"}})
        );
        for argv in [
            &["--window", "w"][..],
            &["show", "--window", "w"],
            &["mode", "tree", "--window", "w"],
            &["flagged-layout", "tree", "--window", "w"],
        ] {
            assert!(outcome(argv).is_err(), "{argv:?}");
        }
    }

    #[test]
    fn width_sends_its_points_and_refuses_a_non_number() {
        assert_eq!(
            request(&["width", "271.3"]),
            json!({"cmd": "sidebar.width", "args": {"sidebarWidth": 271.3}})
        );
        assert_eq!(
            request(&["width", "300", "--window", "abc"]),
            json!({"cmd": "sidebar.width", "args": {"window": "abc", "sidebarWidth": 300.0}})
        );
        assert!(outcome(&["width", "wide"]).is_err());
        assert!(outcome(&["width"]).is_err());
        for not_finite in ["nan", "inf"] {
            assert_eq!(
                outcome(&["width", not_finite]),
                Err("points must be a number".to_owned())
            );
        }
    }

    #[test]
    fn the_applied_width_prints_unrounded() {
        for (stored, rendered) in [(271.3, "271.3"), (271.34, "271.34"), (300.0, "300.0")] {
            let response = Response::with_result(json!({"sidebarWidth": stored}));
            assert_eq!(format_width_reply(&response), rendered);
        }
        // an integral number on the wire is still a Double
        assert_eq!(
            format_width_reply(&Response::with_result(json!({"sidebarWidth": 300}))),
            "300.0"
        );
        // the fields upstream reads first still win, and a reply without a width is `ok`
        let text = Response::with_result(json!({"text": "t", "sidebarWidth": 1.0}));
        assert_eq!(format_width_reply(&text), "t");
        assert_eq!(format_width_reply(&Response::success()), "ok");
    }
}
