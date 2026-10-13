//! The host-free judgements of the restore-running-command feature (upstream's `CommandRestore.swift`): which argv is
//! a login shell with nothing to re-run, which one may be replayed at all, and how an argv renders back into one
//! POSIX shell line. The process-table reads stay with the frontend.
//!
//! This holds what `session.restart`'s replay ([`crate::restart_replay`]) and `session.restore`'s validation need;
//! the restore plan, the denylist file and the capture classification join with the restore-mode work.

/// The login shells read as "no program to restore": a shell held the pane, nothing to re-run.
const KNOWN_SHELLS: [&str; 8] = ["zsh", "bash", "sh", "fish", "dash", "ksh", "tcsh", "csh"];

/// What a pane's foreground argv means: a program, or a recognized shell in the foreground (upstream's
/// `CommandRestore.PaneForeground`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneForeground {
    /// A real foreground program, its argv dash-stripped and ready to render.
    Program(Vec<String>),
    /// A recognized shell in the foreground, as its basename (`zsh`, `fish`).
    ForegroundShell(String),
}

impl PaneForeground {
    /// The argv the tree reports as `foreground`; `None` while a shell holds the foreground.
    #[must_use]
    pub fn command(&self) -> Option<&[String]> {
        match self {
            Self::Program(argv) => Some(argv),
            Self::ForegroundShell(_) => None,
        }
    }

    /// The basename the tree reports as `foregroundShell`; `None` while a program runs.
    #[must_use]
    pub fn shell_name(&self) -> Option<&str> {
        match self {
            Self::Program(_) => None,
            Self::ForegroundShell(name) => Some(name),
        }
    }
}

/// The last path component of `path`, or `path` itself when it has none (a path of slashes only).
#[must_use]
pub fn basename(path: &str) -> &str {
    path.split('/').rfind(|part| !part.is_empty()).unwrap_or(path)
}

/// Whether `basename` is a login shell, also matching the user's `$SHELL` basename passed as `extra`. A leading `-`
/// (a login shell's dash-marked argv[0]) is stripped first.
#[must_use]
pub fn is_known_shell(basename: &str, extra: Option<&str>) -> bool {
    let name = basename.strip_prefix('-').unwrap_or(basename);
    KNOWN_SHELLS.contains(&name) || extra.is_some_and(|extra| !extra.is_empty() && name == extra)
}

/// Whether a captured argv may be re-run: false for an empty argv or an empty `argv[0]`, one whose `argv[0]` basename
/// is on `denylist`, or one carrying a control character or U+FFFD (what an undecodable byte became).
#[must_use]
pub fn should_restore<S: AsRef<str>>(argv: &[String], denylist: &[S]) -> bool {
    let Some(first) = argv.first().filter(|first| !first.is_empty()) else {
        return false;
    };
    if argv.iter().any(|arg| arg.chars().any(is_unreplayable)) {
        return false;
    }
    let name = basename(first);
    !denylist.iter().any(|entry| entry.as_ref() == name)
}

/// Whether `value` carries a character a line editor reads as an editing command (C0 or DEL), tab included.
#[must_use]
pub fn has_control_character(value: &str) -> bool {
    value.chars().any(is_control_character)
}

/// `argv` as one POSIX shell line: each argument single-quoted, an embedded `'` as `'\''`, space-joined.
#[must_use]
pub fn shell_quoted_line<S: AsRef<str>>(argv: &[S]) -> String {
    argv.iter()
        .map(|arg| format!("'{}'", arg.as_ref().replace('\'', r"'\''")))
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_control_character(c: char) -> bool {
    u32::from(c) < 0x20 || c == '\u{7f}'
}

fn is_unreplayable(c: char) -> bool {
    is_control_character(c) || c == '\u{fffd}'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_owned()).collect()
    }

    #[test]
    fn basename_takes_the_last_component() {
        assert_eq!(basename("/opt/homebrew/bin/tmux"), "tmux");
        assert_eq!(basename("tmux"), "tmux");
        assert_eq!(basename("/usr/bin/"), "bin");
        assert_eq!(basename("-/bin/zsh"), "zsh");
        assert_eq!(basename("-zsh"), "-zsh");
    }

    #[test]
    fn a_login_dash_and_the_users_shell_are_recognized() {
        assert!(is_known_shell("zsh", None));
        assert!(is_known_shell("-zsh", None));
        assert!(!is_known_shell("xonsh", None));
        assert!(is_known_shell("xonsh", Some("xonsh")));
        assert!(!is_known_shell("xonsh", Some("")));
    }

    #[test]
    fn a_control_character_or_replacement_char_or_denylisted_program_is_not_restored() {
        let none: [&str; 0] = [];
        assert!(should_restore(&argv(&["sleep", "600"]), &none));
        assert!(!should_restore(&argv(&[]), &none));
        assert!(!should_restore(&argv(&[""]), &none));
        assert!(!should_restore(&argv(&["tool", "a\nb"]), &none));
        assert!(!should_restore(&argv(&["tool", "a\u{fffd}"]), &none));
        assert!(!should_restore(&argv(&["/opt/bin/tmux", "attach"]), &["tmux"]));
    }

    #[test]
    fn control_characters_include_tab_and_delete() {
        assert!(has_control_character("a\tb"));
        assert!(has_control_character("a\u{7f}"));
        assert!(has_control_character("\u{1b}[2J"));
        assert!(!has_control_character("echo 'x' | cat $HOME"));
    }

    #[test]
    fn the_quoted_line_survives_quotes_and_spaces() {
        assert_eq!(
            shell_quoted_line(&["sh", "-c", "echo 'done'", ""]),
            r"'sh' '-c' 'echo '\''done'\''' ''"
        );
    }
}
