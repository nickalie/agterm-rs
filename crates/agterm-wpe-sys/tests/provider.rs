//! Where the build takes WPE WebKit from: `scripts/pkg-config.sh` prefers the system's `wpe-webkit-2.0` when it is at
//! least `WPE_API_VERSION`, else a complete vendored prefix of `scripts/fetch-wpe.sh`; that prefix is keyed by the
//! `WPE_` pins alone.
//!
//! Every case runs the real scripts over a scratch copy of the repository's scripts and pins, with a fake vendored
//! prefix and fake system `.pc` files; `PKG_CONFIG_LIBDIR` hides the machine's own, so nothing here depends on what
//! the host has installed. Nothing is fetched.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A scratch repository holding the scripts and pins the provider logic reads, with `edit` applied to pins.env.
fn checkout(edit: impl Fn(&str) -> String) -> tempfile::TempDir {
    let root = repo_root();
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("scripts/lib")).unwrap();
    fs::create_dir_all(dir.path().join("build")).unwrap();
    for script in ["lib/native.sh", "pkg-config.sh", "fetch-wpe.sh", "build-ghostty-vt.sh"] {
        fs::copy(
            root.join("scripts").join(script),
            dir.path().join("scripts").join(script),
        )
        .unwrap();
    }
    let pins = fs::read_to_string(root.join("build/pins.env")).unwrap();
    fs::write(dir.path().join("build/pins.env"), edit(&pins)).unwrap();
    dir
}

/// Run `script ARGS` of `checkout` with AGTERM_VENDOR at `vendor` and pkg-config searching only `system_pc`.
fn run(checkout: &Path, vendor: &Path, system_pc: &Path, script: &str, args: &[&str]) -> Output {
    Command::new("bash")
        .arg(checkout.join("scripts").join(script))
        .args(args)
        .env("AGTERM_VENDOR", vendor)
        .env("PKG_CONFIG_LIBDIR", system_pc)
        .env_remove("PKG_CONFIG_PATH")
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn write_pc(dir: &Path, prefix: &Path, version: &str) {
    fs::create_dir_all(dir).unwrap();
    let pc = format!(
        "prefix={}\nlibdir=${{prefix}}/lib\nincludedir=${{prefix}}/include\n\nName: WPE WebKit\nDescription: fake\n\
         Version: {version}\nLibs: -L${{libdir}} -lWPEWebKit-2.0\nCflags: -I${{includedir}}/wpe-webkit-2.0\n",
        prefix.display()
    );
    fs::write(dir.join("wpe-webkit-2.0.pc"), pc).unwrap();
}

/// Fill the vendored prefix `fetch-wpe.sh --print-prefix` names with the files a complete one has (empty stand-ins)
/// and the pin manifest of `checkout`.
fn fake_vendored_prefix(checkout: &Path, vendor: &Path, system_pc: &Path) -> PathBuf {
    let prefix = PathBuf::from(stdout(&run(
        checkout,
        vendor,
        system_pc,
        "fetch-wpe.sh",
        &["--print-prefix"],
    )));
    for file in [
        "lib/libWPEWebKit-2.0.so.1",
        "include/wpe-webkit-2.0/wpe/webkit.h",
        "share/gir-1.0/WPEWebKit-2.0.gir",
        "lib/wpe-webkit-2.0/WPEWebProcess",
    ] {
        let path = prefix.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "stand-in\n").unwrap();
    }
    let process = prefix.join("lib/wpe-webkit-2.0/WPEWebProcess");
    let mut permissions = fs::metadata(&process).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    fs::set_permissions(&process, permissions).unwrap();
    write_pc(&prefix.join("lib/pkgconfig"), &prefix, "2.54.1");
    let pins = fs::read_to_string(checkout.join("build/pins.env")).unwrap();
    let wpe_lines: String = pins
        .lines()
        .filter(|line| line.starts_with("WPE_"))
        .map(|line| format!("{line}\n"))
        .collect();
    fs::create_dir_all(prefix.join("share/ngterm")).unwrap();
    fs::write(prefix.join("share/ngterm/wpe-PINS"), wpe_lines).unwrap();
    prefix
}

/// The prefix of the `wpe-webkit-2.0` the shim resolves.
fn resolved_prefix(checkout: &Path, vendor: &Path, system_pc: &Path) -> Output {
    run(
        checkout,
        vendor,
        system_pc,
        "pkg-config.sh",
        &["--variable=prefix", "wpe-webkit-2.0"],
    )
}

struct Scene {
    checkout: tempfile::TempDir,
    scratch: tempfile::TempDir,
}

impl Scene {
    fn new() -> Self {
        Self {
            checkout: checkout(str::to_owned),
            scratch: tempfile::tempdir().unwrap(),
        }
    }
    fn vendor(&self) -> PathBuf {
        self.scratch.path().join("vendor")
    }
    fn system_pc(&self) -> PathBuf {
        let dir = self.scratch.path().join("system-pc");
        fs::create_dir_all(&dir).unwrap();
        dir
    }
    fn system_prefix(&self) -> PathBuf {
        self.scratch.path().join("system")
    }
}

#[test]
fn a_complete_vendored_prefix_serves_a_host_without_wpe() {
    let scene = Scene::new();
    let prefix = fake_vendored_prefix(scene.checkout.path(), &scene.vendor(), &scene.system_pc());
    assert_eq!(
        stdout(&run(
            scene.checkout.path(),
            &scene.vendor(),
            &scene.system_pc(),
            "fetch-wpe.sh",
            &["--verify"]
        )),
        ""
    );
    let resolved = stdout(&resolved_prefix(
        scene.checkout.path(),
        &scene.vendor(),
        &scene.system_pc(),
    ));
    assert_eq!(resolved, prefix.display().to_string());
}

#[test]
fn a_system_wpe_of_the_bound_api_wins_over_the_vendored_prefix() {
    let scene = Scene::new();
    fake_vendored_prefix(scene.checkout.path(), &scene.vendor(), &scene.system_pc());
    write_pc(&scene.system_pc(), &scene.system_prefix(), "2.56.0");
    let resolved = stdout(&resolved_prefix(
        scene.checkout.path(),
        &scene.vendor(),
        &scene.system_pc(),
    ));
    assert_eq!(resolved, scene.system_prefix().display().to_string());
}

#[test]
fn the_vendored_prefix_wins_over_an_older_system_wpe() {
    let scene = Scene::new();
    let prefix = fake_vendored_prefix(scene.checkout.path(), &scene.vendor(), &scene.system_pc());
    write_pc(&scene.system_pc(), &scene.system_prefix(), "2.50.4");
    let resolved = stdout(&resolved_prefix(
        scene.checkout.path(),
        &scene.vendor(),
        &scene.system_pc(),
    ));
    assert_eq!(resolved, prefix.display().to_string());
}

#[test]
fn an_incomplete_vendored_prefix_is_not_offered() {
    let scene = Scene::new();
    let prefix = fake_vendored_prefix(scene.checkout.path(), &scene.vendor(), &scene.system_pc());
    // a manifest of other pins: the prefix was unpacked from another package
    fs::write(prefix.join("share/ngterm/wpe-PINS"), "WPE_VERSION=\"2.52.0\"\n").unwrap();
    let verify = run(
        scene.checkout.path(),
        &scene.vendor(),
        &scene.system_pc(),
        "fetch-wpe.sh",
        &["--verify"],
    );
    assert_eq!(verify.status.code(), Some(1));
    let resolved = resolved_prefix(scene.checkout.path(), &scene.vendor(), &scene.system_pc());
    assert!(
        !resolved.status.success(),
        "{}",
        String::from_utf8_lossy(&resolved.stdout)
    );
}

/// The WPE and libghostty-vt prefixes of a checkout whose pins.env went through `edit`.
fn prefixes(edit: impl Fn(&str) -> String) -> (String, String) {
    let dir = checkout(edit);
    let vendor = Path::new("/agterm-vendor");
    let pc = Path::new("/nonexistent");
    (
        stdout(&run(dir.path(), vendor, pc, "fetch-wpe.sh", &["--print-prefix"])),
        stdout(&run(dir.path(), vendor, pc, "build-ghostty-vt.sh", &["--print-prefix"])),
    )
}

/// Replace the value of the one `NAME="..."` line in pins.env.
fn bump(pins: &str, name: &str, value: &str) -> String {
    let prefix = format!("{name}=");
    assert_eq!(
        pins.lines().filter(|line| line.starts_with(&prefix)).count(),
        1,
        "one {name} line"
    );
    pins.lines()
        .map(|line| {
            if line.starts_with(&prefix) {
                format!("{name}=\"{value}\"")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

#[test]
fn a_wpe_pin_bump_moves_only_the_wpe_prefix() {
    let (wpe, vt) = prefixes(str::to_owned);
    assert!(wpe.starts_with("/agterm-vendor/wpe-"), "{wpe}");
    let (bumped_wpe, bumped_vt) = prefixes(|pins| bump(pins, "WPE_ARCH_PACKAGE_SHA256", &"0".repeat(64)));
    assert_ne!(bumped_wpe, wpe);
    assert_eq!(bumped_vt, vt);
}

#[test]
fn other_pin_bumps_leave_the_wpe_prefix() {
    let (wpe, _) = prefixes(str::to_owned);
    for name in ["GIR_REV", "LIBGHOSTTY_VT_REV", "ZMX_REV"] {
        let (bumped_wpe, _) = prefixes(|pins| bump(pins, name, &"0".repeat(40)));
        assert_eq!(bumped_wpe, wpe, "{name}");
    }
}
