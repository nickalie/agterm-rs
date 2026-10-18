//! The checked-in terminfo source, compiled with `tic -x` into a temporary directory, is the entry the Zig build
//! compiled: `infocmp -x` reads the same entry back from it as from the Zig-built prefix, whose `infocmp` output was
//! captured once as `fixtures/xterm-ghostty.infocmp` so this check outlives that build. The comparison is of the
//! entry, not of `infocmp`'s layout: the names, then every capability with its value, in any order and line
//! wrapping, numbers as numbers and strings as the bytes they stand for, as ncurses releases print them differently
//! (6.x writes `colors#0x100`, `cr=\r`, `kbs=^?`, `\E[4:%p1%dm`; macOS's 5.7 `colors#256`, `cr=^M`, `kbs=\177`,
//! `\E[4\:%p1%dm`).
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
/// `name=value` fields, the value the bytes it stands for, ASCII-escaped.
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

/// The bytes a string capability's value stands for (terminfo(5)): `\E` and `\e` ESC, `\n` and `\l` LF, `\r`, `\t`,
/// `\b`, `\f`, `\s` (space), up to three octal digits (`\0` alone is ncurses' NUL, `\200`), any other escaped
/// character itself (`\\`, `\,`, `\:`, `\^`), `^X` a control character (`^?` DEL); a `%` takes the character after
/// it as written, so `%^` (xor) stays an operator.
fn decode(value: &str) -> TestResult<Vec<u8>> {
    let mut out = Vec::with_capacity(value.len());
    let mut bytes = value.bytes().peekable();
    while let Some(b) = bytes.next() {
        match b {
            b'\\' => match bytes.next().ok_or("a backslash at the end")? {
                b'E' | b'e' => out.push(0x1B),
                b'n' | b'l' => out.push(b'\n'),
                b'r' => out.push(b'\r'),
                b't' => out.push(b'\t'),
                b'b' => out.push(0x08),
                b'f' => out.push(0x0C),
                b's' => out.push(b' '),
                digit @ b'0'..=b'7' => {
                    let mut code = u32::from(digit - b'0');
                    let mut digits = 1;
                    while digits < 3
                        && let Some(&next @ b'0'..=b'7') = bytes.peek()
                    {
                        code = code * 8 + u32::from(next - b'0');
                        digits += 1;
                        bytes.next();
                    }
                    out.push(if digits == 1 && code == 0 {
                        0x80
                    } else {
                        u8::try_from(code)?
                    });
                }
                other => out.push(other),
            },
            b'^' => match bytes.next().ok_or("a caret at the end")? {
                b'?' => out.push(0x7F),
                c => out.push(c & 0x1F),
            },
            b'%' => {
                out.push(b);
                out.extend(bytes.next());
            }
            b => out.push(b),
        }
    }
    Ok(out)
}

fn entry(source: &str) -> TestResult<Entry> {
    let mut fields = fields(source).into_iter();
    let names = fields.next().ok_or("no names")?;
    let capabilities = fields
        .map(|field| {
            Ok(match (field.split_once('='), field.split_once('#')) {
                (Some((name, value)), _) => format!("{name}={}", decode(value)?.escape_ascii()),
                (None, Some((name, value))) => format!("{name}#{}", number(value)?),
                (None, None) => field,
            })
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
    assert!(entry.capabilities.contains("acsc=a,b"));
    assert!(entry.capabilities.contains("initc=\\x1b\\\\"));
    assert!(number("x").is_err());
    Ok(())
}

#[test]
fn values_compare_as_the_bytes_they_stand_for() -> TestResult {
    // how ncurses 6.x and macOS's 5.7 print the same values
    for (a, b) in [
        (r"\r", "^M"),
        (r"\n", "^J"),
        ("^?", r"\177"),
        (r"\E[4:%p1%dm", r"\E[4\:%p1%dm"),
        (r"\E]2;\007", r"\E]2;^G"),
        (r"\E", r"\033"),
    ] {
        assert_eq!(decode(a)?, decode(b)?, "{a} {b}");
    }
    assert_eq!(decode(r"\E[%p1%d q")?, b"\x1b[%p1%d q");
    assert_eq!(decode("%p1%^%d")?, b"%p1%^%d");
    assert_eq!(decode(r"\0")?, [0x80]);
    assert_eq!(decode(r"\s\,\^\\")?, br" ,^\");
    assert_ne!(decode(r"\E[4m")?, decode(r"\E[5m")?);
    assert!(decode("\\").is_err());
    assert!(decode("^").is_err());
    Ok(())
}
