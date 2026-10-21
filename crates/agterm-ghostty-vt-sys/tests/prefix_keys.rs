//! The Ghostty pin keys its prefix alone: bumping a `LIBGHOSTTY_VT_` pin (the library, its Zig, or the Ghostty
//! resources vendored beside it) or a libghostty-vt patch moves the libghostty-vt prefix, and another build's pin or
//! patches leave it where it is.
//!
//! Each case runs the real `--print-prefix` of the build script over a copy of the repository's scripts, pins and
//! patches with one pin line edited; nothing is fetched or built. The script runs with the bash the build script
//! uses (`script.rs`, which finds Git for Windows's on Windows).

#[path = "../script.rs"]
mod script;

use std::fs;
use std::path::{Path, PathBuf};

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
    for script in ["lib/native.sh", "build-ghostty-vt.sh"] {
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

fn print_prefix(checkout: &Path) -> String {
    // the same vendor root for every checkout, so only the keys can differ
    script::run(
        &checkout.join("scripts/build-ghostty-vt.sh"),
        &["--print-prefix"],
        &[("AGTERM_VENDOR", Path::new("/agterm-vendor"))],
    )
    .unwrap_or_else(|err| panic!("build-ghostty-vt.sh: {err}"))
}

fn vt_prefix(edit: impl Fn(&str) -> String) -> String {
    print_prefix(checkout(edit).path())
}

/// The prefix after `patch` changed one file of the checkout's `build/patches/` (pins unedited).
fn patched_prefix(patch: &str) -> String {
    let dir = checkout(str::to_owned);
    let path = dir.path().join("build/patches").join(patch);
    let mut text = fs::read_to_string(&path).unwrap();
    text.push_str("\n# edited\n");
    fs::write(&path, text).unwrap();
    print_prefix(dir.path())
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
fn the_prefix_is_named_after_its_build() {
    let base = vt_prefix(str::to_owned);
    assert!(base.starts_with("/agterm-vendor/ghostty-vt-"), "{base}");
}

#[test]
fn a_libghostty_vt_pin_bump_moves_the_prefix() {
    let base = vt_prefix(str::to_owned);
    for (name, value) in [
        ("LIBGHOSTTY_VT_REV", "0".repeat(40)),
        ("LIBGHOSTTY_VT_ZIG_VERSION", "0.16.1".to_owned()),
        // the resources live in the vt prefix: a new theme package rebuilds it
        ("LIBGHOSTTY_VT_THEMES_SHA256", "0".repeat(64)),
    ] {
        assert_ne!(vt_prefix(|pins| bump(pins, name, &value)), base, "{name}");
    }
}

#[test]
fn a_libghostty_vt_patch_moves_the_prefix() {
    let base = vt_prefix(str::to_owned);
    assert_ne!(
        patched_prefix("ghostty-vt/0001-kitty-placement-namespace-and-parent.patch"),
        base
    );
}

#[test]
fn other_builds_pins_and_patches_leave_the_prefix() {
    let base = vt_prefix(str::to_owned);
    for (name, value) in [
        ("HERDR_REV", "0".repeat(40)),
        ("GIR_REV", "0".repeat(40)),
        ("WPE_ARCH_PACKAGE_SHA256", "0".repeat(64)),
        ("LAVAPIPE_ARCH_PACKAGE_SHA256", "0".repeat(64)),
        ("WESTON_SHA256", "0".repeat(64)),
        ("FONTS_JETBRAINS_MONO_SHA256", "0".repeat(64)),
    ] {
        assert_eq!(vt_prefix(|pins| bump(pins, name, &value)), base, "{name}");
    }
    assert_eq!(patched_prefix("gir/0001-version-from-env.patch"), base);
}

/// What the build script reads: the repository's own prefix, in the host's native form (on Windows cygpath's
/// `C:/...`, not MSYS's `/c/...`), named as the script names it.
#[test]
fn the_build_script_takes_the_scripts_prefix_in_native_form() {
    if std::env::var_os(script::PREFIX_ENV).is_some() {
        return; // the override wins over the script: nothing of the script's to compare
    }
    let printed = script::run(&repo_root().join(script::SCRIPT), &["--print-prefix"], &[]).unwrap();
    let prefix = script::prefix(&repo_root()).unwrap();
    assert!(prefix.is_absolute(), "{}", prefix.display());
    assert_eq!(prefix.file_name(), Path::new(&printed).file_name());
    assert_eq!(prefix, script::native(&printed).unwrap());
}
