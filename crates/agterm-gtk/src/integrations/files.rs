//! The filesystem side of the installers (upstream's `AgentHooksInstaller` helpers and `AgentHooksInstall.writeFile`):
//! reads that tell an absent file from an unreadable one, mode-preserving atomic writes that go through a symlink to
//! its target, and the package copy.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// A file that exists but cannot be read as UTF-8 text: the installers leave it untouched rather than treat it as
/// absent and write over it with no backup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unreadable;

/// The contents of an existing config file: `None` when absent (a fresh install), the text when readable,
/// [`Unreadable`] when it exists but cannot be read (upstream's `readExistingConfig`). A dangling symlink is absent.
pub fn read_existing(path: &Path) -> Result<Option<String>, Unreadable> {
    match fs::metadata(path) {
        Ok(_) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(Unreadable),
    }
    let bytes = fs::read(path).map_err(|_| Unreadable)?;
    String::from_utf8(bytes).map(Some).map_err(|_| Unreadable)
}

/// Whether anything is at `path`, a dangling symlink included (lstat, as upstream's `attributesOfItem`).
pub fn entry_exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// The permission bits of the file at `path`, `None` when absent or unreadable (upstream's `posixMode`).
pub fn posix_mode(path: &Path) -> Option<u32> {
    fs::metadata(path).ok().map(|meta| meta.permissions().mode() & 0o7777)
}

/// The resolved target when `path` is itself a symlink (the chain followed), else `None` (upstream's `symlinkTarget`).
/// A dangling link resolves one step, so the write recreates its target rather than replacing the link.
pub fn symlink_target(path: &Path) -> Option<PathBuf> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_symlink() {
        return None;
    }
    if let Ok(resolved) = fs::canonicalize(path) {
        return Some(resolved);
    }
    let link = fs::read_link(path).ok()?;
    Some(path.parent().map_or_else(|| link.clone(), |parent| parent.join(&link)))
}

/// Writes `text` to `path` atomically (a sibling temp renamed over it), then applies `mode` when given, so a rewrite
/// keeps a restrictive mode instead of the fresh file's default (upstream's `AgentHooksInstall.writeFile`).
pub fn write_file(text: &str, path: &Path, mode: Option<u32>) -> io::Result<()> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
    let mut temp_name = std::ffi::OsString::from(".");
    temp_name.push(name);
    temp_name.push(format!(".agterm-{}.tmp", std::process::id()));
    let temp = path.with_file_name(temp_name);
    let written = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o666)
            .open(&temp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temp, path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written?;
    if let Some(mode) = mode {
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

/// Writes `text` through an existing symlink to its target, so a dotfiles-managed link survives, keeping the target's
/// mode, so a chmod-600 file is not widened and a script stays executable (upstream's `writePreservingSymlink`).
pub fn write_preserving_symlink(text: &str, path: &Path) -> io::Result<()> {
    let target = write_target(path);
    let mode = posix_mode(&target);
    write_file(text, &target, mode)
}

/// The path a write lands on: a symlink's target, else the path itself.
pub fn write_target(path: &Path) -> PathBuf {
    symlink_target(path).unwrap_or_else(|| path.to_path_buf())
}

/// Copies the directory tree `source` into `destination`, replacing every file the source ships and keeping any other
/// file already there. Modes travel with the files, so the scripts stay executable.
pub fn copy_tree(source: &Path, destination: &Path) -> io::Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            if entry_exists(&to) && !to.is_dir() {
                fs::remove_file(&to)?;
            }
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Removes whatever is at `path`: a symlink or file is unlinked, a directory removed with its contents. Absent is fine.
pub fn remove_entry(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
    }
}

/// Every regular file under `root`, relative to it, sorted: what a package comparison walks.
#[cfg(test)]
pub fn relative_files(root: &Path) -> io::Result<Vec<PathBuf>> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_dir() {
                walk(root, &path, out)?;
            } else if let Ok(relative) = path.strip_prefix(root) {
                out.push(relative.to_path_buf());
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    walk(root, root, &mut files)?;
    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn absent_unreadable_and_readable_files_are_told_apart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        assert_eq!(read_existing(&path), Ok(None));
        fs::write(&path, b"{}").unwrap();
        assert_eq!(read_existing(&path), Ok(Some("{}".to_owned())));
        fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
        assert_eq!(
            read_existing(&path),
            Err(Unreadable),
            "non-UTF-8 is unreadable, not absent"
        );
        let dangling = dir.path().join("dangling");
        symlink(dir.path().join("nowhere"), &dangling).unwrap();
        assert_eq!(read_existing(&dangling), Ok(None));
    }

    #[test]
    fn a_rewrite_keeps_the_mode_and_writes_through_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("dotfiles-settings.json");
        fs::write(&target, "old").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        let link = dir.path().join("settings.json");
        symlink(&target, &link).unwrap();

        write_preserving_symlink("new", &link).unwrap();

        assert!(
            fs::symlink_metadata(&link).unwrap().file_type().is_symlink(),
            "the link survives"
        );
        assert_eq!(fs::read_to_string(&target).unwrap(), "new");
        assert_eq!(posix_mode(&target), Some(0o600));
    }

    #[test]
    fn a_fresh_write_without_a_mode_takes_the_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fresh");
        write_file("x", &path, None).unwrap();
        let mode = posix_mode(&path).unwrap();
        assert_eq!(mode & 0o600, 0o600, "{mode:o}");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1, "no temp file left");
    }

    #[test]
    fn copy_tree_replaces_shipped_files_and_keeps_others() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src");
        fs::create_dir_all(source.join("shell")).unwrap();
        fs::write(source.join("run.sh"), "new").unwrap();
        fs::set_permissions(source.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(source.join("shell/integration.sh"), "rc").unwrap();
        let destination = dir.path().join("dst");
        fs::create_dir_all(&destination).unwrap();
        fs::write(destination.join("run.sh"), "old").unwrap();
        fs::write(destination.join("extra.sh"), "kept").unwrap();

        copy_tree(&source, &destination).unwrap();

        assert_eq!(fs::read_to_string(destination.join("run.sh")).unwrap(), "new");
        assert_eq!(posix_mode(&destination.join("run.sh")), Some(0o755));
        assert_eq!(fs::read_to_string(destination.join("extra.sh")).unwrap(), "kept");
        assert_eq!(
            relative_files(&destination).unwrap(),
            [
                PathBuf::from("extra.sh"),
                "run.sh".into(),
                "shell/integration.sh".into()
            ]
        );
    }
}
