//! The checked-in terminfo source, compiled with `tic -x` into a temporary directory, is the entry the Zig build
//! compiled: `infocmp -x` reads the same entry back from it as from the Zig-built prefix, whose `infocmp` output was
//! captured once as `fixtures/xterm-ghostty.infocmp` so this check outlives that build. The comparison is of the
//! entry, not of `infocmp`'s layout: the names, then every capability with its value (numbers as numbers, as an
//! ncurses release may print them in hex or decimal), in any order and line wrapping.
//!
//! `tic` and `infocmp` write and read only the temporary directory (`-o`, `-A`), never `~/.terminfo`, with `TERMINFO`
//! and `TERMINFO_DIRS` cleared.

use std::collections::BTreeSet;
use std::error::Error;
use std::path::Path;
use std::process::Command;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const SOURCE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/terminfo/xterm-ghostty.terminfo");
const FIXTURE: &str = include_str!("fixtures/xterm-ghostty.infocmp");

/// An entry as `infocmp` prints it: its names, and its capabilities as `name`, `name@`, `name#number` (decimal) or
/// `name=value` fields.
#[derive(Debug, PartialEq, Eq)]
struct Entry {
    names: String,
    capabilities: BTreeSet<String>,
}

/// The comma-separated fields of terminfo source with `#` comment lines dropped, trimmed; a backslash escapes the
/// character after it (`\,` is a comma inside a value, `\\` a backslash).
fn fields(source: &str) -> Vec<String> {
    let mut text = String::new();
    for line in source.lines().filter(|line| !line.starts_with('#')) {
        text.push_str(line.trim_end_matches('\r'));
        text.push('\n');
    }
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                field.push(c);
                field.extend(chars.next());
            }
            ',' => fields.push(std::mem::take(&mut field)),
            c => field.push(c),
        }
    }
    fields.push(field);
    fields
        .into_iter()
        .map(|f| f.trim().to_owned())
        .filter(|f| !f.is_empty())
        .collect()
}

/// A numeric capability's value as a number: decimal, `0x` hex or `0` octal, as terminfo(5) reads it.
fn number(value: &str) -> TestResult<u64> {
    Ok(
        if let Some(hex) = value.strip_prefix("0x").or_else(|| value.strip_prefix("0X")) {
            u64::from_str_radix(hex, 16)?
        } else if let Some(octal) = value.strip_prefix('0').filter(|rest| !rest.is_empty()) {
            u64::from_str_radix(octal, 8)?
        } else {
            value.parse()?
        },
    )
}

fn entry(source: &str) -> TestResult<Entry> {
    let mut fields = fields(source).into_iter();
    let names = fields.next().ok_or("no names")?;
    let capabilities = fields
        .map(|field| match field.split_once('#') {
            Some((name, value)) if !name.contains('=') => Ok(format!("{name}#{}", number(value)?)),
            _ => Ok(field),
        })
        .collect::<TestResult<_>>()?;
    Ok(Entry { names, capabilities })
}

fn run(command: &mut Command) -> TestResult<String> {
    let output = command
        .env_remove("TERMINFO")
        .env_remove("TERMINFO_DIRS")
        .output()
        .map_err(|e| format!("{command:?}: {e} (tic and infocmp come with ncurses)"))?;
    if !output.status.success() {
        return Err(format!(
            "{command:?}: {}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn compiled_and_read_back(source: &Path) -> TestResult<String> {
    let dir = tempfile::tempdir()?;
    run(Command::new("tic").arg("-x").arg("-o").arg(dir.path()).arg(source))?;
    run(Command::new("infocmp")
        .arg("-x")
        .arg("-A")
        .arg(dir.path())
        .arg("xterm-ghostty"))
}

#[test]
#[cfg_attr(
    windows,
    ignore = "Windows has no ncurses tic and infocmp; the Linux and macOS runs hold the source to the fixture"
)]
fn the_checked_in_source_compiles_to_the_zig_built_entry() -> TestResult {
    let fixture = entry(FIXTURE)?;
    // the fixture is the whole entry: the names and every capability Ghostty's entry has
    assert_eq!(fixture.names, "xterm-ghostty|ghostty|Ghostty");
    assert_eq!(fixture.capabilities.len(), 270);

    let compiled = entry(&compiled_and_read_back(Path::new(SOURCE))?)?;
    let missing: Vec<_> = fixture.capabilities.difference(&compiled.capabilities).collect();
    let extra: Vec<_> = compiled.capabilities.difference(&fixture.capabilities).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "missing {missing:?}, extra {extra:?}"
    );
    assert_eq!(compiled, fixture);
    Ok(())
}

#[test]
#[cfg_attr(
    windows,
    ignore = "Windows has no ncurses tic and infocmp; the Linux and macOS runs hold the source to the fixture"
)]
fn a_changed_capability_is_told_apart() -> TestResult {
    let changed = std::fs::read_to_string(SOURCE)?.replace("\tcolors#256,\n", "\tcolors#88,\n");
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("changed.terminfo");
    std::fs::write(&path, changed)?;
    let compiled = entry(&compiled_and_read_back(&path)?)?;
    assert!(compiled.capabilities.contains("colors#88"));
    assert_ne!(compiled, entry(FIXTURE)?);
    Ok(())
}

#[test]
fn fields_honor_escapes_and_drop_comments() -> TestResult {
    let source = "# a comment, with commas\nname|alias,\n\tacsc=a\\,b, initc=\\E\\\\,\r\n\tcolors#0x100, it#010, km,\n";
    assert_eq!(
        fields(source),
        [
            "name|alias",
            "acsc=a\\,b",
            "initc=\\E\\\\",
            "colors#0x100",
            "it#010",
            "km"
        ]
    );
    let entry = entry(source)?;
    assert_eq!(entry.names, "name|alias");
    assert!(entry.capabilities.contains("colors#256"));
    assert!(entry.capabilities.contains("it#8"));
    assert!(number("x").is_err());
    Ok(())
}
