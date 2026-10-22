//! `OpenCode`'s status plugins (upstream's `AgentHooksInstall.OpenCode` and `OpenCodeEnvironmentProbe.swift`): the
//! versioned paths and ownership policy, and the login-shell probe that finds the installed major version and the
//! configuration directory `OpenCode` 2 reads.

use std::collections::HashMap;
use std::hash::BuildHasher;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A supported `OpenCode` major version, each with its own plugin entry point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Version {
    V1,
    V2,
}

impl Version {
    pub const ALL: [Self; 2] = [Self::V1, Self::V2];

    /// `v1` / `v2`, as the alerts name it.
    #[must_use]
    pub fn raw(self) -> &'static str {
        match self {
            Self::V1 => "v1",
            Self::V2 => "v2",
        }
    }

    /// The major version of `opencode --version` output: a semantic version, optionally prefixed with `opencode ` or
    /// `v`, majors 1 and 2 only; anything else (two lines, a word between) is not a version.
    #[must_use]
    pub fn from_version_output(output: &str) -> Option<Self> {
        let output = output.trim();
        let output = output.strip_prefix("opencode ").unwrap_or(output);
        let version = output.strip_prefix('v').unwrap_or(output);
        if !is_semantic_version(version) {
            return None;
        }
        match version.split('.').next() {
            Some("1") => Some(Self::V1),
            Some("2") => Some(Self::V2),
            _ => None,
        }
    }

    /// The plugin's path inside the bundled agent-status package.
    #[must_use]
    pub fn relative_path(self) -> String {
        format!("opencode/{}", self.plugin_name())
    }

    /// The ownership sentinel of this version's plugin.
    #[must_use]
    pub fn marker(self) -> &'static str {
        match self {
            Self::V1 => "// ngterm-opencode-status-plugin",
            Self::V2 => "// ngterm-opencode-v2-status-plugin",
        }
    }

    /// The plugin's destination in a resolved configuration directory.
    #[must_use]
    pub fn plugin_path(self, configuration_directory: &Path) -> PathBuf {
        configuration_directory.join("plugins").join(self.plugin_name())
    }

    /// Whether the destination may be replaced: absent, or carrying this version's marker; an unreadable file is
    /// the user's.
    #[must_use]
    pub fn may_overwrite(self, file_exists: bool, existing: Option<&str>) -> bool {
        !file_exists || existing.is_some_and(|text| text.contains(self.marker()))
    }

    fn plugin_name(self) -> &'static str {
        match self {
            Self::V1 => "ngterm-status.js",
            Self::V2 => "ngterm-v2/tui.js",
        }
    }

    /// Where an install from before the build took its own name put this version's plugin, and the marker it
    /// carries there: a reinstall removes such a marked plugin, since `OpenCode` would load it beside the current
    /// one. A one-time migration: remove it once no install from before the rename is left to migrate (the first
    /// reinstall past it has run).
    #[must_use]
    pub fn legacy_plugin(self, configuration_directory: &Path) -> (PathBuf, &'static str) {
        let (name, marker) = match self {
            Self::V1 => ("agterm-status.js", "// agterm-opencode-status-plugin"),
            Self::V2 => ("agterm-v2/tui.js", "// agterm-opencode-v2-status-plugin"),
        };
        (configuration_directory.join("plugins").join(name), marker)
    }
}

/// `MAJOR.MINOR.PATCH`, then an optional `-pre` and `+build` of `[0-9A-Za-z.-]`.
fn is_semantic_version(text: &str) -> bool {
    let ident = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '.' || ch == '-')
    };
    let (rest, build) = match text.split_once('+') {
        Some((rest, build)) => (rest, Some(build)),
        None => (text, None),
    };
    if build.is_some_and(|build| !ident(build)) {
        return false;
    }
    let digits_end = rest
        .char_indices()
        .find(|&(_, ch)| ch == '-')
        .map_or(rest.len(), |(index, _)| index);
    let (core, pre) = rest.split_at(digits_end);
    if !pre.is_empty() && !ident(&pre[1..]) {
        return false;
    }
    let numbers: Vec<&str> = core.split('.').collect();
    numbers.len() == 3
        && numbers
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|ch| ch.is_ascii_digit()))
}

/// The `OpenCode` configuration directory: `OPENCODE_CONFIG_DIR` when set (set but empty: none), else
/// `$XDG_CONFIG_HOME/opencode`, else `<home>/.config/opencode`; a relative result is made absolute against
/// `working_directory` and normalized, when one is given (upstream's `OpenCode.configurationDirectory`).
#[must_use]
pub fn configuration_directory(
    home: &str,
    opencode_config_directory: Option<&str>,
    xdg_config_home: Option<&str>,
    working_directory: Option<&str>,
) -> Option<String> {
    let path = match opencode_config_directory {
        Some("") => return None,
        Some(chosen) => chosen.to_owned(),
        None => {
            let base = xdg_config_home
                .filter(|xdg| !xdg.is_empty())
                .map_or_else(|| format!("{home}/.config"), str::to_owned);
            format!("{base}/opencode")
        }
    };
    let Some(working_directory) = working_directory else {
        return Some(path);
    };
    let absolute = if path.starts_with('/') {
        path
    } else {
        format!("{working_directory}/{path}")
    };
    Some(normalize(Path::new(&absolute)).to_string_lossy().into_owned())
}

/// `.` and `..` resolved lexically, as Foundation's `standardizedFileURL`.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// What the login-shell probe found: the major version `opencode --version` reported, and the configuration directory
/// `OpenCode` 2 reads there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub version: Option<Version>,
    pub v2_configuration_directory: Option<String>,
}

/// How long the probe waits for the login shell.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Runs `opencode --version` in the user's interactive login shell (`$SHELL`, else `/bin/sh`), with
/// `NGTERM_SESSION_ID` dropped, reading the exported `HOME`, `PWD`, `OPENCODE_CONFIG_DIR` and `XDG_CONFIG_HOME` through
/// a POSIX hop first, so a version that fails still reports where `OpenCode` 2 would look. `None` when the shell could
/// not run or printed nothing past its startup noise (upstream's `OpenCodeEnvironmentProbe.detect`).
#[must_use]
pub fn detect<H: BuildHasher>(environment: &HashMap<String, String, H>, fallback_home: &str) -> Option<Probe> {
    let environment: HashMap<String, String> = environment
        .iter()
        .filter(|(key, _)| key.as_str() != "NGTERM_SESSION_ID")
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let shell = environment
        .get("SHELL")
        .filter(|shell| !shell.is_empty())
        .cloned()
        .unwrap_or_else(|| "/bin/sh".to_owned());
    let marker = format!("ngterm-opencode-version-{}", uuid::Uuid::new_v4());
    // `printf` is the POSIX shell's builtin, not upstream's `/usr/bin/printf`, which NixOS and Guix do not have
    let command = format!(
        "printf '\\n{marker}\\n%s\\000%s\\000%s\\000%s\\000%s\\000' \"$HOME\" \"$PWD\" \
         \"${{OPENCODE_CONFIG_DIR+x}}\" \"${{OPENCODE_CONFIG_DIR-}}\" \"${{XDG_CONFIG_HOME-}}\"; \
         exec /usr/bin/env opencode --version"
    );
    let hop = format!("exec /bin/sh -c {}", super::hooks_config::shell_quote(&command));
    let (stdout, succeeded) = run_with_timeout(&shell, &["-ilc", &hop], &environment)?;
    parse_probe(&stdout, &marker, succeeded, fallback_home)
}

/// The probe's fields past the marker: home, working directory, whether `OPENCODE_CONFIG_DIR` is set, its value,
/// `XDG_CONFIG_HOME`, then the version output.
fn parse_probe(stdout: &str, marker: &str, succeeded: bool, fallback_home: &str) -> Option<Probe> {
    let (_, after) = stdout.split_once(&format!("\n{marker}\n"))?;
    let fields: Vec<&str> = after.splitn(6, '\0').collect();
    let [home, pwd, config_set, config, xdg, version] = fields.as_slice() else {
        return None;
    };
    let home = if home.is_empty() { fallback_home } else { home };
    Some(Probe {
        version: succeeded.then(|| Version::from_version_output(version)).flatten(),
        v2_configuration_directory: configuration_directory(
            home,
            (!config_set.is_empty()).then_some(*config),
            Some(xdg),
            Some(pwd),
        ),
    })
}

/// Runs `program args` with `environment`, stdin closed, stderr dropped, killing it past [`PROBE_TIMEOUT`]: its
/// stdout and whether it exited 0 in time; `None` when it could not start.
fn run_with_timeout(program: &str, args: &[&str], environment: &HashMap<String, String>) -> Option<(String, bool)> {
    let mut child = Command::new(program)
        .args(args)
        .env_clear()
        .envs(environment)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    // a grandchild holding the pipe open past the kill would block the read: give it a bounded wait
    let bytes = join_with_deadline(reader, Instant::now() + Duration::from_millis(500)).unwrap_or_default();
    Some((
        String::from_utf8_lossy(&bytes).into_owned(),
        status.is_some_and(|status| status.success()),
    ))
}

fn join_with_deadline(handle: std::thread::JoinHandle<Vec<u8>>, deadline: Instant) -> Option<Vec<u8>> {
    while !handle.is_finished() {
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    handle.join().ok()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use ngterm_host::fakes::write_executable;

    use super::*;

    #[test]
    fn versioned_paths_and_markers() {
        let config = Path::new("/home/me/.config/opencode");
        assert_eq!(
            Version::V1.plugin_path(config),
            Path::new("/home/me/.config/opencode/plugins/ngterm-status.js")
        );
        assert_eq!(
            Version::V2.plugin_path(config),
            Path::new("/home/me/.config/opencode/plugins/ngterm-v2/tui.js")
        );
        assert_eq!(Version::V1.relative_path(), "opencode/ngterm-status.js");
        assert_eq!(Version::V2.relative_path(), "opencode/ngterm-v2/tui.js");
        assert_eq!(Version::V1.marker(), "// ngterm-opencode-status-plugin");
        assert_eq!(Version::V2.marker(), "// ngterm-opencode-v2-status-plugin");
    }

    #[test]
    fn ownership_protects_the_users_plugin() {
        assert!(Version::V1.may_overwrite(false, None));
        assert!(Version::V1.may_overwrite(true, Some("// ngterm-opencode-status-plugin\nexport const A = 1\n")));
        assert!(!Version::V1.may_overwrite(true, Some("export const Other = 1\n")));
        assert!(!Version::V1.may_overwrite(true, None));
        assert!(Version::V2.may_overwrite(true, Some("// ngterm-opencode-v2-status-plugin\n")));
        assert!(!Version::V2.may_overwrite(true, Some(Version::V1.marker())));
    }

    #[test]
    fn the_configuration_directory_honors_the_override_before_xdg() {
        let resolve = configuration_directory;
        assert_eq!(
            resolve("/home/user", Some("/chosen"), Some("/xdg"), Some("/work")).as_deref(),
            Some("/chosen")
        );
        assert_eq!(
            resolve("/home/user", None, Some("/xdg"), Some("/work")).as_deref(),
            Some("/xdg/opencode")
        );
        assert_eq!(
            resolve("/home/user", None, Some(""), Some("/work")).as_deref(),
            Some("/home/user/.config/opencode")
        );
        assert_eq!(resolve("/home/user", Some(""), Some("/xdg"), Some("/work")), None);
        assert_eq!(
            resolve("/home/user", Some("relative"), None, Some("/work")).as_deref(),
            Some("/work/relative")
        );
        assert_eq!(
            resolve("/home/user", Some("~/literal"), None, Some("/work")).as_deref(),
            Some("/work/~/literal")
        );
        assert_eq!(
            resolve("/home/user", Some("../up"), None, Some("/work/x")).as_deref(),
            Some("/work/up")
        );
        assert_eq!(resolve("~", None, None, None).as_deref(), Some("~/.config/opencode"));
    }

    #[test]
    fn the_major_version_accepts_release_and_prerelease_output() {
        for (output, expected) in [
            ("1.18.31\n", Version::V1),
            ("2.0.12\n", Version::V2),
            (" v2.0.12 \n", Version::V2),
            ("2.1.0-beta.3+build.7", Version::V2),
            ("1.19.0-dev", Version::V1),
            ("opencode v2.0.12\n", Version::V2),
            ("opencode 2.0.12\n", Version::V2),
        ] {
            assert_eq!(Version::from_version_output(output), Some(expected), "{output:?}");
        }
        for output in [
            "",
            "3.0.0",
            "0.0.0-dev",
            "2",
            "2.0",
            "2.x.1",
            "opencode version 2.0.12",
            "warning\n2.0.12",
            "1.18.31\n2.0.12",
            "2.0.1-",
            "2.0.1+",
        ] {
            assert_eq!(Version::from_version_output(output), None, "{output:?}");
        }
    }

    /// A fake login shell running the hop the probe hands it, and a fake `opencode` on its `PATH`.
    fn fake_environment(dir: &Path, version_script: &str) -> HashMap<String, String> {
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).unwrap();
        write_executable(&bin.join("opencode"), version_script).unwrap();
        write_executable(&dir.join("shell"), "#!/bin/sh\nshift\nexec /bin/sh -c \"$1\"\n").unwrap();
        HashMap::from([
            ("PATH".to_owned(), format!("{}:/usr/bin:/bin", bin.display())),
            ("HOME".to_owned(), dir.join("home").display().to_string()),
            ("SHELL".to_owned(), dir.join("shell").display().to_string()),
            ("NGTERM_SESSION_ID".to_owned(), "must-not-leak".to_owned()),
        ])
    }

    #[test]
    fn the_probe_reads_the_version_and_the_v2_directory_from_the_shell() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = fake_environment(
            dir.path(),
            "#!/bin/sh\n[ -z \"$NGTERM_SESSION_ID\" ] || exit 9\nprintf 'opencode v2.0.12\\n'\n",
        );
        env.insert("XDG_CONFIG_HOME".to_owned(), "/xdg".to_owned());
        let probe = detect(&env, "/fallback").unwrap();
        assert_eq!(probe.version, Some(Version::V2));
        assert_eq!(probe.v2_configuration_directory.as_deref(), Some("/xdg/opencode"));

        env.insert("OPENCODE_CONFIG_DIR".to_owned(), "/chosen".to_owned());
        let probe = detect(&env, "/fallback").unwrap();
        assert_eq!(probe.v2_configuration_directory.as_deref(), Some("/chosen"));
    }

    #[test]
    fn an_unknown_or_failing_version_still_reports_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let env = fake_environment(dir.path(), "#!/bin/sh\nprintf 'unknown-version\\n'\n");
        let probe = detect(&env, "/fallback").unwrap();
        assert_eq!(probe.version, None);
        let home = dir.path().join("home").display().to_string();
        assert_eq!(
            probe.v2_configuration_directory,
            Some(format!("{home}/.config/opencode"))
        );

        let env = fake_environment(dir.path(), "#!/bin/sh\nprintf '2.0.1\\n'\nexit 1\n");
        assert_eq!(
            detect(&env, "/fallback").unwrap().version,
            None,
            "a failing run is no version"
        );
    }

    #[test]
    fn a_shell_that_cannot_run_probes_nothing() {
        let env = HashMap::from([("SHELL".to_owned(), "/no/such/ngterm-test-shell".to_owned())]);
        assert_eq!(detect(&env, "/fallback"), None);
    }
}
