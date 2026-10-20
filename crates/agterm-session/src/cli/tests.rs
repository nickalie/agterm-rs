use std::collections::HashMap;

use super::*;
use crate::server::BUILD_ID;

fn contract(pairs: &[(&str, &str)]) -> Contract {
    let map: HashMap<String, OsString> = pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), OsString::from(value)))
        .collect();
    Contract::from_env(|name| map.get(name).cloned())
}

fn args(words: &[&str]) -> Vec<OsString> {
    words.iter().map(OsString::from).collect()
}

#[test]
fn the_contract_takes_an_empty_variable_as_unset() {
    let set = contract(&[
        ("AGTERM_LIVE_DIR", "/tmp/ns"),
        ("AGTERM_LIVE_SESSION", "agterm-1"),
        ("AGTERM_LIVE_SESSION_PREFIX", "p-"),
        ("AGTERM_LIVE_NO_DETACH_KEY", "1"),
        ("AGTERM_LIVE_MANAGED", "n1"),
        ("AGTERM_LIVE_MANAGED_CLAIM", "1"),
    ]);
    assert_eq!(
        set,
        Contract {
            directory: Some(PathBuf::from("/tmp/ns")),
            session: Some("agterm-1".to_owned()),
            prefix: "p-".to_owned(),
            detach_key: false,
            managed: Some(Managed {
                nonce: "n1".to_owned(),
                claim: true
            }),
        }
    );
    let empty = contract(&[
        ("AGTERM_LIVE_DIR", ""),
        ("AGTERM_LIVE_SESSION", ""),
        ("AGTERM_LIVE_SESSION_PREFIX", ""),
        ("AGTERM_LIVE_NO_DETACH_KEY", ""),
        ("AGTERM_LIVE_MANAGED", ""),
        ("AGTERM_LIVE_MANAGED_CLAIM", "1"),
    ]);
    assert_eq!(
        empty,
        Contract {
            directory: None,
            session: None,
            prefix: String::new(),
            detach_key: true,
            managed: None,
        }
    );
    let unclaimed = contract(&[("AGTERM_LIVE_MANAGED", "n1"), ("AGTERM_LIVE_MANAGED_CLAIM", "")]);
    assert_eq!(unclaimed.managed.map(|managed| managed.claim), Some(false));
}

#[test]
fn a_nonce_that_is_no_token_leaves_the_attach_unmanaged() {
    for nonce in ["a;b", "a b", &"x".repeat(65)] {
        assert_eq!(contract(&[("AGTERM_LIVE_MANAGED", nonce)]).managed, None, "{nonce:?}");
    }
}

#[test]
fn session_names_take_the_prefix_and_refuse_paths() {
    let prefixed = contract(&[("AGTERM_LIVE_SESSION_PREFIX", "p-")]);
    assert_eq!(prefixed.session_name("a"), Ok("p-a".to_owned()));
    assert_eq!(prefixed.session_name(""), Ok("p-".to_owned()));
    let plain = Contract::default();
    assert!(plain.session_name("").is_err());
    assert!(plain.session_name("a/b").is_err());
    assert!(plain.session_name("..").is_err());
}

#[test]
fn command_lines_parse_as_zmx_spells_them() {
    assert_eq!(
        parse(&args(&["attach", "s", "/bin/sh", "-c", "exit 1"])),
        Ok(Command::Attach {
            name: "s".to_owned(),
            argv: vec!["/bin/sh".to_owned(), "-c".to_owned(), "exit 1".to_owned()]
        })
    );
    assert_eq!(parse(&args(&["list"])), Ok(Command::List { short: false }));
    assert_eq!(parse(&args(&["ls", "--short"])), Ok(Command::List { short: true }));
    assert_eq!(
        parse(&args(&["kill", "a", "--force", "b"])),
        Ok(Command::Kill {
            names: vec!["a".to_owned(), "b".to_owned()],
            force: true
        })
    );
    assert_eq!(
        parse(&args(&["screen", "--all", "s"])),
        Ok(Command::Screen {
            name: "s".to_owned(),
            all: true
        })
    );
    assert_eq!(parse(&args(&["type", "s"])), Ok(Command::Type { name: "s".to_owned() }));
    assert_eq!(
        parse(&args(&["print", "s", "a", "b"])),
        Ok(Command::Print {
            name: "s".to_owned(),
            text: Some("a b".to_owned())
        })
    );
    assert_eq!(
        parse(&args(&["print", "s"])),
        Ok(Command::Print {
            name: "s".to_owned(),
            text: None
        })
    );
    assert_eq!(parse(&args(&["version"])), Ok(Command::Version));
    for bad in [
        &[][..],
        &["attach"],
        &["kill"],
        &["kill", "--all", "a"],
        &["screen"],
        &["screen", "a", "b"],
        &["nope"],
    ] {
        assert!(parse(&args(bad)).is_err(), "{bad:?}");
    }
}

#[test]
fn a_server_line_with_no_or_two_directories_says_so() {
    assert_eq!(
        parse(&args(&["server"])),
        Err("server needs the namespace directory".to_owned())
    );
    assert_eq!(
        parse(&args(&["server", "a", "b"])),
        Err("server takes one namespace directory, not \"b\" too".to_owned())
    );
}

#[test]
fn a_malformed_handoff_import_server_line_says_what_the_import_takes() {
    let wrong_count = Err("server's --handoff-import takes a channel and a token".to_owned());
    for bad in [
        &["server", "/ns", "--handoff-import"][..],
        &["server", "/ns", "--handoff-import", "/chan"],
        &["server", "/ns", "--handoff-import", "ch", "tok", "extra"],
    ] {
        assert_eq!(parse(&args(bad)), wrong_count, "{bad:?}");
    }
    // the well-formed import line is the binary's own, like the one-directory form
    assert_eq!(
        parse(&args(&["server", "/ns", "--handoff-import", "ch", "tok"])),
        Err("server runs only as agterm-session's own command line".to_owned())
    );
    assert!(USAGE.contains(&format!("[{} <channel> <token>]", handoff::IMPORT_FLAG)));
}

#[test]
fn list_and_screen_lines_are_zmxs() {
    let record = SessionRecord {
        name: "agterm-1".to_owned(),
        clients: 2,
        leader_pid: Some(42),
        created_at: 1_700_000_000,
    };
    assert_eq!(
        session_line(&record),
        "name=agterm-1\tpid=42\tclients=2\tcreated=1700000000"
    );
    let ended = SessionRecord {
        leader_pid: None,
        ..record
    };
    assert_eq!(
        session_line(&ended),
        "name=agterm-1\tpid=-\tclients=2\tcreated=1700000000"
    );
    let screen = Screen {
        revision: 9,
        columns: 80,
        rows: 24,
        cursor_column: 3,
        cursor_row: 1,
        alternate: true,
        text: String::new(),
    };
    assert_eq!(screen_header(&screen), "9 80 24 3 1 1");
}

#[test]
fn a_command_without_a_namespace_is_a_usage_error() {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = execute(
        &Command::List { short: false },
        &Contract::default(),
        &mut std::io::empty(),
        &mut out,
        &mut err,
    );
    assert_eq!(code, EXIT_USAGE);
    assert!(String::from_utf8_lossy(&err).contains("AGTERM_LIVE_DIR is not set"));
}

#[test]
fn version_and_help_need_no_namespace() {
    let mut out = Vec::new();
    let mut err = Vec::new();
    assert_eq!(
        execute(
            &Command::Version,
            &Contract::default(),
            &mut std::io::empty(),
            &mut out,
            &mut err
        ),
        EXIT_OK
    );
    let text = String::from_utf8_lossy(&out);
    assert!(text.contains(&format!("protocol\t{PROTOCOL_VERSION}")), "{text}");
    assert!(text.contains(BUILD_ID), "{text}");
    let mut out = Vec::new();
    assert_eq!(
        execute(
            &Command::Help,
            &Contract::default(),
            &mut std::io::empty(),
            &mut out,
            &mut err
        ),
        EXIT_OK
    );
    assert!(String::from_utf8_lossy(&out).contains("usage: agterm-session"));
}

#[test]
fn a_session_without_an_argv_runs_the_shell() {
    let shell = |value: &str| default_argv(Some(OsStr::new(value)));
    if cfg!(windows) {
        assert_eq!(shell("pwsh.exe"), ["pwsh.exe"]);
    } else {
        assert_eq!(shell("/bin/zsh"), ["/bin/zsh", "-l"]);
        assert_eq!(shell("/usr/bin/nu"), ["/usr/bin/nu"]);
    }
    assert!(!default_argv(None).is_empty());
    assert!(!default_argv(Some(OsStr::new(""))).is_empty());
}
