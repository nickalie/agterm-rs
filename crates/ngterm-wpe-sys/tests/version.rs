//! Links the three WPE WebKit -sys crates against the WPE WebKit the build found (the system's, else the vendored
//! package of `scripts/fetch-wpe.sh`) and checks its version against the API they were generated for:
//! `WPE_API_VERSION` of `build/pins.env`, the `min_cfg_version` of every `Gir.toml`.

use std::ffi::c_uint;
use std::path::Path;

/// `WPE_API_VERSION` of `build/pins.env` as (major, minor).
fn bound_api() -> (c_uint, c_uint) {
    let pins = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../build/pins.env");
    let pins = std::fs::read_to_string(&pins).expect("build/pins.env is readable");
    let value = pins
        .lines()
        .find_map(|line| line.strip_prefix("WPE_API_VERSION="))
        .expect("build/pins.env pins WPE_API_VERSION")
        .trim_matches('"');
    let (major, minor) = value.split_once('.').expect("WPE_API_VERSION is MAJOR.MINOR");
    (
        major.parse().expect("a major version"),
        minor.parse().expect("a minor version"),
    )
}

/// The runtime version of each namespace, read through its own crate.
fn runtime_versions() -> [(&'static str, (c_uint, c_uint, c_uint)); 3] {
    // SAFETY: the version getters take no arguments and only read constants compiled into the library.
    let webkit = unsafe {
        (
            ngterm_wpe_sys::webkit_get_major_version(),
            ngterm_wpe_sys::webkit_get_minor_version(),
            ngterm_wpe_sys::webkit_get_micro_version(),
        )
    };
    // SAFETY: as above.
    let platform = unsafe {
        (
            wpe_platform_sys::wpe_platform_get_major_version(),
            wpe_platform_sys::wpe_platform_get_minor_version(),
            wpe_platform_sys::wpe_platform_get_micro_version(),
        )
    };
    // SAFETY: as above.
    let javascriptcore = unsafe {
        (
            wpe_javascriptcore_sys::jsc_get_major_version(),
            wpe_javascriptcore_sys::jsc_get_minor_version(),
            wpe_javascriptcore_sys::jsc_get_micro_version(),
        )
    };
    [
        ("WPEWebKit", webkit),
        ("WPEPlatform", platform),
        ("WPEJavaScriptCore", javascriptcore),
    ]
}

#[test]
fn the_linked_wpe_webkit_is_the_api_the_bindings_were_generated_for() {
    let (major, minor) = bound_api();
    for (namespace, version) in runtime_versions() {
        assert_eq!(
            version.0, major,
            "{namespace} {version:?}: another major version than the bound {major}.{minor}"
        );
        assert!(
            version.1 >= minor,
            "{namespace} {version:?} is older than the bound {major}.{minor}"
        );
    }
}

#[test]
fn every_namespace_comes_from_one_wpe_webkit_build() {
    let [webkit, platform, javascriptcore] = runtime_versions();
    assert_eq!(webkit.1, platform.1, "{webkit:?} vs {platform:?}");
    assert_eq!(webkit.1, javascriptcore.1, "{webkit:?} vs {javascriptcore:?}");
}

#[test]
fn the_bound_api_is_the_pinned_release_series() {
    let pins = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../build/pins.env");
    let pins = std::fs::read_to_string(&pins).expect("build/pins.env is readable");
    let pinned = pins
        .lines()
        .find_map(|line| line.strip_prefix("WPE_VERSION="))
        .expect("build/pins.env pins WPE_VERSION")
        .trim_matches('"');
    let (major, minor) = bound_api();
    assert!(
        pinned.starts_with(&format!("{major}.{minor}.")),
        "WPE_VERSION {pinned} is not in the WPE_API_VERSION {major}.{minor} series the bindings are generated from"
    );
}
