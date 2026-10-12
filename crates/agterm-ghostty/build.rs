//! Emits the rpaths this crate's test binaries need to find `libghostty.so`.
//!
//! `agterm-ghostty-sys` exports the vendored prefix as `DEP_GHOSTTY_ROOT`. `$ORIGIN/../lib` comes first so a staged
//! build (`bin/` beside `lib/`) loads its own copy; `<prefix>/lib` serves dev and test builds.

use std::process::ExitCode;

fn main() -> ExitCode {
    println!("cargo::rerun-if-env-changed=DEP_GHOSTTY_ROOT");
    let Some(root) = std::env::var_os("DEP_GHOSTTY_ROOT") else {
        println!("cargo::error=DEP_GHOSTTY_ROOT is not set: agterm-ghostty-sys did not export the libghostty prefix");
        return ExitCode::SUCCESS;
    };
    let lib = std::path::Path::new(&root).join("lib");
    println!("cargo::rustc-link-arg=-Wl,-rpath,$ORIGIN/../lib");
    println!("cargo::rustc-link-arg=-Wl,-rpath,{}", lib.display());
    ExitCode::SUCCESS
}
