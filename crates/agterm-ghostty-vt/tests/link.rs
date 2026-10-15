//! Links the vendored static libghostty-vt through the wrapper: VT bytes in, the screen read back out.
//! `Send` is checked at compile time below; `!Sync` is pinned by the crate docs' `compile_fail` examples.

use agterm_ghostty_vt::{CursorStyle, Dirty, Error, RenderState, Terminal};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const fn assert_send<T: Send>() {}
const _: () = assert_send::<Terminal>();
const _: () = assert_send::<RenderState>();

fn screen(state: &mut RenderState) -> Result<Vec<String>, Error> {
    let mut rows = state.rows()?;
    let mut lines = Vec::new();
    while let Some(mut row) = rows.next_row() {
        lines.push(row.text()?.trim_end().to_owned());
    }
    Ok(lines)
}

#[test]
fn vt_bytes_come_back_as_the_screen() -> TestResult {
    let mut terminal = Terminal::new(24, 4)?;
    // SGR colors, a cursor move and a wide character, all through the parser.
    terminal.vt_write("\x1b[1;31mhello\x1b[0m world\r\nsecond line\x1b[3;5Hthird\r\n日本 ok".as_bytes());
    let mut state = RenderState::new()?;
    state.update(&mut terminal)?;
    assert_eq!(state.size(), (24, 4));
    assert_eq!(
        screen(&mut state)?,
        ["hello world", "second line", "    third", "日本 ok"]
    );
    Ok(())
}

#[test]
fn a_resize_reflows_into_the_next_capture() -> TestResult {
    let mut terminal = Terminal::new(10, 3)?;
    terminal.vt_write(b"0123456789abc");
    let mut state = RenderState::new()?;
    state.update(&mut terminal)?;
    assert_eq!(screen(&mut state)?, ["0123456789", "abc", ""]);
    state.clean();

    terminal.resize(20, 3, 8, 16)?;
    state.update(&mut terminal)?;
    assert_ne!(state.dirty(), Dirty::Clean);
    assert_eq!(state.size(), (20, 3));
    assert_eq!(screen(&mut state)?, ["0123456789abc", "", ""]);
    Ok(())
}

#[test]
fn a_terminal_and_its_capture_move_to_another_thread() -> TestResult {
    let mut terminal = Terminal::new(16, 2)?;
    let mut state = RenderState::new()?;
    terminal.vt_write(b"from main");
    let worker = std::thread::spawn(move || {
        terminal.vt_write(b"\r\nfrom worker");
        state.update(&mut terminal)?;
        screen(&mut state)
    });
    let lines = worker.join().map_err(|_| "the worker panicked")??;
    assert_eq!(lines, ["from main", "from worker"]);
    Ok(())
}

fn cursor(terminal: &mut Terminal, state: &mut RenderState) -> Result<(CursorStyle, bool), Error> {
    state.update(terminal)?;
    let cursor = state.cursor().unwrap_or_default();
    Ok((cursor.style, cursor.blinking))
}

#[test]
fn the_default_cursor_applies_now_and_on_a_decscusr_reset() -> TestResult {
    let mut terminal = Terminal::new(8, 2)?;
    let mut state = RenderState::new()?;
    assert_eq!(
        cursor(&mut terminal, &mut state)?,
        (CursorStyle::Block, false),
        "the library's default"
    );
    terminal.set_default_cursor(CursorStyle::Bar, true)?;
    assert_eq!(
        cursor(&mut terminal, &mut state)?,
        (CursorStyle::Bar, true),
        "no program chose one yet"
    );
    // a program's choice wins until it asks for the default again
    terminal.vt_write(b"\x1b[4 q");
    assert_eq!(cursor(&mut terminal, &mut state)?, (CursorStyle::Underline, false));
    terminal.vt_write(b"\x1b[0 q");
    assert_eq!(cursor(&mut terminal, &mut state)?, (CursorStyle::Bar, true));
    terminal.vt_write(b"\x1b[2 q\x1bc");
    assert_eq!(
        cursor(&mut terminal, &mut state)?,
        (CursorStyle::Bar, true),
        "a full reset"
    );
    Ok(())
}
