//! A terminal created before any hook install seals the process-global hooks empty, so a later install cannot write
//! the native globals while terminals may be reading them (its own test binary: the hooks are process-wide).

use ngterm_ghostty_vt::Terminal;
use ngterm_ghostty_vt::sys::{self, DecodedImage, Hooks};

fn decode(_: &[u8]) -> Option<DecodedImage> {
    None
}

#[test]
fn a_terminal_first_seals_the_hooks_empty() {
    let mut terminal = Terminal::new(10, 2).unwrap();
    terminal.vt_write(b"running");
    assert!(!sys::install(Hooks {
        png: Some(decode),
        log: None,
    }));
    assert!(sys::installed().is_some_and(|hooks| hooks.png.is_none() && hooks.log.is_none()));
}
