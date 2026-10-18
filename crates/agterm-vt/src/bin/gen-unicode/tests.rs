//! The command line, the uucode version, and how `--check` and the writer compare the generated files with the ones
//! on disk.

use std::fs;

use super::{parse_args, stale_files, uucode_version, write_files};

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|&a| a.to_owned()).collect()
}

#[test]
fn the_command_line_needs_the_inputs_and_defaults_the_output() {
    let parsed = parse_args(
        args(&[
            "--uucode",
            "u",
            "--uucode-rev",
            "abc",
            "--ucd-version",
            "18.0.0",
            "--check",
        ])
        .into_iter(),
    )
    .unwrap();
    assert_eq!(parsed.uucode, std::path::Path::new("u"));
    assert_eq!(parsed.uucode_rev, "abc");
    assert_eq!(parsed.ucd_version, "18.0.0");
    assert!(parsed.check);
    assert!(parsed.out.ends_with("src/unicode/generated"));

    let parsed =
        parse_args(args(&["--out", "o", "--uucode", "u", "--uucode-rev", "r", "--ucd-version", "v"]).into_iter())
            .unwrap();
    assert_eq!(parsed.out, std::path::Path::new("o"));
    assert!(!parsed.check);
}

#[test]
fn the_command_line_refuses_missing_values_and_unknown_arguments() {
    let error = |list: &[&str]| parse_args(args(list).into_iter()).err().unwrap();
    assert!(error(&["--uucode-rev", "r", "--ucd-version", "v"]).starts_with("--uucode is required"));
    assert!(error(&["--uucode", "u", "--ucd-version", "v"]).starts_with("--uucode-rev is required"));
    assert!(error(&["--uucode", "u", "--uucode-rev", "r"]).starts_with("--ucd-version is required"));
    assert!(error(&["--uucode"]).starts_with("--uucode needs a value"));
    assert!(error(&["--force"]).starts_with("unknown argument --force"));
}

#[test]
fn the_uucode_version_comes_from_its_zon() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("build.zig.zon"),
        ".{\n    .name = .uucode,\n    .version = \"0.2.0\",\n    .minimum_zig_version = \"0.16.0\",\n}\n",
    )
    .unwrap();
    assert_eq!(uucode_version(dir.path()), Ok("0.2.0".to_owned()));
    fs::write(dir.path().join("build.zig.zon"), ".{ .name = .uucode }\n").unwrap();
    assert!(uucode_version(dir.path()).unwrap_err().contains("no .version line"));
    assert!(uucode_version(&dir.path().join("missing")).is_err());
}

fn generated() -> Vec<(&'static str, String)> {
    vec![
        ("a.rs", "//! a\nconst A: u8 = 1;\n".to_owned()),
        ("b.rs", "//! b\n".to_owned()),
    ]
}

/// `--check`'s comparison: a changed or missing file is stale, a file with CRLF line endings is not.
#[test]
fn a_changed_or_missing_table_is_stale() {
    let dir = tempfile::tempdir().unwrap();
    let files = generated();
    assert_eq!(stale_files(dir.path(), &files), ["a.rs", "b.rs"]);

    for (name, contents) in &files {
        fs::write(dir.path().join(name), contents).unwrap();
    }
    assert!(stale_files(dir.path(), &files).is_empty());

    fs::write(dir.path().join("a.rs"), "//! a\nconst A: u8 = 2;\n").unwrap();
    assert_eq!(stale_files(dir.path(), &files), ["a.rs"]);

    fs::write(dir.path().join("a.rs"), "//! a\r\nconst A: u8 = 1;\r\n").unwrap();
    assert!(stale_files(dir.path(), &files).is_empty());

    fs::remove_file(dir.path().join("b.rs")).unwrap();
    assert_eq!(stale_files(dir.path(), &files), ["b.rs"]);
}

/// The writer writes the stale files only, and names them.
#[test]
fn the_writer_writes_only_what_changed() {
    let dir = tempfile::tempdir().unwrap();
    let files = generated();
    assert_eq!(write_files(dir.path(), &files).unwrap(), ["a.rs", "b.rs"]);
    assert_eq!(fs::read_to_string(dir.path().join("a.rs")).unwrap(), files[0].1);
    assert!(write_files(dir.path(), &files).unwrap().is_empty());

    fs::write(dir.path().join("b.rs"), "stale").unwrap();
    assert_eq!(write_files(dir.path(), &files).unwrap(), ["b.rs"]);
    assert_eq!(fs::read_to_string(dir.path().join("b.rs")).unwrap(), files[1].1);
    assert!(write_files(&dir.path().join("missing"), &files).is_err());
}
