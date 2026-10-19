//! Where the build script finds the libghostty-vt prefix: `AGTERM_GHOSTTY_VT_PREFIX` when set, else what
//! `scripts/build-ghostty-vt.sh --print-prefix` prints, run with bash. On Windows that bash is Git Bash, found
//! explicitly (never `bash` from `PATH`, which can be WSL's `System32\bash.exe`), and the MSYS path it prints is
//! converted to the native form with `cygpath -m`. `build.rs` and the tests share these functions.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The variable that names a prepared prefix and skips the script (on every OS).
pub const PREFIX_ENV: &str = "AGTERM_GHOSTTY_VT_PREFIX";

/// The prepare script, relative to the repository root.
pub const SCRIPT: &str = "scripts/build-ghostty-vt.sh";

/// Where Git for Windows's `bin\bash.exe` may be, first first: beside the `git.exe` of each `PATH` entry (Git's
/// `cmd\`, `bin\` or `mingw64\bin\`), then the install locations under `ProgramFiles`, `ProgramW6432`,
/// `ProgramFiles(x86)` and `LOCALAPPDATA\Programs`. `bin\bash.exe` is Git's launcher, which puts its `usr\bin`
/// (cygpath, patch, tic) on `PATH`; `System32\bash.exe` is never one of them. `exists` tells whether a file is
/// there.
#[must_use]
pub fn git_bash_candidates(var: &dyn Fn(&str) -> Option<OsString>, exists: &dyn Fn(&Path) -> bool) -> Vec<PathBuf> {
    let named = |dir: &Path, names: &[&str]| {
        dir.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| names.iter().any(|candidate| name.eq_ignore_ascii_case(candidate)))
    };
    let mut candidates = Vec::new();
    for dir in var("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default()
    {
        if !exists(&dir.join("git.exe")) || !named(&dir, &["cmd", "bin"]) {
            continue;
        }
        let Some(mut root) = dir.parent() else { continue };
        // mingw64\bin\git.exe sits two levels below the root
        if named(root, &["mingw64"]) {
            let Some(parent) = root.parent() else { continue };
            root = parent;
        }
        candidates.push(root.join("bin").join("bash.exe"));
    }
    for (name, below) in [
        ("ProgramFiles", &["Git"][..]),
        ("ProgramW6432", &["Git"]),
        ("ProgramFiles(x86)", &["Git"]),
        ("LOCALAPPDATA", &["Programs", "Git"]),
    ] {
        if let Some(dir) = var(name) {
            let mut path = PathBuf::from(dir);
            path.extend(below);
            candidates.push(path.join("bin").join("bash.exe"));
        }
    }
    let mut unique = Vec::new();
    for candidate in candidates {
        if !unique.contains(&candidate) {
            unique.push(candidate);
        }
    }
    unique
}

/// The bash that runs the repository's shell scripts: `bash` on Unix, Git Bash on Windows.
///
/// # Errors
///
/// On Windows, no Git Bash in any of [`git_bash_candidates`].
pub fn bash() -> Result<PathBuf, String> {
    if !cfg!(windows) {
        return Ok(PathBuf::from("bash"));
    }
    let candidates = git_bash_candidates(&|name| std::env::var_os(name), &Path::is_file);
    candidates.iter().find(|path| path.is_file()).cloned().ok_or_else(|| {
        let looked: Vec<String> = candidates.iter().map(|path| path.display().to_string()).collect();
        format!(
            "no Git Bash (Git for Windows's bin\\bash.exe) to run {SCRIPT}; looked in: {}",
            looked.join(", ")
        )
    })
}

/// `path` as an argument bash takes: on Windows with forward slashes (MSYS reads `C:/...`, and `dirname` of a
/// backslashed path is not the script's directory), elsewhere as is.
#[must_use]
pub fn bash_arg(path: &Path) -> OsString {
    if cfg!(windows) {
        OsString::from(path.to_string_lossy().replace('\\', "/"))
    } else {
        path.as_os_str().to_owned()
    }
}

/// Run `script ARGS` with [`bash`], returning its trimmed stdout.
///
/// # Errors
///
/// No bash, or the script failing (its stderr in the error).
pub fn run(script: &Path, args: &[&str], envs: &[(&str, &Path)]) -> Result<String, String> {
    let mut command = Command::new(bash()?);
    command.arg(bash_arg(script)).args(args);
    for (name, value) in envs {
        command.env(name, bash_arg(value));
    }
    let output = command
        .output()
        .map_err(|err| format!("cannot run {}: {err}", script.display()))?;
    if !output.status.success() {
        return Err(format!(
            "{} {} failed: {}",
            script.display(),
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// An MSYS path printed by a script in this host's native form: `cygpath -m` on Windows (`/c/x` → `C:/x`), the path
/// itself elsewhere.
///
/// # Errors
///
/// On Windows, `cygpath` failing.
pub fn native(path: &str) -> Result<PathBuf, String> {
    if !cfg!(windows) {
        return Ok(PathBuf::from(path));
    }
    let output = Command::new(bash()?)
        .args(["-c", "cygpath -m \"$1\"", "cygpath"])
        .arg(path)
        .output()
        .map_err(|err| format!("cannot run cygpath: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "cygpath -m {path} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()))
}

/// The libghostty-vt prefix of the repository at `root`: [`PREFIX_ENV`] when set and not empty, else the one
/// `scripts/build-ghostty-vt.sh --print-prefix` names, in the native form.
///
/// # Errors
///
/// No bash, or the script or `cygpath` failing.
pub fn prefix(root: &Path) -> Result<PathBuf, String> {
    if let Some(prefix) = std::env::var_os(PREFIX_ENV).filter(|prefix| !prefix.is_empty()) {
        return Ok(PathBuf::from(prefix));
    }
    native(&run(&root.join(SCRIPT), &["--print-prefix"], &[])?)
}
