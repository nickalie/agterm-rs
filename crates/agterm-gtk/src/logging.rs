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
//! the previous one. A file that cannot be opened leaves logging on stderr alone, with a warning there.

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

/// Opens [`LOG_FILE_NAME`] in `state_dir` for appending, creating the directory, after moving a file larger than
/// `rotate_bytes` to [`ROTATED_FILE_NAME`].
fn open_log_file(state_dir: &Path, rotate_bytes: u64) -> io::Result<Arc<File>> {
    fs::create_dir_all(state_dir)?;
    let path = state_dir.join(LOG_FILE_NAME);
    rotate_if_large(&path, &state_dir.join(ROTATED_FILE_NAME), rotate_bytes)?;
    let file = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    Ok(Arc::new(file))
}

/// Moves `path` to `rotated` when it is a file larger than `limit`; a missing file needs nothing.
fn rotate_if_large(path: &Path, rotated: &Path, limit: u64) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() && meta.len() > limit => fs::rename(path, rotated),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
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
