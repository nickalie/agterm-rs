//! Helpers for the real-PTY tests: a pane running `sh -c SCRIPT` in a private directory, isolated from any agterm the
//! tests run under (no `AGTERM_*` or `ZMX_*` reaches the child), and waits bounded by a deadline.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use agterm_term::{Frame, GridSize, Pane, PaneCommand, PaneConfig, PaneEvent};

pub const SIZE: GridSize = GridSize {
    cols: 80,
    rows: 24,
    cell_width: 8,
    cell_height: 16,
};

/// How long any wait in these tests may take before it fails.
pub const DEADLINE: Duration = Duration::from_secs(20);

/// A pane and the directory its script runs in.
pub struct Fixture {
    pub pane: Pane,
    pub dir: tempfile::TempDir,
}

impl Fixture {
    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
}

/// `sh -c script` with the inherited `AGTERM_*`/`ZMX_*` variables removed and `DIR` set to a fresh directory.
pub fn command(script: &str, dir: &Path) -> PaneCommand {
    let mut command = PaneCommand::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .cwd(dir)
        .env("DIR", dir);
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("AGTERM_") || name.starts_with("ZMX_") {
            command = command.env_remove(key);
        }
    }
    command
}

pub type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

pub fn spawn_with(script: &str, config: &PaneConfig) -> TestResult<Fixture> {
    let dir = tempfile::tempdir()?;
    let pane = Pane::spawn(&command(script, dir.path()), config)?;
    Ok(Fixture { pane, dir })
}

pub fn spawn(script: &str) -> TestResult<Fixture> {
    spawn_with(script, &PaneConfig::new(SIZE))
}

/// Poll the pane's frame until `done` holds; the last frame on success.
pub fn wait_frame(pane: &Pane, mut done: impl FnMut(&Frame) -> bool) -> Arc<Frame> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let frame = pane.frame().frame;
        if done(&frame) {
            return frame;
        }
        assert!(
            Instant::now() < deadline,
            "timed out; the screen:\n{}",
            frame.lines().join("\n")
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Wait until some line of the screen contains `needle`.
pub fn wait_text(pane: &Pane, needle: &str) -> Arc<Frame> {
    wait_frame(pane, |frame| frame.lines().iter().any(|line| line.contains(needle)))
}

/// Collect events until `done` holds for the collection; everything collected.
pub fn wait_events(pane: &Pane, mut done: impl FnMut(&[PaneEvent]) -> bool) -> Vec<PaneEvent> {
    let deadline = Instant::now() + DEADLINE;
    let mut events = Vec::new();
    loop {
        events.extend(pane.take_events());
        if done(&events) {
            return events;
        }
        assert!(Instant::now() < deadline, "timed out; events so far: {events:#?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Wait until the contents of `path` contain `needle`, then the contents.
pub fn wait_file_containing(path: &Path, needle: &[u8]) -> Vec<u8> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let bytes = std::fs::read(path).unwrap_or_default();
        if bytes.windows(needle.len()).any(|window| window == needle) {
            return bytes;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {:?} in {}: {:?}",
            String::from_utf8_lossy(needle),
            path.display(),
            String::from_utf8_lossy(&bytes)
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Wait until `path` exists with at least `len` bytes, then its contents.
pub fn wait_file(path: &Path, len: usize) -> Vec<u8> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        if let Ok(bytes) = std::fs::read(path)
            && bytes.len() >= len
        {
            return bytes;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {} ({len} bytes): {:?}",
            path.display(),
            std::fs::read(path).map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
