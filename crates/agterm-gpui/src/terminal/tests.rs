//! Headless tests of the terminal (plan > Task 13): cell metrics for a known font, the grid's geometry, row plans
//! (run splitting by style, wide characters, marks, decorations) and their cache, cell and cursor colors, the element
//! in GPUI's test windows over a real pane (spawn at the laid-out grid, glyph placement, two views of one pane, the
//! cursor's blink), and the [`agterm_host::Surface`] contract the fakes pin.
//!
//! Panes run `/bin/sh` scripts with no `AGTERM_*` or `ZMX_*` variable inherited (CLAUDE.md > Never touch the user's
//! running agterm). GPUI's test platform draws nothing to a screen; its default text system is a fixed fake (1000
//! units per em, every ASCII advance 600, ascent 1025, descent -275), the real-font test uses GPUI's Linux text system.

use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agterm_core::ghostty_config::{GhosttyConfig, PaddingBalance, TerminalColor, WindowPadding};
use agterm_host::{Surface, TextScope};
use agterm_term::{
    Cell, CellFlags, Colors, CursorPosition, CursorStyle, Frame, GridSize, Pane, PaneCommand, PaneConfig, Rgb,
    Underline,
};
use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _, TestAppContext,
    TestDispatcher, VisualTestContext, Window, div, px, size,
};

use super::colors::{self, Mark, Palette, Pick};
use super::element::{cursor_shape, lock_cursor};
use super::font::{self, Fonts};
use super::metrics::{self, CellMetrics, FaceMetrics, Geometry, Padding};
use super::row_cache::{Cluster, Line, RowCache, RowMarks, Span, layout_row, search_marks};
use super::{Spawn, Terminal, TerminalConfig, TerminalEvent, TerminalSpec, TerminalSurface, TerminalView, ViewRole};

const DEADLINE: Duration = Duration::from_secs(20);

// ---------------------------------------------------------------------------------------------------------------
// panes

/// `sh -c script` with the inherited `AGTERM_*`/`ZMX_*` variables removed, in `dir`.
fn command(script: &str, dir: &Path) -> PaneCommand {
    let mut command = PaneCommand::new("/bin/sh").arg("-c").arg(script).cwd(dir);
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("AGTERM_") || name.starts_with("ZMX_") {
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

/// `DejaVu Sans Mono`'s file, as fontconfig finds it.
fn dejavu_sans_mono() -> Vec<u8> {
    let output = std::process::Command::new("fc-match")
        .args(["-f", "%{file}", "DejaVu Sans Mono:style=Book"])
        .output()
        .expect("fc-match runs (fontconfig is installed)");
    let path = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        path.contains("DejaVuSansMono"),
        "DejaVu Sans Mono is not installed (fonts-dejavu-core); fontconfig gave {path:?}"
    );
    std::fs::read(&path).expect("the font file reads")
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
    let config = agterm_core::ghostty_config::Font {
        family: vec!["Missing Mono".into(), "Hack".into(), "DejaVu Sans Mono".into()],
        family_bold: vec!["Liberation Mono".into()],
        feature: vec!["-calt".into(), "ss01, liga=0".into(), "zero on".into(), "bogus".into()],
        ..agterm_core::ghostty_config::Font::default()
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
    let unset = Fonts::resolve(&agterm_core::ghostty_config::Font::default(), &installed);
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
    config.selection.background = Some(TerminalColor::Color(agterm_core::chrome::Rgb::new(1, 2, 3)));
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
    let state = agterm_term::SearchState {
        needle: "x".into(),
        highlights: vec![agterm_term::Highlight {
            start: (5, 0),
            end: (2, 2),
        }],
        selected_highlight: Some(agterm_term::Highlight {
            start: (0, 3),
            end: (1, 3),
        }),
        ..agterm_term::SearchState::default()
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
fn the_cursor_shape_follows_focus_blink_and_visibility() {
    let mut cursor = agterm_term::Cursor {
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
    let mut cursor = agterm_term::Cursor {
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
    let terminal = terminal(cx, "printf 'ready'; read _; stty size; exec cat", dir.path());
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
    shared.pane().unwrap().write(b"stty size\n").unwrap();
    wait_painted(&view, cx, "stty size");
    let painted = painted_text(&view, cx);
    assert!(painted.contains("ready"), "{painted}");
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

// ---------------------------------------------------------------------------------------------------------------
// the Surface

/// The contract `agterm_host::fakes::FakeSurface` pins: nothing works, and nothing claims to, before the terminal is
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

#[gpui_kit::test]
fn search_actions_and_teardown(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (terminal, view, events, cx) = realized(cx, "printf 'ready'; exec cat", dir.path());
    let mut surface = surface(&terminal, cx);
    // an unknown action is still accepted, as libghostty's were
    for action in ["search:ready", "start_search", "end_search", "not_an_action"] {
        assert!(surface.binding_action(action), "{action}");
    }
    cx.run_until_parked();
    assert!(events.borrow().contains(&TerminalEvent::StartSearch));
    assert!(events.borrow().contains(&TerminalEvent::EndSearch));
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
    // a paste that could run a command is refused and reported for confirmation
    events.borrow_mut().clear();
    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string("rm -rf x\n".into()));
    assert!(surface.binding_action("paste_from_clipboard"));
    cx.run_until_parked();
    assert_eq!(*events.borrow(), [TerminalEvent::UnsafePaste("rm -rf x\n".into())]);
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
    config.colors.background = agterm_core::chrome::Rgb::new(9, 8, 7);
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
