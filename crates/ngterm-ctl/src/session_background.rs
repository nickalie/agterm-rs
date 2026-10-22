//! `ngtermctl session background image|text|color|clear` (`agtermctlKit/SessionCommands.swift` `Session.Background`):
//! a session's background default, or one pane's override with `--pane`. A bad value is a usage error before anything
//! is sent, checked with the server's own rules ([`ngterm_core::watermark`]).

use ngterm_core::model::{WatermarkFit, WatermarkPosition, is_valid_color_hex};
use ngterm_core::watermark::{MAX_TEXT_LENGTH, is_valid_image_path, is_valid_opacity, is_valid_text};
use ngterm_proto::command::SessionCmd;
use ngterm_proto::{Args, Request};

use crate::session::session_request;
use crate::session_io::validate_pane;
use crate::{BasicOptions, ClientOptions, RequestCommand, TargetOptions};

/// Upstream's `Background.paneHelp`.
const PANE_HELP: &str = "Set only this pane's override: left, right, or scratch (primary/top and split/bottom are \
                         aliases). Omitted sets the session default, which every pane without an override inherits; \
                         `clear --pane` returns that pane to the default.";

/// `ngtermctl session background <subcommand>`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Set or clear a session's background (image, rasterized text, or solid color).",
    subcommand_required = true,
    arg_required_else_help = true
)]
pub struct Background {
    #[command(subcommand)]
    command: BackgroundCommand,
}

impl Background {
    /// The subcommand this invocation runs.
    pub fn command(&self) -> &dyn RequestCommand {
        match &self.command {
            BackgroundCommand::Image(command) => command,
            BackgroundCommand::Text(command) => command,
            BackgroundCommand::Color(command) => command,
            BackgroundCommand::Clear(command) => command,
        }
    }
}

#[derive(Debug, clap::Subcommand)]
enum BackgroundCommand {
    Image(ImageBackground),
    Text(TextBackground),
    Color(ColorBackground),
    Clear(ClearBackground),
}

/// The values a background command may carry, each checked when present (upstream's `Background.validate`).
#[derive(Debug, Default)]
struct Values<'a> {
    fit: Option<&'a str>,
    position: Option<&'a str>,
    opacity: Option<f64>,
    color: Option<&'a str>,
    text: Option<&'a str>,
    path: Option<&'a str>,
}

impl Values<'_> {
    fn validate(&self) -> Result<(), String> {
        let message = if self.fit.is_some_and(|fit| WatermarkFit::from_wire(fit).is_none()) {
            format!(
                "fit must be one of: {}",
                WatermarkFit::ALL.map(WatermarkFit::wire_name).join(", ")
            )
        } else if self
            .position
            .is_some_and(|position| WatermarkPosition::from_wire(position).is_none())
        {
            format!(
                "position must be one of: {}",
                WatermarkPosition::ALL.map(WatermarkPosition::wire_name).join(", ")
            )
        } else if self.opacity.is_some_and(|opacity| !is_valid_opacity(opacity)) {
            "opacity must be between 0.0 and 1.0".to_owned()
        } else if self.color.is_some_and(|color| !is_valid_color_hex(color)) {
            "color must be a #rrggbb hex value".to_owned()
        } else if self.text.is_some_and(|text| !is_valid_text(text)) {
            format!("text must be 1–{MAX_TEXT_LENGTH} characters")
        } else if self.path.is_some_and(|path| !is_valid_image_path(path)) {
            "image path must not contain control characters".to_owned()
        } else {
            return Ok(());
        };
        Err(message)
    }
}

/// A `session.background` request with `args` on the command's target.
fn background_request(args: Args, target: &TargetOptions, options: &ClientOptions) -> Request {
    session_request(SessionCmd::Background, Some(target.target.clone()), Some(args), options)
}

/// `ngtermctl session background image PATH`.
#[derive(Debug, clap::Args)]
#[command(about = "Show a PNG or JPEG image behind the terminal (auto-fits the window).")]
pub struct ImageBackground {
    #[arg(help = "Path to a PNG or JPEG image file.")]
    path: String,

    #[arg(long, help = "Image opacity 0.0-1.0 (default 1.0).")]
    opacity: Option<f64>,

    #[arg(long, help = "Fit: contain (default), cover, stretch, or none.")]
    fit: Option<String>,

    #[arg(
        long,
        help = "Position: center (default) or an edge/corner anchor (top-left, bottom-right, …)."
    )]
    position: Option<String>,

    #[arg(long = "repeat", help = "Tile the image to fill blank space.")]
    repeat_image: bool,

    #[arg(long, help = PANE_HELP)]
    pane: Option<String>,

    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for ImageBackground {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn validate(&self) -> Result<(), String> {
        Values {
            fit: self.fit.as_deref(),
            position: self.position.as_deref(),
            opacity: self.opacity,
            path: Some(&self.path),
            ..Values::default()
        }
        .validate()?;
        validate_pane(self.pane.as_deref())
    }

    fn make_request(&self) -> Request {
        let args = Args {
            mode: Some("image".to_owned()),
            path: Some(self.path.clone()),
            opacity: self.opacity,
            fit: self.fit.clone(),
            position: self.position.clone(),
            repeats: self.repeat_image.then_some(true),
            pane: self.pane.clone(),
            ..Args::default()
        };
        background_request(args, &self.target, &self.options)
    }
}

/// `ngtermctl session background text TEXT`.
#[derive(Debug, clap::Args)]
#[command(about = "Render TEXT as a watermark behind the terminal (auto-fits the window).")]
pub struct TextBackground {
    #[arg(help = "Watermark text.")]
    text: String,

    #[arg(long, help = "Text color as #rrggbb (default: the terminal foreground color).")]
    color: Option<String>,

    #[arg(long, help = "Opacity 0.0-1.0 (default 1.0).")]
    opacity: Option<f64>,

    #[arg(long, help = "Fit: contain (default), cover, stretch, or none.")]
    fit: Option<String>,

    #[arg(
        long,
        help = "Position: center (default) or an edge/corner anchor (top-left, bottom-right, …)."
    )]
    position: Option<String>,

    #[arg(long, help = PANE_HELP)]
    pane: Option<String>,

    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for TextBackground {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn validate(&self) -> Result<(), String> {
        Values {
            fit: self.fit.as_deref(),
            position: self.position.as_deref(),
            opacity: self.opacity,
            color: self.color.as_deref(),
            text: Some(&self.text),
            ..Values::default()
        }
        .validate()?;
        validate_pane(self.pane.as_deref())
    }

    fn make_request(&self) -> Request {
        let args = Args {
            text: Some(self.text.clone()),
            mode: Some("text".to_owned()),
            color: self.color.clone(),
            opacity: self.opacity,
            fit: self.fit.clone(),
            position: self.position.clone(),
            pane: self.pane.clone(),
            ..Args::default()
        };
        background_request(args, &self.target, &self.options)
    }
}

/// `ngtermctl session background color #RRGGBB`.
#[derive(Debug, clap::Args)]
#[command(about = "Set a solid background color for the terminal (honors the Settings window translucency).")]
pub struct ColorBackground {
    #[arg(help = "Background color as #rrggbb.")]
    color: String,

    #[arg(long, help = PANE_HELP)]
    pane: Option<String>,

    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for ColorBackground {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn validate(&self) -> Result<(), String> {
        Values {
            color: Some(&self.color),
            ..Values::default()
        }
        .validate()?;
        validate_pane(self.pane.as_deref())
    }

    fn make_request(&self) -> Request {
        let args = Args {
            mode: Some("color".to_owned()),
            color: Some(self.color.clone()),
            pane: self.pane.clone(),
            ..Args::default()
        };
        background_request(args, &self.target, &self.options)
    }
}

/// `ngtermctl session background clear`.
#[derive(Debug, clap::Args)]
#[command(about = "Remove the session's background (watermark or solid color).")]
pub struct ClearBackground {
    #[arg(long, help = PANE_HELP)]
    pane: Option<String>,

    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for ClearBackground {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn validate(&self) -> Result<(), String> {
        validate_pane(self.pane.as_deref())
    }

    fn make_request(&self) -> Request {
        let args = Args {
            mode: Some("clear".to_owned()),
            pane: self.pane.clone(),
            ..Args::default()
        };
        background_request(args, &self.target, &self.options)
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use serde_json::{Value, json};

    use crate::{Command, Ngtermctl};

    fn parse(words: &[&str]) -> (Result<(), String>, Value) {
        let line = ["ngtermctl", "session", "background"].iter().chain(words).copied();
        let Command::Session(group) = Ngtermctl::try_parse_from(line).unwrap().command else {
            panic!("not a session command")
        };
        let command = group.command();
        (
            command.validate(),
            serde_json::to_value(command.make_request()).unwrap(),
        )
    }

    #[test]
    fn each_mode_sends_its_values() {
        let (valid, request) = parse(&[
            "image",
            "/a.png",
            "--opacity",
            "0.5",
            "--fit",
            "cover",
            "--position",
            "top-left",
            "--repeat",
            "--pane",
            "right",
        ]);
        assert_eq!(valid, Ok(()));
        assert_eq!(
            request["args"],
            json!({"mode": "image", "path": "/a.png", "opacity": 0.5, "fit": "cover", "position": "top-left",
                   "repeats": true, "pane": "right"})
        );
        let (_, request) = parse(&["text", "PROD", "--color", "#336699"]);
        assert_eq!(
            request["args"],
            json!({"text": "PROD", "mode": "text", "color": "#336699"})
        );
        let (_, request) = parse(&["color", "#102030", "--target", "ab"]);
        assert_eq!(
            request,
            json!({"cmd": "session.background", "target": "ab",
                                   "args": {"mode": "color", "color": "#102030"}})
        );
        let (_, request) = parse(&["clear"]);
        assert_eq!(request["args"], json!({"mode": "clear"}));
    }

    #[test]
    fn a_bad_value_is_a_usage_error() {
        let cases: [(&[&str], &str); 7] = [
            (&["color", "blue"], "color must be a #rrggbb hex value"),
            (
                &["image", "/a.png", "--fit", "fill"],
                "fit must be one of: contain, cover, stretch, none",
            ),
            (
                &["image", "/a.png", "--position", "middle"],
                "position must be one of: top-left, top-center, top-right, center-left, center, center-right, \
                 bottom-left, bottom-center, bottom-right",
            ),
            (
                &["text", "x", "--opacity", "1.5"],
                "opacity must be between 0.0 and 1.0",
            ),
            (&["text", ""], "text must be 1–256 characters"),
            (&["image", "a\nb.png"], "image path must not contain control characters"),
            (&["clear", "--pane", "middle"], "--pane must be left, right, or scratch"),
        ];
        for (words, error) in cases {
            assert_eq!(parse(words).0, Err(error.to_owned()), "{words:?}");
        }
    }
}
