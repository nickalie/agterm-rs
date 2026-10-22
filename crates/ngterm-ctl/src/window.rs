//! `ngtermctl window ...` (`agtermctlKit/WindowCommands.swift` `Window`): create, list, select, step through, close,
//! rename, delete, resize, move, zoom, full-screen and minimize windows. The window commands take [`BasicOptions`]
//! only, since a window is addressed by its positional id (default `active`) sent as the request's `target`, never
//! by `--window`. `window list` renders as `formatWindows` in `SocketClient.swift`; `window resize` prints the
//! applied `W H` through [`crate::output`].

use ngterm_proto::command::WindowCmd;
use ngterm_proto::{Args, Command, ControlWindowNode, Request};

use crate::{BasicOptions, RequestCommand};

/// `ngtermctl window <subcommand>`.
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
            WindowCommand::New(command) => command,
            WindowCommand::List(command) => command,
            WindowCommand::Select(command) => command,
            WindowCommand::Go(command) => command,
            WindowCommand::Close(command) => command,
            WindowCommand::Rename(command) => command,
            WindowCommand::Delete(command) => command,
            WindowCommand::Resize(command) => command,
            WindowCommand::Move(command) => command,
            WindowCommand::Zoom(command) => command,
            WindowCommand::Fullscreen(command) => command,
            WindowCommand::Minimize(command) => command,
        }
    }
}

/// The subcommands, in upstream's `Window.configuration.subcommands` order.
#[derive(Debug, clap::Subcommand)]
enum WindowCommand {
    New(New),
    List(List),
    Select(Select),
    Go(Go),
    Close(Close),
    Rename(Rename),
    Delete(Delete),
    Resize(Resize),
    Move(Move),
    Zoom(Zoom),
    Fullscreen(Fullscreen),
    Minimize(Minimize),
}

/// The help of every positional window address.
const ID_HELP: &str = "Window id, unique prefix, or 'active'.";

/// A request for `cmd` on `target` with `args`.
fn window_request(cmd: WindowCmd, target: Option<&str>, args: Option<Args>) -> Request {
    Request {
        cmd: Command::Window(cmd),
        target: target.map(str::to_owned),
        args,
    }
}

/// `ngtermctl window new [<name>] [--minimized]`: echoes the new window's id.
#[derive(Debug, clap::Args)]
#[command(about = "Create and open a window.")]
pub struct New {
    #[arg(help = "Window name (defaults to the auto-generated name).")]
    name: Option<String>,

    #[arg(
        long,
        help = "Park it in the Dock once created, leaving frontmost on a visible window."
    )]
    minimized: bool,

    #[command(flatten)]
    options: BasicOptions,
}

impl RequestCommand for New {
    fn options(&self) -> &BasicOptions {
        &self.options
    }

    fn echoes_result_id(&self) -> bool {
        true
    }

    fn make_request(&self) -> Request {
        // an unflagged create omits `minimized` rather than sending false, and upstream always builds the bag, so a
        // bare `window new` sends `"args": {}`
        let args = Args {
            name: self.name.clone(),
            minimized: self.minimized.then_some(true),
            ..Args::default()
        };
        window_request(WindowCmd::New, None, Some(args))
    }
}

/// `ngtermctl window list [--socket <socket>] [--json]`.
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

/// A subcommand addressing one window by its optional positional id, with no options of its own.
macro_rules! targeted_command {
    ($(#[$doc:meta])* $name:ident, $cmd:expr, $about:literal) => {
        $(#[$doc])*
        #[derive(Debug, clap::Args)]
        #[command(about = $about)]
        pub struct $name {
            #[arg(default_value = "active", help = ID_HELP)]
            id: String,

            #[command(flatten)]
            options: BasicOptions,
        }

        impl RequestCommand for $name {
            fn options(&self) -> &BasicOptions {
                &self.options
            }

            fn make_request(&self) -> Request {
                window_request($cmd, Some(&self.id), None)
            }
        }
    };
}

targeted_command!(
    /// `ngtermctl window select [<id>]`: raises an open window, or opens a closed one.
    Select,
    WindowCmd::Select,
    "Select (raise or open) a window."
);
targeted_command!(
    /// `ngtermctl window close [<id>]`.
    Close,
    WindowCmd::Close,
    "Close a window (its bundle is kept)."
);
targeted_command!(
    /// `ngtermctl window delete [<id>]`: the last window is refused by the app.
    Delete,
    WindowCmd::Delete,
    "Delete a window (keeps at least one)."
);
targeted_command!(
    /// `ngtermctl window zoom [<id>]`.
    Zoom,
    WindowCmd::Zoom,
    "Zoom a window (maximize-to-screen toggle)."
);
targeted_command!(
    /// `ngtermctl window fullscreen [<id>]`.
    Fullscreen,
    WindowCmd::Fullscreen,
    "Toggle native macOS full screen for a window."
);

/// `ngtermctl window go --to next|prev`: raises the next or previous OPEN window, wrapping. Relative to the active
/// window, so it takes no id, as `workspace go`; `select` is the verb that opens a closed one.
#[derive(Debug, clap::Args)]
#[command(name = "go", about = "Navigate open windows: next|prev.")]
pub struct Go {
    #[arg(long, help = "Direction: next or prev.")]
    to: String,

    #[command(flatten)]
    options: BasicOptions,
}

impl RequestCommand for Go {
    fn options(&self) -> &BasicOptions {
        &self.options
    }

    fn make_request(&self) -> Request {
        let args = Args {
            to: Some(self.to.clone()),
            ..Args::default()
        };
        window_request(WindowCmd::Go, None, Some(args))
    }
}

/// `ngtermctl window rename <id> <name>`: the id is required here, since the name follows it.
#[derive(Debug, clap::Args)]
#[command(about = "Rename a window.")]
pub struct Rename {
    #[arg(help = ID_HELP)]
    id: String,

    #[arg(help = "New window name.")]
    name: String,

    #[command(flatten)]
    options: BasicOptions,
}

impl RequestCommand for Rename {
    fn options(&self) -> &BasicOptions {
        &self.options
    }

    fn make_request(&self) -> Request {
        let args = Args {
            name: Some(self.name.clone()),
            ..Args::default()
        };
        window_request(WindowCmd::Rename, Some(&self.id), Some(args))
    }
}

/// `ngtermctl window resize [<id>] --width <width> --height <height>`: prints the applied `W H`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Resize a window (frame size in points).",
    long_about = "Prints the applied width and height as W H after clamping to the window minimum and display bounds."
)]
pub struct Resize {
    #[arg(default_value = "active", help = ID_HELP)]
    id: String,

    #[arg(long, help = "New width in points.")]
    width: i64,

    #[arg(long, help = "New height in points.")]
    height: i64,

    #[command(flatten)]
    options: BasicOptions,
}

impl RequestCommand for Resize {
    fn options(&self) -> &BasicOptions {
        &self.options
    }

    fn make_request(&self) -> Request {
        let args = Args {
            width: Some(self.width),
            height: Some(self.height),
            ..Args::default()
        };
        window_request(WindowCmd::Resize, Some(&self.id), Some(args))
    }
}

/// `ngtermctl window move [<id>] --x <x> --y <y> [--display <display>]`.
#[derive(Debug, clap::Args)]
#[command(about = "Move a window (top-left x,y in points, relative to a display).")]
pub struct Move {
    #[arg(default_value = "active", help = ID_HELP)]
    id: String,

    #[arg(long, help = "Left edge x in points, from the display's left.")]
    x: i64,

    #[arg(long, help = "Top edge y in points, from the display's top.")]
    y: i64,

    #[arg(long, help = "Display index (default: the window's current display).")]
    display: Option<i64>,

    #[command(flatten)]
    options: BasicOptions,
}

impl RequestCommand for Move {
    fn options(&self) -> &BasicOptions {
        &self.options
    }

    fn make_request(&self) -> Request {
        let args = Args {
            x: Some(self.x),
            y: Some(self.y),
            display: self.display,
            ..Args::default()
        };
        window_request(WindowCmd::Move, Some(&self.id), Some(args))
    }
}

/// `ngtermctl window minimize [<id>] [<mode>]`: the mode goes unchecked, the app refuses an unknown one.
#[derive(Debug, clap::Args)]
#[command(about = "Minimize a window to the Dock, or restore it.")]
pub struct Minimize {
    #[arg(default_value = "active", help = ID_HELP)]
    id: String,

    #[arg(help = "on, off, or toggle (default: toggle).")]
    mode: Option<String>,

    #[command(flatten)]
    options: BasicOptions,
}

impl RequestCommand for Minimize {
    fn options(&self) -> &BasicOptions {
        &self.options
    }

    fn make_request(&self) -> Request {
        // both positionals are optional, so a bare `window minimize on` binds the mode word to the id. A window
        // address is a UUID prefix (hex only) or `active`, never a mode word, so recovering the intent is unambiguous
        let (target, mode) = match &self.mode {
            None if ["on", "off", "toggle"].contains(&self.id.as_str()) => ("active", self.id.clone()),
            mode => (self.id.as_str(), mode.clone().unwrap_or_else(|| "toggle".to_owned())),
        };
        let args = Args {
            mode: Some(mode),
            ..Args::default()
        };
        window_request(WindowCmd::Minimize, Some(target), Some(args))
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
    use serde_json::{Value, json};

    use super::*;

    fn node(id: &str, name: &str, open: bool, active: bool) -> ControlWindowNode {
        serde_json::from_value(json!({"id": id, "name": name, "open": open, "active": active})).unwrap()
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
        let command_line = ["ngtermctl", "window"].iter().chain(argv).copied();
        match crate::Ngtermctl::try_parse_from(command_line)?.command {
            crate::Command::Window(group) => Ok(group),
            other => panic!("not a window command: {other:?}"),
        }
    }

    /// The request `ngtermctl window <argv>` sends, as JSON.
    fn request(argv: &[&str]) -> Value {
        serde_json::to_value(parsed(argv).unwrap().command().make_request()).unwrap()
    }

    #[test]
    fn list_sends_a_bare_request_and_keeps_socket_and_json() {
        let group = parsed(&["list", "--socket", "/tmp/x.sock", "--json"]).unwrap();
        let command = group.command();
        assert_eq!(
            serde_json::to_value(command.make_request()).unwrap(),
            json!({"cmd": "window.list"})
        );
        assert_eq!(
            command.options().socket.as_deref(),
            Some(std::path::Path::new("/tmp/x.sock"))
        );
        assert!(command.options().json);
        assert!(!command.echoes_result_id());
    }

    #[test]
    fn window_commands_take_no_window_selector() {
        assert!(parsed(&["list", "--window", "w1"]).is_err());
        assert!(parsed(&["select", "9f3c", "--window", "w1"]).is_err());
    }

    #[test]
    fn new_sends_the_name_and_flag_and_always_an_args_bag() {
        assert_eq!(request(&["new"]), json!({"cmd": "window.new", "args": {}}));
        assert_eq!(
            request(&["new", "Work"]),
            json!({"cmd": "window.new", "args": {"name": "Work"}})
        );
        assert_eq!(
            request(&["new", "Work", "--minimized"]),
            json!({"cmd": "window.new", "args": {"name": "Work", "minimized": true}})
        );
    }

    #[test]
    fn targeted_commands_default_to_the_active_window() {
        for (verb, cmd) in [
            ("select", "window.select"),
            ("close", "window.close"),
            ("delete", "window.delete"),
            ("zoom", "window.zoom"),
            ("fullscreen", "window.fullscreen"),
        ] {
            assert_eq!(request(&[verb]), json!({"cmd": cmd, "target": "active"}), "{verb}");
            assert_eq!(
                request(&[verb, "9f3c"]),
                json!({"cmd": cmd, "target": "9f3c"}),
                "{verb}"
            );
        }
    }

    #[test]
    fn rename_requires_both_the_id_and_the_name() {
        assert_eq!(
            request(&["rename", "9f3c", "Renamed"]),
            json!({"cmd": "window.rename", "target": "9f3c", "args": {"name": "Renamed"}})
        );
        assert!(parsed(&["rename", "9f3c"]).is_err());
    }

    #[test]
    fn go_requires_a_direction_and_takes_no_id() {
        for to in ["next", "prev"] {
            assert_eq!(
                request(&["go", "--to", to]),
                json!({"cmd": "window.go", "args": {"to": to}})
            );
        }
        assert!(parsed(&["go"]).is_err());
        assert!(parsed(&["go", "active"]).is_err());
    }

    #[test]
    fn resize_sends_both_dimensions_and_requires_each() {
        assert_eq!(
            request(&["resize", "9f3c", "--width", "1200", "--height", "800"]),
            json!({"cmd": "window.resize", "target": "9f3c", "args": {"width": 1200, "height": 800}})
        );
        assert_eq!(
            request(&["resize", "--width", "1000", "--height", "700"]),
            json!({"cmd": "window.resize", "target": "active", "args": {"width": 1000, "height": 700}})
        );
        let missing = parsed(&["resize", "--width", "1000"]).unwrap_err();
        assert_eq!(missing.kind(), clap::error::ErrorKind::MissingRequiredArgument);
        assert!(parsed(&["resize", "--width", "wide", "--height", "700"]).is_err());
    }

    #[test]
    fn move_sends_the_origin_and_an_optional_display() {
        assert_eq!(
            request(&["move", "9f3c", "--x", "100", "--y", "50", "--display", "1"]),
            json!({"cmd": "window.move", "target": "9f3c", "args": {"x": 100, "y": 50, "display": 1}})
        );
        assert_eq!(
            request(&["move", "--x", "100", "--y", "50"]),
            json!({"cmd": "window.move", "target": "active", "args": {"x": 100, "y": 50}})
        );
        assert!(parsed(&["move", "--x", "100"]).is_err());
    }

    #[test]
    fn minimize_defaults_to_the_active_window_and_toggle() {
        assert_eq!(
            request(&["minimize"]),
            json!({"cmd": "window.minimize", "target": "active", "args": {"mode": "toggle"}})
        );
        for mode in ["on", "off"] {
            assert_eq!(
                request(&["minimize", "9f3c", mode]),
                json!({"cmd": "window.minimize", "target": "9f3c", "args": {"mode": mode}})
            );
        }
    }

    #[test]
    fn a_bare_minimize_mode_targets_the_active_window() {
        for mode in ["on", "off", "toggle"] {
            assert_eq!(
                request(&["minimize", mode]),
                json!({"cmd": "window.minimize", "target": "active", "args": {"mode": mode}})
            );
        }
        // hex `0ff` is an id, not the word `off`
        assert_eq!(
            request(&["minimize", "0ff"]),
            json!({"cmd": "window.minimize", "target": "0ff", "args": {"mode": "toggle"}})
        );
    }

    #[test]
    fn only_new_echoes_the_result_id() {
        for (argv, echoes) in [
            (&["new"][..], true),
            (&["list"], false),
            (&["select"], false),
            (&["go", "--to", "next"], false),
            (&["close"], false),
            (&["rename", "a", "b"], false),
            (&["delete"], false),
            (&["resize", "--width", "1", "--height", "1"], false),
            (&["move", "--x", "1", "--y", "1"], false),
            (&["zoom"], false),
            (&["fullscreen"], false),
            (&["minimize"], false),
        ] {
            assert_eq!(parsed(argv).unwrap().command().echoes_result_id(), echoes, "{argv:?}");
        }
    }
}
