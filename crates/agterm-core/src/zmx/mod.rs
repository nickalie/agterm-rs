//! Live sessions over zmx, the host-free half (upstream's `ZmxSupport.swift`, `ZmxReplayScript.swift`,
//! `ZmxLifecycle.swift`, `ZmxInventory.swift`, `LiveReset.swift`, `ZmxBuildRecord.swift`, `ZmxLead.swift`). Running the
//! zmx binary and collecting the host's launch inputs live in the `agterm-zmx` crate, which implements
//! [`agterm_host::SessionHost`] over these.
//!
//! - this module: a pane's daemon name and back, and the instance's socket directory;
//! - [`support`]: the wrap configuration of a pane, its rejections, the launch disposition and the attach command;
//! - [`replay`]: the shell script a wrapped pane runs a captured or creation command with;
//! - [`lifecycle`]: the `zmx list` parser, the leader map, the launch reap policy and the refresh gate;
//! - [`inventory`]: pane claims joined against the observed daemons, and the prune policy;
//! - [`reset`]: Reset Live Sessions: selection, the one-shot marker, next-launch narrowing and the outcome;
//! - [`build_record`]: when this state directory first launched the bundled zmx build;
//! - [`lead`]: which client of a daemon leads its grid, from the role reports in terminal titles;
//! - [`screen`]: the `zmx screen` reply.

pub mod build_record;
pub mod inventory;
pub mod lead;
pub mod lifecycle;
pub mod replay;
pub mod reset;
pub mod screen;
pub mod support;

use std::path::{Component, Path, PathBuf};

use uuid::Uuid;

/// The prefix of every daemon name this app emits.
pub const NAME_PREFIX: &str = "agterm-";

/// The daemon of pane `pane_identity`: the prefix and the identity's 32 lower-case hex digits.
#[must_use]
pub fn daemon_name(pane_identity: Uuid) -> String {
    format!("{NAME_PREFIX}{}", pane_identity.simple())
}

/// The pane identity [`daemon_name`] built `name` from; `None` for any other name.
#[must_use]
pub fn pane_identity(name: &str) -> Option<Uuid> {
    is_daemon_name(name)
        .then(|| Uuid::try_parse(&name[NAME_PREFIX.len()..]).ok())
        .flatten()
}

/// Whether `name` is one of this app's daemons: exactly the shape [`daemon_name`] emits. A prefix test is not enough:
/// the namespace is a shared `/tmp` directory, so a user's own session called `agterm-notes` would otherwise read as an
/// unclaimed app daemon and be pruned. ASCII digits only: fullwidth ones are hex to Unicode, never emitted.
#[must_use]
pub fn is_daemon_name(name: &str) -> bool {
    name.strip_prefix(NAME_PREFIX).is_some_and(|body| {
        body.len() == 32
            && body
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// The socket directory of the instance whose state directory is `state_dir`: `/tmp/agterm-zmx-<FNV-1a 64>` of the
/// resolved state directory, so two spellings of one directory share a namespace.
#[must_use]
pub fn socket_directory(state_dir: &Path) -> String {
    let resolved = resolved_state_dir(state_dir);
    format!("/tmp/agterm-zmx-{}", fnv1a64(resolved.as_os_str().as_encoded_bytes()))
}

/// `state_dir` as upstream resolves it (`standardizedFileURL.resolvingSymlinksInPath()`): absolute, `.` and `..`
/// removed lexically, then every symlink of the part that exists resolved; a part that does not exist yet is kept as
/// written, as Python's non-strict `realpath` keeps it (the conformance harness hashes that).
#[must_use]
pub fn resolved_state_dir(state_dir: &Path) -> PathBuf {
    resolve_existing_prefix(&standardized_path(state_dir))
}

/// `path` as `URL(fileURLWithPath:).standardizedFileURL` spells it: absolute against the working directory, `.`
/// dropped and each `..` taking the component before it, never above the root. Lexical: no symlink is read.
pub(crate) fn standardized_path(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut out = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if out.parent().is_some() {
                    out.pop();
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// `path` with its longest existing ancestor canonicalized and the rest appended as written.
fn resolve_existing_prefix(path: &Path) -> PathBuf {
    let mut existing = path;
    let mut rest = Vec::new();
    loop {
        if let Ok(mut resolved) = existing.canonicalize() {
            resolved.extend(rest.iter().rev());
            return resolved;
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name);
                existing = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// FNV-1a 64 of `bytes` as 16 lower-case hex digits (upstream's `stableHash`).
#[must_use]
pub fn fnv1a64(bytes: &[u8]) -> String {
    let hash = bytes.iter().fold(14_695_981_039_346_656_037_u64, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(1_099_511_628_211)
    });
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn the_socket_directory_hashes_as_upstreams_algorithm() {
        // the FNV-1a 64 reference values, and `zmx_socket_directory` of the harness (impls/base.py) for this state dir
        assert_eq!(fnv1a64(b""), "cbf29ce484222325");
        assert_eq!(fnv1a64(b"a"), "af63dc4c8601ec8c");
        assert_eq!(fnv1a64(b"foobar"), "85944171f73967e8");
        assert_eq!(
            socket_directory(Path::new("/nonexistent-agterm/state")),
            "/tmp/agterm-zmx-22c4fb0b9e3e04dc"
        );
        assert_eq!(
            socket_directory(Path::new("/nonexistent-agterm/./x/../state")),
            socket_directory(Path::new("/nonexistent-agterm/state"))
        );
    }

    #[test]
    fn canonical_state_paths_share_a_namespace() {
        let root = tempfile::tempdir().unwrap();
        let real = root.path().join("real");
        let link = root.path().join("link");
        fs::create_dir_all(&real).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(socket_directory(&real), socket_directory(&link));
        // a state directory not created yet resolves through its existing parent
        assert_eq!(
            socket_directory(&link.join("not-yet")),
            socket_directory(&real.canonicalize().unwrap().join("not-yet"))
        );
    }

    #[test]
    fn a_daemon_name_round_trips_to_its_pane_identity() {
        let pane = Uuid::parse_str("ABCDEF01-2345-6789-ABCD-EF0123456789").unwrap();
        let name = daemon_name(pane);
        assert_eq!(name, "agterm-abcdef0123456789abcdef0123456789");
        assert!(is_daemon_name(&name));
        assert_eq!(pane_identity(&name), Some(pane));
        let fresh = Uuid::new_v4();
        assert_eq!(pane_identity(&daemon_name(fresh)), Some(fresh));
    }

    #[test]
    fn only_the_generated_shape_is_a_daemon_name() {
        let fullwidth = format!("agterm-{}", "\u{FF19}".repeat(32));
        for name in [
            "agterm-not-ours",
            "agterm-",
            "agterm-00112233445566778899aabbccddeef",
            "agterm-00112233445566778899aabbccddeeff0",
            "agterm-00112233445566778899AABBCCDDEEFF",
            "agterm-00112233-4455-6677-8899-aabbccddeeff",
            "notes",
            fullwidth.as_str(),
        ] {
            assert!(!is_daemon_name(name), "{name}");
            assert_eq!(pane_identity(name), None, "{name}");
        }
    }
}
