//! Headless tests of the terminal (plan > Task 13): cell metrics for a known font, the grid's geometry, row plans
//! (run splitting by style, wide characters, marks, decorations) and their cache, cell and cursor colors, the element
//! in GPUI's test windows over a real pane (spawn at the laid-out grid, glyph placement, two views of one pane, the
//! cursor's blink), and the [`ngterm_host::Surface`] contract the fakes pin.
//!
//! Panes run `/bin/sh` scripts with no `NGTERM_*`, `AGTERM_*` or `ZMX_*` variable inherited (CLAUDE.md > Never touch
//! the user's running ngterm). GPUI's test platform draws nothing to a screen; its default text system is a fixed fake
//! (1000 units per em, every ASCII advance 600, ascent 1025, descent -275), the real-font test uses GPUI's Linux text
//! system.

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::rc::Rc;
use std::str::FromStr as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _, TestAppContext,
    TestDispatcher, VisualTestContext, Window, div, px, size,
};
use ngterm_core::ghostty_config::binds::Action;
use ngterm_core::ghostty_config::{
    CopyOnSelect, GhosttyConfig, GraphemeWidthMethod, PaddingBalance, TerminalColor, WindowPadding,
};
use ngterm_host::{Surface, TextScope};
use ngterm_term::{
    Cell, CellFlags, Colors, CursorPosition, CursorStyle, Frame, GridPoint, GridSize, Pane, PaneCommand, PaneConfig,
    Rgb, Underline,
};

use super::colors::{self, Mark, Palette, Pick};
use super::element::{cursor_alpha, cursor_colors, cursor_shape, lock_cursor};
use super::font::{self, Fonts};
use super::metrics::{self, CellMetrics, FaceMetrics, Geometry, Padding};
use super::row_cache::{Cluster, Line, RowCache, RowMarks, Span, layout_row, search_marks};
use super::{Spawn, Terminal, TerminalConfig, TerminalEvent, TerminalSpec, TerminalSurface, TerminalView, ViewRole};

const DEADLINE: Duration = Duration::from_secs(20);

// ---------------------------------------------------------------------------------------------------------------
// panes

/// `sh -c script` with the inherited `NGTERM_*`/`AGTERM_*`/`ZMX_*` variables removed, in `dir`.
fn command(script: &str, dir: &Path) -> PaneCommand {
    let mut command = PaneCommand::new("/bin/sh").arg("-c").arg(script).cwd(dir);
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("NGTERM_") || name.starts_with("AGTERM_") || name.starts_with("ZMX_") {
            command = command.env_remove(key);
        }
    }
    command
}

fn spawn_script(script: &str, dir: &Path) -> Spawn {
    let command = command(script, dir);
    Box::new(move |config: PaneConfig| Pane::spawn(&command, &config))
}

/// A pane outside GPUI, for the row plans.
fn pane(script: &str, dir: &Path) -> Pane {
    let size = GridSize {
        cols: 20,
        rows: 4,
        cell_width: 10,
        cell_height: 20,
    };
    Pane::spawn(&command(script, dir), &PaneConfig::new(size)).expect("the pane starts")
}

fn wait_frame(pane: &Pane, done: impl Fn(&Frame) -> bool) -> Arc<Frame> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let frame = pane.frame().frame;
        if done(&frame) {
            return frame;
        }
        assert!(Instant::now() < deadline, "timed out:\n{}", frame.lines().join("\n"));
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait_text(pane: &Pane, needle: &str) -> Arc<Frame> {
    wait_frame(pane, |frame| frame.lines().iter().any(|line| line.contains(needle)))
}

// ---------------------------------------------------------------------------------------------------------------
// metrics and geometry

#[test]
fn ghostty_metrics_round_the_cell_and_center_the_baseline() {
    // DejaVu Sans Mono at 16 px: upem 2048, advance 1233, hhea 1901/-483, x-height 1120
    let face = FaceMetrics {
        cell_width: 1233.0 / 2048.0 * 16.0,
        ascent: 1901.0 / 2048.0 * 16.0,
        descent: -483.0 / 2048.0 * 16.0,
        ex_height: Some(1120.0 / 2048.0 * 16.0),
        cap_height: Some(1493.0 / 2048.0 * 16.0),
        ..FaceMetrics::default()
    };
    let cell = CellMetrics::calc(&face);
    assert_eq!((cell.cell_width, cell.cell_height, cell.cell_baseline), (10, 19, 4));
    // Ghostty's estimates: 15 % of the ex height, one thickness below the baseline
    assert_eq!((cell.underline_thickness, cell.underline_position), (2, 16));
    assert_eq!(cell.strikethrough_position, 10);
    assert_eq!(cell.baseline_from_top(), 15);
    assert_eq!((cell.cursor_thickness, cell.cursor_height), (1, 19));
    // a line gap is split above and below the face
    let gapped = CellMetrics::calc(&FaceMetrics { line_gap: 4.0, ..face });
    assert_eq!((gapped.cell_height, gapped.cell_baseline), (23, 6));
}

#[test]
fn metrics_never_collapse_to_zero() {
    let cell = CellMetrics::calc(&FaceMetrics::default());
    assert_eq!((cell.cell_width, cell.cell_height), (1, 1));
    assert!(cell.underline_thickness >= 1 && cell.strikethrough_thickness >= 1);
}

#[test]
fn points_are_ghostty_points_at_96_dpi() {
    assert!((metrics::points_to_px(12.0) - 16.0).abs() < f32::EPSILON);
    let padding = WindowPadding::default();
    // 2 points each side: floor(2 * 96 / 72 * scale)
    let at_one = Padding::from_config(&padding, 1.0);
    assert_eq!((at_one.left, at_one.top), (2, 2));
    let at_two = Padding::from_config(&padding, 2.0);
    assert_eq!((at_two.right, at_two.bottom), (5, 5));
}

fn cell(width: u32, height: u32) -> CellMetrics {
    CellMetrics::calc(&FaceMetrics {
        cell_width: f64::from(width),
        ascent: f64::from(height),
        ..FaceMetrics::default()
    })
}

#[test]
fn the_grid_fits_inside_the_padding_and_balances_when_asked() {
    let cell = cell(10, 20);
    let explicit = Padding {
        top: 4,
        bottom: 4,
        left: 6,
        right: 6,
    };
    let plain = Geometry::new((1000, 500), &cell, explicit, PaddingBalance::False);
    assert_eq!((plain.cols, plain.rows, plain.padding), (98, 24, explicit));
    // equal: the leftover split evenly around the same grid
    let equal = Geometry::new((1005, 505), &cell, explicit, PaddingBalance::Equal);
    assert_eq!((equal.cols, equal.rows), (99, 24));
    assert_eq!((equal.padding.left, equal.padding.right), (7, 7));
    assert_eq!((equal.padding.top, equal.padding.bottom), (12, 12));
    // true: the top is capped at the horizontal padding's mean plus half a cell, the rest goes down
    let capped = Geometry::new((1005, 505), &cell, explicit, PaddingBalance::True);
    assert_eq!((capped.padding.top, capped.padding.bottom), (11, 13));
    // a view smaller than one cell still has one
    let tiny = Geometry::new((3, 3), &cell, explicit, PaddingBalance::False);
    assert_eq!((tiny.cols, tiny.rows), (1, 1));
}

#[gpui_kit::test]
fn the_cell_comes_from_the_text_systems_face(cx: &mut TestAppContext) {
    // the test platform's fixed face: advance 600 / 1000 em, ascent 1025, descent -275
    let text = cx.text_system().clone();
    let id = text.resolve_font(&gpui_kit::font("any"));
    let cell = metrics::cell_metrics(&text, id, 12.0, 1.0);
    assert_eq!((cell.cell_width, cell.cell_height, cell.cell_baseline), (10, 21, 4));
    let hidpi = metrics::cell_metrics(&text, id, 12.0, 2.0);
    assert_eq!((hidpi.cell_width, hidpi.cell_height), (19, 42));
}

/// The file of `family` (whose file name holds `file`), as fontconfig finds it.
fn font_file(family: &str, file: &str) -> Vec<u8> {
    let output = std::process::Command::new("fc-match")
        .args(["-f", "%{file}", &format!("{family}:style=Book")])
        .output()
        .expect("fc-match runs (fontconfig is installed)");
    let path = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        path.contains(file),
        "{family} is not installed (fonts-dejavu-core); fontconfig gave {path:?}"
    );
    std::fs::read(&path).expect("the font file reads")
}

/// `DejaVu Sans Mono`'s file, as fontconfig finds it.
fn dejavu_sans_mono() -> Vec<u8> {
    font_file("DejaVu Sans Mono", "DejaVuSansMono")
}

/// A test app measuring with GPUI's Linux text system, `DejaVu Sans Mono` and the proportional `DejaVu Sans` loaded.
fn real_fonts() -> TestAppContext {
    let text_system = Arc::new(gpui_wgpu::CosmicTextSystem::new_without_system_fonts(
        "DejaVu Sans Mono",
    ));
    let cx = TestAppContext::build_with_text_system(TestDispatcher::new(0), None, text_system);
    cx.text_system()
        .add_fonts(vec![
            dejavu_sans_mono().into(),
            font_file("DejaVu Sans", "DejaVuSans.").into(),
        ])
        .expect("the fonts load");
    cx
}

#[test]
fn a_known_font_measures_as_ghostty_measures_it() {
    let text_system = Arc::new(gpui_wgpu::CosmicTextSystem::new_without_system_fonts(
        "DejaVu Sans Mono",
    ));
    let cx = TestAppContext::build_with_text_system(TestDispatcher::new(0), None, text_system);
    let text = cx.text_system().clone();
    text.add_fonts(vec![dejavu_sans_mono().into()]).expect("the font loads");
    assert!(text.all_font_names().iter().any(|name| name == "DejaVu Sans Mono"));
    let id = text.resolve_font(&gpui_kit::font("DejaVu Sans Mono"));
    // 12 points = 16 px: 1233/2048 em wide, (1901 + 483)/2048 em high
    let cell = metrics::cell_metrics(&text, id, 12.0, 1.0);
    assert_eq!((cell.cell_width, cell.cell_height, cell.cell_baseline), (10, 19, 4));
    let hidpi = metrics::cell_metrics(&text, id, 12.0, 2.0);
    assert_eq!((hidpi.cell_width, hidpi.cell_height, hidpi.cell_baseline), (19, 37, 8));
}

// ---------------------------------------------------------------------------------------------------------------
// fonts

#[test]
fn the_first_installed_family_wins_and_the_rest_fall_back() {
    let installed =
        |family: &str| ["DejaVu Sans Mono", "Liberation Mono", "Hack", font::SYMBOLS_FAMILY].contains(&family);
    let config = ngterm_core::ghostty_config::Font {
        family: vec!["Missing Mono".into(), "Hack".into(), "DejaVu Sans Mono".into()],
        family_bold: vec!["Liberation Mono".into()],
        feature: vec!["-calt".into(), "ss01, liga=0".into(), "zero on".into(), "bogus".into()],
        ..ngterm_core::ghostty_config::Font::default()
    };
    let fonts = Fonts::resolve(&config, &installed);
    assert_eq!(fonts.regular.family.as_ref(), "Hack");
    // the configured families after the primary, then the default family where installed (not here), then the
    // symbols font
    let fallbacks = fonts.regular.fallbacks.as_ref().map(|f| f.fallback_list().to_vec());
    assert_eq!(
        fallbacks,
        Some(vec!["DejaVu Sans Mono".to_owned(), font::SYMBOLS_FAMILY.to_owned()])
    );
    assert_eq!(fonts.bold.family.as_ref(), "Liberation Mono");
    assert_eq!(fonts.bold.weight, gpui_kit::FontWeight::BOLD);
    // no italic family: the regular one, italic
    assert_eq!(fonts.italic.family.as_ref(), "Hack");
    assert_eq!(fonts.italic.style, gpui_kit::FontStyle::Italic);
    assert_eq!(
        *fonts.regular.features.0,
        [
            ("calt".to_owned(), 0),
            ("ss01".to_owned(), 1),
            ("liga".to_owned(), 0),
            ("zero".to_owned(), 1)
        ]
    );
    // nothing configured: Ghostty's default, which the app embeds
    let unset = Fonts::resolve(&ngterm_core::ghostty_config::Font::default(), &installed);
    assert_eq!(unset.regular.family.as_ref(), "JetBrains Mono");
    assert_eq!(font::DEFAULT_FAMILIES, ["JetBrains Mono"]);
}

// ---------------------------------------------------------------------------------------------------------------
// colors

fn colors() -> Colors {
    Colors {
        background: Rgb::new(0, 0, 0),
        foreground: Rgb::new(200, 200, 200),
        ..Colors::default()
    }
}

#[test]
fn cell_colors_follow_ghosttys_rules() {
    let colors = colors();
    let palette = Palette::default();
    let plain = Cell::default();
    let painted = colors::cell_paint(&plain, "a", Mark::None, &colors, &palette);
    assert_eq!(
        (painted.background, painted.foreground, painted.alpha),
        (None, colors.foreground, 255)
    );
    // inverse swaps the defaults in
    let mut inverse = Cell::default();
    inverse.flags = CellFlags::INVERSE | CellFlags::FAINT;
    let painted = colors::cell_paint(&inverse, "a", Mark::None, &colors, &palette);
    assert_eq!(
        (painted.background, painted.foreground),
        (Some(colors.foreground), colors.background)
    );
    assert_eq!(painted.alpha, 128, "faint is half opaque, rounded up");
    // a full block covers its cell: its foreground is its background
    let block = colors::cell_paint(&plain, "\u{2588}", Mark::None, &colors, &palette);
    assert_eq!(block.background, Some(colors.foreground));
    // the default selection is the defaults reversed; a configured one wins
    let selected = colors::cell_paint(&plain, "a", Mark::Selection, &colors, &palette);
    assert_eq!(
        (selected.background, selected.foreground),
        (Some(colors.foreground), colors.background)
    );
    let mut config = GhosttyConfig::default();
    config.selection.background = Some(TerminalColor::Color(ngterm_core::chrome::Rgb::new(1, 2, 3)));
    config.selection.foreground = Some(TerminalColor::CellBackground);
    let custom = Palette::from_config(&config);
    let selected = colors::cell_paint(&plain, "a", Mark::Selection, &colors, &custom);
    assert_eq!(
        (selected.background, selected.foreground),
        (Some(Rgb::new(1, 2, 3)), colors.background)
    );
    // search marks use Ghostty's search colors
    let found = colors::cell_paint(&plain, "a", Mark::SearchSelected, &colors, &palette);
    assert_eq!(found.background, Some(Rgb::new(0xF2, 0xA5, 0x7E)));
}

#[test]
fn the_cursor_color_prefers_the_program_then_the_config_then_the_foreground() {
    let mut colors = colors();
    let mut palette = Palette::default();
    let mut cell = Cell::default();
    cell.foreground = Some(Rgb::new(9, 9, 9));
    cell.background = Some(Rgb::new(7, 7, 7));
    assert_eq!(colors::cursor_color(Some(&cell), &colors, &palette), colors.foreground);
    assert_eq!(colors::cursor_text(Some(&cell), &colors, &palette), colors.background);
    palette.cursor_color = Some(Pick::CellForeground);
    palette.cursor_text = Some(Pick::CellBackground);
    assert_eq!(colors::cursor_color(Some(&cell), &colors, &palette), Rgb::new(9, 9, 9));
    assert_eq!(colors::cursor_text(Some(&cell), &colors, &palette), Rgb::new(7, 7, 7));
    colors.cursor = Some(Rgb::new(1, 1, 1));
    assert_eq!(colors::cursor_color(Some(&cell), &colors, &palette), Rgb::new(1, 1, 1));
}

// ---------------------------------------------------------------------------------------------------------------
// row plans

#[test]
fn runs_split_by_style_and_keep_wide_characters_on_their_columns() {
    let dir = tempfile::tempdir().unwrap();
    let pane = pane(
        "printf 'ab\\033[1mcd\\033[0m e日本x\\033[31my\\033[0m'; exec sleep 5",
        dir.path(),
    );
    let frame = wait_text(&pane, "x");
    let layout = layout_row(&frame.rows[0], &frame.colors, &Palette::default(), &RowMarks::default());
    let texts: Vec<(&str, bool, u16)> = layout
        .runs
        .iter()
        .map(|run| (run.text.as_str(), run.style.bold, run.clusters[0].col))
        .collect();
    // style changes and the blank split runs; the wide characters stay in theirs, two columns each
    assert_eq!(
        texts,
        [
            ("ab", false, 0),
            ("cd", true, 2),
            ("e日本x", false, 5),
            ("y", false, 11)
        ]
    );
    assert_eq!(
        layout.runs[2].clusters,
        [
            Cluster { byte: 0, col: 5 },
            Cluster { byte: 1, col: 6 },
            Cluster { byte: 4, col: 8 },
            Cluster { byte: 7, col: 10 },
        ]
    );
    assert_eq!(layout.runs[2].cluster_of(5), 2, "a byte inside 本 is its cluster");
    assert_eq!(layout.runs[3].style.color, frame.colors.palette[1]);
    assert!(layout.backgrounds.is_empty(), "default backgrounds are not drawn");
}

#[test]
fn kitty_placeholder_cells_shape_no_text_but_keep_their_background() {
    let dir = tempfile::tempdir().unwrap();
    // a, then two placeholder cells (U+10EEEE U+0305 U+0305, image id 1 in the foreground) on a red background, b
    let script = concat!(
        r"printf 'a\033[38;2;0;0;1;41m\364\216\273\256\314\205\314\205\364\216\273\256\314\205\314\215",
        r"\033[0mb'; exec sleep 5"
    );
    let pane = pane(script, dir.path());
    let frame = wait_text(&pane, "b");
    assert_eq!(frame.rows[0].placeholders().len(), 2);
    let layout = layout_row(&frame.rows[0], &frame.colors, &Palette::default(), &RowMarks::default());
    let texts: Vec<(&str, u16)> = layout
        .runs
        .iter()
        .map(|run| (run.text.as_str(), run.clusters[0].col))
        .collect();
    // nothing drawn over the image: the placeholders split the runs and shape no glyph (Ghostty shapes a space)
    assert_eq!(texts, [("a", 0), ("b", 3)]);
    assert_eq!(
        layout.backgrounds,
        [Span {
            start: 1,
            len: 2,
            value: frame.colors.palette[1]
        }]
    );
}

#[test]
fn backgrounds_merge_and_decorations_span_their_cells() {
    let dir = tempfile::tempdir().unwrap();
    let pane = pane(
        "printf '\\033[41mab\\033[42mc\\033[0m \\033[4:3;9mde\\033[0m \\033[53m日\\033[0mz'; exec sleep 5",
        dir.path(),
    );
    let frame = wait_text(&pane, "z");
    let layout = layout_row(&frame.rows[0], &frame.colors, &Palette::default(), &RowMarks::default());
    let palette = &frame.colors.palette;
    assert_eq!(
        layout.backgrounds,
        [
            Span {
                start: 0,
                len: 2,
                value: palette[1]
            },
            Span {
                start: 2,
                len: 1,
                value: palette[2]
            },
        ]
    );
    let lines: Vec<(Line, u16, u16)> = layout
        .decorations
        .iter()
        .map(|span| (span.value.line, span.start, span.len))
        .collect();
    assert_eq!(
        lines,
        [
            (Line::Underline(Underline::Curly), 4, 2),
            (Line::Strikethrough, 4, 2),
            (Line::Overline, 7, 2),
        ]
    );
}

#[test]
fn a_path_link_ends_at_the_path_not_at_the_rows_blank_tail() {
    let dir = tempfile::tempdir().unwrap();
    let pane = pane("printf 'see ./notes.md'; exec sleep 5", dir.path());
    let frame = wait_text(&pane, "./notes.md");
    let link = super::links::url_at(&frame, 6, 0).expect("the path is a link");
    assert_eq!(link.url, "./notes.md");
    assert_eq!(
        link.cells,
        vec![(0, 4, 13)],
        "the path's cells, none of the blank ones after it"
    );
    assert_eq!(super::links::url_at(&frame, 17, 0), None, "the blank tail is no link");
}

#[test]
fn marks_color_the_selection_and_search_and_a_hovered_link_is_underlined() {
    let dir = tempfile::tempdir().unwrap();
    let pane = pane("printf 'hello world\\033[4m!\\033[0m'; exec sleep 5", dir.path());
    let frame = wait_text(&pane, "world");
    let colors = &frame.colors;
    let marks = RowMarks {
        selection: Some((0, 1)),
        search: vec![(6, 10, Mark::Search)],
        link: Some((6, 11)),
    };
    let layout = layout_row(&frame.rows[0], colors, &Palette::default(), &marks);
    assert_eq!(
        layout.backgrounds[0],
        Span {
            start: 0,
            len: 2,
            value: colors.foreground
        }
    );
    assert_eq!(
        layout.backgrounds[1],
        Span {
            start: 6,
            len: 5,
            value: Rgb::new(0xFF, 0xE0, 0x82)
        }
    );
    let style = |text: &str| layout.runs.iter().find(|run| run.text == text).map(|run| run.style);
    assert_eq!(style("he").map(|style| style.color), Some(colors.background));
    let underlines: Vec<(Underline, u16, u16)> = layout
        .decorations
        .iter()
        .filter_map(|span| match span.value.line {
            Line::Underline(kind) => Some((kind, span.start, span.len)),
            _ => None,
        })
        .collect();
    // the link: single, and double where the cell was underlined already
    assert_eq!(underlines, [(Underline::Single, 6, 5), (Underline::Double, 11, 1)]);
}

#[test]
fn search_highlights_spanning_rows_mark_every_row() {
    let state = ngterm_term::SearchState {
        needle: "x".into(),
        highlights: vec![ngterm_term::Highlight {
            start: (5, 0),
            end: (2, 2),
        }],
        selected_highlight: Some(ngterm_term::Highlight {
            start: (0, 3),
            end: (1, 3),
        }),
        ..ngterm_term::SearchState::default()
    };
    let marks = search_marks(&state, 4, 10);
    assert_eq!(marks[0], [(5, 9, Mark::Search)]);
    assert_eq!(marks[1], [(0, 9, Mark::Search)]);
    assert_eq!(marks[2], [(0, 2, Mark::Search)]);
    assert_eq!(marks[3], [(0, 1, Mark::SearchSelected)]);
}

#[test]
fn the_row_cache_rebuilds_only_rows_that_changed() {
    let dir = tempfile::tempdir().unwrap();
    let pane = pane(
        "stty -echo; printf 'one\\r\\ntwo\\r\\nthree'; read _; printf '\\033[2;1Hdeux'; exec sleep 5",
        dir.path(),
    );
    let first = wait_text(&pane, "three");
    let palette = Arc::new(Palette::default());
    let mut cache: RowCache<(), u32> = RowCache::default();
    cache.update(&first, &palette, &[], 1);
    assert_eq!(cache.rebuilt, [0, 1, 2, 3]);
    cache.update(&first, &palette, &[], 1);
    assert!(cache.rebuilt.is_empty(), "the same frame rebuilds nothing");
    for row in cache.rows_mut() {
        row.shaped = Some(());
    }
    pane.write(b"\n").unwrap();
    let second = wait_text(&pane, "deux");
    cache.update(&second, &palette, &[], 1);
    // the rewritten row, and the rows the cursor left or entered; never the first
    assert!(
        cache.rebuilt.contains(&1) && !cache.rebuilt.contains(&0),
        "{:?}",
        cache.rebuilt
    );
    assert!(
        cache.row(0).is_some_and(|row| row.shaped.is_some()),
        "a kept row keeps its shaping"
    );
    // a mark is an input of its row; a font change drops every shaping but rebuilds nothing
    let marks = [
        RowMarks::default(),
        RowMarks::default(),
        RowMarks {
            selection: Some((0, 0)),
            ..RowMarks::default()
        },
    ];
    cache.update(&second, &palette, &marks, 1);
    assert_eq!(cache.rebuilt, [2]);
    cache.update(&second, &palette, &marks, 2);
    assert!(cache.rebuilt.is_empty());
    assert!(cache.rows().all(|row| row.shaped.is_none()));
    // a new palette (a config reload) rebuilds everything
    cache.update(&second, &Arc::new(Palette::default()), &marks, 2);
    assert_eq!(cache.rebuilt.len(), 4);
}

// ---------------------------------------------------------------------------------------------------------------
// cursor

#[test]
fn cursor_opacity_applies_only_to_a_focused_cursor() {
    let mut config = GhosttyConfig::default();
    config.cursor.opacity = 0.5;
    let palette = Palette::from_config(&config);
    assert_eq!(cursor_alpha(true, &palette), 128);
    assert_eq!(
        cursor_alpha(false, &palette),
        255,
        "the hollow cursor of an unfocused view is opaque"
    );
}

#[test]
fn the_cursor_shape_follows_focus_blink_and_visibility() {
    let mut cursor = ngterm_term::Cursor {
        position: Some(CursorPosition {
            column: 4,
            row: 1,
            wide_tail: false,
        }),
        visible: true,
        blinking: true,
        password_input: false,
        style: CursorStyle::Bar,
    };
    assert_eq!(cursor_shape(&cursor, true, true), Some((CursorStyle::Bar, 4, 1)));
    assert_eq!(cursor_shape(&cursor, true, false), None, "the off phase of the blink");
    assert_eq!(
        cursor_shape(&cursor, false, false),
        Some((CursorStyle::BlockHollow, 4, 1)),
        "unfocused: a steady hollow block"
    );
    cursor.position = Some(CursorPosition {
        column: 4,
        row: 1,
        wide_tail: true,
    });
    assert_eq!(
        cursor_shape(&cursor, true, true),
        Some((CursorStyle::Bar, 3, 1)),
        "on a wide character's head"
    );
    cursor.visible = false;
    assert_eq!(cursor_shape(&cursor, true, true), None);
    cursor.visible = true;
    cursor.position = None;
    assert_eq!(cursor_shape(&cursor, true, true), None, "scrolled out of the viewport");
}

#[test]
fn a_password_prompt_shows_the_lock_whatever_the_cursor() {
    let mut cursor = ngterm_term::Cursor {
        position: Some(CursorPosition {
            column: 3,
            row: 1,
            wide_tail: false,
        }),
        visible: false,
        blinking: true,
        password_input: false,
        style: CursorStyle::Bar,
    };
    assert_eq!(lock_cursor(&cursor), None, "no password, no lock");
    cursor.password_input = true;
    assert_eq!(
        lock_cursor(&cursor),
        Some((3, 1)),
        "hidden, blinking, a bar: the lock all the same"
    );
    cursor.position = Some(CursorPosition {
        column: 3,
        row: 1,
        wide_tail: true,
    });
    assert_eq!(lock_cursor(&cursor), Some((2, 1)), "on a wide character's head");
    cursor.position = None;
    assert_eq!(lock_cursor(&cursor), None, "scrolled out of the viewport");
}

// ---------------------------------------------------------------------------------------------------------------
// the element over a real pane

fn spec() -> TerminalSpec {
    TerminalSpec {
        token: "pane-token".into(),
        live: false,
        font_size: None,
    }
}

fn terminal(cx: &mut TestAppContext, script: &str, dir: &Path) -> Entity<Terminal> {
    // the pane's threads wake the pump from outside the test scheduler
    cx.executor().allow_parking();
    let spawn = spawn_script(script, dir);
    cx.new(|cx| Terminal::new(spec(), TerminalConfig::default(), spawn, cx))
}

/// Runs the app until `done` holds (the pane's threads run meanwhile), or fails.
fn settle(cx: &mut VisualTestContext, mut done: impl FnMut(&mut VisualTestContext) -> bool) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for the terminal");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn painted_text(view: &Entity<TerminalView>, cx: &mut VisualTestContext) -> String {
    view.read_with(cx, |view, _| {
        view.painted().map(|frame| frame.lines().join("\n")).unwrap_or_default()
    })
}

fn wait_painted(view: &Entity<TerminalView>, cx: &mut VisualTestContext, needle: &str) {
    settle(cx, |cx| painted_text(view, cx).contains(needle));
}

#[gpui_kit::test]
fn the_owner_starts_the_pane_at_its_grid_and_resizes_it(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let script = "printf 'ready'; read _; stty size; read _; stty size; exec cat";
    let terminal = terminal(cx, script, dir.path());
    let shared = terminal.read_with(cx, |terminal, _| Rc::clone(terminal.shared()));
    assert!(!shared.is_realized(), "no pane before a view lays it out");
    let (view, cx) = cx.add_window_view(|_, cx| TerminalView::new(&terminal, ViewRole::Owner, cx));
    cx.simulate_scale_factor_change(1.0);
    cx.simulate_resize(size(px(640.), px(480.)));
    wait_painted(&view, cx, "ready");
    assert!(shared.is_realized());
    assert!(shared.drew.get(), "the first frame was drawn (and logged)");
    // the fake face's 10x21 cell, 2 px of padding each side: 63 x 22
    settle(cx, |_| shared.laid.get().is_some_and(|laid| laid.grid.cell_width == 10));
    let laid = shared.laid.get().expect("laid out");
    assert_eq!((laid.grid.cols, laid.grid.rows), (63, 22));
    assert_eq!((laid.grid.cell_width, laid.grid.cell_height), (10, 21));
    shared.pane().unwrap().write(b"\n").unwrap();
    wait_painted(&view, cx, "22 63");
    cx.simulate_resize(size(px(320.), px(240.)));
    settle(cx, |_| shared.laid.get().is_some_and(|laid| laid.grid.cols == 31));
    // the pane itself was resized: its program sees the new size
    shared.pane().unwrap().write(b"\n").unwrap();
    wait_painted(&view, cx, "11 31");
    let frame = view.read_with(cx, |view, _| view.painted().cloned()).unwrap();
    assert_eq!((frame.cols, frame.rows.len()), (31, 11));
}

#[gpui_kit::test]
fn glyphs_sit_at_their_clusters_columns(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let terminal = terminal(cx, "printf 'a日b\\342\\200\\224c'; exec sleep 5", dir.path());
    let (view, cx) = cx.add_window_view(|_, cx| TerminalView::new(&terminal, ViewRole::Owner, cx));
    wait_painted(&view, cx, "a日b");
    view.read_with(cx, |view, _| {
        let glyphs = &view.row_glyphs(0).expect("row 0 is shaped").glyphs;
        let cols: Vec<u16> = glyphs.iter().map(|glyph| glyph.col).collect();
        // a, the wide 日 over columns 1-2, b, the em dash, c
        assert_eq!(cols, [0, 1, 3, 4, 5]);
        assert!(
            glyphs.iter().all(|glyph| glyph.x == px(0.)),
            "each glyph starts its cluster"
        );
    });
}

/// Man, ZWJ, woman, ZWJ, girl (one grapheme, two cells), `e` with a combining acute (one cell) and a heart with the
/// emoji variation selector (two cells), between `a` and `x`.
const CLUSTERS: &str = "a\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}e\u{301}\u{2764}\u{FE0F}x";

#[test]
fn an_emoji_sequence_and_a_combining_mark_are_one_cluster_each_on_their_columns() {
    let dir = tempfile::tempdir().unwrap();
    let pane = pane(&format!("printf '{CLUSTERS}'; exec sleep 5"), dir.path());
    let frame = wait_text(&pane, "x");
    let layout = layout_row(&frame.rows[0], &frame.colors, &Palette::default(), &RowMarks::default());
    let text: String = layout.runs.iter().map(|run| run.text.as_str()).collect();
    assert_eq!(text, CLUSTERS, "every codepoint reaches the shaper, none twice");
    let clusters: Vec<(usize, u16)> = layout
        .runs
        .iter()
        .scan(0, |offset, run| {
            let base = *offset;
            *offset += run.text.len();
            Some(
                run.clusters
                    .iter()
                    .map(move |cluster| (base + cluster.byte, cluster.col)),
            )
        })
        .flatten()
        .collect();
    // the family's 18 bytes from column 1, é's 3 at column 3, the heart's 6 at 4, x at 6
    assert_eq!(clusters, [(0, 0), (1, 1), (19, 3), (22, 4), (28, 6)]);
}

#[gpui_kit::test]
fn the_grapheme_width_method_reaches_a_new_terminal(cx: &mut TestAppContext) {
    let legacy = |text: &str| {
        let mut loader = ngterm_core::ghostty_config::Loader::new(None);
        loader.load_text("test.conf", text);
        TerminalConfig::from_ghostty(&loader.finish().finalize(false, &[]).config)
    };
    assert_eq!(
        TerminalConfig::default().grapheme_width_method,
        GraphemeWidthMethod::Unicode,
        "Ghostty's default"
    );
    let config = legacy("grapheme-width-method = legacy\nimage-storage-limit = 1000\n");
    assert_eq!(config.grapheme_width_method, GraphemeWidthMethod::Legacy);
    assert_eq!(config.kitty_image_limit, 1000);
    // the family and `x`: the cursor after one two-cell cluster, or after three wide emoji
    cx.executor().allow_parking();
    let script = "printf '\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}x'; exec sleep 5";
    for (config, column) in [(TerminalConfig::default(), 3), (config, 7)] {
        let dir = tempfile::tempdir().unwrap();
        let spawn = spawn_script(script, dir.path());
        let terminal = cx.new(|cx| Terminal::new(spec(), config, spawn, cx));
        let (view, cx) = cx.add_window_view(|_, cx| TerminalView::new(&terminal, ViewRole::Owner, cx));
        wait_painted(&view, cx, "x");
        let cursor = view.read_with(cx, |view, _| view.painted().and_then(|frame| frame.cursor.position));
        assert_eq!(cursor.map(|at| at.column), Some(column));
    }
}

/// `image-storage-limit` reaches the pane when it starts and again on a reload (Ghostty applies it live): with the
/// limit at 0 a Kitty transmission stores nothing.
#[gpui_kit::test]
fn the_image_storage_limit_reaches_the_pane_at_its_start_and_on_a_reload(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let dir = tempfile::tempdir().unwrap();
    // image N (2x1 RGBA, quiet) and `sentN` on each line of input, the first right away
    let script = "n=0; while :; do n=$((n+1)); \
                  printf '\\033_Ga=T,f=32,s=2,v=1,i=%d,q=2;AQIDBAUGBwg=\\033\\\\sent%d ' $n $n; read _; done";
    let off = TerminalConfig {
        kitty_image_limit: 0,
        ..TerminalConfig::default()
    };
    let terminal = cx.new(|cx| Terminal::new(spec(), off.clone(), spawn_script(script, dir.path()), cx));
    let (view, cx) = cx.add_window_view(|_, cx| TerminalView::new(&terminal, ViewRole::Owner, cx));
    let stored = |view: &Entity<TerminalView>, cx: &mut VisualTestContext| -> Vec<u32> {
        view.read_with(cx, |view, _| {
            view.painted()
                .map(|frame| frame.kitty.images.iter().map(|image| image.key.id).collect())
                .unwrap_or_default()
        })
    };
    let send = |terminal: &Entity<Terminal>, cx: &mut VisualTestContext| {
        let pane = terminal
            .read_with(cx, |terminal, _| terminal.shared().pane())
            .expect("a pane");
        pane.write(b"\n").unwrap();
    };
    wait_painted(&view, cx, "sent1");
    assert_eq!(
        stored(&view, cx),
        Vec::<u32>::new(),
        "a pane started at 0 stores no image"
    );

    terminal.update(cx, |terminal, cx| terminal.reload_config(TerminalConfig::default(), cx));
    send(&terminal, cx);
    wait_painted(&view, cx, "sent2");
    assert_eq!(
        stored(&view, cx),
        [2],
        "the default limit, reloaded, stores the next one"
    );

    terminal.update(cx, |terminal, cx| terminal.reload_config(off, cx));
    send(&terminal, cx);
    wait_painted(&view, cx, "sent3");
    assert!(!stored(&view, cx).contains(&3), "a reload to 0 stores no new image");
}

#[gpui_kit::test]
fn sprite_characters_leave_the_font_and_split_the_runs(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    // a ─ ┼ b, a space, a powerline arrow and a braille cell, a space, then a stylized powerline glyph (U+E0C0) the
    // sprites leave to the font
    let script = r"printf 'a\342\224\200\342\224\274b \356\202\260\342\243\277 \356\203\200'; exec sleep 5";
    let terminal = terminal(cx, script, dir.path());
    let (view, cx) = cx.add_window_view(|_, cx| TerminalView::new(&terminal, ViewRole::Owner, cx));
    wait_painted(&view, cx, "a─┼b");
    view.read_with(cx, |view, _| {
        let sprites: Vec<(u16, u32, u16)> = view
            .row_sprites(0)
            .expect("row 0 is laid out")
            .iter()
            .map(|cell| (cell.col, cell.codepoint, cell.cells))
            .collect();
        assert_eq!(
            sprites,
            [(1, 0x2500, 1), (2, 0x253c, 1), (5, 0xe0b0, 1), (6, 0x28ff, 1)]
        );
        let glyph_cols: Vec<u16> = view
            .row_glyphs(0)
            .expect("row 0 is shaped")
            .glyphs
            .iter()
            .map(|glyph| glyph.col)
            .collect();
        assert_eq!(glyph_cols, [0, 3, 8], "only a, b and U+E0C0 are shaped");
    });
}

/// Two views of one terminal side by side, as the deck and the dashboard show it.
struct Pair {
    owner: Entity<TerminalView>,
    viewer: Entity<TerminalView>,
}

impl Render for Pair {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .child(div().w(px(400.)).h(px(300.)).child(self.owner.clone()))
            .child(div().w(px(200.)).h(px(100.)).child(self.viewer.clone()))
    }
}

#[gpui_kit::test]
fn two_views_draw_one_frame_and_only_the_owner_lays_out(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let terminal = terminal(cx, "printf 'shared'; exec sleep 5", dir.path());
    let (pair, cx) = cx.add_window_view(|_, cx| Pair {
        owner: cx.new(|cx| TerminalView::new(&terminal, ViewRole::Owner, cx)),
        viewer: cx.new(|cx| {
            let mut viewer = TerminalView::new(&terminal, ViewRole::Viewer, cx);
            viewer.font_override = Some(6.0);
            viewer
        }),
    });
    cx.simulate_scale_factor_change(1.0);
    let (owner, viewer) = pair.read_with(cx, |pair, _| (pair.owner.clone(), pair.viewer.clone()));
    wait_painted(&owner, cx, "shared");
    wait_painted(&viewer, cx, "shared");
    let shared = terminal.read_with(cx, |terminal, _| Rc::clone(terminal.shared()));
    settle(cx, |_| shared.laid.get().is_some_and(|laid| laid.grid.cell_width == 10));
    let laid = shared.laid.get().expect("the owner laid the grid out");
    // the owner's 400x300 view, not the viewer's smaller one
    assert_eq!((laid.grid.cols, laid.grid.rows), (39, 14));
    let owner_frame = owner.read_with(cx, |view, _| view.painted().cloned()).unwrap();
    let viewer_frame = viewer.read_with(cx, |view, _| view.painted().cloned()).unwrap();
    assert!(Arc::ptr_eq(&owner_frame, &viewer_frame) || owner_frame.seq == viewer_frame.seq);
    // each view shapes for its own font size
    assert!((viewer.read_with(cx, |view, _| view.font_points()) - 6.0).abs() < f32::EPSILON);
    assert!((owner.read_with(cx, |view, _| view.font_points()) - 12.0).abs() < f32::EPSILON);
}

#[gpui_kit::test]
fn a_view_only_terminal_never_starts_its_pane(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let terminal = terminal(cx, "exec sleep 5", dir.path());
    let (_view, cx) = cx.add_window_view(|_, cx| TerminalView::new(&terminal, ViewRole::Viewer, cx));
    cx.run_until_parked();
    let shared = terminal.read_with(cx, |terminal, _| Rc::clone(terminal.shared()));
    assert!(!shared.is_realized());
    assert!(shared.laid.get().is_none());
}

#[gpui_kit::test]
fn the_focused_cursor_blinks_and_an_unfocused_one_does_not(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let terminal = terminal(cx, "printf 'blink'; exec sleep 5", dir.path());
    let (view, cx) = cx.add_window_view(|_, cx| TerminalView::new(&terminal, ViewRole::Owner, cx));
    wait_painted(&view, cx, "blink");
    assert!(
        view.read_with(cx, |view, _| view.painted().is_some_and(|frame| frame.cursor.blinking)),
        "Ghostty's default cursor blinks"
    );
    cx.update(|window, cx| {
        let focus = view.read(cx).focus.clone();
        window.focus(&focus, cx);
    });
    cx.run_until_parked();
    assert!(view.read_with(cx, |view, _| view.blink_on()));
    cx.executor().advance_clock(super::BLINK_INTERVAL);
    cx.run_until_parked();
    assert!(!view.read_with(cx, |view, _| view.blink_on()), "the off phase");
    cx.executor().advance_clock(super::BLINK_INTERVAL);
    cx.run_until_parked();
    assert!(view.read_with(cx, |view, _| view.blink_on()));
    cx.update(Window::blur);
    cx.run_until_parked();
    cx.executor().advance_clock(super::BLINK_INTERVAL * 3);
    cx.run_until_parked();
    assert!(
        view.read_with(cx, |view, _| view.blink_on()),
        "unfocused: shown, hollow"
    );
}

#[gpui_kit::test]
fn a_cursor_a_program_starts_blinking_while_idle_blinks(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    // a steady cursor, then the program asks for a blinking one and goes quiet
    let script = "stty -echo; printf '\\033[2 qsteady'; read _; printf '\\033[1 q'; exec sleep 5";
    let terminal = terminal(cx, script, dir.path());
    let (view, cx) = cx.add_window_view(|_, cx| TerminalView::new(&terminal, ViewRole::Owner, cx));
    cx.update(|window, cx| {
        let focus = view.read(cx).focus.clone();
        window.focus(&focus, cx);
    });
    wait_painted(&view, cx, "steady");
    cx.executor().advance_clock(super::BLINK_INTERVAL);
    cx.run_until_parked();
    assert!(view.read_with(cx, |view, _| view.blink_on()), "steady: no blink");
    let shared = terminal.read_with(cx, |terminal, _| Rc::clone(terminal.shared()));
    shared.pane().unwrap().write(b"\n").unwrap();
    settle(cx, |cx| {
        view.read_with(cx, |view, _| view.painted().is_some_and(|frame| frame.cursor.blinking))
    });
    // no render follows the frame that turned blinking on: prepaint started the timer itself
    cx.executor().advance_clock(super::BLINK_INTERVAL);
    cx.run_until_parked();
    assert!(!view.read_with(cx, |view, _| view.blink_on()), "the off phase");
}

#[gpui_kit::test]
fn the_cursor_takes_the_background_of_an_erased_cell(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    // the cursor's cell is blank but erased with a blue background
    let script = "printf 'ready\\r\\n\\033[44m\\033[K'; exec sleep 5";
    let (_terminal, view, _events, cx) = realized(cx, script, dir.path());
    let frame = view.read_with(cx, |view, _| view.painted().cloned()).unwrap();
    let cursor = frame.cursor.position.unwrap();
    let blue = frame.colors.palette[4];
    let palette = Palette {
        cursor_color: Some(Pick::CellBackground),
        cursor_text: Some(Pick::CellBackground),
        ..Palette::default()
    };
    let (color, text) = cursor_colors(&frame, cursor.column, cursor.row, &palette);
    assert_eq!((color, text), (blue, blue));
}

#[gpui_kit::test]
fn a_cell_change_without_a_font_action_is_reported(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (terminal, _view, events, cx) = realized(cx, "printf 'ready'; exec sleep 5", dir.path());
    events.borrow_mut().clear();
    // a display of another scale
    cx.simulate_scale_factor_change(2.0);
    settle(cx, |_| events.borrow().contains(&TerminalEvent::FontSize));
    let shared = terminal.read_with(cx, |terminal, _| Rc::clone(terminal.shared()));
    assert!((shared.scale() - 2.0).abs() < f32::EPSILON);
}

/// A terminal that never starts its pane (no view lays it out), with `family` as its font.
fn unstarted(cx: &mut TestAppContext, family: &str) -> (Entity<Terminal>, Events) {
    unstarted_at(cx, family, None)
}

/// [`unstarted`] starting at `font_size` (a restored session's, or the size a replacement carries over).
fn unstarted_at(cx: &mut TestAppContext, family: &str, font_size: Option<f32>) -> (Entity<Terminal>, Events) {
    let mut config = TerminalConfig::default();
    config.font.family = vec![family.to_owned()];
    let spawn: Spawn = Box::new(|_| Err(ngterm_term::PaneError::Spawn("never started".into())));
    let spec = TerminalSpec { font_size, ..spec() };
    let terminal = cx.new(|cx| Terminal::new(spec, config, spawn, cx));
    let events: Events = Rc::default();
    let seen = Rc::clone(&events);
    cx.update(|cx| {
        cx.subscribe(&terminal, move |_, event: &TerminalEvent, _| {
            seen.borrow_mut().push(event.clone());
        })
        .detach();
    });
    (terminal, events)
}

#[test]
fn a_config_whose_fonts_change_the_cell_reports_it() {
    let mut cx = real_fonts();
    let (terminal, events) = unstarted(&mut cx, "DejaVu Sans Mono");
    let shared = terminal.read_with(&cx, |terminal, _| Rc::clone(terminal.shared()));
    let mono = shared.cell_size();
    assert_eq!(mono, (10.0, 19.0));
    // the proportional face's widest ASCII advance makes a wider cell
    let mut config = TerminalConfig::default();
    config.font.family = vec!["DejaVu Sans".into()];
    terminal.update(&mut cx, |terminal, cx| terminal.set_config(config.clone(), cx));
    cx.run_until_parked();
    assert!(shared.cell_size().0 > mono.0, "{:?}", shared.cell_size());
    assert_eq!(*events.borrow(), [TerminalEvent::FontSize]);
    // a reload that keeps the cell (another background) reports nothing
    config.theme.background = Rgb::new(1, 2, 3);
    terminal.update(&mut cx, |terminal, cx| terminal.set_config(config, cx));
    cx.run_until_parked();
    assert_eq!(events.borrow().len(), 1);
}

/// `unstarted`'s config at `points`.
fn sized(points: f32) -> TerminalConfig {
    let mut config = TerminalConfig::default();
    config.font.family = vec!["DejaVu Sans Mono".into()];
    config.font.size = points;
    config
}

fn assert_points(actual: f32, expected: f32) {
    assert!((actual - expected).abs() < f32::EPSILON, "{actual} != {expected}");
}

#[test]
fn a_config_reload_resizes_a_terminal_whose_size_the_user_kept() {
    let mut cx = real_fonts();
    let (terminal, events) = unstarted(&mut cx, "DejaVu Sans Mono");
    let shared = terminal.read_with(&cx, |terminal, _| Rc::clone(terminal.shared()));
    let cell = shared.cell_size();
    terminal.update(&mut cx, |terminal, cx| terminal.reload_config(sized(24.0), cx));
    cx.run_until_parked();
    assert_points(shared.font_points(), 24.0);
    assert!(shared.cell_size().0 > cell.0, "{:?}", shared.cell_size());
    assert_eq!(*events.borrow(), [TerminalEvent::FontSize]);
    assert_points(shared.original_font_points(), 24.0);
    // a reset gives the reload the size back
    shared.adjust_font_points(30.0, false);
    shared.adjust_font_points(shared.original_font_points(), true);
    terminal.update(&mut cx, |terminal, cx| terminal.reload_config(sized(16.0), cx));
    assert_points(shared.font_points(), 16.0);
}

#[test]
fn a_config_reload_keeps_the_size_the_user_zoomed_to() {
    let mut cx = real_fonts();
    let (terminal, events) = unstarted(&mut cx, "DejaVu Sans Mono");
    let shared = terminal.read_with(&cx, |terminal, _| Rc::clone(terminal.shared()));
    shared.adjust_font_points(18.0, false);
    cx.run_until_parked();
    events.borrow_mut().clear();
    terminal.update(&mut cx, |terminal, cx| terminal.reload_config(sized(24.0), cx));
    cx.run_until_parked();
    assert_points(shared.font_points(), 18.0);
    assert!(events.borrow().is_empty(), "{:?}", events.borrow());
    // the configured size is what a reset returns to
    assert_points(shared.original_font_points(), 24.0);
}

#[test]
fn a_watermark_change_keeps_a_restored_or_replacement_size() {
    // a restored session's persisted size, and the size a takeover or restart replacement carries over
    for start in [18.0, 20.0] {
        let mut cx = real_fonts();
        let (terminal, events) = unstarted_at(&mut cx, "DejaVu Sans Mono", Some(start));
        let shared = terminal.read_with(&cx, |terminal, _| Rc::clone(terminal.shared()));
        // the app config's size (13) comes with the overlay config a background change applies
        let mut config = sized(13.0);
        config.theme.background = Rgb::new(1, 2, 3);
        terminal.update(&mut cx, |terminal, cx| terminal.set_config(config, cx));
        cx.run_until_parked();
        assert_points(shared.font_points(), start);
        assert_points(shared.original_font_points(), start);
        // no font size change, so nothing is written back into the session
        assert!(events.borrow().is_empty(), "{:?}", events.borrow());
    }
}

#[test]
fn a_config_reload_resizes_a_restored_size_the_user_did_not_step() {
    // the GTK build's explicit reload: libghostty's re-feed resets a start size the user did not step
    let mut cx = real_fonts();
    let (terminal, events) = unstarted_at(&mut cx, "DejaVu Sans Mono", Some(18.0));
    let shared = terminal.read_with(&cx, |terminal, _| Rc::clone(terminal.shared()));
    terminal.update(&mut cx, |terminal, cx| terminal.reload_config(sized(13.0), cx));
    cx.run_until_parked();
    assert_points(shared.font_points(), 13.0);
    assert_eq!(*events.borrow(), [TerminalEvent::FontSize]);
}

#[test]
fn an_appearance_flip_keeps_a_restored_or_replacement_size() {
    // upstream's `reloadConfigPreservingSessionZoom`, the GTK build's `keep_zoom`: the flip's reload keeps the live
    // size without marking it stepped, so a later explicit reload still takes the configured one
    let mut cx = real_fonts();
    let (terminal, events) = unstarted_at(&mut cx, "DejaVu Sans Mono", Some(18.0));
    let shared = terminal.read_with(&cx, |terminal, _| Rc::clone(terminal.shared()));
    terminal.update(&mut cx, |terminal, cx| terminal.flip_config(sized(13.0), cx));
    cx.run_until_parked();
    assert_points(shared.font_points(), 18.0);
    assert!(events.borrow().is_empty(), "{:?}", events.borrow());
    // the configured size is what a reset returns to
    assert_points(shared.original_font_points(), 13.0);
    terminal.update(&mut cx, |terminal, cx| terminal.reload_config(sized(13.0), cx));
    assert_points(shared.font_points(), 13.0);
}

#[test]
fn an_appearance_flip_and_a_reload_keep_the_size_the_user_zoomed_to() {
    let mut cx = real_fonts();
    let (terminal, _events) = unstarted(&mut cx, "DejaVu Sans Mono");
    let shared = terminal.read_with(&cx, |terminal, _| Rc::clone(terminal.shared()));
    shared.adjust_font_points(18.0, false);
    terminal.update(&mut cx, |terminal, cx| terminal.flip_config(sized(13.0), cx));
    assert_points(shared.font_points(), 18.0);
    terminal.update(&mut cx, |terminal, cx| terminal.reload_config(sized(13.0), cx));
    assert_points(shared.font_points(), 18.0);
    assert_points(shared.original_font_points(), 13.0);
}

// ---------------------------------------------------------------------------------------------------------------
// the Surface

/// The contract `ngterm_host::fakes::FakeSurface` pins: nothing works, and nothing claims to, before the terminal is
/// realized.
#[gpui_kit::test]
fn an_unrealized_surface_does_nothing(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let terminal = terminal(cx, "exec sleep 5", dir.path());
    let mut surface = terminal.read_with(cx, |terminal, _| terminal.surface());
    assert_eq!(surface.pane_token(), "pane-token");
    assert!(!surface.is_realized());
    assert!(!surface.is_live_session());
    assert!(!surface.type_text("x"));
    assert_eq!(surface.read_text(TextScope::Screen), None);
    assert_eq!(surface.read_selection(), None);
    assert_eq!(surface.cursor_column(), None);
    assert!(!surface.binding_action("select_all"));
    assert_eq!(surface.foreground_pid(), None);
    assert_eq!(surface.font_size(), None);
    assert_eq!(surface.cell_size(), None);
    surface.promote_to_primary();
    Box::new(surface).teardown();
    assert!(terminal.read_with(cx, |terminal, _| terminal.shared().is_torn_down()));
}

type Events = Rc<std::cell::RefCell<Vec<TerminalEvent>>>;

/// A terminal running `script` in a 640x480 test window, laid out and drawing `ready`; its events collected.
fn realized<'a>(
    cx: &'a mut TestAppContext,
    script: &str,
    dir: &Path,
) -> (
    Entity<Terminal>,
    Entity<TerminalView>,
    Events,
    &'a mut VisualTestContext,
) {
    let terminal = terminal(cx, script, dir);
    let events: Events = Rc::default();
    let seen = Rc::clone(&events);
    cx.update(|cx| {
        cx.subscribe(&terminal, move |_, event: &TerminalEvent, _| {
            seen.borrow_mut().push(event.clone());
        })
        .detach();
    });
    let (view, cx) = cx.add_window_view(|_, cx| TerminalView::new(&terminal, ViewRole::Owner, cx));
    cx.simulate_scale_factor_change(1.0);
    cx.simulate_resize(size(px(640.), px(480.)));
    wait_painted(&view, cx, "ready");
    (terminal, view, events, cx)
}

fn surface(terminal: &Entity<Terminal>, cx: &mut VisualTestContext) -> Box<dyn Surface> {
    Box::new(terminal.read_with(cx, |terminal, _| terminal.surface()))
}

#[gpui_kit::test]
fn a_realized_surface_types_and_reads_back(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (terminal, view, events, cx) = realized(cx, "printf 'ready\\r\\n'; exec cat", dir.path());
    let mut surface = surface(&terminal, cx);
    assert!(surface.is_realized());
    assert!(!surface.is_live_session(), "a plain pane");
    assert!(events.borrow().contains(&TerminalEvent::Spawned));
    // keystrokes, not a paste: the line ending is a Return the tty turns into a newline
    assert!(surface.type_text("hello\n"));
    wait_painted(&view, cx, "hello\nhello");
    assert_eq!(surface.cursor_column(), Some(0));
    let screen = surface.read_text(TextScope::Screen).unwrap();
    assert!(screen.starts_with("ready\nhello\nhello"), "{screen:?}");
    assert!(surface.read_text(TextScope::All).unwrap().contains("hello"));
    assert!(surface.foreground_pid().is_some());
    assert_eq!(surface.read_selection(), None);
    assert!(surface.binding_action("select_all"));
    assert!(surface.read_selection().is_some_and(|text| text.contains("ready")));
}

#[gpui_kit::test]
fn font_actions_change_the_size_and_the_cell_at_once(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (terminal, _view, events, cx) = realized(cx, "printf 'ready'; exec cat", dir.path());
    let mut surface = surface(&terminal, cx);
    assert_eq!(surface.font_size(), Some(12.0));
    assert_eq!(surface.cell_size(), Some((10.0, 21.0)));
    assert!(surface.binding_action("increase_font_size:12"));
    assert_eq!(surface.font_size(), Some(24.0));
    assert_eq!(surface.cell_size(), Some((19.0, 42.0)));
    assert!(surface.binding_action("set_font_size:300"));
    assert_eq!(surface.font_size(), Some(255.0), "clamped as Ghostty clamps");
    assert!(surface.binding_action("reset_font_size"));
    assert_eq!(surface.font_size(), Some(12.0));
    cx.run_until_parked();
    assert!(events.borrow().contains(&TerminalEvent::FontSize));
    // the owner lays the grid out again at the new size
    assert!(surface.binding_action("set_font_size:24"));
    let shared = terminal.read_with(cx, |terminal, _| Rc::clone(terminal.shared()));
    settle(cx, |_| shared.laid.get().is_some_and(|laid| laid.grid.cell_width == 19));
}

fn perform(terminal: &Entity<Terminal>, cx: &mut VisualTestContext, action: &str) -> bool {
    let action = Action::from_str(action).expect("a valid action");
    terminal
        .read_with(cx, |terminal, _| terminal.surface())
        .perform(&action)
}

/// Each action and whether it must be performed, in order.
fn expect_performed(terminal: &Entity<Terminal>, cx: &mut VisualTestContext, cases: &[(&str, bool)]) {
    for (action, performed) in cases {
        assert_eq!(perform(terminal, cx, action), *performed, "{action}");
    }
}

fn clipboard_text(cx: &mut VisualTestContext) -> String {
    cx.read_from_clipboard()
        .and_then(|item| item.text())
        .unwrap_or_default()
}

fn primary_text(cx: &mut VisualTestContext) -> String {
    cx.update(|_, cx| cx.read_from_primary())
        .and_then(|item| item.text())
        .unwrap_or_default()
}

#[gpui_kit::test]
fn search_actions_report_performed_as_libghostty_did(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (terminal, view, events, cx) = realized(cx, "printf 'ready'; exec cat", dir.path());
    // nothing open: neither is performed, so the default `performable:escape=end_search` lets Escape through
    expect_performed(
        &terminal,
        cx,
        &[
            ("end_search", false),
            ("search:", false),
            ("search:ready", true),
            ("navigate_search:next", true),
            ("end_search", true),
            ("end_search", false),
            ("navigate_search:next", false),
            ("search_selection", false),
            ("select_all", true),
            ("search_selection", true),
        ],
    );
    // the Surface accepts any action of a realized terminal, an unknown one too
    let mut surface = surface(&terminal, cx);
    for action in ["start_search", "end_search", "not_an_action"] {
        assert!(surface.binding_action(action), "{action}");
    }
    cx.run_until_parked();
    let seen = events.borrow().clone();
    for event in [
        TerminalEvent::StartSearch("ready".into()),
        TerminalEvent::StartSearch(String::new()),
        TerminalEvent::EndSearch,
    ] {
        assert!(seen.contains(&event), "{event:?} in {seen:?}");
    }
    teardown_kills_the_shell(&terminal, &view, surface, cx);
}

fn teardown_kills_the_shell(
    terminal: &Entity<Terminal>,
    view: &Entity<TerminalView>,
    surface: Box<dyn Surface>,
    cx: &mut VisualTestContext,
) {
    let shared = terminal.read_with(cx, |terminal, _| Rc::clone(terminal.shared()));
    let pid = shared.pane().and_then(|pane| pane.pid()).unwrap();
    surface.teardown();
    assert!(shared.is_torn_down());
    assert!(shared.pane().is_none());
    cx.run_until_parked();
    // the child got SIGHUP with the pane and is gone (reaped by the pane's waiter)
    let deadline = Instant::now() + DEADLINE;
    while Path::new(&format!("/proc/{pid}")).exists() {
        assert!(Instant::now() < deadline, "the shell outlived its terminal");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        view.read_with(cx, |view, _| view.painted().is_none()),
        "a torn-down terminal draws nothing"
    );
}

/// Performs `action`, then the clipboard's text.
fn copied(terminal: &Entity<Terminal>, cx: &mut VisualTestContext, action: &str) -> String {
    assert!(perform(terminal, cx, action), "{action}");
    cx.run_until_parked();
    clipboard_text(cx)
}

#[gpui_kit::test]
fn copy_actions_follow_their_formats(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (terminal, _view, _events, cx) = realized(cx, "printf 'ready \\033[31mred\\033[0m'; exec cat", dir.path());
    assert!(!perform(&terminal, cx, "copy_to_clipboard"), "no selection");
    assert!(perform(&terminal, cx, "select_all"));
    cx.run_until_parked();
    // select_all copies to the selection clipboard, as copy-on-select does
    assert!(primary_text(cx).starts_with("ready red"));
    assert!(copied(&terminal, cx, "copy_to_clipboard:plain").starts_with("ready red"));
    assert!(
        copied(&terminal, cx, "copy_to_clipboard:vt").contains("\x1b["),
        "VT sequences kept"
    );
    assert!(
        copied(&terminal, cx, "copy_to_clipboard:mixed").starts_with("ready red"),
        "its plain text"
    );
    // HTML is offered as text/html alone (the Linux platform's patch); GPUI's test platform keeps the item's text,
    // which is the markup then
    let html = copied(&terminal, cx, "copy_to_clipboard:html");
    assert!(html.contains('<') && html.contains("red"), "{html}");
}

/// Takes `copy-on-select` and the trim, runs `select_all`, and reads both clipboards back.
fn select_all_copies(
    terminal: &Entity<Terminal>,
    cx: &mut VisualTestContext,
    copy_on_select: CopyOnSelect,
    trim: bool,
) -> (String, String) {
    let config = TerminalConfig {
        copy_on_select,
        copy_trim: trim,
        ..TerminalConfig::default()
    };
    terminal.update(cx, |terminal, cx| terminal.set_config(config, cx));
    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string("standard".into()));
    cx.update(|_, cx| cx.write_to_primary(gpui_kit::ClipboardItem::new_string("primary".into())));
    assert!(perform(terminal, cx, "select_all"));
    cx.run_until_parked();
    (clipboard_text(cx), primary_text(cx))
}

#[gpui_kit::test]
fn select_all_copies_per_copy_on_select(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (terminal, _view, _events, cx) = realized(cx, "printf 'ready  \\r\\nnext'; exec cat", dir.path());
    assert_eq!(
        select_all_copies(&terminal, cx, CopyOnSelect::None, true),
        ("standard".to_owned(), "primary".to_owned())
    );
    assert_eq!(
        select_all_copies(&terminal, cx, CopyOnSelect::Primary, true),
        ("standard".to_owned(), "ready\nnext".to_owned())
    );
    // `clipboard` copies to the system clipboard alone (Ghostty 1.4)
    assert_eq!(
        select_all_copies(&terminal, cx, CopyOnSelect::Clipboard, true),
        ("ready\nnext".to_owned(), "primary".to_owned())
    );
    // `both` copies to both; without the trim the trailing blanks stay
    assert_eq!(
        select_all_copies(&terminal, cx, CopyOnSelect::Both, false),
        ("ready  \nnext".to_owned(), "ready  \nnext".to_owned())
    );
}

#[gpui_kit::test]
fn search_selection_takes_the_selection_untrimmed(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (terminal, _view, events, cx) = realized(cx, "printf 'ready  x'; exec cat", dir.path());
    let shared = terminal.read_with(cx, |terminal, _| Rc::clone(terminal.shared()));
    let pane = shared.pane().unwrap();
    pane.select(GridPoint::viewport(0, 0), GridPoint::viewport(6, 0), false)
        .unwrap();
    assert!(perform(&terminal, cx, "search_selection"));
    cx.run_until_parked();
    assert!(events.borrow().contains(&TerminalEvent::StartSearch("ready  ".into())));
}

#[gpui_kit::test]
fn the_url_and_title_copies_follow_the_gtk_build(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (terminal, _view, _events, cx) = realized(cx, "printf 'ready'; exec cat", dir.path());
    // neither the GTK build nor upstream copies the title; a URL only with a link under the pointer
    assert!(!perform(&terminal, cx, "copy_title_to_clipboard"));
    assert!(!perform(&terminal, cx, "copy_url_to_clipboard"));
    let shared = terminal.read_with(cx, |terminal, _| Rc::clone(terminal.shared()));
    shared.set_hovered_url(Some("https://example.org/".into()));
    assert_eq!(copied(&terminal, cx, "copy_url_to_clipboard"), "https://example.org/");
}

/// The screen file a `write_*_file:copy` put its path of on the clipboard, by name, with its text; its directory is
/// removed.
fn take_screen_file(cx: &mut VisualTestContext) -> (String, String) {
    cx.run_until_parked();
    let path = std::path::PathBuf::from(clipboard_text(cx));
    let text = std::fs::read_to_string(&path).expect("the screen file exists");
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "readable by the user only");
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    (name, text)
}

/// The path of the first `OpenUrl` event.
fn opened(events: &Events) -> Option<std::path::PathBuf> {
    events.borrow().iter().find_map(|event| match event {
        TerminalEvent::OpenUrl(path) => Some(std::path::PathBuf::from(path)),
        _ => None,
    })
}

#[gpui_kit::test]
fn screen_files_are_written_and_copied(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (terminal, _view, events, cx) = realized(cx, "printf 'ready \\033[31mred\\033[0m'; exec cat", dir.path());
    assert!(perform(&terminal, cx, "write_screen_file:copy"));
    let (name, text) = take_screen_file(cx);
    assert_eq!(name, "screen.txt");
    assert!(text.starts_with("ready red"), "{text:?}");
    assert!(perform(&terminal, cx, "write_screen_file:copy,html"));
    let (name, text) = take_screen_file(cx);
    assert_eq!(name, "screen.html");
    assert!(text.contains('<'));
    // no scrollback and no selection: nothing to write, performed all the same
    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string("kept".into()));
    assert!(perform(&terminal, cx, "write_scrollback_file:copy"));
    assert!(perform(&terminal, cx, "write_selection_file:open"));
    cx.run_until_parked();
    assert_eq!(clipboard_text(cx), "kept");
    assert_eq!(opened(&events), None);
}

#[gpui_kit::test]
fn screen_files_are_opened_and_pasted(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (terminal, view, events, cx) = realized(cx, "printf 'ready'; exec cat", dir.path());
    // open hands the file to the frontend's opener; paste types its path into the program (shown by the tty's echo:
    // with no newline after it, `cat` has not read it yet)
    assert!(perform(&terminal, cx, "select_all"));
    assert!(perform(&terminal, cx, "write_selection_file:open"));
    cx.run_until_parked();
    let path = opened(&events).expect("an open request");
    assert_eq!(path.file_name().unwrap(), "selection.txt");
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    assert!(perform(&terminal, cx, "write_screen_file:paste"));
    wait_painted(&view, cx, "screen.txt");
    let painted = painted_text(&view, cx);
    // typed right after the program's "ready"
    let word = painted
        .split_whitespace()
        .find(|word| word.ends_with("screen.txt"))
        .unwrap();
    let pasted = Path::new(&word[word.find('/').unwrap()..]);
    assert!(std::fs::read_to_string(pasted).unwrap().starts_with("ready"));
    std::fs::remove_dir_all(pasted.parent().unwrap()).unwrap();
}

#[gpui_kit::test]
fn scroll_selection_and_mouse_actions(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (terminal, view, _events, cx) = realized(cx, "seq 1 40; printf 'ready'; exec cat", dir.path());
    expect_performed(
        &terminal,
        cx,
        &[
            ("adjust_selection:left", false),
            ("scroll_to_selection", false),
            ("scroll_to_row:0", true),
        ],
    );
    settle(cx, |cx| painted_text(&view, cx).starts_with("1\n2"));
    expect_performed(
        &terminal,
        cx,
        &[
            ("scroll_to_bottom", true),
            ("select_all", true),
            ("adjust_selection:left", true),
            ("scroll_to_selection", true),
            ("toggle_mouse_reporting", true),
        ],
    );
    settle(cx, |cx| painted_text(&view, cx).starts_with("1\n2"));
    let shared = terminal.read_with(cx, |terminal, _| Rc::clone(terminal.shared()));
    assert!(!shared.pane().unwrap().mouse_reporting());
}

#[gpui_kit::test]
fn read_only_keeps_the_users_input_from_the_program(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (terminal, view, events, cx) = realized(cx, "stty -echo; printf 'ready'; exec cat", dir.path());
    let shared = terminal.read_with(cx, |terminal, _| Rc::clone(terminal.shared()));
    assert!(perform(&terminal, cx, "toggle_readonly"));
    assert!(shared.is_readonly());
    let mut surface = surface(&terminal, cx);
    assert!(surface.type_text("typed\n"), "accepted, as libghostty's key path was");
    assert!(perform(&terminal, cx, "text:bound"), "performed, its bytes dropped");
    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string("pasted".into()));
    assert!(perform(&terminal, cx, "paste_from_clipboard"));
    cx.run_until_parked();
    assert!(perform(&terminal, cx, "toggle_readonly"));
    assert!(surface.type_text("after\n"));
    wait_painted(&view, cx, "after");
    let painted = painted_text(&view, cx);
    assert!(
        !["typed", "bound", "pasted"].iter().any(|text| painted.contains(text)),
        "{painted}"
    );
    let seen = events.borrow().clone();
    assert!(seen.contains(&TerminalEvent::Readonly(true)) && seen.contains(&TerminalEvent::Readonly(false)));
}

#[gpui_kit::test]
fn clear_screen_and_reset(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (terminal, view, _events, cx) = realized(cx, "seq 1 40; printf 'ready'; exec cat", dir.path());
    // clear_screen drops the scrollback and the rows above the cursor; reset wipes the terminal
    assert!(perform(&terminal, cx, "clear_screen"));
    settle(cx, |cx| painted_text(&view, cx).trim_end() == "ready");
    let surface = surface(&terminal, cx);
    let all = surface.read_text(TextScope::All).unwrap_or_default();
    assert_eq!(all.trim_end(), "ready");
    assert!(perform(&terminal, cx, "reset"));
    settle(cx, |cx| painted_text(&view, cx).trim().is_empty());
}

#[gpui_kit::test]
fn clear_screen_is_not_performed_on_the_alternate_screen(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (terminal, _view, _events, cx) = realized(cx, "printf '\\033[?1049hready'; exec cat", dir.path());
    assert!(!perform(&terminal, cx, "clear_screen"));
}

#[gpui_kit::test]
fn paste_and_copy_go_through_the_clipboard(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (terminal, view, events, cx) = realized(cx, "printf 'ready\\r\\n'; exec cat", dir.path());
    let mut surface = surface(&terminal, cx);
    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string("pasted".into()));
    assert!(surface.binding_action("paste_from_clipboard"));
    wait_painted(&view, cx, "pasted");
    assert!(surface.binding_action("select_all"));
    assert!(surface.binding_action("copy_to_clipboard"));
    cx.run_until_parked();
    let copied = cx
        .read_from_clipboard()
        .and_then(|item| item.text())
        .unwrap_or_default();
    assert!(copied.contains("ready") && copied.contains("pasted"), "{copied:?}");
    // a paste that could run a command goes through as confirmed, as upstream and the GTK build complete it
    events.borrow_mut().clear();
    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string("echo unsafe-pasted\n".into()));
    assert!(surface.binding_action("paste_from_clipboard"));
    wait_painted(&view, cx, "unsafe-pasted");
}

#[gpui_kit::test]
fn a_config_reload_reaches_the_pane_and_the_views(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    // OSC 10 overrides the foreground; the theme's background still comes through
    let script = "printf 'ready\\033]10;rgb:01/02/03\\007'; exec cat";
    let (terminal, view, _events, cx) = realized(cx, script, dir.path());
    let foreground = Rgb::new(1, 2, 3);
    let painted =
        |view: &Entity<TerminalView>, cx: &mut VisualTestContext| view.read_with(cx, |view, _| view.painted().cloned());
    settle(cx, |cx| {
        painted(&view, cx).is_some_and(|frame| frame.colors.foreground == foreground)
    });
    let shared = terminal.read_with(cx, |terminal, _| Rc::clone(terminal.shared()));
    let generation = shared.font_generation();
    let mut config = GhosttyConfig::default();
    config.colors.background = ngterm_core::chrome::Rgb::new(9, 8, 7);
    config.font.family = vec!["Some Other Mono".into()];
    terminal.update(cx, |terminal, cx| {
        terminal.set_config(TerminalConfig::from_ghostty(&config), cx);
    });
    settle(cx, |cx| {
        painted(&view, cx).is_some_and(|frame| frame.colors.background == Rgb::new(9, 8, 7))
    });
    let frame = painted(&view, cx).unwrap();
    assert_eq!(
        frame.colors.foreground, foreground,
        "the program's override outlives the reload"
    );
    assert_eq!(
        shared.font_generation(),
        generation + 1,
        "a font change reshapes every view"
    );
    assert_eq!(shared.fonts().regular.family.as_ref(), "Some Other Mono");
}

#[test]
fn a_surface_is_the_hosts_surface() {
    fn assert_surface<T: Surface>() {}
    assert_surface::<TerminalSurface>();
}

/// Ghostty's `FreeType` face: a zero `yStrikeoutSize` is a broken OS/2 table (the thickness estimated), and so is a zero
/// position along with it; a nonzero position stands alone.
#[test]
fn a_broken_os2_strikeout_falls_back_to_ghosttys_estimates() {
    let scaled = |units: f32| f64::from(units) / 64.0;
    assert_eq!(metrics::strikethrough(320.0, 48.0, scaled), (Some(5.0), Some(0.75)));
    assert_eq!(metrics::strikethrough(0.0, 0.0, scaled), (None, None));
    assert_eq!(metrics::strikethrough(320.0, 0.0, scaled), (Some(5.0), None));
}
