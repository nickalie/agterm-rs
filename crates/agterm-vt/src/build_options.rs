//! The terminal's build options: the types of the options [`crate::options`] holds.
//!
//! Ghostty: `src/terminal/build_options.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/build_options.rs` at `RUSTTY_REV` (MIT).
//! Changes: only `Artifact` is kept. The feature flags (`Features`, `-Dvt-features`), the SIMD, Oniguruma and C ABI
//! switches and the version parsing are not: every libghostty-vt feature agterm uses is always on here, the cut ones
//! are gone, and the version constants come with the embedder layer. A branch on an always-on feature is dropped
//! where its module is ported, not turned into a flag.

/// The artifact the terminal is built into, which gates some behavior (the Kitty image storage default, which
/// function keys are encoded, a few OSC and mouse details).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Artifact {
    /// The Ghostty application.
    Ghostty,
    /// libghostty-vt, the library agterm embedded before this crate replaced it.
    Lib,
}

#[cfg(test)]
mod tests {
    use super::Artifact;

    #[test]
    fn the_two_artifacts_differ() {
        assert_ne!(Artifact::Ghostty, Artifact::Lib);
        assert_eq!(format!("{:?}", Artifact::Lib), "Lib");
    }
}
