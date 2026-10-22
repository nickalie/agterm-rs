//! `ngtermctl notify <body>` (`agtermctlKit/MiscCommands.swift` `Notify`): a desktop notification on a session. The
//! reply prints as any other: `ok`, or the server's `result.text` note when banners are off, so a run that only raised
//! the unseen badge does not look like one that posted a banner.

use ngterm_proto::{Args, Command, Request};

use crate::{BasicOptions, ClientOptions, RequestCommand, TargetOptions};

/// `ngtermctl notify <body> [--title T]`.
#[derive(Debug, clap::Args)]
#[command(about = "Post a desktop notification (default: the active session of the frontmost window).")]
pub struct Notify {
    #[arg(help = "Notification body.")]
    body: String,

    #[arg(long, help = "Notification title (defaults to the session name).")]
    title: Option<String>,

    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Notify {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn make_request(&self) -> Request {
        let args = Args {
            title: self.title.clone(),
            body: Some(self.body.clone()),
            ..Args::default()
        };
        Request {
            cmd: Command::Notify,
            target: Some(self.target.target.clone()),
            args: self.options.with_window(Some(args)),
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use serde_json::{Value, json};

    use crate::{Command, Ngtermctl};

    fn request(words: &[&str]) -> Value {
        let line = ["ngtermctl", "notify"].iter().chain(words).copied();
        let Command::Notify(command) = Ngtermctl::try_parse_from(line).unwrap().command else {
            panic!("not notify")
        };
        serde_json::to_value(crate::RequestCommand::make_request(&command)).unwrap()
    }

    #[test]
    fn defaults_to_the_active_session_without_a_title() {
        assert_eq!(
            request(&["hi"]),
            json!({"cmd": "notify", "target": "active", "args": {"body": "hi"}})
        );
    }

    #[test]
    fn carries_the_title_target_and_window() {
        assert_eq!(
            request(&["done", "--title", "Build", "--target", "build", "--window", "w1"]),
            json!({"cmd": "notify", "target": "build", "args": {"title": "Build", "body": "done", "window": "w1"}})
        );
    }

    #[test]
    fn a_missing_body_does_not_parse() {
        assert!(Ngtermctl::try_parse_from(["ngtermctl", "notify", "--target", "s1"]).is_err());
    }
}
