//! The pointer over a pane fed by a real PTY: Ghostty's selection gesture (click counts, word and line clicks, drags,
//! autoscroll past the bottom), the cell under a position, OSC 8 hyperlinks and soft-wrapped rows.

#[expect(dead_code, reason = "the file helpers serve the other test files")]
mod support;

use std::time::{Duration, Instant};

use agterm_term::{Autoscroll, Gesture, GestureEvent, GridPoint, Pane, SelectionText};
use support::{SIZE, TestResult, spawn, wait_text};

/// The surface position of the middle of viewport cell (`col`, `row`): [`SIZE`]'s 8x16 cells, no padding.
fn at(col: u16, row: u16) -> (f32, f32) {
    (f32::from(col) * 8.0 + 4.0, f32::from(row) * 16.0 + 8.0)
}

fn raw(pane: &Pane) -> Option<String> {
    pane.selection_text(SelectionText::Raw)
}

/// A press and its release at a cell.
fn click(pane: &Pane, col: u16, row: u16, time: Instant) -> TestResult<Gesture> {
    let (x, y) = at(col, row);
    let gesture = pane.gesture(GestureEvent::Press {
        x,
        y,
        time,
        output: false,
        repeat_interval: agterm_term::CLICK_REPEAT_INTERVAL,
    })?;
    pane.gesture(GestureEvent::Release { x, y })?;
    Ok(gesture)
}

#[test]
fn clicks_select_words_and_lines_and_a_single_click_clears() -> TestResult {
    let fixture = spawn("printf 'hello world\\r\\nsecond line'; exec sleep 30")?;
    let pane = &fixture.pane;
    wait_text(pane, "second line");
    let start = Instant::now();
    let ms = Duration::from_millis;
    assert_eq!(click(pane, 1, 0, start)?.click_count, 1);
    assert_eq!(pane.selection(), None);
    assert_eq!(click(pane, 1, 0, start + ms(100))?.click_count, 2);
    assert_eq!(raw(pane).as_deref(), Some("hello"));
    assert_eq!(click(pane, 1, 0, start + ms(200))?.click_count, 3);
    assert_eq!(raw(pane).as_deref(), Some("hello world"));
    // a click past the repeat interval starts over and drops the selection
    assert_eq!(click(pane, 4, 1, start + ms(2000))?.click_count, 1);
    assert_eq!(pane.selection(), None);
    Ok(())
}

#[test]
fn a_drag_selects_cells_and_past_the_bottom_asks_for_autoscroll() -> TestResult {
    let fixture = spawn("printf 'hello world'; exec sleep 30")?;
    let pane = &fixture.pane;
    wait_text(pane, "hello world");
    let (x, y) = at(0, 0);
    pane.gesture(GestureEvent::Press {
        x: x - 3.0,
        y,
        time: Instant::now(),
        output: false,
        repeat_interval: agterm_term::CLICK_REPEAT_INTERVAL,
    })?;
    // Ghostty's half-cell rule: a cell joins once the pointer passes its middle
    let (x, y) = at(4, 0);
    let x = x + 3.0;
    let dragged = pane.gesture(GestureEvent::Drag { x, y, rectangle: false })?;
    assert_eq!(dragged.autoscroll, Autoscroll::None);
    assert_eq!(raw(pane).as_deref(), Some("hello"));
    pane.gesture(GestureEvent::Release { x, y })?;
    assert!(pane.gesture_state().dragged);

    pane.gesture(GestureEvent::Press {
        x,
        y,
        time: Instant::now() + Duration::from_secs(5),
        output: false,
        repeat_interval: agterm_term::CLICK_REPEAT_INTERVAL,
    })?;
    let below = f32::from(SIZE.rows) * 16.0 + 40.0;
    let state = pane.gesture(GestureEvent::Drag {
        x,
        y: below,
        rectangle: false,
    })?;
    assert_eq!(state.autoscroll, Autoscroll::Down);
    pane.reset_gesture();
    assert_eq!(pane.gesture_state().click_count, 0);
    Ok(())
}

#[test]
fn the_pointer_finds_its_cell_and_osc8_links() -> TestResult {
    let fixture =
        spawn("printf 'see \\033]8;;https://example.com/x\\033\\\\here\\033]8;;\\033\\\\ done'; exec sleep 30")?;
    let pane = &fixture.pane;
    wait_text(pane, "done");
    let (x, y) = at(5, 0);
    assert_eq!(pane.cell_at(x, y), GridPoint::viewport(5, 0));
    assert_eq!(pane.cell_at(-10.0, -10.0), GridPoint::viewport(0, 0));
    assert_eq!(
        pane.cell_at(1.0e6, 1.0e6),
        GridPoint::viewport(SIZE.cols - 1, u32::from(SIZE.rows) - 1)
    );
    assert_eq!(
        pane.hyperlink(GridPoint::viewport(5, 0)).as_deref(),
        Some("https://example.com/x")
    );
    assert_eq!(pane.hyperlink(GridPoint::viewport(1, 0)), None);
    Ok(())
}

#[test]
fn a_soft_wrapped_row_says_so() -> TestResult {
    let line = "x".repeat(usize::from(SIZE.cols) + 3);
    let fixture = spawn(&format!("printf '{line}\\r\\nend'; exec sleep 30"))?;
    let frame = wait_text(&fixture.pane, "end");
    assert!(frame.rows[0].wrapped());
    assert!(!frame.rows[1].wrapped());
    assert!(!frame.rows[2].wrapped());
    Ok(())
}
