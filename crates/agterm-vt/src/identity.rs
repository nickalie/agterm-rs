//! The identity the terminal reports, which is libghostty-vt's at the pin: the names of its terminfo entry, the
//! library's version and the token XTVERSION answers with when the embedder reports no version.
//!
//! Ghostty: no file of its own; each value names where Ghostty sets it at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-identity/src/lib.rs` at `RUSTTY_REV` (MIT).
//! Changes: rustty's identity is undone. rustty reports its own (`xterm-rustty`, `librustty`, version `0.1.0`) and
//! Ghostty's only under its parity feature `identity-oracle`; here there is no profile, and every value is the one
//! libghostty-vt reports. Only those values are kept: rustty's application identity (the app name and version, the
//! bundle id, `TERM`, `TERM_PROGRAM`, the config directory and file, the environment prefix, the application's
//! XTVERSION answer, the website and issue URLs) belongs to Ghostty's app, not to its VT library, and agterm sets its
//! own panes' environment. The responses that carry them are the stream handler's to build, which is not ported yet.

/// The terminfo entry's first name, which `TERM` must equal for tcell-based programs (see the entry's names).
/// Ghostty: `src/terminfo/ghostty.zig`, the first of `.names`.
pub(crate) const TERMINFO_PRIMARY_NAME: &str = "xterm-ghostty";

/// The terminfo entry's preferred, short name. Ghostty: `src/terminfo/ghostty.zig` ("The preferred name").
pub(crate) const TERMINFO_PREFERRED_NAME: &str = "ghostty";

/// The terminfo entry's formal name, the capitalized product name. Ghostty: `src/terminfo/ghostty.zig` ("Our
/// "formal" name").
pub(crate) const TERMINFO_FORMAL_NAME: &str = "Ghostty";

/// The VT library's version, which Ghostty keeps apart from the application's (`1.3.2-dev` at the pin). Ghostty:
/// `build.zig` (`const lib_version = "0.1.0-dev"`, "We use a separate version from the app"), parsed as a semantic
/// version in `src/build/Config.zig` when no `-Dlib-version-string` is given; agterm's build passed none, so this is
/// what libghostty-vt's `GHOSTTY_BUILD_INFO_VERSION_STRING` reported.
pub(crate) const VT_LIB_VERSION: &str = "0.1.0-dev";

/// What XTVERSION (`CSI > 0 q`) reports, as `ESC P > | libghostty ESC \`, when the embedder reports no version (or an
/// empty one). Ghostty: `src/terminal/stream_terminal.zig`, `reportXtversion`.
pub(crate) const XTVERSION_FALLBACK: &str = "libghostty";

#[cfg(test)]
mod tests {
    use super::{
        TERMINFO_FORMAL_NAME, TERMINFO_PREFERRED_NAME, TERMINFO_PRIMARY_NAME, VT_LIB_VERSION, XTVERSION_FALLBACK,
    };

    #[test]
    fn the_identity_is_libghostty_vts() {
        assert_eq!(TERMINFO_PRIMARY_NAME, "xterm-ghostty");
        assert_eq!(TERMINFO_PREFERRED_NAME, "ghostty");
        assert_eq!(TERMINFO_FORMAL_NAME, "Ghostty");
        assert_eq!(VT_LIB_VERSION, "0.1.0-dev");
        assert_eq!(XTVERSION_FALLBACK, "libghostty");
    }

    #[test]
    fn nothing_reports_rusttys_identity() {
        for value in [
            TERMINFO_PRIMARY_NAME,
            TERMINFO_PREFERRED_NAME,
            TERMINFO_FORMAL_NAME,
            XTVERSION_FALLBACK,
        ] {
            assert!(!value.to_ascii_lowercase().contains("rustty"), "{value}");
        }
        // rustty's own library version is 0.1.0, without Ghostty's pre-release tag
        assert_ne!(VT_LIB_VERSION, "0.1.0");
    }

    #[test]
    fn the_library_version_is_a_semantic_version() {
        let (core, pre) = VT_LIB_VERSION.split_once('-').unwrap_or((VT_LIB_VERSION, ""));
        let parts: Vec<&str> = core.split('.').collect();
        assert_eq!(parts.len(), 3);
        assert!(
            parts
                .iter()
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        );
        assert_eq!(pre, "dev");
    }
}
