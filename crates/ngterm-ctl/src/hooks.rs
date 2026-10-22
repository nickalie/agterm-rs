//! `ngtermctl hooks reload|list` (`agtermctlKit/MiscCommands.swift` `Hooks`): app-global like the keymap commands, so
//! neither takes `--window`. `list` prints through [`format_hooks`], `reload` its diagnostic count through
//! [`crate::output`]'s `count` branch.

use ngterm_core::paths::Env;
use ngterm_proto::command::HooksCmd;
use ngterm_proto::keymap::{ControlHookEntry, ControlHooks};
use ngterm_proto::{Command, Request};

use crate::{BasicOptions, Failure, RequestCommand};

/// `ngtermctl hooks <subcommand>`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Event hook commands.",
    subcommand_required = true,
    arg_required_else_help = true
)]
pub struct Hooks {
    #[command(subcommand)]
    command: HooksCommand,
}

/// The subcommands, in upstream's `Hooks.configuration.subcommands` order.
#[derive(Debug, clap::Subcommand)]
enum HooksCommand {
    Reload(Reload),
    List(List),
}

impl Hooks {
    /// The subcommand this invocation runs.
    fn command(&self) -> &dyn RequestCommand {
        match &self.command {
            HooksCommand::Reload(command) => command,
            HooksCommand::List(command) => command,
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

/// A request for `cmd`: neither subcommand takes a target or args.
fn bare_request(cmd: HooksCmd) -> Request {
    Request {
        cmd: Command::Hooks(cmd),
        target: None,
        args: None,
    }
}

/// `ngtermctl hooks reload`: prints the diagnostic count, `ok` for none.
#[derive(Debug, clap::Args)]
#[command(about = "Re-read and apply hooks.conf (prints the diagnostic count).")]
pub struct Reload {
    #[command(flatten)]
    options: BasicOptions,
}

impl RequestCommand for Reload {
    fn options(&self) -> &BasicOptions {
        &self.options
    }

    fn make_request(&self) -> Request {
        bare_request(HooksCmd::Reload)
    }
}

/// `ngtermctl hooks list`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Show every hook with its running child, queue depth, dropped count and last failure.",
    long_about = "One row per `on <kind> <shell...>` line in file order, then any hook removed from the file whose \
                  child is still running, marked retired. `running` is the child's pid and elapsed seconds while a \
                  hook is busy; `pending` is how many events wait behind it and `dropped` how many the bounded queue \
                  discarded; `last failure` stays until the hook's next clean run (a reload keeps it)."
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
        bare_request(HooksCmd::List)
    }
}

/// Renders the `hooks.list` payload (upstream's `SocketClient.formatHooks`, no trailing newline): one row per hook in
/// file order, `(no hooks)` for none, then the parse diagnostics. Unlike the keymap listing, a diagnostic always
/// carries its line number.
#[must_use]
pub fn format_hooks(hooks: &ControlHooks) -> String {
    let mut lines = vec![format!("hooks: {}", hooks.path)];
    if hooks.hooks.is_empty() {
        lines.push("  (no hooks)".to_owned());
    }
    lines.extend(hooks.hooks.iter().map(hook_row));
    if !hooks.diagnostics.is_empty() {
        lines.extend([String::new(), "diagnostics:".to_owned()]);
        lines.extend(
            hooks
                .diagnostics
                .iter()
                .map(|diagnostic| format!("    line {}: {}", diagnostic.line, diagnostic.message)),
        );
    }
    lines.join("\n")
}

/// One hook's row: its definition, then whichever live facts it has.
fn hook_row(hook: &ControlHookEntry) -> String {
    let running = hook.running_pid.map_or_else(String::new, |pid| {
        let elapsed = hook
            .elapsed_seconds
            .map_or_else(String::new, |elapsed| format!(" for {}s", whole_seconds(elapsed)));
        format!("  running pid {pid}{elapsed}")
    });
    let pending = if hook.pending > 0 {
        format!("  pending {}", hook.pending)
    } else {
        String::new()
    };
    let dropped = if hook.dropped > 0 {
        format!("  dropped {}", hook.dropped)
    } else {
        String::new()
    };
    let failure = hook
        .last_failure
        .as_ref()
        .map_or_else(String::new, |failure| format!("  last failure: {failure}"));
    let retired = if hook.retired == Some(true) {
        "  (retired, removed from the file)"
    } else {
        ""
    };
    format!(
        "  line {}: on {} {}{running}{pending}{dropped}{failure}{retired}",
        hook.line, hook.kind, hook.command
    )
}

/// Elapsed seconds truncated toward zero, as Swift's `Int(_:)`.
#[expect(
    clippy::cast_possible_truncation,
    reason = "a child's run time fits i64; `as` truncates toward zero as Swift's `Int(_:)`, saturating the absurd"
)]
fn whole_seconds(elapsed: f64) -> i64 {
    elapsed as i64
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use ngterm_proto::keymap::ControlKeymapDiagnostic;
    use serde_json::{Value, json};

    use super::*;
    use crate::{Command as Cli, Ngtermctl};

    /// `ngtermctl hooks <argv>` parsed and validated: the request a run sends as JSON, or the refusal.
    fn outcome(argv: &[&str]) -> Result<Value, String> {
        let line = ["ngtermctl", "hooks"].iter().chain(argv).copied();
        let parsed = Ngtermctl::try_parse_from(line).map_err(|error| error.to_string())?;
        let Cli::Hooks(group) = parsed.command else {
            panic!("not a hooks command")
        };
        group.check()?;
        Ok(serde_json::to_value(group.command().make_request()).unwrap())
    }

    #[test]
    fn reload_and_list_send_no_args() {
        assert_eq!(outcome(&["reload"]).unwrap(), json!({"cmd": "hooks.reload"}));
        assert_eq!(outcome(&["list"]).unwrap(), json!({"cmd": "hooks.list"}));
    }

    #[test]
    fn neither_takes_a_window_or_target() {
        assert!(outcome(&["reload", "--window", "w1"]).is_err());
        assert!(outcome(&["list", "--window", "w1"]).is_err());
        assert!(outcome(&["list", "--target", "s1"]).is_err());
    }

    fn hook(kind: &str, command: &str, line: i64) -> ControlHookEntry {
        ControlHookEntry {
            kind: kind.to_owned(),
            command: command.to_owned(),
            line,
            running_pid: None,
            elapsed_seconds: None,
            pending: 0,
            dropped: 0,
            last_failure: None,
            retired: None,
        }
    }

    #[test]
    fn formats_hooks_rows_and_diagnostics() {
        let payload = ControlHooks {
            path: "/tmp/hooks.conf".to_owned(),
            diagnostics: vec![ControlKeymapDiagnostic {
                line: 9,
                message: "unknown verb 'map'".to_owned(),
            }],
            hooks: vec![
                hook("status", "~/s.sh", 1),
                ControlHookEntry {
                    running_pid: Some(4242),
                    elapsed_seconds: Some(12.7),
                    pending: 3,
                    dropped: 1,
                    last_failure: Some("exit 2".to_owned()),
                    ..hook("notify", "echo x | cat", 4)
                },
                ControlHookEntry {
                    running_pid: Some(77),
                    elapsed_seconds: Some(0.2),
                    retired: Some(true),
                    ..hook("status", "~/old.sh", 2)
                },
                // a pid without an elapsed time prints no duration
                ControlHookEntry {
                    running_pid: Some(5),
                    ..hook("status", "~/x.sh", 3)
                },
            ],
        };
        assert_eq!(
            format_hooks(&payload),
            "hooks: /tmp/hooks.conf\n  line 1: on status ~/s.sh\n  line 4: on notify echo x | cat  running pid 4242 \
             for 12s  pending 3  dropped 1  last failure: exit 2\n  line 2: on status ~/old.sh  running pid 77 for \
             0s  (retired, removed from the file)\n  line 3: on status ~/x.sh  running pid 5\n\ndiagnostics:\n    \
             line 9: unknown verb 'map'"
        );
    }

    #[test]
    fn an_empty_listing_says_so_and_a_line_0_diagnostic_keeps_its_number() {
        let empty = ControlHooks {
            path: "/p".to_owned(),
            diagnostics: Vec::new(),
            hooks: Vec::new(),
        };
        assert_eq!(format_hooks(&empty), "hooks: /p\n  (no hooks)");
        let whole_file = ControlHooks {
            diagnostics: vec![ControlKeymapDiagnostic {
                line: 0,
                message: "unreadable".to_owned(),
            }],
            ..empty
        };
        assert_eq!(
            format_hooks(&whole_file),
            "hooks: /p\n  (no hooks)\n\ndiagnostics:\n    line 0: unreadable"
        );
    }
}
