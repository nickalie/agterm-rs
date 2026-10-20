//! The outer terminal on Windows: the console in VT mode both ways (herdr's `enable_windows_virtual_terminal_input`,
//! its input then read as records, `windows_console_input_items`), the window's size from the screen buffer, and the
//! input's reader, which also reports the console's window-size events, and a watcher polling the window's size.
//!
//! herdr: `src/client/terminal_setup.rs` and `src/client/input/windows_vti.rs` at `HERDR_REV` (`build/pins.env`),
//! licensed under the Apache License 2.0, modified as the parent module's docs say.

use std::fs::File;
use std::io::{self, IsTerminal as _, Read as _, Write as _};
use std::mem::ManuallyDrop;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{
    CONSOLE_MODE, CONSOLE_SCREEN_BUFFER_INFO, DISABLE_NEWLINE_AUTO_RETURN, ENABLE_EXTENDED_FLAGS,
    ENABLE_PROCESSED_OUTPUT, ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, ENABLE_WINDOW_INPUT,
    GetConsoleCP, GetConsoleMode, GetConsoleOutputCP, GetConsoleScreenBufferInfo, GetStdHandle, INPUT_RECORD,
    KEY_EVENT, KEY_EVENT_RECORD, LEFT_ALT_PRESSED, LEFT_CTRL_PRESSED, RIGHT_ALT_PRESSED, RIGHT_CTRL_PRESSED,
    ReadConsoleInputW, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleCP, SetConsoleMode, SetConsoleOutputCP,
    WINDOW_BUFFER_SIZE_EVENT,
};

use super::{TtyEvent, TtySize};
use crate::protocol::GridSize;

/// UTF-8's code page.
const CP_UTF8: u32 = 65001;

/// The size of one read of a piped input.
const READ_SIZE: usize = 16 * 1024;

/// The console records one read takes.
const RECORDS: u32 = 64;

/// How often the window's size is looked at.
const SIZE_POLL: Duration = Duration::from_millis(200);

/// The space bar's virtual-key code (`VK_SPACE`).
const VK_SPACE: u16 = 0x20;
/// The 2 key's virtual-key code (`'2'`), Ctrl+@ with Shift.
const VK_2: u16 = 0x32;

pub(super) fn is_terminal() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

fn std_handle(which: u32) -> Option<HANDLE> {
    // SAFETY: querying a standard handle has no preconditions.
    let handle = unsafe { GetStdHandle(which) };
    (!handle.is_null() && handle != INVALID_HANDLE_VALUE).then_some(handle)
}

fn console_mode(handle: HANDLE) -> io::Result<CONSOLE_MODE> {
    let mut mode: CONSOLE_MODE = 0;
    // SAFETY: a console handle and a valid out-pointer.
    if unsafe { GetConsoleMode(handle, &raw mut mode) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(mode)
}

fn set_console_mode(handle: HANDLE, mode: CONSOLE_MODE) -> io::Result<()> {
    // SAFETY: a console handle.
    if unsafe { SetConsoleMode(handle, mode) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The console in VT mode: its input as VT sequences with window-size events and no line editing, echo or Ctrl+C
/// handling (`ENABLE_EXTENDED_FLAGS` without quick edit, so the mouse reaches the session), its output's VT sequences
/// processed and no automatic return; both in UTF-8. Dropped, everything is restored.
#[derive(Debug)]
pub(super) struct RawMode {
    input: HANDLE,
    output: HANDLE,
    input_mode: CONSOLE_MODE,
    output_mode: CONSOLE_MODE,
    input_code_page: u32,
    output_code_page: u32,
}

impl RawMode {
    pub(super) fn enter() -> io::Result<Self> {
        let not_console = || io::Error::other("the standard handles are not a console");
        let input = std_handle(STD_INPUT_HANDLE).ok_or_else(not_console)?;
        let output = std_handle(STD_OUTPUT_HANDLE).ok_or_else(not_console)?;
        let input_mode = console_mode(input)?;
        let output_mode = console_mode(output)?;
        // SAFETY: no preconditions.
        let input_code_page = unsafe { GetConsoleCP() };
        // SAFETY: no preconditions.
        let output_code_page = unsafe { GetConsoleOutputCP() };
        let raw = Self {
            input,
            output,
            input_mode,
            output_mode,
            input_code_page,
            output_code_page,
        };
        set_console_mode(
            input,
            ENABLE_VIRTUAL_TERMINAL_INPUT | ENABLE_WINDOW_INPUT | ENABLE_EXTENDED_FLAGS,
        )?;
        set_console_mode(
            output,
            output_mode | ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING | DISABLE_NEWLINE_AUTO_RETURN,
        )?;
        // SAFETY: no preconditions; a failure leaves the code page as it was.
        unsafe { SetConsoleCP(CP_UTF8) };
        // SAFETY: as above.
        unsafe { SetConsoleOutputCP(CP_UTF8) };
        Ok(raw)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = set_console_mode(self.input, self.input_mode);
        let _ = set_console_mode(self.output, self.output_mode);
        // SAFETY: no preconditions.
        unsafe { SetConsoleCP(self.input_code_page) };
        // SAFETY: no preconditions.
        unsafe { SetConsoleOutputCP(self.output_code_page) };
    }
}

pub(super) fn size() -> Option<TtySize> {
    let output = std_handle(STD_OUTPUT_HANDLE)?;
    let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
    // SAFETY: a console output handle and a valid out-pointer.
    if unsafe { GetConsoleScreenBufferInfo(output, &raw mut info) } == 0 {
        return None;
    }
    let window = info.srWindow;
    let cols = u16::try_from(i32::from(window.Right) - i32::from(window.Left) + 1).ok()?;
    let rows = u16::try_from(i32::from(window.Bottom) - i32::from(window.Top) + 1).ok()?;
    Some(TtySize {
        grid: GridSize { cols, rows },
        pixels: None,
    })
}

pub(super) fn start_threads<E: From<TtyEvent> + Send + 'static>(
    raw: Option<&RawMode>,
    events: &Sender<E>,
) -> io::Result<()> {
    let input = events.clone();
    let console = raw.map(|raw| ConsoleInput(raw.input));
    let watch = console.is_some();
    thread::Builder::new()
        .name("attach-input".to_owned())
        .spawn(move || match console {
            Some(console) => read_console(&console, &input),
            None => read_piped(&input),
        })?;
    if watch {
        let resize = events.clone();
        thread::Builder::new()
            .name("attach-resize".to_owned())
            .spawn(move || watch_size(&resize))?;
    }
    Ok(())
}

/// Reports a change of the console's window size, polled: a console reports a change of its buffer's size as an input
/// record, but not every change of its window's (herdr relies on polling on Windows too).
fn watch_size<E: From<TtyEvent>>(events: &Sender<E>) {
    let mut last = size();
    loop {
        thread::sleep(SIZE_POLL);
        let now = size();
        if now != last {
            last = now;
            if events.send(TtyEvent::Resize.into()).is_err() {
                return;
            }
        }
    }
}

/// The console's input handle, moved to the reader.
struct ConsoleInput(HANDLE);

// SAFETY: a process-wide standard handle, valid from any thread.
unsafe impl Send for ConsoleInput {}

/// The console's input records until it fails: the characters of key presses (the VT bytes the console encodes the
/// keys, and the terminal's answers, as; NUL from the key a record without a character names, [`nul_key`]), and
/// window-size events.
fn read_console<E: From<TtyEvent>>(console: &ConsoleInput, events: &Sender<E>) {
    let mut records = [INPUT_RECORD::default(); RECORDS as usize];
    let mut decoder = Utf16Decoder::default();
    loop {
        let mut read = 0_u32;
        // SAFETY: a console input handle, a buffer of `RECORDS` records and a valid out-pointer.
        let ok = unsafe { ReadConsoleInputW(console.0, records.as_mut_ptr(), RECORDS, &raw mut read) };
        if ok == 0 {
            break;
        }
        let mut bytes = Vec::new();
        let mut resized = false;
        for record in records.iter().take(read as usize) {
            match u32::from(record.EventType) {
                KEY_EVENT => {
                    // SAFETY: the record is a key event, so the union holds a key event record.
                    let key = unsafe { record.Event.KeyEvent };
                    key_input(&key, &mut decoder, &mut bytes);
                }
                WINDOW_BUFFER_SIZE_EVENT => resized = true,
                _ => {}
            }
        }
        if !bytes.is_empty() && events.send(TtyEvent::Input(bytes).into()).is_err() {
            return;
        }
        if resized && events.send(TtyEvent::Resize.into()).is_err() {
            return;
        }
    }
    let _ = events.send(TtyEvent::InputEnd.into());
}

/// The bytes a key event record carries: a press's character, as many times as it repeats (NUL from the key when it
/// carries none, [`nul_key`]); nothing for a release.
fn key_input(key: &KEY_EVENT_RECORD, decoder: &mut Utf16Decoder, out: &mut Vec<u8>) {
    if key.bKeyDown == 0 {
        return;
    }
    // SAFETY: both members of the character union are plain data; the W API fills `UnicodeChar`.
    let unit = unsafe { key.uChar.UnicodeChar };
    for _ in 0..key.wRepeatCount.max(1) {
        if unit == 0 {
            out.extend_from_slice(nul_key(key.wVirtualKeyCode, key.dwControlKeyState));
        } else {
            decoder.push(unit, out);
        }
    }
}

/// The bytes of a key press whose record carries no character: NUL for Ctrl+Space, Ctrl+2 and Ctrl+@ (Ctrl+Shift+2),
/// as xterm sends them, ESC first with the left Alt; nothing for any other (a modifier alone, a key the console encodes
/// as nothing, a chord with the right Alt, which is `AltGr` with Ctrl on the layouts that have it).
fn nul_key(virtual_key: u16, control_state: u32) -> &'static [u8] {
    let ctrl = control_state & (LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED) != 0;
    let alt = control_state & LEFT_ALT_PRESSED != 0;
    let alt_gr = control_state & RIGHT_ALT_PRESSED != 0;
    match (virtual_key, ctrl && !alt_gr, alt) {
        (VK_SPACE | VK_2, true, false) => b"\0",
        (VK_SPACE | VK_2, true, true) => b"\x1b\0",
        _ => &[],
    }
}

/// UTF-16 code units to UTF-8, a surrogate pair split across records joined.
#[derive(Debug, Default)]
struct Utf16Decoder {
    high: Option<u16>,
}

impl Utf16Decoder {
    fn push(&mut self, unit: u16, out: &mut Vec<u8>) {
        if unit == 0 {
            return;
        }
        let units: Vec<u16> = match (self.high.take(), unit) {
            (None, 0xD800..=0xDBFF) => {
                self.high = Some(unit);
                return;
            }
            (Some(high), 0xDC00..=0xDFFF) => vec![high, unit],
            (_, unit) => vec![unit],
        };
        for decoded in char::decode_utf16(units) {
            let ch = decoded.unwrap_or(char::REPLACEMENT_CHARACTER);
            let mut buf = [0_u8; 4];
            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
        }
    }
}

/// A piped standard input's bytes as they come, until its end.
fn read_piped<E: From<TtyEvent>>(events: &Sender<E>) {
    let mut stdin = io::stdin().lock();
    let mut buf = vec![0_u8; READ_SIZE];
    loop {
        match stdin.read(&mut buf) {
            Ok(0) => break,
            Ok(read) => {
                if events.send(TtyEvent::Input(buf[..read].to_vec()).into()).is_err() {
                    return;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    let _ = events.send(TtyEvent::InputEnd.into());
}

pub(super) fn write_out(bytes: &[u8]) -> io::Result<()> {
    let stdout = io::stdout();
    let handle = stdout.as_raw_handle();
    // SAFETY: the process's standard output handle, borrowed for the write and never closed (`ManuallyDrop`).
    let mut file = ManuallyDrop::new(unsafe { File::from_raw_handle(handle) });
    file.write_all(bytes)?;
    file.flush()
}

#[cfg(test)]
mod tests {
    use windows_sys::Win32::System::Console::SHIFT_PRESSED;

    use super::*;

    #[test]
    fn a_record_without_a_character_is_nul_only_for_the_keys_that_send_it() {
        assert_eq!(nul_key(VK_SPACE, LEFT_CTRL_PRESSED), b"\0");
        assert_eq!(nul_key(VK_2, RIGHT_CTRL_PRESSED | SHIFT_PRESSED), b"\0", "Ctrl+@");
        assert_eq!(nul_key(VK_SPACE, LEFT_CTRL_PRESSED | LEFT_ALT_PRESSED), b"\x1b\0");
        // modifiers alone, and keys the console encodes as nothing
        assert_eq!(nul_key(0x11, LEFT_CTRL_PRESSED), b"");
        assert_eq!(nul_key(0x10, SHIFT_PRESSED), b"");
        assert_eq!(nul_key(VK_SPACE, 0), b"");
        assert_eq!(nul_key(VK_2, LEFT_CTRL_PRESSED | RIGHT_ALT_PRESSED), b"", "AltGr+2");
        assert_eq!(nul_key(0x7C, LEFT_CTRL_PRESSED), b"", "F13");
    }

    #[test]
    fn utf16_units_decode_to_utf8_with_split_surrogates() {
        let mut decoder = Utf16Decoder::default();
        let mut out = Vec::new();
        for unit in "a€".encode_utf16() {
            decoder.push(unit, &mut out);
        }
        let pair: Vec<u16> = "😀".encode_utf16().collect();
        decoder.push(pair[0], &mut out);
        decoder.push(pair[1], &mut out);
        decoder.push(0x1b, &mut out);
        assert_eq!(out, "a€😀\x1b".as_bytes());
    }
}
