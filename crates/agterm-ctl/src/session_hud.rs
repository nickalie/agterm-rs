//! `agtermctl session hud [open]|update|close` (`agtermctlKit/SessionHudCommands.swift` `Session.Hud`): the passive
//! message panel over a session. `open` is the group's default subcommand
//! ([`crate::usage::name_default_subcommands`]), so posting one is `agtermctl session hud "gathering options…"`; a
//! message that is literally `update` or `close` needs the explicit `open` verb. Message length and control characters
//! are the server's to refuse: only what needs no socket is checked here, with the server's own vocabularies
//! ([`agterm_core::hud`]), so a value fails the same way locally and over a raw socket.

use std::fs;

use agterm_core::hud::{HudPosition, HudSpec, HudSpinner, MAX_SIZE_PERCENT, MIN_SIZE_PERCENT};
use agterm_core::model::is_valid_color_hex;
use agterm_core::pane_env::StatusPane;
use agterm_proto::command::SessionCmd;
use agterm_proto::{Args, Request};

use crate::client::os_message;
use crate::session::{flag, session_request, validate_size_percent};
use crate::{BasicOptions, ClientOptions, Failure, RequestCommand, TargetOptions};

/// Upstream's `Hud.stickyHelp`.
const STICKY_HELP: &str = "Put the panel flush against the edge or corner --position names, with no margin; center \
                           has no edge and ignores it. Add --size-percent 100 for a strip across the pane.";
/// Upstream's `Hud.noFrameHelp`.
const NO_FRAME_HELP: &str = "Draw no border, no rounded corners and no blank row above and below the text.";

/// `agtermctl session hud <subcommand>`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Post, update, or close a passive message panel over a session.",
    subcommand_required = true,
    arg_required_else_help = true
)]
pub struct Hud {
    #[command(subcommand)]
    command: HudCommand,
}

impl Hud {
    /// The subcommand this invocation runs.
    pub fn command(&self) -> &dyn RequestCommand {
        match &self.command {
            HudCommand::Open(command) => command.as_ref(),
            HudCommand::Update(command) => command.as_ref(),
            HudCommand::Close(command) => command,
        }
    }
}

/// The subcommands, in upstream's `Hud.configuration.subcommands` order. `open` and `update` are boxed: their many
/// options would otherwise make every parsed command as large.
#[derive(Debug, clap::Subcommand)]
enum HudCommand {
    Open(Box<Open>),
    Update(Box<Update>),
    Close(Close),
}

// MARK: - checks (upstream's `Hud.validate*`)

/// A `#rrggbb` color option, named as its refusal names it.
fn validate_color(option: &str, color: Option<&str>) -> Result<(), String> {
    match color {
        Some(color) if !is_valid_color_hex(color) => Err(format!("{option} must be a #rrggbb hex value")),
        _ => Ok(()),
    }
}

/// Accepts the `top`/`bottom` aliases exactly as the server does: refusing one here would fail a value the identical
/// raw-socket request takes.
fn validate_position(position: Option<&str>) -> Result<(), String> {
    match position {
        Some(position) if HudPosition::parse(position).is_none() => Err(format!(
            "position must be one of: {}",
            HudPosition::accepted_names_phrase()
        )),
        _ => Ok(()),
    }
}

/// Accepts `none` beside the styles, exactly as the server does: it is what the read-back reports for a static panel,
/// and refusing it here would fail a value `tree` just handed the caller.
fn validate_spinner_style(style: Option<&str>) -> Result<(), String> {
    match style {
        Some(style) if style != HudSpinner::NONE_NAME && HudSpinner::from_wire(style).is_none() => Err(format!(
            "spinner style must be one of: {}",
            HudSpinner::accepted_names_phrase()
        )),
        _ => Ok(()),
    }
}

/// Refuses what cannot be scheduled rather than clamping it, as the server does.
fn validate_hide_after(seconds: Option<f64>) -> Result<(), String> {
    match seconds {
        Some(seconds) if !HudSpec::is_valid_hide_after(seconds) => {
            Err(format!("hide-after must be 0...{:.0} seconds", HudSpec::MAX_HIDE_AFTER))
        }
        _ => Ok(()),
    }
}

/// Exactly one of the MESSAGE argument and `--file`.
fn validate_message_source(message: Option<&str>, file: Option<&str>) -> Result<(), String> {
    match (message, file) {
        (None, None) => Err("provide MESSAGE or --file".to_owned()),
        (Some(_), Some(_)) => Err("MESSAGE and --file are mutually exclusive".to_owned()),
        _ => Ok(()),
    }
}

fn validate_font_size(points: Option<f64>) -> Result<(), String> {
    match points {
        Some(points) if !HudSpec::is_valid_font_size(points) => Err(format!(
            "font-size must be {:.0}...{:.0} points",
            HudSpec::FONT_SIZE_RANGE.start(),
            HudSpec::FONT_SIZE_RANGE.end()
        )),
        _ => Ok(()),
    }
}

/// A panel covers the session or one split pane, never the scratch terminal (upstream's `Overlay.validatePane`, which
/// takes `OverlayPane`'s role and axis aliases).
fn validate_overlay_pane(pane: Option<&str>) -> Result<(), String> {
    match pane.map(StatusPane::from_control_name) {
        Some(None | Some(StatusPane::Scratch)) => Err("--pane must be left or right".to_owned()),
        _ => Ok(()),
    }
}

// MARK: - the request

/// The one spinner value the socket carries, from the two ways to ask for one: `--spinner-style` names it and turns it
/// on by itself, so the bare `--spinner` flag is only needed for the default. `None` when neither is given, the static
/// panel. An explicit `--spinner-style none` is `None` too and beats a bare `--spinner` beside it: naming a value is
/// the more specific instruction, the rule that makes a named style win over the flag's default.
fn spinner_value(spinner: bool, style: Option<&str>) -> Option<String> {
    match style {
        Some(HudSpinner::NONE_NAME) => None,
        Some(style) => Some(style.to_owned()),
        None => spinner.then(|| HudSpinner::DEFAULT_STYLE.wire_name().to_owned()),
    }
}

/// The message a run sends: the argument, or the UTF-8 contents of `file` with CRLF line endings normalized and one
/// trailing newline dropped (files end with one, and a plain panel refuses newlines). The server still applies every
/// cap and refusal. An unreadable or non-UTF-8 file is a usage error, as upstream's `ValidationError` from `run()`.
fn message_text(message: Option<&str>, file: Option<&str>) -> Result<String, Failure> {
    let Some(file) = file else {
        return Ok(message.unwrap_or_default().to_owned());
    };
    let data =
        fs::read(file).map_err(|error| Failure::Usage(format!("cannot read --file {file}: {}", os_message(&error))))?;
    let text = String::from_utf8(data).map_err(|_| Failure::Usage(format!("--file {file} is not valid UTF-8")))?;
    let mut text = text.replace("\r\n", "\n");
    if text.ends_with('\n') {
        text.pop();
    }
    Ok(text)
}

/// `--spinner-style` help on `open`.
fn open_spinner_style_help() -> String {
    format!(
        "Spinner style: {} (default: {}). Implies --spinner; {} leaves the panel static.",
        HudSpinner::accepted_names_phrase(),
        HudSpinner::DEFAULT_STYLE.wire_name(),
        HudSpinner::NONE_NAME
    )
}

/// `--spinner-style` help on `update`.
fn update_spinner_style_help() -> String {
    format!(
        "Switch the spinner to {}; implies --spinner, and repaints the live panel without a re-spawn. {} stops it.",
        HudSpinner::accepted_names_phrase(),
        HudSpinner::NONE_NAME
    )
}

/// `--size-percent` help; `verb` is `Set` on `open`, `Resize` on `update`.
fn size_percent_help(verb: &str) -> String {
    format!(
        "{verb} the panel's WIDTH to PERCENT (1-100) of the pane instead of measuring the message; bounded to \
         {MIN_SIZE_PERCENT}-{MAX_SIZE_PERCENT}, or up to 100 with --sticky off center. Height always follows the \
         message."
    )
}

// MARK: - open

/// `--sticky` and `--no-frame` on `open`, a group of their own only so the command's flags stay within clippy's
/// per-struct count; they print where upstream declares them.
#[derive(Debug, clap::Args)]
struct OpenEdge {
    #[arg(long, help = STICKY_HELP)]
    sticky: bool,

    #[arg(long = "no-frame", help = NO_FRAME_HELP)]
    no_frame: bool,
}

/// `agtermctl session hud [open] <message>`: `--background-color` rides the wire as `color`, as the overlay group's.
#[derive(Debug, clap::Args)]
#[command(about = "Post a message panel over the session; the session keeps focus and stays typable.")]
pub struct Open {
    #[arg(help = "Message shown in the panel (omit with --file).")]
    message: Option<String>,

    #[arg(long, help = "Read the message from FILE instead of the argument.")]
    file: Option<String>,

    #[arg(
        long,
        help = format!(
            "Render the message as markdown, up to {} characters. A single newline inside a paragraph is a soft \
             break; end a line with two spaces or a backslash to break it. A link is underlined and opens on a \
             command-click.",
            HudSpec::MAX_MARKDOWN_LENGTH
        )
    )]
    markdown: bool,

    #[arg(
        long = "font-size",
        help = format!(
            "The panel's own font size in points, {:.0}-{:.0}; omit to use the session's. Fixed for the panel's life.",
            HudSpec::FONT_SIZE_RANGE.start(),
            HudSpec::FONT_SIZE_RANGE.end()
        )
    )]
    font_size: Option<f64>,

    #[arg(
        long,
        help = "Dim second line under the message (e.g. what the caller is waiting on)."
    )]
    detail: Option<String>,

    #[arg(long, help = "Animate a spinner glyph in the panel, in the default style.")]
    spinner: bool,

    #[arg(long, help = open_spinner_style_help())]
    spinner_style: Option<String>,

    // the canonical nine are what `session background` shares; the aliases are this command's own, so naming them in
    // the same breath would send a caller to a --position background refuses
    #[arg(
        long,
        help = format!(
            "Placement in the pane: {} (default: center), the same anchors session background takes. Every anchor \
             off center holds a fixed margin at that edge. Here top and bottom are also accepted, for top-center and \
             bottom-center.",
            HudPosition::valid_names_phrase()
        )
    )]
    position: Option<String>,

    #[arg(
        long,
        help = "Solid background color (#rrggbb) for the panel, independent of the session's own."
    )]
    background_color: Option<String>,

    #[arg(
        long,
        help = "Color (#rrggbb) for the panel's text; omit to keep the terminal foreground."
    )]
    text_color: Option<String>,

    #[arg(long, help = size_percent_help("Set"))]
    size_percent: Option<i64>,

    #[command(flatten)]
    edge: OpenEdge,

    #[arg(
        long,
        help = "Anchor inside primary/left/top or split/right/bottom; omit for the whole session."
    )]
    pane: Option<String>,

    #[arg(
        long = "pane-id",
        help = "Stable pane token ($AGTERM_PANE_ID); overrides --pane when it resolves."
    )]
    pane_id: Option<String>,

    #[arg(
        long = "hide-after",
        help = format!(
            "Take the panel down by itself after SECONDS, 0...{:.0}; omit or 0 to leave it up until something closes \
             it. The clock runs whether or not the session is on screen.",
            HudSpec::MAX_HIDE_AFTER
        )
    )]
    hide_after: Option<f64>,

    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl Open {
    fn request_for(&self, message: String) -> Request {
        let args = Args {
            size_percent: self.size_percent,
            message: Some(message),
            detail: self.detail.clone(),
            spinner: spinner_value(self.spinner, self.spinner_style.as_deref()),
            hide_after: self.hide_after,
            markdown: flag(self.markdown),
            sticky: flag(self.edge.sticky),
            frame: self.edge.no_frame.then_some(false),
            pane: self.pane.clone(),
            pane_id: self.pane_id.clone(),
            color: self.background_color.clone(),
            text_color: self.text_color.clone(),
            position: self.position.clone(),
            font_size: self.font_size,
            ..Args::default()
        };
        session_request(
            SessionCmd::HudOpen,
            Some(self.target.target.clone()),
            Some(args),
            &self.options,
        )
    }
}

impl RequestCommand for Open {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn validate(&self) -> Result<(), String> {
        validate_color("background-color", self.background_color.as_deref())?;
        validate_color("text-color", self.text_color.as_deref())?;
        validate_position(self.position.as_deref())?;
        validate_spinner_style(self.spinner_style.as_deref())?;
        validate_hide_after(self.hide_after)?;
        validate_message_source(self.message.as_deref(), self.file.as_deref())?;
        validate_font_size(self.font_size)?;
        validate_size_percent(self.size_percent)?;
        validate_overlay_pane(self.pane.as_deref())
    }

    /// The request for the MESSAGE argument; a run goes through [`Self::run_request`], which reads `--file`.
    fn make_request(&self) -> Request {
        self.request_for(self.message.clone().unwrap_or_default())
    }

    fn run_request(&self) -> Result<Request, Failure> {
        Ok(self.request_for(message_text(self.message.as_deref(), self.file.as_deref())?))
    }
}

// MARK: - update

/// `--sticky` and `--no-frame` on `update`, grouped as [`OpenEdge`], with update's help.
#[derive(Debug, clap::Args)]
struct UpdateEdge {
    #[arg(long, help = format!("{STICKY_HELP} Omit to return the panel to its edge margin."))]
    sticky: bool,

    #[arg(long = "no-frame", help = format!("{NO_FRAME_HELP} Omit to bring the frame back."))]
    no_frame: bool,
}

/// `agtermctl session hud update <message>`: repaints the live panel in place. An update replaces the whole message, so
/// every option it takes must be repeated to survive, `--spinner`, `--text-color` and pane scope included.
/// `--background-color` and `--font-size` are deliberately absent: the surface reads them once at creation, so only a
/// fresh `open` can change them.
#[derive(Debug, clap::Args)]
#[command(about = "Replace the panel's text in place (no re-spawn, no blink).")]
pub struct Update {
    #[arg(help = "New message; it replaces the old one entirely (omit with --file).")]
    message: Option<String>,

    #[arg(long, help = "Read the new message from FILE instead of the argument.")]
    file: Option<String>,

    #[arg(
        long,
        help = "Render the message as markdown; omit to return the panel to plain text."
    )]
    markdown: bool,

    #[arg(long, help = "Dim second line under the message; omit to drop the old one.")]
    detail: Option<String>,

    #[arg(long, help = "Keep (or start) the spinner in the default style; omit to stop it.")]
    spinner: bool,

    #[arg(long, help = update_spinner_style_help())]
    spinner_style: Option<String>,

    #[arg(long, help = format!("Move the panel to {} (default: center).", HudPosition::accepted_names_phrase()))]
    position: Option<String>,

    #[arg(
        long,
        help = "Recolor the panel's text (#rrggbb); omit to return it to the terminal foreground."
    )]
    text_color: Option<String>,

    #[arg(long, help = size_percent_help("Resize"))]
    size_percent: Option<i64>,

    #[command(flatten)]
    edge: UpdateEdge,

    #[arg(
        long,
        help = "Anchor inside primary/left/top or split/right/bottom; omit to return to whole-session placement."
    )]
    pane: Option<String>,

    #[arg(
        long = "pane-id",
        help = "Stable pane token ($AGTERM_PANE_ID); repeat it on update to keep pane scope."
    )]
    pane_id: Option<String>,

    #[arg(
        long = "hide-after",
        help = format!(
            "Restart the panel's auto-hide at SECONDS, 0...{:.0}; omit or 0 to cancel it, like every other option an \
             update replaces rather than patches.",
            HudSpec::MAX_HIDE_AFTER
        )
    )]
    hide_after: Option<f64>,

    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl Update {
    fn request_for(&self, message: String) -> Request {
        let args = Args {
            size_percent: self.size_percent,
            message: Some(message),
            detail: self.detail.clone(),
            spinner: spinner_value(self.spinner, self.spinner_style.as_deref()),
            hide_after: self.hide_after,
            markdown: flag(self.markdown),
            sticky: flag(self.edge.sticky),
            frame: self.edge.no_frame.then_some(false),
            pane: self.pane.clone(),
            pane_id: self.pane_id.clone(),
            text_color: self.text_color.clone(),
            position: self.position.clone(),
            ..Args::default()
        };
        session_request(
            SessionCmd::HudUpdate,
            Some(self.target.target.clone()),
            Some(args),
            &self.options,
        )
    }
}

impl RequestCommand for Update {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn validate(&self) -> Result<(), String> {
        validate_color("text-color", self.text_color.as_deref())?;
        validate_position(self.position.as_deref())?;
        validate_spinner_style(self.spinner_style.as_deref())?;
        validate_hide_after(self.hide_after)?;
        validate_message_source(self.message.as_deref(), self.file.as_deref())?;
        validate_size_percent(self.size_percent)?;
        validate_overlay_pane(self.pane.as_deref())
    }

    /// The request for the MESSAGE argument; a run goes through [`Self::run_request`], which reads `--file`.
    fn make_request(&self) -> Request {
        self.request_for(self.message.clone().unwrap_or_default())
    }

    fn run_request(&self) -> Result<Request, Failure> {
        Ok(self.request_for(message_text(self.message.as_deref(), self.file.as_deref())?))
    }
}

// MARK: - close

/// `agtermctl session hud close`.
#[derive(Debug, clap::Args)]
#[command(about = "Take the message panel down (a program overlay in the same slot is left alone).")]
pub struct Close {
    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Close {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn make_request(&self) -> Request {
        session_request(
            SessionCmd::HudClose,
            Some(self.target.target.clone()),
            None,
            &self.options,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;

    use clap::{CommandFactory, Parser};
    use serde_json::{Value, json};

    use super::*;
    use crate::usage::name_default_subcommands;
    use crate::{Agtermctl, Command};

    /// `agtermctl session hud <argv>`, the default subcommand named first.
    fn command_line(argv: &[&str]) -> Vec<OsString> {
        let line: Vec<OsString> = ["agtermctl", "session", "hud"]
            .iter()
            .chain(argv)
            .map(OsString::from)
            .collect();
        name_default_subcommands(&Agtermctl::command(), &line)
    }

    /// `agtermctl session hud <argv>` parsed and validated: the request a run sends as JSON, or the refusal.
    fn outcome(argv: &[&str]) -> Result<Value, String> {
        let parsed = Agtermctl::try_parse_from(command_line(argv)).map_err(|error| error.to_string())?;
        let Command::Session(group) = parsed.command else {
            panic!("not a session command")
        };
        let command = group.command();
        command.validate()?;
        match command.run_request() {
            Ok(request) => Ok(serde_json::to_value(request).unwrap()),
            Err(Failure::Usage(message) | Failure::Local(message)) => Err(message),
            Err(Failure::Exit(code)) => Err(format!("{code:?}")),
        }
    }

    fn request(argv: &[&str]) -> Value {
        outcome(argv).unwrap()
    }

    fn args(argv: &[&str]) -> Value {
        request(argv)["args"].clone()
    }

    /// The refusal of `argv`, which parses.
    fn refusal(argv: &[&str]) -> String {
        outcome(argv).unwrap_err()
    }

    /// Whether `argv` fails to parse at all.
    fn does_not_parse(argv: &[&str]) -> bool {
        Agtermctl::try_parse_from(command_line(argv)).is_err()
    }

    /// The help of `session hud <subcommand>`, wrapped to 200 columns.
    fn help(subcommand: &str) -> String {
        let line: Vec<OsString> = ["agtermctl", "session", "hud", subcommand, "--help"]
            .iter()
            .map(OsString::from)
            .collect();
        let error = Agtermctl::try_parse_from(&line).unwrap_err();
        crate::parse_report(&line, &error, 200).text
    }

    #[test]
    fn open_is_the_default_subcommand() {
        assert_eq!(
            request(&["gathering options"]),
            json!({"cmd": "session.hud.open", "target": "active", "args": {"message": "gathering options"}})
        );
        // a message named like a subcommand needs the explicit verb
        assert_eq!(
            request(&["open", "close"]),
            json!({"cmd": "session.hud.open", "target": "active", "args": {"message": "close"}})
        );
    }

    #[test]
    fn open_sends_every_option() {
        // --background-color rides the wire as `color`, as the overlay group's
        assert_eq!(
            request(&[
                "building index",
                "--detail",
                "3 of 12 repos",
                "--spinner",
                "--position",
                "top",
                "--background-color",
                "#2a1a3a",
                "--size-percent",
                "40",
                "--target",
                "9f3c",
                "--window",
                "w1",
            ]),
            json!({"cmd": "session.hud.open", "target": "9f3c", "args": {
                "sizePercent": 40, "message": "building index", "detail": "3 of 12 repos", "spinner": "bar",
                "window": "w1", "color": "#2a1a3a", "position": "top"}})
        );
        assert_eq!(
            args(&[
                "x",
                "--text-color",
                "#7ec07e",
                "--markdown",
                "--font-size",
                "18",
                "--hide-after",
                "5",
                "--pane",
                "right",
                "--pane-id",
                "stable-token",
            ]),
            json!({"message": "x", "textColor": "#7ec07e", "markdown": true, "fontSize": 18.0, "hideAfter": 5.0,
                   "pane": "right", "paneID": "stable-token"})
        );
    }

    #[test]
    fn open_omits_unset_options() {
        assert_eq!(args(&["wait"]), json!({"message": "wait"}));
    }

    #[test]
    fn the_spinner_flag_and_style_resolve_to_one_wire_value() {
        assert_eq!(args(&["working", "--spinner"])["spinner"], "bar");
        for style in HudSpinner::ALL {
            // a named style turns the spinner on without --spinner
            assert_eq!(
                args(&["working", "--spinner-style", style.wire_name()])["spinner"],
                style.wire_name()
            );
        }
        assert_eq!(
            args(&["working", "--spinner", "--spinner-style", "braille"])["spinner"],
            "braille"
        );
        assert_eq!(args(&["working"]).get("spinner"), None);
        // `none` is what the read-back reports for a static panel, and it beats a bare --spinner beside it
        assert_eq!(args(&["working", "--spinner-style", "none"]).get("spinner"), None);
        assert_eq!(
            args(&["working", "--spinner", "--spinner-style", "none"]).get("spinner"),
            None
        );
        assert_eq!(
            args(&["update", "done", "--spinner-style", "none"]).get("spinner"),
            None
        );
        assert_eq!(
            args(&["update", "still working", "--spinner-style", "dot"])["spinner"],
            "dot"
        );
        assert_eq!(
            refusal(&["working", "--spinner-style", "swirl"]),
            "spinner style must be one of: bar, braille, circle, blocks, dot, none"
        );
    }

    #[test]
    fn every_position_and_both_aliases_are_taken() {
        for position in HudPosition::ALL {
            assert_eq!(
                args(&["wait", "--position", position.wire_name()])["position"],
                position.wire_name()
            );
        }
        for alias in ["top", "bottom"] {
            assert_eq!(args(&["wait", "--position", alias])["position"], alias);
            assert_eq!(args(&["update", "wait", "--position", alias])["position"], alias);
        }
        let expected = format!("position must be one of: {}", HudPosition::accepted_names_phrase());
        assert_eq!(refusal(&["wait", "--position", "middle"]), expected);
        assert_eq!(refusal(&["update", "wait", "--position", "middle"]), expected);
    }

    #[test]
    fn sticky_and_no_frame_are_sent_only_when_asked() {
        for verb in [&[][..], &["update"]] {
            let asked = args(&[verb, &["caption", "--sticky", "--no-frame", "--size-percent", "100"]].concat());
            assert_eq!(
                (&asked["sticky"], &asked["frame"], &asked["sizePercent"]),
                (&json!(true), &json!(false), &json!(100))
            );
            let plain = args(&[verb, &["caption"]].concat());
            assert_eq!((plain.get("sticky"), plain.get("frame")), (None, None));
        }
    }

    #[test]
    fn update_carries_pane_scope_text_color_and_markdown() {
        assert_eq!(
            args(&[
                "update",
                "done",
                "--pane",
                "left",
                "--pane-id",
                "stable-token",
                "--text-color",
                "#e0e0e0",
                "--markdown",
            ]),
            json!({"message": "done", "pane": "left", "paneID": "stable-token", "textColor": "#e0e0e0",
                   "markdown": true})
        );
        assert_eq!(args(&["plain"]).get("markdown"), None);
        assert_eq!(args(&["update", "plain"]).get("markdown"), None);
    }

    #[test]
    fn update_sends_its_options_and_close_its_target() {
        assert_eq!(
            request(&[
                "update",
                "ready",
                "--detail",
                "12 repos",
                "--position",
                "bottom",
                "--target",
                "9f3c"
            ]),
            json!({"cmd": "session.hud.update", "target": "9f3c",
                   "args": {"message": "ready", "detail": "12 repos", "position": "bottom"}})
        );
        assert_eq!(
            args(&["update", "still working", "--spinner", "--size-percent", "25"]),
            json!({"sizePercent": 25, "message": "still working", "spinner": "bar"})
        );
        assert_eq!(
            request(&["close"]),
            json!({"cmd": "session.hud.close", "target": "active"})
        );
        assert_eq!(
            request(&["close", "--target", "9f3c", "--window", "w1"]),
            json!({"cmd": "session.hud.close", "target": "9f3c", "args": {"window": "w1"}})
        );
    }

    #[test]
    fn update_takes_no_creation_only_option() {
        assert!(does_not_parse(&["update", "done", "--font-size", "14"]));
        assert!(does_not_parse(&["update", "done", "--background-color", "#112233"]));
    }

    #[test]
    fn bad_values_are_refused_before_sending() {
        let cases: [(&[&str], &str); 13] = [
            (&["wait", "--pane", "scratch"], "--pane must be left or right"),
            (&["wait", "--pane", "middle"], "--pane must be left or right"),
            (&["update", "done", "--pane", "scratch"], "--pane must be left or right"),
            (
                &["wait", "--text-color", "7ec07"],
                "text-color must be a #rrggbb hex value",
            ),
            (
                &["update", "done", "--text-color", "#gggggg"],
                "text-color must be a #rrggbb hex value",
            ),
            (
                &["wait", "--background-color", "purple"],
                "background-color must be a #rrggbb hex value",
            ),
            (
                &["wait", "--size-percent", "0"],
                "--size-percent must be between 1 and 100",
            ),
            (
                &["wait", "--size-percent", "150"],
                "--size-percent must be between 1 and 100",
            ),
            (
                &["update", "wait", "--size-percent", "101"],
                "--size-percent must be between 1 and 100",
            ),
            (&["tiny", "--font-size", "5"], "font-size must be 6...72 points"),
            (&["huge", "--font-size", "73"], "font-size must be 6...72 points"),
            (
                &["wait", "--hide-after", "86401"],
                "hide-after must be 0...86400 seconds",
            ),
            (&["wait", "--hide-after=-1"], "hide-after must be 0...86400 seconds"),
        ];
        for (argv, message) in cases {
            assert_eq!(refusal(argv), message, "{argv:?}");
        }
        // the pane's role and axis aliases are taken
        assert_eq!(args(&["wait", "--pane", "primary"])["pane"], "primary");
    }

    #[test]
    fn exactly_one_message_source_is_taken() {
        assert_eq!(refusal(&["open"]), "provide MESSAGE or --file");
        assert_eq!(refusal(&[]), "provide MESSAGE or --file");
        assert_eq!(refusal(&["update"]), "provide MESSAGE or --file");
        assert_eq!(
            refusal(&["both", "--file", "/tmp/x"]),
            "MESSAGE and --file are mutually exclusive"
        );
        assert_eq!(
            refusal(&["update", "both", "--file", "/tmp/x"]),
            "MESSAGE and --file are mutually exclusive"
        );
    }

    #[test]
    fn the_message_is_read_from_a_file_with_one_terminator_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let cases: [(&[u8], &str); 4] = [
            (b"# Tasks\n\n- build  \n\n", "# Tasks\n\n- build  \n"),
            (b"# Tasks\r\n\r\n- build\r\n", "# Tasks\n\n- build"),
            (b"done\r\n", "done"),
            // a lone carriage return is the server's to refuse
            (b"a\rb\n", "a\rb"),
        ];
        for (at, (contents, message)) in cases.into_iter().enumerate() {
            let path = dir.path().join(format!("{at}.md"));
            fs::write(&path, contents).unwrap();
            let path = path.to_str().unwrap();
            assert_eq!(
                args(&["--file", path, "--markdown"])["message"],
                message,
                "{contents:?}"
            );
            assert_eq!(args(&["update", "--file", path])["message"], message, "{contents:?}");
        }
    }

    #[test]
    fn an_unreadable_or_non_utf8_file_is_refused_before_sending() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.md");
        let missing = missing.to_str().unwrap();
        let refused = refusal(&["--file", missing]);
        assert!(
            refused.starts_with(&format!("cannot read --file {missing}: ")),
            "{refused}"
        );
        let binary = dir.path().join("hud.bin");
        fs::write(&binary, [0xFF, 0xFE, 0x00]).unwrap();
        let binary = binary.to_str().unwrap();
        assert_eq!(
            refusal(&["--file", binary]),
            format!("--file {binary} is not valid UTF-8")
        );
    }

    #[test]
    fn open_help_names_the_markdown_cap_the_soft_break_and_the_fixed_font_size() {
        let help = help("open");
        for phrase in [
            "up to 4096 characters",
            "soft break",
            "Fixed for the panel's life",
            "--file",
        ] {
            assert!(help.contains(phrase), "{phrase}: {help}");
        }
    }

    #[test]
    fn update_help_says_omission_clears_pane_scope() {
        let help = help("update");
        for phrase in [
            "omit to return to whole-session placement",
            "repeat it on update to keep pane scope",
        ] {
            assert!(help.contains(phrase), "{phrase}: {help}");
        }
    }

    #[test]
    fn help_names_the_two_flags_and_the_width_a_sticky_panel_may_take() {
        for help in [help("open"), help("update")] {
            for phrase in [
                "--sticky",
                "--no-frame",
                "flush against the edge or corner",
                "up to 100 with --sticky off center",
            ] {
                assert!(help.contains(phrase), "{phrase}: {help}");
            }
        }
    }
}
