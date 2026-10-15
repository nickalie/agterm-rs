//! SIGTERM quits without a prompt (`conformance/docs/rust-build.md`: within 10 s): a thread of its own waits for the
//! signal and posts the quit to the main loop, whose consumer runs the orderly shutdown (the quit save, the control
//! channel stopped) and GPUI's quit. A second SIGTERM gets the default action, as in the GTK build. Installed only
//! once this process is the primary: a launch that forwards to a running agterm keeps the default action.

use std::io;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread;

use signal_hook::consts::SIGTERM;
use signal_hook::iterator::Signals;

use crate::state::{AppState, Poster, post};

/// Handles the next SIGTERM through the main loop behind `poster`; a second one terminates the process.
///
/// # Errors
///
/// The handler could not be registered or its thread not started.
pub fn quit_on_sigterm(poster: Poster) -> io::Result<()> {
    // registered first, so it sees the flag the next registration sets on the first signal
    let seen = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register_conditional_default(SIGTERM, Arc::clone(&seen))?;
    signal_hook::flag::register(SIGTERM, seen)?;
    let mut signals = Signals::new([SIGTERM])?;
    thread::Builder::new()
        .name("agterm-sigterm".to_owned())
        .spawn(move || {
            if signals.forever().next().is_some() {
                tracing::info!("SIGTERM: quitting");
                post(&poster, AppState::request_quit);
            }
        })?;
    Ok(())
}
