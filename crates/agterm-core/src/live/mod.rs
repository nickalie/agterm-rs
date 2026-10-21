//! Live sessions, the host-free half (upstream's `ZmxSupport.swift`, `ZmxReplayScript.swift`,
//! `ZmxLifecycle.swift`, `ZmxInventory.swift`, `LiveReset.swift`, `ZmxBuildRecord.swift`, `ZmxLead.swift`). The session
//! server that hosts the sessions in place of upstream's zmx and the host's launch inputs live in the `agterm-session`
//! crate, which implements [`agterm_host::SessionHost`] over these.
//!
//! - this module: a pane's daemon name and back, the instance's socket directory and the attach contract's variables;
//! - [`support`]: the wrap configuration of a pane, its rejections, the launch disposition and the attach command;
//! - [`replay`]: the shell script a wrapped pane runs a captured or creation command with;
//! - [`shell_integration`]: the shell integration a wrapped zsh, bash, fish or PowerShell login shell starts with;
//! - [`lifecycle`]: the leader map, the launch reap policy and the refresh gate;
//! - [`inventory`]: pane claims joined against the observed daemons, and the prune policy;
//! - [`reset`]: Reset Live Sessions: selection, the one-shot marker, next-launch narrowing and the outcome;
//! - [`build_record`]: when this state directory first launched the bundled session server build;
//! - [`lead`]: which client of a daemon leads its grid, from the role reports in terminal titles;
//! - [`screen`]: the last lines of a session's screen read;
//! - [`sweep`]: the foreground job a killed daemon's shell leaves behind.

pub mod build_record;
pub mod inventory;
pub mod lead;
pub mod lifecycle;
pub mod replay;
pub mod reset;
pub mod screen;
pub mod shell_integration;
pub mod support;
pub mod sweep;

use std::path::{Component, Path, PathBuf};

use uuid::Uuid;

use crate::paths::APP_DIR_NAME;

/// The prefix of every daemon name this app emits.
pub const NAME_PREFIX: &str = "agterm-";

/// The attach contract's variable naming the namespace directory every `agterm-session` command reaches, which the
/// server also sets in each session's programs (`agterm_session::cli`).
pub const DIRECTORY_VARIABLE: &str = "AGTERM_LIVE_DIR";
/// The variable the server sets in each session's programs to the session's name; an attach run where it is set is
/// refused, so an attach line clears it (empty means unset).
pub const SESSION_VARIABLE: &str = "AGTERM_LIVE_SESSION";
/// The prefix the `agterm-session` commands put before every session name they are given.
pub const SESSION_PREFIX_VARIABLE: &str = "AGTERM_LIVE_SESSION_PREFIX";
/// Set, the attach's detach key (`Ctrl+\`) goes to the program.
pub const NO_DETACH_KEY_VARIABLE: &str = "AGTERM_LIVE_NO_DETACH_KEY";

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

/// The socket directory of the instance whose state directory is `state_dir`, named by the FNV-1a 64 of the resolved
/// state directory, so two spellings of one directory share a namespace: [`unix_socket_directory`] on Unix,
/// [`windows_socket_directory`] under the process environment's local application data on Windows.
#[must_use]
pub fn socket_directory(state_dir: &Path) -> String {
    let hash = state_dir_hash(state_dir);
    #[cfg(unix)]
    {
        unix_socket_directory(&hash)
    }
    #[cfg(windows)]
    {
        windows_socket_directory(&crate::paths::ProcessEnv, &hash)
    }
}

/// The FNV-1a 64 of `state_dir` resolved ([`resolved_state_dir`]), over its bytes as the OS encodes them.
#[must_use]
pub fn state_dir_hash(state_dir: &Path) -> String {
    fnv1a64(resolved_state_dir(state_dir).as_os_str().as_encoded_bytes())
}

/// The Unix socket directory of the state directory hashed `hash` ([`state_dir_hash`]): `/tmp/ngterm-live-<hash>`,
/// upstream's shape (`/tmp/agterm-zmx-<hash>`, `ZmxSupport.socketDirectory`) under this build's name and the session
/// server's group, so the harness derives it the same way.
#[must_use]
pub fn unix_socket_directory(hash: &str) -> String {
    format!("/tmp/{APP_DIR_NAME}-live-{hash}")
}

/// The Windows namespace directory of the state directory hashed `hash` ([`state_dir_hash`]):
/// `%LOCALAPPDATA%\ngterm\live\<hash>`, per user as `/tmp` is not; `%USERPROFILE%\AppData\Local` when
/// `LOCALAPPDATA` is unset or empty, then the profile directory the OS reports ([`crate::paths::Env::passwd_home`]),
/// and the system's temporary directory with no profile at all. The live-session server derives its pipe from this
/// path (`agterm_session::endpoint::pipe_name`). Spelled with `\` on every OS, so a test anywhere reads the
/// Windows form.
#[must_use]
pub fn windows_socket_directory(env: &dyn crate::paths::Env, hash: &str) -> String {
    let local = |base: PathBuf| {
        base.as_os_str()
            .to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .to_owned()
    };
    let profile_local = |profile: PathBuf| format!(r"{}\AppData\Local", local(profile));
    let non_empty = |key: &str| env.var(key).filter(|value| !value.is_empty()).map(PathBuf::from);
    let base = non_empty("LOCALAPPDATA")
        .map(local)
        .or_else(|| non_empty("USERPROFILE").map(profile_local))
        .or_else(|| env.passwd_home().map(profile_local))
        .unwrap_or_else(|| local(std::env::temp_dir()));
    format!(r"{base}\{APP_DIR_NAME}\live\{hash}")
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
        if let Ok(resolved) = existing.canonicalize() {
            let mut resolved = without_verbatim_prefix(resolved);
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

/// `path` as canonicalized: Unix's spelling is the path itself.
#[cfg(unix)]
fn without_verbatim_prefix(path: PathBuf) -> PathBuf {
    path
}

/// `path` as canonicalized, without the verbatim prefix Windows' canonicalization adds (`\\?\C:\x` is `C:\x`,
/// `\\?\UNC\server\share` is `\\server\share`), as Python's `realpath` spells it, so the namespace hash is the one
/// of the path a user writes. Any other verbatim form (a device, a volume GUID) has no plainer spelling and stays.
#[cfg(windows)]
fn without_verbatim_prefix(path: PathBuf) -> PathBuf {
    use std::path::Prefix;

    let mut components = path.components();
    let plain = match components.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::VerbatimDisk(letter) => format!("{}:", char::from(letter)),
            Prefix::VerbatimUNC(server, share) => {
                format!(r"\\{}\{}", server.to_string_lossy(), share.to_string_lossy())
            }
            _ => return path,
        },
        _ => return path,
    };
    let mut out = PathBuf::from(plain);
    out.extend(components);
    out
}

/// FNV-1a 64 of `bytes` as 16 lower-case hex digits (upstream's `stableHash`).
#[must_use]
pub fn fnv1a64(bytes: &[u8]) -> String {
    let hash = bytes.iter().fold(14_695_981_039_346_656_037_u64, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(1_099_511_628_211)
    });
    format!("{hash:016x}")
}

/// An executable file named `name` for a test's wrap, which [`support::is_executable_file`] accepts: `/bin/<name>` on
/// Unix (`sh`, `echo` and `cat` exist on Linux and macOS alike); on Windows an empty `<name>.exe` written into `dir`,
/// since an extension is all it asks there.
#[cfg(test)]
pub(crate) fn test_executable(dir: &Path, name: &str) -> PathBuf {
    #[cfg(unix)]
    {
        let _ = dir;
        Path::new("/bin").join(name)
    }
    #[cfg(windows)]
    {
        let path = dir.join(name).with_extension("exe");
        std::fs::write(&path, b"").unwrap();
        path
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn the_socket_directory_hashes_as_upstreams_algorithm() {
        // the FNV-1a 64 reference values, and the harness's `upstream_socket_directory` (vocabulary.py) of this dir
        assert_eq!(fnv1a64(b""), "cbf29ce484222325");
        assert_eq!(fnv1a64(b"a"), "af63dc4c8601ec8c");
        assert_eq!(fnv1a64(b"foobar"), "85944171f73967e8");
        let state = Path::new("/nonexistent-agterm/state");
        #[cfg(unix)]
        assert_eq!(socket_directory(state), "/tmp/ngterm-live-22c4fb0b9e3e04dc");
        assert_eq!(
            socket_directory(Path::new("/nonexistent-agterm/./x/../state")),
            socket_directory(state)
        );
    }

    #[test]
    fn the_unix_socket_directory_is_upstreams_shape() {
        assert_eq!(
            unix_socket_directory("22c4fb0b9e3e04dc"),
            "/tmp/ngterm-live-22c4fb0b9e3e04dc"
        );
        #[cfg(unix)]
        {
            let state = Path::new("/nonexistent-agterm/state");
            assert_eq!(socket_directory(state), unix_socket_directory(&state_dir_hash(state)));
        }
    }

    #[test]
    fn the_windows_namespace_is_under_the_local_application_data() {
        use crate::paths::MapEnv;

        let hash = "0123456789abcdef";
        let local = MapEnv::new().with("LOCALAPPDATA", r"C:\Users\u\AppData\Local");
        assert_eq!(
            windows_socket_directory(&local, hash),
            r"C:\Users\u\AppData\Local\ngterm\live\0123456789abcdef"
        );
        let profile = MapEnv::new()
            .with("LOCALAPPDATA", "")
            .with("USERPROFILE", r"C:\Users\u")
            .with_passwd_home(r"D:\other");
        assert_eq!(
            windows_socket_directory(&profile, hash),
            r"C:\Users\u\AppData\Local\ngterm\live\0123456789abcdef",
            "an empty LOCALAPPDATA falls back to the profile's"
        );
        let reported = MapEnv::new().with_passwd_home(r"D:\home\u");
        assert_eq!(
            windows_socket_directory(&reported, hash),
            r"D:\home\u\AppData\Local\ngterm\live\0123456789abcdef"
        );
        assert!(
            windows_socket_directory(&MapEnv::new(), hash).ends_with(r"\ngterm\live\0123456789abcdef"),
            "no profile at all: the temporary directory"
        );
        #[cfg(windows)]
        {
            let state = Path::new(r"C:\nonexistent-agterm\state");
            let process = windows_socket_directory(&crate::paths::ProcessEnv, &state_dir_hash(state));
            assert_eq!(socket_directory(state), process);
            if let Some(local) = std::env::var_os("LOCALAPPDATA").filter(|value| !value.is_empty()) {
                assert!(
                    process.starts_with(local.to_str().unwrap().trim_end_matches('\\')),
                    "{process}"
                );
            }
        }
    }

    #[cfg(unix)]
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

    /// Windows spellings of one directory: its case, `/` separators, the verbatim prefix and, where this account may
    /// create one (developer mode, an administrator), a directory symlink.
    #[cfg(windows)]
    #[test]
    fn canonical_state_paths_share_a_namespace() {
        let root = tempfile::tempdir().unwrap();
        let real = root.path().join("Real");
        fs::create_dir_all(&real).unwrap();
        let canonical = real.canonicalize().unwrap();
        let plain = without_verbatim_prefix(canonical.clone());
        let spellings = [
            PathBuf::from(plain.to_str().unwrap().to_uppercase()),
            PathBuf::from(plain.to_str().unwrap().replace('\\', "/")),
            canonical.clone(),
        ];
        for spelling in &spellings {
            assert_eq!(
                socket_directory(spelling),
                socket_directory(&real),
                "{}",
                spelling.display()
            );
        }
        assert_eq!(resolved_state_dir(&real), plain, "no verbatim prefix");
        // a state directory not created yet resolves through its existing parent
        assert_eq!(
            socket_directory(&PathBuf::from(plain.to_str().unwrap().to_uppercase()).join("not-yet")),
            socket_directory(&canonical.join("not-yet"))
        );
        let link = root.path().join("link");
        if std::os::windows::fs::symlink_dir(&real, &link).is_ok() {
            assert_eq!(socket_directory(&link), socket_directory(&real));
        }
    }

    #[cfg(windows)]
    #[test]
    fn the_verbatim_prefix_of_a_disk_or_share_is_dropped() {
        assert_eq!(
            without_verbatim_prefix(PathBuf::from(r"\\?\C:\Users\u\state")),
            Path::new(r"C:\Users\u\state")
        );
        assert_eq!(
            without_verbatim_prefix(PathBuf::from(r"\\?\UNC\server\share\state")),
            Path::new(r"\\server\share\state")
        );
        for kept in [
            r"\\?\Volume{0f0e0d0c-0b0a-0908-0706-050403020100}\state",
            r"C:\Users\u\state",
        ] {
            assert_eq!(without_verbatim_prefix(PathBuf::from(kept)), Path::new(kept));
        }
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
