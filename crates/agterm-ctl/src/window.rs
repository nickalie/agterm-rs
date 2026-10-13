//! `agtermctl window ...` (`agtermctlKit/WindowCommands.swift` `Window`): the window commands take
//! [`BasicOptions`] only, since a window is addressed by its positional id, never `--window`. `window list` renders
//! as `formatWindows` in `SocketClient.swift`. The rest of upstream's group joins with Task 14.

use agterm_proto::command::WindowCmd;
use agterm_proto::{Command, ControlWindowNode, Request};

use crate::{BasicOptions, RequestCommand};

/// `agtermctl window <subcommand>`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Window commands.",
    subcommand_required = true,
    arg_required_else_help = true
)]
pub struct Window {
    #[command(subcommand)]
    command: WindowCommand,
}

impl Window {
    /// The subcommand this invocation runs.
    pub fn command(&self) -> &dyn RequestCommand {
        match &self.command {
            WindowCommand::List(command) => command,
        }
    }
}

/// The subcommands, in upstream's `Window.configuration.subcommands` order.
#[derive(Debug, clap::Subcommand)]
enum WindowCommand {
    List(List),
}

/// `agtermctl window list [--socket <socket>] [--json]`.
#[derive(Debug, clap::Args)]
#[command(about = "List windows (id, name, open, active).")]
pub struct List {
    #[command(flatten)]
    options: BasicOptions,
}

impl RequestCommand for List {
    fn options(&self) -> &BasicOptions {
        &self.options
    }

    fn make_request(&self) -> Request {
        Request::new(Command::Window(WindowCmd::List))
    }
}

/// One line per window (no trailing newline): `<id>  <name>`, tagged `[open]` and `[active]`. A closed or inactive
/// window still lists, without the tag.
#[must_use]
pub fn format_windows(windows: &[ControlWindowNode]) -> String {
    windows
        .iter()
        .map(|window| {
            let open = if window.open { " [open]" } else { "" };
            let active = if window.active { " [active]" } else { "" };
            format!("{}  {}{open}{active}", window.id, window.name)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, name: &str, open: bool, active: bool) -> ControlWindowNode {
        serde_json::from_value(serde_json::json!({"id": id, "name": name, "open": open, "active": active})).unwrap()
    }

    #[test]
    fn windows_list_with_their_open_and_active_tags() {
        let windows = [
            node("w1", "work", true, true),
            node("w2", "personal", true, false),
            node("w3", "archive", false, false),
            // a closed-but-active window (a frontmost id not yet loaded) renders [active] without [open]
            node("w4", "pending", false, true),
        ];
        assert_eq!(
            format_windows(&windows),
            "w1  work [open] [active]\nw2  personal [open]\nw3  archive\nw4  pending [active]"
        );
    }

    #[test]
    fn no_windows_render_empty() {
        assert_eq!(format_windows(&[]), "");
    }

    fn parsed(argv: &[&str]) -> Result<Window, clap::Error> {
        use clap::Parser;
        let command_line = ["agtermctl", "window"].iter().chain(argv).copied();
        match crate::Agtermctl::try_parse_from(command_line)?.command {
            crate::Command::Window(group) => Ok(group),
            other => panic!("not a window command: {other:?}"),
        }
    }

    #[test]
    fn list_sends_a_bare_request_and_keeps_socket_and_json() {
        let group = parsed(&["list", "--socket", "/tmp/x.sock", "--json"]).unwrap();
        let command = group.command();
        assert_eq!(
            serde_json::to_value(command.make_request()).unwrap(),
            serde_json::json!({"cmd": "window.list"})
        );
        assert_eq!(
            command.options().socket.as_deref(),
            Some(std::path::Path::new("/tmp/x.sock"))
        );
        assert!(command.options().json);
        assert!(!command.echoes_result_id());
    }

    #[test]
    fn list_takes_no_window_selector() {
        assert!(parsed(&["list", "--window", "w1"]).is_err());
    }
}
