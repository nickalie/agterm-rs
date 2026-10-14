//! The watch on `remotes.conf` (upstream's `RemotesWatcher` file sources): the file, its nearest existing directory,
//! and, when the file is a symlink, its target's nearest existing directory, each under a `gio::FileMonitor`. Every
//! change that touches the file or the way to it re-arms the monitors, so a directory created later, a symlink target
//! recreated or re-pointed, and an editor's replace-on-save are all seen, then runs the change handler, which reads
//! the file again in the core ([`agterm_core::Core::refresh_remotes`]).

use std::cell::RefCell;
use std::path::{Component, Path, PathBuf};
use std::rc::{Rc, Weak};

use gtk::gio;
use gtk::prelude::*;

/// Follows one `remotes.conf`; dropping it cancels the monitors.
pub struct RemotesWatcher {
    inner: Rc<RefCell<Inner>>,
}

struct Inner {
    /// The file watched; `None` until the first [`RemotesWatcher::follow`].
    path: Option<PathBuf>,
    /// The paths whose change concerns the file: the file itself and its symlink target.
    interest: Vec<PathBuf>,
    monitors: Vec<gio::FileMonitor>,
    on_change: Rc<dyn Fn()>,
}

impl RemotesWatcher {
    /// A watcher that runs `on_change` on every change of the file it follows, on the main context it was armed on.
    pub fn new(on_change: impl Fn() + 'static) -> Self {
        Self {
            inner: Rc::new(RefCell::new(Inner {
                path: None,
                interest: Vec::new(),
                monitors: Vec::new(),
                on_change: Rc::new(on_change),
            })),
        }
    }

    /// Watches `path`, as the launch and a config directory change ask; the same path again keeps the monitors.
    pub fn follow(&self, path: &Path) {
        if self.inner.borrow().path.as_deref() == Some(path) {
            return;
        }
        self.inner.borrow_mut().path = Some(path.to_path_buf());
        arm(&self.inner);
    }
}

impl Drop for RemotesWatcher {
    fn drop(&mut self) {
        for monitor in self.inner.borrow_mut().monitors.drain(..) {
            monitor.cancel();
        }
    }
}

/// Replaces the monitors with ones over the paths the file needs now; the new ones start before the old ones stop, so
/// an edit in between is not lost.
fn arm(inner: &Rc<RefCell<Inner>>) {
    let Some(path) = inner.borrow().path.clone() else {
        return;
    };
    let watched = watched_paths(&path);
    let mut monitors = Vec::with_capacity(watched.len());
    for (index, watched) in watched.iter().enumerate() {
        let file = gio::File::for_path(watched);
        let monitor = if index == 0 {
            file.monitor_file(gio::FileMonitorFlags::NONE, None::<&gio::Cancellable>)
        } else {
            file.monitor_directory(gio::FileMonitorFlags::NONE, None::<&gio::Cancellable>)
        };
        match monitor {
            Ok(monitor) => {
                let weak = Rc::downgrade(inner);
                monitor.connect_changed(move |_, file, other, _| changed(&weak, file, other));
                monitors.push(monitor);
            }
            Err(error) => tracing::debug!(path = %watched.display(), %error, "remotes.conf: no monitor"),
        }
    }
    let mut interest = vec![path.clone()];
    interest.extend(symlink_target(&path));
    let mut state = inner.borrow_mut();
    state.interest = interest;
    for old in std::mem::replace(&mut state.monitors, monitors) {
        old.cancel();
    }
}

fn changed(inner: &Weak<RefCell<Inner>>, file: &gio::File, other: Option<&gio::File>) {
    let Some(inner) = inner.upgrade() else {
        return;
    };
    let relevant = {
        let state = inner.borrow();
        [Some(file), other]
            .into_iter()
            .flatten()
            .filter_map(gio::prelude::FileExt::path)
            .any(|changed| concerns(&state.interest, &changed))
    };
    if !relevant {
        return;
    }
    arm(&inner);
    let on_change = Rc::clone(&inner.borrow().on_change);
    on_change();
}

/// Whether a change of `changed` concerns the file: it is one of `interest` or a directory on the way to one, which a
/// directory created later or removed is.
fn concerns(interest: &[PathBuf], changed: &Path) -> bool {
    interest.iter().any(|path| path.starts_with(changed))
}

/// The file, its nearest existing directory and, for a symlink, its target's nearest existing directory (upstream's
/// `watchedPaths`).
fn watched_paths(path: &Path) -> Vec<PathBuf> {
    let mut paths = vec![path.to_path_buf(), nearest_existing_directory(path)];
    if let Some(target) = symlink_target(path) {
        let directory = nearest_existing_directory(&target);
        if !paths.contains(&directory) {
            paths.push(directory);
        }
    }
    paths
}

/// Where the symlink at `path` points, relative to its directory and normalized lexically (Foundation's
/// `standardizedFileURL`); `None` when `path` is no symlink.
fn symlink_target(path: &Path) -> Option<PathBuf> {
    let destination = std::fs::read_link(path).ok()?;
    let joined = path.parent().unwrap_or(Path::new("/")).join(destination);
    Some(normalized(&joined))
}

fn normalized(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other),
        }
    }
    result
}

/// The closest ancestor directory of `path` that exists, `/` at worst (upstream's `nearestExistingDirectory`).
fn nearest_existing_directory(path: &Path) -> PathBuf {
    let mut directory = path.parent().unwrap_or(Path::new("/"));
    while !directory.is_dir() {
        match directory.parent() {
            Some(parent) => directory = parent,
            None => break,
        }
    }
    directory.to_path_buf()
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::time::{Duration, Instant};

    use gtk::glib;

    use super::*;

    /// Iterates `context` until `condition` holds or five seconds pass.
    fn wait(context: &glib::MainContext, what: &str, condition: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() && Instant::now() < deadline {
            while context.iteration(false) {}
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(condition(), "{what}");
    }

    /// A watcher on `path` that reads the file's first line on every change, as the core's refresh does, under a
    /// private main context.
    fn watching(path: &Path, test: impl FnOnce(&glib::MainContext, &dyn Fn() -> String)) {
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                let read = Rc::new(RefCell::new(std::fs::read_to_string(path).unwrap_or_default()));
                let changes = Rc::new(Cell::new(0));
                let file = path.to_path_buf();
                let watcher = {
                    let (read, changes) = (Rc::clone(&read), Rc::clone(&changes));
                    RemotesWatcher::new(move || {
                        changes.set(changes.get() + 1);
                        *read.borrow_mut() = std::fs::read_to_string(&file).unwrap_or_default();
                    })
                };
                watcher.follow(path);
                test(&context, &|| read.borrow().clone());
                drop(watcher);
            })
            .unwrap();
    }

    #[test]
    fn a_symlink_whose_target_appears_later_is_followed() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        let file = dir.path().join("remotes.conf");
        std::os::unix::fs::symlink("elsewhere/real.conf", &file).unwrap();
        watching(&file, |context, read| {
            std::fs::write(elsewhere.join("real.conf"), "studio\n").unwrap();
            wait(context, "the target created later is read", || read() == "studio\n");
        });
    }

    #[test]
    fn a_symlink_whose_target_is_deleted_then_recreated_is_followed() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        let target = elsewhere.join("real.conf");
        std::fs::write(&target, "studio\n").unwrap();
        let file = dir.path().join("remotes.conf");
        std::os::unix::fs::symlink(&target, &file).unwrap();
        watching(&file, |context, read| {
            std::fs::remove_file(&target).unwrap();
            wait(context, "the deleted target lists nothing", || read().is_empty());
            std::fs::write(&target, "mini\n").unwrap();
            wait(context, "the recreated target is read", || read() == "mini\n");
        });
    }

    #[test]
    fn every_kind_of_edit_is_seen_on_the_re_armed_monitors() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("remotes.conf");
        let replace = |text: &str| {
            let staged = dir.path().join("remotes.conf.tmp");
            std::fs::write(&staged, text).unwrap();
            std::fs::rename(&staged, &file).unwrap();
        };
        watching(&file, |context, read| {
            std::fs::write(&file, "studio\n").unwrap();
            wait(context, "a created file", || read() == "studio\n");
            let mut handle = std::fs::OpenOptions::new().append(true).open(&file).unwrap();
            std::io::Write::write_all(&mut handle, b"mini\n").unwrap();
            drop(handle);
            wait(context, "an in-place append", || read() == "studio\nmini\n");
            replace("mini\n");
            wait(context, "an atomic replace", || read() == "mini\n");
            replace("mini\nstudio\n");
            wait(context, "a second replace, on the re-armed monitor", || {
                read() == "mini\nstudio\n"
            });
            std::fs::remove_file(&file).unwrap();
            wait(context, "a deleted file", || read().is_empty());
        });
    }

    #[test]
    fn a_config_directory_created_later_is_followed() {
        let dir = tempfile::tempdir().unwrap();
        let later = dir.path().join("later/nested");
        let file = later.join("remotes.conf");
        watching(&file, |context, read| {
            std::fs::create_dir_all(&later).unwrap();
            std::fs::write(&file, "studio\n").unwrap();
            wait(context, "a file in a directory that did not exist", || {
                read() == "studio\n"
            });
        });
    }

    #[test]
    fn a_change_elsewhere_in_the_directory_is_not_one_of_the_files() {
        let interest = [PathBuf::from("/cfg/remotes.conf"), PathBuf::from("/other/real.conf")];
        assert!(concerns(&interest, Path::new("/cfg/remotes.conf")));
        assert!(concerns(&interest, Path::new("/cfg")), "the directory on the way");
        assert!(concerns(&interest, Path::new("/other/real.conf")), "the symlink target");
        assert!(!concerns(&interest, Path::new("/cfg/keymap.conf")));
        assert!(!concerns(&interest, Path::new("/cfg/remotes.conf.tmp")));
    }

    #[test]
    fn the_watched_paths_include_a_symlink_targets_directory() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let file = root.join("cfg/remotes.conf");
        assert_eq!(
            watched_paths(&file),
            [file.clone(), root.clone()],
            "the nearest existing directory"
        );
        std::fs::create_dir_all(root.join("cfg")).unwrap();
        std::fs::create_dir_all(root.join("elsewhere")).unwrap();
        std::os::unix::fs::symlink("../elsewhere/./real.conf", &file).unwrap();
        assert_eq!(
            watched_paths(&file),
            [file.clone(), root.join("cfg"), root.join("elsewhere")]
        );
        assert_eq!(symlink_target(&file), Some(root.join("elsewhere/real.conf")));
    }
}
