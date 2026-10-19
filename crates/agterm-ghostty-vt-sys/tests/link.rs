//! The link test: the archive the build script built links with nothing beside it, and reports the build the script
//! asked for (`-Doptimize=ReleaseFast`, `-Dsimd=true`, the default feature set).

use agterm_ghostty_vt_sys as sys;

/// A `bool` build-info value.
fn flag(data: sys::GhosttyBuildInfo) -> bool {
    let mut value = false;
    // SAFETY: a bool-valued `GhosttyBuildInfo` writes one `bool` through the pointer.
    let result = unsafe { sys::ghostty_build_info(data, (&raw mut value).cast()) };
    assert_eq!(result, sys::GHOSTTY_SUCCESS, "{data}");
    value
}

#[test]
fn the_archive_links_and_reports_the_requested_build() {
    let mut optimize: sys::GhosttyOptimizeMode = sys::GHOSTTY_OPTIMIZE_DEBUG;
    // SAFETY: `OPTIMIZE` writes one `GhosttyOptimizeMode`.
    let result = unsafe { sys::ghostty_build_info(sys::GHOSTTY_BUILD_INFO_OPTIMIZE, (&raw mut optimize).cast()) };
    assert_eq!(result, sys::GHOSTTY_SUCCESS);
    assert_eq!(optimize, sys::GHOSTTY_OPTIMIZE_RELEASE_FAST);
    assert!(flag(sys::GHOSTTY_BUILD_INFO_SIMD));
    assert!(flag(sys::GHOSTTY_BUILD_INFO_KITTY_GRAPHICS));

    let mut version = sys::GhosttyString {
        ptr: std::ptr::null(),
        len: 0,
    };
    // SAFETY: `VERSION_STRING` writes a `GhosttyString` pointing at static library data.
    let result = unsafe { sys::ghostty_build_info(sys::GHOSTTY_BUILD_INFO_VERSION_STRING, (&raw mut version).cast()) };
    assert_eq!(result, sys::GHOSTTY_SUCCESS);
    assert!(!version.ptr.is_null() && version.len > 0);
}

/// A `major.minor[.patch]` version as numbers, for comparing.
#[cfg(target_os = "macos")]
fn version(text: &str) -> Vec<u32> {
    text.split('.').map(|part| part.parse().unwrap()).collect()
}

/// No object of the archive is built for a newer macOS than the one this test links for (the `-Dtarget` version,
/// rustc's deployment target): Apple's linker would warn "built for newer macOS version", and the archive could call
/// what the older system lacks. Read from each member's `LC_BUILD_VERSION` (`otool -l`).
#[cfg(target_os = "macos")]
#[test]
fn the_archive_is_built_for_the_deployment_target() {
    let target = env!("AGTERM_GHOSTTY_VT_ZIG_TARGET");
    let deployment = target.rsplit_once("-macos.").unwrap().1;
    let output = std::process::Command::new("otool")
        .arg("-l")
        .arg(env!("AGTERM_GHOSTTY_VT_ARCHIVE"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let text = String::from_utf8_lossy(&output.stdout);
    let minimums: Vec<&str> = text
        .lines()
        .filter_map(|line| line.trim().strip_prefix("minos "))
        .collect();
    assert!(!minimums.is_empty(), "no LC_BUILD_VERSION in the archive:\n{text}");
    for minimum in minimums {
        assert!(
            version(minimum) <= version(deployment),
            "a member is built for macOS {minimum}, newer than the deployment target {deployment}"
        );
    }
}
