//! `ngtermctl surface ...` (`agtermctlKit/MiscCommands.swift` `Surface`): commands addressing one terminal surface
//! by the id `tree` lists for it.

use ngterm_proto::command::SurfaceCmd;
use ngterm_proto::{Args, Command, Request};

use crate::{BasicOptions, ClientOptions, RequestCommand};

/// `ngtermctl surface <subcommand>`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Terminal surface commands.",
    subcommand_required = true,
    arg_required_else_help = true
)]
pub struct Surface {
    #[command(subcommand)]
    command: SurfaceCommand,
}

impl Surface {
    /// The subcommand this invocation runs.
    pub fn command(&self) -> &dyn RequestCommand {
        match &self.command {
            SurfaceCommand::Zoom(command) => command,
            SurfaceCommand::Cursor(command) => command,
        }
    }
}

/// The subcommands, in upstream's `Surface.configuration.subcommands` order.
#[derive(Debug, clap::Subcommand)]
enum SurfaceCommand {
    Zoom(Zoom),
    Cursor(Cursor),
}

/// The `--target` of a surface command (upstream's `SurfaceTargetOptions`).
#[derive(Debug, clap::Args)]
struct SurfaceTargetOptions {
    #[arg(
        long,
        default_value = "active",
        help = "Target surface id from tree, 'quick' (the quick terminal), or 'active'."
    )]
    target: String,
}

/// A request for `cmd` on `target` with `args` (`--window` folded in); both commands always send their bag.
fn surface_request(cmd: SurfaceCmd, target: &SurfaceTargetOptions, args: Args, options: &ClientOptions) -> Request {
    Request {
        cmd: Command::Surface(cmd),
        target: Some(target.target.clone()),
        args: options.with_window(Some(args)),
    }
}

/// `ngtermctl surface zoom [show|hide|toggle]`: the mode is the server's to check.
#[derive(Debug, clap::Args)]
#[command(about = "Zoom a terminal surface (show|hide|toggle).")]
pub struct Zoom {
    #[arg(default_value = "toggle", help = "Mode: show, hide, or toggle (default).")]
    mode: String,

    #[command(flatten)]
    target: SurfaceTargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Zoom {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn make_request(&self) -> Request {
        let args = Args {
            mode: Some(self.mode.clone()),
            ..Args::default()
        };
        surface_request(SurfaceCmd::Zoom, &self.target, args, &self.options)
    }
}

/// `ngtermctl surface cursor`: prints the column alone.
#[derive(Debug, clap::Args)]
#[command(
    about = "Report a terminal surface's zero-based cursor column.",
    long_about = "Prints the column alone, so it drops straight into a command substitution. Row is not reported: \
                  the pinned libghostty exposes no cursor accessor and the vertical metrics it does export cannot \
                  recover a row that survives a custom `adjust-font-baseline`.\n\nA column is a signal, not proof \
                  about the line's content. Past the prompt it establishes the line is not empty; AT the prompt it \
                  establishes nothing, since the caret may have been moved back over text that is still there."
)]
pub struct Cursor {
    #[arg(
        long = "pane-id",
        help = "Stable pane token ($NGTERM_PANE_ID). With it --target names the session ('active' or a session id), \
                not a surface."
    )]
    pane_id: Option<String>,

    #[command(flatten)]
    target: SurfaceTargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Cursor {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn make_request(&self) -> Request {
        let args = Args {
            pane_id: self.pane_id.clone(),
            ..Args::default()
        };
        surface_request(SurfaceCmd::Cursor, &self.target, args, &self.options)
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use serde_json::{Value, json};

    use crate::Ngtermctl;

    const SURFACE: &str = "surface:5E5B1C5B-75C5-49E6-8806-2C61D8D6BBA9:right";

    fn request(argv: &[&str]) -> Value {
        let command_line = ["ngtermctl", "surface"].iter().chain(argv).copied();
        let crate::Command::Surface(group) = Ngtermctl::try_parse_from(command_line).unwrap().command else {
            panic!("not a surface command")
        };
        let command = group.command();
        command.validate().unwrap();
        serde_json::to_value(command.make_request()).unwrap()
    }

    #[test]
    fn zoom_defaults_to_toggling_the_active_surface() {
        assert_eq!(
            request(&["zoom"]),
            json!({"cmd": "surface.zoom", "target": "active", "args": {"mode": "toggle"}})
        );
        assert_eq!(
            request(&["zoom", "show", "--target", SURFACE]),
            json!({"cmd": "surface.zoom", "target": SURFACE, "args": {"mode": "show"}})
        );
        assert_eq!(
            request(&["zoom", "hide", "--window", "win"]),
            json!({"cmd": "surface.zoom", "target": "active", "args": {"mode": "hide", "window": "win"}})
        );
    }

    #[test]
    fn cursor_always_sends_an_args_bag() {
        assert_eq!(
            request(&["cursor"]),
            json!({"cmd": "surface.cursor", "target": "active", "args": {}})
        );
        assert_eq!(
            request(&["cursor", "--target", SURFACE, "--window", "win"]),
            json!({"cmd": "surface.cursor", "target": SURFACE, "args": {"window": "win"}})
        );
        assert_eq!(
            request(&["cursor", "--pane-id", "tok", "--target", "s1"]),
            json!({"cmd": "surface.cursor", "target": "s1", "args": {"paneID": "tok"}})
        );
    }
}
