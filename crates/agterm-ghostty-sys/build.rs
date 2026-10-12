//! Links the vendored libghostty prefix and exports its root to dependents as `DEP_GHOSTTY_ROOT`.
//!
//! The prefix path comes from `scripts/build-ghostty.sh --print-prefix`, the one place that knows how it is keyed;
//! this script never builds it.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    match prefix() {
        Ok(prefix) => {
            let lib = prefix.join("lib");
            println!("cargo::rerun-if-changed={}", lib.join("libghostty.so").display());
            println!("cargo::rerun-if-changed={}", prefix.join("include/ghostty.h").display());
            println!("cargo::rustc-link-search=native={}", lib.display());
            println!("cargo::rustc-link-lib=dylib=ghostty");
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
    let script = root.join("scripts/build-ghostty.sh");
    for input in [
        "build/pins.env",
        "build/patches/ghostty",
        "scripts/build-ghostty.sh",
        "scripts/lib/native.sh",
    ] {
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
    let complete = prefix.join("lib/libghostty.so").is_file() && prefix.join("include/ghostty.h").is_file();
    if complete {
        Ok(prefix)
    } else {
        Err(format!(
            "no libghostty at {}: run scripts/build-ghostty.sh (it builds the pinned, patched libghostty there)",
            prefix.display()
        ))
    }
}
