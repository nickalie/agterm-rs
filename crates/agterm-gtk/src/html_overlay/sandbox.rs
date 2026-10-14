//! Whether `WebKitGTK`'s web process can start here, asked before the first page is accepted.
//!
//! `WebKit` runs its web process (and the D-Bus proxy beside it) inside bubblewrap, and when bubblewrap cannot set up
//! its namespaces it ends the app's own process (a `g_error`, which aborts): every terminal of the instance would go
//! with the page. That is the case on Ubuntu 24.04 and later, whose `AppArmor` denies an unconfined program the
//! capabilities of a user namespace (`kernel.apparmor_restrict_unprivileged_userns=1`), unless a profile grants it
//! `userns`; `bwrap` then fails with `setting up uid map: Permission denied`. So the check `WebKit` itself runs inside
//! a container (`WTF::isInsideUnsupportedContainer`, the same `bwrap` invocation) runs here once, and a failure
//! refuses pages with its reason instead. The sandbox is never turned off: where `WebKit` does not start bubblewrap
//! (`WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS`, Flatpak, Snap, a container, where it checks and decides for itself)
//! nothing is checked.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

/// The probe `WebKit` runs before it trusts bubblewrap inside a container (`Source/WTF/wtf/glib/Sandbox.cpp`).
const PROBE: [&str; 9] = [
    "--ro-bind",
    "/",
    "/",
    "--proc",
    "/proc",
    "--dev",
    "/dev",
    "--unshare-all",
    "true",
];

/// What decides whether `WebKit` starts bubblewrap at all.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Environment {
    /// `WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS`.
    sandbox_switch: Option<String>,
    /// `/.flatpak-info` exists.
    flatpak: bool,
    /// `SNAP`, `SNAP_NAME` and `SNAP_REVISION` are all set.
    snap: bool,
    /// `/run/.containerenv` exists.
    container: bool,
}

impl Environment {
    fn current() -> Self {
        let set = |name: &str| std::env::var_os(name).is_some();
        Self {
            sandbox_switch: std::env::var("WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS").ok(),
            flatpak: Path::new("/.flatpak-info").exists(),
            snap: set("SNAP") && set("SNAP_NAME") && set("SNAP_REVISION"),
            container: Path::new("/run/.containerenv").exists(),
        }
    }

    /// Whether `WebKit` will start its web process through bubblewrap (`WebProcessPool`'s sandbox switch and
    /// `WTF::shouldUseBubblewrap`), leaving a container out: there `WebKit` runs the probe itself and goes on without
    /// the sandbox when it fails.
    fn starts_bubblewrap(&self) -> bool {
        let disabled = self.sandbox_switch.as_deref().is_some_and(|value| value != "0");
        !(disabled || self.flatpak || self.snap || self.container)
    }
}

/// Why no web view can run here, `None` when one can; the probe runs once, on first ask.
pub(super) fn failure() -> Option<String> {
    static FAILURE: OnceLock<Option<String>> = OnceLock::new();
    FAILURE
        .get_or_init(|| {
            let failure = check(&Environment::current(), probe);
            if let Some(reason) = &failure {
                tracing::warn!(
                    reason,
                    "WebKit's sandbox cannot start, so html overlays are refused: on Ubuntu, an AppArmor profile \
                     granting agterm `userns` lets bubblewrap set up its namespaces"
                );
            }
            failure
        })
        .clone()
}

fn check(environment: &Environment, probe: impl FnOnce() -> Result<(), String>) -> Option<String> {
    if !environment.starts_bubblewrap() {
        return None;
    }
    probe()
        .err()
        .map(|reason| format!("the web engine's sandbox cannot start ({reason})"))
}

/// Runs bubblewrap as `WebKit` would; its first line on stderr when it fails.
fn probe() -> Result<(), String> {
    let output = Command::new("bwrap")
        .args(PROBE)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| format!("bwrap: {err}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map_or_else(|| format!("bwrap: {}", output.status), str::to_owned))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused() -> Result<(), String> {
        Err("bwrap: setting up uid map: Permission denied".to_owned())
    }

    #[test]
    fn a_failing_bubblewrap_refuses_pages_with_its_reason() {
        assert_eq!(
            check(&Environment::default(), refused),
            Some("the web engine's sandbox cannot start (bwrap: setting up uid map: Permission denied)".to_owned())
        );
        assert_eq!(check(&Environment::default(), || Ok(())), None);
    }

    #[test]
    fn nothing_is_checked_where_webkit_starts_no_bubblewrap() {
        let skipped = |environment: Environment| {
            check(&environment, || {
                panic!("probed although WebKit starts no bubblewrap: {environment:?}")
            })
        };
        let switch = |value: &str| Environment {
            sandbox_switch: Some(value.to_owned()),
            ..Environment::default()
        };
        assert_eq!(skipped(switch("1")), None);
        assert_eq!(skipped(switch("")), None, "any value but 0 turns the sandbox off");
        assert_eq!(
            skipped(Environment {
                flatpak: true,
                ..Environment::default()
            }),
            None
        );
        assert_eq!(
            skipped(Environment {
                snap: true,
                ..Environment::default()
            }),
            None
        );
        assert_eq!(
            skipped(Environment {
                container: true,
                ..Environment::default()
            }),
            None
        );
        assert!(check(&switch("0"), refused).is_some(), "0 keeps the sandbox on");
    }
}
