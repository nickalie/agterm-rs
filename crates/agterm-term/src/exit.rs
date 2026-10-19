//! What a pane does when its program ends: libghostty's `Surface.childExited` as the GTK build drove it through
//! `SHOW_CHILD_EXITED` (`agterm-gtk/src/surface.rs` `child_exited`).
//!
//! - A command that should not be waited for closes its pane, failed or not (the GTK build suppressed libghostty's
//!   message for it).
//! - An abnormal exit — nonzero within [`ExitPolicy::abnormal_runtime`] (`abnormal-command-exit-runtime`, 250 ms) —
//!   holds the pane on libghostty's failure report, a plain shell's included.
//! - A command run with `wait_after_command` holds the pane on "Process exited. Press any key to close the terminal."
//! - Anything else closes.
//!
//! A held pane's terminal shows the message with the cursor hidden and normal key encoding back on (keyboard action
//! mode off, Kitty keyboard flags cleared), so any key that encodes can close it, as libghostty closes it.

use std::fmt::Write as _;
use std::time::Duration;

use agterm_ghostty_vt as vt;

/// libghostty's default `abnormal-command-exit-runtime`.
pub const ABNORMAL_EXIT_RUNTIME: Duration = Duration::from_millis(250);

/// How a pane's program ending is handled.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ExitPolicy {
    /// Only report the exit (a pane spawned from a bare [`crate::PaneCommand`]).
    #[default]
    Report,
    /// A launched pane ([`crate::Launch`]), handled as libghostty and the GTK build handled a surface's child.
    Launched {
        /// The program line the failure report names (`/bin/sh -c <command>`).
        argv: String,
        /// The pane runs a command, not the login shell.
        command: bool,
        wait_after_command: bool,
        /// An exit this soon with a nonzero status counts as a failure to launch.
        abnormal_runtime: Duration,
    },
}

/// What the end of a pane's program leaves on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Nothing: the frontend closes the pane.
    Close,
    /// The pane stays on "Process exited" until a key.
    Prompt,
    /// The pane stays on libghostty's failure report until a key.
    Failed,
}

impl Outcome {
    pub(crate) fn held(self) -> bool {
        self != Self::Close
    }
}

impl ExitPolicy {
    pub(crate) fn outcome(&self, code: u32, runtime: Duration) -> Outcome {
        let Self::Launched {
            command,
            wait_after_command,
            abnormal_runtime,
            ..
        } = self
        else {
            return Outcome::Close;
        };
        if *command && !*wait_after_command {
            Outcome::Close
        } else if runtime <= *abnormal_runtime && code != 0 {
            Outcome::Failed
        } else if *wait_after_command {
            Outcome::Prompt
        } else {
            Outcome::Close
        }
    }

    /// The bytes `outcome` writes into the terminal: `viewport_used` (the screen shows text) puts a rule above a
    /// failure report, `cols` is its width. libghostty wrote the message on the terminal directly; written through the
    /// parser it starts with CAN, which ends whatever sequence the program left unfinished (a CSI would otherwise take
    /// the message's first letter as its final byte, a string would swallow the message).
    pub(crate) fn message(&self, outcome: Outcome, code: u32, runtime: Duration, layout: Layout) -> Vec<u8> {
        let mut out = String::from("\x18");
        match outcome {
            Outcome::Close => return Vec::new(),
            Outcome::Prompt => out.push_str("\r\nProcess exited. Press any key to close the terminal."),
            Outcome::Failed => {
                let argv = match self {
                    Self::Launched { argv, .. } => argv.as_str(),
                    Self::Report => "",
                };
                out.push_str("\r\x1b[0m");
                if layout.viewport_used {
                    out.push('\n');
                    out.push_str(&"\u{2501}".repeat(usize::from(layout.cols)));
                    out.push_str("\r\n\n");
                }
                out.push_str("\x1b[91m\x1b[1mGhostty failed to launch the requested command:\x1b[0m\r\n\n");
                out.push_str(argv);
                out.push_str("\x1b[0m\r\n\nRuntime: \x1b[31m");
                let _ = write!(
                    out,
                    "{} ms\x1b[0m\r\nExit Code: \x1b[31m{code}\x1b[0m\r\n\nPress any key to close the window.",
                    runtime.as_millis()
                );
            }
        }
        // the cursor hidden, keyboard action mode off, Kitty keyboard flags cleared
        out.push_str("\x1b[?25l\x1b[2l\x1b[=0;1u");
        out.into_bytes()
    }
}

/// The screen a failure report goes on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Layout {
    pub viewport_used: bool,
    pub cols: u16,
}

impl Layout {
    /// The layout of `terminal`'s screen now.
    pub(crate) fn of(terminal: &mut vt::Terminal) -> Self {
        let options = vt::FormatOptions {
            format: vt::Format::Plain,
            unwrap: false,
            trim: true,
            extras: vt::FormatExtras::empty(),
        };
        let viewport_used = terminal
            .format(vt::Region::Viewport, options)
            .ok()
            .flatten()
            .is_some_and(|text| text.iter().any(|byte| !byte.is_ascii_whitespace()));
        Self {
            viewport_used,
            cols: terminal.cols(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launched(command: bool, wait: bool) -> ExitPolicy {
        ExitPolicy::Launched {
            argv: "/bin/sh -c nope".to_owned(),
            command,
            wait_after_command: wait,
            abnormal_runtime: ABNORMAL_EXIT_RUNTIME,
        }
    }

    #[test]
    fn the_outcome_follows_libghostty_and_the_gtk_build() {
        let soon = Duration::from_millis(10);
        let later = Duration::from_secs(3);
        // a bare pane only reports
        assert_eq!(ExitPolicy::Report.outcome(127, soon), Outcome::Close);
        // a command not waited for closes, failed or not
        assert_eq!(launched(true, false).outcome(127, soon), Outcome::Close);
        assert_eq!(launched(true, false).outcome(0, later), Outcome::Close);
        // a waited command holds on the prompt, or on the report when it failed to launch
        assert_eq!(launched(true, true).outcome(0, later), Outcome::Prompt);
        assert_eq!(launched(true, true).outcome(3, later), Outcome::Prompt);
        assert_eq!(launched(true, true).outcome(0, soon), Outcome::Prompt);
        assert_eq!(launched(true, true).outcome(127, soon), Outcome::Failed);
        // a login shell closes, unless it failed right away
        assert_eq!(launched(false, false).outcome(0, later), Outcome::Close);
        assert_eq!(launched(false, false).outcome(1, later), Outcome::Close);
        assert_eq!(launched(false, false).outcome(1, soon), Outcome::Failed);
        assert_eq!(
            launched(false, false).outcome(1, ABNORMAL_EXIT_RUNTIME),
            Outcome::Failed
        );
        assert!(!Outcome::Close.held() && Outcome::Prompt.held() && Outcome::Failed.held());
    }

    #[test]
    fn the_messages_are_libghosttys() {
        let policy = launched(true, true);
        let layout = Layout {
            viewport_used: false,
            cols: 4,
        };
        let prompt = String::from_utf8(policy.message(Outcome::Prompt, 0, Duration::ZERO, layout)).unwrap();
        assert_eq!(
            prompt,
            "\x18\r\nProcess exited. Press any key to close the terminal.\x1b[?25l\x1b[2l\x1b[=0;1u"
        );
        let failed =
            String::from_utf8(policy.message(Outcome::Failed, 127, Duration::from_millis(12), layout)).unwrap();
        assert!(failed.starts_with("\x18\r\x1b[0m\x1b[91m\x1b[1mGhostty failed to launch the requested command:"));
        assert!(
            failed.contains("/bin/sh -c nope") && failed.contains("12 ms") && failed.contains("Exit Code: \x1b[31m127")
        );
        assert!(!failed.contains('\u{2501}'));
        let ruled = Layout {
            viewport_used: true,
            cols: 4,
        };
        let failed = String::from_utf8(policy.message(Outcome::Failed, 1, Duration::ZERO, ruled)).unwrap();
        assert!(failed.contains("\n\u{2501}\u{2501}\u{2501}\u{2501}\r\n\n"));
        assert!(policy.message(Outcome::Close, 0, Duration::ZERO, layout).is_empty());
    }
}
