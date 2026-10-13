//! The host-free judgements of the restore-running-command feature (upstream's `CommandRestore.swift`): which argv is
//! a login shell with nothing to re-run, what a pane's foreground argv means, which pids stand in for an unreadable
//! group leader, which argv may be replayed at all, how an argv renders back into one POSIX shell line, the seed a
//! pane restores with ([`restore_plan`]) and the user's `restore-denylist.conf`. The process-table reads stay with the
//! frontend; upstream's `KERN_PROCARGS2` parser is macOS-only and has no counterpart here.

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

/// Whether `argv` is shell-shaped with nothing to restore: `argv[0]` is a known shell (or `$SHELL`'s basename, passed
/// as `extra`) and only option flags follow it. Not proof of an interactive prompt: a builtin or a shell loop runs in
/// the shell process and leaves argv untouched. A shell running something is not idle: a script path
/// (`/bin/sh /usr/local/bin/cld`, the foreground of any `#!/bin/sh` wrapper) or a `-c` command leaves a non-flag
/// argument after `argv[0]`.
#[must_use]
pub fn is_idle_shell(argv: &[String], extra: Option<&str>) -> bool {
    let Some((first, rest)) = argv.split_first() else {
        return false;
    };
    is_known_shell(basename(first), extra) && rest.iter().all(|arg| arg.starts_with('-'))
}

/// Classifies a pane's raw foreground argv for the tree read; `None` for an empty argv only. `extra` is `$SHELL`'s
/// basename, widening recognition to a non-standard login shell exactly as [`is_idle_shell`] does.
///
/// The shell basename is taken after [`strip_login_dash`], never before: [`basename`] splits on `/`, so it drops the
/// login mark from the path form (`-/bin/zsh`) but keeps it on the bare form (`-zsh`), the common case, which would
/// otherwise reach callers as `-zsh`. The restore capture collapses [`PaneForeground::ForegroundShell`] to "nothing
/// captured", or `had_foreground` would suppress the session's initial command in [`restore_plan`].
#[must_use]
pub fn pane_foreground(argv: &[String], extra: Option<&str>) -> Option<PaneForeground> {
    let stripped = strip_login_dash(argv);
    let first = stripped.first()?;
    if is_idle_shell(argv, extra) {
        return Some(PaneForeground::ForegroundShell(basename(first).to_owned()));
    }
    Some(PaneForeground::Program(stripped))
}

/// `argv` without the `-` a login process's `argv[0]` is marked with, so it names a program that can be rendered and
/// re-run (`-sleep` becomes `sleep`). Only `argv[0]` carries the mark and only the mark goes: the path form
/// (`-/bin/zsh`) keeps the rest of the path.
#[must_use]
pub fn strip_login_dash(argv: &[String]) -> Vec<String> {
    let mut result = argv.to_vec();
    if let Some(first) = result.first_mut()
        && let Some(rest) = first.strip_prefix('-')
    {
        *first = rest.to_owned();
    }
    result
}

/// One process-group member as the process table reports it: its own pid and its parent's (upstream's
/// `CommandRestore.ProcessGroupMember`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessGroupMember {
    pub pid: i32,
    pub ppid: i32,
}

/// The pids to try when a process group's leader argv is unreadable, lowest pid first: the leader's direct children.
/// A pane with no job-control shell (a `--command` session) may run its program under a leader whose argv cannot be
/// read, so that child is the real answer.
///
/// Only direct children qualify while the leader is alive, and parentage rather than pid order decides: a pipeline
/// under a job-control shell puts every element in one group led by the first while parenting them all to the shell,
/// so `sudo tail … | grep …` must not report `grep`, a sibling of the leader rather than its child; and once pids
/// wrap, a freshly forked grandchild can sort below the program that spawned it.
///
/// A leader that has already exited is the exception: `cat f | less` keeps the group id of the reaped `cat`, so no
/// survivor is its child. With no leader in the group there is nothing to check parentage against, so every survivor
/// qualifies. A non-positive pid never does.
#[must_use]
pub fn group_descent_candidates(pgid: i32, members: &[ProcessGroupMember]) -> Vec<i32> {
    let leader_alive = members.iter().any(|member| member.pid == pgid);
    let mut pids: Vec<i32> = members
        .iter()
        .filter(|member| member.pid != pgid && member.pid > 0)
        .filter(|member| !leader_alive || member.ppid == pgid)
        .map(|member| member.pid)
        .collect();
    pids.sort_unstable();
    pids
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

/// The seed a pane restores or is created with (upstream's `CommandRestore.RestorePlan`), the two fields mutually
/// exclusive: `command` takes the exec path (it replaces the shell and the pane closes on its exit); otherwise
/// `initial_input` is typed into a login shell, and both `None` is a plain shell. `wait_after_command` holds the pane
/// after the command exits and is effective only on the exec path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestorePlan {
    pub command: Option<String>,
    pub initial_input: Option<String>,
    pub wait_after_command: bool,
}

/// What [`restore_plan`] decides from (upstream's `CommandRestore.RestoreInputs`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "mirrors upstream's RestoreInputs: each flag is an independent input of the precedence table"
)]
pub struct RestoreInputs {
    /// The session came from a restore: a fresh command session always runs its command, a restored one only in
    /// rerun mode.
    pub was_restored: bool,
    /// The launch restore mode, frozen for the process, is `rerun`.
    pub restore_enabled: bool,
    /// A foreground command was captured, at the last quit or by `restore.capture`. It preempts `initial_command`
    /// even when suppressed (denylisted or off, so `foreground_input` is `None`), yielding a plain shell rather than
    /// the stale creation command: gate on the capture, not on the input surviving.
    pub had_foreground: bool,
    /// The rendered foreground command line to type, or `None` (nothing captured, or suppressed).
    pub foreground_input: Option<String>,
    /// The session's persisted `--command`.
    pub initial_command: Option<String>,
    /// The pane's pinned restore command (`session.restore`): `None` is no override, `""` pins the pane to nothing,
    /// anything else is a shell line to run.
    pub restore_override: Option<String>,
    /// A creation command should hold the pane after it exits; ignored without an effective command.
    pub requested_wait: bool,
}

/// A pane's `initial_input`: a pinned override when one exists (empty pins to nothing, a plain shell), else the
/// captured foreground input. The override is gated on `restore_enabled`, keeping rerun mode the single switch, and is
/// typed verbatim, never through [`shell_quoted_line`], so `cd x && claude --resume y` works as written; one carrying
/// a control character is refused, since a pin loaded from a snapshot never passed `session.restore`'s write check.
/// The captured input arrives already gated and denylist-filtered.
#[must_use]
pub fn restore_input(
    restore_enabled: bool,
    restore_override: Option<&str>,
    captured_input: Option<&str>,
) -> Option<String> {
    let Some(restore_override) = restore_override else {
        return captured_input.map(str::to_owned);
    };
    if !restore_enabled || restore_override.is_empty() || has_control_character(restore_override) {
        return None;
    }
    Some(format!("{restore_override}\n"))
}

/// Decides a pane's seed on create or restore. A present `restore_override` short-circuits everything: it wins over
/// the captured foreground and `initial_command`, never takes the exec path, and its input comes from
/// [`restore_input`], so it obeys the launch mode while bypassing the denylist, which guards blind capture, not a
/// deliberately named command. Without an override the capture preempts `initial_command`, which a restored session
/// runs only in rerun mode.
#[must_use]
pub fn restore_plan(inputs: &RestoreInputs) -> RestorePlan {
    if let Some(restore_override) = inputs.restore_override.as_deref() {
        return RestorePlan {
            command: None,
            initial_input: restore_input(
                inputs.restore_enabled,
                Some(restore_override),
                inputs.foreground_input.as_deref(),
            ),
            wait_after_command: false,
        };
    }
    let may_run_initial = !inputs.was_restored || inputs.restore_enabled;
    let command = if !inputs.had_foreground && may_run_initial {
        inputs.initial_command.clone()
    } else {
        None
    };
    RestorePlan {
        initial_input: if command.is_none() {
            inputs.foreground_input.clone()
        } else {
            None
        },
        wait_after_command: command.is_some() && inputs.requested_wait,
        command,
    }
}

/// The denylist's file name in the config directory.
pub const DENYLIST_FILE: &str = "restore-denylist.conf";

/// The commented starter `restore-denylist.conf`, written when none exists (upstream's
/// `SettingsModel.starterRestoreDenylistText`).
pub const DENYLIST_STARTER: &str = "\
# restore-denylist.conf: programs NOT to re-run in rerun restore mode. One command name per line,
# matched on the command's basename. Blank lines and lines
# starting with # are ignored. Read at launch; edits take effect on the next launch.
#
# Terminal multiplexers just start a fresh, empty session when re-run (your old session is gone),
# so they are listed by default — delete a line to let it restore:
tmux
screen
zellij
#
# Add anything else you would rather start fresh, e.g. an editor or pager:
# vim
# less
";

/// Parses `restore-denylist.conf` into the program basenames not to re-run on restore, each once, in file order: one
/// entry per line, trimmed of spaces and tabs; blank lines and `#` comments ignored. There is no built-in list: the
/// file is the whole source. A list rather than upstream's set, to fit [`should_restore`]'s slice.
#[must_use]
pub fn parse_denylist(text: &str) -> Vec<String> {
    let mut entries: Vec<String> = Vec::new();
    for line in text.split('\n').map(|line| line.trim_matches(is_inline_whitespace)) {
        if line.is_empty() || line.starts_with('#') || entries.iter().any(|entry| entry == line) {
            continue;
        }
        entries.push(line.to_owned());
    }
    entries
}

/// Foundation's `.whitespaces`: horizontal whitespace (tab and the space separators), no line breaks.
fn is_inline_whitespace(c: char) -> bool {
    c.is_whitespace() && !matches!(c, '\n' | '\u{b}' | '\u{c}' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}')
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

    fn inputs(was_restored: bool, enabled: bool, had_foreground: bool) -> RestoreInputs {
        RestoreInputs {
            was_restored,
            restore_enabled: enabled,
            had_foreground,
            ..RestoreInputs::default()
        }
    }

    fn plan(command: Option<&str>, initial_input: Option<&str>) -> RestorePlan {
        RestorePlan {
            command: command.map(str::to_owned),
            initial_input: initial_input.map(str::to_owned),
            wait_after_command: false,
        }
    }

    #[test]
    fn strip_login_dash_drops_only_the_mark() {
        assert_eq!(strip_login_dash(&argv(&["-sleep", "900"])), argv(&["sleep", "900"]));
        assert_eq!(strip_login_dash(&argv(&["-/bin/zsh"])), argv(&["/bin/zsh"]));
        assert_eq!(strip_login_dash(&argv(&["sleep", "900"])), argv(&["sleep", "900"]));
        assert_eq!(
            strip_login_dash(&argv(&["git", "-C", "/tmp", "status"])),
            argv(&["git", "-C", "/tmp", "status"])
        );
        assert_eq!(strip_login_dash(&argv(&[])), argv(&[]));
        assert_eq!(strip_login_dash(&argv(&["-"])), argv(&[""]));
    }

    #[test]
    fn group_descent_keeps_only_the_leaders_children() {
        fn m(pid: i32, parent: i32) -> ProcessGroupMember {
            ProcessGroupMember { pid, ppid: parent }
        }
        assert_eq!(
            group_descent_candidates(100, &[m(100, 9), m(102, 100), m(101, 100)]),
            [101, 102]
        );
        assert!(group_descent_candidates(100, &[m(100, 9)]).is_empty());
        assert!(group_descent_candidates(100, &[]).is_empty());
        assert_eq!(
            group_descent_candidates(100, &[m(0, 100), m(-1, 100), m(101, 100)]),
            [101]
        );
        // a pipeline parents every element to the shell while the group leads on the first: a sibling of the leader
        // is not the pane's foreground.
        assert!(group_descent_candidates(100, &[m(100, 9), m(101, 9)]).is_empty());
        // a grandchild that took a low pid after the wrap must not outrank its own parent.
        assert_eq!(
            group_descent_candidates(100, &[m(100, 9), m(99500, 100), m(500, 99500)]),
            [99500]
        );
        // a reaped leader (`cat f | less`) leaves nothing to check parentage against, so survivors qualify.
        assert_eq!(group_descent_candidates(100, &[m(101, 9)]), [101]);
        assert_eq!(group_descent_candidates(100, &[m(102, 9), m(101, 9)]), [101, 102]);
    }

    #[test]
    fn an_idle_shell_is_a_bare_prompt_not_a_script() {
        assert!(is_idle_shell(&argv(&["-zsh"]), None));
        assert!(is_idle_shell(&argv(&["/bin/zsh"]), None));
        assert!(is_idle_shell(&argv(&["-/bin/zsh"]), None));
        assert!(is_idle_shell(&argv(&["zsh", "-i", "-l"]), None));
        assert!(is_idle_shell(&argv(&["bash"]), Some("bash")));
        // a shell running a script or -c is not idle: the real-world `cld` launcher bug.
        assert!(!is_idle_shell(&argv(&["/bin/sh", "/usr/local/bin/cld"]), None));
        assert!(!is_idle_shell(
            &argv(&["/bin/sh", "/usr/local/bin/cld", "--flag"]),
            None
        ));
        assert!(!is_idle_shell(&argv(&["bash", "-c", "echo hi"]), None));
        assert!(!is_idle_shell(&argv(&["htop"]), None));
        assert!(!is_idle_shell(&argv(&[]), None));
    }

    #[test]
    fn pane_foreground_names_the_shell_without_its_login_dash() {
        let shell = |name: &str| Some(PaneForeground::ForegroundShell(name.to_owned()));
        assert_eq!(pane_foreground(&argv(&["-zsh"]), None), shell("zsh"));
        assert_eq!(pane_foreground(&argv(&["-/bin/zsh"]), None), shell("zsh"));
        assert_eq!(pane_foreground(&argv(&["/bin/zsh"]), None), shell("zsh"));
        assert_eq!(pane_foreground(&argv(&["zsh", "-i", "-l"]), None), shell("zsh"));
        assert_eq!(pane_foreground(&argv(&["fish"]), None), shell("fish"));
        assert_eq!(pane_foreground(&argv(&["-nu"]), Some("nu")), shell("nu"));
        // a shell busy inside itself (`read`, a loop) keeps its argv, so it reads as one at a prompt.
        assert_eq!(pane_foreground(&argv(&["-bash"]), None), shell("bash"));
        assert_eq!(pane_foreground(&argv(&[]), None), None);
    }

    #[test]
    fn pane_foreground_reports_programs_and_unrecognized_shells() {
        let program = |parts: &[&str]| Some(PaneForeground::Program(argv(parts)));
        assert_eq!(pane_foreground(&argv(&["htop"]), None), program(&["htop"]));
        assert_eq!(pane_foreground(&argv(&["-sleep", "5"]), None), program(&["sleep", "5"]));
        assert_eq!(
            pane_foreground(&argv(&["/bin/sh", "/usr/local/bin/cld"]), None),
            program(&["/bin/sh", "/usr/local/bin/cld"])
        );
        assert_eq!(
            pane_foreground(&argv(&["bash", "-c", "echo hi"]), None),
            program(&["bash", "-c", "echo hi"])
        );
        assert_eq!(pane_foreground(&argv(&["nu"]), None), program(&["nu"]));
        let shell = pane_foreground(&argv(&["-zsh"]), None).unwrap();
        assert_eq!((shell.command(), shell.shell_name()), (None, Some("zsh")));
        let program = pane_foreground(&argv(&["htop"]), None).unwrap();
        assert_eq!(program.command(), Some(argv(&["htop"]).as_slice()));
        assert_eq!(program.shell_name(), None);
    }

    #[test]
    fn a_fresh_command_session_always_runs_its_command() {
        for enabled in [true, false] {
            let fresh = RestoreInputs {
                initial_command: Some("ssh host".to_owned()),
                ..inputs(false, enabled, false)
            };
            assert_eq!(restore_plan(&fresh), plan(Some("ssh host"), None));
        }
    }

    #[test]
    fn a_restored_command_session_runs_its_command_only_when_enabled() {
        let on = RestoreInputs {
            initial_command: Some("ssh host".to_owned()),
            ..inputs(true, true, false)
        };
        assert_eq!(restore_plan(&on), plan(Some("ssh host"), None));
        let off = RestoreInputs {
            restore_enabled: false,
            ..on
        };
        assert_eq!(restore_plan(&off), plan(None, None));
    }

    #[test]
    fn a_capture_preempts_the_initial_command_even_when_suppressed() {
        let captured = RestoreInputs {
            foreground_input: Some("top\n".to_owned()),
            initial_command: Some("ssh host".to_owned()),
            ..inputs(true, true, true)
        };
        assert_eq!(restore_plan(&captured), plan(None, Some("top\n")));
        // captured but suppressed (denylisted, unreplayable or off): a plain shell, not the stale command.
        let suppressed = RestoreInputs {
            foreground_input: None,
            ..captured
        };
        assert_eq!(restore_plan(&suppressed), plan(None, None));
        assert_eq!(restore_plan(&inputs(true, true, false)), plan(None, None));
    }

    #[test]
    fn wait_after_command_needs_an_effective_exec_command() {
        let holding = RestoreInputs {
            initial_command: Some("ssh host".to_owned()),
            requested_wait: true,
            ..inputs(true, true, false)
        };
        assert!(restore_plan(&holding).wait_after_command);
        let not_requested = RestoreInputs {
            requested_wait: false,
            ..holding.clone()
        };
        assert!(!restore_plan(&not_requested).wait_after_command);
        let captured = RestoreInputs {
            had_foreground: true,
            foreground_input: Some("top\n".to_owned()),
            ..holding.clone()
        };
        let captured = restore_plan(&captured);
        assert_eq!((captured.command, captured.wait_after_command), (None, false));
        let overridden = RestoreInputs {
            restore_override: Some("claude --resume split".to_owned()),
            ..holding
        };
        assert_eq!(restore_plan(&overridden), plan(None, Some("claude --resume split\n")));
    }

    #[test]
    fn restore_input_falls_through_without_an_override() {
        for enabled in [true, false] {
            assert_eq!(restore_input(enabled, None, Some("top\n")).as_deref(), Some("top\n"));
            assert_eq!(restore_input(enabled, None, None), None);
        }
    }

    #[test]
    fn a_pinned_override_runs_verbatim_only_when_enabled() {
        assert_eq!(
            restore_input(true, Some("cd x && claude --resume y"), Some("top\n")).as_deref(),
            Some("cd x && claude --resume y\n")
        );
        assert_eq!(restore_input(false, Some("claude --resume y"), Some("top\n")), None);
        for enabled in [true, false] {
            assert_eq!(restore_input(enabled, Some(""), Some("top\n")), None);
            assert_eq!(restore_input(enabled, Some(""), None), None);
        }
        // `session.restore set` rejects these on write, but a pin loaded from a snapshot never passed that check.
        assert_eq!(restore_input(true, Some("tail -f\u{1b}x"), Some("'top'\n")), None);
        assert_eq!(restore_input(true, Some("a\nrm -rf x"), None), None);
    }

    #[test]
    fn an_override_never_takes_the_exec_path_and_beats_everything() {
        let fresh = RestoreInputs {
            initial_command: Some("ssh host".to_owned()),
            restore_override: Some("claude --resume y".to_owned()),
            ..inputs(false, true, false)
        };
        assert_eq!(restore_plan(&fresh), plan(None, Some("claude --resume y\n")));
        let captured = RestoreInputs {
            foreground_input: Some("top\n".to_owned()),
            restore_override: Some("claude --resume y".to_owned()),
            ..inputs(true, true, true)
        };
        assert_eq!(restore_plan(&captured), plan(None, Some("claude --resume y\n")));
        // "" pins the pane to nothing: neither the creation command nor the capture runs.
        let pinned_empty = RestoreInputs {
            initial_command: Some("ssh host".to_owned()),
            restore_override: Some(String::new()),
            ..captured.clone()
        };
        assert_eq!(restore_plan(&pinned_empty), plan(None, None));
        let pinned_empty_fresh = RestoreInputs {
            restore_override: Some(String::new()),
            ..fresh
        };
        assert_eq!(restore_plan(&pinned_empty_fresh), plan(None, None));
        // the disabled launch mode suppresses an override even over a capture.
        let disabled = RestoreInputs {
            restore_enabled: false,
            initial_command: Some("ssh host".to_owned()),
            ..captured
        };
        assert_eq!(restore_plan(&disabled), plan(None, None));
    }

    #[test]
    fn the_denylist_reads_basenames_ignoring_comments_and_blanks() {
        let text =
            "# programs not to restore\ntmux\n  screen  \n \n# vim   (commented out, not active)\nzellij\n\ttmux";
        assert_eq!(parse_denylist(text), argv(&["tmux", "screen", "zellij"]));
        assert!(parse_denylist("").is_empty());
        assert!(parse_denylist("# only comments\n\n").is_empty());
        assert_eq!(parse_denylist(DENYLIST_STARTER), argv(&["tmux", "screen", "zellij"]));
        assert!(DENYLIST_STARTER.ends_with("# less\n"));
        let denylist = parse_denylist(DENYLIST_STARTER);
        assert!(!should_restore(&argv(&["/usr/bin/tmux", "attach"]), &denylist));
        assert!(should_restore(&argv(&["vim", "x"]), &denylist));
    }

    #[test]
    fn the_quoted_line_survives_quotes_and_spaces() {
        assert_eq!(
            shell_quoted_line(&["sh", "-c", "echo 'done'", ""]),
            r"'sh' '-c' 'echo '\''done'\''' ''"
        );
    }
}
