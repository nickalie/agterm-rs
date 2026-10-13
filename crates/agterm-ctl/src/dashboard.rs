//! `agtermctl dashboard` (upstream's `agtermctlKit/MiscCommands.swift` `Dashboard`): opens a window's view-only grid of
//! pane cells, or closes it. Flag combinations are refused before connecting; the server checks them again, and the
//! pane grammar of the ids is the server's alone.

use agterm_core::paths::Env;
use agterm_proto::{Args, Command, Request};

use crate::{BasicOptions, ClientOptions, Failure, RequestCommand};

/// `agtermctl dashboard [<ids> ...]`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Open a view-only grid of live sessions, or --close the open one.",
    long_about = "dashboard S1 S2 S3                 open a grid of the named sessions (ids or unique prefixes, max \
                  9)\ndashboard S1:left                  place only the main pane of a split session\ndashboard \
                  S1:left S2:right         mix panes across sessions; a bare id still takes all of its \
                  panes\ndashboard S1 S2 --font-size 12     open with an absolute cell font size \
                  (points)\ndashboard S1 S2 --auto-size        open sizing cells relative to the Settings default \
                  font\ndashboard --mru                    open a grid of the window's most-recently-used sessions \
                  (up to 9)\ndashboard --mru --auto-size        the same, sizing cells relative to the Settings \
                  default font\ndashboard S1 --window W            open in a specific window (defaults to the \
                  frontmost)\ndashboard --close                  close the open dashboard\n\nThe 9-cell cap counts \
                  PANES, so a split session normally takes two of them. A pane suffix keeps the pane you want and \
                  frees the other cell; `:right` on a session with no split is reported as unresolved. The suffix is \
                  the same form `tree --json` reports in `dashboardMembers`."
)]
pub struct Dashboard {
    #[arg(
        help = "Session ids (or unique prefixes) to show, max 9. Each may carry a :left/:right pane suffix; a bare id \
                takes every pane of the session. Omit only with --mru or --close."
    )]
    ids: Vec<String>,

    #[arg(
        long = "font-size",
        help = "Absolute cell font size in points (mutually exclusive with --auto-size)."
    )]
    font_size: Option<f64>,

    #[arg(
        long,
        help = "Size cells relative to the Settings default font, shrinking as the grid grows."
    )]
    auto_size: bool,

    #[arg(
        long,
        help = "Populate the grid from the window's most-recently-used sessions (up to 9)."
    )]
    mru: bool,

    #[arg(long, help = "Close the open dashboard (takes no ids, --mru, or font options).")]
    close: bool,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Dashboard {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn make_request(&self) -> Request {
        let args = Args {
            targets: (!self.ids.is_empty()).then(|| self.ids.clone()),
            close: self.close.then_some(true),
            font_size: self.font_size,
            auto_size: self.auto_size.then_some(true),
            mru: self.mru.then_some(true),
            ..Args::default()
        };
        Request {
            cmd: Command::Dashboard,
            target: None,
            args: self.options.with_window(Some(args)),
        }
    }

    /// Upstream's `validate()`: the flag combinations, before anything is sent.
    fn validate(&self) -> Result<(), String> {
        if self.close {
            if !self.ids.is_empty() || self.mru || self.font_size.is_some() || self.auto_size {
                return Err("--close takes no ids, --mru, or font options".to_owned());
            }
            return Ok(());
        }
        if self.mru && !self.ids.is_empty() {
            return Err("--mru cannot be combined with session ids".to_owned());
        }
        if self.ids.is_empty() && !self.mru {
            return Err("dashboard requires at least one session id (or --mru, or --close)".to_owned());
        }
        if self.font_size.is_some() && self.auto_size {
            return Err("--font-size is mutually exclusive with --auto-size".to_owned());
        }
        // nan and inf parse as numbers but are no size
        if self.font_size.is_some_and(|size| !size.is_finite() || size <= 0.0) {
            return Err("--font-size must be a positive number".to_owned());
        }
        Ok(())
    }
}

impl Dashboard {
    /// What the parser cannot check, as a usage error's message.
    pub fn check(&self) -> Result<(), String> {
        RequestCommand::validate(self)
    }

    /// Runs the command.
    pub fn execute(&self, env: &dyn Env) -> Result<(), Failure> {
        self.default_run(env)
    }
}
