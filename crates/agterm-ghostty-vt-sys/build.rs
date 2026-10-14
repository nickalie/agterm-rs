//! Links the vendored static libghostty-vt and exports its prefix to dependents as `DEP_GHOSTTY_VT_ROOT`.
//!
//! The prefix path comes from `scripts/build-ghostty-vt.sh --print-prefix`, the one place that knows how it is keyed;
//! this script never builds it. The archive needs no system library beyond libc (`libghostty-vt-static.pc` lists no
//! `Libs.private` at `LIBGHOSTTY_VT_REV`, and the link test links it with nothing else).

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    match prefix() {
        Ok(prefix) => {
            let lib = prefix.join("lib");
            println!("cargo::rerun-if-changed={}", lib.join("libghostty-vt.a").display());
            println!(
                "cargo::rerun-if-changed={}",
                prefix.join("include/ghostty/vt.h").display()
            );
            println!("cargo::rustc-link-search=native={}", lib.display());
            println!("cargo::rustc-link-lib=static=ghostty-vt");
            println!("cargo::metadata=root={}", prefix.display());
            ExitCode::SUCCESS
        }
        Err(message) => {
            println!("cargo::error={message}");
            ExitCode::SUCCESS
        }
    }
}

fn prefix() -> Result<PathBuf, String> {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR").ok_or("CARGO_MANIFEST_DIR is not set")?;
    let root = Path::new(&manifest).join("../..");
    let script = root.join("scripts/build-ghostty-vt.sh");
    for input in ["build/pins.env", "scripts/build-ghostty-vt.sh", "scripts/lib/native.sh"] {
        println!("cargo::rerun-if-changed={}", root.join(input).display());
    }
    println!("cargo::rerun-if-env-changed=AGTERM_VENDOR");
    println!("cargo::rerun-if-env-changed=HOME");

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
    let prefix = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    let complete = prefix.join("lib/libghostty-vt.a").is_file() && prefix.join("include/ghostty/vt.h").is_file();
    if complete {
        Ok(prefix)
    } else {
        Err(format!(
            "no libghostty-vt at {}: run scripts/build-ghostty-vt.sh (it builds the pinned libghostty-vt there)",
            prefix.display()
        ))
    }
}
