//! What a `session.restart` without a command runs (upstream's `RestartReplay.swift`): the pane's foreground program
//! again, in the directory it runs in. Every refusal is decided before anything is ended. Pure, so the decisions are
//! tested off the process table; the frontend reads the observation when live sessions arrive.

use crate::command_restore::{PaneForeground, basename, is_known_shell, shell_quoted_line, should_restore};

/// The longest shell line `session.restart` takes, in UTF-8 bytes (upstream's
/// `ControlSessionRestartOptions.maxCommandBytes`): it travels in the new shell's argv, inside the session host's
/// creation frame beside the pane's environment.
pub const MAX_COMMAND_BYTES: usize = 4096;

/// One read of the process holding the pane's foreground.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    /// `None` when the process could not be read.
    pub foreground: Option<PaneForeground>,
    /// Whether it is the daemon's root process, a program it exec'd included.
    pub is_daemon_leader: bool,
    /// `None` when unreadable or not a directory.
    pub working_directory: Option<String>,
}

/// What the new shell is asked to run, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub argv: Vec<String>,
    pub working_directory: String,
}

/// Why a pane's foreground cannot be replayed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Unreadable,
    /// A shell holds the pane, by its basename.
    Shell(String),
    /// The program's basename is on the restore denylist.
    Denylisted(String),
    Unreplayable,
    TooLong,
    NoDirectory,
}

impl Refusal {
    /// The wire error: the reason, then the way out.
    #[must_use]
    pub fn message(&self) -> String {
        let reason = match self {
            Self::Unreadable => "the pane's foreground program cannot be read".to_owned(),
            Self::Shell(name) => format!("no replayable foreground program: {name} holds the pane"),
            Self::Denylisted(name) => format!("{name} is on the restore denylist"),
            Self::Unreplayable => "the foreground program's arguments cannot be replayed".to_owned(),
            Self::TooLong => {
                format!("the foreground program's command line is too long to replay (max {MAX_COMMAND_BYTES} bytes)")
            }
            Self::NoDirectory => "the foreground program's working directory is unavailable".to_owned(),
        };
        format!("{reason}; pass --command to name the line. nothing was changed")
    }
}

/// The launch to replay. A daemon-leader shell is refused, since replaying its launch wrapper can nest shells;
/// `shell` is the user's `$SHELL` basename, so a non-standard one is recognized too.
///
/// # Errors
///
/// The [`Refusal`] naming why the foreground cannot be replayed.
pub fn resolve<S: AsRef<str>>(
    observation: &Observation,
    shell: Option<&str>,
    denylist: &[S],
) -> Result<Launch, Refusal> {
    let foreground = observation.foreground.as_ref().ok_or(Refusal::Unreadable)?;
    if let Some(name) = foreground.shell_name() {
        return Err(Refusal::Shell(name.to_owned()));
    }
    let argv = foreground.command().ok_or(Refusal::Unreadable)?;
    let program = argv.first().ok_or(Refusal::Unreadable)?;
    let name = basename(program);
    if observation.is_daemon_leader && is_known_shell(name, shell) {
        return Err(Refusal::Shell(name.to_owned()));
    }
    if denylist.iter().any(|entry| entry.as_ref() == name) {
        return Err(Refusal::Denylisted(name.to_owned()));
    }
    let none: [&str; 0] = [];
    if !should_restore(argv, &none) {
        return Err(Refusal::Unreplayable);
    }
    if shell_quoted_line(argv).len() > MAX_COMMAND_BYTES {
        return Err(Refusal::TooLong);
    }
    let working_directory = observation.working_directory.clone().ok_or(Refusal::NoDirectory)?;
    Ok(Launch {
        argv: argv.to_vec(),
        working_directory,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_owned()).collect()
    }

    fn resolve_with(
        foreground: Option<PaneForeground>,
        leader: bool,
        directory: Option<&str>,
        shell: Option<&str>,
        denylist: &[&str],
    ) -> Result<Launch, Refusal> {
        let observation = Observation {
            foreground,
            is_daemon_leader: leader,
            working_directory: directory.map(str::to_owned),
        };
        resolve(&observation, shell, denylist)
    }

    fn program(parts: &[&str]) -> PaneForeground {
        PaneForeground::Program(argv(parts))
    }

    /// The launch of `parts` in `/work`.
    fn launch(parts: &[&str]) -> Launch {
        Launch {
            argv: argv(parts),
            working_directory: "/work".to_owned(),
        }
    }

    #[test]
    fn a_readable_program_replays_its_exact_argv_in_its_own_directory() {
        for parts in [
            &["sleep", "600"][..],
            &["/bin/sh", "/usr/local/bin/cld", "--resume", "an id with spaces"],
            &["sh", "-c", "sleep 600; echo 'done'"],
            &["zsh", "-lic", "echo hi"],
            &["tool", ""],
        ] {
            assert_eq!(
                resolve_with(Some(program(parts)), false, Some("/work"), None, &["tmux"]),
                Ok(launch(parts))
            );
        }
    }

    #[test]
    fn a_program_that_replaced_the_root_shell_is_still_replayed() {
        assert_eq!(
            resolve_with(Some(program(&["sleep", "600"])), true, Some("/work"), None, &[]),
            Ok(launch(&["sleep", "600"]))
        );
    }

    #[test]
    fn the_root_shell_running_its_own_line_is_refused() {
        let cases: [(&[&str], Option<&str>, &str); 3] = [
            (
                &[
                    "/bin/zsh",
                    "-lic",
                    "'builtin' 'eval' -- 'read x' ; 'builtin' 'exec' -- '/bin/zsh' -il",
                ],
                None,
                "zsh",
            ),
            (&["/bin/sh", "-c", "read x"], None, "sh"),
            (&["/opt/bin/xonsh", "-c", "input()"], Some("xonsh"), "xonsh"),
        ];
        for (parts, shell, name) in cases {
            assert_eq!(
                resolve_with(Some(program(parts)), true, Some("/work"), shell, &[]),
                Err(Refusal::Shell(name.to_owned()))
            );
        }
    }

    #[test]
    fn an_unreadable_foreground_or_a_shell_in_it_is_refused() {
        assert_eq!(
            resolve_with(None, false, Some("/work"), None, &[]),
            Err(Refusal::Unreadable)
        );
        assert_eq!(
            resolve_with(
                Some(PaneForeground::ForegroundShell("zsh".to_owned())),
                false,
                Some("/work"),
                None,
                &[]
            ),
            Err(Refusal::Shell("zsh".to_owned()))
        );
    }

    #[test]
    fn a_denylisted_or_unreplayable_program_is_refused() {
        assert_eq!(
            resolve_with(
                Some(program(&["/opt/homebrew/bin/tmux", "attach"])),
                false,
                Some("/work"),
                None,
                &["tmux"]
            ),
            Err(Refusal::Denylisted("tmux".to_owned()))
        );
        assert_eq!(
            resolve_with(Some(program(&["tool", "a\nb"])), false, Some("/work"), None, &[]),
            Err(Refusal::Unreplayable)
        );
    }

    #[test]
    fn the_rendered_line_is_held_to_the_explicit_command_limit() {
        let room = MAX_COMMAND_BYTES - shell_quoted_line(&["tool", ""]).len();
        let fits = "a".repeat(room);
        let over = "a".repeat(room + 1);
        assert_eq!(
            resolve_with(Some(program(&["tool", &fits])), false, Some("/work"), None, &[]),
            Ok(launch(&["tool", &fits]))
        );
        assert_eq!(
            resolve_with(Some(program(&["tool", &over])), false, Some("/work"), None, &[]),
            Err(Refusal::TooLong)
        );
    }

    #[test]
    fn a_program_whose_directory_is_unavailable_is_refused() {
        assert_eq!(
            resolve_with(Some(program(&["sleep", "600"])), false, None, None, &[]),
            Err(Refusal::NoDirectory)
        );
    }

    #[test]
    fn every_refusal_says_nothing_changed_and_names_the_way_out() {
        let cases = [
            (Refusal::Unreadable, "the pane's foreground program cannot be read"),
            (
                Refusal::Shell("zsh".to_owned()),
                "no replayable foreground program: zsh holds the pane",
            ),
            (
                Refusal::Denylisted("tmux".to_owned()),
                "tmux is on the restore denylist",
            ),
            (
                Refusal::Unreplayable,
                "the foreground program's arguments cannot be replayed",
            ),
            (
                Refusal::TooLong,
                "the foreground program's command line is too long to replay (max 4096 bytes)",
            ),
            (
                Refusal::NoDirectory,
                "the foreground program's working directory is unavailable",
            ),
        ];
        for (refusal, reason) in cases {
            assert_eq!(
                refusal.message(),
                format!("{reason}; pass --command to name the line. nothing was changed")
            );
        }
    }
}
