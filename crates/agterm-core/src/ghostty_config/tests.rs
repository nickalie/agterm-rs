//! The ghostty config parser against ghostty's own rules at `GHOSTTY_REV`; `agterm-ghostty`'s `config_parity` test
//! checks the same counts against the linked libghostty.

use std::path::{Path, PathBuf};

use strum::IntoEnumIterator;

use super::validate::{self, Accepted, Compat, Kind, ValueError, check, check_kind};
use super::*;
use crate::chrome::Rgb;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The config of `text` loaded alone, light side, no theme directories.
fn load(text: &str) -> FinalConfig {
    let mut loader = Loader::new(None);
    loader.load_text("test.conf", text);
    loader.finish().finalize(false, &[])
}

fn count(text: &str) -> usize {
    load(text).diagnostics.len()
}

fn kind(name: &str) -> Kind {
    validate::key_spec(name).map(|spec| spec.kind).expect("a known key")
}

fn write(path: &Path, text: &str) -> std::io::Result<PathBuf> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text)?;
    Ok(path.to_path_buf())
}

// -- reading lines

#[test]
fn lines_are_trimmed_comments_and_blanks_skipped() {
    let text = "a = 1\n\n# comment\n  # indented comment\n  b=2  \r\nc\nd = \"quoted \"\ne =\n";
    let read = lines(text);
    assert_eq!(
        read,
        [
            (1, "a".to_owned(), Some("1".to_owned())),
            (5, "b".to_owned(), Some("2".to_owned())),
            (6, "c".to_owned(), None),
            (7, "d".to_owned(), Some("quoted ".to_owned())),
            (8, "e".to_owned(), Some(String::new())),
        ]
    );
}

#[test]
fn non_ascii_colors_are_refused_not_sliced() {
    for bad in ["aé", "éa", "#aé", "#é0", "1=éa"] {
        assert_eq!(validate::parse_color(bad), Err(ValueError::Invalid), "{bad}");
    }
    assert_eq!(
        count("background = aé\ncursor-color = #é0\nbold-color = éa\npalette = 1=éa\n"),
        4
    );
}

#[test]
fn hex_floats_stay_finite_and_exact_at_the_extremes() {
    assert_eq!(validate::parse_float("0x1.0p-2147483647"), Ok(0.0));
    assert_eq!(validate::parse_float("0x1p-3000000000"), Ok(0.0));
    assert_eq!(validate::parse_float("0x1p3000000000"), Ok(f64::INFINITY));
    assert_eq!(validate::parse_float("0x0p1024"), Ok(0.0));
    assert_eq!(
        validate::parse_float("0x1p-1074"),
        Ok(f64::from_bits(1)),
        "the smallest subnormal"
    );
    let wide = format!("0xe{}p-1200", "0".repeat(300));
    assert_eq!(validate::parse_float(&wide), Ok(14.0));
    assert_eq!(validate::parse_float("0x0.00fp12"), Ok(15.0));
}

#[test]
fn a_bom_is_skipped_in_config_files_and_kept_by_the_bare_line_reader() {
    assert_eq!(count("\u{feff}font-size = 13\n"), 0, "loadReader skips it");
    assert_eq!(
        lines("\u{feff}a = 1\n")[0].1,
        "\u{feff}a",
        "LineIterator alone keeps it in the key"
    );
}

#[test]
fn a_line_longer_than_libghostty_reads_ends_the_file() {
    let text = format!("font-size = 14\n{}\nno-such-key = 1\n", "x".repeat(4095));
    let built = load(&text);
    assert_eq!(built.diagnostics, [], "nothing after the long line is read");
    assert!((built.config.font.size - 14.0).abs() < f32::EPSILON);
    let fits = format!("{}\n", "y".repeat(4094));
    assert_eq!(count(&fits), 1, "a 4094-byte line is still a line: an unknown key");
}

#[test]
fn diagnostics_display_as_ghostty_formats_them() {
    let built = load("font-size = 12\nno-such-key = 1\n");
    let [diagnostic] = built.diagnostics.as_slice() else {
        panic!("{:?}", built.diagnostics)
    };
    assert_eq!(diagnostic.to_string(), "test.conf:2:no-such-key: unknown field");
    let file_level = Diagnostic {
        location: None,
        key: String::new(),
        message: "config-file /x: cycle detected".to_owned(),
    };
    assert_eq!(file_level.to_string(), "config-file /x: cycle detected");
}

// -- values by kind

#[test]
fn unknown_keys_and_malformed_values_count_valid_unsupported_ones_do_not() {
    assert_eq!(count("no-such-key = 1\n"), 1);
    assert_eq!(
        count("macos-titlebar-style = nope\n"),
        1,
        "malformed value of a key agterm ignores"
    );
    assert_eq!(
        count("macos-titlebar-style = tabs\n"),
        0,
        "valid value of a key agterm ignores"
    );
    assert_eq!(count("font-size = 15\nno-such-key = 1\n"), 1);
    assert_eq!(count("cursor-style = blcok\nbackground = nope\nfoo\n"), 3);
}

#[test]
fn messages_follow_ghostty() {
    assert_eq!(check("no-such-key", Some("1")), Err("unknown field".to_owned()));
    assert_eq!(
        check("cursor-style", Some("x")),
        Err("invalid value \"x\", valid values are: bar, block, underline, block_hollow".to_owned())
    );
    assert_eq!(
        check("cursor-style", None),
        Err("value required, valid values are: bar, block, underline, block_hollow".to_owned())
    );
    // an optional enum lists no values
    assert_eq!(check("auto-update", Some("x")), Err("invalid value \"x\"".to_owned()));
    assert_eq!(check("font-size", None), Err("value required".to_owned()));
    assert_eq!(
        check("palette", Some("256=red")),
        Err("unknown error error.Overflow".to_owned())
    );
    assert_eq!(
        check("link", Some("x")),
        Err("unknown error error.NotImplemented".to_owned())
    );
}

#[test]
fn an_empty_value_resets_any_known_key() {
    for spec in known_keys() {
        assert!(check_kind(spec.kind, Some("")).is_ok(), "{}", spec.name);
    }
    assert_eq!(count("no-such-key =\n"), 1, "an unknown key stays unknown");
}

#[test]
fn bools_take_zig_spellings_and_a_bare_key() {
    for good in ["1", "t", "T", "true", "0", "f", "F", "false"] {
        assert_eq!(check_kind(Kind::Bool, Some(good)), Ok(()), "{good}");
    }
    for bad in ["yes", "TRUE", "on", "2"] {
        assert_eq!(check_kind(Kind::Bool, Some(bad)), Err(ValueError::Invalid), "{bad}");
    }
    assert_eq!(check_kind(Kind::Bool, None), Ok(()));
    assert!(load("mouse-hide-while-typing\n").config.mouse.hide_while_typing);
}

#[test]
fn integers_take_zig_bases_separators_and_ranges() {
    let u32_kind = kind("abnormal-command-exit-runtime");
    for good in [
        "0",
        "250",
        "+7",
        "-0",
        "0x10",
        "0o17",
        "0b101",
        "1_000",
        "4294967295",
        "0XfF",
    ] {
        assert_eq!(check_kind(u32_kind, Some(good)), Ok(()), "{good}");
    }
    for bad in ["-1", "4294967296", "_1", "1_", "0x", "0x_1", "1.0", "ten", "-", "+"] {
        assert_eq!(check_kind(u32_kind, Some(bad)), Err(ValueError::Invalid), "{bad}");
    }
    let i16_kind = kind("window-position-x");
    assert_eq!(check_kind(i16_kind, Some("-32768")), Ok(()));
    assert_eq!(check_kind(i16_kind, Some("32768")), Err(ValueError::Invalid));
    assert_eq!(validate::parse_int("0x1F", 0, false, 8), Ok(31));
    assert_eq!(
        validate::parse_int("256", 0, false, 8),
        Err(validate::IntError::Overflow)
    );
}

#[test]
fn floats_take_zig_syntax() {
    for good in [
        "0.5",
        "1",
        "5.",
        ".5",
        "+1.5",
        "-2",
        "1e3",
        "1E-3",
        "1_0.5",
        "0x1p-1",
        "0x1.8",
        "inf",
        "-Infinity",
        "NaN",
    ] {
        assert!(validate::parse_float(good).is_ok(), "{good}");
    }
    for bad in [
        "", "abc", ".", "1e", "1__0", "_1", "1_", "1._5", "0x", "infx", "1.5.5", "- 1", "0xg",
    ] {
        assert!(validate::parse_float(bad).is_err(), "{bad}");
    }
    assert_eq!(validate::parse_float("1_0.5"), Ok(10.5));
    assert_eq!(validate::parse_float("0x1p-1"), Ok(0.5));
    assert_eq!(validate::parse_float("0x1.8"), Ok(1.5));
    assert_eq!(validate::parse_float(".25e1"), Ok(2.5));
}

#[test]
fn colors_take_hex_names_and_rgb_specs() {
    let cases = [
        ("#123", Rgb::new(0x11, 0x22, 0x33)),
        ("#0A0B0C", Rgb::new(10, 11, 12)),
        ("0A0B0C", Rgb::new(10, 11, 12)),
        ("FFF", Rgb::WHITE),
        ("#fff000fff", Rgb::new(255, 0, 255)),
        ("#ffff00000000", Rgb::new(255, 0, 0)),
        ("  black ", Rgb::BLACK),
        ("Dark Slate Gray", Rgb::new(47, 79, 79)),
        ("DARKSLATEGRAY", Rgb::new(47, 79, 79)),
        ("rgb:f/ff/fff", Rgb::WHITE),
        ("rgbi:1/0/0.5", Rgb::new(255, 0, 127)),
    ];
    for (value, rgb) in cases {
        assert_eq!(validate::parse_color(value), Ok(rgb), "{value}");
    }
    for bad in [
        "",
        "#12",
        "#ggg",
        "notacolor",
        "rgb:1/2",
        "rgbi:2/0/0",
        "rgb:/1/1",
        "rgb:12345/1/1",
        "1234",
    ] {
        assert_eq!(validate::parse_color(bad), Err(ValueError::Invalid), "{bad}");
    }
    assert_eq!(
        validate::parse_terminal_color("cell-foreground"),
        Ok(TerminalColor::CellForeground)
    );
    assert_eq!(check_kind(Kind::BoldColor, Some("bright")), Ok(()));
    assert_eq!(check_kind(Kind::BoldColor, Some("dull")), Err(ValueError::Invalid));
}

#[test]
fn durations_take_units_and_a_bare_zero() {
    let second = 1_000_000_000;
    assert_eq!(validate::parse_duration("5s"), Ok(5 * second));
    assert_eq!(validate::parse_duration("1h 30m"), Ok(5400 * second));
    assert_eq!(
        validate::parse_duration("10ms"),
        Ok(10_000_000),
        "the longest unit wins"
    );
    assert_eq!(validate::parse_duration("1m1ms"), Ok(60 * second + 1_000_000));
    assert_eq!(validate::parse_duration("5µs"), Ok(5_000));
    assert_eq!(validate::parse_duration("0"), Ok(0));
    assert_eq!(validate::parse_duration("5"), Err(ValueError::Invalid));
    assert_eq!(validate::parse_duration("5x"), Err(ValueError::Invalid));
    assert_eq!(validate::parse_duration("s"), Err(ValueError::Invalid));
    assert_eq!(validate::parse_duration("  "), Err(ValueError::Required));
}

#[test]
fn paddings_palettes_limits_and_flags() {
    assert_eq!(validate::parse_padding("4"), Ok(Padding { start: 4, end: 4 }));
    assert_eq!(validate::parse_padding(" 4 , 8 "), Ok(Padding { start: 4, end: 8 }));
    for bad in ["-1", "4,", "x", "0x4"] {
        assert_eq!(validate::parse_padding(bad), Err(ValueError::Invalid), "{bad}");
    }
    assert_eq!(
        validate::parse_palette_entry("0xF = red"),
        Ok((15, Rgb::new(255, 0, 0)))
    );
    assert_eq!(validate::parse_palette_entry("3"), Err(ValueError::Invalid));
    assert_eq!(validate::parse_limit("unlimited"), Ok(None));
    assert_eq!(validate::parse_limit("0x10"), Ok(Some(16)));
    assert_eq!(validate::parse_limit("lots"), Err(ValueError::Invalid));
    let features = kind("shell-integration-features");
    for good in ["no-cursor,no-title", " sudo , path ", "true", "0"] {
        assert_eq!(check_kind(features, Some(good)), Ok(()), "{good}");
    }
    for bad in ["bogus", "cursor,", "no-", "True"] {
        assert_eq!(check_kind(features, Some(bad)), Err(ValueError::Invalid), "{bad}");
    }
}

#[test]
fn themes_take_one_name_or_both_sides() {
    let both = |light: &str, dark: &str| ThemeName {
        light: light.to_owned(),
        dark: dark.to_owned(),
    };
    assert_eq!(validate::parse_theme("  Dracula "), Ok(both("Dracula", "Dracula")));
    assert_eq!(validate::parse_theme(" light:A,  dark : B "), Ok(both("A", "B")));
    assert_eq!(validate::parse_theme("dark:B,light:A,"), Ok(both("A", "B")));
    assert_eq!(validate::parse_theme("light:\"A, B\",dark:C"), Ok(both("A, B", "C")));
    for bad in [
        "light:A",
        "dark:B",
        "light:A,dark:B,other:C",
        "A,B",
        "light=A,dark=B",
        ",light:A,dark:B",
    ] {
        assert!(validate::parse_theme(bad).is_err(), "{bad}");
    }
    assert!(validate::parse_theme("light:\"A,dark:B").is_err(), "an unclosed quote");
}

#[test]
fn quoted_theme_sides_decode_zig_string_escapes() {
    let pair = |value: &str| validate::parse_theme(value).map(|theme| (theme.light, theme.dark));
    let both = |light: &str, dark: &str| Ok((light.to_owned(), dark.to_owned()));
    assert_eq!(pair(r#"light:"\x50aper",dark:Ink"#), both("Paper", "Ink"));
    assert_eq!(
        pair(r#"light:"\u{1F600}x",dark:"\u{0000041}""#),
        both("\u{1F600}x", "A")
    );
    assert_eq!(pair(r#"light:"\t\"\\",dark:y"#), both("\t\"\\", "y"));
    assert_eq!(
        pair(r#"light:"\xff",dark:y"#),
        both("\u{fffd}", "y"),
        "invalid UTF-8 is replaced"
    );
    assert_eq!(
        pair(r"light:a\x41,dark:b"),
        both(r"a\x41", "b"),
        "an unquoted side is not decoded"
    );
    for bad in [
        r#"light:"\u{d800}",dark:y"#,
        r#"light:"\u{110000}",dark:y"#,
        r#"light:"\u{}",dark:y"#,
        r#"light:"\x5",dark:y"#,
        r#"light:"\q",dark:y"#,
        r"light:a\qb,dark:c",
        r"light:a,dark:c\",
    ] {
        assert!(validate::parse_theme(bad).is_err(), "{bad}");
    }
}

#[test]
fn other_kinds_follow_their_parse() {
    assert_eq!(check_kind(kind("window-decoration"), None), Ok(()));
    assert_eq!(check_kind(kind("window-decoration"), Some("false")), Ok(()));
    assert_eq!(check_kind(kind("window-decoration"), Some("server")), Ok(()));
    assert_eq!(
        check_kind(kind("window-decoration"), Some("maybe")),
        Err(ValueError::Invalid)
    );
    assert_eq!(check_kind(kind("keybind"), None), Err(ValueError::Required));
    assert_eq!(
        check_kind(kind("key-remap"), None),
        Ok(()),
        "an opaque type that takes a bare key"
    );
    assert_eq!(check_kind(kind("title"), None), Err(ValueError::Required));
    assert_eq!(check_kind(kind("config-file"), None), Err(ValueError::Required));
}

#[test]
fn compatibility_handlers_take_renamed_and_removed_keys() {
    assert_eq!(
        validate::compat("scrollback-limit"),
        Some(Compat::Renamed("scrollback-limit-bytes"))
    );
    assert_eq!(count("scrollback-limit = 1000\n"), 0);
    assert_eq!(count("scrollback-limit = lots\n"), 1);
    assert_eq!(
        load("scrollback-limit = 1000\n").config.scrollback.limit_bytes,
        Some(1000)
    );
    assert_eq!(count("gtk-tabs-location = hidden\n"), 0, "an old value of a known key");
    assert_eq!(
        check("gtk-tabs-location", Some("left")).map(drop),
        Err("invalid value \"left\", valid values are: top, bottom".to_owned())
    );
    assert_eq!(count("bold-is-bright\ncursor-invert-fg-bg = false\n"), 0);
    assert_eq!(
        check("cursor-invert-fg-bg", Some("maybe")),
        Err("unknown field".to_owned())
    );
    let inverted = load("cursor-invert-fg-bg = true\nselection-invert-fg-bg\n").config;
    assert_eq!(inverted.cursor.color, Some(TerminalColor::CellForeground));
    assert_eq!(inverted.cursor.text, Some(TerminalColor::CellBackground));
    assert_eq!(inverted.selection.foreground, Some(TerminalColor::CellBackground));
    assert_eq!(inverted.selection.background, Some(TerminalColor::CellForeground));
}

#[test]
fn residual_differences_accept_any_value_of_an_opaque_key() {
    // libghostty counts each of these once; agterm does not port their parse (module docs)
    for line in [
        "adjust-cell-height = tall",
        "mouse-scroll-multiplier = lots",
        "font-style = x",
    ] {
        assert_eq!(count(&format!("{line}\n")), 0, "{line}");
    }
    let opaque: Vec<_> = known_keys()
        .iter()
        .filter(|spec| matches!(spec.kind, Kind::Opaque { .. }))
        .map(|spec| spec.name)
        .collect();
    // exactly the residual list of the module docs: a new opaque key is a new residual difference to document
    let adjust = [
        "box-thickness",
        "cell-height",
        "cell-width",
        "cursor-height",
        "cursor-thickness",
        "font-baseline",
        "icon-height",
        "overline-position",
        "overline-thickness",
        "strikethrough-position",
        "strikethrough-thickness",
        "underline-position",
        "underline-thickness",
    ]
    .map(|metric| format!("adjust-{metric}"));
    let mut expected: Vec<&str> = adjust.iter().map(String::as_str).collect();
    expected.extend([
        "background-blur",
        "clipboard-codepoint-map",
        "command",
        "command-palette-entry",
        "env",
        "font-codepoint-map",
        "font-style",
        "font-style-bold",
        "font-style-bold-italic",
        "font-style-italic",
        "font-variation",
        "font-variation-bold",
        "font-variation-bold-italic",
        "font-variation-italic",
        "initial-command",
        "input",
        "key-remap",
        "macos-icon-screen-color",
        "mouse-scroll-multiplier",
        "quick-terminal-size",
        "selection-word-chars",
        "working-directory",
    ]);
    assert_eq!(opaque, expected);
}

// -- the generated table

#[test]
fn the_key_table_is_sorted_and_carries_the_supported_keys() {
    let keys = known_keys();
    assert!(keys.windows(2).all(|pair| pair[0].name < pair[1].name));
    assert_eq!(GHOSTTY_REV.len(), 40);
    for name in [
        "font-family",
        "font-family-bold",
        "font-family-italic",
        "font-family-bold-italic",
        "font-size",
        "font-feature",
        "theme",
        "palette",
        "background",
        "foreground",
        "cursor-style",
        "selection-background",
        "background-opacity",
        "background-image",
        "window-padding-balance",
        "scrollback-limit-bytes",
        "clipboard-read",
        "mouse-hide-while-typing",
        "cursor-click-to-move",
        "shell-integration-features",
        "term",
        "keybind",
        "title",
        "config-file",
    ] {
        assert!(validate::key_spec(name).is_some(), "{name}");
    }
}

fn enum_names<T: IntoEnumIterator + Into<&'static str>>() -> Vec<&'static str> {
    T::iter().map(Into::into).collect()
}

#[test]
fn the_pointer_keys_load_with_ghosttys_defaults() {
    let defaults = load("").config.mouse;
    assert_eq!(defaults.right_click_action, RightClickAction::ContextMenu);
    assert_eq!(defaults.middle_click_action, MiddleClickAction::PrimaryPaste);
    assert_eq!(defaults.click_repeat_interval, 0);
    assert!(defaults.link_url);
    let set = load("right-click-action = copy-or-paste\nmiddle-click-action = ignore\nclick-repeat-interval = 300\nlink-url = false\n")
        .config
        .mouse;
    assert_eq!(set.right_click_action, RightClickAction::CopyOrPaste);
    assert_eq!(set.middle_click_action, MiddleClickAction::Ignore);
    assert_eq!(set.click_repeat_interval, 300);
    assert!(!set.link_url);
    // an empty value restores the default
    let reset = load("right-click-action = paste\nright-click-action =\n").config.mouse;
    assert_eq!(reset.right_click_action, RightClickAction::ContextMenu);
}

#[test]
fn the_model_enums_spell_ghosttys_values() {
    let cases: [(&str, Vec<&str>); 11] = [
        ("copy-on-select", enum_names::<CopyOnSelect>()),
        ("cursor-style", enum_names::<CursorStyle>()),
        ("background-image-position", enum_names::<BackgroundImagePosition>()),
        ("background-image-fit", enum_names::<BackgroundImageFit>()),
        ("window-padding-balance", enum_names::<PaddingBalance>()),
        ("window-padding-color", enum_names::<PaddingColor>()),
        ("clipboard-read", enum_names::<ClipboardAccess>()),
        ("clipboard-write", enum_names::<ClipboardAccess>()),
        ("mouse-shift-capture", enum_names::<MouseShiftCapture>()),
        ("right-click-action", enum_names::<RightClickAction>()),
        ("middle-click-action", enum_names::<MiddleClickAction>()),
    ];
    for (key, names) in cases {
        let Kind::Enum(expected) = kind(key) else {
            panic!("{key} is not an enum")
        };
        assert_eq!(names, expected, "{key}");
    }
    let Kind::Flags(flags) = kind("shell-integration-features") else {
        panic!("not flags")
    };
    assert_eq!(flags, ShellIntegrationFeatures::NAMES);
}

#[test]
fn shell_features_reach_the_shell_as_libghostty_spells_them() {
    // Ghostty's `setup features` cases: sorted by name, the cursor's blink spelled out, nothing when all are off
    assert_eq!(
        ShellIntegrationFeatures::all().env_value(true).as_deref(),
        Some("cursor:blink,path,ssh-env,ssh-terminfo,sudo,title")
    );
    assert_eq!(ShellIntegrationFeatures::empty().env_value(true), None);
    let ssh_sudo = ShellIntegrationFeatures::SSH_ENV | ShellIntegrationFeatures::SUDO;
    assert_eq!(ssh_sudo.env_value(false).as_deref(), Some("ssh-env,sudo"));
    assert_eq!(
        ShellIntegrationFeatures::CURSOR.env_value(false).as_deref(),
        Some("cursor:steady")
    );
    // agterm's defaults leave only `path`, and never the ssh wrappers
    let defaults = load(DEFAULTS).config.shell_integration_features - ShellIntegrationFeatures::UNSUPPORTED;
    assert_eq!(defaults.env_value(true).as_deref(), Some("path"));
}

// -- the typed model

#[test]
fn defaults_are_ghosttys() {
    let config = GhosttyConfig::default();
    assert_eq!(config.colors.background, Rgb::new(0x28, 0x2c, 0x34));
    assert_eq!(config.window_padding.x, Padding { start: 2, end: 2 });
    assert_eq!(config.cursor.style, CursorStyle::Block);
    assert!(config.cursor.click_to_move);
    assert_eq!(config.scrollback.limit_bytes, Some(50_000_000));
    assert_eq!(config.clipboard.read, ClipboardAccess::Ask);
    assert_eq!(config.shell_integration_features, ShellIntegrationFeatures::DEFAULT);
    assert_eq!(config.term, "xterm-ghostty");
    assert_eq!(config.background_image.position, BackgroundImagePosition::Center);
}

#[test]
fn agterms_defaults_load_clean() {
    let built = load(DEFAULTS);
    assert_eq!(built.diagnostics, []);
    let config = built.config;
    assert_eq!(config.window_padding.x, Padding { start: 8, end: 8 });
    assert_eq!(config.window_padding.y, Padding { start: 6, end: 6 });
    assert_eq!(config.cursor.style, CursorStyle::Block);
    assert!(!config.cursor.click_to_move);
    assert_eq!(config.shell_integration_features, ShellIntegrationFeatures::PATH);
    let mut expected = binds::Keybinds::defaults();
    for line in [
        "ctrl+shift+key_c=copy_to_clipboard",
        "ctrl+shift+key_v=paste_from_clipboard",
        "ctrl+shift+key_a=select_all",
    ] {
        expected.parse_cli(line).expect("a valid bind");
    }
    assert_eq!(
        config.keybind, expected,
        "ghostty's binds with the physical-key Edit chords over them"
    );
}

#[test]
fn unresolved_resources_fall_back_to_xterm_256color() {
    assert!(!defaults_text(true).contains("term ="));
    assert!(defaults_text(false).ends_with("term = xterm-256color\n"));
    assert_eq!(load(&defaults_text(false)).config.term, "xterm-256color");
}

#[test]
fn font_keys() {
    let config = load(
        "font-family = A\nfont-family = B\nfont-family-italic = I\nfont-size = 13.5\nfont-feature = -calt\n\
         font-feature = ss01\n",
    )
    .config;
    assert_eq!(config.font.family, ["A", "B"]);
    assert_eq!(
        config.font.family_bold,
        ["A", "B"],
        "the styled families default to font-family"
    );
    assert_eq!(config.font.family_italic, ["I"]);
    assert!((config.font.size - 13.5).abs() < f32::EPSILON);
    assert_eq!(config.font.feature, ["-calt", "ss01"]);
    let reset = load("font-family = A\nfont-family =\nfont-family = C\nfont-size = 20\nfont-size =\n").config;
    assert_eq!(reset.font.family, ["C"], "an empty value clears the list");
    assert!((reset.font.size - GhosttyConfig::default().font.size).abs() < f32::EPSILON);
}

#[test]
fn color_keys() {
    let config = load(
        "background = #102030\nforeground = white\npalette = 0=#000001\npalette = 15 = red\npalette = 0=#000002\n\
         background-opacity = 0.85\n",
    )
    .config;
    assert_eq!(config.colors.background, Rgb::new(0x10, 0x20, 0x30));
    assert_eq!(config.colors.foreground, Rgb::WHITE);
    assert_eq!(config.colors.palette.get(&0), Some(&Rgb::new(0, 0, 2)));
    assert_eq!(config.colors.palette.get(&15), Some(&Rgb::new(255, 0, 0)));
    assert_eq!(config.colors.palette.len(), 2);
    assert!((config.background_opacity - 0.85).abs() < f64::EPSILON);
    let reset = load("palette = 1=red\npalette =\nbackground = red\nbackground =\n").config;
    assert!(reset.colors.palette.is_empty());
    assert_eq!(reset.colors.background, Colors::default().background);
}

#[test]
fn cursor_and_selection_keys() {
    let config = load(
        "cursor-style = bar\ncursor-style-blink\ncursor-color = #ff0000\ncursor-text = cell-background\n\
         cursor-opacity = 0.5\ncursor-click-to-move = false\nselection-foreground = cell-foreground\n\
         selection-background = #00ff00\nselection-clear-on-typing = false\nselection-clear-on-copy = true\n\
         selection-word-chars = \" ()\"\n",
    )
    .config;
    assert_eq!(config.cursor.style, CursorStyle::Bar);
    assert_eq!(config.cursor.style_blink, Some(true));
    assert_eq!(config.cursor.color, Some(TerminalColor::Color(Rgb::new(255, 0, 0))));
    assert_eq!(config.cursor.text, Some(TerminalColor::CellBackground));
    assert!((config.cursor.opacity - 0.5).abs() < f64::EPSILON);
    assert!(!config.cursor.click_to_move);
    assert_eq!(config.selection.foreground, Some(TerminalColor::CellForeground));
    assert_eq!(
        config.selection.background,
        Some(TerminalColor::Color(Rgb::new(0, 255, 0)))
    );
    assert!(!config.selection.clear_on_typing);
    assert!(config.selection.clear_on_copy);
    assert_eq!(config.selection.word_chars.as_deref(), Some(" ()"));
    let unset = load("cursor-style-blink = false\ncursor-style-blink =\ncursor-color = red\ncursor-color =\n").config;
    assert_eq!(unset.cursor.style_blink, None);
    assert_eq!(unset.cursor.color, None);
}

#[test]
fn background_image_and_padding_keys() -> TestResult {
    let dir = tempfile::tempdir()?;
    let root = dir.path().canonicalize()?;
    let file = write(
        &root.join("conf/ghostty.conf"),
        "background-image = ../img/w.png\nbackground-image-opacity = 0.25\nbackground-image-position = top-left\n\
         background-image-fit = cover\nbackground-image-repeat = true\nwindow-padding-x = 4,8\n\
         window-padding-y = 3\nwindow-padding-balance = equal\nwindow-padding-color = extend\n",
    )?;
    let mut loader = Loader::new(None);
    loader.load_file(&file);
    let config = loader.finish().finalize(false, &[]).config;
    let image = config.background_image;
    assert_eq!(
        image.path,
        Some(ConfigPath {
            path: root.join("img/w.png"),
            optional: false
        }),
        "a missing relative path resolves lexically against its file"
    );
    assert!((image.opacity - 0.25).abs() < f32::EPSILON);
    assert_eq!(image.position, BackgroundImagePosition::TopLeft);
    assert_eq!(image.fit, BackgroundImageFit::Cover);
    assert!(image.repeat);
    assert_eq!(config.window_padding.x, Padding { start: 4, end: 8 });
    assert_eq!(config.window_padding.y, Padding { start: 3, end: 3 });
    assert_eq!(config.window_padding.balance, PaddingBalance::Equal);
    assert_eq!(config.window_padding.color, PaddingColor::Extend);
    Ok(())
}

#[test]
fn scrollback_clipboard_and_mouse_keys() {
    let config = load(
        "scrollback-limit-bytes = unlimited\nscrollback-limit-lines = 10_000\nclipboard-read = allow\n\
         clipboard-write = deny\nclipboard-write-limit-bytes = 1024\nclipboard-trim-trailing-spaces = false\n\
         clipboard-paste-protection = false\nclipboard-paste-bracketed-safe = false\n\
         clipboard-codepoint-map = U+2500=-\nmouse-hide-while-typing = true\nmouse-shift-capture = always\n\
         mouse-reporting = false\nmouse-scroll-multiplier = 2\n",
    )
    .config;
    assert_eq!(config.scrollback.limit_bytes, None);
    assert_eq!(config.scrollback.limit_lines, Some(10_000));
    assert_eq!(config.clipboard.read, ClipboardAccess::Allow);
    assert_eq!(config.clipboard.write, ClipboardAccess::Deny);
    assert_eq!(config.clipboard.write_limit_bytes, Some(1024));
    assert!(!config.clipboard.trim_trailing_spaces);
    assert!(!config.clipboard.paste_protection);
    assert!(!config.clipboard.paste_bracketed_safe);
    assert_eq!(config.clipboard.codepoint_map, ["U+2500=-"]);
    assert!(config.mouse.hide_while_typing);
    assert_eq!(config.mouse.shift_capture, MouseShiftCapture::Always);
    assert!(!config.mouse.reporting);
    assert_eq!(config.mouse.scroll_multiplier.as_deref(), Some("2"));
}

#[test]
fn copy_on_select_key() {
    assert_eq!(
        GhosttyConfig::default().copy_on_select,
        CopyOnSelect::True,
        "Ghostty's Linux default"
    );
    assert_eq!(
        load("copy-on-select = clipboard\n").config.copy_on_select,
        CopyOnSelect::Clipboard
    );
    assert_eq!(
        load("copy-on-select = false\n").config.copy_on_select,
        CopyOnSelect::False
    );
    assert_eq!(
        load("copy-on-select = false\ncopy-on-select =\n").config.copy_on_select,
        CopyOnSelect::True,
        "an empty value restores the default"
    );
}

#[test]
fn shell_features_term_keybind_and_title() {
    let config = load(
        "shell-integration-features = no-cursor,sudo\nterm = xterm-256color\nkeybind = ctrl+a=select_all\n\
         keybind =\nkeybind = ctrl+b=copy_to_clipboard\ntitle = pinned\n",
    )
    .config;
    let expected = ShellIntegrationFeatures::SUDO | ShellIntegrationFeatures::TITLE | ShellIntegrationFeatures::PATH;
    assert_eq!(
        config.shell_integration_features, expected,
        "named flags over ghostty's defaults"
    );
    assert_eq!(config.term, "xterm-256color");
    let mut expected = binds::Keybinds::defaults();
    expected.parse_cli("ctrl+b=copy_to_clipboard").expect("a valid bind");
    assert_eq!(config.keybind, expected, "an empty line restores ghostty's binds");
    assert_eq!(config.title.as_deref(), Some("pinned"));
    let reset = load("shell-integration-features = false\nterm =\ntitle = x\ntitle =\n").config;
    assert_eq!(reset.shell_integration_features, ShellIntegrationFeatures::empty());
    assert_eq!(reset.term, DEFAULT_TERM);
    assert_eq!(reset.title, None);
}

#[test]
fn refused_lines_change_nothing_and_unsupported_keys_are_ignored() {
    let config = load("font-size = 20\nfont-size = big\nmacos-titlebar-style = tabs\n").config;
    assert!((config.font.size - 20.0).abs() < f32::EPSILON);
    assert_eq!(config, load("font-size = 20\n").config);
}

// -- load order, includes

#[test]
fn later_sources_override_earlier_ones_in_load_order() -> TestResult {
    let state = tempfile::tempdir()?;
    write(
        &state.path().join("config/ghostty.conf"),
        "font-size = 14\nconfig-file = inc.conf\n",
    )?;
    write(
        &state.path().join("config/inc.conf"),
        "font-size = 16\nwindow-padding-y = 9\n",
    )?;
    write(
        &state.path().join("ghostty-settings.conf"),
        "font-size = 15\nwindow-padding-x = 1\n",
    )?;
    let env = paths::MapEnv::new()
        .with("HOME", "/nonexistent")
        .with(paths::STATE_DIR_ENV, state.path());
    let config = load_app_config(&env, true, None).finalize(false, &[]);
    assert_eq!(config.diagnostics, []);
    // defaults pad x by 8; ghostty-settings.conf by 1; the include, built after both, sets font-size last
    assert_eq!(config.config.window_padding.x, Padding { start: 1, end: 1 });
    assert_eq!(config.config.window_padding.y, Padding { start: 9, end: 9 });
    assert!((config.config.font.size - 16.0).abs() < f32::EPSILON);
    let overlaid = load_app_config(&env, true, Some("font-size = 30\n")).finalize(false, &[]);
    assert!(
        (overlaid.config.font.size - 30.0).abs() < f32::EPSILON,
        "the overlay wins over the includes"
    );
    Ok(())
}

#[test]
fn the_settings_pick_the_config_directory_and_the_global_config() -> TestResult {
    let state = tempfile::tempdir()?;
    let env = paths::MapEnv::new()
        .with("HOME", "/home/u")
        .with(paths::STATE_DIR_ENV, state.path());
    let settings_conf = state.path().join("ghostty-settings.conf");
    assert_eq!(
        config_files(&env),
        [state.path().join("config/ghostty.conf"), settings_conf.clone()]
    );
    std::fs::write(
        state.path().join("settings.json"),
        r#"{"configDirectory": "/elsewhere", "inheritGlobalGhosttyConfig": true}"#,
    )?;
    assert_eq!(
        config_files(&env),
        [
            PathBuf::from("/home/u/.config/ghostty/config"),
            PathBuf::from("/elsewhere/ghostty.conf"),
            settings_conf
        ]
    );
    Ok(())
}

#[test]
fn includes_resolve_against_their_file_optional_ones_may_be_missing() -> TestResult {
    let dir = tempfile::tempdir()?;
    let root = dir.path().canonicalize()?;
    let main = write(
        &root.join("main.conf"),
        "config-file = sub/a.conf\nconfig-file = ?absent.conf\nconfig-file = absent-too.conf\nfont-size = 11\n",
    )?;
    write(&root.join("sub/a.conf"), "config-file = b.conf\nno-such-key = 1\n")?;
    write(&root.join("sub/b.conf"), "font-size = 19\n")?;
    let mut loader = Loader::new(None);
    loader.load_file(&main);
    loader.load_includes();
    let built = loader.finish().finalize(false, &[]);
    let messages: Vec<_> = built.diagnostics.iter().map(ToString::to_string).collect();
    let absent = root.join("absent-too.conf");
    assert_eq!(
        messages,
        [
            format!("{}:2:no-such-key: unknown field", root.join("sub/a.conf").display()),
            format!("error opening config-file {}: error.FileNotFound", absent.display()),
        ],
        "a.conf loads before the next include is opened"
    );
    assert!(
        (built.config.font.size - 19.0).abs() < f32::EPSILON,
        "b.conf, relative to a.conf, loads last"
    );
    Ok(())
}

#[test]
fn an_include_cycle_counts_once_and_a_directory_is_refused() -> TestResult {
    let dir = tempfile::tempdir()?;
    let root = dir.path().canonicalize()?;
    let a = write(&root.join("a.conf"), "config-file = b.conf\n")?;
    write(&root.join("b.conf"), "config-file = a.conf\nconfig-file = d\n")?;
    std::fs::create_dir(root.join("d"))?;
    let mut loader = Loader::new(None);
    loader.load_file(&a);
    loader.load_includes();
    let messages: Vec<_> = loader
        .finish()
        .finalize(false, &[])
        .diagnostics
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        messages,
        [
            format!(
                "config-file {}: not reading because file type is directory",
                root.join("d").display()
            ),
            format!("config-file {}: cycle detected", root.join("b.conf").display()),
        ],
        "a.conf (a base file) loads again once as an include; b.conf's second time is the cycle"
    );
    Ok(())
}

#[test]
fn an_empty_config_file_value_drops_the_includes_named_before_it() -> TestResult {
    let dir = tempfile::tempdir()?;
    let main = write(
        &dir.path().join("main.conf"),
        "config-file = gone.conf\nconfig-file =\nconfig-file = ?kept.conf\n",
    )?;
    write(&dir.path().join("kept.conf"), "font-size = 21\n")?;
    let mut loader = Loader::new(None);
    loader.load_file(&main);
    loader.load_includes();
    let built = loader.finish().finalize(false, &[]);
    assert_eq!(built.diagnostics, []);
    assert!((built.config.font.size - 21.0).abs() < f32::EPSILON);
    Ok(())
}

#[test]
fn home_relative_paths_expand_against_the_home() -> TestResult {
    let home = tempfile::tempdir()?;
    write(&home.path().join("inc.conf"), "font-size = 23\n")?;
    let mut loader = Loader::new(Some(home.path().to_path_buf()));
    loader.load_text("test.conf", "config-file = ~/inc.conf\nbackground-image = ~/w.png\n");
    loader.load_includes();
    let built = loader.finish().finalize(false, &[]);
    assert_eq!(built.diagnostics, []);
    assert!((built.config.font.size - 23.0).abs() < f32::EPSILON);
    assert_eq!(
        built.config.background_image.path.map(|path| path.path),
        Some(home.path().join("w.png"))
    );
    let mut homeless = Loader::new(None);
    homeless.load_text("test.conf", "config-file = ~/inc.conf\n");
    homeless.load_includes();
    assert_eq!(homeless.finish().diagnostic_count(&[]), 1);
    Ok(())
}

#[test]
fn a_base_file_that_is_not_regular_is_skipped() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut loader = Loader::new(None);
    loader.load_file(dir.path());
    loader.load_file(&dir.path().join("missing.conf"));
    assert!(loader.finish().steps().is_empty());
    Ok(())
}

// -- themes

#[test]
fn a_dual_theme_picks_its_side_and_every_line_beats_it() -> TestResult {
    let themes = tempfile::tempdir()?;
    write(
        &themes.path().join("Paper"),
        "background = #eeeeee\nforeground = #111111\ncursor-style = underline\n",
    )?;
    write(
        &themes.path().join("Ink"),
        "background = #000000\nforeground = #fafafa\n",
    )?;
    let dirs = [themes.path().to_path_buf()];
    let mut loader = Loader::new(None);
    loader.load_text("ghostty-defaults.conf", DEFAULTS);
    loader.load_text("test.conf", "theme = light:Paper,dark:Ink\nforeground = #123456\n");
    let built = loader.finish();
    let light = built.finalize(false, &dirs);
    assert_eq!(light.diagnostics, []);
    assert_eq!(light.theme_path, Some(themes.path().join("Paper")));
    assert_eq!(light.config.colors.background, Rgb::new(0xee, 0xee, 0xee));
    assert_eq!(
        light.config.colors.foreground,
        Rgb::new(0x12, 0x34, 0x56),
        "a config line beats the theme"
    );
    assert_eq!(
        light.config.cursor.style,
        CursorStyle::Block,
        "even agterm's defaults beat the theme"
    );
    let dark = built.finalize(true, &dirs);
    assert_eq!(dark.config.colors.background, Rgb::BLACK);
    assert_eq!(
        dark.config.theme,
        Some(ThemeName {
            light: "Paper".to_owned(),
            dark: "Ink".to_owned()
        })
    );
    Ok(())
}

#[test]
fn a_missing_theme_counts_once_per_directory_tried() -> TestResult {
    let user = tempfile::tempdir()?;
    let bundled = tempfile::tempdir()?;
    let dirs = [user.path().to_path_buf(), bundled.path().to_path_buf()];
    let mut loader = Loader::new(None);
    loader.load_text("test.conf", "theme = Nope\nno-such-key = 1\n");
    let built = loader.finish();
    let messages: Vec<_> = built
        .finalize(false, &dirs)
        .diagnostics
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        messages,
        [
            "test.conf:2:no-such-key: unknown field".to_owned(),
            format!(
                "theme \"Nope\" not found, tried path \"{}\"",
                user.path().join("Nope").display()
            ),
            format!(
                "theme \"Nope\" not found, tried path \"{}\"",
                bundled.path().join("Nope").display()
            ),
        ]
    );
    assert_eq!(built.diagnostic_count(&dirs[..1]), 2);
    Ok(())
}

#[test]
fn the_count_takes_the_light_side_of_a_dual_theme() -> TestResult {
    let themes = tempfile::tempdir()?;
    write(&themes.path().join("Ink"), "background = nope\n")?;
    let dirs = [themes.path().to_path_buf()];
    let mut loader = Loader::new(None);
    loader.load_text("test.conf", "theme = light:Missing,dark:Ink\n");
    let built = loader.finish();
    assert_eq!(built.diagnostic_count(&dirs), 1, "the light theme is not found");
    let dark = built.finalize(true, &dirs);
    assert_eq!(dark.diagnostics.len(), 1, "the dark theme's bad line");
    assert_eq!(
        dark.diagnostics[0].to_string(),
        format!(
            "{}:1:background: invalid value \"nope\"",
            themes.path().join("Ink").display()
        )
    );
    Ok(())
}

#[test]
fn theme_paths_absolute_or_bare_names_only() -> TestResult {
    let dir = tempfile::tempdir()?;
    let theme = write(&dir.path().join("mine"), "palette = 1=#010101\n")?;
    let built = load(&format!("theme = {}\n", theme.display()));
    assert_eq!(built.diagnostics, []);
    assert_eq!(built.config.colors.palette.get(&1), Some(&Rgb::new(1, 1, 1)));
    let missing = dir.path().join("missing");
    let messages: Vec<_> = load(&format!("theme = {}\n", missing.display()))
        .diagnostics
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        messages,
        [format!("failed to load theme from the path \"{}\"", missing.display())]
    );
    assert_eq!(count("theme = sub/dir\n"), 1);
    assert_eq!(count(&format!("theme = {}\n", dir.path().display())), 1, "a directory");
    assert_eq!(
        count("theme = light:A\n"),
        1,
        "a pair without its dark side is malformed"
    );
    Ok(())
}

#[test]
fn theme_files_go_through_the_same_parser() -> TestResult {
    let dir = tempfile::tempdir()?;
    let theme = write(
        &dir.path().join("t"),
        "\u{feff}background = red\r\nselection-background = nope\nwho = 1\n",
    )?;
    let built = load(&format!("theme = {}\n", theme.display()));
    // loadTheme skips no BOM: the first key is "\u{feff}background", an unknown field
    assert_eq!(built.diagnostics.len(), 3);
    assert_eq!(built.diagnostics[0].key, "\u{feff}background");
    assert_eq!(built.config.colors.background, Colors::default().background);
    Ok(())
}

#[test]
fn theme_dirs_are_the_users_then_the_bundled_ones() {
    let env = paths::MapEnv::new().with("HOME", "/home/u");
    assert_eq!(
        theme_dirs(&env, Some(Path::new("/res/themes"))),
        [
            PathBuf::from("/home/u/.config/ghostty/themes"),
            PathBuf::from("/res/themes")
        ]
    );
    let xdg = paths::MapEnv::new()
        .with("HOME", "/home/u")
        .with("XDG_CONFIG_HOME", "/xdg");
    assert_eq!(theme_dirs(&xdg, None), [PathBuf::from("/xdg/ghostty/themes")]);
}

#[test]
fn accepted_lines_name_their_key() {
    assert!(matches!(check("font-size", Some("12")), Ok(Accepted::Field(spec)) if spec.name == "font-size"));
    assert_eq!(check("bold-is-bright", None), Ok(Accepted::Compat(Compat::Bool)));
}
