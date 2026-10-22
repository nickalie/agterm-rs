//! Frames from real PTY output: dirty rows after partial updates, one frame shared by two views, a synchronized-output
//! hold publishing the held frame (and timing out), styles and wide cells.
//!
//! Unix only: the children are POSIX shell scripts whose output the tests pin byte for byte, which Windows'
//! pseudoconsole re-renders; `platform.rs` runs a pane's launch, echo, resize, exit, selection and search there.
#![cfg(unix)]

#[expect(dead_code, reason = "the event helpers serve tests/pane.rs")]
mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use ngterm_term::{CellFlags, CellWidth, CursorDefaults, CursorStyle, PaneConfig, Rgb, Underline};
use support::{SIZE, spawn, spawn_with, wait_file, wait_frame, wait_text};

#[test]
fn only_rows_written_since_a_frame_are_marked_changed() {
    let fixture = spawn("printf 'one\\r\\ntwo\\r\\nthree'; read _; printf '\\033[2;1Hdeux'; exec sleep 5").unwrap();
    let first = wait_text(&fixture.pane, "three");
    fixture.pane.write(b"\n").unwrap();
    let second = wait_text(&fixture.pane, "deux");
    assert_eq!(second.lines()[..3], ["one", "deux", "three"]);
    assert!(second.seq > first.seq);
    assert!(!second.row_changed_since(0, first.seq));
    assert!(second.row_changed_since(1, first.seq));
    assert!(Arc::ptr_eq(&first.rows[0], &second.rows[0]));
}

#[test]
fn two_views_of_one_pane_draw_one_frame() {
    let fixture = spawn("printf 'shared'; exec sleep 5").unwrap();
    wait_text(&fixture.pane, "shared");
    let pane = &fixture.pane;
    let (a, b) = std::thread::scope(|scope| {
        let a = scope.spawn(|| pane.frame());
        let b = scope.spawn(|| pane.frame());
        (a.join().unwrap(), b.join().unwrap())
    });
    let current = pane.frame();
    assert!(!current.stale);
    for view in [&a, &b] {
        assert_eq!(view.frame.seq, current.frame.seq);
        assert!(Arc::ptr_eq(&view.frame, &current.frame));
    }
}

#[test]
fn a_synchronized_output_hold_publishes_the_held_frame_until_it_ends() {
    let fixture = spawn(concat!(
        "printf 'before'; printf '\\033[?2026h'; printf ' during'; ",
        "read _; printf '\\033[?2026l'; exec sleep 5"
    ))
    .unwrap();
    // the hold began with "before" on screen; " during" was drawn under it and must not show
    let held = wait_frame(&fixture.pane, |frame| frame.held);
    assert_eq!(held.lines()[0], "before");
    std::thread::sleep(Duration::from_millis(100));
    let still = fixture.pane.frame().frame;
    assert!(still.held);
    assert_eq!(still.lines()[0], "before");
    fixture.pane.write(b"\n").unwrap();
    let after = wait_frame(&fixture.pane, |frame| !frame.held);
    assert_eq!(after.lines()[0], "before during");
}

#[test]
fn a_hold_that_never_ends_times_out() {
    let fixture = spawn("printf 'stuck'; printf '\\033[?2026h'; printf ' anyway'; exec sleep 10").unwrap();
    wait_frame(&fixture.pane, |frame| frame.held);
    // after a second the pane ends the hold itself and shows the live screen
    let live = wait_frame(&fixture.pane, |frame| !frame.held && frame.lines()[0] == "stuck anyway");
    assert!(!live.held);
}

#[test]
fn styles_colors_and_wide_cells_come_through() {
    let fixture = spawn(concat!(
        "printf '\\033[1;4;38;2;10;20;30mB\\033[0m\\033[3;9;48;5;2mI\\033[0m'; ",
        "printf '\\346\\227\\245\\346\\234\\254 ok'; exec sleep 5"
    ))
    .unwrap();
    let frame = wait_text(&fixture.pane, "ok");
    let row = &frame.rows[0];
    let cells = row.cells();
    assert_eq!(row.text(0), "B");
    assert!(cells[0].flags.contains(CellFlags::BOLD));
    assert_eq!(cells[0].underline, Underline::Single);
    assert_eq!(cells[0].foreground, Some(Rgb::new(10, 20, 30)));
    assert_eq!(row.text(1), "I");
    assert!(cells[1].flags.contains(CellFlags::ITALIC | CellFlags::STRIKETHROUGH));
    assert_eq!(cells[1].background, Some(frame.colors.palette[2]));
    assert_eq!(row.text(2), "日");
    assert_eq!(cells[2].width, CellWidth::Wide);
    assert_eq!(cells[3].width, CellWidth::SpacerTail);
    assert_eq!(row.text(3), "");
    assert_eq!(row.line(), "BI日本 ok");
}

#[test]
fn the_theme_reaches_the_frame_and_can_change() {
    let theme = ngterm_term::ThemeColors {
        background: Rgb::new(1, 2, 3),
        ..ngterm_term::ThemeColors::default()
    };
    let config = PaneConfig {
        colors: theme.clone(),
        ..PaneConfig::new(SIZE)
    };
    let fixture = spawn_with("seq 1 5000 > \"$DIR/done\"; seq 1 5000; exec sleep 5", &config).unwrap();
    wait_file(&fixture.path("done"), 1);
    let frame = wait_text(&fixture.pane, "5000");
    assert_eq!(frame.colors.background, Rgb::new(1, 2, 3));
    let light = ngterm_term::ThemeColors {
        background: Rgb::new(250, 250, 250),
        ..theme
    };
    fixture.pane.set_colors(&light).unwrap();
    let frame = wait_frame(&fixture.pane, |frame| {
        frame.colors.background == Rgb::new(250, 250, 250)
    });
    assert_eq!(frame.colors.foreground, light.foreground);
}

#[test]
fn an_expired_hold_wakes_the_frontend_without_a_paint() {
    let wakes = Arc::new(AtomicUsize::new(0));
    let config = PaneConfig {
        wake: Some({
            let wakes = Arc::clone(&wakes);
            Arc::new(move || {
                wakes.fetch_add(1, Ordering::SeqCst);
            })
        }),
        ..PaneConfig::new(SIZE)
    };
    let fixture = spawn_with(
        "printf 'stuck'; printf '\\033[?2026h'; printf ' anyway'; exec sleep 30",
        &config,
    )
    .unwrap();
    wait_frame(&fixture.pane, |frame| frame.held);
    // from here on the test paints only when woken, as a frontend does: the pane must wake it to end the hold
    // the hold expires after a second; the child stays, so its exit cannot be what wakes the frontend
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut seen = wakes.load(Ordering::SeqCst);
    // a wake already counted may have left the coalescing flag set; clear it without painting, as a frontend
    // draining events does, so the expiry's wake can fire
    let _ = fixture.pane.take_events();
    loop {
        while wakes.load(Ordering::SeqCst) == seen {
            assert!(Instant::now() < deadline, "no wake came for the expired hold");
            std::thread::sleep(Duration::from_millis(5));
        }
        seen = wakes.load(Ordering::SeqCst);
        let frame = fixture.pane.frame().frame;
        if !frame.held {
            assert_eq!(frame.lines()[0], "stuck anyway");
            break;
        }
    }
}

#[test]
fn the_configured_cursor_is_the_terminals_default() {
    let config = PaneConfig {
        cursor: CursorDefaults {
            style: CursorStyle::Bar,
            blink: false,
        },
        ..PaneConfig::new(SIZE)
    };
    let fixture = spawn_with(
        "printf 'ready'; read _; printf '\\033[4 q'; read _; printf '\\033[0 q'; exec sleep 5",
        &config,
    )
    .unwrap();
    let frame = wait_text(&fixture.pane, "ready");
    assert_eq!((frame.cursor.style, frame.cursor.blinking), (CursorStyle::Bar, false));
    fixture.pane.write(b"\n").unwrap();
    wait_frame(&fixture.pane, |frame| frame.cursor.style == CursorStyle::Underline);
    fixture.pane.write(b"\n").unwrap();
    wait_frame(&fixture.pane, |frame| frame.cursor.style == CursorStyle::Bar);
    // a reload changes the default, and the cursor follows while no program chose one
    fixture.pane.set_cursor_defaults(CursorDefaults::default()).unwrap();
    let frame = wait_frame(&fixture.pane, |frame| frame.cursor.style == CursorStyle::Block);
    assert!(frame.cursor.blinking, "Ghostty's default blinks");
}
