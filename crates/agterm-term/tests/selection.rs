//! Selection, read-back and search on a pane fed by a real PTY: the selected text and the frame's selected columns,
//! the screen and scrollback read back as `ghostty_surface_read_text` returned them (wide characters, wrapped lines),
//! the cursor column, and the search's counts, navigation and reports.

#[expect(dead_code, reason = "the file helpers serve the other test files")]
mod support;

use agterm_term::{
    GridPoint, Pane, PaneEvent, Scroll, SearchState, SearchStep, SelectionRange, SelectionText, TextRegion,
};
use support::{Fixture, SIZE, TestResult, spawn, wait_events, wait_frame, wait_text};

/// Two rows: `hello world  ` (two trailing blanks written) and `second line`.
fn two_rows() -> TestResult<Fixture> {
    let fixture = spawn("printf 'hello world  \\r\\nsecond line'; exec sleep 30")?;
    wait_text(&fixture.pane, "second line");
    Ok(fixture)
}

fn raw(pane: &Pane) -> Option<String> {
    pane.selection_text(SelectionText::Raw)
}

#[test]
fn a_word_and_a_line_select_and_show_in_the_frame() -> TestResult {
    let fixture = two_rows()?;
    let pane = &fixture.pane;
    assert_eq!(pane.selection(), None);
    assert_eq!(raw(pane), None);

    assert!(pane.select_word(GridPoint::viewport(1, 0))?);
    assert_eq!(raw(pane).as_deref(), Some("hello"));
    let frame = wait_frame(pane, |frame| frame.rows[0].selection() == Some((0, 4)));
    assert_eq!(frame.rows[1].selection(), None);

    assert!(pane.select_line(GridPoint::viewport(3, 1))?);
    assert_eq!(raw(pane).as_deref(), Some("second line"));
    wait_frame(pane, |frame| {
        frame.rows[0].selection().is_none() && frame.rows[1].selection() == Some((0, 10))
    });
    Ok(())
}

#[test]
fn ranges_and_rectangles_read_back_raw_or_as_copied() -> TestResult {
    let fixture = two_rows()?;
    let pane = &fixture.pane;
    pane.select(GridPoint::viewport(6, 0), GridPoint::viewport(2, 1), false)?;
    assert_eq!(raw(pane).as_deref(), Some("world  \nsec"));
    assert_eq!(pane.selection_text(SelectionText::Copy).as_deref(), Some("world\nsec"));
    assert_eq!(
        pane.selection(),
        Some(SelectionRange {
            start: GridPoint::screen(6, 0),
            end: GridPoint::screen(2, 1),
            rectangle: false,
        })
    );
    pane.select(GridPoint::viewport(1, 0), GridPoint::viewport(3, 1), true)?;
    assert_eq!(raw(pane).as_deref(), Some("ell\neco"));
    Ok(())
}

#[test]
fn everything_selects_and_clears() -> TestResult {
    let fixture = two_rows()?;
    let pane = &fixture.pane;
    assert!(pane.select_all()?);
    assert_eq!(
        pane.selection_text(SelectionText::Copy).as_deref(),
        Some("hello world\nsecond line")
    );
    // nothing to select on a blank row: the selection stays
    assert!(!pane.select_word(GridPoint::viewport(0, 5))?);
    assert!(pane.selection().is_some());
    pane.clear_selection()?;
    assert_eq!(pane.selection(), None);
    wait_frame(pane, |frame| frame.rows.iter().all(|row| row.selection().is_none()));
    // a point off the screen is an error, not a panic
    assert!(
        pane.select(GridPoint::viewport(0, 99), GridPoint::viewport(0, 0), false)
            .is_err()
    );
    Ok(())
}

#[test]
fn read_back_joins_wrapped_lines_and_keeps_wide_characters() -> TestResult {
    let long = "x".repeat(usize::from(SIZE.cols) + 5);
    let fixture = spawn(&format!("printf '{long}\\r\\n日本語 ok\\r\\nabc'; exec sleep 30"))?;
    let pane = &fixture.pane;
    wait_text(pane, "abc");
    let text = pane.read_text(TextRegion::Viewport)?;
    assert_eq!(text, format!("{long}\n日本語 ok\nabc"));
    assert_eq!(pane.cursor_column(), 3);
    Ok(())
}

#[test]
fn the_viewport_and_all_differ_by_the_scrollback() -> TestResult {
    let fixture = spawn("for i in $(seq 1 40); do printf 'row %s\\r\\n' $i; done; printf end; exec sleep 30")?;
    let pane = &fixture.pane;
    wait_text(pane, "end");
    let viewport = pane.read_text(TextRegion::Viewport)?;
    let all = pane.read_text(TextRegion::All)?;
    assert!(!viewport.contains("row 1\n"), "{viewport}");
    assert!(viewport.ends_with("row 40\nend"), "{viewport}");
    assert!(all.starts_with("row 1\nrow 2\n"), "{all}");
    assert!(all.ends_with(&viewport));
    // scrolled to the top, the viewport is the oldest rows
    pane.scroll(Scroll::Top);
    assert!(pane.read_text(TextRegion::Viewport)?.starts_with("row 1\n"));
    pane.scroll(Scroll::Bottom);
    assert_eq!(pane.read_text(TextRegion::Viewport)?, viewport);
    Ok(())
}

#[test]
fn a_blank_screen_reads_as_empty() -> TestResult {
    let fixture = spawn("exec sleep 30")?;
    assert_eq!(fixture.pane.read_text(TextRegion::Viewport)?, "");
    assert_eq!(fixture.pane.cursor_column(), 0);
    Ok(())
}

/// Every `SearchTotal` reported in `events`, in order.
fn totals(events: &[PaneEvent]) -> Vec<Option<usize>> {
    events
        .iter()
        .filter_map(|event| match event {
            PaneEvent::SearchTotal(total) => Some(*total),
            _ => None,
        })
        .collect()
}

/// Every `SearchSelected` reported in `events`, in order.
fn selections(events: &[PaneEvent]) -> Vec<Option<usize>> {
    events
        .iter()
        .filter_map(|event| match event {
            PaneEvent::SearchSelected(selected) => Some(*selected),
            _ => None,
        })
        .collect()
}

/// Wait until the last reported total is `total`.
fn wait_total(pane: &Pane, total: usize) {
    wait_events(pane, |events| totals(events).last() == Some(&Some(total)));
}

/// Thirty lines, then two matches of `match` (one capitalized); a third after a line of input.
fn searchable() -> TestResult<Fixture> {
    let fixture = spawn(concat!(
        "for i in $(seq 1 30); do printf 'line %s\\r\\n' $i; done; ",
        "printf 'Match one\\r\\nfiller\\r\\nmatch two\\r\\n'; ",
        "read _; printf 'match three'; exec sleep 30"
    ))?;
    wait_text(&fixture.pane, "match two");
    Ok(fixture)
}

#[test]
fn search_counts_and_navigates_newest_first() -> TestResult {
    let fixture = searchable()?;
    let pane = &fixture.pane;
    assert!(!pane.navigate_search(SearchStep::Next), "no search open");
    assert!(!pane.search(""), "nothing to end");

    assert!(pane.search("match"));
    wait_total(pane, 2);
    let state = pane.search_state();
    assert_eq!(
        (state.needle.as_str(), state.total, state.selected),
        ("match", Some(2), None)
    );
    assert_eq!(state.highlights.len(), 2);

    // next walks toward older output: the newest match first
    assert!(pane.navigate_search(SearchStep::Next));
    assert_eq!(selections(&pane.take_events()), [Some(0)]);
    let newest = pane.search_state().selected_highlight;
    assert!(pane.navigate_search(SearchStep::Next));
    assert_eq!(selections(&pane.take_events()), [Some(1)]);
    let older = pane.search_state().selected_highlight;
    assert!(
        older
            .zip(newest)
            .is_some_and(|(older, newest)| older.start.1 < newest.start.1),
        "{older:?} above {newest:?}"
    );
    assert!(pane.navigate_search(SearchStep::Previous));
    assert_eq!(selections(&pane.take_events()), [Some(0)]);
    Ok(())
}

#[test]
fn search_follows_new_output_restarts_and_ends() -> TestResult {
    let fixture = searchable()?;
    let pane = &fixture.pane;
    assert!(pane.search("match"));
    wait_total(pane, 2);
    pane.write(b"\n")?;
    wait_total(pane, 3);

    // a new needle restarts; an empty one ends the search
    assert!(pane.search("line 3"));
    wait_total(pane, 2);
    assert!(pane.search(""));
    assert_eq!(pane.search_state(), SearchState::default());
    assert!(!pane.navigate_search(SearchStep::Next));
    Ok(())
}

#[test]
fn a_search_with_no_match_reports_zero() -> TestResult {
    let fixture = spawn("printf 'nothing here'; exec sleep 30")?;
    let pane = &fixture.pane;
    wait_text(pane, "nothing here");
    assert!(pane.search("zzz"));
    wait_total(pane, 0);
    assert!(pane.navigate_search(SearchStep::Next));
    assert_eq!(pane.search_state().selected, None);
    Ok(())
}
