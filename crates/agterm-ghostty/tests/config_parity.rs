//! agterm-core's ghostty config parser against the linked libghostty (`GHOSTTY_REV`): the same config files must give
//! the same diagnostic count, the wire contract of `config.reload`. The residual differences the parser documents are
//! pinned here too, so closing one (or opening a new one) shows up as a failure.

use std::error::Error;
use std::path::Path;

use agterm_core::ghostty_config::{Loader, defaults_text};
use agterm_ghostty::Config;

type TestResult = Result<(), Box<dyn Error>>;

/// Config files of one case, `main.conf` loaded first; `{dir}` in a text stands for the case's directory.
type Files<'a> = &'a [(&'a str, &'a str)];

/// libghostty's count and agterm-core's for the files, laid out in a fresh directory.
fn counts(files: Files<'_>) -> Result<(usize, usize), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().canonicalize()?;
    for (name, text) in files {
        let path = root.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if name.ends_with('/') {
            std::fs::create_dir_all(&path)?;
        } else {
            std::fs::write(&path, text.replace("{dir}", &root.to_string_lossy()))?;
        }
    }
    let main = root.join("main.conf");
    Ok((ghostty_count(&main)?, core_count(&main)))
}

fn ghostty_count(main: &Path) -> Result<usize, Box<dyn Error>> {
    let mut config = Config::new()?;
    config.load_file(main);
    config.load_recursive_files();
    config.finalize();
    Ok(config.diagnostics().len())
}

fn core_count(main: &Path) -> usize {
    let mut loader = Loader::new(None);
    loader.load_file(main);
    loader.load_includes();
    loader.finish().diagnostic_count(&[])
}

/// One-file cases: the line, and libghostty's count for it (checked too, so a case cannot pass vacuously).
const LINES: &[(&str, usize)] = &[
    ("", 0),
    ("font-size = 17\nno-such-key = 1", 1),
    ("\"quoted key\" = 1", 1),
    ("macos-titlebar-style = nope", 1),
    ("macos-titlebar-style = tabs", 0),
    ("cursor-style = blcok", 1),
    ("cursor-style", 1),
    ("auto-update = sometimes", 1),
    ("mouse-hide-while-typing = yes", 1),
    ("mouse-hide-while-typing = T", 0),
    ("mouse-hide-while-typing", 0),
    ("abnormal-command-exit-runtime = 0x10", 0),
    ("abnormal-command-exit-runtime = 1_000", 0),
    ("abnormal-command-exit-runtime = -1", 1),
    ("abnormal-command-exit-runtime = 4294967296", 1),
    ("window-position-x = -32768", 0),
    ("background-opacity = 0.5", 0),
    ("background-opacity = 1_0.5", 0),
    ("background-opacity = 0x1p-1", 0),
    ("background-opacity = .5", 0),
    ("background-opacity = 5.", 0),
    ("background-opacity = inf", 0),
    ("background-opacity = abc", 1),
    ("background-opacity = 1__0", 1),
    ("background-opacity = 1e", 1),
    ("background-opacity = .", 1),
    ("background = #123", 0),
    ("background = #111222333", 0),
    ("background = Dark Slate Gray", 0),
    ("background = DARKSLATEGRAY", 0),
    ("background = a1b2c3", 0),
    ("background = rgb:f/ff/fff", 0),
    ("background = rgbi:0.5/1/0", 0),
    ("background = #12", 1),
    ("background = rgb:1/2", 1),
    ("background = rgbi:2/0/0", 1),
    ("background = notacolor", 1),
    ("cursor-color = cell-foreground", 0),
    ("selection-background = cell-backgrounds", 1),
    ("bold-color = bright", 0),
    ("bold-color = dull", 1),
    ("notify-on-command-finish-after = 1h 30m", 0),
    ("notify-on-command-finish-after = 0", 0),
    ("notify-on-command-finish-after = 1m1ms", 0),
    ("notify-on-command-finish-after = 5µs", 0),
    ("notify-on-command-finish-after = 5", 1),
    ("notify-on-command-finish-after = 10x", 1),
    ("window-padding-x = 4,8", 0),
    ("window-padding-x = 4,", 1),
    ("window-padding-x = -1", 1),
    ("window-decoration", 0),
    ("window-decoration = false", 0),
    ("window-decoration = maybe", 1),
    ("palette = 0xff=red", 0),
    ("palette = 256=#000000", 1),
    ("palette = 3", 1),
    ("scrollback-limit-bytes = unlimited", 0),
    ("scrollback-limit-bytes = lots", 1),
    ("scrollback-limit = 1000", 0),
    ("scrollback-limit = lots", 1),
    ("shell-integration-features = no-cursor,sudo", 0),
    ("shell-integration-features = true", 0),
    ("shell-integration-features = cursor,", 1),
    ("shell-integration-features = bogus", 1),
    ("link = x", 1),
    ("gtk-tabs-location = hidden", 0),
    ("gtk-tabs-location = left", 1),
    ("cursor-invert-fg-bg = true", 0),
    ("cursor-invert-fg-bg = maybe", 1),
    ("bold-is-bright", 0),
    ("term =", 0),
    ("font-family = Iosevka\nfont-family =", 0),
    ("title", 1),
    ("keybind", 1),
    ("key-remap", 0),
    ("theme = light:Only", 1),
    ("theme = sub/dir", 1),
    ("theme = {dir}/missing", 1),
    ("theme = {dir}", 1),
    ("\u{feff}font-size = 13\r\nno-such-key\r\n", 1),
    ("config-file = ?absent.conf", 0),
    ("config-file = absent.conf", 1),
    ("config-file =", 0),
];

#[test]
fn single_files_count_as_libghostty_counts_them() -> TestResult {
    let mut mismatches = Vec::new();
    for (text, expected) in LINES {
        let text = format!("{text}\n");
        let (ghostty, core) = counts(&[("main.conf", &text)])?;
        if ghostty != *expected || core != ghostty {
            mismatches.push(format!(
                "{text:?}: libghostty {ghostty} (expected {expected}), agterm-core {core}"
            ));
        }
    }
    assert!(mismatches.is_empty(), "{mismatches:#?}");
    Ok(())
}

#[test]
fn agterms_defaults_load_clean_in_both() -> TestResult {
    for terminfo in [true, false] {
        let text = defaults_text(terminfo);
        assert_eq!(
            counts(&[("main.conf", &text)])?,
            (0, 0),
            "terminfo resolved: {terminfo}"
        );
    }
    Ok(())
}

#[test]
fn a_line_too_long_for_libghostty_ends_the_file() -> TestResult {
    let text = format!("font-size = 13\n{}\nno-such-key = 1\n", "x".repeat(5000));
    assert_eq!(counts(&[("main.conf", &text)])?, (0, 0));
    Ok(())
}

#[test]
fn includes_count_as_libghostty_counts_them() -> TestResult {
    let cases: [Files<'_>; 5] = [
        &[
            ("main.conf", "config-file = sub/a.conf\nconfig-file = ?gone.conf\n"),
            ("sub/a.conf", "config-file = b.conf\nno-such-key = 1\n"),
            ("sub/b.conf", "cursor-style = nope\n"),
        ],
        &[
            ("main.conf", "config-file = b.conf\n"),
            ("b.conf", "config-file = main.conf\n"),
        ],
        &[("main.conf", "config-file = d\n"), ("d/", "")],
        &[
            (
                "main.conf",
                "config-file = gone.conf\nconfig-file =\nconfig-file = ?kept.conf\n",
            ),
            ("kept.conf", "x\n"),
        ],
        &[
            ("main.conf", "config-file = {dir}/abs.conf\n"),
            ("abs.conf", "palette = 999=red\n"),
        ],
    ];
    for files in cases {
        let (ghostty, core) = counts(files)?;
        assert_eq!(core, ghostty, "{files:?}");
    }
    Ok(())
}

#[test]
fn themes_count_as_libghostty_counts_them() -> TestResult {
    let cases: [Files<'_>; 3] = [
        &[
            ("main.conf", "theme = {dir}/t\n"),
            ("t", "background = nope\nwho = 1\nforeground = red\n"),
        ],
        &[
            ("main.conf", "theme = light:{dir}/t,dark:{dir}/gone\n"),
            ("t", "palette = 1=red\n"),
        ],
        &[
            ("main.conf", "theme = light:{dir}/gone,dark:{dir}/t\n"),
            ("t", "palette = 1=red\n"),
        ],
    ];
    for files in cases {
        let (ghostty, core) = counts(files)?;
        assert_eq!(core, ghostty, "{files:?}");
    }
    Ok(())
}

#[test]
fn residual_differences_are_the_documented_ones() -> TestResult {
    // malformed values of keys whose parse agterm does not port: libghostty counts them, agterm-core does not
    for line in [
        "keybind = garbage",
        "adjust-cell-height = tall",
        "mouse-scroll-multiplier = lots",
        "font-variation = wght",
    ] {
        let (ghostty, core) = counts(&[("main.conf", &format!("{line}\n"))])?;
        assert_eq!((ghostty, core), (1, 0), "{line}");
    }
    Ok(())
}
