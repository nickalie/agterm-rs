//! A pane's mailbox: everything that reaches its IO worker ([`crate::pane`]) — the program's output from the PTY
//! actor's read callback, the frontend's clipboard answers, the child's exit, the end of the output, the pane's
//! shutdown — behind one lock and one condition variable, so the worker waits on all of them at once, with the
//! synchronized-output hold's deadline as its timeout.
//!
//! - The read callback ([`Mailbox::deliver`]) only copies the output in and never waits, so nothing that holds the
//!   VT lock can wait on a callback that waits on the VT.
//! - Flow control is the producer's: once the output not yet fed to the VT (queued, and the chunk the worker holds)
//!   reaches [`HIGH_WATER`], the callback pauses the actor's reads ([`agterm_pty::ReadPause`]) before it returns, so
//!   at most one more read lands; the worker resumes them once it is down to [`LOW_WATER`]. Both decisions are made
//!   under the mailbox's lock, so a stale resume never overrides a newer pause. While reads are paused the actor still
//!   writes the program's input — a worker waiting on a clipboard answer holds the program's output back, not its
//!   input.
//! - The child's exit and the shutdown are flags that pass queued output, so a clipboard wait never hides them.
//! - A live pane's reader queues the attach stream's events that must follow the output before them
//!   ([`crate::session_io::LiveMail`]) in order with it ([`Mailbox::deliver_live`]); they count no bytes.
//!
//! Lock rule: the mailbox's lock is a leaf. Nothing calls into the VT, the actor or the frontend's wake while holding
//! it.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use agterm_pty::ReadPause;

use crate::effects::ClipboardAnswer;
use crate::session_io::LiveMail;

/// The output bytes waiting for the VT at which the actor's reads pause.
pub(crate) const HIGH_WATER: usize = 1024 * 1024;
/// The output bytes waiting for the VT at which paused reads resume.
pub(crate) const LOW_WATER: usize = 256 * 1024;

/// What the worker takes next.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Mail {
    /// A read of the program's output.
    Output(Vec<u8>),
    /// A live pane's event, after the output before it.
    Live(LiveMail),
    /// The deadline passed with nothing to take.
    Idle,
    /// The output ended (every read was taken) or the pane shut down.
    End,
}

/// What a clipboard read the worker waits on came to.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Answer {
    Given(ClipboardAnswer),
    /// The child exited: nobody is left to read an answer.
    ChildExited,
    Shutdown,
    /// The deadline passed first.
    Idle,
}

/// What waits for the worker, in order.
#[derive(Debug)]
enum Item {
    Output(Vec<u8>),
    Live(LiveMail),
}

#[derive(Debug, Default)]
struct State {
    output: VecDeque<Item>,
    /// Output bytes not fed to the VT yet: the queued reads, and the one the worker holds.
    pending: usize,
    answers: Vec<(u64, ClipboardAnswer)>,
    eof: bool,
    child_exited: bool,
    shutdown: bool,
}

#[derive(Debug)]
pub(crate) struct Mailbox {
    state: Mutex<State>,
    changed: Condvar,
    pause: ReadPause,
}

impl Mailbox {
    /// A mailbox pausing an actor's reads through `pause`.
    pub(crate) fn new(pause: ReadPause) -> Self {
        Self {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            pause,
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Change the state and wake the worker.
    fn post(&self, change: impl FnOnce(&mut State)) {
        change(&mut self.lock());
        self.changed.notify_all();
    }

    /// The read callback's side: queue a read of the program's output, pausing the actor's reads at the high mark.
    pub(crate) fn deliver(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.post(|state| {
            state.output.push_back(Item::Output(bytes.to_vec()));
            state.pending += bytes.len();
            if state.pending >= HIGH_WATER {
                self.pause.pause();
            }
        });
    }

    /// A live pane's reader: queue `mail` after the output delivered so far.
    pub(crate) fn deliver_live(&self, mail: LiveMail) {
        self.post(|state| state.output.push_back(Item::Live(mail)));
    }

    /// The actor stopped: no more output comes.
    pub(crate) fn end_of_output(&self) {
        self.post(|state| state.eof = true);
    }

    /// The child exited: a clipboard read waiting now, or waited on later, is denied.
    pub(crate) fn child_exited(&self) {
        self.post(|state| state.child_exited = true);
    }

    /// The pane goes away: the worker stops at its next look.
    pub(crate) fn shutdown(&self) {
        self.post(|state| state.shutdown = true);
    }

    /// The frontend's answer to the clipboard read `id`.
    pub(crate) fn answer(&self, id: u64, answer: ClipboardAnswer) {
        self.post(|state| state.answers.push((id, answer)));
    }

    /// Wait on the state until `deadline` (forever without one); whether it passed.
    fn wait<'a>(&self, state: MutexGuard<'a, State>, deadline: Option<Instant>) -> (MutexGuard<'a, State>, bool) {
        let Some(deadline) = deadline else {
            return (self.changed.wait(state).unwrap_or_else(PoisonError::into_inner), false);
        };
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return (state, true);
        }
        let (state, _) = self
            .changed
            .wait_timeout(state, left)
            .unwrap_or_else(PoisonError::into_inner);
        // the clock decides, not the condvar's flag: Windows' timed wait can end a little before `left` has passed
        (state, Instant::now() >= deadline)
    }

    /// The worker's side: the next read of output, waiting for one until `deadline`. A read taken stays pending
    /// until [`Mailbox::consumed`].
    pub(crate) fn next(&self, deadline: Option<Instant>) -> Mail {
        let mut state = self.lock();
        loop {
            if state.shutdown {
                return Mail::End;
            }
            match state.output.pop_front() {
                Some(Item::Output(output)) => return Mail::Output(output),
                Some(Item::Live(mail)) => return Mail::Live(mail),
                None => {}
            }
            if state.eof {
                return Mail::End;
            }
            let (next, timed_out) = self.wait(state, deadline);
            state = next;
            if timed_out {
                return Mail::Idle;
            }
        }
    }

    /// `len` bytes of a taken read reached the VT. Whether reads were resumed: the caller wakes the actor's reader
    /// (after this returned, with the lock let go).
    pub(crate) fn consumed(&self, len: usize) -> bool {
        let mut state = self.lock();
        state.pending = state.pending.saturating_sub(len);
        if state.pending <= LOW_WATER && self.pause.is_paused() {
            self.pause.resume();
            return true;
        }
        false
    }

    /// The worker waits on the clipboard read `id` until `deadline`; answers to any other read are dropped.
    pub(crate) fn await_answer(&self, id: u64, deadline: Option<Instant>) -> Answer {
        let mut state = self.lock();
        loop {
            if state.shutdown {
                return Answer::Shutdown;
            }
            if state.child_exited {
                return Answer::ChildExited;
            }
            let answers = std::mem::take(&mut state.answers);
            if let Some((_, answer)) = answers.into_iter().find(|(answered, _)| *answered == id) {
                return Answer::Given(answer);
            }
            let (next, timed_out) = self.wait(state, deadline);
            state = next;
            if timed_out {
                return Answer::Idle;
            }
        }
    }

    /// Output bytes not fed to the VT yet.
    #[cfg(test)]
    pub(crate) fn pending(&self) -> usize {
        self.lock().pending
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;

    fn mailbox() -> (Mailbox, ReadPause) {
        let pause = ReadPause::new();
        (Mailbox::new(pause.clone()), pause)
    }

    #[test]
    fn output_comes_in_order_and_ends_after_the_last_read() {
        let (mailbox, _) = mailbox();
        mailbox.deliver(b"one");
        mailbox.deliver(b"");
        mailbox.deliver(b"two");
        mailbox.end_of_output();
        assert_eq!(mailbox.next(None), Mail::Output(b"one".to_vec()));
        assert_eq!(mailbox.next(None), Mail::Output(b"two".to_vec()));
        assert_eq!(mailbox.next(None), Mail::End);
        assert_eq!(mailbox.pending(), 6, "taken reads stay pending until consumed");
    }

    #[test]
    fn the_deadline_ends_a_wait_with_nothing_to_take() {
        let (mailbox, _) = mailbox();
        let started = Instant::now();
        assert_eq!(mailbox.next(Some(started + Duration::from_millis(30))), Mail::Idle);
        assert!(started.elapsed() >= Duration::from_millis(30));
        assert_eq!(
            mailbox.next(Some(started)),
            Mail::Idle,
            "a passed deadline does not wait"
        );
    }

    #[test]
    fn a_shutdown_passes_queued_output() {
        let (mailbox, _) = mailbox();
        mailbox.deliver(b"queued");
        mailbox.shutdown();
        assert_eq!(mailbox.next(None), Mail::End);
        assert_eq!(mailbox.await_answer(1, None), Answer::Shutdown);
    }

    #[test]
    fn reads_pause_at_the_high_mark_and_resume_at_the_low_one() {
        let (mailbox, pause) = mailbox();
        let chunk = vec![b'x'; HIGH_WATER / 4];
        for _ in 0..3 {
            mailbox.deliver(&chunk);
        }
        assert!(!pause.is_paused());
        mailbox.deliver(&chunk);
        assert!(pause.is_paused(), "paused at the high mark");
        // the read the worker holds still counts, so taking it resumes nothing
        assert!(matches!(mailbox.next(None), Mail::Output(_)));
        assert!(pause.is_paused());
        assert!(!mailbox.consumed(chunk.len()), "above the low mark");
        assert!(!mailbox.consumed(chunk.len()), "above the low mark");
        assert!(mailbox.consumed(chunk.len()), "down to the low mark");
        assert!(!pause.is_paused());
        assert!(!mailbox.consumed(chunk.len()), "not paused: nothing to resume");
        assert_eq!(mailbox.pending(), 0);
    }

    #[test]
    fn an_answer_serves_only_its_own_read() {
        let (mailbox, _) = mailbox();
        mailbox.answer(1, ClipboardAnswer::Denied);
        mailbox.answer(2, ClipboardAnswer::Text(Some("two".to_owned())));
        assert_eq!(
            mailbox.await_answer(2, None),
            Answer::Given(ClipboardAnswer::Text(Some("two".to_owned())))
        );
        // the stale answer to read 1 was dropped with it
        let deadline = Instant::now() + Duration::from_millis(20);
        assert_eq!(mailbox.await_answer(1, Some(deadline)), Answer::Idle);
    }

    #[test]
    fn the_childs_exit_ends_a_wait_on_an_answer_and_every_later_one() {
        let mailbox = Arc::new(mailbox().0);
        let waiter = {
            let mailbox = Arc::clone(&mailbox);
            std::thread::spawn(move || mailbox.await_answer(1, None))
        };
        mailbox.child_exited();
        assert_eq!(waiter.join().unwrap(), Answer::ChildExited);
        mailbox.answer(2, ClipboardAnswer::Denied);
        assert_eq!(mailbox.await_answer(2, None), Answer::ChildExited);
    }
}
