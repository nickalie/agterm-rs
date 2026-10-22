//! The spawn on macOS: `posix_spawn` with a session of the child's own, every descriptor but stdio closed, and the
//! child made responsible for itself.
//!
//! herdr: `src/platform/mod.rs` (`detach_server_daemon_command`, `current_process_is_detached_server_daemon`) at
//! `HERDR_REV` (`build/pins.env`), licensed under the Apache License 2.0, modified. Changed: `posix_spawn` with
//! `POSIX_SPAWN_SETSID` in place of herdr's `setsid` in `pre_exec`, and `responsibility_spawnattrs_setdisclaim` in
//! place of herdr's launchd job and Mach bootstrap context, as upstream agterm's session host spawns
//! (`agtermCore/Sources/NgtermResponsibility/Responsibility.swift` `spawnDisclaimed`, resolved through `dlsym` and
//! required, as upstream's `ClientConnector.ensureRunning` requires it), so the server and its sessions keep their own
//! permission identity rather than the app's; plus `POSIX_SPAWN_CLOEXEC_DEFAULT` (Task 2's probe: a stray inherited
//! PTY master held an exiting shell in the tty drain), an empty signal mask and default dispositions, so nothing the
//! launcher blocked or ignored reaches the server; the server is detached when it leads its own session and is
//! responsible for itself; a thread reaps the child.

use std::ffi::{CString, OsStr, c_char, c_int, c_short, c_void};
use std::io;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::process::ExitStatusExt as _;
use std::process::ExitStatus;
use std::ptr;

use super::reaper;
use super::{ServerCommand, SpawnPath};

pub(super) use reaper::Process;

/// `<spawn.h>`'s `POSIX_SPAWN_SETSID`, which the libc crate exports for Linux only.
const POSIX_SPAWN_SETSID: c_int = 0x0400;

/// `responsibility_spawnattrs_setdisclaim(attr, disclaim)`.
type SetDisclaim = unsafe extern "C" fn(*mut libc::posix_spawnattr_t, c_int) -> c_int;
/// `responsibility_get_pid_responsible_for_pid(pid)`.
type ResponsibleFor = unsafe extern "C" fn(libc::pid_t) -> libc::pid_t;

pub(super) fn spawn(command: &ServerCommand) -> io::Result<(u32, SpawnPath, Process)> {
    let disclaim = set_disclaim().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "the responsibility SPI (responsibility_spawnattrs_setdisclaim) is unavailable",
        )
    })?;
    let program = c_string(command.executable.as_os_str())?;
    let mut argv = vec![program.clone()];
    for arg in &command.args {
        argv.push(c_string(arg)?);
    }
    let env = command
        .env
        .iter()
        .map(|(name, value)| {
            let mut entry = name.clone();
            entry.push("=");
            entry.push(value);
            c_string(&entry)
        })
        .collect::<io::Result<Vec<_>>>()?;
    let pid = posix_spawn(&program, &argv, &env, disclaim)?;
    let process = Process::reap(move || wait(pid))?;
    Ok((pid.cast_unsigned(), SpawnPath::Detached, process))
}

pub(super) fn current_process_is_detached() -> bool {
    let pid = rustix::process::getpid();
    let leads_its_session = rustix::process::getsid(None).is_ok_and(|session| session == pid);
    let raw = pid.as_raw_nonzero().get();
    // without the SPI to read it, responsibility is not asked for
    let responsible_for_itself = responsible_for().is_none() || responsible_pid(raw) == Some(raw);
    leads_its_session && responsible_for_itself
}

fn c_string(value: &OsStr) -> io::Result<CString> {
    CString::new(value.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} holds a NUL byte", value.to_string_lossy()),
        )
    })
}

/// A NULL-terminated array of the strings' pointers, valid while `strings` lives.
fn pointers(strings: &[CString]) -> Vec<*mut c_char> {
    strings
        .iter()
        .map(|string| string.as_ptr().cast_mut())
        .chain(std::iter::once(ptr::null_mut()))
        .collect()
}

/// `posix_spawn(program, argv, env)` with the server's attributes and its stdio on `/dev/null`.
fn posix_spawn(program: &CString, argv: &[CString], env: &[CString], disclaim: SetDisclaim) -> io::Result<libc::pid_t> {
    let mut attributes = Attributes::new()?;
    attributes.configure(disclaim)?;
    let mut actions = FileActions::new()?;
    actions.null_stdio()?;
    let argv = pointers(argv);
    let env = pointers(env);
    let mut pid: libc::pid_t = 0;
    // SAFETY: every pointer is valid for the call: the program and the NULL-terminated argv and env arrays point into
    // CStrings that outlive it, the attributes and actions are initialized, and `pid` is writable.
    let result = unsafe {
        libc::posix_spawn(
            &raw mut pid,
            program.as_ptr(),
            &raw const actions.0,
            &raw const attributes.0,
            argv.as_ptr(),
            env.as_ptr(),
        )
    };
    check(result, "posix_spawn")?;
    Ok(pid)
}

/// Initialized spawn attributes, destroyed on drop.
struct Attributes(libc::posix_spawnattr_t);

impl Attributes {
    fn new() -> io::Result<Self> {
        let mut attributes = MaybeUninit::<libc::posix_spawnattr_t>::uninit();
        // SAFETY: posix_spawnattr_init initializes the attributes `attributes` points to.
        let result = unsafe { libc::posix_spawnattr_init(attributes.as_mut_ptr()) };
        check(result, "posix_spawnattr_init")?;
        // SAFETY: initialized just above.
        Ok(Self(unsafe { attributes.assume_init() }))
    }

    /// A session of its own, nothing inherited but stdio, an empty signal mask, default dispositions, and the
    /// child responsible for itself.
    fn configure(&mut self, disclaim: SetDisclaim) -> io::Result<()> {
        let flags = POSIX_SPAWN_SETSID
            | libc::POSIX_SPAWN_CLOEXEC_DEFAULT
            | libc::POSIX_SPAWN_SETSIGMASK
            | libc::POSIX_SPAWN_SETSIGDEF;
        let flags = c_short::try_from(flags).map_err(io::Error::other)?;
        // SAFETY: the attributes are initialized; the flags are <spawn.h>'s.
        let result = unsafe { libc::posix_spawnattr_setflags(&raw mut self.0, flags) };
        check(result, "posix_spawnattr_setflags")?;
        let mut empty = MaybeUninit::<libc::sigset_t>::uninit();
        // SAFETY: sigemptyset initializes the set `empty` points to.
        unsafe { libc::sigemptyset(empty.as_mut_ptr()) };
        // SAFETY: initialized just above.
        let empty = unsafe { empty.assume_init() };
        // SAFETY: the attributes are initialized, and the set outlives the call.
        let result = unsafe { libc::posix_spawnattr_setsigmask(&raw mut self.0, &raw const empty) };
        check(result, "posix_spawnattr_setsigmask")?;
        let mut every = MaybeUninit::<libc::sigset_t>::uninit();
        // SAFETY: sigfillset initializes the set `every` points to.
        unsafe { libc::sigfillset(every.as_mut_ptr()) };
        // SAFETY: initialized just above.
        let every = unsafe { every.assume_init() };
        // SAFETY: the attributes are initialized, and the set outlives the call.
        let result = unsafe { libc::posix_spawnattr_setsigdefault(&raw mut self.0, &raw const every) };
        check(result, "posix_spawnattr_setsigdefault")?;
        // SAFETY: the symbol has this signature (upstream's `Responsibility.swift`), and the attributes are initialized.
        let result = unsafe { disclaim(&raw mut self.0, 1) };
        check(result, "responsibility_spawnattrs_setdisclaim")
    }
}

impl Drop for Attributes {
    fn drop(&mut self) {
        // SAFETY: the attributes were initialized and are destroyed once.
        unsafe { libc::posix_spawnattr_destroy(&raw mut self.0) };
    }
}

/// Initialized file actions, destroyed on drop.
struct FileActions(libc::posix_spawn_file_actions_t);

impl FileActions {
    fn new() -> io::Result<Self> {
        let mut actions = MaybeUninit::<libc::posix_spawn_file_actions_t>::uninit();
        // SAFETY: posix_spawn_file_actions_init initializes the actions `actions` points to.
        let result = unsafe { libc::posix_spawn_file_actions_init(actions.as_mut_ptr()) };
        check(result, "posix_spawn_file_actions_init")?;
        // SAFETY: initialized just above.
        Ok(Self(unsafe { actions.assume_init() }))
    }

    /// Stdin, stdout and stderr on `/dev/null`, opened in the child (and so inherited past `POSIX_SPAWN_CLOEXEC_DEFAULT`).
    fn null_stdio(&mut self) -> io::Result<()> {
        for descriptor in 0..3 {
            // SAFETY: the actions are initialized, and the path is a NUL-terminated literal.
            let result = unsafe {
                libc::posix_spawn_file_actions_addopen(
                    &raw mut self.0,
                    descriptor,
                    c"/dev/null".as_ptr(),
                    libc::O_RDWR,
                    0,
                )
            };
            check(result, "posix_spawn_file_actions_addopen")?;
        }
        Ok(())
    }
}

impl Drop for FileActions {
    fn drop(&mut self) {
        // SAFETY: the actions were initialized and are destroyed once.
        unsafe { libc::posix_spawn_file_actions_destroy(&raw mut self.0) };
    }
}

fn check(result: c_int, operation: &str) -> io::Result<()> {
    if result == 0 {
        return Ok(());
    }
    let error = io::Error::from_raw_os_error(result);
    Err(io::Error::new(error.kind(), format!("{operation}: {error}")))
}

/// Waits for the child `pid` and its status.
fn wait(pid: libc::pid_t) -> io::Result<ExitStatus> {
    loop {
        let mut status: c_int = 0;
        // SAFETY: `status` is writable for the call.
        if unsafe { libc::waitpid(pid, &raw mut status, 0) } == pid {
            return Ok(ExitStatus::from_raw(status));
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// A function of the responsibility SPI, resolved among the loaded system libraries as upstream does.
fn symbol(name: &std::ffi::CStr) -> Option<*mut c_void> {
    // SAFETY: RTLD_DEFAULT searches the loaded images; the name is NUL-terminated.
    let address = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
    (!address.is_null()).then_some(address)
}

fn set_disclaim() -> Option<SetDisclaim> {
    symbol(c"responsibility_spawnattrs_setdisclaim").map(|address| {
        // SAFETY: the symbol is a function of this signature (upstream's `Responsibility.swift`).
        unsafe { std::mem::transmute::<*mut c_void, SetDisclaim>(address) }
    })
}

fn responsible_for() -> Option<ResponsibleFor> {
    symbol(c"responsibility_get_pid_responsible_for_pid").map(|address| {
        // SAFETY: the symbol is a function of this signature (upstream's `Responsibility.swift`).
        unsafe { std::mem::transmute::<*mut c_void, ResponsibleFor>(address) }
    })
}

/// The pid responsible for `pid`, where the SPI can read it.
#[must_use]
pub fn responsible_pid(pid: i32) -> Option<i32> {
    let read = responsible_for()?;
    // SAFETY: the symbol has this signature, and takes no pointers.
    let responsible = unsafe { read(pid) };
    (responsible > 0).then_some(responsible)
}
