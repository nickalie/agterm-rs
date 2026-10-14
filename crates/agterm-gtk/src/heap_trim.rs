//! Hands the heap a burst of terminal work freed back to the system. Linux glue: upstream has no counterpart, as
//! macOS's allocator returns free pages on its own.
//!
//! Every session's terminal is created up front (the eager deck), and a terminal's first frame compiles the GL
//! driver's shader variants, whose temporaries land between long-lived allocations. glibc keeps the freed pages in
//! its arenas, where they stay resident: with a software GL driver that is about 2 MB per terminal, more than the
//! terminal itself keeps live. `malloc_trim` releases every whole free page of every arena, so a burst of creations,
//! first frames or teardowns schedules one trim once it settles.

use std::cell::RefCell;
use std::time::Duration;

use gtk::glib;

/// How long terminal work must be quiet before the trim: a restore or a sweep through the sessions trims once.
const SETTLE: Duration = Duration::from_secs(2);

thread_local! {
    /// The pending trim, pushed back by every new request.
    static PENDING: RefCell<Option<glib::SourceId>> = const { RefCell::new(None) };
}

/// Schedules a trim once terminal work has been quiet for [`SETTLE`]; a request while one is pending restarts the wait.
pub fn trim_soon() {
    let source = glib::timeout_add_local_once(SETTLE, || {
        PENDING.with(|pending| pending.borrow_mut().take());
        // SAFETY: malloc_trim takes no pointers and only returns free pages to the system, under the arenas' locks.
        unsafe { libc::malloc_trim(0) };
    });
    if let Some(previous) = PENDING.with(|pending| pending.borrow_mut().replace(source)) {
        previous.remove();
    }
}
