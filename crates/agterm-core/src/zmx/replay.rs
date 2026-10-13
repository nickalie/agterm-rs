//! The script a wrapped pane's daemon runs a command with (upstream's `ZmxReplayScript.swift`): the command, then the
//! zsh integration re-armed and the login shell exec'd, so the pane ends at a prompt whatever the command did. Every
//! builtin is quoted, so a user's alias of `builtin`, `export` or `exec` cannot take it over.

use crate::command_restore::shell_quoted_line;

/// The script for a captured `argv`, quoted word by word.
#[must_use]
pub fn render_argv<S: AsRef<str>>(
    argv: &[S],
    integration_dir: &str,
    inherited_zdotdir: Option<&str>,
    shell: &str,
) -> String {
    render(&shell_quoted_line(argv), integration_dir, inherited_zdotdir, shell)
}

/// The script for a durable creation command, a shell line kept with its operators and word parsing (`eval`).
#[must_use]
pub fn render_command_line(
    command_line: &str,
    integration_dir: &str,
    inherited_zdotdir: Option<&str>,
    shell: &str,
) -> String {
    let statement = format!("'builtin' 'eval' -- {}", quoted(command_line));
    render(&statement, integration_dir, inherited_zdotdir, shell)
}

fn render(statement: &str, integration_dir: &str, inherited_zdotdir: Option<&str>, shell: &str) -> String {
    let zdotdir = match inherited_zdotdir {
        Some(dir) => format!("'builtin' 'export' GHOSTTY_ZSH_ZDOTDIR={}", quoted(dir)),
        None => "'builtin' 'unset' GHOSTTY_ZSH_ZDOTDIR".to_owned(),
    };
    [
        statement.to_owned(),
        format!("'builtin' 'export' ZDOTDIR={}", quoted(integration_dir)),
        zdotdir,
        format!("'builtin' 'exec' -- {} -il", quoted(shell)),
    ]
    .join(" ; ")
}

fn quoted(value: &str) -> String {
    shell_quoted_line(&[value])
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::process::Command;

    use super::*;

    #[test]
    fn a_custom_zdotdir_is_rearmed_before_the_final_shell() {
        let script = render_argv(
            &["printf", "a b"],
            "/Applications/agterm resources/zsh",
            Some("/Users/test/.config/zsh"),
            "/bin/zsh",
        );
        assert_eq!(
            script,
            "'printf' 'a b' ; 'builtin' 'export' ZDOTDIR='/Applications/agterm resources/zsh' ; 'builtin' 'export' \
             GHOSTTY_ZSH_ZDOTDIR='/Users/test/.config/zsh' ; 'builtin' 'exec' -- '/bin/zsh' -il"
        );
    }

    #[test]
    fn a_missing_original_zdotdir_is_unset_before_the_final_shell() {
        assert_eq!(
            render_argv(&["top"], "/bundle/zsh", None, "/bin/zsh"),
            "'top' ; 'builtin' 'export' ZDOTDIR='/bundle/zsh' ; 'builtin' 'unset' GHOSTTY_ZSH_ZDOTDIR ; \
             'builtin' 'exec' -- '/bin/zsh' -il"
        );
    }

    #[test]
    fn a_command_line_is_evaluated_as_one_quoted_word() {
        assert_eq!(
            render_command_line("a && b 'c'", "/z", None, "/bin/zsh"),
            r"'builtin' 'eval' -- 'a && b '\''c'\''' ; 'builtin' 'export' ZDOTDIR='/z' ; 'builtin' 'unset' GHOSTTY_ZSH_ZDOTDIR ; 'builtin' 'exec' -- '/bin/zsh' -il"
        );
    }

    fn executable(path: &Path, contents: &str) {
        fs::write(path, contents).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
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

    #[test]
    fn captured_arguments_survive_the_shell_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let result = dir.path().join("argv");
        let record = dir.path().join("record-argv");
        executable(
            &record,
            "#!/bin/sh\nresult=$1\nshift\nprintf '%s\\n' \"$@\" > \"$result\"\n",
        );
        let expected = ["two words", "$HOME", "single'quote", "semi;colon"];
        let mut argv = vec![record.display().to_string(), result.display().to_string()];
        argv.extend(expected.iter().map(|arg| (*arg).to_owned()));
        assert!(run(&render_argv(&argv, "/bundle/zsh", None, "/usr/bin/true")));
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
        assert!(run(&render_command_line(&line, "/bundle/zsh", None, "/usr/bin/true")));
        assert_eq!(fs::read_to_string(&result).unwrap(), "two words and more");
    }
}
