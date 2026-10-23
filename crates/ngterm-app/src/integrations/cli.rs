//! The command line tool installer (upstream's `CLIInstall.swift` and `CLIInstaller.swift`), with the Linux path:
//! the bundled `ngtermctl` is symlinked into `~/.local/bin`, the per-user executables directory most distributions put
//! on the `PATH`, so no admin prompt is needed (upstream links into `/usr/local/bin`, through an admin prompt when
//! that directory is root-owned). An earlier link is replaced; a real file there (a package's or a hand-installed
//! binary) is the user's and is left alone.
//!
//! The CLI is `ngtermctl`, not upstream's `agtermctl`: the two CLIs resolve different default sockets, so this build
//! neither takes nor touches a `~/.local/bin/agtermctl` an agterm install put there.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use super::Environment;

/// The bundled tool's executable name, the link the install puts on the `PATH` and the command the user runs.
pub const TOOL_NAME: &str = "ngtermctl";

/// The directory the tool is linked into.
#[must_use]
pub fn install_directory(home: &Path) -> PathBuf {
    home.join(".local/bin")
}

/// What is at the link's path now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkStatus {
    Absent,
    /// A link already pointing at the bundled tool.
    Current,
    /// A link pointing elsewhere (an earlier build's, a moved checkout's): replaced.
    OtherLink(PathBuf),
    /// A file or directory that is not a link: the user's.
    Foreign,
}

/// The install, decided before it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliPlan {
    Link {
        source: PathBuf,
        link: PathBuf,
        status: LinkStatus,
    },
    Failed(String),
}

/// The link's current state.
#[must_use]
pub fn link_status(link: &Path, source: &Path) -> LinkStatus {
    let Ok(meta) = fs::symlink_metadata(link) else {
        return LinkStatus::Absent;
    };
    if !meta.file_type().is_symlink() {
        // the tool installed in place, e.g. a personal install that put the binary right there
        let same = fs::canonicalize(link).ok() == fs::canonicalize(source).ok();
        return if same { LinkStatus::Current } else { LinkStatus::Foreign };
    }
    match fs::read_link(link) {
        Ok(target) if target == source => LinkStatus::Current,
        Ok(target) => LinkStatus::OtherLink(target),
        Err(_) => LinkStatus::OtherLink(PathBuf::new()),
    }
}

/// Plans the install over `env`'s home.
#[must_use]
pub fn plan(env: &Environment) -> CliPlan {
    let Some(source) = env.bundled_cli.clone() else {
        return CliPlan::Failed(format!("{TOOL_NAME} is not bundled in this build."));
    };
    let link = install_directory(&env.home).join(TOOL_NAME);
    let status = link_status(&link, &source);
    if status == LinkStatus::Foreign {
        return CliPlan::Failed(format!(
            "{} exists and is not a link ngterm made, so ngterm left it untouched. Remove it and run this again.",
            link.display()
        ));
    }
    CliPlan::Link { source, link, status }
}

/// What the install did (upstream's `InstallResult`; Linux has no admin prompt to cancel).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliResult {
    Installed { link: PathBuf, on_path: bool },
    Failed(String),
}

/// Carries `plan` out: the directory is created, an earlier link replaced. `path` is the `PATH` the result checks the
/// directory against.
#[must_use]
pub fn apply(plan: &CliPlan, path: Option<&str>) -> CliResult {
    let (source, link, status) = match plan {
        CliPlan::Failed(message) => return CliResult::Failed(message.clone()),
        CliPlan::Link { source, link, status } => (source, link, status),
    };
    let linked = (|| {
        if *status == LinkStatus::Current {
            return Ok(());
        }
        if let Some(directory) = link.parent() {
            fs::create_dir_all(directory)?;
        }
        if matches!(status, LinkStatus::OtherLink(_)) {
            fs::remove_file(link)?;
        }
        symlink(source, link)
    })();
    match linked {
        Ok(()) => CliResult::Installed {
            link: link.clone(),
            on_path: link.parent().is_some_and(|directory| on_path(directory, path)),
        },
        Err(err) => CliResult::Failed(format!("Could not link {}: {err}", link.display())),
    }
}

/// Whether `directory` is an entry of `path`.
fn on_path(directory: &Path, path: Option<&str>) -> bool {
    path.unwrap_or_default()
        .split(':')
        .any(|entry| !entry.is_empty() && Path::new(entry) == directory)
}

impl CliResult {
    #[must_use]
    pub fn is_warning(&self) -> bool {
        !matches!(self, Self::Installed { on_path: true, .. })
    }

    #[must_use]
    pub fn title(&self) -> &'static str {
        match self {
            Self::Installed { .. } => "Command Line Tool Installed",
            Self::Failed(_) => "Install Failed",
        }
    }

    /// The alert text (upstream's), plus a note when the directory is not on the `PATH` yet.
    #[must_use]
    pub fn text(&self) -> String {
        match self {
            Self::Failed(message) => message.clone(),
            Self::Installed { link, on_path } => {
                let text = format!(
                    "{TOOL_NAME} was linked into {}. Open a new terminal and run “{TOOL_NAME} --help”.",
                    link.display()
                );
                match link.parent().filter(|_| !on_path) {
                    Some(directory) => format!(
                        "{text} {} is not on your PATH yet: add it in your shell's startup file.",
                        directory.display()
                    ),
                    None => text,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(home: &Path, bundled: Option<PathBuf>) -> Environment {
        Environment {
            home: home.to_path_buf(),
            agent_status: None,
            skill: None,
            bundled_cli: bundled,
            vars: std::collections::HashMap::new(),
        }
    }

    fn bundled_tool(dir: &Path) -> PathBuf {
        let tool = dir.join("stage/bin/ngtermctl");
        fs::create_dir_all(tool.parent().unwrap()).unwrap();
        fs::write(&tool, "#!/bin/sh\n").unwrap();
        tool
    }

    #[test]
    fn a_fresh_home_gets_the_link_and_a_second_run_is_current() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let tool = bundled_tool(dir.path());
        let environment = env(&home, Some(tool.clone()));
        let link = home.join(".local/bin/ngtermctl");

        let first = plan(&environment);
        assert_eq!(
            first,
            CliPlan::Link {
                source: tool.clone(),
                link: link.clone(),
                status: LinkStatus::Absent
            }
        );
        assert!(!link.parent().unwrap().exists(), "planning writes nothing");
        let path = format!("/usr/bin:{}", home.join(".local/bin").display());
        let result = apply(&first, Some(&path));
        assert_eq!(
            result,
            CliResult::Installed {
                link: link.clone(),
                on_path: true
            }
        );
        assert_eq!(fs::read_link(&link).unwrap(), tool);
        assert!(!result.is_warning());

        let second = plan(&environment);
        assert!(matches!(
            second,
            CliPlan::Link {
                status: LinkStatus::Current,
                ..
            }
        ));
        assert_eq!(apply(&second, Some(&path)), result);
    }

    #[test]
    fn an_earlier_link_is_replaced_and_a_real_file_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let tool = bundled_tool(dir.path());
        let link = home.join(".local/bin/ngtermctl");
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        symlink("/old/build/ngtermctl", &link).unwrap();

        let planned = plan(&env(&home, Some(tool.clone())));
        assert!(
            matches!(&planned, CliPlan::Link { status: LinkStatus::OtherLink(old), .. } if old == Path::new("/old/build/ngtermctl"))
        );
        let result = apply(&planned, None);
        assert_eq!(fs::read_link(&link).unwrap(), tool);
        assert!(result.text().contains("is not on your PATH yet"), "{}", result.text());

        fs::remove_file(&link).unwrap();
        fs::write(&link, "a package's binary").unwrap();
        let refused = plan(&env(&home, Some(tool)));
        assert!(matches!(&refused, CliPlan::Failed(message) if message.contains("is not a link ngterm made")));
        assert_eq!(apply(&refused, None).title(), "Install Failed");
        assert_eq!(fs::read_to_string(&link).unwrap(), "a package's binary");
    }

    #[test]
    fn an_agtermctl_link_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let tool = bundled_tool(dir.path());
        let agtermctl = home.join(".local/bin/agtermctl");
        fs::create_dir_all(agtermctl.parent().unwrap()).unwrap();
        symlink("/opt/agterm/bin/agtermctl", &agtermctl).unwrap();

        let planned = plan(&env(&home, Some(tool.clone())));
        assert!(
            matches!(&planned, CliPlan::Link { link, status: LinkStatus::Absent, .. } if link.ends_with("ngtermctl"))
        );
        assert!(matches!(apply(&planned, None), CliResult::Installed { .. }));
        assert_eq!(fs::read_link(home.join(".local/bin/ngtermctl")).unwrap(), tool);
        assert_eq!(
            fs::read_link(&agtermctl).unwrap(),
            Path::new("/opt/agterm/bin/agtermctl")
        );
    }

    #[test]
    fn a_build_without_the_tool_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let result = apply(&plan(&env(dir.path(), None)), None);
        assert_eq!(
            result,
            CliResult::Failed("ngtermctl is not bundled in this build.".to_owned())
        );
    }

    #[test]
    fn the_installed_text_is_upstreams() {
        let result = CliResult::Installed {
            link: "/home/me/.local/bin/ngtermctl".into(),
            on_path: true,
        };
        assert_eq!(
            result.text(),
            "ngtermctl was linked into /home/me/.local/bin/ngtermctl. Open a new terminal and run “ngtermctl --help”."
        );
        assert_eq!(result.title(), "Command Line Tool Installed");
    }
}
