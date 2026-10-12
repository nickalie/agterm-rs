//! Emits the `agterm` binary's rpaths to `libghostty.so` and the vendored prefix it was built against.
//!
//! As `agterm-ghostty`'s build script: `$ORIGIN/../lib` for a staged build, `<prefix>/lib` for `cargo run`, both as
//! `DT_RPATH` so an inherited `LD_LIBRARY_PATH` cannot swap in another agterm's libghostty. The prefix is also baked
//! in as `AGTERM_GHOSTTY_PREFIX`, the last place an unstaged build looks for ghostty's resources.

use std::process::ExitCode;

fn main() -> ExitCode {
    println!("cargo::rerun-if-env-changed=DEP_GHOSTTY_ROOT");
    let Some(root) = std::env::var_os("DEP_GHOSTTY_ROOT") else {
        println!("cargo::error=DEP_GHOSTTY_ROOT is not set: agterm-ghostty-sys did not export the libghostty prefix");
        return ExitCode::SUCCESS;
    };
    let root = std::path::PathBuf::from(root);
    println!("cargo::rustc-link-arg=-Wl,--disable-new-dtags");
    println!("cargo::rustc-link-arg=-Wl,-rpath,$ORIGIN/../lib");
    println!("cargo::rustc-link-arg=-Wl,-rpath,{}", root.join("lib").display());
    println!("cargo::rustc-env=AGTERM_GHOSTTY_PREFIX={}", root.display());
    ExitCode::SUCCESS
}
