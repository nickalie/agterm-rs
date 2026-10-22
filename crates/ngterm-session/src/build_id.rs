//! The session server's build id: what a server reports in its welcome and a client compares with the build it would
//! start, to hand a running server's sessions over to that build (`crate::server::handoff`).
//!
//! The id is the hash of the staged `ngterm-session` binary, which the stager writes beside it once the binary is in
//! place, as `share/ngterm/`[`STAGED_FILE`] of the staged root (`bin/` holding the binary):
//!
//! ```text
//! <id>
//! binary <size in bytes> <modification time, nanoseconds since the Unix epoch>
//! ```
//!
//! The second line ties the id to the binary it was written for: a server reads the file once, at its start, and
//! takes the id only when its own executable has that size and modification time (on Linux also only when that path
//! names the running image, `/proc/self/exe`); a client takes it as the build it would start only on the same terms.
//! A binary replaced without its id written after it (an update caught halfway, a stager that does not keep to this)
//! then never passes as the old build: the server reports an unverified id that equals no other
//! ([`unverified`]), which can only cause a handoff to the staged build, never hide a stale one. The match is on
//! metadata, not content, so two different binaries of one size and one modification time to the nanosecond would
//! pass for each other: a stager that preserves timestamps (`install -p`, `cp -p`) must not be paired with
//! same-sized builds. Hashing the binary at run time would close that, at the cost of reading the whole binary at
//! every start (debug builds are about 100 MB).
//!
//! An executable with no staged id beside it (a cargo build run in place, as the tests do) reports [`UNSTAGED`], and a
//! client starting one expects no build: no handoff is ever started for it.
//!
//! A stager publishes the binary by renaming it into place, then the id file the same way (Task 25's `stage.sh`).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// The staged id, relative to the staged root.
pub const STAGED_FILE: &str = "share/ngterm/session-BUILD";

/// The id of an executable with no staged id beside it.
pub const UNSTAGED: &str = concat!("ngterm-session ", env!("CARGO_PKG_VERSION"));

/// The staged id beside a binary, with the binary's stat it was written for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Staged {
    pub id: String,
    pub binary: Stat,
}

/// A binary's size and modification time, which tie a staged id to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat {
    pub size: u64,
    /// Nanoseconds since the Unix epoch.
    pub modified: u128,
}

impl Stat {
    /// The stat of the file at `path` (following links).
    ///
    /// # Errors
    ///
    /// It could not be read, or it was modified before the Unix epoch.
    pub fn of(path: &Path) -> io::Result<Self> {
        let metadata = fs::metadata(path)?;
        let modified = metadata
            .modified()?
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        Ok(Self {
            size: metadata.len(),
            modified,
        })
    }
}

impl Staged {
    /// The file's text as the stager writes it.
    #[must_use]
    pub fn render(&self) -> String {
        format!("{}\nbinary {} {}\n", self.id, self.binary.size, self.binary.modified)
    }

    /// The id file's `text`; `None` unless it holds a non-blank id and a well-formed `binary` line.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        let id = lines.next()?.trim();
        let mut binary = lines.next()?.split_whitespace();
        if id.is_empty() || binary.next()? != "binary" {
            return None;
        }
        let size = binary.next()?.parse().ok()?;
        let modified = binary.next()?.parse().ok()?;
        binary.next().is_none().then(|| Self {
            id: id.to_owned(),
            binary: Stat { size, modified },
        })
    }

    /// The id staged beside `executable` (in `share/ngterm` of the directory above its `bin`), unverified; `None` when
    /// there is none or it does not parse.
    #[must_use]
    pub fn beside(executable: &Path) -> Option<Self> {
        Self::parse(&fs::read_to_string(staged_file(executable)?).ok()?)
    }

    /// Write `id` beside `executable` for the binary as it is now (what a stager does once the binary is in place),
    /// through a temporary file renamed into place.
    ///
    /// # Errors
    ///
    /// The binary could not be read, or the file written.
    pub fn write_beside(executable: &Path, id: &str) -> io::Result<Self> {
        let staged = Self {
            id: id.to_owned(),
            binary: Stat::of(executable)?,
        };
        let file = staged_file(executable).ok_or_else(|| io::Error::other("the executable has no staged root"))?;
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = file.with_extension("tmp");
        fs::write(&temporary, staged.render())?;
        fs::rename(&temporary, &file)?;
        Ok(staged)
    }
}

/// The id file beside `executable`: `share/ngterm/session-BUILD` of the directory above the one holding it.
fn staged_file(executable: &Path) -> Option<PathBuf> {
    Some(executable.parent()?.parent()?.join(STAGED_FILE))
}

/// The build a client expects of a server started from `executable`: the staged id when it is the one written for that
/// binary as it is now; `None` without one (no build check), or when it was written for another binary (an update
/// caught halfway: no check this time, rather than a handoff to a build the id does not name).
#[must_use]
pub fn expected(executable: &Path) -> Option<String> {
    let staged = Staged::beside(executable)?;
    (Stat::of(executable).ok()? == staged.binary).then_some(staged.id)
}

/// The id of a server process that failed to verify its staged id (`<UNSTAGED> unverified <pid>`): unique to the
/// process, so it equals no build a client expects.
#[must_use]
pub fn unverified(pid: u32) -> String {
    format!("{UNSTAGED} unverified {pid}")
}

/// This process's build id, read once at its start: the staged id beside its executable when it verifies (the
/// executable's stat before and after reading it, and on Linux the running image, all match it), [`UNSTAGED`] without
/// one, else [`unverified`].
#[must_use]
pub fn of_this_process() -> String {
    let Ok(executable) = std::env::current_exe() else {
        return UNSTAGED.to_owned();
    };
    let before = Stat::of(&executable).ok();
    let Some(staged) = Staged::beside(&executable) else {
        return UNSTAGED.to_owned();
    };
    let after = Stat::of(&executable).ok();
    let verified = before == Some(staged.binary) && after == before && is_running_image(&executable);
    if verified {
        staged.id
    } else {
        unverified(std::process::id())
    }
}

/// Whether `executable` names the image this process runs: on Linux its device and inode equal `/proc/self/exe`'s
/// (which names the running image even once the path was replaced); elsewhere taken as true, the stat match standing
/// alone.
fn is_running_image(executable: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt as _;
        let identity = |path: &Path| fs::metadata(path).ok().map(|metadata| (metadata.dev(), metadata.ino()));
        identity(executable).is_some_and(|path| identity(Path::new("/proc/self/exe")) == Some(path))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = executable;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn staged(id: &str, size: u64, modified: u128) -> Staged {
        Staged {
            id: id.to_owned(),
            binary: Stat { size, modified },
        }
    }

    #[test]
    fn the_file_round_trips_and_a_malformed_one_is_none() {
        let file = staged("0123abcd", 12_345, 1_700_000_000_123_456_789);
        assert_eq!(file.render(), "0123abcd\nbinary 12345 1700000000123456789\n");
        assert_eq!(Staged::parse(&file.render()), Some(file));
        for text in [
            "",
            "id\n",
            " \nbinary 1 2\n",
            "id\nbinery 1 2\n",
            "id\nbinary x 2\n",
            "id\nbinary 1\n",
            "id\nbinary 1 2 3\n",
        ] {
            assert_eq!(Staged::parse(text), None, "{text:?}");
        }
        // surrounding blanks of the id do not count
        assert_eq!(
            Staged::parse("  id \nbinary 1 2").map(|staged| staged.id),
            Some("id".to_owned())
        );
    }

    #[test]
    fn a_staged_id_is_expected_only_for_the_binary_it_was_written_for() {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("bin").join("ngterm-session");
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::write(&executable, b"build a").unwrap();
        assert_eq!(expected(&executable), None, "nothing staged: no check");
        let written = Staged::write_beside(&executable, "a").unwrap();
        assert_eq!(Staged::beside(&executable), Some(written));
        assert_eq!(expected(&executable), Some("a".to_owned()));
        // the binary replaced, its id not (yet): no build is expected
        fs::write(&executable, b"build b, longer").unwrap();
        assert_eq!(expected(&executable), None);
        Staged::write_beside(&executable, "b").unwrap();
        assert_eq!(expected(&executable), Some("b".to_owned()));
        assert_eq!(
            fs::read_to_string(root.path().join(STAGED_FILE))
                .unwrap()
                .lines()
                .next(),
            Some("b")
        );
    }

    #[test]
    fn an_unverified_id_is_its_process_own() {
        assert_ne!(unverified(1), unverified(2));
        assert!(unverified(7).starts_with(UNSTAGED));
        assert!(UNSTAGED.starts_with("ngterm-session "));
    }

    #[test]
    fn this_unstaged_test_binary_reports_the_unstaged_id() {
        assert_eq!(of_this_process(), UNSTAGED);
    }
}
