//! Builds libghostty-vt for cargo's target with Zig and links the static archive.
//!
//! The prefix comes from `scripts/build-ghostty-vt.sh --print-prefix`, the one place that knows how it is keyed. The
//! script prepares it (the pinned, patched source tree with its Zig packages, the headers, Ghostty's resources and a
//! stamp naming the pinned zig); this script runs `zig build -Demit-lib-vt` over that tree (`zig_build.rs`, ported
//! from herdr's build script), installing into and caching under `OUT_DIR`, and never reaches the network. A missing
//! prefix fails the build with a pointer to the script. The archive needs no system library beyond libc on Linux
//! (`libghostty-vt-static.pc` lists no `Libs.private` at `LIBGHOSTTY_VT_REV`, and the link test links it with nothing
//! else).

mod zig_build;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    if let Err(message) = build() {
        println!("cargo::error={message}");
    }
    ExitCode::SUCCESS
}

fn build() -> Result<(), String> {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR").ok_or("CARGO_MANIFEST_DIR is not set")?;
    let root = Path::new(&manifest).join("../..");
    for input in zig_build::RERUN_FILES {
        println!("cargo::rerun-if-changed={}", root.join(input).display());
    }
    for name in zig_build::RERUN_ENV {
        println!("cargo::rerun-if-env-changed={name}");
    }
    let prefix = prefix(&root)?;
    println!("cargo::rerun-if-changed={}", prefix.join(zig_build::STAMP).display());
    let stamp = zig_build::prepared(&prefix)?;
    check_zig(&stamp)?;

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").ok_or("OUT_DIR is not set")?);
    let target = std::env::var("TARGET").map_err(|err| format!("TARGET: {err}"))?;
    let macos_deployment_target = std::env::var("MACOSX_DEPLOYMENT_TARGET").ok();
    let jobs = std::env::var("NUM_JOBS").ok();
    let build = zig_build::plan(&zig_build::Inputs {
        prefix: &prefix,
        stamp: &stamp,
        out_dir: &out_dir,
        target: &target,
        macos_deployment_target: macos_deployment_target.as_deref(),
        jobs: jobs.as_deref(),
    })?;

    let status = Command::new(&build.zig)
        .args(&build.args)
        .current_dir(&build.source)
        // an install never goes below a staging root a packaging environment may have exported
        .env_remove("DESTDIR")
        .status()
        .map_err(|err| format!("cannot run {}: {err}", build.zig.display()))?;
    if !status.success() {
        return Err(format!(
            "zig build of libghostty-vt in {} failed ({status}); its output is above",
            build.source.display()
        ));
    }
    if !build.archive.is_file() {
        return Err(format!(
            "the libghostty-vt build did not produce {}",
            build.archive.display()
        ));
    }
    if build.archive.parent() != Some(build.link_dir.as_path()) {
        std::fs::create_dir_all(&build.link_dir)
            .map_err(|err| format!("cannot create {}: {err}", build.link_dir.display()))?;
        let name = build.archive.file_name().ok_or("the archive has no file name")?;
        std::fs::copy(&build.archive, build.link_dir.join(name)).map_err(|err| {
            format!(
                "cannot copy {} into {}: {err}",
                build.archive.display(),
                build.link_dir.display()
            )
        })?;
    }
    println!("cargo::rustc-link-search=native={}", build.link_dir.display());
    println!("cargo::rustc-link-lib=static={}", build.link_name);
    Ok(())
}

/// The prefix `scripts/build-ghostty-vt.sh --print-prefix` names.
fn prefix(root: &Path) -> Result<PathBuf, String> {
    let script = root.join("scripts/build-ghostty-vt.sh");
    let output = Command::new("bash")
        .arg(&script)
        .arg("--print-prefix")
        .output()
        .map_err(|err| format!("cannot run {}: {err}", script.display()))?;
    if !output.status.success() {
        return Err(format!(
            "{} --print-prefix failed: {}",
            script.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()))
}

/// The stamp's zig must still be the pinned one: the prefix was prepared with it, and an unpinned zig never builds.
fn check_zig(stamp: &zig_build::Stamp) -> Result<(), String> {
    let output = Command::new(&stamp.zig).arg("version").output();
    let version = output
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
    if version.as_deref() == Some(stamp.zig_version.as_str()) {
        Ok(())
    } else {
        Err(format!(
            "{} is no longer zig {}: run scripts/build-ghostty-vt.sh (it prepares the prefix with the pinned zig)",
            stamp.zig.display(),
            stamp.zig_version
        ))
    }
}
