//! [`MainLoop`] over GPUI's executors: control connections, the bus and the signal thread post tasks from their own
//! threads, and one foreground task runs them one at a time, in posting order, with the state the main thread owns.
//!
//! The posting side is a channel sender plus GPUI's background executor for the delayed tasks' timers; the
//! foreground executor is `!Send` and stays with the consumer ([`drive`]). A delayed task waits in a registry until
//! its timer fires, so [`GpuiMainLoop::close`] drops it (and whatever reply sender it holds) at once rather than at
//! its deadline.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use agterm_host::{MainLoop, MainLoopClosed, MainTask};
use gpui_kit::{App, BackgroundExecutor, Entity, Task};

/// The delayed tasks waiting for their timers, by id; `None` once the loop is closed.
type Delayed<S> = Mutex<Option<DelayedTasks<S>>>;

struct DelayedTasks<S> {
    next: u64,
    tasks: HashMap<u64, MainTask<S>>,
}

/// The posting side, shared with the threads that post.
pub struct GpuiMainLoop<S> {
    sender: async_channel::Sender<MainTask<S>>,
    /// Kept to drop what is still queued at [`GpuiMainLoop::close`].
    receiver: async_channel::Receiver<MainTask<S>>,
    delayed: Arc<Delayed<S>>,
    timers: BackgroundExecutor,
}

impl<S: 'static> GpuiMainLoop<S> {
    /// A loop whose delayed tasks wait on `timers`; nothing runs until [`drive`] consumes [`GpuiMainLoop::tasks`].
    #[must_use]
    pub fn new(timers: BackgroundExecutor) -> Self {
        let (sender, receiver) = async_channel::unbounded();
        Self {
            sender,
            receiver,
            delayed: Arc::new(Mutex::new(Some(DelayedTasks {
                next: 0,
                tasks: HashMap::new(),
            }))),
            timers,
        }
    }

    /// The consuming side, for [`drive`].
    #[must_use]
    pub fn tasks(&self) -> async_channel::Receiver<MainTask<S>> {
        self.receiver.clone()
    }

    /// Stops accepting tasks and drops the queued and the delayed ones unrun, so their waiters see the loop closed.
    pub fn close(&self) {
        self.sender.close();
        while self.receiver.try_recv().is_ok() {}
        let delayed = self.delayed.lock().unwrap_or_else(PoisonError::into_inner).take();
        drop(delayed);
    }
}

impl<S: 'static> MainLoop<S> for GpuiMainLoop<S> {
    fn post(&self, task: MainTask<S>) -> Result<(), MainLoopClosed> {
        self.sender.try_send(task).map_err(|_| MainLoopClosed)
    }

    /// A background timer moves the task from the registry into the queue when it fires; a closed loop has dropped
    /// it by then.
    fn post_after(&self, delay: Duration, task: MainTask<S>) -> Result<(), MainLoopClosed> {
        let id = {
            let mut delayed = self.delayed.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(delayed) = delayed.as_mut() else {
                return Err(MainLoopClosed);
            };
            let id = delayed.next;
            delayed.next += 1;
            delayed.tasks.insert(id, task);
            id
        };
        let registry = Arc::clone(&self.delayed);
        let sender = self.sender.clone();
        let timer = self.timers.timer(delay);
        self.timers
            .spawn(async move {
                timer.await;
                let task = registry
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .as_mut()
                    .and_then(|delayed| delayed.tasks.remove(&id));
                if let Some(task) = task {
                    // a loop closed in between refuses it, which drops it
                    let _ = sender.try_send(task);
                }
            })
            .detach();
        Ok(())
    }
}

/// Runs the tasks posted to `tasks` on the foreground, each with `state`, then `after` with the app context and the
/// state released (where what the task left to the frontend lands). Ends when the loop is closed.
pub fn drive<S: 'static>(
    tasks: async_channel::Receiver<MainTask<S>>,
    state: Entity<S>,
    after: impl Fn(&Entity<S>, &mut App) + 'static,
    cx: &mut App,
) -> Task<()> {
    cx.spawn(async move |cx| {
        while let Ok(task) = tasks.recv().await {
            cx.update(|cx| {
                state.update(cx, |state, _| task(state));
                after(&state, cx);
            });
        }
    })
}

#[cfg(test)]
mod tests;
