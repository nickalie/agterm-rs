//! Builds libghostty-vt for cargo's target with Zig and links the static archive.
//!
//! The prefix comes from `AGTERM_GHOSTTY_VT_PREFIX` when set, else from `scripts/build-ghostty-vt.sh --print-prefix`,
//! the one place that knows how it is keyed, run with bash (Git Bash on Windows, found explicitly, with the printed
//! MSYS path converted by `cygpath -m`: `script.rs`). The script prepares it (the pinned, patched source tree with
//! its Zig packages, the headers, Ghostty's resources and a stamp naming the pinned zig); this script runs `zig build
//! -Demit-lib-vt` over that tree (`zig_build.rs`, ported from herdr's build script), installing into and caching
//! under `OUT_DIR`, and never reaches the network. A missing prefix fails the build with a pointer to the script.
//!
//! The archive needs no system library beyond the C runtime Rust links anyway: on Linux and macOS
//! (`libghostty-vt-static.pc` lists no `Libs.private` at `LIBGHOSTTY_VT_REV`) and on Windows MSVC, where
//! `ghostty-vt-static.lib` links alone (the link test, on the `windows-2025` runner and in the Win11 VM). On Apple
//! the archive is linked from a directory of its own, built for the deployment target rustc links for (the link
//! test reads each member's `LC_BUILD_VERSION`).

mod script;
mod zig_build;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// A failure exits non-zero, so cargo shows what the script printed beside the error: zig's own output when its
/// build fails (cargo hides a build script's output while it exits successfully, even with `cargo::error`).
fn main() -> ExitCode {
    match build() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            println!("cargo::error={message}");
            ExitCode::FAILURE
        }
    }
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
    let prefix = script::prefix(&root)?;
    println!("cargo::rerun-if-changed={}", prefix.join(zig_build::STAMP).display());
    let stamp = zig_build::prepared(&prefix)?;
    check_zig(&stamp)?;
    let read =
        |path: &Path| std::fs::read_to_string(path).map_err(|err| format!("cannot read {}: {err}", path.display()));
    let pin = zig_build::pin(
        &read(&root.join("build/pins.env"))?,
        &read(&prefix.join(zig_build::STAMP))?,
    )?;
    // the pin a session server's handoff compares (`agterm_ghostty_vt_sys::PIN`)
    println!("cargo::rustc-env=AGTERM_GHOSTTY_VT_PIN={pin}");

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
        // the workspace's pkg-config wrapper (`.cargo/config.toml`, a shell script for WPE WebKit) is not for Zig,
        // which runs PKG_CONFIG for its system-library lookups: on Windows it cannot run a script at all
        // (`error: InvalidExe`), and libghostty-vt links no system library through pkg-config anywhere
        .env_remove("PKG_CONFIG")
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
    let name = build.archive.file_name().ok_or("the archive has no file name")?;
    if build.archive.parent() != Some(build.link_dir.as_path()) {
        std::fs::create_dir_all(&build.link_dir)
            .map_err(|err| format!("cannot create {}: {err}", build.link_dir.display()))?;
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
    // for the link test, which checks what the archive was built for
    println!(
        "cargo::rustc-env=AGTERM_GHOSTTY_VT_ARCHIVE={}",
        build.link_dir.join(name).display()
    );
    println!("cargo::rustc-env=AGTERM_GHOSTTY_VT_ZIG_TARGET={}", build.zig_target);
    Ok(())
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
