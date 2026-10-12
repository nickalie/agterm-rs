//! Emits the `agterm` binary's rpaths to `libghostty.so` and the vendored prefix it was built against, and records the
//! build's git commit.
//!
//! As `agterm-ghostty`'s build script: `$ORIGIN/../lib` for a staged build, `<prefix>/lib` for `cargo run`, both as
//! `DT_RPATH` so an inherited `LD_LIBRARY_PATH` cannot swap in another agterm's libghostty. The prefix is also baked
//! in as `AGTERM_GHOSTTY_PREFIX`, the last place an unstaged build looks for ghostty's resources.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const COMMIT_ENV: &str = "AGTERM_BUILD_COMMIT";

fn main() -> ExitCode {
    record_commit();
    println!("cargo::rerun-if-env-changed=DEP_GHOSTTY_ROOT");
    let Some(root) = std::env::var_os("DEP_GHOSTTY_ROOT") else {
        println!("cargo::error=DEP_GHOSTTY_ROOT is not set: agterm-ghostty-sys did not export the libghostty prefix");
        return ExitCode::SUCCESS;
    };
    let root = PathBuf::from(root);
    println!("cargo::rustc-link-arg=-Wl,--disable-new-dtags");
    println!("cargo::rustc-link-arg=-Wl,-rpath,$ORIGIN/../lib");
    println!("cargo::rustc-link-arg=-Wl,-rpath,{}", root.join("lib").display());
    println!("cargo::rustc-env=AGTERM_GHOSTTY_PREFIX={}", root.display());
    ExitCode::SUCCESS
}

/// `git <args>` in this crate's directory, trimmed; `None` when git is missing or fails.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).current_dir(dir).output().ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    (output.status.success() && !text.is_empty()).then(|| text.to_owned())
}

/// The commit `version` reports (upstream's `GitCommit` bundle key): `AGTERM_BUILD_COMMIT` when the build environment
/// sets it (a source tarball has no git), else the checkout's short HEAD; none when neither answers. The build reruns
/// when HEAD, its branch or `packed-refs` changes; only existing files are watched, as cargo reruns on a missing one
/// every time.
fn record_commit() {
    println!("cargo::rerun-if-env-changed={COMMIT_ENV}");
    if let Some(commit) = std::env::var(COMMIT_ENV).ok().filter(|commit| !commit.is_empty()) {
        println!("cargo::rustc-env={COMMIT_ENV}={commit}");
        return;
    }
    let Some(dir) = std::env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from) else {
        return;
    };
    let Some(commit) = git(&dir, &["rev-parse", "--short", "HEAD"]) else {
        return;
    };
    let mut refs = vec!["HEAD".to_owned(), "packed-refs".to_owned()];
    refs.extend(git(&dir, &["symbolic-ref", "-q", "HEAD"]));
    for name in refs {
        if let Some(path) = git(&dir, &["rev-parse", "--git-path", &name]).map(|path| dir.join(path))
            && path.exists()
        {
            println!("cargo::rerun-if-changed={}", path.display());
        }
    }
    println!("cargo::rustc-env={COMMIT_ENV}={commit}");
}
