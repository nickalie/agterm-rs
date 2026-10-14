//! Compiles the WPE shim against the WPE WebKit prefix (`WPE_PREFIX`, default `.cache/prefix/usr`, the extracted
//! Arch `wpewebkit` package) and links `libWPEWebKit-2.0`.

use std::path::PathBuf;
use std::process::Command;

fn pkg_config_cflags(packages: &[&str]) -> Vec<String> {
    let out = Command::new("pkg-config").arg("--cflags").args(packages).output().expect("pkg-config");
    assert!(out.status.success(), "pkg-config --cflags {packages:?} failed");
    String::from_utf8(out.stdout).unwrap().split_whitespace().map(str::to_owned).collect()
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let prefix = std::env::var("WPE_PREFIX")
        .map(PathBuf::from)
        .unwrap_or_else(|_| manifest.join(".cache/prefix/usr"));
    let include = prefix.join("include/wpe-webkit-2.0");
    assert!(include.is_dir(), "no WPE headers at {} (run scripts/fetch-wpe.sh)", include.display());

    let mut build = cc::Build::new();
    build.file("csrc/wpe_shim.c").include(&include).include(include.join("wpe-platform")).warnings(true);
    for flag in pkg_config_cflags(&["glib-2.0", "gobject-2.0", "gio-2.0", "libsoup-3.0"]) {
        build.flag(&flag);
    }
    build.compile("wpe_shim");

    println!("cargo:rustc-link-search=native={}", prefix.join("lib").display());
    println!("cargo:rustc-link-lib=dylib=WPEWebKit-2.0");
    for lib in ["gobject-2.0", "glib-2.0", "gio-2.0"] {
        println!("cargo:rustc-link-lib=dylib={lib}");
    }
    println!("cargo:rerun-if-changed=csrc/wpe_shim.c");
    println!("cargo:rerun-if-env-changed=WPE_PREFIX");
}
