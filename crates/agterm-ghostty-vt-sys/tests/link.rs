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
