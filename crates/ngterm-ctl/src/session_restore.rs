//! The `session` subcommands that decide what a pane's shell runs: `restore` (`agtermctlKit/SessionMetadataCommands.swift`
//! `Session.Restore`), which pins the line the next launch re-runs, and `restart` (`Session+Restart.swift`), which
//! replaces a live pane's shell now. Each takes its own options first, then `--target`, then [`ClientOptions`].

use ngterm_proto::command::SessionCmd;
use ngterm_proto::{Args, Request};

use crate::session::session_request;
use crate::session_io::validate_pane;
use crate::{BasicOptions, ClientOptions, RequestCommand, TargetOptions};

/// `ngtermctl session restore COMMAND|--none|--clear`: exactly one form; the line goes as typed.
#[derive(Debug, clap::Args)]
#[command(
    about = "Pin the command a session's pane re-runs on the next launch.",
    long_about = "session restore \"claude --resume ID\"   pin a shell line for the next launch\n\
                  session restore --none                 pin to nothing (the pane restores a plain shell)\n\
                  session restore --clear                drop the override, back to auto-capture\n\
                  \n\
                  The override is written now and consumed on the NEXT launch — it never touches the running session. \
                  It wins over the pane's captured foreground command in rerun mode and reads back on `tree` as \
                  restoreCommand (main pane) or splitRestoreCommand (split pane). It is STICKY: it fires again on \
                  every launch until cleared.\n\
                  \n\
                  Set and --none still save policy while this launch is in fresh-shell or live mode; the response \
                  names the active mode and says the policy is saved for rerun mode. --clear works in every mode. A \
                  pin never opts one session out of live mode.\n\
                  \n\
                  COMMAND is shell code, stored verbatim in the window's state file and readable via `tree`, so it \
                  must not carry secrets.\n\
                  \n\
                  Not to be confused with `ngtermctl restore clear`, which is app-global and clears every session's \
                  CAPTURED foreground command; this one is per-session and clears only the override."
)]
pub struct Restore {
    #[arg(help = "Shell line to run on the next launch (omit with --none or --clear).")]
    command: Option<String>,

    #[arg(
        long,
        help = "Pin the pane to nothing: it restores a plain shell, suppressing the captured command."
    )]
    none: bool,

    #[arg(
        long,
        help = "Drop the override so the pane goes back to restoring its captured foreground command."
    )]
    clear: bool,

    #[arg(
        long,
        help = "Which pane to pin: primary/left/top or split/right/bottom; scratch is rejected. Defaults to primary."
    )]
    pane: Option<String>,

    #[arg(
        long = "pane-id",
        help = "A surface's stable token (the shell's $NGTERM_PANE_ID) — resolves to the pane's CURRENT slot, so a \
                hook in a promoted-then-re-split pane still pins the right one. Unlike `session status`, a token that \
                does not resolve is an error unless --pane is also given as the fallback."
    )]
    pane_id: Option<String>,

    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Restore {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn validate(&self) -> Result<(), String> {
        let forms = [self.command.is_some(), self.none, self.clear];
        if forms.iter().filter(|form| **form).count() != 1 {
            return Err("provide exactly one of a COMMAND, --none, or --clear".to_owned());
        }
        validate_pane(self.pane.as_deref())
    }

    fn make_request(&self) -> Request {
        let mode = if self.none {
            "none"
        } else if self.clear {
            "clear"
        } else {
            "set"
        };
        let args = Args {
            mode: Some(mode.to_owned()),
            command: self.command.clone(),
            pane: self.pane.clone(),
            pane_id: self.pane_id.clone(),
            ..Args::default()
        };
        session_request(
            SessionCmd::Restore,
            Some(self.target.target.clone()),
            Some(args),
            &self.options,
        )
    }
}

/// `ngtermctl session restart --pane-id TOKEN|--pane P [--command LINE]`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Replace the shell of one live pane and run a shell line, or the same program again, in the new one.",
    long_about = "session restart --target ID --pane-id TOKEN --command 'cld brief.md'\n\
                  session restart --target ID --pane-id TOKEN\n\
                  \n\
                  Ends the pane's shell and its foreground program, then starts a new login shell in the same pane \
                  that runs COMMAND and stays as an interactive shell afterwards. The pane keeps its place, its stable \
                  id and its NGTERM_* environment, and starts with an empty screen. Nothing is typed into the pane.\n\
                  \n\
                  Without --command the new shell runs the pane's current foreground program again, with the \
                  arguments `tree` reports as foreground or splitForeground, in the directory that program is running \
                  in. That is the program as it runs now, not the line that started it: environment assignments, \
                  redirections and the other parts of a pipeline are not reconstructed. It is refused, with nothing \
                  changed, when a shell holds the pane (the pane's own shell running a builtin or a loop included), \
                  when the program cannot be read (sudo, top), when its directory is unavailable, or when it is listed \
                  in restore-denylist.conf. The reply carries the argv the new shell was asked to run as \
                  restart.replayedArgv.\n\
                  \n\
                  The reply comes once a new shell exists in the pane, and carries the ended shell's pid and the new \
                  one's as restart.oldPid and restart.newPid. They are the pane's shells, not the program COMMAND \
                  starts; read that from `tree` as foreground or splitForeground. An error names the step that did \
                  not happen.\n\
                  \n\
                  Works only in Live sessions mode, for a local pane. A hidden split and a pane in a background window \
                  are restarted like any other. A background or disowned job of the old shell is not ended.\n\
                  \n\
                  COMMAND is one shell line of at most 4096 bytes."
)]
pub struct Restart {
    #[arg(
        long,
        help = "Shell line the new shell runs. Omit to run the pane's foreground program again."
    )]
    command: Option<String>,

    #[arg(long, help = "Which pane to restart: primary/left/top or split/right/bottom.")]
    pane: Option<String>,

    #[arg(
        long = "pane-id",
        help = "The pane's stable token (the shell's $NGTERM_PANE_ID). It resolves to the pane's current slot and \
                wins over --pane; a token that does not resolve is an error, with or without --pane."
    )]
    pane_id: Option<String>,

    #[command(flatten)]
    target: TargetOptions,

    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Restart {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn validate(&self) -> Result<(), String> {
        if self.pane.is_none() && self.pane_id.as_deref().is_none_or(str::is_empty) {
            return Err("provide --pane-id or --pane".to_owned());
        }
        validate_pane(self.pane.as_deref())
    }

    fn make_request(&self) -> Request {
        let args = Args {
            command: self.command.clone(),
            pane: self.pane.clone(),
            pane_id: self.pane_id.clone(),
            ..Args::default()
        };
        session_request(
            SessionCmd::Restart,
            Some(self.target.target.clone()),
            Some(args),
            &self.options,
        )
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use serde_json::{Value, json};

    use crate::{Command, Ngtermctl};

    /// The parsed `ngtermctl session ...` invocation's validation and request.
    fn parse(words: &[&str]) -> (Result<(), String>, Value) {
        let line = ["ngtermctl", "session"].iter().chain(words).copied();
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
    fn restore_takes_exactly_one_form() {
        let (valid, request) = parse(&["restore", "echo pinned", "--pane", "split", "--pane-id", "T"]);
        assert_eq!(valid, Ok(()));
        assert_eq!(
            request,
            json!({"cmd": "session.restore", "target": "active", "args": {
                "mode": "set", "pane": "split", "paneID": "T", "command": "echo pinned"}})
        );
        let (_, request) = parse(&["restore", "--clear", "--target", "ab"]);
        assert_eq!(request["args"], json!({"mode": "clear"}));
        assert_eq!(request["target"], json!("ab"));
        for words in [
            &["restore"][..],
            &["restore", "--none", "--clear"],
            &["restore", "x", "--none"],
        ] {
            assert_eq!(
                parse(words).0,
                Err("provide exactly one of a COMMAND, --none, or --clear".to_owned()),
                "{words:?}"
            );
        }
        assert_eq!(
            parse(&["restore", "--none", "--pane", "middle"]).0,
            Err("--pane must be left, right, or scratch".to_owned())
        );
    }

    #[test]
    fn restart_needs_a_pane_or_a_token() {
        let (valid, request) = parse(&["restart", "--pane", "left", "--command", "cat"]);
        assert_eq!(valid, Ok(()));
        assert_eq!(
            request,
            json!({"cmd": "session.restart", "target": "active", "args": {"pane": "left", "command": "cat"}})
        );
        assert_eq!(parse(&["restart", "--pane-id", "T"]).0, Ok(()));
        for words in [&["restart"][..], &["restart", "--pane-id", ""]] {
            assert_eq!(
                parse(words).0,
                Err("provide --pane-id or --pane".to_owned()),
                "{words:?}"
            );
        }
        assert_eq!(
            parse(&["restart", "--pane", "middle"]).0,
            Err("--pane must be left, right, or scratch".to_owned())
        );
    }

    #[test]
    fn restart_carries_the_line_the_pane_and_the_token() {
        let (valid, request) = parse(&[
            "restart",
            "--target",
            "s1",
            "--pane",
            "right",
            "--pane-id",
            "tok",
            "--command",
            "cld 'brief one'",
            "--window",
            "w1",
        ]);
        assert_eq!(valid, Ok(()));
        assert_eq!(
            request,
            json!({"cmd": "session.restart", "target": "s1", "args": {
                "command": "cld 'brief one'", "window": "w1", "pane": "right", "paneID": "tok"}})
        );
    }

    #[test]
    fn restart_omits_the_line_to_ask_for_a_replay() {
        let (valid, request) = parse(&["restart", "--target", "s1", "--pane-id", "tok"]);
        assert_eq!(valid, Ok(()));
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            r#"{"cmd":"session.restart","target":"s1","args":{"paneID":"tok"}}"#
        );
    }

    #[test]
    fn restart_sends_an_empty_line_for_the_server_to_refuse() {
        let (valid, request) = parse(&["restart", "--pane-id", "tok", "--command", ""]);
        assert_eq!(valid, Ok(()));
        assert_eq!(
            request,
            json!({"cmd": "session.restart", "target": "active", "args": {"command": "", "paneID": "tok"}})
        );
    }

    #[test]
    fn restart_requires_a_valid_pane_selector() {
        for (words, error) in [
            (&["restart", "--command", "cld"][..], "provide --pane-id or --pane"),
            (&["restart"], "provide --pane-id or --pane"),
            (
                &["restart", "--command", "cld", "--pane", "middle"],
                "--pane must be left, right, or scratch",
            ),
        ] {
            assert_eq!(parse(words).0, Err(error.to_owned()), "{words:?}");
        }
    }
}
