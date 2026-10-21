use std::ffi::OsString;
use std::path::Path;

use agterm_session::cli::{self, Command, default_argv, variable as lookup};
use agterm_session::server::{EXIT_FAILED, EXIT_USAGE};

use super::*;

fn attach(argv: &[&str]) -> LiveAttach {
    LiveAttach {
        executable: "/opt/ngterm/bin/agterm-session".to_owned(),
        session: "agterm-0123".to_owned(),
        argv: argv.iter().map(|arg| (*arg).to_owned()).collect(),
    }
}

/// The app's environment: an outer session's variables, which the launch overrides.
fn base() -> Vec<(String, String)> {
    [
        ("PATH", "/usr/bin"),
        ("AGTERM_LIVE_SESSION", "outer"),
        ("AGTERM_LIVE_MANAGED", "stale"),
        ("GHOSTTY_LOG", "1"),
    ]
    .map(|(key, value)| (key.to_owned(), value.to_owned()))
    .to_vec()
}

/// A wrapped pane's prepared command: the wrap configuration's variables, as `Host::prepare` sets them.
fn command() -> PaneCommand {
    let dir = std::env::temp_dir().join("ngterm-live-test");
    PaneCommand::new("/bin/sh")
        .cwd(std::env::temp_dir())
        .env("AGTERM_LIVE_DIR", &dir)
        .env("AGTERM_LIVE_SESSION", "")
        .env("AGTERM_LIVE_SESSION_PREFIX", "")
        .env("AGTERM_LIVE_NO_DETACH_KEY", "1")
        .env("AGTERM_LIVE_MANAGED", "n1")
        .env("AGTERM_LIVE_MANAGED_CLAIM", "1")
        .env("SHELL", "/bin/zsh")
        .env_remove("GHOSTTY_LOG")
}

#[test]
fn the_in_process_attach_is_the_one_its_shell_line_runs() {
    for argv in [&[][..], &["/bin/zsh", "-lic", "top"][..]] {
        let attach = attach(argv);
        let command = command();
        let session = resolve(&attach, &command, base(), None).unwrap();
        // the CLI reads its command line…
        let arguments: Vec<OsString> = attach.arguments()[1..].iter().map(OsString::from).collect();
        let Ok(Command::Attach { name, argv: line_argv }) = cli::parse(&arguments) else {
            panic!("not an attach: {arguments:?}");
        };
        // …and its environment, the child's
        let contract = Contract::from_env(|key| lookup(&session.spec.env, key).map(Into::into));
        assert_eq!(session.name, contract.session_name(&name).unwrap());
        assert_eq!(Some(session.directory.clone()), contract.directory);
        assert_eq!(session.managed, contract.managed);
        assert_eq!(
            session.managed,
            Some(Managed {
                nonce: "n1".to_owned(),
                claim: true,
            })
        );
        let expected = if line_argv.is_empty() {
            default_argv(Some(std::ffi::OsStr::new("/bin/zsh")))
        } else {
            line_argv
        };
        assert_eq!(session.spec.argv, expected);
        assert_eq!(session.executable, PathBuf::from(&attach.executable));
        // the session is created in the directory and environment the CLI would have run in
        assert_eq!(
            session.spec.cwd.as_deref(),
            Some(std::env::temp_dir().to_string_lossy().as_ref())
        );
        assert_eq!(lookup(&session.spec.env, "PATH"), Some("/usr/bin"));
        assert_eq!(lookup(&session.spec.env, "AGTERM_LIVE_SESSION"), Some(""));
        assert_eq!(lookup(&session.spec.env, "GHOSTTY_LOG"), None);
        assert!(session.spec.tracked_env.is_empty());
    }
}

#[test]
fn an_attach_the_cli_would_refuse_is_refused_with_its_status() {
    // inside another session (the launch keeps the inherited name)
    let inside = PaneCommand::new("/bin/sh").env("AGTERM_LIVE_DIR", "/tmp/ns");
    let refusal = resolve(&attach(&[]), &inside, base(), None).unwrap_err();
    assert_eq!(refusal.code, EXIT_FAILED);
    assert!(
        refusal
            .message
            .as_deref()
            .is_some_and(|message| message.contains("inside the live session outer")),
        "{refusal:?}"
    );
    // no namespace
    let nowhere = PaneCommand::new("/bin/sh").env("AGTERM_LIVE_SESSION", "");
    let refusal = resolve(&attach(&[]), &nowhere, base(), None).unwrap_err();
    assert_eq!(refusal.code, EXIT_USAGE);
    // no session name
    let unnamed = LiveAttach {
        session: String::new(),
        ..attach(&[])
    };
    assert_eq!(
        resolve(&unnamed, &command(), base(), None).unwrap_err().code,
        EXIT_USAGE
    );
    // the app's directory serves a launch without one
    let here = PaneCommand { cwd: None, ..command() };
    let session = resolve(&attach(&[]), &here, base(), Some(PathBuf::from("/app"))).unwrap();
    assert_eq!(session.spec.cwd.as_deref(), Some("/app"));
    // a relative one is the app's, as a plain pane's child resolves it (the server runs from the filesystem root)
    let relative = command().cwd("work/tree");
    let session = resolve(&attach(&[]), &relative, base(), Some(PathBuf::from("/app"))).unwrap();
    assert_eq!(
        session.spec.cwd.map(PathBuf::from),
        Some(Path::new("/app").join("work").join("tree"))
    );
}

#[test]
fn the_childs_environment_is_the_apps_under_the_launchs() {
    let command = PaneCommand::new("x").env("B", "launch").env("C", "new").env_remove("A");
    let env = child_environment(
        &command,
        [("A", "1"), ("B", "2"), ("D", "4")].map(|(key, value)| (key.to_owned(), value.to_owned())),
    );
    assert_eq!(lookup(&env, "A"), None);
    assert_eq!(lookup(&env, "B"), Some("launch"));
    assert_eq!(lookup(&env, "C"), Some("new"));
    assert_eq!(lookup(&env, "D"), Some("4"));
    assert_eq!(env.iter().filter(|(key, _)| key == "B").count(), 1);
    // where names ignore case a launch's `PATH` replaces the app's `Path`
    let path = child_environment(
        &PaneCommand::new("x").env("PATH", "launch"),
        [("Path".to_owned(), "app".to_owned())],
    );
    if cfg!(windows) {
        assert_eq!(path, [("PATH".to_owned(), "launch".to_owned())]);
    } else {
        assert_eq!(path.len(), 2);
    }
}

#[test]
fn only_whole_kitty_answers_are_kept_for_a_barrier() {
    let mut answers = Vec::new();
    keep_kitty_answer(&mut answers, b"\x1b_Gi=1;OK\x1b\\");
    keep_kitty_answer(&mut answers, b"\x1b[?62;22c");
    keep_kitty_answer(&mut answers, b"\x1b]11;rgb:0000/0000/0000\x1b\\");
    keep_kitty_answer(&mut answers, b"\x1b_Gi=2;ENOENT:gone\x1b\\");
    assert_eq!(answers, b"\x1b_Gi=1;OK\x1b\\\x1b_Gi=2;ENOENT:gone\x1b\\");
    let mut full = vec![b'x'; KITTY_ANSWERS_LIMIT];
    keep_kitty_answer(&mut full, b"\x1b_Gi=3;OK\x1b\\");
    assert_eq!(full.len(), KITTY_ANSWERS_LIMIT, "past the limit an answer is dropped");
}

#[test]
fn input_waits_within_the_write_limit_and_none_is_taken_after_leaving() {
    let live = Live::new(ReadPause::new(), 8, true);
    live.input(b"0123456789").unwrap();
    assert_eq!(live.queued(), 10, "an empty queue takes a write whole");
    assert_eq!(live.input(b"x"), Err(PaneError::Backpressure));
    // the pane's own input goes past the limit, uncounted
    live.program_input(b"\x0c");
    assert_eq!(live.queued(), 10);
    live.leave();
    assert_eq!(live.input(b"x"), Err(PaneError::Closed));
    assert_eq!(live.queued(), 0);
}

#[test]
fn facts_are_sent_only_when_they_change_and_an_unknown_answer_is_dropped() {
    let live = Live::new(ReadPause::new(), 1024, true);
    let facts = ReplyMetadata {
        cell_width_px: 8,
        ..ReplyMetadata::default()
    };
    live.update_metadata(facts.clone());
    live.update_metadata(facts.clone());
    assert_eq!(lock(&live.outbox).queue.len(), 1);
    assert_eq!(*lock(&live.metadata), facts);
    live.answer(7, ClipboardAnswer::Denied);
    assert_eq!(lock(&live.outbox).queue.len(), 1, "no read 7 waits");
    live.resize(GridSize {
        cols: 80,
        rows: 24,
        cell_width: 8,
        cell_height: 16,
    });
    let queued: Vec<String> = lock(&live.outbox)
        .queue
        .iter()
        .map(|message| format!("{message:?}"))
        .collect();
    assert!(queued[1].starts_with("Resize"), "{queued:?}");
}
