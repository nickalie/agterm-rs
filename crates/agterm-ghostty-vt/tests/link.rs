//! Links the vendored static libghostty-vt through the wrapper: VT bytes in, the screen read back out.
//! `Send` is checked at compile time below; `!Sync` is pinned by the crate docs' `compile_fail` examples.

use agterm_ghostty_vt::{Dirty, Error, RenderState, Terminal};

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
