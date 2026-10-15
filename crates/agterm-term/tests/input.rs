//! Input through a real PTY: keys encoded for the modes the program set (legacy, application cursor keys, the Kitty
//! keyboard protocol), mouse reports, focus reports and pastes reach the child as the bytes it asked for.
//!
//! Each child turns its terminal raw, sets the modes under test, prints `ready` (so the VT has processed the modes
//! before any key is encoded) and copies its input to a file.

#[expect(dead_code, reason = "the frame and file-content helpers serve the other test files")]
mod support;

use std::fmt::Write as _;

use agterm_core::ghostty_config::binds::{KeyAction, Mods, PhysicalKey};
use agterm_term::{
    ClipboardLocation, KeyInput, MouseAction, MouseButton, MouseInput, Padding, PaneError, Paste, PasteSource,
    TextRegion, Wheel, WheelInput,
};
use support::{Fixture, TestResult, spawn, wait_file, wait_text};

/// A child in raw mode that sets `modes`, says `ready` and copies its input to `$DIR/in`.
fn recorder(modes: &str) -> TestResult<Fixture> {
    let fixture = spawn(&format!(
        "stty raw -echo; printf '{modes}'; printf ready; exec cat > \"$DIR/in\""
    ))?;
    wait_text(&fixture.pane, "ready");
    Ok(fixture)
}

/// What the child received, once it holds `expected.len()` bytes.
fn received(fixture: &Fixture, expected: &[u8]) -> Vec<u8> {
    wait_file(&fixture.path("in"), expected.len())
}

/// The key named `name` (a misspelt name encodes as an unidentified key, and the test fails on its bytes).
fn key(name: &str) -> PhysicalKey {
    PhysicalKey::new(name).unwrap_or(PhysicalKey::UNIDENTIFIED)
}

fn press<'a>(name: &str, mods: Mods, text: &'a str) -> KeyInput<'a> {
    KeyInput::press(key(name), mods, text)
}

/// Send each key and the bytes they should produce, then check the child got exactly those, in order.
fn type_keys(fixture: &Fixture, keys: &[(KeyInput<'_>, &[u8])]) -> TestResult {
    let mut expected = Vec::new();
    for (input, bytes) in keys {
        assert_eq!(fixture.pane.key(input)?, !bytes.is_empty(), "{input:?}");
        expected.extend_from_slice(bytes);
    }
    let got = received(fixture, &expected);
    assert_eq!(String::from_utf8_lossy(&got), String::from_utf8_lossy(&expected));
    Ok(())
}

#[test]
fn legacy_encodings_follow_the_cursor_key_mode() -> TestResult {
    let fixture = recorder("")?;
    type_keys(
        &fixture,
        &[
            (press("key_a", Mods::empty(), "a"), b"a"),
            (press("key_a", Mods::SHIFT, "A"), b"A"),
            (press("arrow_up", Mods::empty(), ""), b"\x1b[A"),
            (press("arrow_left", Mods::CTRL, ""), b"\x1b[1;5D"),
            (press("f1", Mods::empty(), ""), b"\x1bOP"),
            (press("f5", Mods::empty(), ""), b"\x1b[15~"),
            (press("f12", Mods::SHIFT, ""), b"\x1b[24;2~"),
            (press("key_c", Mods::CTRL, "c"), b"\x03"),
            (press("key_x", Mods::ALT, "x"), b"\x1bx"),
            (press("enter", Mods::empty(), ""), b"\r"),
            (press("backspace", Mods::empty(), ""), b"\x7f"),
            (press("tab", Mods::SHIFT, ""), b"\x1b[Z"),
            // a lone modifier and a release send nothing
            (press("shift_left", Mods::SHIFT, ""), b""),
            (
                KeyInput {
                    action: KeyAction::Release,
                    ..press("key_a", Mods::empty(), "a")
                },
                b"",
            ),
        ],
    )?;
    let app = recorder("\\033[?1h")?;
    type_keys(&app, &[(press("arrow_up", Mods::empty(), ""), b"\x1bOA")])
}

#[test]
fn kitty_encodings_follow_the_flags_the_program_pushed() -> TestResult {
    // disambiguate escape codes (flag 1)
    let fixture = recorder("\\033[>1u")?;
    type_keys(
        &fixture,
        &[
            (press("escape", Mods::empty(), ""), b"\x1b[27u"),
            (press("key_c", Mods::CTRL, "c"), b"\x1b[99;5u"),
            (press("key_x", Mods::ALT, "x"), b"\x1b[120;3u"),
            (press("key_a", Mods::empty(), "a"), b"a"),
            (press("enter", Mods::empty(), ""), b"\r"),
            (press("arrow_up", Mods::empty(), ""), b"\x1b[A"),
            (press("f5", Mods::empty(), ""), b"\x1b[15~"),
        ],
    )?;
    // report event types too (flags 1 + 2): a release is reported
    let events = recorder("\\033[>3u")?;
    type_keys(
        &events,
        &[(
            KeyInput {
                action: KeyAction::Release,
                ..press("key_c", Mods::CTRL, "c")
            },
            b"\x1b[99;5:3u",
        )],
    )
}

#[test]
fn the_mouse_reports_only_once_a_program_asks() -> TestResult {
    let quiet = recorder("")?;
    let click = MouseInput {
        action: MouseAction::Press,
        button: Some(MouseButton::Left),
        mods: Mods::empty(),
        // padding 4 px: cell (2, 1) of an 8x16 grid
        x: 4.0 + 8.0 * 2.0 + 1.0,
        y: 4.0 + 16.0 + 1.0,
    };
    assert!(!quiet.pane.mouse_tracking());
    assert!(!quiet.pane.mouse(&click)?);

    let fixture = recorder("\\033[?1002h\\033[?1006h")?;
    assert!(fixture.pane.mouse_tracking());
    fixture.pane.set_padding(Padding {
        top: 4,
        bottom: 4,
        left: 4,
        right: 4,
    });
    let drag = MouseInput {
        action: MouseAction::Motion,
        x: click.x + 8.0,
        ..click
    };
    let release = MouseInput {
        action: MouseAction::Release,
        ..drag
    };
    let hover = MouseInput {
        action: MouseAction::Motion,
        button: None,
        x: drag.x + 8.0,
        ..click
    };
    assert!(fixture.pane.mouse(&click)?);
    assert!(fixture.pane.mouse(&drag)?, "button-event tracking reports a drag");
    assert!(fixture.pane.mouse(&release)?);
    assert!(!fixture.pane.mouse(&hover)?, "but not motion with no button held");
    let expected = b"\x1b[<0;3;2M\x1b[<32;4;2M\x1b[<0;4;2m";
    assert_eq!(received(&fixture, expected), expected);
    Ok(())
}

/// A wheel turn of `rows` steps at cell (2, 1) of the unpadded 8x16 grid.
fn wheel(rows: isize) -> WheelInput {
    WheelInput {
        rows,
        columns: 0,
        mods: Mods::empty(),
        x: 8.0 * 2.0 + 1.0,
        y: 16.0 + 1.0,
    }
}

#[test]
fn a_wheel_step_is_reported_and_never_counts_as_a_held_button() -> TestResult {
    // button-event tracking: motion is reported only while a button is held
    let fixture = recorder("\\033[?1002h\\033[?1006h")?;
    assert_eq!(fixture.pane.wheel(&wheel(1))?, Wheel::Reported);
    assert_eq!(fixture.pane.wheel(&wheel(-2))?, Wheel::Reported);
    let sideways = WheelInput {
        rows: 0,
        columns: 1,
        ..wheel(0)
    };
    assert_eq!(fixture.pane.wheel(&sideways)?, Wheel::Reported);
    // a wheel button pressed through the plain mouse path is not held either
    let step = MouseInput {
        action: MouseAction::Press,
        button: Some(MouseButton::Four),
        mods: Mods::empty(),
        x: 17.0,
        y: 17.0,
    };
    assert!(fixture.pane.mouse(&step)?);
    let hover = MouseInput {
        action: MouseAction::Motion,
        button: None,
        x: 40.0,
        ..step
    };
    assert!(!fixture.pane.mouse(&hover)?, "a hover after wheel steps is no drag");
    let expected = b"\x1b[<64;3;2M\x1b[<65;3;2M\x1b[<65;3;2M\x1b[<66;3;2M\x1b[<64;3;2M";
    assert_eq!(received(&fixture, expected), expected);
    Ok(())
}

#[test]
fn the_wheel_sends_cursor_keys_on_the_alternate_screen_without_mouse_tracking() -> TestResult {
    let normal = recorder("\\033[?1049h")?;
    assert_eq!(normal.pane.wheel(&wheel(2))?, Wheel::CursorKeys);
    assert_eq!(normal.pane.wheel(&wheel(0))?, Wheel::Nothing);
    assert_eq!(normal.pane.wheel(&wheel(-1))?, Wheel::CursorKeys);
    assert_eq!(received(&normal, b"\x1b[A\x1b[A\x1b[B"), b"\x1b[A\x1b[A\x1b[B");

    let application = recorder("\\033[?1049h\\033[?1h")?;
    assert_eq!(application.pane.wheel(&wheel(-1))?, Wheel::CursorKeys);
    assert_eq!(received(&application, b"\x1bOB"), b"\x1bOB");

    // alternate scroll off (mode 1007 reset): the viewport scrolls instead
    let off = recorder("\\033[?1049h\\033[?1007l")?;
    assert_eq!(off.pane.wheel(&wheel(1))?, Wheel::Scrolled);
    Ok(())
}

#[test]
fn the_wheel_scrolls_the_viewport_when_no_program_takes_it() -> TestResult {
    let fixture = spawn("for i in $(seq 1 40); do printf 'row %s\\r\\n' $i; done; printf end; exec sleep 30")?;
    let pane = &fixture.pane;
    wait_text(pane, "end");
    assert_eq!(pane.wheel(&wheel(3))?, Wheel::Scrolled);
    let scrolled = pane.read_text(TextRegion::Viewport)?;
    assert!(
        scrolled.starts_with("row 15\n") && scrolled.ends_with("row 38"),
        "{scrolled}"
    );
    assert_eq!(pane.wheel(&wheel(-3))?, Wheel::Scrolled);
    assert!(pane.read_text(TextRegion::Viewport)?.ends_with("row 40\nend"));
    Ok(())
}

#[test]
fn mouse_reporting_off_keeps_the_mouse_from_programs() -> TestResult {
    let fixture = recorder("\\033[?1000h\\033[?1006h")?;
    let pane = &fixture.pane;
    pane.set_mouse_reporting(false);
    assert!(!pane.mouse_tracking());
    let click = MouseInput {
        action: MouseAction::Press,
        button: Some(MouseButton::Left),
        mods: Mods::empty(),
        x: 1.0,
        y: 1.0,
    };
    assert!(!pane.mouse(&click)?);
    assert_eq!(pane.wheel(&wheel(1))?, Wheel::Scrolled);
    pane.set_mouse_reporting(true);
    assert!(pane.mouse_tracking());
    assert!(pane.mouse(&click)?);
    assert_eq!(received(&fixture, b"\x1b[<0;1;1M"), b"\x1b[<0;1;1M");
    Ok(())
}

#[test]
fn focus_reports_only_in_focus_reporting_mode() -> TestResult {
    let quiet = recorder("")?;
    assert!(!quiet.pane.focus(true)?);
    let fixture = recorder("\\033[?1004h")?;
    assert!(fixture.pane.focus(true)?);
    assert!(fixture.pane.focus(false)?);
    assert_eq!(received(&fixture, b"\x1b[I\x1b[O"), b"\x1b[I\x1b[O");
    Ok(())
}

#[test]
fn a_bracketed_paste_is_wrapped_and_a_plain_one_needs_consent_for_newlines() -> TestResult {
    let plain = recorder("")?;
    let paste = |fixture: &Fixture, text: &str, allow| {
        fixture
            .pane
            .paste(text, PasteSource::Clipboard, ClipboardLocation::Standard, allow)
    };
    assert_eq!(paste(&plain, "echo hi", false)?, Paste::Written);
    assert_eq!(paste(&plain, "ls\nrm -rf x", false)?, Paste::Unsafe);
    assert_eq!(paste(&plain, "", false)?, Paste::Empty);
    assert_eq!(paste(&plain, "ls\nrm", true)?, Paste::Written);
    assert_eq!(received(&plain, b"echo hils\rrm"), b"echo hils\rrm");

    let bracketed = recorder("\\033[?2004h")?;
    assert_eq!(paste(&bracketed, "a\nb\x1bc", false)?, Paste::Written);
    let expected = b"\x1b[200~a\nb c\x1b[201~";
    assert_eq!(received(&bracketed, expected), expected);
    Ok(())
}

#[test]
fn a_large_paste_arrives_whole_and_in_order() -> TestResult {
    let fixture = recorder("\\033[?2004h")?;
    let text = (0..20_000).fold(String::new(), |mut text, line| {
        let _ = writeln!(text, "line {line}");
        text
    });
    assert!(text.len() > 100_000);
    let outcome = fixture
        .pane
        .paste(&text, PasteSource::Text, ClipboardLocation::Standard, false)?;
    assert_eq!(outcome, Paste::Written);
    let expected = [b"\x1b[200~".as_slice(), text.as_bytes(), b"\x1b[201~"].concat();
    assert_eq!(received(&fixture, &expected), expected);
    Ok(())
}

#[test]
fn input_to_a_closed_pane_fails() -> TestResult {
    let fixture = spawn("exit 0")?;
    support::wait_events(&fixture.pane, |events| {
        events
            .iter()
            .any(|event| matches!(event, agterm_term::PaneEvent::Exited(_)))
    });
    let deadline = std::time::Instant::now() + support::DEADLINE;
    // the writer notices the closed pty on its next write
    loop {
        match fixture.pane.key(&press("key_a", Mods::empty(), "a")) {
            Err(PaneError::Closed) => return Ok(()),
            Ok(_) => {}
            Err(err) => return Err(err.into()),
        }
        assert!(std::time::Instant::now() < deadline, "input was never refused");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}
