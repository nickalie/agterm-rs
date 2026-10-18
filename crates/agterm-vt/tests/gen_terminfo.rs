//! `gen-terminfo` end to end, on a file of its own: it writes the source, `--check` passes on what it wrote and on
//! the checked-in source, and fails on a changed or missing file; a bad argument or an unwritable file exits 2. The
//! checked-in source is held to the entry by `scripts/gen-ghostty-terminfo.sh --check`, which CI runs on every OS.

use std::fs;
use std::io;
use std::path::Path;
use std::process::{Command, Output};

fn gen_terminfo(args: &[&str], out: Option<&Path>) -> io::Result<Output> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_gen-terminfo"));
    command.args(args);
    if let Some(out) = out {
        command.arg("--out").arg(out);
    }
    command.output()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn it_writes_the_source_and_check_passes_on_it() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("xterm-ghostty.terminfo");

    let written = gen_terminfo(&[], Some(&out)).unwrap();
    assert!(written.status.success(), "{}", stderr(&written));
    assert!(stderr(&written).contains("wrote"), "{}", stderr(&written));
    let source = fs::read_to_string(&out).unwrap();
    assert!(source.starts_with("# xterm-ghostty: Ghostty's terminfo entry"));
    assert!(source.contains("\nxterm-ghostty|ghostty|Ghostty,\n\tam,\n"));

    let check = gen_terminfo(&["--check"], Some(&out)).unwrap();
    assert_eq!(check.status.code(), Some(0), "{}", stderr(&check));
    assert!(stderr(&check).contains("is current"), "{}", stderr(&check));

    // a second run writes nothing
    let again = gen_terminfo(&[], Some(&out)).unwrap();
    assert!(again.status.success());
    assert!(stderr(&again).contains("is current"), "{}", stderr(&again));

    // what it writes is the checked-in source
    let checked_in = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("terminfo/xterm-ghostty.terminfo"))
        .unwrap()
        .replace("\r\n", "\n");
    assert_eq!(source, checked_in);
}

#[test]
fn check_passes_on_the_checked_in_source() {
    let check = gen_terminfo(&["--check"], None).unwrap();
    assert_eq!(check.status.code(), Some(0), "{}", stderr(&check));
}

#[test]
fn check_fails_on_a_changed_or_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("xterm-ghostty.terminfo");
    assert!(gen_terminfo(&[], Some(&out)).unwrap().status.success());

    let source = fs::read_to_string(&out).unwrap();
    fs::write(&out, source.replace("\tcolors#256,\n", "\tcolors#8,\n")).unwrap();
    let changed = gen_terminfo(&["--check"], Some(&out)).unwrap();
    assert_eq!(changed.status.code(), Some(1), "{}", stderr(&changed));
    assert!(stderr(&changed).contains("is stale"), "{}", stderr(&changed));
    // --check wrote nothing
    assert!(fs::read_to_string(&out).unwrap().contains("\tcolors#8,\n"));

    fs::remove_file(&out).unwrap();
    let missing = gen_terminfo(&["--check"], Some(&out)).unwrap();
    assert_eq!(missing.status.code(), Some(1), "{}", stderr(&missing));
    assert!(!out.exists());
}

#[test]
fn check_takes_crlf_line_endings_as_equal() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("xterm-ghostty.terminfo");
    assert!(gen_terminfo(&[], Some(&out)).unwrap().status.success());
    let source = fs::read_to_string(&out).unwrap();
    fs::write(&out, source.replace('\n', "\r\n")).unwrap();
    let check = gen_terminfo(&["--check"], Some(&out)).unwrap();
    assert_eq!(check.status.code(), Some(0), "{}", stderr(&check));
}

#[test]
fn a_bad_argument_or_an_unwritable_file_exits_2() {
    let bogus = gen_terminfo(&["--bogus"], None).unwrap();
    assert_eq!(bogus.status.code(), Some(2));
    assert!(
        stderr(&bogus).contains("unknown argument --bogus"),
        "{}",
        stderr(&bogus)
    );

    // a directory where the file should be
    let dir = tempfile::tempdir().unwrap();
    let unwritable = gen_terminfo(&[], Some(dir.path())).unwrap();
    assert_eq!(unwritable.status.code(), Some(2), "{}", stderr(&unwritable));
}
