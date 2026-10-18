//! `gen-unicode` end to end, on a small UCD of its own: it writes the four tables, `--check` passes on what it
//! wrote and fails on a changed or missing table, and a UCD of another version is refused. The real tables are held
//! to the pinned UCD by `scripts/gen-vt-unicode.sh --check`, which CI runs on every OS.

use std::fs;
use std::io;
use std::path::Path;
use std::process::{Command, Output};

const TABLES: [&str; 4] = [
    "case_folding_data.rs",
    "grapheme_data.rs",
    "props_data.rs",
    "symbols_data.rs",
];

/// A uucode tree holding a few code points of each file the generator reads, in the files' own formats.
fn fixture(dir: &Path, version: &str) -> io::Result<()> {
    let files: [(&str, String); 10] = [
        (
            "build.zig.zon",
            ".{\n    .name = .uucode,\n    .version = \"0.2.0\",\n}\n".to_owned(),
        ),
        (
            "ucd/UnicodeData.txt",
            "0020;SPACE;Zs;0;WS;;;;;N;;;;;\n0041;LATIN CAPITAL LETTER A;Lu;0;L;;;;;N;;;;0061;\n\
             0308;COMBINING DIAERESIS;Mn;230;NSM;;;;;N;;;;;\n\
             4E00;<CJK Ideograph, First>;Lo;0;L;;;;;N;;;;;\n9FFF;<CJK Ideograph, Last>;Lo;0;L;;;;;N;;;;;\n\
             E000;<Private Use, First>;Co;0;L;;;;;N;;;;;\nF8FF;<Private Use, Last>;Co;0;L;;;;;N;;;;;\n"
                .to_owned(),
        ),
        (
            "ucd/CaseFolding.txt",
            format!("# CaseFolding-{version}.txt\n0041; C; 0061; # A\n00DF; F; 0073 0073; # SHARP S\n"),
        ),
        (
            "ucd/DerivedCoreProperties.txt",
            format!("# DerivedCoreProperties-{version}.txt\n0308 ; InCB; Extend # Mn DIAERESIS\n"),
        ),
        (
            "ucd/extracted/DerivedEastAsianWidth.txt",
            format!("# DerivedEastAsianWidth-{version}.txt\n# @missing: 0000..10FFFF; Neutral\n4E00..9FFF ; W\n"),
        ),
        (
            "ucd/auxiliary/GraphemeBreakProperty.txt",
            format!("# GraphemeBreakProperty-{version}.txt\n0308 ; Extend # Mn DIAERESIS\n"),
        ),
        (
            "ucd/auxiliary/GraphemeBreakTest.txt",
            format!("# GraphemeBreakTest-{version}.txt\n÷ 0020 ÷ 0020 ÷\n÷ 0041 × 0308 ÷\n"),
        ),
        (
            "ucd/emoji/emoji-data.txt",
            format!("# emoji-data.txt\n# Version: {version}\n0023 ; Emoji # NUMBER SIGN\n"),
        ),
        (
            "ucd/emoji/emoji-variation-sequences.txt",
            format!("# emoji-variation-sequences.txt\n# Version: {version}\n0023 FE0E ; text style;\n"),
        ),
        (
            "ucd/Blocks.txt",
            format!("# Blocks-{version}.txt\n2190..21FF; Arrows\n"),
        ),
    ];
    for (name, text) in files {
        let path = dir.join(name);
        fs::create_dir_all(path.parent().unwrap_or(dir))?;
        fs::write(path, text)?;
    }
    Ok(())
}

fn gen_unicode(uucode: &Path, out: &Path, check: bool) -> io::Result<Output> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_gen-unicode"));
    command.args([
        "--uucode-rev",
        "9d55524551411b493cca41ca06363625d90aff1e",
        "--ucd-version",
        "18.0.0",
    ]);
    command.arg("--uucode").arg(uucode).arg("--out").arg(out);
    if check {
        command.arg("--check");
    }
    command.output()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn check_passes_on_what_it_wrote_and_fails_on_a_changed_table() {
    let dir = tempfile::tempdir().unwrap();
    let (uucode, out) = (dir.path().join("uucode"), dir.path().join("out"));
    fixture(&uucode, "18.0.0").unwrap();
    fs::create_dir(&out).unwrap();

    let written = gen_unicode(&uucode, &out, false).unwrap();
    assert!(written.status.success(), "{}", stderr(&written));
    assert!(
        stderr(&written).contains("GraphemeBreakTest.txt: 2 pairs agree"),
        "{}",
        stderr(&written)
    );
    for table in TABLES {
        let text = fs::read_to_string(out.join(table)).unwrap();
        assert!(text.contains("uucode 0.2.0 @ `9d555245`"), "{table}");
    }
    let props = fs::read_to_string(out.join("props_data.rs")).unwrap();
    let folding = fs::read_to_string(out.join("case_folding_data.rs")).unwrap();
    assert!(folding.contains("(0x00DF, [0x0073, 0x0073, 0x0000]),"), "{folding}");

    let check = gen_unicode(&uucode, &out, true).unwrap();
    assert!(check.status.success(), "{}", stderr(&check));
    assert!(stderr(&check).contains("is current"));

    // one entry of one table changed: --check names the file and fails, and writes nothing
    let changed = props.replacen("width: 1,", "width: 2,", 1);
    assert_ne!(changed, props);
    fs::write(out.join("props_data.rs"), &changed).unwrap();
    let check = gen_unicode(&uucode, &out, true).unwrap();
    assert_eq!(check.status.code(), Some(1), "{}", stderr(&check));
    assert!(stderr(&check).contains("stale in"), "{}", stderr(&check));
    assert!(stderr(&check).ends_with(": props_data.rs\n"), "{}", stderr(&check));
    assert_eq!(fs::read_to_string(out.join("props_data.rs")).unwrap(), changed);

    // a missing table is stale too
    fs::remove_file(out.join("grapheme_data.rs")).unwrap();
    let check = gen_unicode(&uucode, &out, true).unwrap();
    assert_eq!(check.status.code(), Some(1));
    assert!(
        stderr(&check).ends_with(": grapheme_data.rs, props_data.rs\n"),
        "{}",
        stderr(&check)
    );
}

#[test]
fn a_ucd_of_another_version_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (uucode, out) = (dir.path().join("uucode"), dir.path().join("out"));
    fixture(&uucode, "17.0.0").unwrap();
    fs::create_dir(&out).unwrap();
    let run = gen_unicode(&uucode, &out, true).unwrap();
    assert_eq!(run.status.code(), Some(2));
    assert!(
        stderr(&run).contains("UCD 17.0.0, but the pin is UCD 18.0.0"),
        "{}",
        stderr(&run)
    );
}
