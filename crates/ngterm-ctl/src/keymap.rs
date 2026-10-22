//! `ngtermctl keymap reload|list|run` (`agtermctlKit/MiscCommands.swift` `Keymap`). `reload` and `list` are app-global
//! (the frontmost window's settings model), so neither takes `--window`; `run` addresses a session with `--target` and
//! takes the window selector. `list` prints through [`format_keymap`], `reload` its diagnostic count through
//! [`crate::output`]'s `count` branch.

use ngterm_core::paths::Env;
use ngterm_proto::command::KeymapCmd;
use ngterm_proto::keymap::{ControlKeymap, ControlKeymapCommand};
use ngterm_proto::{Args, Command, Request};

use crate::{BasicOptions, ClientOptions, Failure, RequestCommand, TargetOptions};

/// `ngtermctl keymap <subcommand>`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Keymap commands.",
    subcommand_required = true,
    arg_required_else_help = true
)]
pub struct Keymap {
    #[command(subcommand)]
    command: KeymapCommand,
}

/// The subcommands, in upstream's `Keymap.configuration.subcommands` order.
#[derive(Debug, clap::Subcommand)]
enum KeymapCommand {
    Reload(Reload),
    List(List),
    Run(Run),
}

impl Keymap {
    /// The subcommand this invocation runs.
    fn command(&self) -> &dyn RequestCommand {
        match &self.command {
            KeymapCommand::Reload(command) => command,
            KeymapCommand::List(command) => command,
            KeymapCommand::Run(command) => command,
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

/// A request for `cmd` without a target or args.
fn bare_request(cmd: KeymapCmd) -> Request {
    Request {
        cmd: Command::Keymap(cmd),
        target: None,
        args: None,
    }
}

/// `ngtermctl keymap run <name>`: runs a custom command against the target session's focused pane.
#[derive(Debug, clap::Args)]
#[command(
    about = "Run one of your custom commands by name against a session.",
    long_about = "NAME is a custom command's name exactly as `keymap list` prints it. The command runs as it does \
                  from the palette, with the target session's focused pane, primary or split and never its scratch \
                  or an overlay, supplying the working directory, the selection and the context tokens. A name no \
                  command carries is refused.\n\nThe reply says the command started. It runs detached, so its exit \
                  status and output are not reported here; a command set to show a failure panel still shows it."
)]
pub struct Run {
    #[arg(help = "Custom command name, from `keymap list`.")]
    name: String,

    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Run {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn make_request(&self) -> Request {
        let args = Args {
            name: Some(self.name.clone()),
            ..Args::default()
        };
        Request {
            cmd: Command::Keymap(KeymapCmd::Run),
            target: Some(self.target.target.clone()),
            args: self.options.with_window(Some(args)),
        }
    }
}

/// `ngtermctl keymap reload`: prints the diagnostic count, `ok` for none.
#[derive(Debug, clap::Args)]
#[command(about = "Re-read and apply keymap.conf (prints the diagnostic count).")]
pub struct Reload {
    #[command(flatten)]
    options: BasicOptions,
}

impl RequestCommand for Reload {
    fn options(&self) -> &BasicOptions {
        &self.options
    }

    fn make_request(&self) -> Request {
        bare_request(KeymapCmd::Reload)
    }
}

/// `ngtermctl keymap list`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Show the resolved keymap and the live menu key equivalents.",
    long_about = "Prints every built-in with the binds the keymap resolved for it — the menu shortcut first, then any \
                  monitor-bound alternatives, joined with `|` — plus the custom commands, any parse diagnostics, and \
                  the key equivalents the menu bar is actually carrying. The last section is what makes a stale or \
                  hijacked chord visible: SwiftUI rebuilds the menu only on the next app activation, so a chord can \
                  be right in the keymap and wrong in the menu."
)]
pub struct List {
    #[command(flatten)]
    options: BasicOptions,
}

impl RequestCommand for List {
    fn options(&self) -> &BasicOptions {
        &self.options
    }

    fn make_request(&self) -> Request {
        bare_request(KeymapCmd::List)
    }
}

/// Renders the `keymap.list` payload (upstream's `SocketClient.formatKeymap`, no trailing newline): the resolved
/// built-ins, then the custom commands, parse diagnostics and live menu accelerators, each section only when present.
/// An overridden built-in is marked `*` and a keyless one prints `-`; the binds column is the menu chord, then the
/// monitor-bound alternatives, joined with `|`. A diagnostic on line 0 belongs to no single line and drops the number.
#[must_use]
pub fn format_keymap(keymap: &ControlKeymap) -> String {
    let mut lines = vec![format!("keymap: {}", keymap.path), String::new(), "actions:".to_owned()];
    let width = keymap
        .actions
        .iter()
        .map(|action| action.action.chars().count())
        .max()
        .unwrap_or(0);
    for action in &keymap.actions {
        let mark = if action.overridden == Some(true) { '*' } else { ' ' };
        let binds = action
            .chord
            .iter()
            .chain(action.alternates.iter().flatten())
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("|");
        let binds = if binds.is_empty() { "-" } else { binds.as_str() };
        let repeat = if action.repeats == Some(true) { "  --repeat" } else { "" };
        lines.push(format!("  {mark} {:<width$}  {binds}{repeat}", action.action));
    }
    if !keymap.commands.is_empty() {
        lines.extend([String::new(), "commands:".to_owned()]);
        lines.extend(keymap.commands.iter().map(command_row));
    }
    if !keymap.diagnostics.is_empty() {
        lines.extend([String::new(), "diagnostics:".to_owned()]);
        lines.extend(keymap.diagnostics.iter().map(|diagnostic| {
            if diagnostic.line > 0 {
                format!("    line {}: {}", diagnostic.line, diagnostic.message)
            } else {
                format!("    {}", diagnostic.message)
            }
        }));
    }
    if let Some(menu) = &keymap.menu {
        lines.extend([String::new(), "menu:".to_owned()]);
        lines.extend(menu.iter().map(|item| {
            let disabled = if item.enabled == Some(false) {
                "  (disabled)"
            } else {
                ""
            };
            format!("    {}  {} ▸ {}{disabled}", item.chord, item.menu, item.title)
        }));
    }
    lines.join("\n")
}

/// One custom command's row: its name and shortcut, then the options its `command` line set.
fn command_row(command: &ControlKeymapCommand) -> String {
    let shortcut = command.shortcut.as_deref().unwrap_or("(palette only)");
    let repeat = if command.repeats { "  --repeat" } else { "" };
    let error_hud = if command.error_hud {
        let pane = command
            .error_pane
            .as_ref()
            .map_or_else(String::new, |pane| format!(" --error-pane {pane}"));
        format!("  --error-hud --error-position {}{pane}", command.error_position)
    } else {
        String::new()
    };
    format!("    {}  {shortcut}{repeat}{error_hud}", command.name)
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use ngterm_proto::keymap::{ControlKeymapAction, ControlKeymapDiagnostic, ControlKeymapMenuItem};
    use serde_json::{Value, json};

    use super::*;
    use crate::{Command as Cli, Ngtermctl};

    /// `ngtermctl keymap <argv>` parsed and validated: the request a run sends as JSON, or the refusal.
    fn outcome(argv: &[&str]) -> Result<Value, String> {
        let line = ["ngtermctl", "keymap"].iter().chain(argv).copied();
        let parsed = Ngtermctl::try_parse_from(line).map_err(|error| error.to_string())?;
        let Cli::Keymap(group) = parsed.command else {
            panic!("not a keymap command")
        };
        group.check()?;
        Ok(serde_json::to_value(group.command().make_request()).unwrap())
    }

    fn request(argv: &[&str]) -> Value {
        outcome(argv).unwrap()
    }

    #[test]
    fn reload_and_list_send_no_args_and_take_no_window() {
        assert_eq!(request(&["reload"]), json!({"cmd": "keymap.reload"}));
        assert_eq!(request(&["list"]), json!({"cmd": "keymap.list"}));
        assert!(outcome(&["reload", "--window", "w1"]).is_err());
        assert!(outcome(&["list", "--window", "w1"]).is_err());
        assert!(outcome(&["list", "--target", "s1"]).is_err());
    }

    #[test]
    fn run_sends_the_name_with_its_target_and_window() {
        assert_eq!(
            request(&["run", "lazy git"]),
            json!({"cmd": "keymap.run", "target": "active", "args": {"name": "lazy git"}})
        );
        assert_eq!(
            request(&["run", "Zed", "--target", "s1", "--window", "w1"]),
            json!({"cmd": "keymap.run", "target": "s1", "args": {"name": "Zed", "window": "w1"}})
        );
        assert!(outcome(&["run"]).is_err());
    }

    #[test]
    fn run_without_a_name_names_its_argument_and_usage() {
        let args: Vec<std::ffi::OsString> = ["ngtermctl", "keymap", "run"].map(Into::into).into();
        let error = Ngtermctl::try_parse_from(&args).unwrap_err();
        let report = crate::parse_report(&args, &error, 80);
        assert_eq!(report.code, crate::usage::USAGE_EXIT);
        assert_eq!(
            report.text,
            "Error: Missing expected argument '<name>'\nHelp:  <name>  Custom command name, from `keymap list`.\n\
             Usage: ngtermctl keymap run <name> [--target <target>] [--socket <socket>] [--json] [--window \
             <window>]\n  See 'ngtermctl keymap run --help' for more information.\n"
        );
    }

    fn action(name: &str, chord: Option<&str>) -> ControlKeymapAction {
        ControlKeymapAction {
            action: name.to_owned(),
            chord: chord.map(str::to_owned),
            alternates: None,
            overridden: None,
            repeats: None,
        }
    }

    fn command(name: &str) -> ControlKeymapCommand {
        ControlKeymapCommand {
            name: name.to_owned(),
            shortcut: None,
            error_hud: false,
            error_position: "center".to_owned(),
            error_pane: None,
            repeats: false,
        }
    }

    fn keymap(actions: Vec<ControlKeymapAction>, commands: Vec<ControlKeymapCommand>) -> ControlKeymap {
        ControlKeymap {
            path: "/tmp/keymap.conf".to_owned(),
            actions,
            commands,
            diagnostics: Vec::new(),
            menu: None,
        }
    }

    #[test]
    fn formats_custom_command_error_options() {
        let placed = ControlKeymapCommand {
            shortcut: Some("ctrl+a>p".to_owned()),
            error_hud: true,
            error_position: "top-right".to_owned(),
            error_pane: Some("left".to_owned()),
            ..command("placed")
        };
        let repeating = ControlKeymapCommand {
            shortcut: Some("ctrl+a>r".to_owned()),
            repeats: true,
            ..command("again")
        };
        let defaulted = ControlKeymapCommand {
            error_hud: true,
            ..command("default")
        };
        let out = format_keymap(&keymap(
            Vec::new(),
            vec![command("quiet"), defaulted, placed, repeating],
        ));
        assert!(out.contains("    quiet  (palette only)\n"), "{out}");
        assert!(
            out.contains("    default  (palette only)  --error-hud --error-position center\n"),
            "{out}"
        );
        assert!(
            out.contains("    placed  ctrl+a>p  --error-hud --error-position top-right --error-pane left\n"),
            "{out}"
        );
        assert!(out.ends_with("    again  ctrl+a>r  --repeat"), "{out}");
    }

    #[test]
    fn formats_every_section_in_order() {
        let mut listing = keymap(
            vec![
                ControlKeymapAction {
                    overridden: Some(true),
                    ..action("close_session", Some("cmd+e"))
                },
                action("rename_session", None),
                ControlKeymapAction {
                    alternates: Some(vec!["ctrl+space>s".to_owned()]),
                    repeats: Some(true),
                    ..action("toggle_split", Some("cmd+t"))
                },
                ControlKeymapAction {
                    alternates: Some(vec!["ctrl+a>g".to_owned()]),
                    ..action("toggle_sidebar", None)
                },
            ],
            vec![ControlKeymapCommand {
                shortcut: Some("cmd+shift+y".to_owned()),
                ..command("Deploy")
            }],
        );
        listing.diagnostics = vec![
            ControlKeymapDiagnostic {
                line: 0,
                message: "conflicts with a built-in; keybind dropped".to_owned(),
            },
            ControlKeymapDiagnostic {
                line: 7,
                message: "unknown action".to_owned(),
            },
        ];
        listing.menu = Some(vec![
            ControlKeymapMenuItem {
                menu: "Navigate".to_owned(),
                title: "Focus Left Pane".to_owned(),
                chord: "cmd+opt+left".to_owned(),
                selector: Some("menuAction:".to_owned()),
                enabled: Some(false),
            },
            ControlKeymapMenuItem {
                menu: "File".to_owned(),
                title: "New Session".to_owned(),
                chord: "cmd+n".to_owned(),
                selector: None,
                enabled: None,
            },
        ]);
        assert_eq!(
            format_keymap(&listing),
            "keymap: /tmp/keymap.conf\n\nactions:\n  * close_session   cmd+e\n    rename_session  -\n    \
             toggle_split    cmd+t|ctrl+space>s  --repeat\n    toggle_sidebar  ctrl+a>g\n\ncommands:\n    Deploy  \
             cmd+shift+y\n\ndiagnostics:\n    conflicts with a built-in; keybind dropped\n    line 7: unknown \
             action\n\nmenu:\n    cmd+opt+left  Navigate ▸ Focus Left Pane  (disabled)\n    cmd+n  File ▸ New Session"
        );
    }

    #[test]
    fn omits_the_optional_sections_when_empty() {
        let out = format_keymap(&keymap(vec![action("close_session", Some("cmd+w"))], Vec::new()));
        assert_eq!(out, "keymap: /tmp/keymap.conf\n\nactions:\n    close_session  cmd+w");
        // a present-but-empty menu still prints its header, as upstream tests `menu` for nil only
        let mut empty_menu = keymap(Vec::new(), Vec::new());
        empty_menu.menu = Some(Vec::new());
        assert_eq!(
            format_keymap(&empty_menu),
            "keymap: /tmp/keymap.conf\n\nactions:\n\nmenu:"
        );
    }
}
