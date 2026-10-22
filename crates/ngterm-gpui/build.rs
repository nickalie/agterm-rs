//! Points the terminal's embedded fonts (`terminal::font`) at the vendored fonts prefix: `NGTERM_GPUI_FONTS` is its
//! `share/fonts`, which `include_bytes!` reads; with the `html` feature, gives the binaries an rpath to the WPE
//! `WebKit` they link (`wpe_rpath`).
//!
//! The prefix path comes from `scripts/fetch-fonts.sh --print-prefix`, the one place that knows how it is keyed; this
//! script never fetches. A missing or incomplete prefix fails the build with a pointer to the script.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// The faces `terminal::font` embeds (scripts/lib/native.sh `FONT_FILES`).
const FONT_FILES: [&str; 5] = [
    "JetBrainsMono-Regular.ttf",
    "JetBrainsMono-Bold.ttf",
    "JetBrainsMono-Italic.ttf",
    "JetBrainsMono-BoldItalic.ttf",
    "SymbolsNerdFontMono-Regular.ttf",
];

fn main() -> ExitCode {
    wpe_rpath();
    match fonts() {
        Ok(fonts) => {
            for file in FONT_FILES {
                println!("cargo::rerun-if-changed={}", fonts.join(file).display());
            }
            println!("cargo::rustc-env=NGTERM_GPUI_FONTS={}", fonts.display());
        }
        Err(message) => println!("cargo::error={message}"),
    }
    ExitCode::SUCCESS
}

/// With the `html` feature, an rpath to the WPE `WebKit` the build linked (`ngterm-wpe`'s `libdirs`): the vendored
/// package of `scripts/fetch-wpe.sh` lies outside the loader's search path, and the app never enters that package's
/// overlay (only its engine worker does, `ngterm_wpe::worker`).
fn wpe_rpath() {
    println!("cargo::rerun-if-env-changed=DEP_NGTERM_WPE_LIBDIRS");
    if std::env::var_os("CARGO_FEATURE_HTML").is_none() {
        return;
    }
    let Ok(libdirs) = std::env::var("DEP_NGTERM_WPE_LIBDIRS") else {
        return;
    };
    for libdir in libdirs.split(':').filter(|dir| !dir.is_empty()) {
        println!("cargo::rustc-link-arg=-Wl,-rpath,{libdir}");
    }
}

fn fonts() -> Result<PathBuf, String> {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR").ok_or("CARGO_MANIFEST_DIR is not set")?;
    let root = Path::new(&manifest).join("../..");
    let script = root.join("scripts/fetch-fonts.sh");
    for input in ["build/pins.env", "scripts/fetch-fonts.sh", "scripts/lib/native.sh"] {
        println!("cargo::rerun-if-changed={}", root.join(input).display());
    }
    println!("cargo::rerun-if-env-changed=NGTERM_VENDOR");
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
    let fonts = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()).join("share/fonts");
    if FONT_FILES.iter().all(|file| fonts.join(file).is_file()) {
        Ok(fonts)
    } else {
        Err(format!(
            "no terminal fonts at {}: run scripts/fetch-fonts.sh (it fetches the fonts Ghostty embeds there)",
            fonts.display()
        ))
    }
}
