//! The state files on disk: fixtures in upstream's format and in the format the installed Linux fork writes, the
//! launch bootstrap's recovery matrix, the saves the model asks for, and Open Recent's `recent-closed.json` (ported
//! from upstream's `WindowLibraryTests`, `WindowLibraryIndexTests`, `SettingsStoreTests` and `RecentClosedTests`).
//!
//! Fixtures (`tests/fixtures/`) are hand-written from upstream's `Snapshot.swift`, `WindowLibrary.swift`,
//! `AppSettings.swift` and Foundation's `JSONEncoder` output rules (`[.prettyPrinted, .sortedKeys]`):
//! - `upstream/`: a state dir as upstream writes it: an index with an open and a closed window, a window with every
//!   snapshot field the model writes, a closed window carrying a captured command and a font size, settings, and an
//!   Open Recent list with a closed workspace (collapsed, a focus member, its selection) and a closed split session;
//! - `fork/`: files the Linux fork writes. It keeps upstream's `agtermCore` (its `Snapshot.swift`,
//!   `PersistenceStore.swift`, `SettingsStore.swift` and the index types are identical), so the format is the same;
//!   the fixture carries what older fork builds left behind: the legacy single focus key, sessions without pane
//!   identities, a split recorded only as `isSplit`, the legacy `restoreRunningCommand`, and the fork-only
//!   `sessionNameFromTerminalTitle` setting, which upstream does not know and this build adopts.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Instant;

use ngterm_core::model::{NewSession, PENDING_CLOSE_GRACE, WindowLibrary};
use ngterm_core::persist::library::{Commit, LibraryPersistence};
use ngterm_core::persist::recent_closed::reference_date;
use ngterm_core::persist::settings::RestoreMode;
use ngterm_core::persist::{
    DebouncedSave, PersistenceStore, RecentClosedKind, RecentClosedState, RecentClosedStore, SAVE_DEBOUNCE,
    SessionSnapshot, SettingsStore, Snapshot, WindowEntry, WindowsIndex, WorkspaceSnapshot, arm_debounced_saves, json,
};
use ngterm_core::watermark;
use ngterm_host::MainLoop;
use ngterm_host::fakes::FakeMainLoop;
use ngterm_proto::events::EventKind;
use tempfile::TempDir;
use uuid::Uuid;

const HOME: &str = "/home/user";
const WINDOW_A: &str = "0A11AAAA-1111-4111-8111-00000000000A";
const WINDOW_B: &str = "0B22BBBB-2222-4222-8222-00000000000B";
const FORK_WINDOW: &str = "7C33CCCC-3333-4333-8333-00000000000C";

/// One of the constant ids above; the nil id for a typo, which then fails the test's assertions.
fn id(text: &str) -> Uuid {
    Uuid::try_parse(text).unwrap_or_default()
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// A temporary state dir seeded with a fixture tree.
fn state_from(fixture: &str) -> io::Result<TempDir> {
    let dir = tempfile::tempdir()?;
    copy_tree(&fixtures().join(fixture), dir.path())?;
    Ok(dir)
}

/// A temporary state dir and a persistence over it.
fn fresh() -> io::Result<(TempDir, LibraryPersistence)> {
    let dir = tempfile::tempdir()?;
    let persistence = LibraryPersistence::new(dir.path());
    Ok((dir, persistence))
}

fn launch(dir: &Path) -> (LibraryPersistence, WindowLibrary) {
    let mut persistence = LibraryPersistence::new(dir);
    let library = persistence.bootstrap(HOME);
    (persistence, library)
}

fn window_store(dir: &Path, window: Uuid) -> PersistenceStore {
    PersistenceStore::new(LibraryPersistence::new(dir).window_path(window))
}

fn write_window(dir: &Path, window: Uuid, snapshot: &Snapshot) -> io::Result<()> {
    window_store(dir, window).save(snapshot)
}

fn write_index(dir: &Path, index: &WindowsIndex) -> io::Result<()> {
    json::save_json(&dir.join("windows.json"), index)
}

fn workspace(name: &str, sessions: Vec<SessionSnapshot>) -> WorkspaceSnapshot {
    WorkspaceSnapshot {
        id: Uuid::new_v4(),
        name: name.to_owned(),
        sessions,
        collapsed: None,
    }
}

fn tree(workspaces: Vec<WorkspaceSnapshot>) -> Snapshot {
    Snapshot {
        workspaces,
        ..Snapshot::default()
    }
}

fn entry(window: Uuid, name: &str, is_open: bool) -> WindowEntry {
    WindowEntry {
        id: window,
        name: name.to_owned(),
        is_open,
    }
}

fn names(library: &WindowLibrary) -> Vec<String> {
    library.windows().iter().map(|info| info.name.clone()).collect()
}

/// The workspace names of an open window; none for a closed or unknown one.
fn workspace_names(library: &WindowLibrary, window: Uuid) -> Vec<String> {
    library
        .store(window)
        .map(|store| {
            store
                .workspaces()
                .iter()
                .map(|workspace| workspace.name.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Makes writes into a directory fail for the guard's life: on Unix the directory read-only (0500); on Windows, where a
/// directory's read-only attribute stops nothing, each file in it read-only, which a write or a replace refuses.
struct ReadOnly(PathBuf);

impl ReadOnly {
    #[cfg(unix)]
    fn new(dir: &Path) -> io::Result<Self> {
        use std::os::unix::fs::PermissionsExt as _;

        fs::set_permissions(dir, fs::Permissions::from_mode(0o500))?;
        Ok(Self(dir.to_owned()))
    }

    #[cfg(windows)]
    fn new(dir: &Path) -> io::Result<Self> {
        Self::set_files_read_only(dir, true)?;
        Ok(Self(dir.to_owned()))
    }

    #[cfg(windows)]
    fn set_files_read_only(dir: &Path, read_only: bool) -> io::Result<()> {
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            let mut permissions = fs::metadata(&path)?.permissions();
            if path.is_file() {
                permissions.set_readonly(read_only);
                fs::set_permissions(&path, permissions)?;
            }
        }
        Ok(())
    }
}

impl Drop for ReadOnly {
    #[cfg(unix)]
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt as _;

        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
    }

    #[cfg(windows)]
    fn drop(&mut self) {
        let _ = Self::set_files_read_only(&self.0, false);
    }
}

// MARK: - Fixtures in upstream's format

#[test]
fn an_upstream_window_file_reencodes_byte_for_byte() {
    let path = fixtures().join(format!("upstream/windows/{WINDOW_A}.json"));
    let original = fs::read(&path).unwrap();
    let snapshot = PersistenceStore::new(&path).load_checked().unwrap();
    assert_eq!(snapshot.workspaces.len(), 2);
    let session = &snapshot.workspaces[0].sessions[0];
    assert_eq!(session.custom_name.as_deref(), Some("build"));
    assert_eq!(session.split_restore_command.as_deref(), Some(""));
    assert_eq!(session.context.as_deref(), Some("PR #517: restore reap ordering"));
    assert_eq!(
        String::from_utf8(json::to_foundation_pretty(&snapshot).unwrap()).unwrap(),
        String::from_utf8(original).unwrap()
    );
}

#[test]
fn an_upstream_window_round_trips_through_the_model_losslessly() {
    let path = fixtures().join(format!("upstream/windows/{WINDOW_A}.json"));
    let snapshot = PersistenceStore::new(&path).load_checked().unwrap();
    let mut store = ngterm_core::model::AppStore::new();
    store.restore(&snapshot, false);
    assert_eq!(store.snapshot(), snapshot);
    let resaved = json::to_foundation_pretty(&store.snapshot()).unwrap();
    assert_eq!(resaved, fs::read(&path).unwrap());
}

#[test]
fn upstream_settings_reencode_byte_for_byte() {
    let dir = state_from("upstream").unwrap();
    let store = SettingsStore::new(dir.path());
    let original = fs::read_to_string(store.path()).unwrap();
    let settings = store.load();
    assert_eq!(settings.effective_restore_mode(), RestoreMode::Rerun);
    assert_eq!(settings.font_size, Some(13.5));
    assert_eq!(settings.background_blur, Some(20));
    store.save(&settings).unwrap();
    // every byte but upstream's bundled `agterm` theme, which this build does not ship (`migrate_legacy_theme`)
    let without_theme = original.replace("  \"theme\" : \"agterm\",\n", "");
    assert_ne!(without_theme, original, "the fixture names upstream's bundled theme");
    assert_eq!(fs::read_to_string(store.path()).unwrap(), without_theme);
}

#[test]
fn an_upstream_state_dir_launches_as_upstream_would() {
    let dir = state_from("upstream").unwrap();
    let index_before = fs::read(dir.path().join("windows.json")).unwrap();
    let window_before = fs::read(window_store(dir.path(), id(WINDOW_A)).path()).unwrap();
    let (persistence, library) = launch(dir.path());
    assert_eq!(names(&library), ["window 1", "servers"]);
    assert_eq!(library.open_ids(), [id(WINDOW_A)]);
    assert_eq!(library.frontmost_window_id, Some(id(WINDOW_A)));
    let store = library.store(id(WINDOW_A)).unwrap();
    assert_eq!(workspace_names(&library, id(WINDOW_A)), ["work", "personal"]);
    let build = store.session(id("5E000000-0000-4000-8000-000000000001")).unwrap();
    assert_eq!(
        build.restore.pending_command.as_deref(),
        Some("claude --resume abc"),
        "a launch arms the override"
    );
    assert_eq!(build.split_restore.pending_command.as_deref(), Some(""));
    assert!(build.split.shown && build.split.exists);
    assert!(store.focus().enabled());
    assert!(!persistence.index_unsaved());
    assert_eq!(
        fs::read(dir.path().join("windows.json")).unwrap(),
        index_before,
        "the index is rewritten as it was"
    );
    assert_eq!(
        fs::read(window_store(dir.path(), id(WINDOW_A)).path()).unwrap(),
        window_before
    );
}

#[test]
fn reopening_a_closed_upstream_window_scrubs_its_capture_and_announces_its_sessions() {
    let dir = state_from("upstream").unwrap();
    let (mut persistence, mut library) = launch(dir.path());
    assert!(persistence.load_window(&mut library, id(WINDOW_B)));
    let Commit { events, .. } = persistence.commit(&mut library);
    let store = library.store(id(WINDOW_B)).unwrap();
    let ops = store.session(id("5E000000-0000-4000-8000-000000000003")).unwrap();
    assert_eq!(ops.restore.pending_foreground, None, "a runtime reload arms nothing");
    assert_eq!(ops.font_size, Some(18.0));
    let created: Vec<_> = events
        .iter()
        .filter(|event| event.event.kind == EventKind::SessionCreated)
        .map(|event| event.window)
        .collect();
    assert_eq!(created, [id(WINDOW_B)]);
    let on_disk = window_store(dir.path(), id(WINDOW_B)).load();
    assert_eq!(on_disk.workspaces[0].sessions[0].foreground_command, None);
    let index = WindowsIndex::load(&dir.path().join("windows.json")).unwrap();
    assert!(index.windows.iter().all(|entry| entry.is_open));
}

// MARK: - Files the installed fork wrote

#[test]
fn a_fork_state_dir_launches_with_its_legacy_fields_migrated() {
    let dir = state_from("fork").unwrap();
    let (_persistence, library) = launch(dir.path());
    let window = id(FORK_WINDOW);
    assert_eq!(library.open_ids(), [window]);
    let store = library.store(window).unwrap();
    assert_eq!(workspace_names(&library, window), ["linux", "other"]);
    assert!(
        store.focus().enabled(),
        "the legacy single focus key is an enabled one-member set"
    );
    let visible: Vec<_> = store
        .visible_workspaces()
        .iter()
        .map(|workspace| workspace.name.clone())
        .collect();
    assert_eq!(visible, ["linux"]);
    assert!((store.sidebar().width - 240.5).abs() < f64::EPSILON);
    let editor = store.session(id("5E000000-0000-4000-8000-0000000000F1")).unwrap();
    assert!(editor.split.exists && editor.split.shown);
    assert!(
        editor.split_pane_identity.is_some(),
        "missing pane identities are minted"
    );
    assert_eq!(editor.initial_split_cwd.as_deref(), Some("/home/user"));
    assert_eq!(
        store.selected_session_id(),
        Some(id("5E000000-0000-4000-8000-0000000000F2"))
    );
}

#[test]
fn a_fork_window_saved_again_drops_its_legacy_keys_and_keeps_the_tree() {
    let dir = state_from("fork").unwrap();
    let (mut persistence, library) = launch(dir.path());
    let window = id(FORK_WINDOW);
    assert!(persistence.save_window(&library, window));
    let text = fs::read_to_string(window_store(dir.path(), window).path()).unwrap();
    assert!(!text.contains("\"focusedWorkspaceID\""), "{text}");
    let again = window_store(dir.path(), window).load_checked().unwrap();
    assert_eq!(
        again.focused_workspace_ids,
        Some(vec![id("1D000000-0000-4000-8000-0000000000F1")])
    );
    assert_eq!(again.workspaces[0].sessions[0].has_split, Some(true));
    assert!(again.workspaces[0].sessions[0].pane_identity.is_some());
    let (_, relaunched) = launch(dir.path());
    assert_eq!(workspace_names(&relaunched, window), ["linux", "other"]);
}

#[test]
fn fork_settings_load_migrated_keeping_the_fork_only_key() {
    let dir = state_from("fork").unwrap();
    let store = SettingsStore::new(dir.path());
    let settings = store.load();
    assert_eq!(
        settings.restore_mode,
        Some(RestoreMode::Rerun),
        "the legacy boolean migrates"
    );
    assert_eq!(settings.restore_running_command, None);
    assert_eq!(settings.theme.as_deref(), Some("Builtin Dark"));
    assert_eq!(settings.font_size, Some(12.0));
    assert_eq!(settings.session_name_from_terminal_title, Some(true));
    store.save(&settings).unwrap();
    let text = fs::read_to_string(store.path()).unwrap();
    assert!(!text.contains("restoreRunningCommand"), "{text}");
    assert!(text.contains(r#""sessionNameFromTerminalTitle" : true"#), "{text}");
    assert!(text.contains(r#""restoreMode" : "rerun""#), "{text}");
}

// MARK: - Seeding and the index

#[test]
fn a_fresh_state_dir_seeds_one_window_and_writes_it() {
    let (dir, mut persistence) = fresh().unwrap();
    let library = persistence.bootstrap(HOME);
    assert_eq!(names(&library), ["window 1"]);
    let window = library.windows()[0].id;
    let store = library.store(window).unwrap();
    assert_eq!(store.workspaces()[0].name, "workspace 1");
    assert_eq!(store.workspaces()[0].sessions.len(), 1);
    assert_eq!(store.workspaces()[0].sessions[0].initial_cwd(), HOME);
    assert_eq!(library.open_ids(), [window]);
    assert!(dir.path().join("windows.json").exists());
    assert!(persistence.window_path(window).exists());
    assert!(
        persistence
            .window_path(window)
            .ends_with(format!("windows/{}.json", window.to_string().to_uppercase()))
    );
}

#[test]
fn the_index_and_trees_round_trip_through_a_relaunch() {
    let (dir, mut persistence) = fresh().unwrap();
    let mut library = persistence.bootstrap(HOME);
    let first = library.windows()[0].id;
    let extra = library.new_window(Some("personal"));
    {
        let store = library.store_mut(first).unwrap();
        let workspace = store.workspaces()[0].id();
        let session = store.add_session(workspace, NewSession::at_cwd("/var/log")).unwrap();
        store.rename_session(session, "logs");
    }
    library.set_frontmost(Some(extra));
    persistence.commit(&mut library);

    let (_, reloaded) = launch(dir.path());
    assert_eq!(names(&reloaded), ["window 1", "personal"]);
    assert_eq!(reloaded.frontmost_window_id, Some(extra));
    let mut open = reloaded.open_ids();
    open.sort();
    let mut expected = vec![first, extra];
    expected.sort();
    assert_eq!(open, expected);
    let sessions = &reloaded.store(first).unwrap().workspaces()[0].sessions;
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[1].display_name(), "logs");
}

#[test]
fn a_closed_window_stays_listed_but_does_not_reopen() {
    let (dir, mut persistence) = fresh().unwrap();
    let mut library = persistence.bootstrap(HOME);
    let extra = library.new_window(Some("extra"));
    persistence.commit(&mut library);
    persistence.close_window(&mut library, extra);
    persistence.commit(&mut library);
    let (mut persistence, mut reloaded) = launch(dir.path());
    assert!(reloaded.windows().iter().any(|info| info.id == extra));
    assert!(!reloaded.is_open(extra));
    assert_eq!(reloaded.open_ids(), [library.windows()[0].id]);
    assert!(persistence.load_window(&mut reloaded, extra));
    assert_eq!(reloaded.store(extra).unwrap().workspaces().len(), 1);
    assert!(!persistence.load_window(&mut reloaded, Uuid::new_v4()));
}

#[test]
fn reopen_falls_back_to_the_frontmost_then_the_first_window() {
    let (dir, _) = fresh().unwrap();
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    for window in [a, b] {
        write_window(dir.path(), window, &tree(vec![workspace("ws", Vec::new())])).unwrap();
    }
    write_index(
        dir.path(),
        &WindowsIndex::new(Some(b), vec![entry(a, "a", false), entry(b, "b", false)]),
    )
    .unwrap();
    assert_eq!(launch(dir.path()).1.open_ids(), [b]);
    write_index(
        dir.path(),
        &WindowsIndex::new(None, vec![entry(a, "a", false), entry(b, "b", false)]),
    )
    .unwrap();
    assert_eq!(launch(dir.path()).1.open_ids(), [a]);
    let stale = Uuid::new_v4();
    write_index(dir.path(), &WindowsIndex::new(Some(stale), vec![entry(a, "a", false)])).unwrap();
    assert_eq!(launch(dir.path()).1.open_ids(), [a]);
}

#[test]
fn closing_every_window_pins_the_exit_window_for_the_next_launch() {
    let (dir, _) = fresh().unwrap();
    let oldest = id("0A11AAAA-0000-0000-0000-000000000011");
    let exit = id("7B33CCCC-0000-0000-0000-000000000013");
    write_window(dir.path(), oldest, &tree(vec![workspace("old", Vec::new())])).unwrap();
    write_window(dir.path(), exit, &tree(vec![workspace("work", Vec::new())])).unwrap();
    write_index(
        dir.path(),
        &WindowsIndex::new(Some(exit), vec![entry(oldest, "old", false), entry(exit, "work", true)]),
    )
    .unwrap();
    let (mut persistence, mut library) = launch(dir.path());
    persistence.close_window(&mut library, exit);
    persistence.commit(&mut library);
    assert_eq!(library.frontmost_window_id, Some(exit));
    assert_eq!(launch(dir.path()).1.open_ids(), [exit]);
}

// MARK: - Migration and recovery

#[test]
fn a_legacy_file_migrates_into_one_window_and_stays() {
    let (dir, _) = fresh().unwrap();
    let session = Uuid::new_v4();
    let legacy = Snapshot {
        selected_session_id: Some(session),
        ..tree(vec![workspace(
            "legacy",
            vec![SessionSnapshot {
                custom_name: Some("build".to_owned()),
                restore_command: Some("claude --resume abc".to_owned()),
                ..SessionSnapshot::new(session, "/legacy")
            }],
        )])
    };
    PersistenceStore::new(dir.path().join("workspaces.json"))
        .save(&legacy)
        .unwrap();
    let (persistence, library) = launch(dir.path());
    assert_eq!(names(&library), ["window 1"]);
    let window = library.windows()[0].id;
    assert_eq!(library.frontmost_window_id, Some(window));
    assert_eq!(workspace_names(&library, window), ["legacy"]);
    let migrated = library.store(window).unwrap().session(session).unwrap();
    assert_eq!(migrated.display_name(), "build");
    assert_eq!(migrated.restore.pending_command.as_deref(), Some("claude --resume abc"));
    assert!(persistence.window_path(window).exists());
    assert!(dir.path().join("windows.json").exists());
    assert!(
        dir.path().join("workspaces.json").exists(),
        "the legacy file is left in place"
    );
}

#[test]
fn an_empty_legacy_file_seeds_and_a_valid_index_ignores_legacy() {
    let (dir, _) = fresh().unwrap();
    PersistenceStore::new(dir.path().join("workspaces.json"))
        .save(&Snapshot::default())
        .unwrap();
    let (_, library) = launch(dir.path());
    assert_eq!(workspace_names(&library, library.windows()[0].id), ["workspace 1"]);

    let (dir, _) = fresh().unwrap();
    PersistenceStore::new(dir.path().join("workspaces.json"))
        .save(&tree(vec![workspace("legacy", Vec::new())]))
        .unwrap();
    let indexed = Uuid::new_v4();
    write_window(dir.path(), indexed, &tree(vec![workspace("indexed", Vec::new())])).unwrap();
    write_index(
        dir.path(),
        &WindowsIndex::new(Some(indexed), vec![entry(indexed, "indexed-win", true)]),
    )
    .unwrap();
    let (_, library) = launch(dir.path());
    assert_eq!(names(&library), ["indexed-win"]);
    assert_eq!(workspace_names(&library, indexed), ["indexed"]);
}

#[test]
fn a_corrupt_mismatched_or_empty_index_counts_as_absent() {
    for index in [
        "{ not valid json ]",
        r#"{"version": 2, "windows": [{"id": "0A11AAAA-1111-4111-8111-00000000000A", "name": "future", "isOpen": true}]}"#,
        r#"{"version": 1, "windows": []}"#,
    ] {
        let (dir, _) = fresh().unwrap();
        fs::write(dir.path().join("windows.json"), index).unwrap();
        let (_, library) = launch(dir.path());
        assert_eq!(names(&library), ["window 1"], "{index}");
        assert_eq!(
            library.store(library.windows()[0].id).unwrap().workspaces()[0]
                .sessions
                .len(),
            1
        );
    }
    let (dir, _) = fresh().unwrap();
    fs::write(dir.path().join("windows.json"), "garbage").unwrap();
    PersistenceStore::new(dir.path().join("workspaces.json"))
        .save(&tree(vec![workspace("legacy", Vec::new())]))
        .unwrap();
    let (_, library) = launch(dir.path());
    assert_eq!(workspace_names(&library, library.windows()[0].id), ["legacy"]);
}

#[test]
fn a_missing_or_corrupt_window_file_opens_an_empty_tree() {
    let (dir, _) = fresh().unwrap();
    let missing = Uuid::new_v4();
    write_index(
        dir.path(),
        &WindowsIndex::new(Some(missing), vec![entry(missing, "orphan", true)]),
    )
    .unwrap();
    let (_, library) = launch(dir.path());
    assert_eq!(names(&library), ["orphan"]);
    assert!(library.is_open(missing));
    assert!(library.store(missing).unwrap().workspaces().is_empty());

    let (dir, persistence) = fresh().unwrap();
    let corrupt = Uuid::new_v4();
    fs::create_dir_all(dir.path().join("windows")).unwrap();
    fs::write(persistence.window_path(corrupt), "{ corrupt ]").unwrap();
    write_index(
        dir.path(),
        &WindowsIndex::new(Some(corrupt), vec![entry(corrupt, "corrupt", true)]),
    )
    .unwrap();
    let (_, library) = launch(dir.path());
    assert!(library.store(corrupt).unwrap().workspaces().is_empty());
}

#[test]
fn a_lost_index_recovers_the_surviving_window_files() {
    let (dir, _) = fresh().unwrap();
    let (a, b) = (
        id("0AAAAAAA-0000-4000-8000-000000000001"),
        id("0BBBBBBB-0000-4000-8000-000000000002"),
    );
    let (a_session, b_session) = (Uuid::new_v4(), Uuid::new_v4());
    let named = |session: Uuid, name: &str, cwd: &str| SessionSnapshot {
        custom_name: Some(name.to_owned()),
        ..SessionSnapshot::new(session, cwd)
    };
    write_window(
        dir.path(),
        a,
        &tree(vec![workspace("alpha", vec![named(a_session, "a-sess", "/a")])]),
    )
    .unwrap();
    write_window(
        dir.path(),
        b,
        &tree(vec![workspace("beta", vec![named(b_session, "b-sess", "/b")])]),
    )
    .unwrap();
    fs::write(dir.path().join("windows/notes.json"), "noise").unwrap();
    fs::write(dir.path().join("windows.json"), "{ not valid json ]").unwrap();
    let (_, library) = launch(dir.path());
    assert_eq!(
        library.windows().iter().map(|info| info.id).collect::<Vec<_>>(),
        [a, b],
        "file name order"
    );
    assert_eq!(names(&library), ["window 1", "window 2"]);
    assert_eq!(library.open_ids(), [a, b]);
    assert_eq!(library.frontmost_window_id, Some(a));
    assert_eq!(
        library.store(a).unwrap().session(a_session).unwrap().display_name(),
        "a-sess"
    );
    assert_eq!(workspace_names(&library, b), ["beta"]);
    let (_, healed) = launch(dir.path());
    assert_eq!(healed.windows().len(), 2, "the healed index round-trips");
}

#[test]
fn noise_without_window_files_falls_through_to_legacy_migration() {
    let (dir, _) = fresh().unwrap();
    fs::write(dir.path().join("windows.json"), "garbage").unwrap();
    fs::create_dir_all(dir.path().join("windows")).unwrap();
    fs::write(dir.path().join("windows/notes.json"), "noise").unwrap();
    fs::write(dir.path().join("windows/0A11AAAA1111411181110000000000AA.json"), "{}").unwrap();
    PersistenceStore::new(dir.path().join("workspaces.json"))
        .save(&tree(vec![workspace("legacy", Vec::new())]))
        .unwrap();
    let (_, library) = launch(dir.path());
    assert_eq!(names(&library), ["window 1"]);
    assert_eq!(workspace_names(&library, library.windows()[0].id), ["legacy"]);
}

/// A lower-case stem names a different file on a case-sensitive file system than the upper-case path a window loads
/// from, so recovering it would open the window empty and strand its tree; it is skipped and left as it was.
#[test]
fn recovery_skips_a_window_file_not_spelled_upper_case() {
    let (dir, _) = fresh().unwrap();
    let (kept, skipped) = (
        id("0AAAAAAA-0000-4000-8000-000000000001"),
        id("0BBBBBBB-0000-4000-8000-00000000000B"),
    );
    write_window(dir.path(), kept, &tree(vec![workspace("alpha", Vec::new())])).unwrap();
    let lower = dir.path().join("windows/0bbbbbbb-0000-4000-8000-00000000000b.json");
    fs::write(&lower, "{}").unwrap();
    let (_, library) = launch(dir.path());
    assert_eq!(library.windows().iter().map(|info| info.id).collect::<Vec<_>>(), [kept]);
    assert_eq!(names(&library), ["window 1"]);
    assert_eq!(workspace_names(&library, kept), ["alpha"]);
    assert!(!library.windows().iter().any(|info| info.id == skipped));
    assert_eq!(
        fs::read_to_string(&lower).unwrap(),
        "{}",
        "the skipped file is left untouched"
    );
}

// MARK: - Captured commands at launch

fn captured(session: Uuid) -> SessionSnapshot {
    SessionSnapshot {
        is_split: Some(true),
        foreground_command: Some(vec!["tee".to_owned(), "/tmp/m".to_owned()]),
        split_foreground_command: Some(vec!["tail".to_owned(), "-f".to_owned(), "/var/log/x".to_owned()]),
        restore_command: Some("claude --resume abc".to_owned()),
        ..SessionSnapshot::new(session, "/a")
    }
}

fn one_open_window(dir: &Path, window: Uuid, session: SessionSnapshot) -> io::Result<()> {
    write_window(dir, window, &tree(vec![workspace("work", vec![session])]))?;
    write_index(dir, &WindowsIndex::new(Some(window), vec![entry(window, "work", true)]))
}

#[test]
fn a_launch_arms_the_capture_in_memory_and_strips_it_from_disk() {
    let (dir, _) = fresh().unwrap();
    let (window, session) = (Uuid::new_v4(), Uuid::new_v4());
    one_open_window(dir.path(), window, captured(session)).unwrap();
    let (mut persistence, library) = launch(dir.path());
    let armed = library.store(window).unwrap().session(session).unwrap();
    assert_eq!(armed.restore.pending_foreground.as_ref().map(Vec::len), Some(2));
    assert_eq!(armed.split_restore.pending_foreground.as_ref().map(Vec::len), Some(3));
    assert_eq!(armed.restore.foreground, None);
    assert!(
        persistence.save_window(&library, window),
        "a save before the panes spawn"
    );
    let on_disk = window_store(dir.path(), window).load();
    let persisted = &on_disk.workspaces[0].sessions[0];
    assert_eq!(
        (
            persisted.foreground_command.as_ref(),
            persisted.split_foreground_command.as_ref()
        ),
        (None, None)
    );
    assert_eq!(
        persisted.restore_command.as_deref(),
        Some("claude --resume abc"),
        "the sticky override stays"
    );
    assert_eq!(persisted.cwd, "/a");
}

#[test]
fn a_failed_strip_disarms_both_panes_but_keeps_the_override() {
    let (dir, _) = fresh().unwrap();
    let (window, session) = (Uuid::new_v4(), Uuid::new_v4());
    one_open_window(dir.path(), window, captured(session)).unwrap();
    let guard = ReadOnly::new(&dir.path().join("windows")).unwrap();
    let (_, library) = launch(dir.path());
    drop(guard);
    let armed = library.store(window).unwrap().session(session).unwrap();
    assert_eq!(armed.restore.pending_foreground, None);
    assert_eq!(armed.split_restore.pending_foreground, None);
    assert_eq!(armed.restore.pending_command.as_deref(), Some("claude --resume abc"));
}

#[test]
fn a_runtime_reload_arms_nothing_and_scrubs_a_split_only_capture() {
    let (dir, _) = fresh().unwrap();
    let (anchor, window, session) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let split_only = SessionSnapshot {
        is_split: Some(true),
        split_foreground_command: Some(vec!["tail".to_owned()]),
        restore_command: Some("claude --resume abc".to_owned()),
        ..SessionSnapshot::new(session, "/a")
    };
    write_window(dir.path(), anchor, &tree(vec![workspace("open", Vec::new())])).unwrap();
    write_window(dir.path(), window, &tree(vec![workspace("work", vec![split_only])])).unwrap();
    write_index(
        dir.path(),
        &WindowsIndex::new(
            Some(anchor),
            vec![entry(anchor, "open", true), entry(window, "work", false)],
        ),
    )
    .unwrap();
    let (mut persistence, mut library) = launch(dir.path());
    assert!(persistence.load_window(&mut library, window));
    persistence.commit(&mut library);
    let reloaded = library.store(window).unwrap().session(session).unwrap();
    assert_eq!(reloaded.restore.pending_command, None);
    assert_eq!(reloaded.split_restore.pending_foreground, None);
    assert_eq!(reloaded.restore.command.as_deref(), Some("claude --resume abc"));
    let on_disk = window_store(dir.path(), window).load();
    assert_eq!(on_disk.workspaces[0].sessions[0].split_foreground_command, None);
}

#[test]
fn recovery_drops_the_captures_but_arms_the_override() {
    let (dir, _) = fresh().unwrap();
    let (window, session, split_only) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let main = SessionSnapshot {
        foreground_command: Some(vec!["ssh".to_owned(), "prod".to_owned()]),
        restore_command: Some("claude --resume abc".to_owned()),
        ..SessionSnapshot::new(session, "/a")
    };
    let split = SessionSnapshot {
        is_split: Some(true),
        split_foreground_command: Some(vec!["tail".to_owned()]),
        ..SessionSnapshot::new(split_only, "/b")
    };
    write_window(dir.path(), window, &tree(vec![workspace("work", vec![main, split])])).unwrap();
    let (_, library) = launch(dir.path());
    let store = library.store(window).unwrap();
    let recovered = store.session(session).unwrap();
    assert_eq!(recovered.restore.pending_foreground, None);
    assert_eq!(
        recovered.restore.pending_command.as_deref(),
        Some("claude --resume abc")
    );
    assert_eq!(
        store.session(split_only).unwrap().split_restore.pending_foreground,
        None
    );
    let on_disk = window_store(dir.path(), window).load();
    assert_eq!(on_disk.workspaces[0].sessions[0].foreground_command, None);
    assert_eq!(on_disk.workspaces[0].sessions[1].split_foreground_command, None);
}

// MARK: - Saving

#[test]
fn structural_changes_save_at_once_and_a_cwd_report_does_not() {
    let (dir, mut persistence) = fresh().unwrap();
    let mut library = persistence.bootstrap(HOME);
    let window = library.windows()[0].id;
    let session = library.store(window).unwrap().workspaces()[0].sessions[0].id();
    library
        .store_mut(window)
        .unwrap()
        .session_mut(session)
        .unwrap()
        .set_current_cwd("/changed");
    let commit = persistence.commit(&mut library);
    assert!(commit.events.is_empty() && commit.debounced.is_empty());
    assert_eq!(
        window_store(dir.path(), window).load().workspaces[0].sessions[0].cwd,
        HOME,
        "no save on a cwd report"
    );

    library.store_mut(window).unwrap().rename_session(session, "named");
    persistence.commit(&mut library);
    let on_disk = window_store(dir.path(), window).load();
    assert_eq!(on_disk.workspaces[0].sessions[0].custom_name.as_deref(), Some("named"));
    assert_eq!(
        on_disk.workspaces[0].sessions[0].cwd, "/changed",
        "the live cwd rides the next save"
    );
}

#[test]
fn a_selection_save_is_debounced_and_the_latest_one_lands() {
    let (dir, mut persistence) = fresh().unwrap();
    let mut library = persistence.bootstrap(HOME);
    let window = library.windows()[0].id;
    let store = library.store_mut(window).unwrap();
    let workspace = store.workspaces()[0].id();
    let first = store.workspaces()[0].sessions[0].id();
    let second = store.add_session(workspace, NewSession::at_cwd("/b")).unwrap();
    persistence.commit(&mut library);

    library.store_mut(window).unwrap().select_session(Some(first));
    let early = persistence.commit(&mut library).debounced;
    library.store_mut(window).unwrap().select_session(Some(second));
    let late = persistence.commit(&mut library).debounced;
    assert_eq!((early.len(), late.len()), (1, 1));
    assert_eq!(
        window_store(dir.path(), window).load().selected_session_id,
        Some(second),
        "created selected"
    );
    library.store_mut(window).unwrap().select_session(Some(first));
    let latest = persistence.commit(&mut library).debounced;
    assert!(!persistence.fire_debounced(&library, early[0]), "superseded");
    assert!(!persistence.fire_debounced(&library, late[0]), "superseded");
    assert_eq!(
        window_store(dir.path(), window).load().selected_session_id,
        Some(second)
    );
    assert!(persistence.fire_debounced(&library, latest[0]));
    assert_eq!(window_store(dir.path(), window).load().selected_session_id, Some(first));
    assert!(
        !persistence.fire_debounced(&library, latest[0]),
        "a debounced save lands once"
    );

    library.store_mut(window).unwrap().select_session(Some(second));
    let pending = persistence.commit(&mut library).debounced;
    library.store_mut(window).unwrap().rename_session(second, "now");
    persistence.commit(&mut library);
    assert!(
        !persistence.fire_debounced(&library, pending[0]),
        "an immediate save covers the pending one"
    );
}

/// What the main loop of the debounce test saw: each save it was handed, and when, each also announced on `tick`.
struct Fired {
    saves: Vec<(DebouncedSave, Instant)>,
    tick: mpsc::Sender<()>,
}

fn record(fired: &mut Fired, save: DebouncedSave) {
    fired.saves.push((save, Instant::now()));
    let _ = fired.tick.send(());
}

#[test]
fn debounced_saves_come_back_through_the_main_loop_after_the_quiet_window() {
    let (dir, mut persistence) = fresh().unwrap();
    let mut library = persistence.bootstrap(HOME);
    let window = library.windows()[0].id;
    let session = library.store(window).unwrap().workspaces()[0].sessions[0].id();
    let (tick, ticks) = mpsc::channel();
    let main_loop = FakeMainLoop::spawn(Fired {
        saves: Vec::new(),
        tick,
    })
    .unwrap();
    let armed_at = Instant::now();
    let mut armed = Vec::new();
    for size in [10.0, 11.0, 12.0] {
        library.store_mut(window).unwrap().set_font_size(session, size);
        let commit = persistence.commit(&mut library);
        arm_debounced_saves(&main_loop as &dyn MainLoop<Fired>, &commit.debounced, record);
        armed.extend(commit.debounced);
    }
    // a shutdown drops pending timers, so wait for the three fires first
    for _ in 0..3 {
        ticks
            .recv_timeout(SAVE_DEBOUNCE * 50)
            .expect("a debounced save never fired");
    }
    let fired = main_loop.shutdown().unwrap().saves;
    assert_eq!(fired.len(), 3);
    assert!(armed.iter().all(|save| fired.iter().any(|(seen, _)| seen == save)));
    assert!(fired.iter().all(|(_, at)| at.duration_since(armed_at) >= SAVE_DEBOUNCE));
    let landed: Vec<bool> = armed
        .iter()
        .map(|save| persistence.fire_debounced(&library, *save))
        .collect();
    assert_eq!(landed, [false, false, true], "only the latest of a burst writes");
    assert_eq!(
        window_store(dir.path(), window).load().workspaces[0].sessions[0].font_size,
        Some(12.0)
    );
}

#[test]
fn removing_a_window_deletes_its_file_and_drops_its_pending_save() {
    let (dir, mut persistence) = fresh().unwrap();
    let mut library = persistence.bootstrap(HOME);
    let extra = library.new_window(Some("extra"));
    persistence.commit(&mut library);
    assert!(persistence.window_path(extra).exists());
    let session = library.store(extra).unwrap().workspaces()[0].sessions[0].id();
    library.store_mut(extra).unwrap().set_font_size(session, 30.0);
    let pending = persistence.commit(&mut library).debounced;
    persistence.remove_window(&mut library, extra);
    persistence.commit(&mut library);
    assert!(!persistence.fire_debounced(&library, pending[0]));
    assert!(!persistence.window_path(extra).exists());
    assert!(!library.windows().iter().any(|info| info.id == extra));
    let index = WindowsIndex::load(&dir.path().join("windows.json")).unwrap();
    assert_eq!(index.windows.len(), 1);
    let only = library.windows()[0].id;
    persistence.remove_window(&mut library, only);
    assert_eq!(library.windows().len(), 1, "the last window stays");
    assert!(persistence.window_path(only).exists());
}

#[test]
fn closing_a_window_writes_its_live_state_first() {
    let (dir, mut persistence) = fresh().unwrap();
    let mut library = persistence.bootstrap(HOME);
    let extra = library.new_window(Some("extra"));
    persistence.commit(&mut library);
    let session = library.store(extra).unwrap().workspaces()[0].sessions[0].id();
    library
        .store_mut(extra)
        .unwrap()
        .session_mut(session)
        .unwrap()
        .set_current_cwd("/live");
    persistence.close_window(&mut library, extra);
    let Commit { events, .. } = persistence.commit(&mut library);
    assert!(events.iter().any(|event| event.event.kind == EventKind::SessionClosed));
    assert_eq!(
        window_store(dir.path(), extra).load().workspaces[0].sessions[0].cwd,
        "/live"
    );
    let index = WindowsIndex::load(&dir.path().join("windows.json")).unwrap();
    assert_eq!(index.windows.iter().filter(|entry| entry.is_open).count(), 1);
    library.is_terminating = true;
    let first = library.windows()[0].id;
    persistence.close_window(&mut library, first);
    assert!(library.is_open(first), "terminating keeps the open set");
}

#[test]
fn resetting_font_sizes_reaches_closed_windows_on_disk() {
    let (dir, _) = fresh().unwrap();
    let (closed, open) = (Uuid::new_v4(), Uuid::new_v4());
    let sized = SessionSnapshot {
        font_size: Some(18.0),
        ..SessionSnapshot::new(Uuid::new_v4(), "/tmp")
    };
    write_window(dir.path(), closed, &tree(vec![workspace("ws", vec![sized])])).unwrap();
    write_window(dir.path(), open, &tree(vec![workspace("ws", Vec::new())])).unwrap();
    write_index(
        dir.path(),
        &WindowsIndex::new(
            Some(open),
            vec![entry(open, "open", true), entry(closed, "closed", false)],
        ),
    )
    .unwrap();
    let (mut persistence, mut library) = launch(dir.path());
    let store = library.store_mut(open).unwrap();
    let workspace = store.workspaces()[0].id();
    let session = store.add_session(workspace, NewSession::at_cwd("/tmp")).unwrap();
    store.set_font_size(session, 22.0);
    persistence.commit(&mut library);
    persistence.reset_session_font_sizes_all_windows(&mut library);
    persistence.commit(&mut library);
    assert_eq!(library.store(open).unwrap().session(session).unwrap().font_size, None);
    assert_eq!(
        window_store(dir.path(), closed).load().workspaces[0].sessions[0].font_size,
        None
    );
    assert!(!library.is_open(closed));
}

#[test]
fn save_all_checked_flushes_live_state_and_reports_a_failed_write() {
    let (dir, mut persistence) = fresh().unwrap();
    let mut library = persistence.bootstrap(HOME);
    let window = library.windows()[0].id;
    let session = library.store(window).unwrap().workspaces()[0].sessions[0].id();
    library
        .store_mut(window)
        .unwrap()
        .session_mut(session)
        .unwrap()
        .set_current_cwd("/changed");
    library.new_window(Some("extra"));
    persistence.commit(&mut library);
    assert!(persistence.save_all_checked(&library));
    let (_, reloaded) = launch(dir.path());
    assert_eq!(
        reloaded.store(window).unwrap().session(session).unwrap().initial_cwd(),
        "/changed"
    );

    let _guard = ReadOnly::new(&dir.path().join("windows")).unwrap();
    assert!(!persistence.save_all_checked(&library));
}

#[test]
fn a_failed_index_write_is_retried_by_the_next_snapshot_save_that_lands() {
    let (dir, mut persistence) = fresh().unwrap();
    let mut library = persistence.bootstrap(HOME);
    let window = library.windows()[0].id;
    let index = dir.path().join("windows.json");
    fs::remove_file(&index).unwrap();
    fs::create_dir(&index).unwrap();
    library.rename_window(window, "renamed");
    persistence.commit(&mut library);
    assert!(persistence.index_unsaved());
    fs::remove_dir(&index).unwrap();
    assert!(persistence.save_window(&library, window));
    assert!(!persistence.index_unsaved());
    assert_eq!(WindowsIndex::load(&index).unwrap().windows[0].name, "renamed");
}

// MARK: - Open Recent

#[test]
fn an_upstream_recent_closed_file_reencodes_byte_for_byte() {
    let dir = state_from("upstream").unwrap();
    let store = RecentClosedStore::new(dir.path());
    let original = fs::read_to_string(store.path()).unwrap();
    let items = store.load();
    assert_eq!(items.len(), 2);
    let workspace = &items[0];
    assert_eq!(
        (workspace.kind, workspace.title.as_str()),
        (RecentClosedKind::Workspace, "infra")
    );
    assert!((reference_date::seconds(workspace.closed_at) - 781_912_345.25).abs() < 1e-6);
    let recent = workspace.workspace.as_ref().unwrap();
    assert_eq!(recent.focus_member, Some(true));
    assert_eq!(
        recent.selected_session_id,
        Some(id("5E000000-0000-4000-8000-000000000012"))
    );
    assert_eq!(recent.snapshot.collapsed, Some(true));
    let session = items[1].session.as_ref().unwrap();
    assert_eq!((session.workspace_name.as_str(), session.session_index), ("work", 1));
    assert!(session.snapshot.has_any_split());
    assert_eq!(
        String::from_utf8(json::to_foundation_pretty(&RecentClosedState::new(items)).unwrap()).unwrap(),
        original
    );
}

#[test]
fn the_launch_loads_open_recent_and_a_reopen_consumes_the_entry_on_disk() {
    let dir = state_from("upstream").unwrap();
    let (mut persistence, mut library) = launch(dir.path());
    let items = library.recent_closed_items();
    assert_eq!(items.len(), 2);
    let window = id(WINDOW_A);

    assert_eq!(
        library.reopen_recent_closed_reporting_window(items[0].id, None),
        Some(window)
    );
    persistence.commit(&mut library);

    let infra = id("1D000000-0000-4000-8000-000000000011");
    let store = library.store(window).unwrap();
    let restored = store.workspace(infra).unwrap();
    assert_eq!((restored.name.as_str(), restored.is_expanded), ("infra", false));
    assert_eq!(
        store.selected_session_id(),
        Some(id("5E000000-0000-4000-8000-000000000012"))
    );
    assert!(store.focus().ids().contains(&infra), "a member comes back marked");
    let deploy = store.session(id("5E000000-0000-4000-8000-000000000012")).unwrap();
    assert_eq!(deploy.restore.command.as_deref(), Some("claude --resume abc"));
    assert_eq!(deploy.restore.pending_command, None, "a reopen arms nothing");

    let left: Vec<_> = RecentClosedStore::new(dir.path())
        .load()
        .into_iter()
        .map(|item| item.id)
        .collect();
    assert_eq!(left, [items[1].id]);
    let on_disk = window_store(dir.path(), window).load();
    assert!(
        on_disk.workspaces.iter().any(|workspace| workspace.id == infra),
        "the reopen saves the window"
    );
}

#[test]
fn a_hard_close_is_recorded_on_disk_by_the_commit() {
    let (dir, mut persistence) = fresh().unwrap();
    let mut library = persistence.bootstrap(HOME);
    let window = library.windows()[0].id;
    let store = library.store_mut(window).unwrap();
    let workspace = store.workspaces()[0].id();
    let closing = store.add_session(workspace, NewSession::at_cwd("/closing")).unwrap();
    persistence.commit(&mut library);
    assert!(!persistence.recent_closed_store().path().exists());

    library.store_mut(window).unwrap().close_session(closing);
    persistence.commit(&mut library);

    let on_disk = RecentClosedStore::new(dir.path()).load();
    assert_eq!(on_disk, library.recent_closed_items());
    assert_eq!(on_disk[0].session.as_ref().unwrap().snapshot.id, closing);
    let text = fs::read_to_string(persistence.recent_closed_store().path()).unwrap();
    assert!(
        text.starts_with("{\n  \"items\" : [") && text.ends_with("\"version\" : 1\n}"),
        "{text}"
    );

    library.clear_recent_closed_items();
    persistence.commit(&mut library);
    assert!(RecentClosedStore::new(dir.path()).load().is_empty());
}

#[test]
fn a_soft_close_keeps_the_window_file_until_undo_or_finalize() {
    let (dir, mut persistence) = fresh().unwrap();
    let mut library = persistence.bootstrap(HOME);
    let window = library.windows()[0].id;
    let store = library.store_mut(window).unwrap();
    let workspace = store.workspaces()[0].id();
    let kept = store.workspaces()[0].sessions[0].id();
    let closing = store.add_session(workspace, NewSession::at_cwd("/closing")).unwrap();
    persistence.commit(&mut library);
    library.store_mut(window).unwrap().select_session(Some(kept));
    let armed = persistence.commit(&mut library).debounced;
    let in_file = |dir: &Path, session| {
        window_store(dir, window).load().workspaces[0]
            .sessions
            .iter()
            .any(|candidate| candidate.id == session)
    };

    assert!(library.store_mut(window).unwrap().soft_close_session(closing));
    let commit = persistence.commit(&mut library);

    assert_eq!(commit.pending_closes.len(), 1);
    assert_eq!(commit.pending_closes[0].grace, PENDING_CLOSE_GRACE);
    assert!(
        in_file(dir.path(), closing),
        "the file keeps the session through the grace"
    );
    assert!(
        !persistence.fire_debounced(&library, armed[0]),
        "the armed write was dropped"
    );
    assert_eq!(
        RecentClosedStore::new(dir.path()).load().len(),
        1,
        "recorded at the close"
    );

    assert!(library.store_mut(window).unwrap().undo_pending_close(None));
    persistence.commit(&mut library);
    assert!(in_file(dir.path(), closing));
    assert!(
        RecentClosedStore::new(dir.path()).load().is_empty(),
        "the undo removes the entry"
    );

    assert!(library.store_mut(window).unwrap().soft_close_session(closing));
    let timer = persistence.commit(&mut library).pending_closes[0];
    library
        .store_mut(timer.window)
        .unwrap()
        .finalize_pending_close(timer.record);
    persistence.commit(&mut library);
    assert!(!in_file(dir.path(), closing), "the finalize writes the window");
    assert_eq!(
        RecentClosedStore::new(dir.path()).load().len(),
        1,
        "the entry outlives the finalize"
    );
}

/// A rendered text watermark file for `session` (the default's, or a pane override's by `key`).
fn rendered(dir: &Path, session: Uuid, key: Option<&str>) -> io::Result<PathBuf> {
    let path = watermark::rendered_text_path(dir, session, key);
    fs::create_dir_all(watermark::directory(dir))?;
    fs::write(&path, b"png")?;
    Ok(path)
}

/// Upstream's `removeAllRenderedText` on a hard close, a soft close's finalize and `WindowLibrary.removeWindow`, open
/// or closed.
#[test]
fn a_session_or_window_removed_for_good_takes_its_rendered_text_along() {
    let (dir, mut persistence) = fresh().unwrap();
    let mut library = persistence.bootstrap(HOME);
    let window = library.open_ids()[0];
    let store = library.store_mut(window).unwrap();
    let workspace = store.workspaces()[0].id();
    let kept = store.workspaces()[0].sessions[0].id();
    let closed = store.add_session(workspace, NewSession::at_cwd("/tmp")).unwrap();
    let soft = store.add_session(workspace, NewSession::at_cwd("/tmp")).unwrap();
    persistence.commit(&mut library);
    let kept_file = rendered(dir.path(), kept, None).unwrap();
    let closed_files = [
        rendered(dir.path(), closed, None).unwrap(),
        rendered(dir.path(), closed, Some("scratch")).unwrap(),
    ];
    library.store_mut(window).unwrap().close_session(closed);
    persistence.commit(&mut library);
    assert!(closed_files.iter().all(|file| !file.exists()));
    assert!(kept_file.exists(), "another session's file stays");

    let soft_file = rendered(dir.path(), soft, None).unwrap();
    assert!(library.store_mut(window).unwrap().soft_close_session(soft));
    let timer = persistence.commit(&mut library).pending_closes[0];
    assert!(soft_file.exists(), "an undo can still bring it back");
    library
        .store_mut(timer.window)
        .unwrap()
        .finalize_pending_close(timer.record);
    persistence.commit(&mut library);
    assert!(!soft_file.exists());

    // a closed window's sessions are read from its file, an open one's from its store
    let shut = library.new_window(Some("shut"));
    let open = library.new_window(Some("open"));
    persistence.commit(&mut library);
    let first = |library: &WindowLibrary, window: Uuid| library.store(window).unwrap().workspaces()[0].sessions[0].id();
    let shut_file = rendered(dir.path(), first(&library, shut), None).unwrap();
    let open_file = rendered(dir.path(), first(&library, open), None).unwrap();
    persistence.close_window(&mut library, shut);
    persistence.commit(&mut library);
    assert!(shut_file.exists(), "closing a window is not removing it");
    persistence.remove_window(&mut library, shut);
    persistence.commit(&mut library);
    assert!(!shut_file.exists());
    persistence.remove_window(&mut library, open);
    persistence.commit(&mut library);
    assert!(!open_file.exists());
    assert!(kept_file.exists());
}
