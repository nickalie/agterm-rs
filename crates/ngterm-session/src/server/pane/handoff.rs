//! A pane's part in the update handoff ([`crate::server::handoff`]): held at a cut in the server that gives it, and
//! rebuilt from that cut, inert until the commit, in the server that takes it.
//!
//! herdr: `src/pane.rs` (`begin_handoff`, `duplicate_handoff_fd`, `handoff_runtime_state`, `preserve_for_handoff`,
//! `from_handoff_fd`, `assume_handoff_ownership`) and `src/handoff_runtime.rs` (`HandoffRuntimeState`) at `HERDR_REV`
//! (`build/pins.env`), licensed under the Apache License 2.0, modified: the terminal passes (libghostty-vt's binary
//! snapshot, or its formatter's VT restoration where the two servers' libghostty-vt differ; what neither carries is
//! below) in place of herdr's 8 KiB of history seeded into a fresh terminal; the replies not yet written pass too;
//! the cut is a freeze acknowledged by the replies' helper and fenced from the child's waiter; agent state, titles and
//! input state are herdr's own and not here.
//!
//! # The cut
//!
//! 1. [`Pane::freeze`]: the replies' helper finishes the batch it took and then neither writes nor ends anything; the
//!    child's waiter, should the program end now, publishes nothing until a thaw (a pane already ending is left out).
//! 2. [`Pane::quiesce`]: the PTY actor writes what it queued and stops; output a Windows actor read meanwhile goes
//!    into the terminal first, so the actor holds nothing.
//! 3. [`Pane::cut`]: a waiting clipboard read is denied and the Kitty barriers end, then the terminal's state, the
//!    output offset, the revision and the replies ready to be written are taken, all under the pane's lock.
//!
//! What the terminal's state leaves behind at this pin: neither encoding carries Kitty images, their placements or
//! glyph registrations (Ghostty's snapshot codec ignores them on capture, and the formatter has no extra for them,
//! as for attach: [`crate::server::replies`]), so re-placing an earlier image by its id fails in the server's
//! terminal after a handoff; and the formatter has no saved-cursor extra, so across a pin change a later restore
//! (`ESC 8`) puts the server's cursor at the default position, and its CPR and DSR answers and later snapshots follow
//! it until the program moves the cursor. Carrying them needs a libghostty-vt that exposes them.
//!
//! A rollback ([`Pane::resume`]) undoes the first two, the replies still queued; a commit ([`Pane::hand_over`])
//! releases the PTY and leaves the pane frozen, ending nothing when dropped.
//!
//! # The import
//!
//! [`Pane::import`] rebuilds the terminal from the cut, with no client and no reply owner, the replies queued first,
//! the PTY actor started quiesced over the handed-over PTY, and the helper and the waiter started frozen: nothing it
//! does can fail later. [`Pane::activate`], on the commit, makes the actor the PTY's owner and thaws the rest.
//!
//! On Unix the import drives the PTY first, a resize to the size it has (which signals nothing to the program). On
//! Windows it leaves the pseudoconsole alone: every `ResizePseudoConsole`, to the size it has too, makes conhost
//! repaint its viewport from its own buffer, over what [`Pane::print`] wrote into the session's terminal alone (in
//! the old server's too, should the handoff roll back); the handles' checks stand in for it (`handoff::windows`).
//!
//! On Unix the program is not a child of the server that took it (herdr's `ChildExitReason::Handoff`): its end is
//! the PTY's end or its pid no longer naming it (its start time, from the process table), and it ends as
//! [`ExitStatus::Unknown`]. On Windows the program's process handle passed too, so its exit code is read as before.

use std::io;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use ngterm_ghostty_vt as vt;
#[cfg(unix)]
use ngterm_pty::ActorHandle;
use ngterm_pty::{Child, Master};

use super::{
    Condvar, DELEGATION_WAIT, ExitStatus, HELPER_IDLE, KITTY_WAIT, Lead, NEXT_PANE, Outlet, Pane, Parts, Replies,
    SNAPSHOT_WAIT, assemble, configure, server_terminal, snapshot, start_actor, start_helper, vt_error,
};
use crate::protocol::GridSize;
use crate::server::clients::{Clients, DEFAULT_QUEUE_LIMIT, ResyncBudget};
use crate::server::handoff::{PaneState, VtEncoding};

/// How often an adopted program's watcher looks at the process table while the PTY lasts.
#[cfg(unix)]
const WATCH_POLL: Duration = Duration::from_millis(250);

/// The tunables of a pane an update handoff brings over; a new session takes them from its
/// [`super::PaneConfig`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub client_queue_limit: usize,
    pub resync_budget: ResyncBudget,
    pub snapshot_wait: Duration,
    pub continuation_limit: usize,
    pub delegation_wait: Duration,
    pub kitty_wait: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            client_queue_limit: DEFAULT_QUEUE_LIMIT,
            resync_budget: ResyncBudget::default(),
            snapshot_wait: SNAPSHOT_WAIT,
            continuation_limit: snapshot::CONTINUATION_LIMIT,
            delegation_wait: DELEGATION_WAIT,
            kitty_wait: KITTY_WAIT,
        }
    }
}

/// A pane at an update handoff's cut: what the manifest names of it, its terminal's state in the encoding the
/// manifest names, and the replies the program was not written yet, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cut {
    pub state: PaneState,
    pub vt: Vec<u8>,
    pub replies: Vec<u8>,
}

/// A program an update handoff brought over on Unix: its start time, which tells it from a later process that got
/// its pid (`None` when the table could not be read at the import, when only the pid is compared).
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Adopted {
    started: Option<u64>,
}

#[cfg(unix)]
impl Adopted {
    /// The program `pid` as the process table shows it now; `None` when it is not there (it ended already).
    fn of(pid: u32) -> Option<Self> {
        let Ok(pid) = i32::try_from(pid) else {
            return None;
        };
        match crate::job::read_table() {
            None => Some(Self { started: None }),
            Some(table) => table.iter().find(|entry| entry.record.pid == pid).map(|entry| Self {
                started: Some(entry.record.started),
            }),
        }
    }

    /// Whether `pid` still names the program: in the table with its start time. An unreadable table counts as alive
    /// (nothing is known to have ended).
    pub(super) fn alive(&self, pid: u32) -> bool {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        crate::job::read_table().is_none_or(|table| {
            table.iter().any(|entry| {
                entry.record.pid == pid && self.started.is_none_or(|started| entry.record.started == started)
            })
        })
    }
}

/// The program's process handle and the job its tree runs in, duplicated when the pane starts, so an update handoff
/// can pass them on while the child's waiter holds the child.
#[cfg(windows)]
#[derive(Debug)]
pub(super) struct Origin {
    process: std::os::windows::io::OwnedHandle,
    job: std::os::windows::io::OwnedHandle,
}

#[cfg(windows)]
impl Origin {
    pub(super) fn of(child: &Child) -> io::Result<Self> {
        let process = child
            .process()
            .ok_or_else(|| io::Error::other("the program has no process handle"))?;
        Ok(Self {
            process: process.try_clone_to_owned()?,
            job: child.job().try_clone_to_owned()?,
        })
    }
}

impl Pane {
    /// Step 1 of the cut (module docs): hold the replies' helper and the child's waiter. `false`, holding nothing, when
    /// the program ended or is ending: the pane is left out of the handoff.
    pub(crate) fn freeze(&self) -> bool {
        let mut state = self.core.lock();
        if state.exit.is_some() || state.hold.ending {
            return false;
        }
        state.hold.frozen = true;
        while state.hold.helper_busy {
            state = self
                .core
                .changed
                .wait_timeout(state, HELPER_IDLE)
                .map_or_else(|poisoned| poisoned.into_inner().0, |(guard, _)| guard);
        }
        true
    }

    /// Undo [`Pane::freeze`]: the helper writes what is ready, the waiter publishes an end it saw.
    pub(crate) fn thaw(&self) {
        self.core.lock().hold.frozen = false;
        self.core.changed.notify_all();
        self.core.timer.notify_all();
    }

    /// Step 2 of the cut (module docs): the PTY actor quiesced, within `timeout`, with nothing held.
    ///
    /// # Errors
    ///
    /// The actor did not quiesce ([`ngterm_pty::ActorHandle::begin_handoff`]); it runs again then.
    pub(crate) fn quiesce(&self, timeout: Duration) -> io::Result<()> {
        self.actor.begin_handoff(timeout)?;
        self.actor.deliver_held();
        Ok(())
    }

    /// A rollback: the PTY actor runs again ([`ngterm_pty::ActorHandle::rollback_handoff`], when it was quiesced),
    /// then the pane thaws.
    pub(crate) fn resume(&self, quiesced: bool) {
        if quiesced && let Err(err) = self.actor.rollback_handoff() {
            tracing::warn!(session = %self.name, %err, "a live session's PTY did not resume after a failed handoff");
        }
        self.thaw();
    }

    /// Step 3 of the cut (module docs), with the terminal in `encoding`.
    ///
    /// # Errors
    ///
    /// The terminal could not be encoded: a formatter snapshot of a session on its alternate screen (it would lose
    /// the primary screen), or one without a cut, or a binary snapshot the library refused.
    pub(crate) fn cut(&self, encoding: VtEncoding) -> Result<Cut, String> {
        let mut state = self.core.lock();
        state.settle_for_handoff();
        let vt = match encoding {
            VtEncoding::Binary => state.terminal.encode_snapshot().map_err(|err| {
                format!(
                    "the terminal of the live session {:?} could not be encoded: {err}",
                    self.name
                )
            })?,
            VtEncoding::Formatter if state.terminal.alternate_screen() => {
                return Err(format!(
                    "the live session {:?} shows its alternate screen, whose primary screen the formatter cannot carry \
                     to a server of another libghostty-vt",
                    self.name
                ));
            }
            VtEncoding::Formatter => super::capture(&mut state, snapshot::Kind::Attach)
                .map_err(|err| format!("the live session {:?}: {err}", self.name))?,
        };
        let replies = self.core.outlet.ready_replies().concat();
        let state = PaneState {
            name: self.name.clone(),
            pid: self.pid,
            created_at: self.created_at,
            tracked_env: self.tracked_env.clone(),
            term: self.term.clone(),
            size: state.size,
            offset: state.offset,
            revision: state.revision,
            vt: encoding,
            vt_len: vt.len() as u64,
            replies_len: replies.len() as u64,
        };
        Ok(Cut { state, vt, replies })
    }

    /// A duplicate of the PTY's master for the server taking the pane over; only while quiesced.
    ///
    /// # Errors
    ///
    /// As [`ngterm_pty::ActorHandle::duplicate_for_handoff`].
    #[cfg(unix)]
    pub(crate) fn master(&self) -> io::Result<std::os::fd::OwnedFd> {
        self.actor.duplicate_for_handoff()
    }

    /// Duplicates of the pseudoconsole's pipes and handles for the server taking the pane over; only while quiesced.
    ///
    /// # Safety
    ///
    /// As [`ngterm_pty::ActorHandle::duplicate_for_handoff`]: the caller's layout gate passed for this `ConPTY`.
    ///
    /// # Errors
    ///
    /// As [`ngterm_pty::ActorHandle::duplicate_for_handoff`].
    #[cfg(windows)]
    pub(crate) unsafe fn console(&self) -> io::Result<ngterm_pty::ConPtyHandoff> {
        // SAFETY: the caller's contract.
        unsafe { self.actor.duplicate_for_handoff() }
    }

    /// The program's process handle and its job, borrowed, for the server taking the pane over.
    #[cfg(windows)]
    pub(crate) fn origin(
        &self,
    ) -> (
        std::os::windows::io::BorrowedHandle<'_>,
        std::os::windows::io::BorrowedHandle<'_>,
    ) {
        use std::os::windows::io::AsHandle as _;
        (self.origin.process.as_handle(), self.origin.job.as_handle())
    }

    /// The commit: another server owns the pane now. The PTY actor lets go of the PTY without reading or writing
    /// again, the pane stays frozen, and dropping it ends nothing.
    pub(crate) fn hand_over(&self) {
        self.handed_over.store(true, Ordering::Release);
        if let Err(err) = self.actor.release_after_commit() {
            tracing::warn!(session = %self.name, %err, "a handed-over live session's PTY actor did not let go");
        }
    }

    /// The pane `state` names, rebuilt over the handed-over `master` and `child` from its terminal `vt` and its
    /// `replies` (module docs), inert until [`Pane::activate`]; dropped before that, it ends nothing.
    ///
    /// # Errors
    ///
    /// The terminal could not be rebuilt, the handed-over PTY could not be driven (Unix), or a thread could not
    /// start.
    pub(crate) fn import(
        state: &PaneState,
        vt: &[u8],
        replies: &[u8],
        (master, child): (Master, Child),
        limits: Limits,
    ) -> io::Result<Self> {
        let size = GridSize {
            cols: state.size.cols.max(1),
            rows: state.size.rows.max(1),
        };
        let outlet = Arc::new(Outlet::default());
        let timer = Arc::new(Condvar::new());
        let mut session_replies = Replies::ownerless(
            Arc::clone(&outlet),
            Arc::clone(&timer),
            (limits.delegation_wait, limits.kitty_wait),
        );
        let terminal = imported_terminal(state, vt, size, limits, &outlet, &mut session_replies)?;
        let parts = Parts {
            terminal,
            clients: Clients::new(limits.client_queue_limit),
            lead: Lead::new(),
            replies: session_replies,
            offset: state.offset,
            revision: state.revision,
            size,
            frozen: true,
        };
        let core = assemble(parts, timer, outlet);
        if !replies.is_empty() {
            core.outlet.push(replies);
        }
        let id = NEXT_PANE.fetch_add(1, Ordering::Relaxed);
        let killer = child.killer();
        let pid = child.pid();
        #[cfg(windows)]
        let origin = Origin::of(&child)?;
        #[cfg(unix)]
        let adopted = Adopted::of(pid).unwrap_or(Adopted { started: None });
        let actor = start_actor(id, master, &core, true)?;
        // the pseudoconsole is left alone on Windows (module docs)
        #[cfg(unix)]
        let started = check_driven(&actor, size).and_then(|()| start_helper(id, &core, &actor));
        #[cfg(windows)]
        let started = start_helper(id, &core, &actor);
        let started = started.and_then(|()| {
            #[cfg(unix)]
            {
                start_watcher(id, (&killer, pid, adopted), &core, &actor)
            }
            #[cfg(windows)]
            {
                super::start_waiter(id, child, &core, &actor)
            }
        });
        if let Err(err) = started {
            // the actor never owned the PTY: its shutdown leaves it to the server that does
            actor.shutdown();
            core.end(ExitStatus::Unknown);
            return Err(err);
        }
        Ok(Self {
            id,
            name: state.name.clone(),
            pid,
            created_at: state.created_at,
            tracked_env: state.tracked_env.clone(),
            resync_budget: limits.resync_budget,
            snapshot_wait: limits.snapshot_wait,
            core,
            actor,
            killer,
            term: state.term.clone(),
            // not this server's until the commit
            handed_over: std::sync::atomic::AtomicBool::new(true),
            #[cfg(unix)]
            adopted: Some(adopted),
            #[cfg(windows)]
            origin,
        })
    }

    /// The commit, in the server that took the pane: the PTY actor owns the PTY (reading and writing, the replies
    /// carried over first), the helper and the waiter thaw, and the pane is this server's to end.
    pub(crate) fn activate(&self) {
        if let Err(err) = self.actor.rollback_handoff() {
            tracing::warn!(session = %self.name, %err, "a handed-over live session's PTY actor did not take the PTY");
        }
        self.handed_over.store(false, Ordering::Release);
        self.thaw();
    }
}

/// The terminal of an imported session of `size` from its cut `vt`, set up as the server's, the replies' scanner
/// brought in step with its parser.
fn imported_terminal(
    state: &PaneState,
    vt: &[u8],
    size: GridSize,
    limits: Limits,
    outlet: &Arc<Outlet>,
    replies: &mut Replies,
) -> io::Result<vt::Terminal> {
    let term = state.term.as_deref();
    match state.vt {
        VtEncoding::Binary => {
            let options = vt::SnapshotOptions {
                max_continuation_bytes: Some(limits.continuation_limit),
                retain_continuation: true,
                compress_history: false,
            };
            let mut terminal = vt::Terminal::decode_snapshot(vt, options).map_err(vt_error)?;
            if (terminal.cols(), terminal.rows()) != (size.cols, size.rows) {
                terminal.resize(size.cols, size.rows, 0, 0).map_err(vt_error)?;
            }
            configure(&mut terminal, size, outlet, replies.facts(), term)?;
            // the unfinished sequence the cut stood in: the scanner follows the parser into it
            if let Ok(vt::Continuation::Bytes(continuation)) = terminal.continuation() {
                replies.observe(&continuation);
            }
            Ok(terminal)
        }
        VtEncoding::Formatter => {
            let mut terminal = server_terminal(size, limits.continuation_limit, outlet, replies.facts(), term)?;
            // the terminal's replies to its own restoration go to no program
            outlet.discard(true);
            terminal.vt_write(vt);
            outlet.discard(false);
            replies.observe(vt);
            Ok(terminal)
        }
    }
}

/// Whether this server can drive the handed-over PTY: a resize to the size it has. Not on Windows (module docs: a
/// resize there repaints over printed text).
#[cfg(unix)]
fn check_driven(actor: &ActorHandle, size: GridSize) -> io::Result<()> {
    actor
        .try_resize(ngterm_pty::Size::cells(size.rows, size.cols))
        .map_err(|err| {
            io::Error::new(
                err.kind(),
                format!("the handed-over terminal could not be driven: {err}"),
            )
        })
}

/// The watcher of a program an update handoff brought over on Unix, which this server cannot wait for: once the PTY
/// ended or its pid no longer names it, the end is published (after the PTY's end, within [`EXIT_GRACE`]) with an
/// unknown status, and its group is never signalled again.
#[cfg(unix)]
fn start_watcher(
    id: u64,
    (killer, pid, adopted): (&ngterm_pty::Killer, u32, Adopted),
    core: &Arc<super::Core>,
    actor: &ActorHandle,
) -> io::Result<()> {
    let (core, actor, killer) = (Arc::clone(core), actor.clone(), killer.clone());
    std::thread::Builder::new()
        .name(format!("live-{id}-watch"))
        .spawn(move || {
            while !core.wait_for(WATCH_POLL, |state| state.pty_ended) {
                if !adopted.alive(pid) {
                    break;
                }
            }
            core.hold_for_handoff();
            core.child_exited(&actor);
            actor.child_exited();
            core.wait_for(super::EXIT_GRACE, |state| state.pty_ended);
            killer.retire();
            actor.shutdown();
            tracing::debug!(pid, "a handed-over live session's program ended");
            core.end(ExitStatus::Unknown);
        })
        .map(drop)
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::protocol::SessionSpec;
    use crate::server::pane::PaneConfig;

    const PATIENCE: Duration = Duration::from_secs(60);

    /// A shell that runs each line it reads (as the integration tests' `support::shell`).
    fn shell() -> SessionSpec {
        let argv: &[&str] = if cfg!(windows) {
            &["cmd.exe", "/d", "/q", "/k"]
        } else {
            &["/bin/sh", "-c", "while read line; do eval \"$line\"; done"]
        };
        SessionSpec {
            argv: argv.iter().map(|arg| (*arg).to_owned()).collect(),
            env: std::env::vars()
                .filter(|(key, _)| {
                    let key = key.to_ascii_uppercase();
                    !key.starts_with("NGTERM_") && !key.starts_with("AGTERM_") && !key.starts_with("ZMX_")
                })
                .collect(),
            cwd: None,
            tracked_env: Vec::new(),
        }
    }

    fn type_line(pane: &Pane, line: &str) {
        let enter = if cfg!(windows) { "\r" } else { "\n" };
        pane.write(format!("{line}{enter}").as_bytes()).unwrap();
    }

    /// The screen of `pane`, scrollback included, once it holds `needle`.
    fn wait_screen(pane: &Pane, needle: &str) -> String {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let screen = pane
                .observe(|terminal, revision| crate::server::api::read_screen(terminal, revision, true))
                .and_then(Result::ok)
                .map(|screen| screen.text)
                .unwrap_or_default();
            if screen.contains(needle) {
                return screen;
            }
            assert!(Instant::now() < deadline, "{needle:?} never showed: {screen}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The PTY and program of `pane`, as a handoff passes them (here into this same process).
    fn taken_over(pane: &Pane) -> (Master, Child) {
        #[cfg(unix)]
        {
            (Master::from_fd(pane.master().unwrap()), Child::adopt(pane.pid()))
        }
        #[cfg(windows)]
        {
            assert_eq!(
                crate::server::handoff::Support::of_this_host(),
                crate::server::handoff::Support::Supported
            );
            // SAFETY: the layout gate passes on this host (just above).
            let console = unsafe { pane.console() }.unwrap();
            let (process, job) = pane.origin();
            let child = Child::from_handoff(process.try_clone_to_owned().unwrap(), job.try_clone_to_owned().unwrap());
            (
                Master::from_handoff(console, ngterm_pty::Size::cells(24, 80)).unwrap(),
                child.unwrap(),
            )
        }
    }

    /// The line that makes [`shell`] print `got:<word>` (its echo, `got^:` or `got"":`, does not match).
    fn tagged(word: &str) -> String {
        if cfg!(windows) {
            format!("echo got^:{word}")
        } else {
            format!("echo got\"\":{word}")
        }
    }

    /// Text `print` wrote on `pane`'s screen, the shell's output after it: on Windows the pseudoconsole's buffer
    /// never holds it, so a resize of the pseudoconsole, which repaints the screen from that buffer, would show.
    const PRINTED: &str = "printed-mark";

    fn print_on_screen(pane: &Pane) {
        pane.print(PRINTED.as_bytes()).unwrap();
        type_line(pane, &tagged("printed"));
        wait_screen(pane, "got:printed");
    }

    /// A pane held at its cut in `encoding`, rebuilt from it over its own PTY and made the PTY's owner, as a
    /// replacement server does: the scrollback, the screen, what `print` wrote on it, and the shell carry over.
    fn crosses(encoding: VtEncoding) {
        let namespace = tempfile::tempdir().unwrap();
        let config = PaneConfig::new("crossing", namespace.path(), shell(), GridSize { cols: 80, rows: 24 });
        let (pane, _creator) = Pane::spawn(config).unwrap();
        let numbered = if cfg!(windows) {
            "for /L %i in (1,1,50) do @echo line%i"
        } else {
            "i=1; while [ $i -le 50 ]; do echo line$i; i=$((i+1)); done"
        };
        type_line(&pane, numbered);
        wait_screen(&pane, "line50");
        print_on_screen(&pane);

        assert!(pane.freeze());
        pane.quiesce(Duration::from_secs(5)).unwrap();
        let cut = pane.cut(encoding).unwrap();
        assert_eq!(cut.state.vt, encoding);
        assert_eq!(cut.state.vt_len, cut.vt.len() as u64);
        assert_eq!(cut.state.pid, pane.pid());
        let imported = Pane::import(&cut.state, &cut.vt, &cut.replies, taken_over(&pane), Limits::default()).unwrap();
        // not this server's until activated: a drop before would end nothing
        assert!(imported.handed_over.load(Ordering::Acquire));
        pane.hand_over();
        imported.activate();

        let screen = wait_screen(&imported, "line50");
        assert!(screen.contains("line1\n"), "the scrollback did not cross: {screen}");
        assert!(screen.contains(PRINTED), "the printed text did not cross: {screen}");
        assert_eq!(imported.offset(), cut.state.offset);
        type_line(&imported, &tagged("after"));
        let screen = wait_screen(&imported, "got:after");
        assert!(screen.contains(PRINTED), "the printed text did not last: {screen}");
        assert!(imported.kill(true).unwrap());
        drop(pane);
    }

    #[test]
    fn a_pane_crosses_by_the_binary_snapshot() {
        crosses(VtEncoding::Binary);
    }

    #[test]
    fn a_pane_crosses_by_the_formatter() {
        crosses(VtEncoding::Formatter);
    }

    /// A pane rebuilt in a replacement that never commits, dropped there, and its cut rolled back: the import touched
    /// nothing of the PTY (on Windows no resize repainted over what `print` wrote), and the shell runs on.
    #[test]
    fn an_import_rolled_back_leaves_the_screen_and_the_shell() {
        let namespace = tempfile::tempdir().unwrap();
        let config = PaneConfig::new(
            "uncommitted",
            namespace.path(),
            shell(),
            GridSize { cols: 80, rows: 24 },
        );
        let (pane, _creator) = Pane::spawn(config).unwrap();
        type_line(&pane, &tagged("ready"));
        wait_screen(&pane, "got:ready");
        print_on_screen(&pane);

        assert!(pane.freeze());
        pane.quiesce(Duration::from_secs(5)).unwrap();
        let cut = pane.cut(VtEncoding::Binary).unwrap();
        let imported = Pane::import(&cut.state, &cut.vt, &cut.replies, taken_over(&pane), Limits::default()).unwrap();
        drop(imported);
        pane.resume(true);

        type_line(&pane, &tagged("resumed"));
        let screen = wait_screen(&pane, "got:resumed");
        assert!(screen.contains(PRINTED), "the printed text did not last: {screen}");
        assert!(pane.kill(true).unwrap());
    }

    #[test]
    fn a_rolled_back_pane_runs_on_with_the_replies_it_held() {
        let namespace = tempfile::tempdir().unwrap();
        let config = PaneConfig::new("rollback", namespace.path(), shell(), GridSize { cols: 80, rows: 24 });
        let (pane, _creator) = Pane::spawn(config).unwrap();
        assert!(pane.freeze());
        pane.quiesce(Duration::from_secs(5)).unwrap();
        // a reply made while frozen waits, and is carried by the cut, until the rollback writes it (a line the shell
        // runs, here, so its arrival shows)
        let held = if cfg!(windows) {
            "echo got^:held\r"
        } else {
            "echo got\"\":held\n"
        };
        pane.core.outlet.push(held.as_bytes());
        let cut = pane.cut(VtEncoding::Binary).unwrap();
        assert_eq!(cut.replies, held.as_bytes());
        assert_eq!(
            pane.core.outlet.ready_replies(),
            [held.as_bytes().to_vec()],
            "kept for a rollback"
        );
        pane.resume(true);
        wait_screen(&pane, "got:held");
        let tagged = if cfg!(windows) {
            "echo got^:resumed"
        } else {
            "echo got\"\":resumed"
        };
        type_line(&pane, tagged);
        wait_screen(&pane, "got:resumed");
        assert!(pane.kill(true).unwrap());
    }

    #[test]
    fn an_ended_pane_is_not_frozen() {
        let namespace = tempfile::tempdir().unwrap();
        let program = SessionSpec {
            argv: if cfg!(windows) {
                vec![
                    "cmd.exe".to_owned(),
                    "/d".to_owned(),
                    "/c".to_owned(),
                    "exit 0".to_owned(),
                ]
            } else {
                vec!["/bin/sh".to_owned(), "-c".to_owned(), "exit 0".to_owned()]
            },
            ..shell()
        };
        let config = PaneConfig::new("ended", namespace.path(), program, GridSize { cols: 80, rows: 24 });
        let (pane, _creator) = Pane::spawn(config).unwrap();
        assert!(pane.wait_exit(PATIENCE).is_some());
        assert!(!pane.freeze());
    }
}
