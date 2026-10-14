//! Logging: `tracing` to stderr and to [`LOG_FILE_NAME`] in the state directory, both at the level from
//! [`LOG_ENV`] (default `info`).
//!
//! stderr stays: the conformance harness captures it as the instance log `conformance/tools/ghostty_probe.py`
//! reads. The file follows upstream's own log under the state directory, the session host's `session-host.log`
//! (`SessionHost.paths` in `agtermCore/Sources/agtermCore/SessionHost.swift`), which the host opens append-only,
//! owner-only and without following a symlink (`Host.redirectStandardIO` in
//! `agtermCore/Sources/SessionHostRuntime/Host.swift`) and never rotates; upstream's app itself logs to the unified
//! log and the Linux fork to stderr only. Appending matters here too: a second `agterm` launch runs this before
//! `GApplication` forwards it to the running instance, so a truncation at startup would wipe the live instance's log.
//! The one addition is a bound: a file past [`ROTATE_BYTES`] at startup moves to [`ROTATED_FILE_NAME`], replacing
//! the previous one. That too must spare a running instance, whose log may have grown past the bound since its own
//! startup: the process that logs to the file holds an exclusive `flock` on it for its lifetime, and a launch that
//! finds it held never rotates, nor does one whose file another launch rotated away after it was opened. A file
//! that cannot be opened leaves logging on stderr alone, with a warning there.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::Arc;

use agterm_core::paths::{self, Env};
use tracing::Subscriber;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// The filter directives for both sinks, as `EnvFilter` reads them.
pub const LOG_ENV: &str = "AGTERM_LOG";
/// The level when [`LOG_ENV`] is unset or does not parse.
const DEFAULT_FILTER: &str = "info";
/// The log file inside the state directory, named like the socket beside it (`agterm.sock`).
pub const LOG_FILE_NAME: &str = "agterm.log";
/// The previous log file, kept once a rotation happened.
pub const ROTATED_FILE_NAME: &str = "agterm.log.1";
/// The size past which the log file is rotated at startup.
pub const ROTATE_BYTES: u64 = 8 * 1024 * 1024;

/// Installs the global subscriber: stderr, plus the log file under `env`'s state directory when it opens.
pub fn init(env: &dyn Env) {
    let dir = paths::state_dir(env);
    match open_log_file(&dir, ROTATE_BYTES) {
        Ok(file) => subscriber(filter(env), Some(file)).init(),
        Err(error) => {
            subscriber(filter(env), None).init();
            let path = dir.join(LOG_FILE_NAME);
            tracing::warn!(path = %path.display(), %error, "log file unavailable; logging to stderr only");
        }
    }
}

/// The filter from `env`'s [`LOG_ENV`], or [`DEFAULT_FILTER`].
fn filter(env: &dyn Env) -> EnvFilter {
    env.var(LOG_ENV)
        .and_then(|value| value.into_string().ok())
        .and_then(|directives| EnvFilter::try_new(directives).ok())
        .unwrap_or_else(|| EnvFilter::new(DEFAULT_FILTER))
}

/// stderr and, when given, `file` (without ANSI colors), both behind `filter`.
fn subscriber(filter: EnvFilter, file: Option<Arc<File>>) -> impl Subscriber + Send + Sync {
    let file_layer = file.map(|file| fmt::layer().with_ansi(false).with_writer(file));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(io::stderr))
        .with(file_layer)
}

/// Opens [`LOG_FILE_NAME`] in `state_dir` for appending, creating the directory, and takes the instance lock on it:
/// an exclusive `flock` the returned file holds while it stays open, the whole life of the process that installs it.
/// Only a launch that gets the lock (no running agterm writes this log) moves a file larger than `rotate_bytes` to
/// [`ROTATED_FILE_NAME`] and starts a fresh one, locked in turn. A launch that finds the lock held (a second `agterm`
/// about to be forwarded to the running instance) only appends, so the live instance's file is never renamed under it.
/// A child the last instance forked can still hold that lock until its exec closes the inherited descriptor, or it
/// exits; a launch in that interval only appends too, and the rotation waits for a later launch.
fn open_log_file(state_dir: &Path, rotate_bytes: u64) -> io::Result<Arc<File>> {
    fs::create_dir_all(state_dir)?;
    let file = open_append(&state_dir.join(LOG_FILE_NAME))?;
    settle_log_file(state_dir, file, rotate_bytes)
}

/// How many times [`settle_log_file`] reopens the log after finding the file it opened rotated away.
const OPEN_ATTEMPTS: usize = 3;

/// The rest of [`open_log_file`], given `file` as opened from [`LOG_FILE_NAME`] in `state_dir`: the lock, and the
/// rotation when the lock is had. The lock and the size count only while `file` is still the one at that path: a
/// launch that stalls between its open and its lock can find the file it opened rotated to [`ROTATED_FILE_NAME`] by
/// another launch, whose returning drops its own lock on it, and renaming by path then would move the other launch's
/// live fresh log over the previous one. Such a launch drops the moved file and reopens the path, at most
/// [`OPEN_ATTEMPTS`] times, then only appends to what it opened last, locked when its lock is free.
fn settle_log_file(state_dir: &Path, mut file: File, rotate_bytes: u64) -> io::Result<Arc<File>> {
    let path = state_dir.join(LOG_FILE_NAME);
    for _ in 0..OPEN_ATTEMPTS {
        match claim(&path, &file, rotate_bytes)? {
            Claim::Append => return Ok(Arc::new(file)),
            Claim::Rotate => return rotate(state_dir, &path, file),
            Claim::Moved => file = open_append(&path)?,
        }
    }
    // still moving after every attempt: append to the last open, holding its lock when free, as a fresh file is held
    let _ = file.try_lock();
    Ok(Arc::new(file))
}

/// What [`settle_log_file`] does with the file it opened.
enum Claim {
    /// Write to it as it is: its lock is held elsewhere (a running instance, or no locks on this file system), or it
    /// is within the bound.
    Append,
    /// Rotate it: this launch holds its lock, and it is past the bound.
    Rotate,
    /// Reopen the path: the file is no longer the one there, another launch rotated it after it was opened.
    Moved,
}

/// Tries the lock on `file`, opened from `path` (held while `file` stays open), and decides what to do with it.
fn claim(path: &Path, file: &File, rotate_bytes: u64) -> io::Result<Claim> {
    use std::os::unix::fs::MetadataExt;

    let locked = file.try_lock().is_ok();
    let meta = file.metadata()?;
    // checked after the lock: only a holder rotates, so with the lock had the path stays this file until the rename
    let at_path = match fs::symlink_metadata(path) {
        Ok(current) => current.dev() == meta.dev() && current.ino() == meta.ino(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(error),
    };
    Ok(if !at_path {
        Claim::Moved
    } else if locked && meta.is_file() && meta.len() > rotate_bytes {
        Claim::Rotate
    } else {
        Claim::Append
    })
}

/// Moves the log at `path` to [`ROTATED_FILE_NAME`] and opens a fresh one, locked in turn; `old`, the moved file, keeps
/// its lock until then.
fn rotate(state_dir: &Path, path: &Path, old: File) -> io::Result<Arc<File>> {
    fs::rename(path, state_dir.join(ROTATED_FILE_NAME))?;
    let fresh = open_append(path)?;
    // a launch racing in before this lock finds the fresh file empty, so it does not rotate it either; one that takes
    // the lock first leaves this launch appending without it, as a launch that finds the lock held
    let _ = fresh.try_lock();
    drop(old);
    Ok(Arc::new(fresh))
}

/// Opens `path` for appending, creating it owner-only; a symlink is refused.
fn open_append(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agterm_core::paths::{MapEnv, STATE_DIR_ENV};

    #[test]
    fn the_log_file_appears_under_the_state_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("nested/state");
        let env = MapEnv::new().with(STATE_DIR_ENV, dir.as_os_str());

        let file = open_log_file(&paths::state_dir(&env), ROTATE_BYTES).unwrap();
        tracing::subscriber::with_default(subscriber(filter(&env), Some(file)), || {
            tracing::info!("hello from the log test");
            tracing::debug!("below the default level");
        });

        let written = fs::read_to_string(dir.join(LOG_FILE_NAME)).unwrap();
        assert!(written.contains("hello from the log test"), "{written}");
        assert!(!written.contains("below the default level"), "{written}");
        assert!(!written.contains('\x1b'), "the file has no ANSI colors: {written:?}");
    }

    #[test]
    fn the_level_comes_from_agterm_log() {
        let tmp = tempfile::tempdir().unwrap();
        let env = MapEnv::new().with(LOG_ENV, "debug");

        let file = open_log_file(tmp.path(), ROTATE_BYTES).unwrap();
        tracing::subscriber::with_default(subscriber(filter(&env), Some(file)), || {
            tracing::debug!("debug line");
            tracing::trace!("trace line");
        });

        let written = fs::read_to_string(tmp.path().join(LOG_FILE_NAME)).unwrap();
        assert!(written.contains("debug line"), "{written}");
        assert!(!written.contains("trace line"), "{written}");
        assert_eq!(filter(&MapEnv::new()).to_string(), DEFAULT_FILTER);
        assert_eq!(
            filter(&MapEnv::new().with(LOG_ENV, "=bad=")).to_string(),
            DEFAULT_FILTER
        );
    }

    #[test]
    fn a_small_log_is_appended_to() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(LOG_FILE_NAME);
        fs::write(&path, "earlier run\n").unwrap();

        let file = open_log_file(tmp.path(), 1024).unwrap();
        io::Write::write_all(&mut &*file, b"this run\n").unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "earlier run\nthis run\n");
        assert!(!tmp.path().join(ROTATED_FILE_NAME).exists());
    }

    #[test]
    fn a_large_log_rotates_to_one_previous_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(LOG_FILE_NAME);
        let rotated = tmp.path().join(ROTATED_FILE_NAME);
        fs::write(&rotated, "oldest\n").unwrap();
        fs::write(&path, "previous run, past the limit\n").unwrap();

        let file = open_log_file(tmp.path(), 8).unwrap();
        io::Write::write_all(&mut &*file, b"this run\n").unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "this run\n");
        assert_eq!(fs::read_to_string(&rotated).unwrap(), "previous run, past the limit\n");
    }

    /// Names the test a re-run copy of the test binary runs in place of re-running it: see [`in_a_fresh_process`].
    const FRESH_PROCESS_ENV: &str = "AGTERM_APP_TEST_FRESH_PROCESS";

    /// Runs `body` alone in a fresh copy of the test binary: in the copy [`FRESH_PROCESS_ENV`] names `test` to (its
    /// full path, `module::tests::name`), it runs `body`; anywhere else it re-runs `test` alone in such a copy and
    /// asserts the copy passed. A test that needs a lock gone the moment its last handle drops runs this way: the lock
    /// belongs to the open file description, and a child that another test of this multi-threaded binary spawns holds
    /// a duplicate of every descriptor from its fork until its exec, so in the shared harness the lock can outlive the
    /// handles for a moment. In the copy no other test runs, so nothing forks.
    fn in_a_fresh_process(test: &str, body: impl FnOnce()) {
        if std::env::var(FRESH_PROCESS_ENV).is_ok_and(|probed| probed == test) {
            body();
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([test, "--exact", "--test-threads=1", "--nocapture"])
            .env(FRESH_PROCESS_ENV, test)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{test} in a fresh process: {:?}\n{stdout}{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        // the filter matched the test: a mistyped name would pass with no test run
        assert!(stdout.contains("1 passed"), "{test} did not run:\n{stdout}");
    }

    /// A second launch, forwarded to the running instance, must not rotate the log that instance is writing, even
    /// once it has grown past the bound; the rotating launch's fresh file is held the same way. In a fresh process, as
    /// the rotation once no instance is left needs the lock released when the last handle drops.
    #[test]
    fn a_log_held_by_a_running_instance_is_never_rotated() {
        in_a_fresh_process(
            "logging::tests::a_log_held_by_a_running_instance_is_never_rotated",
            the_running_instance_keeps_its_log,
        );
    }

    fn the_running_instance_keeps_its_log() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(LOG_FILE_NAME);
        let rotated = tmp.path().join(ROTATED_FILE_NAME);
        fs::write(&rotated, "oldest\n").unwrap();

        let running = open_log_file(tmp.path(), 1024).unwrap();
        io::Write::write_all(&mut &*running, b"running instance, now past the limit\n").unwrap();
        let second = open_log_file(tmp.path(), 8).unwrap();
        io::Write::write_all(&mut &*second, b"second launch\n").unwrap();
        io::Write::write_all(&mut &*running, b"running instance again\n").unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "running instance, now past the limit\nsecond launch\nrunning instance again\n"
        );
        assert_eq!(fs::read_to_string(&rotated).unwrap(), "oldest\n");
        drop((running, second));

        // with no instance left, the next launch rotates, and holds the fresh file against the launch after it
        let primary = open_log_file(tmp.path(), 8).unwrap();
        assert_eq!(fs::read_to_string(&rotated).unwrap().lines().count(), 3);
        io::Write::write_all(&mut &*primary, b"new primary, past the limit\n").unwrap();
        drop(open_log_file(tmp.path(), 8).unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), "new primary, past the limit\n");
        assert_eq!(fs::read_to_string(&rotated).unwrap().lines().count(), 3);
    }

    /// A launch that stalls between opening the oversized log and locking it can find, once it resumes, the file it
    /// opened rotated away by a launch that ran in between, whose return dropped its lock on it. It must not rename
    /// the path then: that names the other launch's live fresh log, which would replace the previous one while the
    /// other launch kept writing to it. In a fresh process, as the stalled launch's lock needs the rotating launch's
    /// lock released when its handle drops.
    #[test]
    fn a_launch_whose_log_was_rotated_away_leaves_the_fresh_one_in_place() {
        in_a_fresh_process(
            "logging::tests::a_launch_whose_log_was_rotated_away_leaves_the_fresh_one_in_place",
            the_stalled_launch_keeps_off_the_fresh_log,
        );
    }

    fn the_stalled_launch_keeps_off_the_fresh_log() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(LOG_FILE_NAME);
        let rotated = tmp.path().join(ROTATED_FILE_NAME);
        fs::write(&rotated, "oldest\n").unwrap();
        fs::write(&path, "previous run, past the limit\n").unwrap();

        // the stalled launch has opened the log, not locked it yet
        let stalled = open_append(&path).unwrap();
        // a launch in between rotates it and keeps its fresh file, as the running instance does
        let running = open_log_file(tmp.path(), 8).unwrap();
        io::Write::write_all(&mut &*running, b"running instance\n").unwrap();
        // the stalled launch resumes on the rotated file, unlocked by now and past the bound
        let second = settle_log_file(tmp.path(), stalled, 8).unwrap();
        io::Write::write_all(&mut &*second, b"second launch\n").unwrap();
        io::Write::write_all(&mut &*running, b"running instance again\n").unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "running instance\nsecond launch\nrunning instance again\n"
        );
        assert_eq!(fs::read_to_string(&rotated).unwrap(), "previous run, past the limit\n");
    }

    /// A launch that stalls between opening the log and locking it while another launch rotates it, the rotating
    /// launch still holding the moved file's lock, writes to the fresh log, not to the previous one it opened.
    #[test]
    fn a_launch_whose_log_is_rotated_while_held_writes_to_the_fresh_one() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(LOG_FILE_NAME);
        let rotated = tmp.path().join(ROTATED_FILE_NAME);
        fs::write(&path, "previous run, past the limit\n").unwrap();

        // the rotating launch, mid-rotation: it holds the lock on the file it moves and on its fresh one
        let moving = open_append(&path).unwrap();
        moving.try_lock().unwrap();
        let stalled = open_append(&path).unwrap();
        fs::rename(&path, &rotated).unwrap();
        let fresh = open_append(&path).unwrap();
        fresh.try_lock().unwrap();
        let second = settle_log_file(tmp.path(), stalled, 8).unwrap();
        io::Write::write_all(&mut &*second, b"second launch\n").unwrap();
        io::Write::write_all(&mut &fresh, b"rotating launch\n").unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "second launch\nrotating launch\n");
        assert_eq!(fs::read_to_string(&rotated).unwrap(), "previous run, past the limit\n");
        drop((moving, fresh));
    }

    #[test]
    fn the_log_file_is_owner_only_and_never_a_symlink() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        drop(open_log_file(tmp.path(), ROTATE_BYTES).unwrap());
        let mode = fs::metadata(tmp.path().join(LOG_FILE_NAME))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);

        let other = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        std::os::unix::fs::symlink(&target, other.path().join(LOG_FILE_NAME)).unwrap();
        assert!(open_log_file(other.path(), ROTATE_BYTES).is_err());
        assert!(!target.exists());
    }

    #[test]
    fn an_unwritable_state_dir_fails_without_panicking() {
        let tmp = tempfile::tempdir().unwrap();
        let blocker = tmp.path().join("not-a-dir");
        fs::write(&blocker, "").unwrap();

        assert!(open_log_file(&blocker.join("state"), ROTATE_BYTES).is_err());
        // the fallback subscriber still works, stderr alone
        tracing::subscriber::with_default(subscriber(filter(&MapEnv::new()), None), || {
            tracing::info!("stderr only");
        });
    }
}
