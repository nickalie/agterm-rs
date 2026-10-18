//! The Ghostty shell integration a wrapped pane's login shell starts with, ported from Ghostty's
//! `src/termio/shell_integration.zig` (at the pinned `LIBGHOSTTY_VT_REV`). libghostty injects it only into a shell it spawns
//! itself; a live pane's shell is the zmx daemon's child, so the wrap injects it the same way:
//!
//! - zsh as upstream agterm: `ZDOTDIR` names the integration, the app's own one handed on as `GHOSTTY_ZSH_ZDOTDIR`;
//! - bash in POSIX mode (`--posix`), whose interactive start reads `ENV`: the integration script, told by
//!   `GHOSTTY_BASH_INJECT` to rebuild bash's own startup sequence, the app's `ENV` kept as `GHOSTTY_BASH_ENV` and
//!   `HISTFILE` pinned to `~/.bash_history` (POSIX mode's default is `~/.sh_history`);
//! - fish through `XDG_DATA_DIRS`, the integration's `vendor_conf.d` ahead of the inherited (or the specification's
//!   default) directories, which the integration removes again.
//!
//! bash and fish are a Rust-only extension: upstream wraps zsh only (capability `live-any-shell`).

use std::collections::BTreeMap;
use std::path::Path;

use crate::command_restore::basename;
use crate::paths::{self, Env};

/// `XDG_DATA_DIRS` when unset, as the base directory specification defines it.
const DEFAULT_XDG_DATA_DIRS: &str = "/usr/local/share:/usr/share";

/// A login shell a pane can be wrapped in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginShell {
    Zsh,
    Bash,
    Fish,
}

impl LoginShell {
    /// The shell `path` names, by its basename; `None` for any other shell.
    #[must_use]
    pub fn of(path: &str) -> Option<Self> {
        match basename(path) {
            "zsh" => Some(Self::Zsh),
            "bash" => Some(Self::Bash),
            "fish" => Some(Self::Fish),
            _ => None,
        }
    }

    /// The shell's name, as the integration directories are named.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Zsh => "zsh",
            Self::Bash => "bash",
            Self::Fish => "fish",
        }
    }

    /// The integration file under the resources directory whose presence means the integration is bundled.
    fn loader(self) -> &'static str {
        match self {
            Self::Zsh => "shell-integration/zsh/.zshenv",
            Self::Bash => "shell-integration/bash/ghostty.bash",
            Self::Fish => "shell-integration/fish/vendor_conf.d/ghostty-shell-integration.fish",
        }
    }

    /// Whether `resources` (a Ghostty resources directory) bundles this shell's integration.
    #[must_use]
    pub fn is_bundled(self, resources: &Path) -> bool {
        resources.join(self.loader()).exists()
    }

    /// The flags of the interactive login shell a replay ends in (`exec <shell> <flags>`).
    #[must_use]
    pub fn interactive_flags(self) -> &'static [&'static str] {
        match self {
            Self::Zsh | Self::Fish => &["-il"],
            Self::Bash => &["--posix", "-il"],
        }
    }

    /// The flags of the interactive login shell that runs a replay script (`<shell> <flags> <script>`).
    #[must_use]
    pub fn script_flags(self) -> &'static [&'static str] {
        match self {
            Self::Zsh | Self::Fish => &["-lic"],
            Self::Bash => &["--posix", "-lic"],
        }
    }

    /// The program a new daemon runs when the attach carries nothing to replay. zmx itself starts `$SHELL` as a login
    /// shell (`-zsh`, `-fish`), which zsh and fish need; bash reads `ENV` only in POSIX mode, a flag zmx never passes.
    #[must_use]
    pub fn login_arguments(self, shell: &str) -> Vec<String> {
        match self {
            Self::Zsh | Self::Fish => Vec::new(),
            Self::Bash => vec![shell.to_owned(), "--posix".to_owned(), "-l".to_owned()],
        }
    }

    /// Whether a replay script is written in fish's syntax rather than the POSIX one zsh and bash share.
    #[must_use]
    pub fn is_fish(self) -> bool {
        self == Self::Fish
    }
}

/// The app's own values of the variables an integration hands on or extends, read once from its environment.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Inherited {
    /// `ZDOTDIR`, handed on to the zsh integration.
    pub zdotdir: Option<String>,
    /// `ENV`, which the bash integration restores.
    pub env: Option<String>,
    /// `HISTFILE`; without one bash's history is pinned to `<home>/.bash_history`.
    pub histfile: Option<String>,
    /// The home directory (`HOME`, else the password database's).
    pub home: Option<String>,
    /// `XDG_DATA_DIRS`, which the fish integration's directory goes in front of.
    pub xdg_data_dirs: Option<String>,
}

impl Inherited {
    /// The values in `env`.
    #[must_use]
    pub fn read(env: &dyn Env) -> Self {
        let var = |key: &str| env.var(key).map(|value| value.to_string_lossy().into_owned());
        Self {
            zdotdir: var("ZDOTDIR"),
            env: var("ENV"),
            histfile: var("HISTFILE"),
            home: paths::home_dir(env).map(|home| home.to_string_lossy().into_owned()),
            xdg_data_dirs: var("XDG_DATA_DIRS"),
        }
    }
}

/// Sets `shell`'s integration variables in `environment`, the integration under `resources` (a Ghostty resources
/// directory, standardized).
pub fn inject(shell: LoginShell, resources: &Path, inherited: &Inherited, environment: &mut BTreeMap<String, String>) {
    let integration = resources.join("shell-integration");
    let mut set = |key: &str, value: String| environment.insert(key.to_owned(), value);
    match shell {
        LoginShell::Zsh => {
            set("ZDOTDIR", integration.join("zsh").to_string_lossy().into_owned());
            if let Some(zdotdir) = &inherited.zdotdir {
                set("GHOSTTY_ZSH_ZDOTDIR", zdotdir.clone());
            }
        }
        LoginShell::Bash => {
            if let Some(env) = &inherited.env {
                set("GHOSTTY_BASH_ENV", env.clone());
            }
            set(
                "ENV",
                integration.join("bash/ghostty.bash").to_string_lossy().into_owned(),
            );
            set("GHOSTTY_BASH_INJECT", "1".to_owned());
            if inherited.histfile.is_none()
                && let Some(home) = &inherited.home
            {
                set("HISTFILE", format!("{home}/.bash_history"));
                set("GHOSTTY_BASH_UNEXPORT_HISTFILE", "1".to_owned());
            }
        }
        LoginShell::Fish => {
            let integration = integration.to_string_lossy().into_owned();
            let current = inherited.xdg_data_dirs.as_deref().unwrap_or(DEFAULT_XDG_DATA_DIRS);
            let data_dirs = if current.is_empty() {
                integration.clone()
            } else {
                format!("{integration}:{current}")
            };
            set("GHOSTTY_SHELL_INTEGRATION_XDG_DIR", integration);
            set("XDG_DATA_DIRS", data_dirs);
        }
    }
}

/// What libghostty's automatic integration (`shell_integration.setup`, `shell-integration = detect`) makes of a plain
/// pane's command line, a shell line its terminal runs through `/bin/sh -c`: when its program is a zsh, bash or fish
/// whose integration `resources` bundles, that shell's variables are set in `environment` and the line to run comes
/// back — bash's in POSIX mode (`<exe> --posix <args>`), its `--norc`/`--noprofile` handed to the integration through
/// `GHOSTTY_BASH_INJECT` and its `--rcfile`/`--init-file` through `GHOSTTY_BASH_RCFILE`, the others unchanged.
/// `None`, with `environment` untouched, for any other program, an integration that is not bundled, a bash given
/// `--posix` or `-c` (no interactive start to hook), and a line with quotes or escapes, which this split does not parse
/// (Ghostty's does; such a line runs without the integration). Ghostty's elvish and nushell integrations are not
/// ported.
#[must_use]
pub fn setup(
    command: &str,
    resources: &Path,
    inherited: &Inherited,
    environment: &mut BTreeMap<String, String>,
) -> Option<String> {
    setup_as(None, command, resources, inherited, environment)
}

/// [`setup`] with the shell `force`d (`shell-integration = bash`, `fish` or `zsh`): that shell's scheme whatever the
/// program's name, as libghostty's `setup` takes a forced shell over the one it would detect.
#[must_use]
pub fn setup_as(
    force: Option<LoginShell>,
    command: &str,
    resources: &Path,
    inherited: &Inherited,
    environment: &mut BTreeMap<String, String>,
) -> Option<String> {
    if command.contains(['\'', '"', '\\']) {
        return None;
    }
    let mut words = command.split_whitespace();
    let exe = words.next()?;
    let shell = force
        .or_else(|| LoginShell::of(exe))
        .filter(|shell| shell.is_bundled(resources))?;
    let (line, flags) = match shell {
        LoginShell::Bash => {
            let (line, flags) = bash_command(exe, words)?;
            (line, Some(flags))
        }
        LoginShell::Zsh | LoginShell::Fish => (command.to_owned(), None),
    };
    inject(shell, resources, inherited, environment);
    if let Some(BashFlags { inject, rcfile }) = flags {
        environment.insert("GHOSTTY_BASH_INJECT".to_owned(), inject);
        if let Some(rcfile) = rcfile {
            environment.insert("GHOSTTY_BASH_RCFILE".to_owned(), rcfile);
        }
    }
    Some(line)
}

/// What a bash command line's own flags hand to the integration.
struct BashFlags {
    /// `GHOSTTY_BASH_INJECT`: `1`, then the startup files the user turned off.
    inject: String,
    rcfile: Option<String>,
}

/// Ghostty's `setupBash` walk over bash's arguments after `exe`: the POSIX-mode line and the flags for the
/// integration, `None` when `--posix` or a short option with `c` rules the integration out.
fn bash_command<'a>(exe: &'a str, mut words: impl Iterator<Item = &'a str>) -> Option<(String, BashFlags)> {
    let mut line = vec![exe, "--posix"];
    let mut flags = BashFlags {
        inject: "1".to_owned(),
        rcfile: None,
    };
    while let Some(word) = words.next() {
        match word {
            "--posix" => return None,
            "--norc" | "--noprofile" => {
                flags.inject.push(' ');
                flags.inject.push_str(word);
            }
            "--rcfile" | "--init-file" => flags.rcfile = words.next().map(str::to_owned),
            "-" | "--" => {
                line.push(word);
                line.extend(words.by_ref());
            }
            short if short.len() > 1 && short.starts_with('-') && !short.starts_with("--") => {
                if short.contains('c') {
                    return None;
                }
                line.push(short);
            }
            other => line.push(other),
        }
    }
    Some((line.join(" "), flags))
}

/// One step of re-arming the integration before a replay's final shell: export the variable with this value, or
/// unset it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rearm {
    Export(&'static str, String),
    Unset(&'static str),
}

/// What a replay re-arms before it execs the final shell, read off the wrap's `environment`: the integration consumed
/// its variables in the shell that ran the command, so the final shell would start without it. `None` when the
/// environment carries no integration for `shell`.
#[must_use]
pub fn rearm(shell: LoginShell, environment: &BTreeMap<String, String>) -> Option<Vec<Rearm>> {
    let required = |key: &'static str| environment.get(key).map(|value| Rearm::Export(key, value.clone()));
    let optional = |key: &'static str| required(key).unwrap_or(Rearm::Unset(key));
    let steps = match shell {
        LoginShell::Zsh => vec![required("ZDOTDIR")?, optional("GHOSTTY_ZSH_ZDOTDIR")],
        LoginShell::Bash => {
            let mut steps = vec![
                required("ENV")?,
                required("GHOSTTY_BASH_INJECT")?,
                optional("GHOSTTY_BASH_ENV"),
            ];
            // only a HISTFILE the wrap pinned: the user's own one stays as their startup files leave it
            if let (Some(histfile), Some(unexport)) = (required("HISTFILE"), required("GHOSTTY_BASH_UNEXPORT_HISTFILE"))
            {
                steps.extend([histfile, unexport]);
            }
            steps
        }
        LoginShell::Fish => vec![
            required("XDG_DATA_DIRS")?,
            required("GHOSTTY_SHELL_INTEGRATION_XDG_DIR")?,
        ],
    };
    Some(steps)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use crate::paths::MapEnv;

    use super::*;

    fn injected(shell: LoginShell, inherited: &Inherited) -> BTreeMap<String, String> {
        let mut environment = BTreeMap::from([("AGTERM_ENABLED".to_owned(), "1".to_owned())]);
        inject(shell, Path::new("/res"), inherited, &mut environment);
        environment
    }

    fn pairs(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn a_login_shell_is_known_by_its_basename() {
        assert_eq!(LoginShell::of("/bin/zsh"), Some(LoginShell::Zsh));
        assert_eq!(LoginShell::of("/usr/local/bin/bash"), Some(LoginShell::Bash));
        assert_eq!(LoginShell::of("fish"), Some(LoginShell::Fish));
        for other in ["/bin/sh", "/bin/tcsh", "/usr/bin/nu", "/bin/zsh5", ""] {
            assert_eq!(LoginShell::of(other), None, "{other}");
        }
    }

    #[test]
    fn each_shell_is_bundled_by_its_own_loader() {
        let resources = tempfile::tempdir().unwrap();
        for shell in [LoginShell::Zsh, LoginShell::Bash, LoginShell::Fish] {
            assert!(!shell.is_bundled(resources.path()), "{}", shell.name());
        }
        for loader in [
            "shell-integration/zsh/.zshenv",
            "shell-integration/bash/ghostty.bash",
            "shell-integration/fish/vendor_conf.d/ghostty-shell-integration.fish",
        ] {
            let path = resources.path().join(loader);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "").unwrap();
        }
        for shell in [LoginShell::Zsh, LoginShell::Bash, LoginShell::Fish] {
            assert!(shell.is_bundled(resources.path()), "{}", shell.name());
        }
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "the zsh, bash and fish integrations are Unix shells' and spelled with /; a Windows live pane runs PowerShell"
    )]
    fn zsh_points_zdotdir_at_the_integration_and_hands_the_apps_own_on() {
        assert_eq!(
            injected(LoginShell::Zsh, &Inherited::default()),
            pairs(&[("AGTERM_ENABLED", "1"), ("ZDOTDIR", "/res/shell-integration/zsh")])
        );
        let inherited = Inherited {
            zdotdir: Some("/home/u/.zsh".to_owned()),
            ..Inherited::default()
        };
        assert_eq!(
            injected(LoginShell::Zsh, &inherited),
            pairs(&[
                ("AGTERM_ENABLED", "1"),
                ("GHOSTTY_ZSH_ZDOTDIR", "/home/u/.zsh"),
                ("ZDOTDIR", "/res/shell-integration/zsh"),
            ])
        );
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "the zsh, bash and fish integrations are Unix shells' and spelled with /; a Windows live pane runs PowerShell"
    )]
    fn bash_reads_the_integration_through_env_and_keeps_the_apps_env_and_history() {
        let inherited = Inherited {
            home: Some("/home/u".to_owned()),
            ..Inherited::default()
        };
        assert_eq!(
            injected(LoginShell::Bash, &inherited),
            pairs(&[
                ("AGTERM_ENABLED", "1"),
                ("ENV", "/res/shell-integration/bash/ghostty.bash"),
                ("GHOSTTY_BASH_INJECT", "1"),
                ("GHOSTTY_BASH_UNEXPORT_HISTFILE", "1"),
                ("HISTFILE", "/home/u/.bash_history"),
            ])
        );
        // an inherited ENV is restored by the integration, an inherited HISTFILE left alone
        let inherited = Inherited {
            env: Some("/home/u/.shrc".to_owned()),
            histfile: Some("/home/u/.hist".to_owned()),
            home: Some("/home/u".to_owned()),
            ..Inherited::default()
        };
        assert_eq!(
            injected(LoginShell::Bash, &inherited),
            pairs(&[
                ("AGTERM_ENABLED", "1"),
                ("ENV", "/res/shell-integration/bash/ghostty.bash"),
                ("GHOSTTY_BASH_ENV", "/home/u/.shrc"),
                ("GHOSTTY_BASH_INJECT", "1"),
            ])
        );
        // no home to pin the history to: POSIX mode's default stands, as Ghostty's
        let homeless = injected(LoginShell::Bash, &Inherited::default());
        assert!(!homeless.contains_key("HISTFILE") && !homeless.contains_key("GHOSTTY_BASH_UNEXPORT_HISTFILE"));
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "the zsh, bash and fish integrations are Unix shells' and spelled with /; a Windows live pane runs PowerShell"
    )]
    fn fish_finds_the_integration_ahead_of_the_inherited_data_dirs() {
        let with = |dirs: Option<&str>| Inherited {
            xdg_data_dirs: dirs.map(str::to_owned),
            ..Inherited::default()
        };
        assert_eq!(
            injected(LoginShell::Fish, &with(None)),
            pairs(&[
                ("AGTERM_ENABLED", "1"),
                ("GHOSTTY_SHELL_INTEGRATION_XDG_DIR", "/res/shell-integration"),
                ("XDG_DATA_DIRS", "/res/shell-integration:/usr/local/share:/usr/share"),
            ])
        );
        let data_dirs = |dirs| injected(LoginShell::Fish, &with(dirs))["XDG_DATA_DIRS"].clone();
        assert_eq!(data_dirs(Some("/opt/share")), "/res/shell-integration:/opt/share");
        assert_eq!(data_dirs(Some("")), "/res/shell-integration");
    }

    #[test]
    fn the_inherited_values_come_from_the_environment_and_the_home_from_the_password_database_too() {
        let env = MapEnv::new()
            .with("ZDOTDIR", "/z")
            .with("ENV", "/e")
            .with("HISTFILE", "/h")
            .with("XDG_DATA_DIRS", "/x")
            .with_passwd_home("/passwd/home");
        assert_eq!(
            Inherited::read(&env),
            Inherited {
                zdotdir: Some("/z".to_owned()),
                env: Some("/e".to_owned()),
                histfile: Some("/h".to_owned()),
                home: Some("/passwd/home".to_owned()),
                xdg_data_dirs: Some("/x".to_owned()),
            }
        );
        let home = Inherited::read(&MapEnv::new().with("HOME", "/home/u"));
        assert_eq!(home.home.map(PathBuf::from), Some(PathBuf::from("/home/u")));
        assert_eq!(home.zdotdir, None);
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "the zsh, bash and fish integrations are Unix shells' and spelled with /; a Windows live pane runs PowerShell"
    )]
    fn the_rearm_exports_what_the_integration_consumed() {
        let zsh = rearm(LoginShell::Zsh, &injected(LoginShell::Zsh, &Inherited::default())).unwrap();
        assert_eq!(
            zsh,
            [
                Rearm::Export("ZDOTDIR", "/res/shell-integration/zsh".to_owned()),
                Rearm::Unset("GHOSTTY_ZSH_ZDOTDIR")
            ]
        );
        let inherited = Inherited {
            home: Some("/home/u".to_owned()),
            ..Inherited::default()
        };
        let bash = rearm(LoginShell::Bash, &injected(LoginShell::Bash, &inherited)).unwrap();
        assert_eq!(
            bash,
            [
                Rearm::Export("ENV", "/res/shell-integration/bash/ghostty.bash".to_owned()),
                Rearm::Export("GHOSTTY_BASH_INJECT", "1".to_owned()),
                Rearm::Unset("GHOSTTY_BASH_ENV"),
                Rearm::Export("HISTFILE", "/home/u/.bash_history".to_owned()),
                Rearm::Export("GHOSTTY_BASH_UNEXPORT_HISTFILE", "1".to_owned()),
            ]
        );
        let fish = rearm(LoginShell::Fish, &injected(LoginShell::Fish, &Inherited::default())).unwrap();
        assert_eq!(
            fish,
            [
                Rearm::Export(
                    "XDG_DATA_DIRS",
                    "/res/shell-integration:/usr/local/share:/usr/share".to_owned()
                ),
                Rearm::Export("GHOSTTY_SHELL_INTEGRATION_XDG_DIR", "/res/shell-integration".to_owned()),
            ]
        );
        // a wrap without the integration has nothing to re-arm
        for shell in [LoginShell::Zsh, LoginShell::Bash, LoginShell::Fish] {
            assert_eq!(rearm(shell, &BTreeMap::new()), None, "{}", shell.name());
        }
    }

    /// A resources directory bundling every integration.
    fn bundled() -> tempfile::TempDir {
        let resources = tempfile::tempdir().unwrap();
        for shell in [LoginShell::Zsh, LoginShell::Bash, LoginShell::Fish] {
            let path = resources.path().join(shell.loader());
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "").unwrap();
        }
        resources
    }

    /// [`setup`] of `command` over a bundled integration and a home: the line, and the variables it set.
    fn set_up(resources: &Path, command: &str) -> (Option<String>, BTreeMap<String, String>) {
        let inherited = Inherited {
            home: Some("/home/u".to_owned()),
            ..Inherited::default()
        };
        let mut environment = BTreeMap::new();
        let line = setup(command, resources, &inherited, &mut environment);
        (line, environment)
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "the zsh, bash and fish integrations are Unix shells' and spelled with /; a Windows live pane runs PowerShell"
    )]
    fn a_plain_panes_zsh_and_fish_run_as_given_with_their_integration() {
        let resources = bundled();
        let (line, environment) = set_up(resources.path(), "/usr/bin/zsh");
        assert_eq!(line.as_deref(), Some("/usr/bin/zsh"));
        assert!(environment["ZDOTDIR"].ends_with("shell-integration/zsh"));
        let (line, environment) = set_up(resources.path(), "fish -l");
        assert_eq!(line.as_deref(), Some("fish -l"));
        assert!(environment.contains_key("GHOSTTY_SHELL_INTEGRATION_XDG_DIR"));
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "the zsh, bash and fish integrations are Unix shells' and spelled with /; a Windows live pane runs PowerShell"
    )]
    fn a_plain_panes_bash_goes_to_posix_mode_its_startup_flags_to_the_integration() {
        // Ghostty's `setupBash` cases
        let resources = bundled();
        let (line, environment) = set_up(resources.path(), "/bin/bash");
        assert_eq!(line.as_deref(), Some("/bin/bash --posix"));
        assert_eq!(
            (
                environment["GHOSTTY_BASH_INJECT"].as_str(),
                environment["HISTFILE"].as_str()
            ),
            ("1", "/home/u/.bash_history")
        );
        assert!(environment["ENV"].ends_with("shell-integration/bash/ghostty.bash"));
        let (line, environment) = set_up(resources.path(), "bash --norc --noprofile -i");
        assert_eq!(line.as_deref(), Some("bash --posix -i"));
        assert_eq!(environment["GHOSTTY_BASH_INJECT"], "1 --norc --noprofile");
        let (line, environment) = set_up(resources.path(), "bash --rcfile script.sh -l");
        assert_eq!(line.as_deref(), Some("bash --posix -l"));
        assert_eq!(environment["GHOSTTY_BASH_RCFILE"], "script.sh");
        let (line, _) = set_up(resources.path(), "bash - --arg file1 --posix");
        assert_eq!(line.as_deref(), Some("bash --posix - --arg file1 --posix"));
    }

    #[test]
    fn a_plain_pane_without_an_integrated_shell_runs_as_given() {
        // bash that cannot start interactively, other programs, quoting, nothing bundled
        let resources = bundled();
        let run = |command: &str| set_up(resources.path(), command);
        for command in [
            "bash --posix",
            "bash -c 'ls'",
            "bash -lc ls",
            "vim notes",
            "sh",
            "\"/a b/bash\"",
            "",
        ] {
            assert_eq!(run(command), (None, BTreeMap::new()), "{command}");
        }
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(set_up(empty.path(), "zsh"), (None, BTreeMap::new()));
    }

    #[test]
    fn bash_alone_needs_posix_mode_flags() {
        assert_eq!(LoginShell::Zsh.login_arguments("/bin/zsh"), Vec::<String>::new());
        assert_eq!(LoginShell::Fish.login_arguments("/bin/fish"), Vec::<String>::new());
        assert_eq!(
            LoginShell::Bash.login_arguments("/bin/bash"),
            ["/bin/bash", "--posix", "-l"]
        );
        assert_eq!(LoginShell::Bash.script_flags(), ["--posix", "-lic"]);
        assert_eq!(LoginShell::Bash.interactive_flags(), ["--posix", "-il"]);
        assert_eq!(LoginShell::Fish.script_flags(), ["-lic"]);
        assert_eq!(LoginShell::Zsh.interactive_flags(), ["-il"]);
    }
}
