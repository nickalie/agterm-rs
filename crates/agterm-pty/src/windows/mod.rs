//! The PTY on Windows: a pseudoconsole (`ConPTY`) through portable-pty, the child assigned as it is created to a job
//! object that kills it and every process it starts once the job's last handle closes.
//!
//! herdr: `src/pty/backend.rs` (`spawn_with_portable_pty`, its Windows form) and `src/platform/windows.rs`
//! (`StatusCommandGuard`'s kill-on-close job) at `HERDR_REV` (`build/pins.env`), licensed under the Apache License
//! 2.0, modified: the child is created inside the job (`build/patches/portable-pty/0004`) instead of started
//! suspended and assigned; the pipes are taken as owned handles (`0005`), so the actor holds what a handoff passes
//! on; a master is rebuilt from handed-over handles.

mod actor;

use std::fs::File;
use std::io;
use std::os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
use std::sync::Arc;

use portable_pty::win::conpty::{ConPtyHandoff, ConPtyMasterPty, ConPtySystem};
use portable_pty::{CommandBuilder, ExitStatus, MasterPty, PtySystem};
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
};

pub(crate) use actor::{Control, spawn_actor};

use crate::{Size, Spawned, pty_error};

/// A PTY's master side, for [`crate::Actor::spawn`]: the pseudoconsole and the two ends of its pipes this process
/// holds.
pub struct Master {
    conpty: Box<ConPtyMasterPty>,
    /// The write end of the console's input pipe.
    input: File,
    /// The read end of the console's output pipe.
    output: File,
}

impl std::fmt::Debug for Master {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Master")
            .field("input", &self.input)
            .field("output", &self.output)
            .finish_non_exhaustive()
    }
}

impl Master {
    /// A master over a session another process handed over (the session server's handoff), of the size the giver
    /// last set; it owns every handle of `handoff`.
    ///
    /// # Errors
    ///
    /// The pipes could not be taken from the rebuilt master.
    pub fn from_handoff(handoff: ConPtyHandoff, size: Size) -> io::Result<Self> {
        Self::new(Box::new(ConPtyMasterPty::from_handoff(handoff, size.pty())))
    }

    fn new(conpty: Box<ConPtyMasterPty>) -> io::Result<Self> {
        let (input, output) = conpty.take_pipes().map_err(|err| pty_error(&err))?;
        Ok(Self {
            conpty,
            input: File::from(input),
            output: File::from(output),
        })
    }
}

/// The child on a pseudoconsole, and the job that holds it and every process it starts.
#[derive(Debug)]
pub struct Child {
    inner: Box<dyn portable_pty::Child + Send + Sync>,
    pid: u32,
    /// Killed on close: the tree ends once this and every [`Killer`] are dropped.
    job: Arc<OwnedHandle>,
}

impl Child {
    /// The child's process id.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Wait for the child to exit.
    ///
    /// # Errors
    ///
    /// Waiting for the process handle failed.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.inner.wait()
    }

    /// The child's exit status if it has exited, else `None`.
    ///
    /// # Errors
    ///
    /// As [`Child::wait`].
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.inner.try_wait()
    }

    /// A handle that ends the child's tree from another thread, safe while this one waits.
    #[must_use]
    pub fn killer(&self) -> Killer {
        Killer {
            job: Arc::clone(&self.job),
        }
    }

    /// The job object the child's tree runs in, borrowed (a handoff duplicates it into the new owner, whose copy
    /// then keeps the tree alive).
    #[must_use]
    pub fn job(&self) -> BorrowedHandle<'_> {
        self.job.as_handle()
    }
}

/// Ends a child's tree: [`Child::killer`]. Holds the child's job open, so the tree outlives a dropped [`Child`]
/// while a killer is left.
#[derive(Debug, Clone)]
pub struct Killer {
    job: Arc<OwnedHandle>,
}

impl Killer {
    /// End the child's tree: Windows has no hangup to send, so this is [`Killer::kill_tree`].
    ///
    /// # Errors
    ///
    /// As [`Killer::kill_tree`].
    pub fn hang_up(&self) -> io::Result<()> {
        self.kill_tree()
    }

    /// Terminate every process of the child's job, the child too (exit code 1).
    ///
    /// # Errors
    ///
    /// `TerminateJobObject` failed.
    pub fn kill_tree(&self) -> io::Result<()> {
        // SAFETY: terminates the processes of a job handle this killer keeps open; no memory is passed.
        if unsafe { TerminateJobObject(self.job.as_raw_handle(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// A job object whose last handle's close kills every process in it.
fn kill_on_close_job() -> io::Result<OwnedHandle> {
    // SAFETY: creates an unnamed job with default security; no memory is passed.
    let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a fresh handle nothing else owns.
    let job = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let len = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
        .map_err(|_| io::Error::other("the job limits do not fit a u32"))?;
    // SAFETY: passes a live local of the class's struct type and its exact size, for a job handle owned here.
    let set = unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            len,
        )
    };
    if set == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(job)
}

pub(crate) fn spawn(builder: CommandBuilder, size: Size) -> io::Result<Spawned> {
    let job = kill_on_close_job()?;
    let pair = ConPtySystem::default()
        .openpty(size.pty())
        .map_err(|err| pty_error(&err))?;
    // the master spawns the child: the slave behind `PtyPair::slave` cannot be downcast
    drop(pair.slave);
    let master: Box<dyn MasterPty> = pair.master;
    let conpty = master
        .downcast::<ConPtyMasterPty>()
        .map_err(|_| io::Error::other("the PTY master is not a ConPTY master"))?;
    let child = conpty
        .spawn_command_in_job(builder, job.as_raw_handle())
        .map_err(|err| pty_error(&err))?;
    let pid = child
        .process_id()
        .ok_or_else(|| io::Error::other("the PTY child has no process id"))?;
    // on an error from here the job's close ends the child
    let master = Master::new(conpty)?;
    Ok(Spawned {
        master,
        child: Child {
            inner: child,
            pid,
            job: Arc::new(job),
        },
    })
}
