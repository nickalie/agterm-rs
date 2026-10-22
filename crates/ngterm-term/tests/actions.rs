//! The terminal-scoped binding actions over a real PTY: `clear_screen` (off and at a prompt, the alternate screen),
//! `reset`, `adjust_selection`, `scroll_to_selection`, `scroll_to_row` and the formatter's regions and formats.
//!
//! Unix only: the children are POSIX shell scripts whose output the tests pin byte for byte, which Windows'
//! pseudoconsole re-renders; `platform.rs` runs a pane's launch, echo, resize, exit, selection and search there.
#![cfg(unix)]

#[expect(dead_code, reason = "some helpers serve the other test files")]
mod support;

use ngterm_term::{FormatRegion, GridPoint, Scroll, SelectionAdjust, TextFormat};
use support::{spawn, wait_file_containing, wait_frame, wait_text};

#[test]
fn clear_screen_off_a_prompt_drops_the_rows_above_and_the_scrollback() {
    let fixture = spawn("seq 1 40; printf 'last'; exec sleep 5").unwrap();
    wait_text(&fixture.pane, "last");
    assert!(
        fixture
            .pane
            .format(FormatRegion::History, TextFormat::Plain, true)
            .is_some()
    );
    assert!(fixture.pane.clear_screen());
    let frame = wait_frame(&fixture.pane, |frame| frame.lines()[0] == "last");
    assert!(frame.lines()[1..].iter().all(String::is_empty), "{:?}", frame.lines());
    // the cursor followed its row to the top, its column kept
    let cursor = frame.cursor.position.unwrap();
    assert_eq!((cursor.row, cursor.column), (0, 4));
    assert_eq!(
        fixture.pane.format(FormatRegion::History, TextFormat::Plain, true),
        None,
        "no scrollback"
    );
    // the program's output goes on where the cursor is
    let text = fixture.pane.read_text(ngterm_term::TextRegion::All).unwrap();
    assert!(text.starts_with("last"), "{text:?}");
}

#[test]
fn clear_screen_at_a_prompt_erases_everything_and_asks_the_shell_to_repaint() {
    // a shell-like program: a marked prompt, then it reads one byte raw and records it
    let fixture = spawn(
        "stty raw -echo; printf 'old output\\r\\n\\033]133;A\\007$ '; head -c1 | od -An -c > \"$DIR/got\"; exec sleep 5",
    )
    .unwrap();
    wait_text(&fixture.pane, "$");
    assert!(fixture.pane.clear_screen());
    wait_frame(&fixture.pane, |frame| frame.lines().iter().all(String::is_empty));
    let got = wait_file_containing(&fixture.path("got"), b"\\f");
    assert!(
        String::from_utf8_lossy(&got).contains("\\f"),
        "the shell got a form feed"
    );
}

#[test]
fn clear_screen_does_nothing_on_the_alternate_screen() {
    let fixture = spawn("printf '\\033[?1049hfull screen'; exec sleep 5").unwrap();
    wait_text(&fixture.pane, "full screen");
    assert!(!fixture.pane.clear_screen(), "not performed, so the key goes through");
    assert_eq!(
        fixture.pane.format(FormatRegion::History, TextFormat::Plain, true),
        None
    );
    wait_text(&fixture.pane, "full screen");
}

#[test]
fn reset_clears_the_terminal() {
    let fixture = spawn("printf '\\033[31mred\\033[?25l'; exec sleep 5").unwrap();
    wait_frame(&fixture.pane, |frame| !frame.cursor.visible);
    fixture.pane.reset();
    let frame = wait_frame(&fixture.pane, |frame| frame.cursor.visible);
    assert!(frame.lines().iter().all(String::is_empty));
}

#[test]
fn the_selection_adjusts_and_scrolls_into_view() {
    let fixture = spawn("seq 1 100; exec sleep 5").unwrap();
    wait_text(&fixture.pane, "100");
    assert!(!fixture.pane.adjust_selection(SelectionAdjust::Right), "no selection");
    assert!(!fixture.pane.scroll_to_selection());
    // the line "6" is screen row 5
    fixture
        .pane
        .select(GridPoint::screen(0, 5), GridPoint::screen(0, 5), false)
        .unwrap();
    assert!(fixture.pane.adjust_selection(SelectionAdjust::Right));
    let selection = fixture.pane.selection().unwrap();
    // Ghostty moves the end to the next non-blank cell, wrapping: the "7" on the next row
    assert_eq!((selection.end.x, selection.end.y), (0, 6));
    // its end is above the viewport: adjusting scrolled it to the top
    wait_frame(&fixture.pane, |frame| frame.lines()[0] == "7");
    fixture.pane.scroll(Scroll::Bottom);
    wait_frame(&fixture.pane, |frame| frame.lines()[0] != "6");
    assert!(fixture.pane.scroll_to_selection());
    wait_frame(&fixture.pane, |frame| frame.lines()[0] == "6");
    fixture.pane.scroll(Scroll::Row(9));
    wait_frame(&fixture.pane, |frame| frame.lines()[0] == "10");
}

#[test]
fn the_formatter_reads_regions_in_each_format() {
    let fixture = spawn("printf '\\033[31mred\\033[0m plain'; exec sleep 5").unwrap();
    wait_text(&fixture.pane, "plain");
    let pane = &fixture.pane;
    let plain = pane.format(FormatRegion::Screen, TextFormat::Plain, true).unwrap();
    assert!(plain.starts_with("red plain"), "{plain:?}");
    let vt = pane.format(FormatRegion::Screen, TextFormat::Vt, true).unwrap();
    assert!(vt.contains('\x1b') && vt.contains("red"), "{vt:?}");
    let html = pane.format(FormatRegion::Screen, TextFormat::Html, true).unwrap();
    assert!(html.contains('<') && html.contains("red"), "{html:?}");
    assert_eq!(
        pane.format(FormatRegion::History, TextFormat::Plain, true),
        None,
        "no scrollback yet"
    );
    assert_eq!(
        pane.format(FormatRegion::Selection, TextFormat::Plain, true),
        None,
        "no selection"
    );
    pane.select(GridPoint::viewport(4, 0), GridPoint::viewport(8, 0), false)
        .unwrap();
    assert_eq!(
        pane.format(FormatRegion::Selection, TextFormat::Plain, true).as_deref(),
        Some("plain")
    );
}

#[test]
fn mouse_reporting_reads_back() {
    let fixture = spawn("exec sleep 5").unwrap();
    assert!(fixture.pane.mouse_reporting());
    fixture.pane.set_mouse_reporting(false);
    assert!(!fixture.pane.mouse_reporting());
}
