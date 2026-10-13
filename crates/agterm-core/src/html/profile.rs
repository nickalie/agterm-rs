//! The persistent browser store's id of one state directory (upstream's `BrowserProfile.swift`). The web engine keeps
//! an identified store keyed by this id alone, so the id file is what keeps two state directories on separate stores.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use uuid::Uuid;

/// The id file's name in the state directory (upstream's `BrowserProfile.filename`).
pub const FILENAME: &str = "browser-profile";

/// Ids the web engine keeps for itself: it raises an exception for either instead of returning an error (upstream's
/// `BrowserProfile.reserved`).
const RESERVED: [Uuid; 2] = [Uuid::nil(), Uuid::from_u128(1)];

/// Why the profile id cannot be read or written (upstream's `BrowserProfile.Failure`, plus the file-system error
/// upstream's `identifier()` rethrows as is).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProfileFailure {
    #[error("browser profile cannot be read: {0}")]
    Unreadable(String),
    #[error("browser profile is not a UUID: {0}")]
    Malformed(String),
    /// A new id could not be written; the file system's own error text.
    #[error("{0}")]
    Unwritable(String),
}

/// The persistent browser store of one agterm state directory (upstream's `BrowserProfile`).
#[derive(Debug, Clone)]
pub struct BrowserProfile {
    file: PathBuf,
}

impl BrowserProfile {
    /// The profile kept in `directory` (the state directory).
    #[must_use]
    pub fn new(directory: &Path) -> Self {
        Self {
            file: directory.join(FILENAME),
        }
    }

    /// The id file.
    #[must_use]
    pub fn file(&self) -> &Path {
        &self.file
    }

    /// The profile id without creating one; `None` only when the file does not exist (upstream's
    /// `existingIdentifier`). A file that cannot be read or holds anything else is an error and is left alone: a
    /// regenerated id would orphan the store holding every saved login.
    ///
    /// # Errors
    ///
    /// [`ProfileFailure::Unreadable`] for a file that cannot be read (a lookup refused for lack of search permission
    /// included), [`ProfileFailure::Malformed`] for one that holds anything but a UUID the web engine accepts.
    pub fn existing_identifier(&self) -> Result<Option<Uuid>, ProfileFailure> {
        let data = match fs::read(&self.file) {
            Ok(data) => data,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(ProfileFailure::Unreadable(self.path())),
        };
        parse(String::from_utf8_lossy(&data).trim())
            .filter(|id| !RESERVED.contains(id))
            .map(Some)
            .ok_or_else(|| ProfileFailure::Malformed(self.path()))
    }

    /// The profile id, writing a new one (`<UPPERCASE-UUID>\n`, atomically) only when the file does not exist
    /// (upstream's `identifier`).
    ///
    /// # Errors
    ///
    /// As [`BrowserProfile::existing_identifier`], or [`ProfileFailure::Unwritable`] when the new file cannot be
    /// written.
    pub fn identifier(&self) -> Result<Uuid, ProfileFailure> {
        if let Some(id) = self.existing_identifier()? {
            return Ok(id);
        }
        let id = Uuid::new_v4();
        self.write(id)
            .map_err(|error| ProfileFailure::Unwritable(error.to_string()))?;
        Ok(id)
    }

    /// Writes `id` through a temporary file renamed over the id file, as Foundation's `.atomic` write does, creating
    /// the directory first.
    fn write(&self, id: Uuid) -> io::Result<()> {
        let directory = self.file.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(directory)?;
        let temporary = directory.join(format!(".{FILENAME}.{}.tmp", Uuid::new_v4().simple()));
        let written = fs::File::create(&temporary).and_then(|mut file| {
            file.write_all(format!("{}\n", crate::pane_env::uuid_string(id)).as_bytes())?;
            file.sync_all()
        });
        let renamed = written.and_then(|()| fs::rename(&temporary, &self.file));
        if renamed.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        renamed
    }

    fn path(&self) -> String {
        self.file.display().to_string()
    }
}

/// Whether the saved browser store may be emptied now, the refusals in upstream's `clearPersistentStore` order: a
/// clear already running, then the persistent pages still holding the store (`open_pages`, a soft-closed one
/// included: its login lives in memory and would be written back), then a missing or unreadable profile. `Ok(true)`
/// when a store exists to empty, `Ok(false)` when the profile was never created: nothing to remove, and none is
/// created for it.
///
/// # Errors
///
/// The refusal's text ([`super::errors`] or the profile's [`ProfileFailure`]).
pub fn clear_check(clearing: bool, open_pages: usize, profile: Option<&BrowserProfile>) -> Result<bool, String> {
    if clearing {
        return Err(super::errors::CLEARING.to_owned());
    }
    if open_pages > 0 {
        return Err(super::errors::pages_open(open_pages));
    }
    let profile = profile.ok_or_else(|| super::errors::PERSISTENT_UNAVAILABLE.to_owned())?;
    profile
        .existing_identifier()
        .map(|id| id.is_some())
        .map_err(|failure| failure.to_string())
}

/// A UUID as Foundation's `UUID(uuidString:)` reads one: the hyphenated 36-character form only, in either case.
fn parse(text: &str) -> Option<Uuid> {
    let hyphenated = text.len() == 36
        && text
            .bytes()
            .enumerate()
            .all(|(index, byte)| matches!(index, 8 | 13 | 18 | 23) == (byte == b'-'));
    hyphenated.then(|| Uuid::parse_str(text).ok()).flatten()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn a_missing_file_creates_an_identifier_that_later_reads_return() {
        let dir = tempfile::tempdir().unwrap();
        let profile = BrowserProfile::new(dir.path());
        assert_eq!(profile.existing_identifier(), Ok(None));

        let first = profile.identifier().unwrap();

        assert_eq!(profile.existing_identifier(), Ok(Some(first)));
        assert_eq!(profile.identifier(), Ok(first));
        assert_eq!(BrowserProfile::new(dir.path()).identifier(), Ok(first));
        let written = fs::read_to_string(profile.file()).unwrap();
        assert_eq!(written, format!("{}\n", crate::pane_env::uuid_string(first)));
        assert_eq!(
            fs::read_dir(dir.path()).unwrap().count(),
            1,
            "no temporary file left behind"
        );
    }

    #[test]
    fn two_directories_get_different_identifiers() {
        let one = tempfile::tempdir().unwrap();
        let two = tempfile::tempdir().unwrap();
        assert_ne!(
            BrowserProfile::new(one.path()).identifier().unwrap(),
            BrowserProfile::new(two.path()).identifier().unwrap()
        );
    }

    #[test]
    fn a_missing_directory_reads_as_no_profile_and_is_created_on_demand() {
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("state");
        assert_eq!(BrowserProfile::new(&dir).existing_identifier(), Ok(None));

        let id = BrowserProfile::new(&dir).identifier().unwrap();

        assert_eq!(BrowserProfile::new(&dir).existing_identifier(), Ok(Some(id)));
    }

    #[test]
    fn a_malformed_file_is_an_error_and_stays_as_it_was() {
        for content in [
            "",
            "not-a-uuid",
            "123",
            "00000000-0000-0000-0000-000000000000",
            "00000000-0000-0000-0000-000000000001",
            "6ba7b8109dad11d180b400c04fd430c8",
            "{6ba7b810-9dad-11d1-80b4-00c04fd430c8}",
            "urn:uuid:6ba7b810-9dad-11d1-80b4-00c04fd430c8",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join(FILENAME);
            fs::write(&file, content).unwrap();
            let malformed = Err(ProfileFailure::Malformed(file.display().to_string()));

            assert_eq!(BrowserProfile::new(dir.path()).identifier(), malformed, "{content}");
            assert_eq!(
                BrowserProfile::new(dir.path()).existing_identifier(),
                malformed.map(Some),
                "{content}"
            );
            assert_eq!(fs::read_to_string(&file).unwrap(), content);
        }
    }

    #[test]
    fn an_unreadable_file_is_an_error_and_stays_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(FILENAME);
        fs::create_dir(&file).unwrap();

        assert_eq!(
            BrowserProfile::new(dir.path()).identifier(),
            Err(ProfileFailure::Unreadable(file.display().to_string()))
        );
        assert!(file.is_dir());
    }

    /// A lookup that fails for lack of search permission is not a missing profile.
    #[test]
    fn a_profile_behind_an_unsearchable_directory_is_an_error_not_a_missing_profile() {
        let dir = tempfile::tempdir().unwrap();
        let profile = BrowserProfile::new(dir.path());
        let id = profile.identifier().unwrap();
        let unreadable = Err(ProfileFailure::Unreadable(profile.file().display().to_string()));
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o600)).unwrap();

        let existing = profile.existing_identifier();
        let created = profile.identifier();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();

        assert_eq!(existing, unreadable.clone().map(Some));
        assert_eq!(created, unreadable);
        assert_eq!(profile.existing_identifier(), Ok(Some(id)));
    }

    #[test]
    fn a_trailing_newline_and_lowercase_are_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        fs::write(
            dir.path().join(FILENAME),
            format!("{}\n", crate::pane_env::uuid_string(id)),
        )
        .unwrap();
        assert_eq!(BrowserProfile::new(dir.path()).identifier(), Ok(id));
        fs::write(dir.path().join(FILENAME), format!("  {}\n", id.hyphenated())).unwrap();
        assert_eq!(BrowserProfile::new(dir.path()).identifier(), Ok(id));
    }

    #[test]
    fn a_failure_names_the_file() {
        assert_eq!(
            ProfileFailure::Unreadable("/s/browser-profile".to_owned()).to_string(),
            "browser profile cannot be read: /s/browser-profile"
        );
        assert_eq!(
            ProfileFailure::Malformed("/s/browser-profile".to_owned()).to_string(),
            "browser profile is not a UUID: /s/browser-profile"
        );
    }

    /// Upstream's `clearPersistentStore` order: clearing, open pages, no profile, then the profile file itself.
    #[test]
    fn a_clear_is_refused_in_upstream_order() {
        let dir = tempfile::tempdir().unwrap();
        let profile = BrowserProfile::new(dir.path());
        fs::write(profile.file(), "garbage").unwrap();
        assert_eq!(
            clear_check(true, 2, Some(&profile)),
            Err("browser storage is being cleared".to_owned())
        );
        assert_eq!(
            clear_check(false, 1, Some(&profile)),
            Err("1 persistent page still open".to_owned())
        );
        assert_eq!(
            clear_check(false, 3, None),
            Err("3 persistent pages still open".to_owned())
        );
        assert_eq!(
            clear_check(false, 0, None),
            Err("persistent browser storage is not available".to_owned())
        );
        assert_eq!(
            clear_check(false, 0, Some(&profile)),
            Err(format!("browser profile is not a UUID: {}", profile.file().display()))
        );
    }

    #[test]
    fn a_clear_without_a_profile_creates_none() {
        let dir = tempfile::tempdir().unwrap();
        let profile = BrowserProfile::new(dir.path());
        assert_eq!(clear_check(false, 0, Some(&profile)), Ok(false));
        assert!(!profile.file().exists());
        let id = profile.identifier().unwrap();
        assert_eq!(clear_check(false, 0, Some(&profile)), Ok(true));
        assert_eq!(
            profile.existing_identifier(),
            Ok(Some(id)),
            "a clear keeps the profile id"
        );
    }

    #[test]
    fn a_clear_behind_an_unreadable_profile_names_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let profile = BrowserProfile::new(dir.path());
        fs::create_dir(profile.file()).unwrap();
        assert_eq!(
            clear_check(false, 0, Some(&profile)),
            Err(format!("browser profile cannot be read: {}", profile.file().display()))
        );
    }
}
