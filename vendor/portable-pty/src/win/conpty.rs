use crate::cmdbuilder::CommandBuilder;
use crate::win::psuedocon::PsuedoCon;
use crate::{Child, MasterPty, PtyPair, PtySize, PtySystem, SlavePty};
use anyhow::Error;
use filedescriptor::{FileDescriptor, Pipe};
use std::os::windows::io::{FromRawHandle, IntoRawHandle, OwnedHandle, RawHandle};
use std::sync::{Arc, Mutex};
use winapi::um::wincon::COORD;

#[derive(Default)]
pub struct ConPtySystem {}

impl PtySystem for ConPtySystem {
    fn openpty(&self, size: PtySize) -> anyhow::Result<PtyPair> {
        let stdin = Pipe::new()?;
        let stdout = Pipe::new()?;

        let con = PsuedoCon::new(
            COORD {
                X: size.cols as i16,
                Y: size.rows as i16,
            },
            stdin.read,
            stdout.write,
        )?;

        let master = ConPtyMasterPty {
            inner: Arc::new(Mutex::new(Inner {
                con,
                readable: stdout.read,
                writable: Some(stdin.write),
                size,
            })),
        };

        let slave = ConPtySlavePty {
            inner: master.inner.clone(),
        };

        Ok(PtyPair {
            master: Box::new(master),
            slave: Box::new(slave),
        })
    }
}

struct Inner {
    con: PsuedoCon,
    readable: FileDescriptor,
    writable: Option<FileDescriptor>,
    size: PtySize,
}

impl Inner {
    pub fn resize(
        &mut self,
        num_rows: u16,
        num_cols: u16,
        pixel_width: u16,
        pixel_height: u16,
    ) -> Result<(), Error> {
        self.con.resize(COORD {
            X: num_cols as i16,
            Y: num_rows as i16,
        })?;
        self.size = PtySize {
            rows: num_rows,
            cols: num_cols,
            pixel_width,
            pixel_height,
        };
        Ok(())
    }
}

#[derive(Clone)]
pub struct ConPtyMasterPty {
    inner: Arc<Mutex<Inner>>,
}

impl ConPtyMasterPty {
    /// Spawn `cmd` attached to this pseudoconsole, assigned to the job object
    /// `job` as it is created, so neither it nor any process it starts runs
    /// outside the job. `job` must stay open until this returns.
    pub fn spawn_command_in_job(
        &self,
        cmd: CommandBuilder,
        job: std::os::windows::io::RawHandle,
    ) -> anyhow::Result<Box<dyn Child + Send + Sync>> {
        let inner = self.inner.lock().unwrap();
        let child = inner.con.spawn_command_in_job(cmd, Some(job as _))?;
        Ok(Box::new(child))
    }

    /// The write end of the console's input pipe and the read end of its
    /// output pipe, in place of `take_writer` and `try_clone_reader`: the
    /// caller owns them, and can hand them to another process. Fails once the
    /// writer was taken.
    pub fn take_pipes(&self) -> anyhow::Result<(OwnedHandle, OwnedHandle)> {
        let mut inner = self.inner.lock().unwrap();
        let output = inner.readable.try_clone()?;
        let input = inner
            .writable
            .take()
            .ok_or_else(|| anyhow::anyhow!("writer already taken"))?;
        // SAFETY: both are open handles each FileDescriptor gives up here.
        unsafe {
            Ok((
                OwnedHandle::from_raw_handle(input.into_raw_handle()),
                OwnedHandle::from_raw_handle(output.into_raw_handle()),
            ))
        }
    }

    /// The signal, reference and console host process handles behind the
    /// pseudoconsole, borrowed (duplicate them to hand them over).
    ///
    /// # Safety
    ///
    /// The HPCON layout is undocumented: the caller has checked that the
    /// ConPTY implementation this process runs on keeps these three handles
    /// at the start of the struct an HPCON points at.
    pub unsafe fn console_handles(&self) -> [RawHandle; 3] {
        let inner = self.inner.lock().unwrap();
        inner.con.console_handles().map(|handle| handle as RawHandle)
    }

    /// A master over a session another process handed over: its pipe ends
    /// and console handles (as [`ConPtyMasterPty::console_handles`] orders
    /// them), all owned by the master from here on. The console's size is
    /// `size` as the giver last set it.
    pub fn from_handoff(handoff: ConPtyHandoff, size: PtySize) -> Self {
        let ConPtyHandoff {
            input,
            output,
            console,
        } = handoff;
        // SAFETY: each handle is open and owned, given up to the descriptor.
        let (readable, writable) = unsafe {
            (
                FileDescriptor::from_raw_handle(output.into_raw_handle()),
                FileDescriptor::from_raw_handle(input.into_raw_handle()),
            )
        };
        Self {
            inner: Arc::new(Mutex::new(Inner {
                con: PsuedoCon::from_console_handles(console),
                readable,
                writable: Some(writable),
                size,
            })),
        }
    }
}

/// The handles a ConPTY session is made of, as one process hands them to
/// another ([`ConPtyMasterPty::from_handoff`]).
pub struct ConPtyHandoff {
    /// The write end of the console's input pipe.
    pub input: OwnedHandle,
    /// The read end of the console's output pipe.
    pub output: OwnedHandle,
    /// The signal, reference and console host process handles.
    pub console: [OwnedHandle; 3],
}

pub struct ConPtySlavePty {
    inner: Arc<Mutex<Inner>>,
}

impl MasterPty for ConPtyMasterPty {
    fn resize(&self, size: PtySize) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        inner.resize(size.rows, size.cols, size.pixel_width, size.pixel_height)
    }

    fn get_size(&self) -> Result<PtySize, Error> {
        let inner = self.inner.lock().unwrap();
        Ok(inner.size.clone())
    }

    fn try_clone_reader(&self) -> anyhow::Result<Box<dyn std::io::Read + Send>> {
        Ok(Box::new(self.inner.lock().unwrap().readable.try_clone()?))
    }

    fn take_writer(&self) -> anyhow::Result<Box<dyn std::io::Write + Send>> {
        Ok(Box::new(
            self.inner
                .lock()
                .unwrap()
                .writable
                .take()
                .ok_or_else(|| anyhow::anyhow!("writer already taken"))?,
        ))
    }
}

impl SlavePty for ConPtySlavePty {
    fn spawn_command(&self, cmd: CommandBuilder) -> anyhow::Result<Box<dyn Child + Send + Sync>> {
        let inner = self.inner.lock().unwrap();
        let child = inner.con.spawn_command(cmd)?;
        Ok(Box::new(child))
    }
}
