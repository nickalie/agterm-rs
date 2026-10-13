//! Reading `keymap.conf` from disk (upstream's `KeymapStore.load`) and seeding the starter file when there is none.

use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::Path;

use super::KeymapDiagnostic;
use super::defaults::ShippedDefaults;
use super::parse::parse_keymap;
use super::parsed::Keymap;
use super::starter::starter_keymap_conf;

/// Loads and parses the keymap at `path`. A missing file is the empty keymap with no diagnostics; one that exists
/// but cannot be read as UTF-8 text is the empty keymap with a single line-0 diagnostic.
#[must_use]
pub fn load_keymap(path: &Path, defaults: &ShippedDefaults) -> (Keymap, Vec<KeymapDiagnostic>) {
    let unreadable = |reason: String| {
        let message = format!("could not read keymap.conf: {reason}");
        (Keymap::empty(defaults), vec![KeymapDiagnostic::new(0, message)])
    };
    match fs::read(path) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => parse_keymap(&text, defaults),
            Err(error) => unreadable(error.to_string()),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => (Keymap::empty(defaults), Vec::new()),
        Err(error) => unreadable(error.to_string()),
    }
}

/// Writes the starter `keymap.conf` at `path` when no file is there, creating its directory. Never overwrites: an
/// existing file, even one created concurrently, is left alone. Failures are logged, not returned, since a missing
/// starter only costs the user the commented reference.
pub fn ensure_starter_keymap(path: &Path, defaults: &ShippedDefaults) {
    if let Err(error) = write_starter(path, defaults) {
        tracing::warn!(path = %path.display(), %error, "could not write the starter keymap.conf");
    }
}

fn write_starter(path: &Path, defaults: &ShippedDefaults) -> io::Result<()> {
    if path.try_exists()? {
        return Ok(());
    }
    if let Some(directory) = path.parent() {
        fs::create_dir_all(directory)?;
    }
    let mut file = match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(()),
        Err(error) => return Err(error),
    };
    file.write_all(starter_keymap_conf(defaults).as_bytes())
}
