//! Where a program that runs WPE `WebKit` finds its helper processes.
//!
//! A distribution build has its helper path compiled in (`/usr/lib/wpe-webkit-2.0/WPEWebProcess` on Arch; release
//! builds read no `WEBKIT_EXEC_PATH`), so the library and its helpers must sit where the package put them. That holds
//! for the system's `wpewebkit`. The vendored package of `scripts/fetch-wpe.sh` lives in a prefix of its own, and its
//! pages run only with that prefix's `lib/` overlaid on `/usr/lib`, read-only, in a mount namespace of the program's
//! own (`bwrap --ro-overlay`, no root; Task 2's spike launcher). [`command`] builds that launch for the WPE `WebKit`
//! this crate was built against; for the system's it is the plain program.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The library directory the build found WPE `WebKit` in (`build.rs`).
const LIBDIR: &str = env!("AGTERM_WPE_LIBDIR");
/// Where a distribution build looks for its helpers, and so where a vendored prefix's `lib/` is overlaid.
const SYSTEM_LIBDIR: &str = "/usr/lib";
/// The manifest `scripts/fetch-wpe.sh` leaves in a vendored prefix, relative to its `lib/`.
const VENDORED_MANIFEST: &str = "../share/agterm/wpe-PINS";

/// Where WPE `WebKit` comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Provider {
    /// Installed by the system, helpers where the library looks for them.
    System,
    /// The vendored package, its library directory overlaid on `/usr/lib` at launch.
    Vendored { libdir: PathBuf },
}

/// The provider of the WPE `WebKit` this crate was built against.
#[must_use]
pub fn provider() -> Provider {
    provider_for(Path::new(LIBDIR))
}

fn provider_for(libdir: &Path) -> Provider {
    if libdir.join(VENDORED_MANIFEST).is_file() {
        Provider::Vendored {
            libdir: libdir.to_path_buf(),
        }
    } else {
        Provider::System
    }
}

/// A command that runs `program` where the engine's helpers are found: as is for the system's WPE `WebKit`, under
/// `bwrap` with the vendored prefix overlaid otherwise (the working directory kept, the program ended with its parent).
#[must_use]
pub fn command(program: &Path) -> Command {
    match provider() {
        Provider::System => Command::new(program),
        Provider::Vendored { libdir } => {
            let cwd = std::env::current_dir().ok();
            let mut command = Command::new("bwrap");
            command.args(overlay_args(&libdir, cwd.as_deref(), program));
            command
        }
    }
}

/// `bwrap`'s arguments for running `program` with `libdir` overlaid on `/usr/lib`.
fn overlay_args(libdir: &Path, cwd: Option<&Path>, program: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["--dev-bind", "/", "/", "--die-with-parent", "--overlay-src"]
        .into_iter()
        .map(OsString::from)
        .collect();
    args.push(libdir.into());
    args.extend(["--overlay-src", SYSTEM_LIBDIR, "--ro-overlay", SYSTEM_LIBDIR].map(OsString::from));
    if let Some(cwd) = cwd {
        args.push("--chdir".into());
        args.push(cwd.into());
    }
    args.push("--".into());
    args.push(program.into());
    args
}

/// Whether the engine's web process is where this build's WPE `WebKit` looks for it: true for the system's, and for
/// the vendored package only inside its overlay ([`command`]).
#[must_use]
pub fn helpers_reachable() -> bool {
    match provider() {
        Provider::System => true,
        Provider::Vendored { libdir } => {
            // the overlay shows the prefix's own file: same size, same modification time
            let stamp = |dir: &Path| {
                let metadata = std::fs::metadata(dir.join("wpe-webkit-2.0/WPEWebProcess")).ok()?;
                Some((metadata.len(), metadata.modified().ok()?))
            };
            stamp(&libdir).is_some_and(|prefix| stamp(Path::new(SYSTEM_LIBDIR)) == Some(prefix))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prefix_with_fetch_wpe_s_manifest_is_vendored() {
        let prefix = tempfile::tempdir().unwrap();
        let libdir = prefix.path().join("lib");
        std::fs::create_dir_all(&libdir).unwrap();
        assert_eq!(provider_for(&libdir), Provider::System);
        std::fs::create_dir_all(prefix.path().join("share/agterm")).unwrap();
        std::fs::write(prefix.path().join("share/agterm/wpe-PINS"), "WPE_VERSION=2.54.1\n").unwrap();
        assert_eq!(provider_for(&libdir), Provider::Vendored { libdir });
        assert_eq!(provider_for(Path::new("/usr/lib")), Provider::System);
    }

    #[test]
    fn the_overlay_puts_the_prefix_over_usr_lib_and_keeps_the_working_directory() {
        let args = overlay_args(
            Path::new("/v/wpe/lib"),
            Some(Path::new("/work")),
            Path::new("/bin/probe"),
        );
        let args: Vec<&str> = args.iter().map(|arg| arg.to_str().unwrap()).collect();
        assert_eq!(
            args,
            [
                "--dev-bind",
                "/",
                "/",
                "--die-with-parent",
                "--overlay-src",
                "/v/wpe/lib",
                "--overlay-src",
                "/usr/lib",
                "--ro-overlay",
                "/usr/lib",
                "--chdir",
                "/work",
                "--",
                "/bin/probe"
            ]
        );
        let without_cwd = overlay_args(Path::new("/v/wpe/lib"), None, Path::new("/bin/probe"));
        assert!(!without_cwd.iter().any(|arg| arg == "--chdir"));
    }
}
