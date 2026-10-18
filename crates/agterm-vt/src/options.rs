//! The terminal's resolved build options, which the rest of the terminal branches on.
//!
//! Ghostty: the `terminal_options` module `build_options.zig`'s `Options.add` generates, at `LIBGHOSTTY_VT_REV`
//! (`src/build/GhosttyZig.zig` builds libghostty-vt with `.artifact = .lib`).
//! rustty: `crates/rustty-vt/src/options.rs` at `RUSTTY_REV` (MIT).
//! Changes: the artifact is libghostty-vt's (`Lib`), not rustty's application (`Ghostty`), because this crate stands
//! in for libghostty-vt and must behave as it does; the runtime artifact switch rustty's C layer flips is gone (there
//! is one artifact). Kept are only the constants the ported code branches on; the feature flags are always on and
//! not represented, and the version constants come with the embedder layer.

use crate::build_options::Artifact;

/// The artifact this crate is: libghostty-vt.
pub(crate) const ARTIFACT: Artifact = Artifact::Lib;

/// Whether the "slow" runtime safety checks run: integrity checks that are too slow for a production build. On in
/// builds with debug assertions, as Ghostty enables them in its Debug builds.
pub(crate) const SLOW_RUNTIME_SAFETY: bool = cfg!(debug_assertions);

#[cfg(test)]
mod tests {
    use super::{ARTIFACT, SLOW_RUNTIME_SAFETY};
    use crate::build_options::Artifact;

    #[test]
    fn the_artifact_is_libghostty_vt() {
        assert_eq!(ARTIFACT, Artifact::Lib);
        assert_ne!(ARTIFACT, Artifact::Ghostty);
    }

    #[test]
    fn slow_runtime_safety_follows_debug_assertions() {
        assert_eq!(SLOW_RUNTIME_SAFETY, cfg!(debug_assertions));
    }
}
