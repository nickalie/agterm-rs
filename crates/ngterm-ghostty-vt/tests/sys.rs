//! The process-global hooks, in a test binary of their own (they are process-wide and installed once): the PNG decoder
//! serves a Kitty graphics PNG transmission, the log sink receives the library's warnings, and a second install keeps
//! the first hooks.

use std::sync::atomic::{AtomicUsize, Ordering};

use ngterm_ghostty_vt::sys::{self, DecodedImage, Hooks, LogLevel};
use ngterm_ghostty_vt::{Effects, Terminal};

/// Every effect left at its default; installing it turns the clipboard write path (and its base64 warning) on.
struct Defaults;

impl Effects for Defaults {}

static DECODES: AtomicUsize = AtomicUsize::new(0);
static WARNINGS: AtomicUsize = AtomicUsize::new(0);

fn decode(png: &[u8]) -> Option<DecodedImage> {
    DECODES.fetch_add(1, Ordering::SeqCst);
    // not a real decoder: any payload starting with the PNG signature becomes one red 1x1 pixel
    png.starts_with(b"\x89PNG").then(|| DecodedImage {
        width: 1,
        height: 1,
        rgba: vec![255, 0, 0, 255],
    })
}

fn log(level: LogLevel, _scope: &str, _message: &str) {
    if matches!(level, LogLevel::Warning | LogLevel::Error) {
        WARNINGS.fetch_add(1, Ordering::SeqCst);
    }
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[(n >> (18 - 6 * i) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[test]
fn hooks_are_installed_once_and_serve_every_terminal() {
    assert!(sys::install(Hooks {
        png: Some(decode),
        log: Some(log),
    }));
    assert!(!sys::install(Hooks { png: None, log: None }));
    assert!(sys::installed().is_some_and(|hooks| hooks.png.is_some()));

    let mut terminal = Terminal::new(20, 5).unwrap();
    terminal.set_effects(Box::new(Defaults)).unwrap();
    terminal.set_kitty_image_storage_limit(1 << 20).unwrap();
    let png = base64(b"\x89PNG\r\n\x1a\nnot really a png");
    terminal.vt_write(format!("\x1b_Ga=t,f=100,i=7,q=2;{png}\x1b\\").as_bytes());
    assert_eq!(DECODES.load(Ordering::SeqCst), 1);

    // an OSC 52 write that is not base64 is logged as a warning and ignored
    let before = WARNINGS.load(Ordering::SeqCst);
    terminal.vt_write(b"\x1b]52;c;not*base64\x07");
    assert!(WARNINGS.load(Ordering::SeqCst) > before);
}
