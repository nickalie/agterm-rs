//! The script a wrapped pane's daemon runs a command with (upstream's `ZmxReplayScript.swift`): the command, then the
//! shell integration re-armed and the login shell exec'd, so the pane ends at a prompt whatever the command did.
//!
//! zsh and bash share the POSIX form, where every builtin is quoted, so a user's alias of `builtin`, `export` or `exec`
//! cannot take it over. fish has no aliases (its `alias` defines a function, which `builtin` bypasses, and `exec` is a
//! reserved word no function can take), so its form is `builtin set -gx` and a plain `exec`, with fish's own quoting.
//!
//! PowerShell, a Rust-only extension, has no `exec`: its replay runs in the shell `-NoExit` keeps
//! ([`LoginShell::script_flags`]), so the script re-arms the integration's variable and loads the integration first,
//! then runs the command (`& 'program' 'argument'`, or a durable line as a script block). Loading it first keeps the
//! prompt integrated however the command ends: a Ctrl+C stops the rest of a `-Command` script, not only the command.
//! Only language syntax and .NET calls, so no alias or function of the user's takes it over. Words are single-quoted
//! ([`powershell_quoted`]). How a native program receives them is PowerShell's own argument passing
//! (`$PSNativeCommandArgumentPassing`), which the captured argv runs under in a scope of its own: a profile's `Legacy`
//! is replaced there by the mode PowerShell 7.3+ defaults to on the OS (`Standard`; `Windows` on Windows), the user's
//! preference left as it is for later commands. On Linux and macOS, where a program receives an argv, that passing is
//! exact. On Windows a program receives one command line it splits itself: `Windows` mode quotes for the C runtime's
//! split (`CommandLineToArgvW`), exact for programs that split that way, but keeps the legacy passing for `cmd.exe`,
//! batch files (`.bat`, `.cmd`), Windows Script Host scripts (`.js`, `.vbs`, `.wsf`, run by `cscript`/`wscript`) and
//! `msiexec`, whose own parsers no argv rebuilds exactly (`cmd.exe`'s least of all: its `/c` text is no argv).
//! Windows PowerShell 5.1 has only the legacy passing, which loses an embedded double quote or an empty argument.

use super::shell_integration::{LoginShell, PWSH_LOAD, Rearm};
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
    let words = |quote: fn(&str) -> String| {
        argv.iter()
            .map(|word| quote(word.as_ref()))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let statement = if shell.kind.is_fish() {
        words(fish_quoted)
    } else if shell.kind.is_powershell() {
        format!(
            "& {{ if ({NATIVE_PASSING_LOOKUP} -eq 'Legacy') {{ $PSNativeCommandArgumentPassing = '{}' }} ; & {} }}",
            exact_native_passing(),
            words(powershell_quoted)
        )
    } else {
        shell_quoted_line(argv)
    };
    render(&statement, shell)
}

/// `$PSNativeCommandArgumentPassing` read without failing where it is unset (Windows PowerShell 5.1) under a
/// profile's `Set-StrictMode`.
const NATIVE_PASSING_LOOKUP: &str =
    "$ExecutionContext.SessionState.PSVariable.GetValue('PSNativeCommandArgumentPassing')";

/// The native argument passing PowerShell 7.3+ defaults to on this OS, which a captured argv is replayed under in place
/// of a profile's `Legacy`.
fn exact_native_passing() -> &'static str {
    if cfg!(windows) { "Windows" } else { "Standard" }
}

/// The script for a durable creation command, a shell line kept with its operators and word parsing (`eval`; in
/// PowerShell a script block made of it, dot-sourced).
#[must_use]
pub fn render_command_line(command_line: &str, shell: &FinalShell<'_>) -> String {
    let statement = if shell.kind.is_fish() {
        format!("builtin eval {}", fish_quoted(command_line))
    } else if shell.kind.is_powershell() {
        format!(". ([scriptblock]::Create({}))", powershell_quoted(command_line))
    } else {
        format!("'builtin' 'eval' -- {}", quoted(command_line))
    };
    render(&statement, shell)
}

fn render(statement: &str, shell: &FinalShell<'_>) -> String {
    if shell.kind.is_powershell() {
        return render_powershell(statement, shell);
    }
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

/// PowerShell's replay: the re-armed variables, the integration loaded, then the command, in the shell that stays.
fn render_powershell(statement: &str, shell: &FinalShell<'_>) -> String {
    let mut steps: Vec<String> = shell
        .rearm
        .iter()
        .map(|step| match step {
            Rearm::Export(key, value) => format!("$env:{key} = {}", powershell_quoted(value)),
            Rearm::Unset(key) => format!("$env:{key} = $null"),
        })
        .collect();
    steps.push(PWSH_LOAD.to_owned());
    steps.push(statement.to_owned());
    steps.join(" ; ")
}

fn quoted(value: &str) -> String {
    shell_quoted_line(&[value])
}

/// `value` as one PowerShell word: a single-quoted string, inside which a single quote is written twice. PowerShell
/// takes the typographic single quotes (U+2018–U+201B) for one as well, so those are doubled too.
#[must_use]
pub fn powershell_quoted(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for character in value.chars() {
        if matches!(character, '\'' | '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}') {
            quoted.push(character);
        }
        quoted.push(character);
    }
    quoted.push('\'');
    quoted
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
        let rearm = zsh_rearm("/Applications/ngterm resources/zsh", Some("/Users/test/.config/zsh"));
        let script = render_argv(&["printf", "a b"], &zsh(&rearm, "/bin/zsh"));
        assert_eq!(
            script,
            "'printf' 'a b' ; 'builtin' 'export' ZDOTDIR='/Applications/ngterm resources/zsh' ; 'builtin' 'export' \
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
        ngterm_host::fakes::write_executable(path, contents).unwrap();
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
    #[cfg_attr(
        windows,
        ignore = "runs its script with the POSIX shell /bin/sh, which Windows has not"
    )]
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
    #[cfg_attr(
        windows,
        ignore = "runs its script with the POSIX shell /bin/sh, which Windows has not"
    )]
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
    #[cfg_attr(windows, ignore = "runs the replay under fish, which Windows has not")]
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

    fn pwsh_shell<'a>(rearm: &'a [Rearm], path: &'a str) -> FinalShell<'a> {
        FinalShell {
            path,
            kind: LoginShell::Pwsh,
            rearm,
        }
    }

    #[test]
    fn a_powershell_word_doubles_every_single_quote_powershell_reads() {
        assert_eq!(powershell_quoted("plain"), "'plain'");
        assert_eq!(powershell_quoted(""), "''");
        assert_eq!(powershell_quoted("it's"), "'it''s'");
        assert_eq!(
            powershell_quoted("\u{2018}curly\u{2019}"),
            "'\u{2018}\u{2018}curly\u{2019}\u{2019}'"
        );
        assert_eq!(
            powershell_quoted("\u{201a}low\u{201b}"),
            "'\u{201a}\u{201a}low\u{201b}\u{201b}'"
        );
        // nothing else is special inside single quotes: no variables, escapes or subexpressions
        assert_eq!(
            powershell_quoted(r#"$HOME `n "x" $(y) C:\dir\"#),
            r#"'$HOME `n "x" $(y) C:\dir\'"#
        );
    }

    #[test]
    fn powershell_rearms_and_loads_the_integration_before_the_command() {
        let rearm = [
            Rearm::Export("NGTERM_PWSH_INTEGRATION", r"C:\it's\ngterm.ps1".to_owned()),
            Rearm::Unset("STALE"),
        ];
        let shell = pwsh_shell(&rearm, r"C:\Program Files\PowerShell\7\pwsh.exe");
        let prefix =
            format!(r"$env:NGTERM_PWSH_INTEGRATION = 'C:\it''s\ngterm.ps1' ; $env:STALE = $null ; {PWSH_LOAD} ; ");
        // a captured argv runs in a scope of its own, out of a profile's legacy argument passing
        let passing = if cfg!(windows) { "Windows" } else { "Standard" };
        assert_eq!(
            render_argv(&[r"C:\tools\top.exe", "a b", "it's"], &shell),
            format!(
                "{prefix}& {{ if ($ExecutionContext.SessionState.PSVariable.GetValue('PSNativeCommandArgumentPassing') \
                 -eq 'Legacy') {{ $PSNativeCommandArgumentPassing = '{passing}' }} ; & 'C:\\tools\\top.exe' 'a b' \
                 'it''s' }}"
            )
        );
        assert_eq!(
            render_command_line("Get-ChildItem | Select-Object -First 1; 'done'", &shell),
            format!("{prefix}. ([scriptblock]::Create('Get-ChildItem | Select-Object -First 1; ''done'''))")
        );
        // no exec: the shell `-NoExit` keeps is the final one, so its path is no part of the script
        assert!(!render_argv(&["top"], &shell).contains("pwsh.exe"));
    }

    /// PowerShell 7 when this host has one on `PATH` (the CI runners of every OS carry it): `None` otherwise.
    fn pwsh() -> Option<std::path::PathBuf> {
        let name = if cfg!(windows) { "pwsh.exe" } else { "pwsh" };
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|dir| dir.join(name))
            .find(|path| path.is_file())
    }

    /// Where [`argv_recorder`] writes what it was run with.
    const RECORD: &str = "REPLAY_TEST_RECORD";

    /// Runs `script` under `pwsh` as a non-interactive command, with the live ngterm's variables stripped.
    fn run_pwsh(pwsh: &Path, script: &str, record: &Path) -> std::process::Output {
        let mut command = Command::new(pwsh);
        command
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", script])
            .env(RECORD, record)
            .env("STALE", "inherited")
            .stdin(std::process::Stdio::null());
        for (key, _) in std::env::vars_os() {
            if key
                .to_str()
                .is_some_and(|key| key.starts_with("NGTERM_") || key.starts_with("AGTERM_") || key.starts_with("ZMX_"))
            {
                command.env_remove(key);
            }
        }
        command.output().unwrap()
    }

    #[test]
    fn a_powershell_replay_round_trips_arguments_after_loading_the_integration() {
        let Some(pwsh) = pwsh() else {
            eprintln!("SKIPPED: no pwsh on PATH");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("record.json");
        // the stand-in integration marks what the command then inherits; its path needs quoting
        let integration = dir.path().join("it's integration.ps1");
        fs::write(&integration, "$env:REPLAY_TEST_LOADED = $env:NGTERM_PWSH_INTEGRATION\n").unwrap();
        let expected = [
            "a b",
            "it's \"quoted\"",
            r#"back\"slash"#,
            r"C:\dir with space\",
            "tab\there",
            "",
            "$HOME",
            "semi;colon",
            "\u{2018}curly\u{2019}",
        ];
        let mut argv = vec![
            std::env::current_exe().unwrap().display().to_string(),
            "--ignored".to_owned(),
            "--exact".to_owned(),
            "live::replay::tests::argv_recorder".to_owned(),
            "--".to_owned(),
        ];
        argv.extend(expected.iter().map(|arg| (*arg).to_owned()));
        let rearm = [
            Rearm::Export("NGTERM_PWSH_INTEGRATION", integration.display().to_string()),
            Rearm::Unset("STALE"),
        ];
        let pwsh_path = pwsh.display().to_string();
        let shell = pwsh_shell(&rearm, &pwsh_path);
        let output = run_pwsh(&pwsh, &render_argv(&argv, &shell), &record);
        assert!(output.status.success(), "{output:?}");
        let recorded: serde_json::Value = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
        assert_eq!(
            recorded,
            serde_json::json!({
                "argv": expected,
                "loaded": integration.display().to_string(),
                "stale": null,
            })
        );
        // a profile's strict mode and legacy argument passing (which drops the empty argument and splits at the
        // quotes) change nothing, and the preference is the user's again after the command
        fs::remove_file(&record).unwrap();
        let preference = dir.path().join("preference");
        let script = format!(
            "Set-StrictMode -Version Latest ; $PSNativeCommandArgumentPassing = 'Legacy' ; {} ; \
             [System.IO.File]::WriteAllText({}, \"$PSNativeCommandArgumentPassing\")",
            render_argv(&argv, &shell),
            powershell_quoted(&preference.display().to_string())
        );
        let output = run_pwsh(&pwsh, &script, &record);
        assert!(output.status.success(), "{output:?}");
        let recorded: serde_json::Value = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
        assert_eq!(recorded["argv"], serde_json::json!(expected));
        assert_eq!(fs::read_to_string(&preference).unwrap(), "Legacy");
        // a durable line keeps its statements and PowerShell's own word parsing
        let line = format!(
            "$words = 'two words'; Set-Content -LiteralPath {} -Value ($words + ' and more') -NoNewline",
            powershell_quoted(&dir.path().join("line").display().to_string())
        );
        let output = run_pwsh(&pwsh, &render_command_line(&line, &shell), &record);
        assert!(output.status.success(), "{output:?}");
        assert_eq!(
            fs::read_to_string(dir.path().join("line")).unwrap(),
            "two words and more"
        );
    }

    /// The repository's PowerShell integration script.
    fn integration_script() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("resources")
            .join("shell-integration")
            .join("pwsh")
            .join("ngterm.ps1")
    }

    #[test]
    fn the_powershell_integration_loads_under_strict_mode_and_starts_the_prompt_ahead_of_its_host_output() {
        let Some(pwsh) = pwsh() else {
            eprintln!("SKIPPED: no pwsh on PATH");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        // a profile's strict mode, and its prompt writing to the host before it returns its text; then the prompt at
        // the start (no exit code yet) and after a command
        let script = format!(
            "Set-StrictMode -Version Latest ; \
             Microsoft.PowerShell.Utility\\Remove-Variable -Name LASTEXITCODE -Scope Global -ErrorAction Ignore ; \
             function global:prompt {{ Microsoft.PowerShell.Utility\\Write-Host -NoNewline 'HOST|' ; 'PS> ' }} ; \
             $env:NGTERM_PWSH_INTEGRATION = {} ; {PWSH_LOAD} ; {PWSH_LOAD} ; \
             if (-not (Microsoft.PowerShell.Utility\\Get-Variable -Name __NgtermIntegration -Scope Global \
             -ErrorAction Ignore)) {{ exit 3 }} ; \
             [System.Console]::Write((prompt)) ; [System.Console]::Write('|') ; \
             $global:__NgtermIntegration.Running = $true ; [System.Console]::Write((prompt))",
            powershell_quoted(&integration_script().display().to_string())
        );
        let output = run_pwsh(&pwsh, &script, &dir.path().join("unused"));
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{output:?}");
        // a strict-mode error is no failure of the script, only its message
        assert!(output.stderr.is_empty(), "{output:?}");
        assert!(!stdout.contains("did not load"), "{stdout:?}");
        let prompt = "\u{1b}]133;A\u{7}HOST|PS> \u{1b}]133;B\u{7}";
        let (first, second) = stdout
            .split_once(&format!("{prompt}|"))
            .unwrap_or_else(|| panic!("{stdout:?}"));
        assert!(first.contains("\u{1b}]7;kitty-shell-cwd://"), "{stdout:?}");
        assert!(!first.contains("133;D"), "no command ran: {stdout:?}");
        assert!(
            second.contains("\u{1b}]133;D;0\u{7}") && second.ends_with(prompt),
            "{stdout:?}"
        );
    }

    /// Not a test of its own: the program the PowerShell replay test runs, which records its arguments after `--`
    /// and the variables the replay set, as JSON in the file [`RECORD`] names.
    #[test]
    #[ignore = "the PowerShell replay test's child"]
    fn argv_recorder() {
        let Some(record) = std::env::var_os(RECORD) else {
            return;
        };
        let argv: Vec<String> = std::env::args().skip_while(|arg| arg != "--").skip(1).collect();
        let recorded = serde_json::json!({
            "argv": argv,
            "loaded": std::env::var("REPLAY_TEST_LOADED").ok(),
            "stale": std::env::var("STALE").ok(),
        });
        fs::write(record, recorded.to_string()).unwrap();
    }
}
