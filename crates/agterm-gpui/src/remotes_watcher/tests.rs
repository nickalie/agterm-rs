//! The watch on real files in temporary directories (the GTK build's cases), the debounce, and the pure path parts.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::*;

/// Polls `condition` until it holds or five seconds pass.
fn wait(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(condition(), "{what}");
}

/// What a watcher's handler saw: the file's text at each read, and how many reads.
#[derive(Clone, Default)]
struct Reads {
    text: Arc<Mutex<String>>,
    count: Arc<AtomicUsize>,
}

impl Reads {
    fn text(&self) -> String {
        self.text.lock().unwrap().clone()
    }

    fn count(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
}

/// A watcher on `path` that reads the file on every change, as the core's refresh does.
fn watching(path: &Path, test: impl FnOnce(&Reads)) {
    let reads = Reads::default();
    *reads.text.lock().unwrap() = std::fs::read_to_string(path).unwrap_or_default();
    let file = path.to_path_buf();
    let handler = reads.clone();
    let mut watcher = RemotesWatcher::new(move || {
        *handler.text.lock().unwrap() = std::fs::read_to_string(&file).unwrap_or_default();
        handler.count.fetch_add(1, Ordering::SeqCst);
    })
    .unwrap();
    watcher.follow(path);
    // following arms the watch, then reads once: from then on, edits count
    wait("the read once following", || reads.count() == 1);
    reads.count.store(0, Ordering::SeqCst);
    test(&reads);
    drop(watcher);
}

#[test]
fn a_symlink_whose_target_appears_later_is_followed() {
    let dir = tempfile::tempdir().unwrap();
    let elsewhere = dir.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    let file = dir.path().join("remotes.conf");
    std::os::unix::fs::symlink("elsewhere/real.conf", &file).unwrap();
    watching(&file, |reads| {
        std::fs::write(elsewhere.join("real.conf"), "studio\n").unwrap();
        wait("the target created later is read", || reads.text() == "studio\n");
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
    watching(&file, |reads| {
        std::fs::remove_file(&target).unwrap();
        wait("the deleted target lists nothing", || reads.text().is_empty());
        std::fs::write(&target, "mini\n").unwrap();
        wait("the recreated target is read", || reads.text() == "mini\n");
    });
}

#[test]
fn every_kind_of_edit_is_seen_on_the_re_armed_watches() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("remotes.conf");
    let replace = |text: &str| {
        let staged = dir.path().join("remotes.conf.tmp");
        std::fs::write(&staged, text).unwrap();
        std::fs::rename(&staged, &file).unwrap();
    };
    watching(&file, |reads| {
        std::fs::write(&file, "studio\n").unwrap();
        wait("a created file", || reads.text() == "studio\n");
        let mut handle = std::fs::OpenOptions::new().append(true).open(&file).unwrap();
        std::io::Write::write_all(&mut handle, b"mini\n").unwrap();
        drop(handle);
        wait("an in-place append", || reads.text() == "studio\nmini\n");
        replace("mini\n");
        wait("an atomic replace", || reads.text() == "mini\n");
        replace("mini\nstudio\n");
        wait("a second replace, on the re-armed watch", || {
            reads.text() == "mini\nstudio\n"
        });
        let mut handle = std::fs::OpenOptions::new().append(true).open(&file).unwrap();
        std::io::Write::write_all(&mut handle, b"box\n").unwrap();
        drop(handle);
        wait("an append to the replaced file", || {
            reads.text() == "mini\nstudio\nbox\n"
        });
        std::fs::remove_file(&file).unwrap();
        wait("a deleted file", || reads.text().is_empty());
    });
}

#[test]
fn a_config_directory_created_later_is_followed() {
    let dir = tempfile::tempdir().unwrap();
    let later = dir.path().join("later/nested");
    let file = later.join("remotes.conf");
    watching(&file, |reads| {
        std::fs::create_dir_all(&later).unwrap();
        std::fs::write(&file, "studio\n").unwrap();
        wait("a file in a directory that did not exist", || {
            reads.text() == "studio\n"
        });
    });
}

#[test]
fn a_chain_of_symlinks_is_followed_to_the_real_file() {
    let dir = tempfile::tempdir().unwrap();
    for directory in ["cfg", "links", "actual"] {
        std::fs::create_dir(dir.path().join(directory)).unwrap();
    }
    let real = dir.path().join("actual/remotes.conf");
    std::fs::write(&real, "studio\n").unwrap();
    std::os::unix::fs::symlink("../actual/remotes.conf", dir.path().join("links/current.conf")).unwrap();
    let file = dir.path().join("cfg/remotes.conf");
    std::os::unix::fs::symlink("../links/current.conf", &file).unwrap();
    watching(&file, |reads| {
        let mut handle = std::fs::OpenOptions::new().append(true).open(&real).unwrap();
        std::io::Write::write_all(&mut handle, b"mini\n").unwrap();
        drop(handle);
        wait("an edit of the file at the end of the chain", || {
            reads.text() == "studio\nmini\n"
        });
        let staged = dir.path().join("actual/remotes.conf.tmp");
        std::fs::write(&staged, "mini\n").unwrap();
        std::fs::rename(&staged, &real).unwrap();
        wait("a replace of the file at the end of the chain", || {
            reads.text() == "mini\n"
        });
    });
}

#[test]
fn an_unnormalized_path_is_followed() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("cfg/sub")).unwrap();
    let file = dir.path().join("cfg/./sub/../remotes.conf");
    watching(&file, |reads| {
        std::fs::write(dir.path().join("cfg/remotes.conf"), "studio\n").unwrap();
        wait("a file named through `.` and `..`", || reads.text() == "studio\n");
    });
}

#[test]
fn a_burst_of_edits_is_read_once_it_settles() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("remotes.conf");
    std::fs::write(&file, "").unwrap();
    watching(&file, |reads| {
        let mut whole = String::new();
        for line in 0..20 {
            let text = format!("host{line}\n");
            let mut handle = std::fs::OpenOptions::new().append(true).open(&file).unwrap();
            std::io::Write::write_all(&mut handle, text.as_bytes()).unwrap();
            whole.push_str(&text);
        }
        wait("the burst's end is read", || reads.text() == whole);
        std::thread::sleep(DEBOUNCE * 4);
        assert_eq!(reads.count(), 1, "one read for the whole burst");
    });
}

#[test]
fn reading_the_file_or_listing_its_directory_is_no_change() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("remotes.conf");
    std::fs::write(&file, "studio\n").unwrap();
    watching(&file, |reads| {
        for _ in 0..5 {
            assert_eq!(std::fs::read_to_string(&file).unwrap(), "studio\n");
            assert!(std::fs::read_dir(dir.path()).unwrap().count() >= 1);
        }
        std::thread::sleep(DEBOUNCE * 4);
        assert_eq!(reads.count(), 0, "the handler's own read would loop otherwise");
        std::fs::write(&file, "mini\n").unwrap();
        wait("a real edit still counts", || reads.text() == "mini\n");
        std::thread::sleep(DEBOUNCE * 4);
        assert_eq!(reads.count(), 1, "the handler's read of the edit is no change either");
    });
}

#[test]
fn a_steady_stream_of_edits_is_read_by_the_cap() {
    let mut burst = Burst::default();
    let start = Instant::now();
    assert_eq!(burst.due(), None, "nothing waits");
    burst.changed(start);
    assert_eq!(burst.due(), Some(start + DEBOUNCE));
    let later = start + DEBOUNCE / 2;
    burst.changed(later);
    assert_eq!(
        burst.due(),
        Some(later + DEBOUNCE),
        "a later change pushes the read back"
    );
    let steady = start + DEBOUNCE_CAP;
    burst.changed(steady);
    assert_eq!(burst.due(), Some(start + DEBOUNCE_CAP), "never past the cap");
}

#[test]
fn only_changes_count() {
    use notify::event::{CreateKind, DataChange, ModifyKind, RemoveKind, RenameMode};
    assert!(is_change(EventKind::Create(CreateKind::File)));
    assert!(is_change(EventKind::Modify(ModifyKind::Data(DataChange::Any))));
    assert!(is_change(EventKind::Modify(ModifyKind::Name(RenameMode::To))));
    assert!(is_change(EventKind::Remove(RemoveKind::File)));
    assert!(is_change(EventKind::Access(AccessKind::Close(AccessMode::Write))));
    assert!(is_change(EventKind::Other), "a rescan");
    assert!(!is_change(EventKind::Access(AccessKind::Open(AccessMode::Any))));
    assert!(!is_change(EventKind::Access(AccessKind::Close(AccessMode::Read))));
    assert!(!is_change(EventKind::Access(AccessKind::Read)));
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
    std::fs::create_dir_all(root.join("actual")).unwrap();
    std::fs::remove_file(&file).unwrap();
    std::os::unix::fs::symlink("../elsewhere/hop.conf", &file).unwrap();
    std::os::unix::fs::symlink("../actual/real.conf", root.join("elsewhere/hop.conf")).unwrap();
    assert_eq!(
        watched_paths(&file),
        [
            file.clone(),
            root.join("cfg"),
            root.join("elsewhere"),
            root.join("actual")
        ],
        "every hop of a chain"
    );
}

#[test]
fn a_relative_path_is_made_absolute() {
    let cwd = std::env::current_dir().unwrap();
    assert_eq!(
        absolute(Path::new("cfg/./x/../remotes.conf")),
        cwd.join("cfg/remotes.conf")
    );
    assert_eq!(absolute(Path::new("/cfg/remotes.conf")), Path::new("/cfg/remotes.conf"));
}

#[test]
fn the_symlink_hops_end_at_a_loop() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let (first, second) = (root.join("a.conf"), root.join("b.conf"));
    std::os::unix::fs::symlink("b.conf", &first).unwrap();
    std::os::unix::fs::symlink("a.conf", &second).unwrap();
    assert_eq!(symlink_targets(&first), [second]);
    assert!(symlink_targets(&root.join("missing.conf")).is_empty());
}

#[test]
fn following_the_same_path_again_keeps_the_watch_and_a_new_one_moves_it() {
    let dir = tempfile::tempdir().unwrap();
    let (first, second) = (dir.path().join("a/remotes.conf"), dir.path().join("b/remotes.conf"));
    for file in [&first, &second] {
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, "").unwrap();
    }
    let count = Arc::new(AtomicUsize::new(0));
    let counting = Arc::clone(&count);
    let mut watcher = RemotesWatcher::new(move || {
        counting.fetch_add(1, Ordering::SeqCst);
    })
    .unwrap();
    watcher.follow(&first);
    watcher.follow(&first);
    wait("one read for following it, not two", || {
        count.load(Ordering::SeqCst) == 1
    });
    std::thread::sleep(DEBOUNCE * 4);
    assert_eq!(count.load(Ordering::SeqCst), 1, "the same path again is no new follow");
    std::fs::write(&first, "studio\n").unwrap();
    wait("the first file's edit", || count.load(Ordering::SeqCst) == 2);
    watcher.follow(&second);
    wait("the read for following the second", || {
        count.load(Ordering::SeqCst) == 3
    });
    std::fs::write(&first, "mini\n").unwrap();
    std::fs::write(&second, "mini\n").unwrap();
    wait("the second file's edit", || count.load(Ordering::SeqCst) == 4);
    std::thread::sleep(DEBOUNCE * 4);
    assert_eq!(count.load(Ordering::SeqCst), 4, "the first file is no longer followed");
}
