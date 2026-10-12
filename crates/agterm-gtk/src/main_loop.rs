//! [`MainLoop`] over a `GLib` main context: control connections post tasks from their own threads, and a future on the
//! context runs them one at a time, in posting order, with the state the main thread owns.

use std::cell::RefCell;
use std::rc::Rc;

use agterm_host::{MainLoop, MainLoopClosed, MainTask};
use gtk::glib;

/// The posting side, shared with the control server's threads.
pub struct GlibMainLoop<S> {
    sender: async_channel::Sender<MainTask<S>>,
    /// Kept to drop what is still queued at [`GlibMainLoop::close`]: the context's future, which holds the other
    /// receiver, never runs again once the loop has quit.
    receiver: async_channel::Receiver<MainTask<S>>,
}

impl<S: 'static> GlibMainLoop<S> {
    /// Starts running posted tasks on `context` with `state`. Call on the thread that owns `context`.
    #[must_use]
    pub fn attach(context: &glib::MainContext, state: Rc<RefCell<S>>) -> Self {
        let (sender, receiver) = async_channel::unbounded::<MainTask<S>>();
        let tasks = receiver.clone();
        context.spawn_local(async move {
            while let Ok(task) = tasks.recv().await {
                if let Ok(mut state) = state.try_borrow_mut() {
                    task(&mut state);
                } else {
                    // only a task spinning a nested main loop could get here; its waiter sees the dropped reply
                    tracing::error!("main-loop task dropped: the app state is in use");
                }
            }
        });
        Self { sender, receiver }
    }

    /// Stops accepting tasks and drops the queued ones unrun, so their waiters see the loop closed. Main thread only.
    pub fn close(&self) {
        self.sender.close();
        while self.receiver.try_recv().is_ok() {}
    }
}

impl<S: 'static> MainLoop<S> for GlibMainLoop<S> {
    fn post(&self, task: MainTask<S>) -> Result<(), MainLoopClosed> {
        self.sender.try_send(task).map_err(|_| MainLoopClosed)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;

    /// Iterates `context` until `done` holds, or panics after a few seconds.
    fn run_until(context: &glib::MainContext, done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "the main loop did not get there in time");
            context.iteration(false);
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn tasks_posted_from_other_threads_run_in_order_on_the_owning_thread() {
        let context = glib::MainContext::new();
        let state = Rc::new(RefCell::new(Vec::new()));
        let main_loop = context
            .with_thread_default(|| GlibMainLoop::attach(&context, Rc::clone(&state)))
            .unwrap();
        let owner = thread::current().id();
        let record = move |n: u32| -> MainTask<Vec<(u32, bool)>> {
            Box::new(move |seen: &mut Vec<(u32, bool)>| seen.push((n, thread::current().id() == owner)))
        };
        let poster = thread::scope(|scope| {
            scope
                .spawn(|| (0..20).try_for_each(|n| main_loop.post(record(n))))
                .join()
        });
        assert_eq!(poster.ok(), Some(Ok(())));
        run_until(&context, || state.borrow().len() == 20);
        let expected: Vec<(u32, bool)> = (0..20).map(|n| (n, true)).collect();
        assert_eq!(*state.borrow(), expected);
    }

    #[test]
    fn close_refuses_new_tasks_and_drops_the_queued_ones() {
        let context = glib::MainContext::new();
        let state = Rc::new(RefCell::new(0_u32));
        let main_loop = context
            .with_thread_default(|| GlibMainLoop::attach(&context, Rc::clone(&state)))
            .unwrap();
        let (reply_tx, reply_rx) = mpsc::channel::<()>();
        let queued: MainTask<u32> = Box::new(move |count: &mut u32| {
            *count += 1;
            reply_tx.send(()).unwrap();
        });
        main_loop.post(queued).unwrap();
        main_loop.close();
        assert_eq!(reply_rx.recv(), Err(mpsc::RecvError));
        assert_eq!(
            main_loop.post(Box::new(|count: &mut u32| *count += 1)),
            Err(MainLoopClosed)
        );
        while context.iteration(false) {}
        assert_eq!(*state.borrow(), 0);
    }
}
