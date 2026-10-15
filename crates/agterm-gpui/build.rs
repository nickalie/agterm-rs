//! Points the terminal's embedded fonts (`terminal::font`) at the vendored fonts prefix: `AGTERM_GPUI_FONTS` is its
//! `share/fonts`, which `include_bytes!` reads.
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
    match fonts() {
        Ok(fonts) => {
            for file in FONT_FILES {
                println!("cargo::rerun-if-changed={}", fonts.join(file).display());
            }
            println!("cargo::rustc-env=AGTERM_GPUI_FONTS={}", fonts.display());
        }
        Err(message) => println!("cargo::error={message}"),
    }
    ExitCode::SUCCESS
}

fn fonts() -> Result<PathBuf, String> {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR").ok_or("CARGO_MANIFEST_DIR is not set")?;
    let root = Path::new(&manifest).join("../..");
    let script = root.join("scripts/fetch-fonts.sh");
    for input in ["build/pins.env", "scripts/fetch-fonts.sh", "scripts/lib/native.sh"] {
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
