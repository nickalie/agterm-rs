//! The Ghostty shell integration a wrapped pane's login shell starts with, ported from Ghostty's
//! `src/termio/shell_integration.zig` (at the pinned `GHOSTTY_REV`). libghostty injects it only into a shell it spawns
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
