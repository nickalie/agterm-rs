//! The Ghostty pin and zmx's key their prefixes independently: bumping a `LIBGHOSTTY_VT_` pin (the library, or the
//! Ghostty resources vendored beside it) or a libghostty-vt patch leaves the zmx prefix where it is, and a `ZMX_` pin
//! bump leaves the libghostty-vt prefix where it is.
//!
//! Each case runs the real `--print-prefix` of the build scripts over a copy of the repository's scripts, pins and
//! patches with one pin line edited; nothing is fetched or built.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, PartialEq, Eq)]
struct Prefixes {
    ghostty_vt: String,
    zmx: String,
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// A scratch repository holding the scripts, pins and patches the prefix keys read, with `edit` applied to pins.env.
fn checkout(edit: impl Fn(&str) -> String) -> tempfile::TempDir {
    let root = repo_root();
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("scripts/lib")).unwrap();
    for script in ["lib/native.sh", "build-ghostty-vt.sh", "build-zmx.sh"] {
        fs::copy(
            root.join("scripts").join(script),
            dir.path().join("scripts").join(script),
        )
        .unwrap();
    }
    copy_dir(&root.join("build"), &dir.path().join("build"));
    let pins = dir.path().join("build/pins.env");
    let edited = edit(&fs::read_to_string(&pins).unwrap());
    fs::write(&pins, edited).unwrap();
    dir
}

fn print_prefix(checkout: &Path, script: &str) -> String {
    let output = Command::new("bash")
        .arg(checkout.join("scripts").join(script))
        .arg("--print-prefix")
        // the same vendor root for every checkout, so only the keys can differ
        .env("AGTERM_VENDOR", "/agterm-vendor")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{script}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn prefixes(edit: impl Fn(&str) -> String) -> Prefixes {
    prefixes_of(&checkout(edit))
}

/// The prefixes after `patch` changed one file of the checkout's `build/patches/` (pins unedited).
fn patched_prefixes(patch: &str) -> Prefixes {
    let dir = checkout(str::to_owned);
    let path = dir.path().join("build/patches").join(patch);
    let mut text = fs::read_to_string(&path).unwrap();
    text.push_str("\n# edited\n");
    fs::write(&path, text).unwrap();
    prefixes_of(&dir)
}

fn prefixes_of(dir: &tempfile::TempDir) -> Prefixes {
    Prefixes {
        ghostty_vt: print_prefix(dir.path(), "build-ghostty-vt.sh"),
        zmx: print_prefix(dir.path(), "build-zmx.sh"),
    }
}

/// Replace the value of the one `NAME="..."` line in pins.env.
fn bump(pins: &str, name: &str, value: &str) -> String {
    let prefix = format!("{name}=");
    let mut found = 0;
    let lines: Vec<String> = pins
        .lines()
        .map(|line| {
            if line.starts_with(&prefix) {
                found += 1;
                format!("{name}=\"{value}\"")
            } else {
                line.to_owned()
            }
        })
        .collect();
    assert_eq!(found, 1, "pins.env has {found} {name} lines");
    lines.join("\n") + "\n"
}

#[test]
fn the_prefixes_are_named_after_their_builds() {
    let base = prefixes(str::to_owned);
    assert!(base.ghostty_vt.starts_with("/agterm-vendor/ghostty-vt-"), "{base:?}");
    assert!(base.zmx.starts_with("/agterm-vendor/zmx-"), "{base:?}");
}

#[test]
fn a_libghostty_vt_bump_moves_only_the_vt_prefix() {
    let base = prefixes(str::to_owned);
    let bumped = prefixes(|pins| bump(pins, "LIBGHOSTTY_VT_REV", "0000000000000000000000000000000000000000"));
    assert_ne!(bumped.ghostty_vt, base.ghostty_vt);
    assert_eq!(bumped.zmx, base.zmx);
}

#[test]
fn a_libghostty_vt_zig_bump_moves_only_the_vt_prefix() {
    let base = prefixes(str::to_owned);
    let bumped = prefixes(|pins| bump(pins, "LIBGHOSTTY_VT_ZIG_VERSION", "0.16.1"));
    assert_ne!(bumped.ghostty_vt, base.ghostty_vt);
    assert_eq!(bumped.zmx, base.zmx);
}

#[test]
fn a_theme_package_bump_moves_only_the_vt_prefix() {
    // the resources live in the vt prefix: a new theme package rebuilds it
    let base = prefixes(str::to_owned);
    let bumped = prefixes(|pins| bump(pins, "LIBGHOSTTY_VT_THEMES_SHA256", &"0".repeat(64)));
    assert_ne!(bumped.ghostty_vt, base.ghostty_vt);
    assert_eq!(bumped.zmx, base.zmx);
}

#[test]
fn a_libghostty_vt_patch_moves_only_the_vt_prefix() {
    let base = prefixes(str::to_owned);
    let patched = patched_prefixes("ghostty-vt/0001-kitty-placement-namespace-and-parent.patch");
    assert_ne!(patched.ghostty_vt, base.ghostty_vt);
    assert_eq!(patched.zmx, base.zmx);
}

#[test]
fn a_zmx_bump_or_patch_leaves_the_vt_prefix() {
    let base = prefixes(str::to_owned);
    let bumped = prefixes(|pins| bump(pins, "ZMX_REV", "0000000000000000000000000000000000000000"));
    assert_ne!(bumped.zmx, base.zmx);
    assert_eq!(bumped.ghostty_vt, base.ghostty_vt);
    let patched = patched_prefixes("zmx/0001-explicit-leadership.patch");
    assert_ne!(patched.zmx, base.zmx);
    assert_eq!(patched.ghostty_vt, base.ghostty_vt);
}
