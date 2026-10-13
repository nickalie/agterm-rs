//! The unit tests' one GTK thread: GTK refuses a second initialising thread and libtest runs every test on its own,
//! so tests that need GTK hand their body to this thread, one at a time, in place of initialising GTK themselves.

use std::panic::{self, AssertUnwindSafe};
use std::sync::{OnceLock, mpsc};
use std::thread;

type Job = Box<dyn FnOnce() + Send>;

/// The GTK thread's job queue; `None` when GTK has no display.
static GTK_THREAD: OnceLock<Option<mpsc::Sender<Job>>> = OnceLock::new();

/// Runs `test` on the GTK thread and hands back its result, or a panic of it; `None` (the test skips) when GTK has no
/// display.
pub fn on_gtk_thread<T: Send + 'static>(test: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let jobs = GTK_THREAD.get_or_init(spawn).as_ref()?;
    let (done, result) = mpsc::channel();
    jobs.send(Box::new(move || {
        let _ = done.send(panic::catch_unwind(AssertUnwindSafe(test)));
    }))
    .expect("the GTK thread runs for the whole test run");
    match result.recv().expect("the GTK thread answers every job") {
        Ok(value) => Some(value),
        Err(payload) => panic::resume_unwind(payload),
    }
}

fn spawn() -> Option<mpsc::Sender<Job>> {
    let (ready, initialised) = mpsc::channel();
    let (jobs, queue) = mpsc::channel::<Job>();
    thread::Builder::new()
        .name("gtk".into())
        .spawn(move || {
            if let Err(err) = adw::init() {
                eprintln!("GTK tests skipped: GTK has no display ({err})");
                let _ = ready.send(false);
                return;
            }
            let _ = ready.send(true);
            for job in queue {
                job();
            }
        })
        .expect("spawn the GTK thread");
    initialised.recv().unwrap_or(false).then_some(jobs)
}
