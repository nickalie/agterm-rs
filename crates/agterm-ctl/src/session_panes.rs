//! The `session` subcommands that arrange a session's panes (`agtermctlKit/SessionCommands.swift`,
//! `Session+Swap.swift`, `Session+Lead.swift`): the `split` group (`visibility`, its default, and `close`), `swap`,
//! `lead`, `scratch`, `focus` and `resize`. Each takes its own options first, then `--target`, then
//! [`ClientOptions`], in upstream's declaration order.

use agterm_core::model::SplitAxis;
use agterm_proto::command::SessionCmd;
use agterm_proto::{Args, Request};

use crate::session::session_request;
use crate::session_io::validate_pane;
use crate::{BasicOptions, ClientOptions, RequestCommand, TargetOptions};

/// `ngtermctl session split <subcommand>`; a bare `session split [on|off|toggle]` runs `visibility`, the library's
/// default subcommand ([`crate::usage::name_default_subcommands`]).
#[derive(Debug, clap::Args)]
#[command(
    about = "Show, hide, or close a session split.",
    subcommand_required = true,
    arg_required_else_help = true
)]
pub struct Split {
    #[command(subcommand)]
    command: SplitCommand,
}

impl Split {
    /// The subcommand this invocation runs.
    pub fn command(&self) -> &dyn RequestCommand {
        match &self.command {
            SplitCommand::Visibility(command) => command,
            SplitCommand::Close(command) => command,
        }
    }
}

#[derive(Debug, clap::Subcommand)]
enum SplitCommand {
    Visibility(SplitVisibility),
    Close(SplitClose),
}

/// `ngtermctl session split [visibility] [on|off|toggle] [--axis vertical|horizontal]`.
#[derive(Debug, clap::Args)]
#[command(about = "Show or hide a session split (on|off|toggle).")]
pub struct SplitVisibility {
    #[arg(
        default_value = "toggle",
        help = "Mode: on (show), off (hide), or toggle (default). Hidden panes stay alive."
    )]
    mode: String,

    #[arg(long, help = "Divider direction: vertical (left/right) or horizontal (top/bottom).")]
    axis: Option<String>,

    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for SplitVisibility {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn validate(&self) -> Result<(), String> {
        match self.axis.as_deref() {
            Some(axis) if SplitAxis::from_wire(axis).is_none() => {
                Err("--axis must be vertical or horizontal".to_owned())
            }
            _ => Ok(()),
        }
    }

    fn make_request(&self) -> Request {
        let args = Args {
            mode: Some(self.mode.clone()),
            axis: self.axis.clone(),
            ..Args::default()
        };
        session_request(
            SessionCmd::Split,
            Some(self.target.target.clone()),
            Some(args),
            &self.options,
        )
    }
}

/// `ngtermctl session split close` (wire `session.split.close`).
#[derive(Debug, clap::Args)]
#[command(about = "Close the split pane (destroys it, killing whatever it runs); ok when there is none.")]
pub struct SplitClose {
    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for SplitClose {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn make_request(&self) -> Request {
        session_request(
            SessionCmd::SplitClose,
            Some(self.target.target.clone()),
            None,
            &self.options,
        )
    }
}

/// `ngtermctl session swap`.
#[derive(Debug, clap::Args)]
#[command(about = "Exchange a split session's two panes, including their live roles and state.")]
pub struct Swap {
    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Swap {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn make_request(&self) -> Request {
        session_request(SessionCmd::Swap, Some(self.target.target.clone()), None, &self.options)
    }
}

/// `ngtermctl session lead [--pane P]`. The abstract and discussion are upstream's, `Mac` included.
#[derive(Debug, clap::Args)]
#[command(
    about = "Take the lead of a pane's terminal for this Mac.",
    long_about = "A session attached from another Mac has one leading client per pane, whose window size the program \
                  sees. A pane that does not lead is covered; this uncovers it, as pressing a key on the cover does, \
                  and covers it on the other Mac. Read each pane's `lead` from `tree`."
)]
pub struct Lead {
    #[arg(
        long,
        help = "Which pane: primary/left/top or split/right/bottom. Defaults to primary."
    )]
    pane: Option<String>,

    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Lead {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn validate(&self) -> Result<(), String> {
        validate_pane(self.pane.as_deref())
    }

    fn make_request(&self) -> Request {
        let args = Args {
            pane: self.pane.clone(),
            ..Args::default()
        };
        // upstream always builds the bag, so a bare `session lead` sends `"args": {}`
        session_request(
            SessionCmd::Lead,
            Some(self.target.target.clone()),
            Some(args),
            &self.options,
        )
    }
}

/// `ngtermctl session scratch [on|off|toggle] [--command CMD]`.
#[derive(Debug, clap::Args)]
#[command(about = "Show or hide a session scratch terminal (on|off|toggle).")]
pub struct Scratch {
    #[arg(
        default_value = "toggle",
        help = "Mode: on (show), off (hide), or toggle (default). The hidden scratch shell stays alive."
    )]
    mode: String,

    #[arg(
        long,
        help = "When showing, run this command as the scratch's process instead of a login shell (run-once; respawns \
                the scratch if one is already open)."
    )]
    command: Option<String>,

    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Scratch {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn make_request(&self) -> Request {
        let args = Args {
            mode: Some(self.mode.clone()),
            command: self.command.clone(),
            ..Args::default()
        };
        session_request(
            SessionCmd::Scratch,
            Some(self.target.target.clone()),
            Some(args),
            &self.options,
        )
    }
}

/// `ngtermctl session focus [pane]`: the pane goes as typed, the server resolves its aliases.
#[derive(Debug, clap::Args)]
#[command(about = "Focus a split session's pane by position or role.")]
pub struct Focus {
    #[arg(
        default_value = "other",
        help = "Pane: primary/left/top, split/right/bottom, or other (toggle, default)."
    )]
    pane: String,

    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Focus {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn make_request(&self) -> Request {
        let args = Args {
            pane: Some(self.pane.clone()),
            ..Args::default()
        };
        session_request(
            SessionCmd::Focus,
            Some(self.target.target.clone()),
            Some(args),
            &self.options,
        )
    }
}

/// `ngtermctl session resize`: exactly one absolute ratio or one relative grow, each alias a signed delta of the
/// primary pane's fraction. Prints the applied, clamped fraction ([`crate::output`]).
#[derive(Debug, clap::Args)]
#[command(about = "Resize a split session's divider (set or nudge the primary-pane fraction).")]
pub struct Resize {
    #[arg(
        long = "split-ratio",
        help = "Absolute primary-pane fraction 0..1 of the area below the titlebar (left or top; e.g. 0.7). Clamped \
                to 0.05..0.95."
    )]
    split_ratio: Option<f64>,

    #[arg(
        long = "grow-left",
        help = "Grow the left pane by this fraction (e.g. 0.05); shrinks the right."
    )]
    grow_left: Option<f64>,

    #[arg(
        long = "grow-right",
        help = "Grow the right pane by this fraction (e.g. 0.05); shrinks the left."
    )]
    grow_right: Option<f64>,

    #[arg(long = "grow-primary", help = "Grow the primary pane by this fraction.")]
    grow_primary: Option<f64>,

    #[arg(long = "grow-split", help = "Grow the split pane by this fraction.")]
    grow_split: Option<f64>,

    #[arg(long = "grow-top", help = "Alias for --grow-primary in a horizontal split.")]
    grow_top: Option<f64>,

    #[arg(long = "grow-bottom", help = "Alias for --grow-split in a horizontal split.")]
    grow_bottom: Option<f64>,

    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl Resize {
    /// Every form given, in upstream's order.
    fn values(&self) -> Vec<f64> {
        [
            self.split_ratio,
            self.grow_left,
            self.grow_right,
            self.grow_primary,
            self.grow_split,
            self.grow_top,
            self.grow_bottom,
        ]
        .into_iter()
        .flatten()
        .collect()
    }
}

impl RequestCommand for Resize {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn validate(&self) -> Result<(), String> {
        match self.values().as_slice() {
            // nan and inf parse but cannot go on the wire as JSON numbers
            [value] if !value.is_finite() => Err("the resize value must be a finite number".to_owned()),
            [_] => Ok(()),
            _ => Err("provide exactly one split ratio or grow option".to_owned()),
        }
    }

    fn make_request(&self) -> Request {
        let args = if let Some(ratio) = self.split_ratio {
            Args {
                ratio: Some(ratio),
                ..Args::default()
            }
        } else if let Some(grow) = self.grow_left.or(self.grow_primary).or(self.grow_top) {
            Args {
                ratio_delta: Some(grow),
                ..Args::default()
            }
        } else {
            let shrink = self.grow_right.or(self.grow_split).or(self.grow_bottom).unwrap_or(0.0);
            Args {
                ratio_delta: Some(-shrink),
                ..Args::default()
            }
        };
        session_request(
            SessionCmd::Resize,
            Some(self.target.target.clone()),
            Some(args),
            &self.options,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use clap::{CommandFactory, Parser};
    use serde_json::{Value, json};

    use crate::{Agtermctl, usage};

    /// `ngtermctl session <argv>` as the binary parses it, the default subcommand named.
    fn parsed(argv: &[&str]) -> crate::session::Session {
        let command_line: Vec<OsString> = ["ngtermctl", "session"]
            .iter()
            .chain(argv)
            .map(OsString::from)
            .collect();
        let command_line = usage::name_default_subcommands(&Agtermctl::command(), &command_line);
        let crate::Command::Session(group) = Agtermctl::try_parse_from(command_line).unwrap().command else {
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
    fn split_defaults_to_visibility_and_toggle() {
        let toggle = json!({"cmd": "session.split", "target": "active", "args": {"mode": "toggle"}});
        assert_eq!(request(&["split"]), toggle);
        assert_eq!(request(&["split", "visibility"]), toggle);
        assert_eq!(
            request(&["split", "on"]),
            json!({"cmd": "session.split", "target": "active", "args": {"mode": "on"}})
        );
        assert_eq!(
            request(&["split", "off", "--target", "s1", "--window", "w1"]),
            json!({"cmd": "session.split", "target": "s1", "args": {"mode": "off", "window": "w1"}})
        );
        assert_eq!(
            request(&["split", "visibility", "on", "--axis", "vertical"]),
            json!({"cmd": "session.split", "target": "active", "args": {"mode": "on", "axis": "vertical"}})
        );
    }

    #[test]
    fn split_takes_an_optional_axis_and_refuses_another() {
        assert_eq!(
            request(&["split", "--axis", "horizontal"]),
            json!({"cmd": "session.split", "target": "active", "args": {"mode": "toggle", "axis": "horizontal"}})
        );
        for axis in ["diagonal", "Vertical", ""] {
            assert_eq!(
                refusal(&["split", "--axis", axis]),
                "--axis must be vertical or horizontal",
                "{axis:?}"
            );
        }
        // the mode is the server's to check
        assert_eq!(request(&["split", "sideways"])["args"]["mode"], "sideways");
    }

    #[test]
    fn the_default_subcommand_is_named_where_the_library_falls_back_to_it() {
        let named = |argv: &[&str]| -> Vec<String> {
            let args: Vec<OsString> = std::iter::once("ngtermctl")
                .chain(argv.iter().copied())
                .map(OsString::from)
                .collect();
            usage::name_default_subcommands(&Agtermctl::command(), &args)
                .into_iter()
                .skip(1)
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect()
        };
        let cases: [(&[&str], &[&str]); 8] = [
            (&["session", "split"], &["session", "split", "visibility"]),
            (&["session", "split", "on"], &["session", "split", "visibility", "on"]),
            (&["session", "split", "close"], &["session", "split", "close"]),
            (
                &["session", "split", "visibility", "off"],
                &["session", "split", "visibility", "off"],
            ),
            // only the next argument can name a subcommand
            (
                &["session", "split", "--target", "s1", "close"],
                &["session", "split", "visibility", "--target", "s1", "close"],
            ),
            // a lone help flag is the group's; beside anything else it is the default subcommand's
            (&["session", "split", "--help"], &["session", "split", "--help"]),
            (
                &["session", "split", "on", "-h"],
                &["session", "split", "visibility", "on", "-h"],
            ),
            (&["help", "session", "split"], &["help", "session", "split"]),
        ];
        for (argv, expected) in cases {
            assert_eq!(named(argv), expected, "{argv:?}");
        }
        assert_eq!(named(&["session", "scratch", "on"]), ["session", "scratch", "on"]);
    }

    #[test]
    fn split_close_carries_args_only_for_a_window() {
        assert_eq!(
            request(&["split", "close"]),
            json!({"cmd": "session.split.close", "target": "active"})
        );
        assert_eq!(
            request(&["split", "close", "--target", "s1", "--window", "w1"]),
            json!({"cmd": "session.split.close", "target": "s1", "args": {"window": "w1"}})
        );
    }

    #[test]
    fn swap_carries_args_only_for_a_window() {
        assert_eq!(request(&["swap"]), json!({"cmd": "session.swap", "target": "active"}));
        assert_eq!(
            request(&["swap", "--target", "s1", "--window", "w1"]),
            json!({"cmd": "session.swap", "target": "s1", "args": {"window": "w1"}})
        );
    }

    #[test]
    fn lead_carries_the_pane_and_always_an_args_bag() {
        assert_eq!(
            request(&["lead"]),
            json!({"cmd": "session.lead", "target": "active", "args": {}})
        );
        assert_eq!(
            request(&["lead", "--target", "s1", "--pane", "right", "--window", "w1"]),
            json!({"cmd": "session.lead", "target": "s1", "args": {"pane": "right", "window": "w1"}})
        );
        for pane in ["primary", "left", "top", "split", "right", "bottom", "scratch"] {
            assert_eq!(request(&["lead", "--pane", pane])["args"]["pane"], pane);
        }
        assert_eq!(
            refusal(&["lead", "--pane", "other"]),
            "--pane must be left, right, or scratch"
        );
    }

    #[test]
    fn scratch_defaults_to_toggle_and_carries_a_command() {
        assert_eq!(
            request(&["scratch"]),
            json!({"cmd": "session.scratch", "target": "active", "args": {"mode": "toggle"}})
        );
        assert_eq!(
            request(&["scratch", "off"]),
            json!({"cmd": "session.scratch", "target": "active", "args": {"mode": "off"}})
        );
        assert_eq!(
            request(&["scratch", "on", "--command", "htop", "--target", "s1", "--window", "w1"]),
            json!({"cmd": "session.scratch", "target": "s1",
                   "args": {"mode": "on", "command": "htop", "window": "w1"}})
        );
    }

    #[test]
    fn focus_defaults_to_other_and_passes_aliases_verbatim() {
        assert_eq!(
            request(&["focus"]),
            json!({"cmd": "session.focus", "target": "active", "args": {"pane": "other"}})
        );
        for pane in ["primary", "left", "top", "split", "right", "bottom"] {
            assert_eq!(request(&["focus", pane])["args"]["pane"], pane);
        }
        assert_eq!(
            request(&["focus", "right", "--target", "s1", "--window", "w1"]),
            json!({"cmd": "session.focus", "target": "s1", "args": {"pane": "right", "window": "w1"}})
        );
    }

    #[test]
    fn resize_maps_each_form_to_a_ratio_or_a_signed_primary_delta() {
        assert_eq!(
            request(&["resize", "--split-ratio", "0.7"]),
            json!({"cmd": "session.resize", "target": "active", "args": {"ratio": 0.7}})
        );
        for option in ["--grow-left", "--grow-primary", "--grow-top"] {
            assert_eq!(
                request(&["resize", option, "0.05"])["args"],
                json!({"ratioDelta": 0.05}),
                "{option}"
            );
        }
        for option in ["--grow-right", "--grow-split", "--grow-bottom"] {
            assert_eq!(
                request(&["resize", option, "0.05"])["args"],
                json!({"ratioDelta": -0.05}),
                "{option}"
            );
        }
        assert_eq!(
            request(&["resize", "--grow-left=-0.1", "--target", "s1", "--window", "w1"]),
            json!({"cmd": "session.resize", "target": "s1", "args": {"ratioDelta": -0.1, "window": "w1"}})
        );
    }

    #[test]
    fn resize_requires_exactly_one_finite_form() {
        let cases: [(&[&str], &str); 6] = [
            (&[], "provide exactly one split ratio or grow option"),
            (
                &["--split-ratio", "0.7", "--grow-left", "0.1"],
                "provide exactly one split ratio or grow option",
            ),
            (
                &["--grow-top", "0.1", "--grow-bottom", "nan"],
                "provide exactly one split ratio or grow option",
            ),
            (&["--split-ratio", "nan"], "the resize value must be a finite number"),
            (&["--grow-left", "inf"], "the resize value must be a finite number"),
            (&["--split-ratio", "1e999"], "the resize value must be a finite number"),
        ];
        for (argv, message) in cases {
            assert_eq!(refusal(&[&["resize"][..], argv].concat()), message, "{argv:?}");
        }
    }

    #[test]
    fn none_of_them_echo_the_result_id() {
        for argv in [
            &["split"][..],
            &["split", "close"],
            &["swap"],
            &["lead"],
            &["scratch"],
            &["focus"],
            &["resize", "--split-ratio", "0.5"],
        ] {
            assert!(!parsed(argv).command().echoes_result_id(), "{argv:?}");
        }
    }
}
