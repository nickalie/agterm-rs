//! Which socket `ngtermctl` talks to, and what a failed connect says about the instance behind it.
//!
//! Resolution is upstream's `BasicOptions.socketPath` (`agtermctlKit/Commands.swift`, over
//! `ControlResolve.socketPath`): `--socket`, then `<AGTERM_STATE_DIR>/ngterm.sock`, then the state directory's default,
//! then `/tmp/ngterm/ngterm.sock`. Upstream's default is macOS's `~/Library/Application Support/agterm`; the Linux one
//! is `<XDG data home>/ngterm` (Linux glue: Foundation's application support directory on Linux, which the fork's
//! `PersistenceStore.defaultDirectory` resolves, under this build's own directory name, `agterm_core::paths`). The CLI
//! shares the app's state-dir code but, as upstream's, never reads the app's socket variables: neither
//! `AGTERM_CONTROL_SOCKET` nor a pane's `AGTERM_SOCKET` routes a command anywhere.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use agterm_core::paths::{Env, ownership_lock_path, socket_in, state_dir};

/// The socket a command connects to: `option` (`--socket`) when given, else `<state dir>/ngterm.sock`.
#[must_use]
pub fn resolve(option: Option<&Path>, env: &dyn Env) -> PathBuf {
    option.map_or_else(|| socket_in(&state_dir(env)), Path::to_path_buf)
}

/// A device number split as `/proc/locks` and `/proc/self/mountinfo` print it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Device {
    major: u64,
    minor: u64,
}

impl Device {
    /// Splits `st_dev` as glibc's `gnu_dev_major`/`gnu_dev_minor`.
    fn from_dev(dev: u64) -> Self {
        Self {
            major: ((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff),
            minor: (dev & 0xff) | ((dev >> 12) & !0xff),
        }
    }

    /// `/proc/locks` names a file `MAJ:MIN:INODE`, the device numbers in hex.
    fn lock_key(self, ino: u64) -> String {
        format!("{:02x}:{:02x}:{ino}", self.major, self.minor)
    }
}

/// Whether a process holds the server's ownership `flock` on `<socket>.lock`, `None` when that cannot be answered.
/// Linux keeps `flock` and POSIX locks apart, so `F_GETLK` never sees it; `/proc/locks` observes it without competing
/// for it (taking a shared lock to test would fail a starting instance's own `LOCK_EX|LOCK_NB`).
///
/// `/proc/locks` prints the superblock's device, which differs from `st_dev` on btrfs (a subvolume reports its own
/// anonymous device), so the device of the mount holding the file counts too.
///
/// One read of `/proc/locks` can miss a held lock: the table is a `seq_file`, each `read` returns at most a page and
/// the next resumes the walk by position, so a lock released ahead of the cursor between two reads shifts the rest
/// back and one entry goes unlisted. Only [`LOCKS_READS`] reads that all miss it count as not held.
#[must_use]
pub fn ownership_lock_held(socket: &Path) -> Option<bool> {
    let lock = ownership_lock_path(socket);
    let metadata = fs::metadata(&lock).ok()?;
    let mut devices = vec![Device::from_dev(metadata.dev())];
    let mount = fs::canonicalize(&lock)
        .ok()
        .zip(fs::read_to_string("/proc/self/mountinfo").ok())
        .and_then(|(path, mountinfo)| mount_device(&mountinfo, &path));
    devices.extend(mount);
    let keys: Vec<String> = devices.iter().map(|device| device.lock_key(metadata.ino())).collect();
    for _ in 0..LOCKS_READS {
        let table = fs::read_to_string("/proc/locks").ok()?;
        if keys.iter().any(|key| flock_listed(&table, key)) {
            return Some(true);
        }
    }
    Some(false)
}

/// How many whole reads of `/proc/locks` [`ownership_lock_held`] makes before it calls a lock not held. A miss needs
/// the lock just past a page edge while locks ahead of it go between two `read`s. Measured with a held lock among
/// hundreds of others taken and released on 12 cores at once: 1 read in 55 missed it, 3 in a row 8 times in 477k
/// reads, 5 in a row never.
const LOCKS_READS: usize = 5;

/// The device of the mount that holds `path` in `mountinfo` (the text of `/proc/self/mountinfo`): the longest mount
/// point that is a prefix of `path`, the later line on a tie (a mount over another). A line reads
/// `id parent MAJ:MIN root mountpoint ...`, decimal device numbers, octal escapes in the mount point.
fn mount_device(mountinfo: &str, path: &Path) -> Option<Device> {
    let mut best: Option<(usize, Device)> = None;
    for line in mountinfo.lines() {
        let fields: Vec<&str> = line.split(' ').collect();
        let (Some(device), Some(point)) = (fields.get(2), fields.get(4)) else {
            continue;
        };
        let point = unescape_octal(point);
        let Some((major, minor)) = device.split_once(':') else {
            continue;
        };
        let (Ok(major), Ok(minor)) = (major.parse(), minor.parse()) else {
            continue;
        };
        if path.starts_with(&point) && best.is_none_or(|(len, _)| point.len() >= len) {
            best = Some((point.len(), Device { major, minor }));
        }
    }
    best.map(|(_, device)| device)
}

/// Undoes mountinfo's `\ooo` escapes (space, tab, newline, backslash).
fn unescape_octal(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    let mut rest = field;
    while let Some(at) = rest.find('\\') {
        out.push_str(&rest[..at]);
        let code = rest
            .get(at + 1..at + 4)
            .and_then(|digits| u8::from_str_radix(digits, 8).ok());
        if let Some(code) = code {
            out.push(char::from(code));
            rest = &rest[at + 4..];
        } else {
            out.push('\\');
            rest = &rest[at + 1..];
        }
    }
    out.push_str(rest);
    out
}

/// Whether `table` (the text of `/proc/locks`) holds a granted `flock` on the file `key` (`MAJ:MIN:INODE`). An entry
/// reads `N: FLOCK ADVISORY WRITE pid MAJ:MIN:INODE start end`; a blocked waiter's carries `->` after the ordinal, so
/// it never matches.
fn flock_listed(table: &str, key: &str) -> bool {
    table.lines().any(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        fields.len() > 5 && fields[1] == "FLOCK" && fields[5] == key
    })
}

#[cfg(test)]
mod tests {
    use std::fs::File;

    use agterm_core::paths::{CONTROL_SOCKET_ENV, MapEnv, STATE_DIR_ENV};

    use super::*;

    fn env() -> MapEnv {
        MapEnv::new()
            .with("HOME", "/home/u")
            .with("XDG_DATA_HOME", "/data")
            .with("AGTERM_SOCKET", "/pane/ngterm.sock")
            .with(CONTROL_SOCKET_ENV, "/app/ngterm.sock")
    }

    #[test]
    fn socket_option_wins() {
        let env = env().with(STATE_DIR_ENV, "/state");
        assert_eq!(resolve(Some(Path::new("/x/s.sock")), &env), Path::new("/x/s.sock"));
    }

    #[test]
    fn state_dir_comes_next() {
        let env = env().with(STATE_DIR_ENV, "/state");
        assert_eq!(resolve(None, &env), Path::new("/state/ngterm.sock"));
    }

    #[test]
    fn then_the_xdg_data_home_then_home_then_tmp() {
        assert_eq!(resolve(None, &env()), Path::new("/data/ngterm/ngterm.sock"));
        let no_xdg = MapEnv::new().with("HOME", "/home/u");
        assert_eq!(
            resolve(None, &no_xdg),
            Path::new("/home/u/.local/share/ngterm/ngterm.sock")
        );
        assert_eq!(resolve(None, &MapEnv::new()), Path::new("/tmp/ngterm/ngterm.sock"));
    }

    #[test]
    fn an_empty_state_dir_is_unset_as_the_app_reads_it() {
        let env = env().with(STATE_DIR_ENV, "");
        assert_eq!(resolve(None, &env), Path::new("/data/ngterm/ngterm.sock"));
    }

    #[test]
    fn the_device_splits_as_glibc() {
        // makedev(8, 2) and makedev(259, 0x12345)
        assert_eq!(Device::from_dev(0x802).lock_key(42), "08:02:42");
        let dev = (0x103 << 8) | 0x45 | (0x12300 << 12);
        assert_eq!(Device::from_dev(dev).lock_key(7), "103:12345:7");
    }

    #[test]
    fn only_a_granted_flock_on_the_file_counts() {
        let table = "1: POSIX  ADVISORY  WRITE 10 08:02:42 0 EOF\n\
                     2: FLOCK  ADVISORY  WRITE 11 08:02:43 0 EOF\n\
                     2: -> FLOCK  ADVISORY  WRITE 12 08:02:42 0 EOF\n";
        assert!(!flock_listed(table, "08:02:42"));
        assert!(flock_listed(table, "08:02:43"));
        let granted = format!("{table}3: FLOCK  ADVISORY  WRITE 13 08:02:42 0 EOF\n");
        assert!(flock_listed(&granted, "08:02:42"));
    }

    #[test]
    fn the_innermost_mount_names_the_device() {
        let mountinfo = "22 1 0:31 / / rw - btrfs /dev/x rw\n\
                         40 22 0:59 /@tmp /tmp rw - btrfs /dev/x rw\n\
                         41 22 0:60 / /tmp\\040dir rw - tmpfs tmpfs rw\n\
                         42 40 0:61 / /tmp rw - tmpfs tmpfs rw\n";
        let device = |path: &str| mount_device(mountinfo, Path::new(path));
        assert_eq!(device("/home/u"), Some(Device { major: 0, minor: 31 }));
        assert_eq!(device("/tmp/a/b"), Some(Device { major: 0, minor: 61 }));
        assert_eq!(device("/tmp dir/x"), Some(Device { major: 0, minor: 60 }));
        assert_eq!(device("/tmpfoo"), Some(Device { major: 0, minor: 31 }));
        assert_eq!(mount_device("", Path::new("/x")), None);
    }

    #[test]
    fn octal_escapes_are_undone() {
        assert_eq!(unescape_octal(r"/a\040b\134c\0"), "/a b\\c\\0");
    }

    #[test]
    fn a_held_ownership_lock_is_seen() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("ngterm.sock");
        assert_eq!(ownership_lock_held(&socket), None);
        let lock = File::create(ownership_lock_path(&socket)).unwrap();
        assert_eq!(ownership_lock_held(&socket), Some(false));
        lock.try_lock().unwrap();
        assert_eq!(ownership_lock_held(&socket), Some(true));
        lock.unlock().unwrap();
        assert_eq!(ownership_lock_held(&socket), Some(false));
    }
}
