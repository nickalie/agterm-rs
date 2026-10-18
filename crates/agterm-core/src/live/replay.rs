//! The script a wrapped pane's daemon runs a command with (upstream's `ZmxReplayScript.swift`): the command, then the
//! shell integration re-armed and the login shell exec'd, so the pane ends at a prompt whatever the command did.
//!
//! zsh and bash share the POSIX form, where every builtin is quoted, so a user's alias of `builtin`, `export` or `exec`
//! cannot take it over. fish has no aliases (its `alias` defines a function, which `builtin` bypasses, and `exec` is a
//! reserved word no function can take), so its form is `builtin set -gx` and a plain `exec`, with fish's own quoting.

use super::shell_integration::{LoginShell, Rearm};
use crate::command_restore::shell_quoted_line;

/// The interactive login shell a replay ends in, and what it re-arms before the `exec`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FinalShell<'a> {
    /// The login shell's path.
    pub path: &'a str,
    pub kind: LoginShell,
    pub rearm: &'a [Rearm],
}

/// The script for a captured `argv`, quoted word by word.
#[must_use]
pub fn render_argv<S: AsRef<str>>(argv: &[S], shell: &FinalShell<'_>) -> String {
    let statement = if shell.kind.is_fish() {
        argv.iter()
            .map(|word| fish_quoted(word.as_ref()))
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        shell_quoted_line(argv)
    };
    render(&statement, shell)
}

/// The script for a durable creation command, a shell line kept with its operators and word parsing (`eval`).
#[must_use]
pub fn render_command_line(command_line: &str, shell: &FinalShell<'_>) -> String {
    let statement = if shell.kind.is_fish() {
        format!("builtin eval {}", fish_quoted(command_line))
    } else {
        format!("'builtin' 'eval' -- {}", quoted(command_line))
    };
    render(&statement, shell)
}

fn render(statement: &str, shell: &FinalShell<'_>) -> String {
    let fish = shell.kind.is_fish();
    let mut steps = vec![statement.to_owned()];
    steps.extend(shell.rearm.iter().map(|step| match (step, fish) {
        (Rearm::Export(key, value), false) => format!("'builtin' 'export' {key}={}", quoted(value)),
        (Rearm::Unset(key), false) => format!("'builtin' 'unset' {key}"),
        (Rearm::Export(key, value), true) => format!("builtin set -gx {key} {}", fish_quoted(value)),
        (Rearm::Unset(key), true) => format!("builtin set -e {key}"),
    }));
    let flags = shell.kind.interactive_flags().join(" ");
    steps.push(if fish {
        format!("exec {} {flags}", fish_quoted(shell.path))
    } else {
        format!("'builtin' 'exec' -- {} {flags}", quoted(shell.path))
    });
    steps.join(" ; ")
}

fn quoted(value: &str) -> String {
    shell_quoted_line(&[value])
}

/// `value` as one fish word: single quotes, inside which only a backslash and a single quote are escaped.
#[must_use]
pub fn fish_quoted(value: &str) -> String {
    format!("'{}'", value.replace('\\', r"\\").replace('\'', r"\'"))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use super::*;

    fn zsh_rearm(integration_dir: &str, inherited_zdotdir: Option<&str>) -> Vec<Rearm> {
        vec![
            Rearm::Export("ZDOTDIR", integration_dir.to_owned()),
            inherited_zdotdir.map_or(Rearm::Unset("GHOSTTY_ZSH_ZDOTDIR"), |dir| {
                Rearm::Export("GHOSTTY_ZSH_ZDOTDIR", dir.to_owned())
            }),
        ]
    }

    fn zsh<'a>(rearm: &'a [Rearm], path: &'a str) -> FinalShell<'a> {
        FinalShell {
            path,
            kind: LoginShell::Zsh,
            rearm,
        }
    }

    #[test]
    fn a_custom_zdotdir_is_rearmed_before_the_final_shell() {
        let rearm = zsh_rearm("/Applications/agterm resources/zsh", Some("/Users/test/.config/zsh"));
        let script = render_argv(&["printf", "a b"], &zsh(&rearm, "/bin/zsh"));
        assert_eq!(
            script,
            "'printf' 'a b' ; 'builtin' 'export' ZDOTDIR='/Applications/agterm resources/zsh' ; 'builtin' 'export' \
             GHOSTTY_ZSH_ZDOTDIR='/Users/test/.config/zsh' ; 'builtin' 'exec' -- '/bin/zsh' -il"
        );
    }

    #[test]
    fn a_missing_original_zdotdir_is_unset_before_the_final_shell() {
        let rearm = zsh_rearm("/bundle/zsh", None);
        assert_eq!(
            render_argv(&["top"], &zsh(&rearm, "/bin/zsh")),
            "'top' ; 'builtin' 'export' ZDOTDIR='/bundle/zsh' ; 'builtin' 'unset' GHOSTTY_ZSH_ZDOTDIR ; \
             'builtin' 'exec' -- '/bin/zsh' -il"
        );
    }

    #[test]
    fn a_command_line_is_evaluated_as_one_quoted_word() {
        let rearm = zsh_rearm("/z", None);
        assert_eq!(
            render_command_line("a && b 'c'", &zsh(&rearm, "/bin/zsh")),
            r"'builtin' 'eval' -- 'a && b '\''c'\''' ; 'builtin' 'export' ZDOTDIR='/z' ; 'builtin' 'unset' GHOSTTY_ZSH_ZDOTDIR ; 'builtin' 'exec' -- '/bin/zsh' -il"
        );
    }

    #[test]
    fn bash_rearms_its_posix_mode_injection_and_execs_bash_in_posix_mode() {
        let rearm = [
            Rearm::Export("ENV", "/res/bash/ghostty.bash".to_owned()),
            Rearm::Export("GHOSTTY_BASH_INJECT", "1".to_owned()),
            Rearm::Unset("GHOSTTY_BASH_ENV"),
        ];
        let shell = FinalShell {
            path: "/bin/bash",
            kind: LoginShell::Bash,
            rearm: &rearm,
        };
        assert_eq!(
            render_argv(&["top"], &shell),
            "'top' ; 'builtin' 'export' ENV='/res/bash/ghostty.bash' ; 'builtin' 'export' GHOSTTY_BASH_INJECT='1' ; \
             'builtin' 'unset' GHOSTTY_BASH_ENV ; 'builtin' 'exec' -- '/bin/bash' --posix -il"
        );
    }

    #[test]
    fn fish_sets_its_data_dirs_with_set_and_execs_fish_in_its_own_quoting() {
        let rearm = [
            Rearm::Export("XDG_DATA_DIRS", "/res/it's:/usr/share".to_owned()),
            Rearm::Export("GHOSTTY_SHELL_INTEGRATION_XDG_DIR", "/res/it's".to_owned()),
        ];
        let shell = FinalShell {
            path: "/usr/bin/fish",
            kind: LoginShell::Fish,
            rearm: &rearm,
        };
        assert_eq!(
            render_argv(&["printf", r"a\b", "it's"], &shell),
            r"'printf' 'a\\b' 'it\'s' ; builtin set -gx XDG_DATA_DIRS '/res/it\'s:/usr/share' ; builtin set -gx GHOSTTY_SHELL_INTEGRATION_XDG_DIR '/res/it\'s' ; exec '/usr/bin/fish' -il"
        );
        assert_eq!(
            render_command_line("a && b 'c'", &shell),
            r"builtin eval 'a && b \'c\'' ; builtin set -gx XDG_DATA_DIRS '/res/it\'s:/usr/share' ; builtin set -gx GHOSTTY_SHELL_INTEGRATION_XDG_DIR '/res/it\'s' ; exec '/usr/bin/fish' -il"
        );
        let unset = [Rearm::Unset("X")];
        assert!(render_argv(&["top"], &FinalShell { rearm: &unset, ..shell }).contains(" ; builtin set -e X ; "));
    }

    fn executable(path: &Path, contents: &str) {
        agterm_host::fakes::write_executable(path, contents).unwrap();
    }

    /// Runs `script` under bash: zsh is not on every CI host, and bash has the same quoted builtins, `eval --` and
    /// `exec --` the script relies on.
    fn run(script: &str) -> bool {
        Command::new("/bin/bash")
            .arg("-c")
            .arg(script)
            .status()
            .unwrap()
            .success()
    }

    /// The `record-argv` helper in `dir`: it writes its arguments after the first, one per line, into the first.
    fn recorder(dir: &Path) -> std::path::PathBuf {
        let record = dir.join("record-argv");
        executable(
            &record,
            "#!/bin/sh\nresult=$1\nshift\nprintf '%s\\n' \"$@\" > \"$result\"\n",
        );
        record
    }

    #[test]
    fn captured_arguments_survive_the_shell_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let result = dir.path().join("argv");
        let record = recorder(dir.path());
        let expected = ["two words", "$HOME", "single'quote", "semi;colon"];
        let mut argv = vec![record.display().to_string(), result.display().to_string()];
        argv.extend(expected.iter().map(|arg| (*arg).to_owned()));
        let rearm = zsh_rearm("/bundle/zsh", None);
        assert!(run(&render_argv(&argv, &zsh(&rearm, "/usr/bin/true"))));
        assert_eq!(
            fs::read_to_string(&result).unwrap().lines().collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn a_durable_shell_line_keeps_its_operators_and_word_parsing() {
        let dir = tempfile::tempdir().unwrap();
        let result = dir.path().join("shell-line");
        let output = shell_quoted_line(&[result.display().to_string()]);
        let line = format!("printf '%s' 'two words' > {output} && printf ' and more' >> {output}");
        let rearm = zsh_rearm("/bundle/zsh", None);
        assert!(run(&render_command_line(&line, &zsh(&rearm, "/usr/bin/true"))));
        assert_eq!(fs::read_to_string(&result).unwrap(), "two words and more");
    }

    /// fish, when this host has one (the `rust-linux` CI job installs it for this test): `None` otherwise.
    fn fish() -> Option<&'static str> {
        ["/usr/bin/fish", "/bin/fish", "/usr/local/bin/fish"]
            .into_iter()
            .find(|path| Path::new(path).exists())
    }

    /// Runs `script` under `fish --no-config` in a throwaway home, so no user configuration is read or written.
    fn run_fish(fish: &str, script: &str, home: &Path) -> std::process::Output {
        Command::new(fish)
            .args(["--no-config", "-c", script])
            .env("HOME", home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
    }

    #[test]
    fn a_fish_replay_round_trips_arguments_and_rearms_before_the_exec() {
        let Some(fish) = fish() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let result = dir.path().join("argv");
        let record = recorder(dir.path());
        let expected = [
            "two words",
            "$HOME",
            "single'quote",
            r"back\slash",
            r"trailing\",
            "semi;colon",
        ];
        let mut argv = vec![record.display().to_string(), result.display().to_string()];
        argv.extend(expected.iter().map(|arg| (*arg).to_owned()));
        // the final "shell" prints the re-armed variables, standing in for the interactive fish
        let printer = dir.path().join("print-env");
        executable(
            &printer,
            "#!/bin/sh\nprintf '%s|%s|%s\\n' \"$XDG_DATA_DIRS\" \"$GHOSTTY_SHELL_INTEGRATION_XDG_DIR\" \"$*\"\n",
        );
        let rearm = [
            Rearm::Export("XDG_DATA_DIRS", "/res/it's:/usr/share".to_owned()),
            Rearm::Export("GHOSTTY_SHELL_INTEGRATION_XDG_DIR", "/res/it's".to_owned()),
        ];
        let printer_path = printer.display().to_string();
        let shell = FinalShell {
            path: &printer_path,
            kind: LoginShell::Fish,
            rearm: &rearm,
        };
        let output = run_fish(fish, &render_argv(&argv, &shell), dir.path());
        assert!(output.status.success(), "{output:?}");
        assert_eq!(
            fs::read_to_string(&result).unwrap().lines().collect::<Vec<_>>(),
            expected
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "/res/it's:/usr/share|/res/it's|-il\n"
        );
        let line_result = dir.path().join("line");
        let line = format!(
            "printf '%s' 'two words' > {0} && printf ' and more' >> {0}",
            fish_quoted(&line_result.display().to_string())
        );
        let output = run_fish(fish, &render_command_line(&line, &shell), dir.path());
        assert!(output.status.success(), "{output:?}");
        assert_eq!(fs::read_to_string(&line_result).unwrap(), "two words and more");
    }
}
