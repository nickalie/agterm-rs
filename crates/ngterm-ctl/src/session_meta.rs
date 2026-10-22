//! The `session` subcommands that set per-session metadata rather than drive a surface
//! (`agtermctlKit/SessionMetadataCommands.swift`, `Session.Seen`): `status`, `flag`, `context` and `seen`.

use ngterm_core::model::{StatusShape, is_valid_color_hex};
use ngterm_proto::command::SessionCmd;
use ngterm_proto::{Args, Request};

use crate::session::session_request;
use crate::session_io::validate_pane;
use crate::{BasicOptions, ClientOptions, RequestCommand, TargetOptions};

/// The shapes comma-joined in upstream's `StatusShape.allCases` order (upstream's `StatusShape.validNamesPhrase`).
fn shapes_phrase() -> String {
    StatusShape::ALL.map(StatusShape::wire_name).join(", ")
}

/// `ngtermctl session status <state>`: the state and its overrides are the server's to check, the spellings the
/// CLI's. The `--sound` help is upstream's, its macOS sound names included.
#[derive(Debug, clap::Args)]
#[command(about = "Set a session's agent status indicator.")]
pub struct Status {
    #[arg(help = "State: idle, active, completed, or blocked.")]
    state: String,

    #[arg(long, help = "Pulse the indicator for attention.")]
    blink: bool,

    #[arg(long, help = "Reset the indicator to idle once the session is visited.")]
    auto_reset: bool,

    #[arg(
        long,
        help = "Play a sound when set: 'default' (or 'beep') for the system alert sound, or a system sound name \
                (Basso, Blow, Bottle, Frog, Funk, Glass, Hero, Morse, Ping, Pop, Purr, Sosumi, Submarine, Tink)."
    )]
    sound: Option<String>,

    #[arg(
        long,
        help = "Override the glyph tint for this call only (#rrggbb); reverts on the next status set without it."
    )]
    color: Option<String>,

    #[arg(
        long,
        help = format!(
            "Override the glyph silhouette for this call only: {}; reverts on the next status set without it.",
            shapes_phrase()
        )
    )]
    shape: Option<String>,

    #[arg(
        long,
        help = "Which pane set this status: primary/left/top, split/right/bottom, or scratch. Records the blocked \
                pane so navigation reaches it. Defaults to primary."
    )]
    pane: Option<String>,

    #[arg(
        long = "pane-id",
        help = "A surface's stable token (the shell's $NGTERM_PANE_ID) — the agent-status hook forwards it so a \
                status set from a promoted-then-re-split pane lands on the pane's current slot. Overrides --pane \
                when it resolves; falls back to --pane otherwise."
    )]
    pane_id: Option<String>,

    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Status {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn validate(&self) -> Result<(), String> {
        if self.color.as_deref().is_some_and(|color| !is_valid_color_hex(color)) {
            return Err("color must be a #rrggbb hex value".to_owned());
        }
        if self
            .shape
            .as_deref()
            .is_some_and(|shape| StatusShape::from_wire(shape).is_none())
        {
            return Err(format!("shape must be one of: {}", shapes_phrase()));
        }
        validate_pane(self.pane.as_deref())
    }

    fn make_request(&self) -> Request {
        let args = Args {
            pane: self.pane.clone(),
            pane_id: self.pane_id.clone(),
            status: Some(self.state.clone()),
            blink: self.blink.then_some(true),
            auto_reset: self.auto_reset.then_some(true),
            sound: self.sound.clone(),
            color: self.color.clone(),
            shape: self.shape.clone(),
            ..Args::default()
        };
        session_request(
            SessionCmd::Status,
            Some(self.target.target.clone()),
            Some(args),
            &self.options,
        )
    }
}

/// `ngtermctl session flag [on|off|toggle|clear]`.
#[derive(Debug, clap::Args)]
#[command(about = "Flag a session for the flagged working-set view (on|off|toggle|clear).")]
pub struct Flag {
    #[arg(
        default_value = "toggle",
        help = "Mode: on, off, toggle (default), or clear (unflag all; ignores --target)."
    )]
    mode: String,

    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Flag {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn validate(&self) -> Result<(), String> {
        if ["on", "off", "toggle", "clear"].contains(&self.mode.as_str()) {
            Ok(())
        } else {
            Err("mode must be on, off, toggle, or clear".to_owned())
        }
    }

    fn make_request(&self) -> Request {
        let args = Args {
            mode: Some(self.mode.clone()),
            ..Args::default()
        };
        session_request(
            SessionCmd::Flag,
            Some(self.target.target.clone()),
            Some(args),
            &self.options,
        )
    }
}

/// `ngtermctl session context <text>|--clear`: exactly one form. The text goes as typed; trimming and its limits are
/// the server's.
#[derive(Debug, clap::Args)]
#[command(
    about = "Set or clear what a session is about, shown in the title bar.",
    long_about = "The text says what the session is FOR — a PR, an issue, a task. It persists across a relaunch and \
                  stays until --clear; nothing expires it."
)]
pub struct Context {
    #[arg(
        help = "The context text (omit with --clear). Trimmed; max 256 UTF-8 bytes; no control characters or line \
                breaks."
    )]
    text: Option<String>,

    #[arg(long, help = "Remove the context.")]
    clear: bool,

    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Context {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn validate(&self) -> Result<(), String> {
        if self.text.is_some() == self.clear {
            Err("provide exactly one of a TEXT or --clear".to_owned())
        } else {
            Ok(())
        }
    }

    fn make_request(&self) -> Request {
        let args = Args {
            text: self.text.clone(),
            mode: Some(if self.clear { "clear" } else { "set" }.to_owned()),
            ..Args::default()
        };
        session_request(
            SessionCmd::Context,
            Some(self.target.target.clone()),
            Some(args),
            &self.options,
        )
    }
}

/// `ngtermctl session seen`.
#[derive(Debug, clap::Args)]
#[command(about = "Clear a session's unseen-notification badge without changing the selection or focus (idempotent).")]
pub struct Seen {
    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Seen {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn make_request(&self) -> Request {
        session_request(SessionCmd::Seen, Some(self.target.target.clone()), None, &self.options)
    }
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};
    use serde_json::{Value, json};

    use super::*;
    use crate::Ngtermctl;

    fn parsed(argv: &[&str]) -> crate::session::Session {
        let command_line = ["ngtermctl", "session"].iter().chain(argv).copied();
        let crate::Command::Session(group) = Ngtermctl::try_parse_from(command_line).unwrap().command else {
            panic!("not a session command")
        };
        group
    }

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

    #[test]
    fn status_sends_the_state_and_only_the_overrides_given() {
        let cases: [(&[&str], Value); 7] = [
            (
                &["active", "--blink"],
                json!({"target": "active", "args": {"status": "active", "blink": true}}),
            ),
            (
                &["completed", "--target", "s1"],
                json!({"target": "s1", "args": {"status": "completed"}}),
            ),
            (
                &["completed", "--auto-reset"],
                json!({"target": "active", "args": {"status": "completed", "autoReset": true}}),
            ),
            (
                &[
                    "blocked", "--sound", "Glass", "--color", "#ff0000", "--shape", "triangle",
                ],
                json!({"target": "active",
                       "args": {"status": "blocked", "sound": "Glass", "color": "#ff0000", "shape": "triangle"}}),
            ),
            (
                &["blocked", "--pane", "right", "--pane-id", "agent-tok", "--target", "s1"],
                json!({"target": "s1", "args": {"pane": "right", "paneID": "agent-tok", "status": "blocked"}}),
            ),
            (
                &["blocked", "--pane", "scratch"],
                json!({"target": "active", "args": {"pane": "scratch", "status": "blocked"}}),
            ),
            (
                &["idle", "--window", "w1"],
                json!({"target": "active", "args": {"status": "idle", "window": "w1"}}),
            ),
        ];
        for (argv, mut expected) in cases {
            expected["cmd"] = json!("session.status");
            assert_eq!(request(&[&["status"][..], argv].concat()), expected, "{argv:?}");
        }
        for shape in StatusShape::ALL.map(StatusShape::wire_name) {
            assert_eq!(request(&["status", "active", "--shape", shape])["args"]["shape"], shape);
        }
        // the state is the server's to check
        assert_eq!(request(&["status", "busy"])["args"]["status"], "busy");
    }

    #[test]
    fn status_refuses_a_bad_color_shape_or_pane_in_upstream_order() {
        let cases: [(&[&str], &str); 5] = [
            (&["--color", "nope"], "color must be a #rrggbb hex value"),
            (&["--color", "#ff00"], "color must be a #rrggbb hex value"),
            (
                &["--shape", "hexagon"],
                "shape must be one of: circle, square, triangle, diamond, capsule, star",
            ),
            (&["--pane", "other"], "--pane must be left, right, or scratch"),
            (
                &["--color", "x", "--shape", "y", "--pane", "z"],
                "color must be a #rrggbb hex value",
            ),
        ];
        for (argv, message) in cases {
            assert_eq!(
                refusal(&[&["status", "blocked"][..], argv].concat()),
                message,
                "{argv:?}"
            );
        }
        // the `#` is optional, as upstream's
        assert_eq!(
            request(&["status", "blocked", "--color", "A0b1C2"])["args"]["color"],
            "A0b1C2"
        );
    }

    #[test]
    fn status_shape_help_lists_every_shape() {
        let mut root = Ngtermctl::command();
        let status = root
            .find_subcommand_mut("session")
            .and_then(|session| session.find_subcommand_mut("status"))
            .unwrap();
        let help = status.render_help().to_string();
        for shape in StatusShape::ALL.map(StatusShape::wire_name) {
            assert!(help.contains(shape), "{help}");
        }
    }

    #[test]
    fn flag_defaults_to_toggle_and_refuses_an_unknown_mode() {
        assert_eq!(
            request(&["flag"]),
            json!({"cmd": "session.flag", "target": "active", "args": {"mode": "toggle"}})
        );
        assert_eq!(
            request(&["flag", "on", "--target", "9f3c"]),
            json!({"cmd": "session.flag", "target": "9f3c", "args": {"mode": "on"}})
        );
        assert_eq!(
            request(&["flag", "clear", "--window", "w1"]),
            json!({"cmd": "session.flag", "target": "active", "args": {"mode": "clear", "window": "w1"}})
        );
        assert_eq!(refusal(&["flag", "bogus"]), "mode must be on, off, toggle, or clear");
    }

    #[test]
    fn context_sets_verbatim_text_or_clears() {
        assert_eq!(
            request(&["context", "PR #517", "--target", "s1"]),
            json!({"cmd": "session.context", "target": "s1", "args": {"text": "PR #517", "mode": "set"}})
        );
        assert_eq!(
            request(&["context", "--clear"]),
            json!({"cmd": "session.context", "target": "active", "args": {"mode": "clear"}})
        );
        let padded = "  PR #517: reap ordering  ";
        assert_eq!(request(&["context", padded])["args"]["text"], padded);
        for argv in [&["context"][..], &["context", "PR #517", "--clear"]] {
            assert_eq!(refusal(argv), "provide exactly one of a TEXT or --clear", "{argv:?}");
        }
    }

    #[test]
    fn seen_carries_args_only_for_a_window() {
        assert_eq!(request(&["seen"]), json!({"cmd": "session.seen", "target": "active"}));
        assert_eq!(
            request(&["seen", "--target", "x", "--window", "win"]),
            json!({"cmd": "session.seen", "target": "x", "args": {"window": "win"}})
        );
    }
}
