//! SIGTERM quits without a prompt (`conformance/docs/rust-build.md`: within 10 s): a thread of its own waits for the
//! signal and posts the quit to the main loop, whose consumer runs the orderly shutdown (the quit save, the control
//! channel stopped) and GPUI's quit. A second SIGTERM gets the default action, as in the GTK build. Installed only
//! once this process is the primary: a launch that forwards to a running ngterm keeps the default action.

use std::io;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread;

use signal_hook::SigId;
use signal_hook::consts::SIGTERM;
use signal_hook::iterator::Signals;

use crate::state::{AppState, Poster, post};

/// Handles the next SIGTERM through the main loop behind `poster`; a second one terminates the process.
///
/// # Errors
///
/// The handler could not be registered or its thread not started; nothing of it stays installed then, so SIGTERM
/// keeps its default action.
pub fn quit_on_sigterm(poster: Poster) -> io::Result<()> {
    let mut signals = Signals::new([SIGTERM])?;
    let flags = second_sigterm_terminates()?;
    let spawned = thread::Builder::new().name("ngterm-sigterm".to_owned()).spawn(move || {
        if signals.forever().next().is_some() {
            tracing::info!("SIGTERM: quitting");
            post(&poster, AppState::request_quit);
        }
    });
    if let Err(error) = spawned {
        // the closure, and with it the iterator's registration, was dropped with the failed spawn
        unregister(&flags);
        return Err(error);
    }
    Ok(())
}

/// The first SIGTERM sets a flag, and a SIGTERM finding it set gets the default action. The conditional default is
/// registered first, so it sees the flag the next registration sets on the first signal.
fn second_sigterm_terminates() -> io::Result<[SigId; 2]> {
    let seen = Arc::new(AtomicBool::new(false));
    let default = signal_hook::flag::register_conditional_default(SIGTERM, Arc::clone(&seen))?;
    match signal_hook::flag::register(SIGTERM, seen) {
        Ok(flag) => Ok([default, flag]),
        Err(error) => {
            unregister(&[default]);
            Err(error)
        }
    }
}

fn unregister(ids: &[SigId]) {
    for id in ids {
        signal_hook::low_level::unregister(*id);
    }
}
