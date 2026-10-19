//! Wide and multi-codepoint text through a real PTY: an emoji ZWJ sequence, a combining mark and an emoji variation
//! selector each take the cells of their grapheme cluster (Ghostty's `grapheme-width-method = unicode`, mode 2027 on
//! as the terminal's default, a reset keeping it), the cursor moves past them by those cells, and read-back and
//! selections return whole clusters; with `legacy` each codepoint counts on its own.
//!
//! Unix only: the children are POSIX shell scripts whose output the tests pin byte for byte, which Windows'
//! pseudoconsole re-renders; `platform.rs` runs a pane's launch, echo, resize, exit, selection and search there.
#![cfg(unix)]

#[expect(dead_code, reason = "the file helpers serve the other test files")]
mod support;

use agterm_term::{CellWidth, CursorPosition, GridPoint, PaneConfig, SelectionText, TextRegion};
use support::{SIZE, TestResult, spawn, spawn_with, wait_frame, wait_text};

/// Man, ZWJ, woman, ZWJ, girl: one grapheme two cells wide.
const FAMILY: &str = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
/// `e` and a combining acute accent: one cell.
const E_ACUTE: &str = "e\u{301}";
/// A heavy black heart with the emoji variation selector: two cells.
const HEART: &str = "\u{2764}\u{FE0F}";

/// `a`, the three clusters and `x` on row 0, then `printf`'s `rest`.
fn script(rest: &str) -> String {
    format!("printf 'a{FAMILY}{E_ACUTE}{HEART}x{rest}'; exec sleep 30")
}

#[test]
fn each_cluster_takes_its_width_and_the_cursor_moves_past_it() -> TestResult {
    let fixture = spawn(&script(""))?;
    let pane = &fixture.pane;
    let frame = wait_text(pane, "x");
    let row = &frame.rows[0];
    let cells = row.cells();
    let texts: Vec<String> = (0..7).map(|col| row.text(col).to_owned()).collect();
    assert_eq!(texts, ["a", FAMILY, "", E_ACUTE, HEART, "", "x"]);
    let widths: Vec<CellWidth> = cells[..7].iter().map(|cell| cell.width).collect();
    assert_eq!(
        widths,
        [
            CellWidth::Narrow,
            CellWidth::Wide,
            CellWidth::SpacerTail,
            CellWidth::Narrow,
            CellWidth::Wide,
            CellWidth::SpacerTail,
            CellWidth::Narrow,
        ]
    );
    assert_eq!(pane.cursor_column(), 7);
    assert_eq!(
        frame.cursor.position,
        Some(CursorPosition {
            column: 7,
            row: 0,
            wide_tail: false,
        })
    );
    assert_eq!(
        pane.read_text(TextRegion::Viewport)?,
        format!("a{FAMILY}{E_ACUTE}{HEART}x")
    );
    Ok(())
}

#[test]
fn a_selection_over_any_cell_of_a_cluster_takes_the_whole_cluster() -> TestResult {
    let fixture = spawn(&script(""))?;
    let pane = &fixture.pane;
    wait_text(pane, "x");
    // the family's spacer alone, then the head alone
    for col in [2, 1] {
        pane.select(GridPoint::viewport(col, 0), GridPoint::viewport(col, 0), false)?;
        assert_eq!(
            pane.selection_text(SelectionText::Raw).as_deref(),
            Some(FAMILY),
            "{col}"
        );
    }
    // from the spacer of the family to the heart's spacer
    pane.select(GridPoint::viewport(2, 0), GridPoint::viewport(5, 0), false)?;
    assert_eq!(
        pane.selection_text(SelectionText::Copy).as_deref(),
        Some(format!("{FAMILY}{E_ACUTE}{HEART}").as_str())
    );
    // a word selection keeps the clusters with their neighbours
    assert!(pane.select_word(GridPoint::viewport(3, 0))?);
    assert_eq!(
        pane.selection_text(SelectionText::Raw).as_deref(),
        Some(format!("a{FAMILY}{E_ACUTE}{HEART}x").as_str())
    );
    Ok(())
}

#[test]
fn the_cursor_on_a_wide_characters_tail_says_so() -> TestResult {
    // the cursor put on column 3 (one-based), the family's spacer
    let fixture = spawn(&script("\\033[3G"))?;
    let frame = wait_frame(&fixture.pane, |frame| {
        frame.rows[0].line().ends_with('x') && frame.cursor.position.is_some_and(|at| at.column < 7)
    });
    let position = frame.cursor.position.ok_or("no cursor")?;
    assert_eq!((position.column, position.wide_tail), (2, true));
    Ok(())
}

#[test]
fn a_reset_keeps_clustering_and_legacy_counts_each_codepoint() -> TestResult {
    // a full reset (RIS) by the program restores the configured default, not the library's
    let reset = spawn(&format!("printf '\\033c'; {}", script("")))?;
    wait_text(&reset.pane, "x");
    assert_eq!(reset.pane.cursor_column(), 7);

    let legacy = PaneConfig {
        grapheme_clusters: false,
        ..PaneConfig::new(SIZE)
    };
    let fixture = spawn_with(&format!("printf '{FAMILY}x'; exec sleep 30"), &legacy)?;
    let frame = wait_text(&fixture.pane, "x");
    // three wide emoji, each ZWJ joined to the one before it
    assert_eq!(fixture.pane.cursor_column(), 7);
    let row = &frame.rows[0];
    assert_eq!(row.text(0), "\u{1F468}\u{200D}");
    assert_eq!(row.text(2), "\u{1F469}\u{200D}");
    assert_eq!(row.text(4), "\u{1F467}");
    assert_eq!(row.text(6), "x");
    Ok(())
}
