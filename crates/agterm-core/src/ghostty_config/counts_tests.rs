//! The diagnostic counts libghostty gave, as goldens: the cases the GTK build's `config_parity` test checked
//! agterm-core against the linked libghostty (at upstream agterm v0.36.0's Ghostty, 683d8db), with libghostty's
//! counts and keybind messages as it reported them there. The parse code they exercise (`cli/args.zig`, `Binding.zig`,
//! `Config.zig`'s value types) is unchanged at `LIBGHOSTTY_VT_REV` but for `copy-on-select` and `middle-click-action`,
//! which none of those cases sets, so the goldens stand for the one Ghostty pin; the cases Ghostty 1.4's keys and
//! values add are at the end of [`LINES`], read from `Config.zig` at the pin. Since libghostty is no longer linked,
//! these hold the counts in place of the comparison.

use std::error::Error;
use std::path::Path;

use super::binds::{Action, action_names};
use super::{Loader, defaults_text};

type TestResult = Result<(), Box<dyn Error>>;

/// Config files of one case, `main.conf` loaded first; `{dir}` in a text stands for the case's directory.
type Files<'a> = &'a [(&'a str, &'a str)];

/// agterm-core's diagnostics for the files, laid out in a fresh directory, each as `line:key: message` (the path of
/// the case's directory left out).
fn messages_of(files: Files<'_>) -> Result<Vec<String>, Box<dyn Error>> {
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
    let prefix = format!("{}:", main.display());
    Ok(core_messages(&main)
        .into_iter()
        .map(|message| message.strip_prefix(&prefix).map_or(message.clone(), str::to_owned))
        .collect())
}

fn core_messages(main: &Path) -> Vec<String> {
    let mut loader = Loader::new(None);
    loader.load_file(main);
    loader.load_includes();
    loader
        .finish()
        .finalize(false, &[])
        .diagnostics
        .iter()
        .map(ToString::to_string)
        .collect()
}

fn count(files: Files<'_>) -> Result<usize, Box<dyn Error>> {
    Ok(messages_of(files)?.len())
}

/// One-file cases: the line, and libghostty's count for it.
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
    ("background = aé", 1),
    ("background = #aé", 1),
    ("palette = 1=éa", 1),
    ("background-opacity = 0x1.0p-2147483647", 0),
    ("background-opacity = 0x0p1024", 0),
    ("cursor-color = cell-foreground", 0),
    ("selection-background = cell-backgrounds", 1),
    ("selection-word-chars = \" \\t;\\x41\\u{2502}\"", 0),
    ("selection-word-chars = \"\\q\"", 1),
    ("selection-word-chars = \"\\x4\"", 1),
    ("click-repeat-interval = 0x3e8", 0),
    ("click-repeat-interval = 1_000", 0),
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
    (r#"theme = light:"\u{d800}",dark:x"#, 1),
    (r#"theme = light:"\q",dark:x"#, 1),
    (r"theme = light:a\qb,dark:x", 1),
    (r#"theme = light:"\x5",dark:x"#, 1),
    ("theme = sub/dir", 1),
    ("theme = {dir}/missing", 1),
    ("theme = {dir}", 1),
    ("\u{feff}font-size = 13\r\nno-such-key\r\n", 1),
    ("config-file = ?absent.conf", 0),
    ("config-file = absent.conf", 1),
    ("config-file =", 0),
    // read from `Config.zig` at `LIBGHOSTTY_VT_REV` (no libghostty linked to check them against): the keys and values
    // Ghostty 1.4 added, and the old `copy-on-select` values its compatibility handler still takes
    ("copy-on-select = primary", 0),
    ("copy-on-select = both", 0),
    ("copy-on-select = true", 0),
    ("copy-on-select = false", 0),
    ("copy-on-select = maybe", 1),
    ("middle-click-action = clipboard-paste", 0),
    ("vt-window-resize-allowed = true", 0),
    ("vt-xt-checksum-report", 0),
    ("vt-xt-checksum-extension = no-negate,no-attributes,no-trim,full", 0),
    ("vt-xt-checksum-extension = bogus", 1),
];

/// `keybind` lines (Task 8's port of `Binding.zig`): triggers, flags, sequences, tables, chains and parameters, with
/// libghostty's diagnostics for each, as `line:key: message`.
const KEYBIND_LINES: &[(&str, &[&str])] = &[
    ("keybind = ctrl+a=select_all", &[]),
    ("keybind = ctrl+shift+key_c=copy_to_clipboard", &[]),
    ("keybind = ctrl+KeyC=copy_to_clipboard:vt", &[]),
    ("keybind = ArrowUp=scroll_page_up", &[]),
    ("keybind = F12=toggle_quick_terminal", &[]),
    ("keybind = unconsumed:ctrl+a=text:x", &[]),
    ("keybind = performable:all:ctrl+a=reset", &[]),
    ("keybind = global:ctrl+a=quit", &[]),
    ("keybind = ctrl+a>b=new_window", &[]),
    (
        "keybind = ctrl+a>ctrl+b>c=new_window\nkeybind = ctrl+a>ctrl+b>c=unbind",
        &[],
    ),
    ("keybind = ctrl+==increase_font_size:1", &[]),
    ("keybind = ctrl++=increase_font_size:2.5", &[]),
    ("keybind = +=text:plus", &[]),
    (r"keybind = a=text:\q", &[]),
    ("keybind = a=text:=x", &[]),
    ("keybind = catch_all=ignore", &[]),
    ("keybind = physical:one=goto_tab:1", &[]),
    ("keybind = kp_enter=csi:A", &[]),
    ("keybind = grave_accent=esc:x", &[]),
    ("keybind = ctrl=ignore", &[]),
    ("keybind = unidentified+a=ignore", &[]),
    ("keybind = cmd+opt+control+a=ignore", &[]),
    ("keybind = caps_lock+num_lock+a=ignore", &[]),
    ("keybind = ф=ignore", &[]),
    ("keybind = ctrl+a=new_split", &[]),
    ("keybind = ctrl+a=goto_split:top", &[]),
    ("keybind = ctrl+a=resize_split:up,10", &[]),
    ("keybind = ctrl+a=write_screen_file:open,html", &[]),
    ("keybind = ctrl+a=write_scrollback_file:paste", &[]),
    ("keybind = ctrl+a=close_tab", &[]),
    ("keybind = ctrl+a=open_config:new_window", &[]),
    ("keybind = ctrl+a=jump_to_prompt:-1", &[]),
    ("keybind = ctrl+a=move_tab:+3", &[]),
    ("keybind = ctrl+a=scroll_page_lines:-0", &[]),
    ("keybind = ctrl+a=scroll_page_fractional:0.5", &[]),
    ("keybind = ctrl+a=set_font_size:1e3", &[]),
    ("keybind = ctrl+a=goto_tab:1_0", &[]),
    ("keybind = ctrl+a=search:", &[]),
    ("keybind = vim/", &[]),
    ("keybind = vim/j=scroll_page_lines:1", &[]),
    ("keybind = /=text:x", &[]),
    ("keybind = ctrl+/=text:x", &[]),
    ("keybind = a=text:x\nkeybind = chain=text:y", &[]),
    ("keybind = chain=text:y", &[]),
    ("keybind = vim/j=text:x\nkeybind = chain=text:y", &[]),
    ("keybind = ctrl+a>b=text:x\nkeybind = chain=text:y", &[]),
    ("keybind = clear", &[]),
    ("keybind =", &[]),
    ("keybind", &["1:keybind: value required"]),
    ("keybind = garbage", &["1:keybind: unknown error error.InvalidFormat"]),
    ("keybind = a=nope", &["1:keybind: unknown error error.InvalidAction"]),
    (
        "keybind = a=cursor_key",
        &["1:keybind: unknown error error.InvalidAction"],
    ),
    ("keybind = a=", &["1:keybind: unknown error error.InvalidFormat"]),
    ("keybind = =ignore", &["1:keybind: unknown error error.InvalidFormat"]),
    ("keybind = a>=ignore", &["1:keybind: unknown error error.InvalidFormat"]),
    ("keybind = >a=ignore", &["1:keybind: unknown error error.InvalidFormat"]),
    (
        "keybind = ctrl+ctrl+a=ignore",
        &["1:keybind: unknown error error.InvalidFormat"],
    ),
    (
        "keybind = control+ctrl+a=ignore",
        &["1:keybind: unknown error error.InvalidFormat"],
    ),
    (
        "keybind = a+b=ignore",
        &["1:keybind: unknown error error.InvalidFormat"],
    ),
    (
        "keybind = KeyA+KeyB=ignore",
        &["1:keybind: unknown error error.InvalidFormat"],
    ),
    (
        "keybind = foo=ignore",
        &["1:keybind: unknown error error.InvalidFormat"],
    ),
    (
        "keybind = all:all:a=ignore",
        &["1:keybind: unknown error error.InvalidFormat"],
    ),
    (
        "keybind = unconsumed:unconsumed:a=ignore",
        &["1:keybind: unknown error error.InvalidFormat"],
    ),
    (
        "keybind = global:a>b=ignore",
        &["1:keybind: unknown error error.InvalidFormat"],
    ),
    (
        "keybind = a=select_all:x",
        &["1:keybind: unknown error error.InvalidFormat"],
    ),
    ("keybind = a=text", &["1:keybind: unknown error error.InvalidFormat"]),
    (
        "keybind = a=increase_font_size",
        &["1:keybind: unknown error error.InvalidFormat"],
    ),
    (
        "keybind = a=increase_font_size:big",
        &["1:keybind: unknown error error.InvalidFormat"],
    ),
    (
        "keybind = a=goto_split:sideways",
        &["1:keybind: unknown error error.InvalidFormat"],
    ),
    (
        "keybind = a=resize_split:up",
        &["1:keybind: unknown error error.InvalidFormat"],
    ),
    (
        "keybind = a=resize_split:up,10,1",
        &["1:keybind: unknown error error.InvalidFormat"],
    ),
    (
        "keybind = a=resize_split:up,70000",
        &["1:keybind: unknown error error.InvalidFormat"],
    ),
    (
        "keybind = a=jump_to_prompt:40000",
        &["1:keybind: unknown error error.InvalidFormat"],
    ),
    (
        "keybind = a=goto_tab:-1",
        &["1:keybind: unknown error error.InvalidFormat"],
    ),
    (
        "keybind = a=goto_tab:0x10",
        &["1:keybind: unknown error error.InvalidFormat"],
    ),
    (
        "keybind = a=inspector",
        &["1:keybind: unknown error error.InvalidFormat"],
    ),
    (
        "keybind = a=write_screen_file",
        &["1:keybind: unknown error error.InvalidFormat"],
    ),
    (
        "keybind = a=write_screen_file:copy,pdf",
        &["1:keybind: unknown error error.InvalidFormat"],
    ),
    ("keybind = a=crash", &["1:keybind: unknown error error.InvalidFormat"]),
    (
        "keybind = vim/chain=text:x",
        &["1:keybind: unknown error error.InvalidFormat"],
    ),
    (
        "keybind = unconsumed:chain=text:x",
        &["1:keybind: unknown error error.InvalidFormat"],
    ),
    (
        "keybind = a=text:x\nkeybind = chain=unbind",
        &["2:keybind: unknown error error.InvalidFormat"],
    ),
    (
        "keybind = clear\nkeybind = chain=text:y",
        &["2:keybind: unknown error error.InvalidFormat"],
    ),
    (
        "keybind = a=unbind\nkeybind = chain=new_window",
        &["2:keybind: unknown error error.InvalidFormat"],
    ),
    (
        "keybind = a=text:x\nkeybind = b=bogus\nkeybind = chain=text:y",
        &["2:keybind: unknown error error.InvalidAction"],
    ),
    (
        "keybind = vim/bogus\nkeybind = chain=text:y",
        &["1:keybind: unknown error error.InvalidFormat"],
    ),
];

/// The actions whose bare name libghostty refused (a parameter they need, or a parameter with no default); every
/// other bare name, and every action with a fitting parameter, it took.
const BARE_REFUSED: &[&str] = &[
    "csi",
    "esc",
    "text",
    "increase_font_size",
    "decrease_font_size",
    "set_font_size",
    "search",
    "navigate_search",
    "scroll_to_row",
    "scroll_page_fractional",
    "scroll_page_lines",
    "adjust_selection",
    "jump_to_prompt",
    "write_scrollback_file",
    "write_screen_file",
    "write_selection_file",
    "goto_tab",
    "move_tab",
    "set_surface_title",
    "set_tab_title",
    "set_window_title",
    "goto_split",
    "goto_window",
    "resize_split",
    "inspector",
    "activate_key_table",
    "activate_key_table_once",
    "crash",
];

#[test]
fn single_files_count_as_libghostty_counted_them() -> TestResult {
    let mut mismatches = Vec::new();
    for (text, expected) in LINES {
        let text = format!("{text}\n");
        let core = count(&[("main.conf", &text)])?;
        if core != *expected {
            mismatches.push(format!("{text:?}: libghostty {expected}, agterm-core {core}"));
        }
    }
    assert!(mismatches.is_empty(), "{mismatches:#?}");
    Ok(())
}

#[test]
fn keybind_lines_report_as_libghostty_reported_them() -> TestResult {
    let mut mismatches = Vec::new();
    for (text, expected) in KEYBIND_LINES {
        let core = messages_of(&[("main.conf", &format!("{text}\n"))])?;
        if core != *expected {
            mismatches.push(format!("{text:?}: libghostty {expected:?}, agterm-core {core:?}"));
        }
    }
    assert!(mismatches.is_empty(), "{mismatches:#?}");
    Ok(())
}

#[test]
fn every_action_parses_as_libghostty_parsed_it() -> TestResult {
    let mut mismatches = Vec::new();
    for name in action_names() {
        // the bare name, then a parameter that fits
        let fitting = [":x", ":1", ":up,1", ":copy", ":main", ":toggle", ":next", ":left"]
            .iter()
            .map(|param| format!("{name}{param}"))
            .find(|action| action.parse::<Action>().is_ok());
        if fitting.is_none() && name.parse::<Action>().is_err() {
            mismatches.push(format!("{name}: no parameter tried parses"));
        }
        let bare = messages_of(&[("main.conf", &format!("keybind = ctrl+a={name}\n"))])?;
        let expected: &[&str] = if BARE_REFUSED.contains(&name) {
            &["1:keybind: unknown error error.InvalidFormat"]
        } else {
            &[]
        };
        if bare != expected {
            mismatches.push(format!("{name}: libghostty {expected:?}, agterm-core {bare:?}"));
        }
        if let Some(action) = fitting {
            let core = messages_of(&[("main.conf", &format!("keybind = ctrl+a={action}\n"))])?;
            if !core.is_empty() {
                mismatches.push(format!("{action}: libghostty [], agterm-core {core:?}"));
            }
        }
    }
    assert!(mismatches.is_empty(), "{mismatches:#?}");
    Ok(())
}

#[test]
fn agterms_defaults_load_clean() -> TestResult {
    for terminfo in [true, false] {
        let text = defaults_text(terminfo);
        assert_eq!(count(&[("main.conf", &text)])?, 0, "terminfo resolved: {terminfo}");
    }
    Ok(())
}

#[test]
fn a_line_too_long_for_libghostty_ends_the_file() -> TestResult {
    let text = format!("font-size = 13\n{}\nno-such-key = 1\n", "x".repeat(5000));
    assert_eq!(count(&[("main.conf", &text)])?, 0);
    Ok(())
}

#[test]
fn includes_count_as_libghostty_counted_them() -> TestResult {
    let cases: [(Files<'_>, usize); 5] = [
        (
            &[
                ("main.conf", "config-file = sub/a.conf\nconfig-file = ?gone.conf\n"),
                ("sub/a.conf", "config-file = b.conf\nno-such-key = 1\n"),
                ("sub/b.conf", "cursor-style = nope\n"),
            ],
            2,
        ),
        (
            &[
                ("main.conf", "config-file = b.conf\n"),
                ("b.conf", "config-file = main.conf\n"),
            ],
            1,
        ),
        (&[("main.conf", "config-file = d\n"), ("d/", "")], 1),
        (
            &[
                (
                    "main.conf",
                    "config-file = gone.conf\nconfig-file =\nconfig-file = ?kept.conf\n",
                ),
                ("kept.conf", "x\n"),
            ],
            1,
        ),
        (
            &[
                ("main.conf", "config-file = {dir}/abs.conf\n"),
                ("abs.conf", "palette = 999=red\n"),
            ],
            1,
        ),
    ];
    for (files, expected) in cases {
        assert_eq!(count(files)?, expected, "{files:?}");
    }
    Ok(())
}

#[test]
fn themes_count_as_libghostty_counted_them() -> TestResult {
    let cases: [(Files<'_>, usize); 7] = [
        (
            &[
                ("main.conf", "theme = {dir}/t\n"),
                ("t", "background = nope\nwho = 1\nforeground = red\n"),
            ],
            2,
        ),
        // quoted sides decode Zig escapes: found, the theme's two bad lines count
        (
            &[
                ("main.conf", "theme = light:\"{dir}/\\x74\",dark:x\n"),
                ("t", "who = 1\nwhat = 2\n"),
            ],
            2,
        ),
        (
            &[
                ("main.conf", "theme = light:\"{dir}/\\u{74}\",dark:x\n"),
                ("t", "who = 1\nwhat = 2\n"),
            ],
            2,
        ),
        (
            &[
                ("main.conf", "theme = light:\"{dir}/\\xff\",dark:{dir}/t\n"),
                ("t", "who = 1\nwhat = 2\n"),
            ],
            1,
        ),
        // loadTheme skips no BOM: the first key carries it
        (
            &[
                ("main.conf", "theme = {dir}/t\n"),
                ("t", "\u{feff}background = red\nforeground = red\n"),
            ],
            1,
        ),
        (
            &[
                ("main.conf", "theme = light:{dir}/t,dark:{dir}/gone\n"),
                ("t", "palette = 1=red\n"),
            ],
            0,
        ),
        (
            &[
                ("main.conf", "theme = light:{dir}/gone,dark:{dir}/t\n"),
                ("t", "palette = 1=red\n"),
            ],
            1,
        ),
    ];
    for (files, expected) in cases {
        assert_eq!(count(files)?, expected, "{files:?}");
    }
    Ok(())
}

#[test]
fn residual_differences_are_the_documented_ones() -> TestResult {
    // malformed values of keys whose parse agterm does not port: libghostty counted them once, agterm-core does not
    for line in [
        "adjust-cell-height = tall",
        "mouse-scroll-multiplier = lots",
        "font-variation = wght",
    ] {
        assert_eq!(count(&[("main.conf", &format!("{line}\n"))])?, 0, "{line}");
    }
    Ok(())
}
