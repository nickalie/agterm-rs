//! The on-disk JSON shape of upstream's state files: Foundation's `JSONEncoder` with
//! `outputFormatting = [.prettyPrinted, .sortedKeys]`, and the atomic write of `Data.write(options: .atomic)`.
//!
//! Foundation's pretty printer indents by two spaces, separates a key from its value with `" : "`, prints an empty
//! array or object as an opening bracket, a blank line and the closing bracket, escapes `/` as `\/`, prints a
//! `Double` with an integral value without its `.0`, and sorts keys case-insensitively (`[.numeric,
//! .caseInsensitive, .forcedOrdering]`). Writing the same bytes keeps a file this build saved indistinguishable from
//! one upstream or the Linux fork saved; reading never depends on any of it.

use std::cmp::Ordering;
use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

use serde::Serialize;
use serde_json::{Map, Number, Value};

/// Encodes `value` as Foundation's pretty-printed, key-sorted JSON (no trailing newline, as Foundation writes none).
///
/// # Errors
///
/// `value` does not serialize to JSON (a map with non-string keys, a non-finite float).
pub fn to_foundation_pretty(value: &impl Serialize) -> serde_json::Result<Vec<u8>> {
    let value = serde_json::to_value(value)?;
    let mut out = String::new();
    write_value(&mut out, &value, 0);
    Ok(out.into_bytes())
}

/// Foundation's `.sortedKeys` order: case-insensitive, digit runs by their numeric value, and a literal comparison
/// breaking the remaining ties so the order is total.
#[must_use]
pub fn foundation_key_order(a: &str, b: &str) -> Ordering {
    let folded = |key: &str| key.chars().flat_map(char::to_lowercase).collect::<Vec<_>>();
    compare_numeric(&folded(a), &folded(b)).then_with(|| a.cmp(b))
}

/// Compares two character sequences, a run of ASCII digits in both by its numeric value.
fn compare_numeric(a: &[char], b: &[char]) -> Ordering {
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            let (run_a, run_b) = (digit_run(&a[i..]), digit_run(&b[j..]));
            let ordering = compare_digit_runs(&a[i..i + run_a], &b[j..j + run_b]);
            if ordering != Ordering::Equal {
                return ordering;
            }
            i += run_a;
            j += run_b;
            continue;
        }
        let ordering = a[i].cmp(&b[j]);
        if ordering != Ordering::Equal {
            return ordering;
        }
        i += 1;
        j += 1;
    }
    (a.len() - i).cmp(&(b.len() - j))
}

fn digit_run(chars: &[char]) -> usize {
    chars.iter().take_while(|c| c.is_ascii_digit()).count()
}

/// Compares two digit runs by value: leading zeros dropped, then the longer run is the larger number.
fn compare_digit_runs(a: &[char], b: &[char]) -> Ordering {
    let significant = |run: &[char]| -> Vec<char> { run.iter().copied().skip_while(|c| *c == '0').collect() };
    let (a, b) = (significant(a), significant(b));
    a.len().cmp(&b.len()).then_with(|| a.cmp(&b))
}

fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn write_value(out: &mut String, value: &Value, depth: usize) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Value::Number(number) => write_number(out, number),
        Value::String(text) => write_string(out, text),
        Value::Array(items) => write_array(out, items, depth),
        Value::Object(map) => write_object(out, map, depth),
    }
}

fn write_array(out: &mut String, items: &[Value], depth: usize) {
    out.push_str("[\n");
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            out.push_str(",\n");
        }
        indent(out, depth + 1);
        write_value(out, item, depth + 1);
    }
    out.push('\n');
    indent(out, depth);
    out.push(']');
}

fn write_object(out: &mut String, map: &Map<String, Value>, depth: usize) {
    let mut entries: Vec<(&String, &Value)> = map.iter().collect();
    entries.sort_by(|(a, _), (b, _)| foundation_key_order(a, b));
    out.push_str("{\n");
    for (index, (key, item)) in entries.into_iter().enumerate() {
        if index > 0 {
            out.push_str(",\n");
        }
        indent(out, depth + 1);
        write_string(out, key);
        out.push_str(" : ");
        write_value(out, item, depth + 1);
    }
    out.push('\n');
    indent(out, depth);
    out.push('}');
}

/// A `Double` with an integral value prints without its `.0` (Foundation trims it from the description); integers
/// print as they are.
fn write_number(out: &mut String, number: &Number) {
    match number.as_f64() {
        Some(float) if number.is_f64() => {
            if float.is_finite() && float.fract() == 0.0 && float.abs() < 1e16 {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "an integral value below 1e16 converts to i64 exactly"
                )]
                let integral = float as i64;
                out.push_str(&integral.to_string());
            } else {
                out.push_str(&float.to_string());
            }
        }
        _ => out.push_str(&number.to_string()),
    }
}

/// Foundation's string escaping: quote, backslash and slash, the short forms of the common controls, `\u00xx` (lower
/// case) for the rest of C0; everything else as UTF-8.
fn write_string(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '/' => out.push_str("\\/"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Distinguishes the temporary files of concurrent writes within one process.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Writes `bytes` to `path` atomically, as `Data.write(options: .atomic)`: a temporary file in the same directory,
/// renamed over the destination, so a crash never leaves a half-written file. The directory must exist.
///
/// # Errors
///
/// The temporary file could not be created or written, or the rename failed; the temporary file is removed then.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let counter = TEMP_COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
    let temp = dir.join(format!(".{name}.{}.{counter}.tmp", std::process::id()));
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .and_then(|mut file| {
            file.write_all(bytes)?;
            file.sync_all()
        })
        .and_then(|()| fs::rename(&temp, path));
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
}

/// Encodes `value` as Foundation would and writes it atomically to `path`, creating the parent directories first
/// (`createDirectory(withIntermediateDirectories: true)` before every upstream save).
///
/// # Errors
///
/// The encode, the directory creation or the write failed.
pub fn save_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let bytes = to_foundation_pretty(value).map_err(io::Error::other)?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    write_atomic(path, &bytes)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn pretty(value: &Value) -> String {
        String::from_utf8(to_foundation_pretty(value).unwrap()).unwrap()
    }

    #[test]
    fn pretty_prints_like_foundation() {
        let value = json!({"b": [1, 2], "a": {"x": "y"}, "c": []});
        assert_eq!(
            pretty(&value),
            "{\n  \"a\" : {\n    \"x\" : \"y\"\n  },\n  \"b\" : [\n    1,\n    2\n  ],\n  \"c\" : [\n\n  ]\n}"
        );
        assert_eq!(pretty(&json!({})), "{\n\n}");
    }

    #[test]
    fn keys_sort_case_insensitively_then_literally() {
        let value = json!({"focusEnabled": true, "focusedWorkspaceIDs": [], "Zeta": 1, "alpha": 2, "Alpha": 3});
        let text = pretty(&value);
        let order: Vec<usize> = [
            "\"Alpha\"",
            "\"alpha\"",
            "\"focusedWorkspaceIDs\"",
            "\"focusEnabled\"",
            "\"Zeta\"",
        ]
        .iter()
        .map(|key| text.find(key).unwrap())
        .collect();
        assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{text}");
    }

    #[test]
    fn digit_runs_sort_by_value() {
        assert_eq!(foundation_key_order("item2", "item10"), Ordering::Less);
        assert_eq!(foundation_key_order("item10", "item10"), Ordering::Equal);
        assert_eq!(foundation_key_order("item02", "item2"), Ordering::Less);
    }

    #[test]
    fn integral_doubles_drop_the_fraction_and_slashes_are_escaped() {
        let value = json!({"w": 220.0, "r": 0.63, "n": 3, "p": "/a/\"b\"\n\u{1}"});
        assert_eq!(
            pretty(&value),
            "{\n  \"n\" : 3,\n  \"p\" : \"\\/a\\/\\\"b\\\"\\n\\u0001\",\n  \"r\" : 0.63,\n  \"w\" : 220\n}"
        );
    }

    #[test]
    fn non_ascii_text_is_written_as_is() {
        assert_eq!(pretty(&json!("Grüße ✓")), "\"Grüße ✓\"");
    }

    #[test]
    fn an_atomic_write_replaces_the_file_and_leaves_no_temporary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["a.json"]);
    }

    #[test]
    fn save_json_creates_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/dir/x.json");
        save_json(&path, &json!({"k": 1})).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\n  \"k\" : 1\n}");
    }

    #[test]
    fn a_failed_write_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        // the destination is a directory, so the rename fails after the temporary was written
        let path = dir.path().join("taken");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("inside"), b"x").unwrap();
        assert!(write_atomic(&path, b"data").is_err());
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["taken"]);
    }
}
