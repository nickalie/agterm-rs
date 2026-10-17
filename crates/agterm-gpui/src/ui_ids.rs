//! Upstream's accessibility identifiers (`conformance/ui-ids.yaml`, generated from upstream's Swift sources) are the
//! GPUI build's element ids: a future Linux UI driver finds a control by the id upstream's UI tests use. This test
//! holds the code to the catalog: every id is spelled as a string literal in the frontend's non-test sources, an
//! interpolated id (`palette-item-{item}`) by the literal part before its first placeholder.

use std::path::{Path, PathBuf};

/// The catalog's ids, both the generated (`ids:`) and the hand-maintained (`manual:`) ones.
fn catalog() -> Vec<String> {
    let text = include_str!("../../../conformance/ui-ids.yaml");
    text.lines()
        .filter_map(|line| {
            let entry = line.strip_prefix("  ")?;
            if entry.starts_with(' ') {
                return None;
            }
            let (id, rest) = entry.split_once(':')?;
            rest.trim_start().starts_with('{').then(|| id.to_owned())
        })
        .collect()
}

/// The frontend's sources, its test modules left out.
fn sources(dir: &Path, found: &mut Vec<(PathBuf, String)>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .expect("the source directory reads")
        .map(|entry| entry.expect("a directory entry").path())
        .collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
        if path.is_dir() {
            sources(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "rs")
            && !name.contains("tests")
            && name != "ui_ids.rs"
            && name != "testing.rs"
        {
            let text = std::fs::read_to_string(&path).expect("a source file reads");
            found.push((path, text));
        }
    }
}

#[test]
fn every_upstream_accessibility_id_is_an_element_id_of_the_gpui_build() {
    let ids = catalog();
    assert!(ids.len() > 100, "the catalog parsed: {} ids", ids.len());
    let mut files = Vec::new();
    sources(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
    let missing: Vec<&String> = ids
        .iter()
        .filter(|id| {
            let needle = match id.split_once('{') {
                Some((prefix, _)) => format!("\"{prefix}"),
                None => format!("\"{id}\""),
            };
            !files.iter().any(|(_, text)| text.contains(&needle))
        })
        .collect();
    assert!(missing.is_empty(), "upstream ids no element carries: {missing:?}");
}
