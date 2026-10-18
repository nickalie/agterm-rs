# Appendix: the session-server spike's probes

Sources of the throwaway programs behind `docs/spikes/20261018-session-server.md` (Task 2 of
`docs/plans/20261018-zig-free-core.md`), kept here so the results can be rerun; none of this is built by the
workspace. They ran from the git-ignored `.scratch/session-spike/`.

## `win-handoff/Cargo.toml`

Windows ConPTY handoff probe: manifest.

```toml
[package]
name = "win-handoff-probe"
version = "0.0.0"
edition = "2024"
publish = false

[[bin]]
name = "probe"
path = "src/main.rs"

[dependencies]
windows-sys = { version = "0.61.2", features = [
    "Win32_Foundation",
    "Win32_Security",
    "Win32_Storage_FileSystem",
    "Win32_System_Console",
    "Win32_System_JobObjects",
    "Win32_System_Pipes",
    "Win32_System_Threading",
    "Win32_System_IO",
] }

[workspace]
```

## `win-handoff/src/main.rs`

Windows ConPTY handoff probe (`probe run <dir>`; `PROBE_NO_BREAKAWAY=1` for a job that refuses breakaway).

```rust
//! Throwaway probe for Task 2 of docs/plans/20261018-zig-free-core.md (not merged): can a ConPTY session pass from
//! one detached server process to another while its shell keeps running?
//!
//! `probe run <dir>` drives the whole probe and exits 0 on GO:
//! 1. `launcher` puts itself in a kill-on-close job (as a terminal or IDE would), starts `serverA` detached
//!    (DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB) and exits, closing the job;
//! 2. `serverA` creates a pseudoconsole, starts `cmd.exe` in it inside a KILL_ON_JOB_CLOSE job, sets a variable in
//!    the shell, starts `serverB` detached, duplicates every handle the session needs into it (the pipe ends, the
//!    pseudoconsole's three internal handles, the job, the shell process) and exits without ClosePseudoConsole;
//! 3. `serverB` waits for A to be gone, checks the shell lives, types into it and reads the variable back, resizes
//!    the pseudoconsole through a rebuilt HPCON, then ends the shell by closing the job and the console by closing
//!    the signal pipe, and writes `result.txt`.
#![allow(unsafe_code, clippy::all)]

use std::ffi::c_void;
use std::fs;
use std::io::Write as _;
use std::os::windows::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::Console::*;
use windows_sys::Win32::System::JobObjects::*;
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::*;

fn wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

struct Log(PathBuf);
impl Log {
    fn line(&self, msg: &str) {
        let mut f = fs::OpenOptions::new().create(true).append(true).open(&self.0).unwrap();
        let _ = writeln!(f, "[{:>6}] {}", std::process::id(), msg);
    }
}

fn last_err() -> String {
    format!("{}", std::io::Error::last_os_error())
}

fn exe() -> String {
    std::env::current_exe().unwrap().to_string_lossy().into_owned()
}

/// CreateProcessW for `cmdline` with `flags`, no handle inheritance; returns (process, thread, pid) or the error.
fn spawn(cmdline: &str, flags: u32) -> Result<(HANDLE, HANDLE, u32), String> {
    let mut cmd = wide(cmdline);
    let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        CreateProcessW(
            std::ptr::null(),
            cmd.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            flags,
            std::ptr::null(),
            std::ptr::null(),
            &si,
            &mut pi,
        )
    };
    if ok == 0 { Err(last_err()) } else { Ok((pi.hProcess, pi.hThread, pi.dwProcessId)) }
}

fn kill_on_close_job(breakaway_ok: bool) -> HANDLE {
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        assert!(!job.is_null(), "CreateJobObjectW: {}", last_err());
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            | if breakaway_ok { JOB_OBJECT_LIMIT_BREAKAWAY_OK } else { 0 };
        let ok = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        assert!(ok != 0, "SetInformationJobObject: {}", last_err());
        job
    }
}

fn write_all(h: HANDLE, data: &[u8]) -> bool {
    let mut off = 0;
    while off < data.len() {
        let mut n = 0u32;
        let ok = unsafe { WriteFile(h, data[off..].as_ptr(), (data.len() - off) as u32, &mut n, std::ptr::null_mut()) };
        if ok == 0 {
            return false;
        }
        off += n as usize;
    }
    true
}

/// Drains `h` on a thread into `sink` (and the log file `raw`), so ConPTY never blocks on output.
fn drain(h: HANDLE, sink: Arc<Mutex<Vec<u8>>>, raw: PathBuf) {
    let h = h as usize;
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            let mut n = 0u32;
            let ok = unsafe { ReadFile(h as HANDLE, buf.as_mut_ptr(), buf.len() as u32, &mut n, std::ptr::null_mut()) };
            if ok == 0 || n == 0 {
                break;
            }
            sink.lock().unwrap().extend_from_slice(&buf[..n as usize]);
            if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&raw) {
                let _ = f.write_all(&buf[..n as usize]);
            }
        }
    });
}

fn wait_for(sink: &Arc<Mutex<Vec<u8>>>, needle: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if String::from_utf8_lossy(&sink.lock().unwrap()).contains(needle) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn alive(process: HANDLE) -> bool {
    unsafe { WaitForSingleObject(process, 0) == WAIT_TIMEOUT }
}

fn launcher(dir: &Path) {
    let log = Log(dir.join("probe.log"));
    // the launcher sits in a kill-on-close job, as a process started by Windows Terminal or an IDE may
    // PROBE_NO_BREAKAWAY: a job that refuses breakaway, as some IDEs and CI agents set up
    let breakaway_ok = std::env::var_os("PROBE_NO_BREAKAWAY").is_none();
    let job = kill_on_close_job(breakaway_ok);
    let ok = unsafe { AssignProcessToJobObject(job, GetCurrentProcess()) };
    log.line(&format!("launcher: in kill-on-close job (breakaway allowed: {breakaway_ok}): {}", ok != 0));
    let cmd = format!("\"{}\" serverA \"{}\"", exe(), dir.display());
    let flags = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB;
    match spawn(&cmd, flags) {
        Ok((p, t, pid)) => {
            log.line(&format!("launcher: serverA pid {pid} started with breakaway"));
            unsafe {
                CloseHandle(p);
                CloseHandle(t);
            }
        }
        Err(e) => {
            log.line(&format!("launcher: breakaway refused ({e}), starting without it"));
            let (p, t, pid) = spawn(&cmd, DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP).expect("spawn serverA");
            log.line(&format!("launcher: serverA pid {pid} started without breakaway"));
            unsafe {
                CloseHandle(p);
                CloseHandle(t);
            }
        }
    }
    // closing the last job handle kills every process still in the job
    unsafe { CloseHandle(job) };
    log.line("launcher: exiting, job closed");
}

fn server_a(dir: &Path) {
    let log = Log(dir.join("probe.log"));
    unsafe {
        let (mut in_read, mut in_write, mut out_read, mut out_write) =
            (std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());
        assert!(CreatePipe(&mut in_read, &mut in_write, std::ptr::null(), 0) != 0);
        assert!(CreatePipe(&mut out_read, &mut out_write, std::ptr::null(), 0) != 0);
        let mut hpc: HPCON = 0;
        let hr = CreatePseudoConsole(COORD { X: 80, Y: 25 }, in_read, out_write, 0, &mut hpc);
        assert!(hr >= 0, "CreatePseudoConsole hr={hr:#x}");
        CloseHandle(in_read);
        CloseHandle(out_write);
        // the HPCON points at kernelbase's { hSignal, hPtyReference, hConPtyProcess }
        let inner = *(hpc as *const [HANDLE; 3]);
        log.line(&format!("serverA: pseudoconsole created, inner handles {inner:?}"));

        // cmd.exe attached to the pseudoconsole, suspended until it is in its job
        let mut size = 0usize;
        InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size);
        let mut attrs = vec![0u8; size];
        let list = attrs.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        assert!(InitializeProcThreadAttributeList(list, 1, 0, &mut size) != 0);
        assert!(
            UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                hpc as *const c_void,
                std::mem::size_of::<HPCON>(),
                std::ptr::null_mut(),
                std::ptr::null(),
            ) != 0
        );
        let mut si: STARTUPINFOEXW = std::mem::zeroed();
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        si.lpAttributeList = list;
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        let mut cmd = wide("cmd.exe /q");
        let ok = CreateProcessW(
            std::ptr::null(),
            cmd.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT,
            std::ptr::null(),
            std::ptr::null(),
            &si.StartupInfo,
            &mut pi,
        );
        assert!(ok != 0, "CreateProcessW cmd: {}", last_err());
        let job = kill_on_close_job(false);
        let assigned = AssignProcessToJobObject(job, pi.hProcess);
        ResumeThread(pi.hThread);
        CloseHandle(pi.hThread);
        log.line(&format!("serverA: shell pid {} in kill-on-close job: {}", pi.dwProcessId, assigned != 0));

        let sink = Arc::new(Mutex::new(Vec::new()));
        drain(out_read, sink.clone(), dir.join("a-output.raw"));
        write_all(in_write, b"set PROBE=alive-before-handoff\r\necho A-READY %PROBE%\r\n");
        let ready = wait_for(&sink, "A-READY alive-before-handoff", Duration::from_secs(15));
        log.line(&format!("serverA: shell answered before handoff: {ready}"));

        // the replacement server, detached as A is; it inherits nothing, every handle is duplicated into it
        let cmd = format!("\"{}\" serverB \"{}\"", exe(), dir.display());
        let (b_proc, b_thread, b_pid) = spawn(&cmd, DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_SUSPENDED)
            .expect("spawn serverB");
        let dup = |h: HANDLE| -> usize {
            let mut out = std::ptr::null_mut();
            let ok = DuplicateHandle(GetCurrentProcess(), h, b_proc, &mut out, 0, 0, DUPLICATE_SAME_ACCESS);
            assert!(ok != 0, "DuplicateHandle: {}", last_err());
            out as usize
        };
        let handed = [
            dup(in_write),
            dup(out_read),
            dup(inner[0]),
            dup(inner[1]),
            dup(inner[2]),
            dup(job),
            dup(pi.hProcess),
        ];
        let manifest = format!(
            "{} {} {} {} {} {} {} {} {}",
            handed[0], handed[1], handed[2], handed[3], handed[4], handed[5], handed[6], pi.dwProcessId,
            std::process::id()
        );
        fs::write(dir.join("handoff.tmp"), manifest).unwrap();
        fs::rename(dir.join("handoff.tmp"), dir.join("handoff.txt")).unwrap();
        ResumeThread(b_thread);
        log.line(&format!("serverA: handed the session to serverB pid {b_pid}; exiting without ClosePseudoConsole"));
        // exit as a replaced server would: every handle of A closes with the process
        ExitProcess(0);
    }
}

fn server_b(dir: &Path) {
    let log = Log(dir.join("probe.log"));
    let mut verdict = Vec::new();
    let manifest_path = dir.join("handoff.txt");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !manifest_path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let manifest = fs::read_to_string(&manifest_path).expect("handoff manifest");
    let v: Vec<usize> = manifest.split_whitespace().map(|s| s.parse().unwrap()).collect();
    let h = |i: usize| v[i] as HANDLE;
    let (in_write, out_read, signal, reference, conhost, job, shell) = (h(0), h(1), h(2), h(3), h(4), h(5), h(6));
    let (shell_pid, a_pid) = (v[7] as u32, v[8] as u32);
    log.line(&format!("serverB: got handles for shell pid {shell_pid} from serverA pid {a_pid}"));

    unsafe {
        let a = OpenProcess(PROCESS_SYNCHRONIZE, 0, a_pid);
        if !a.is_null() {
            let gone = WaitForSingleObject(a, 10_000) == WAIT_OBJECT_0;
            log.line(&format!("serverB: serverA exited: {gone}"));
            CloseHandle(a);
        } else {
            log.line("serverB: serverA already gone");
        }
    }
    std::thread::sleep(Duration::from_millis(500));
    let shell_alive = alive(shell);
    let conhost_alive = alive(conhost);
    log.line(&format!("serverB: after A's exit shell alive {shell_alive}, conhost alive {conhost_alive}"));
    verdict.push(("shell survives the old server", shell_alive));
    verdict.push(("console host survives the old server", conhost_alive));

    let sink = Arc::new(Mutex::new(Vec::new()));
    drain(out_read, sink.clone(), dir.join("b-output.raw"));
    let wrote = write_all(in_write, b"echo HANDOFF-OK %PROBE%\r\n");
    let answered = wrote && wait_for(&sink, "HANDOFF-OK alive-before-handoff", Duration::from_secs(15));
    log.line(&format!("serverB: shell kept its state and answered the new server: {answered}"));
    verdict.push(("input and output pass to the new server, shell state kept", answered));

    // resize through an HPCON rebuilt from the duplicated handles
    let rebuilt: Box<[HANDLE; 3]> = Box::new([signal, reference, conhost]);
    let hpc = Box::into_raw(rebuilt) as HPCON;
    let hr = unsafe { ResizePseudoConsole(hpc, COORD { X: 100, Y: 30 }) };
    write_all(in_write, b"powershell -NoLogo -NoProfile -Command \"'WIDTH=' + [Console]::WindowWidth\"\r\n");
    let resized = hr >= 0 && wait_for(&sink, "WIDTH=100", Duration::from_secs(30));
    log.line(&format!("serverB: ResizePseudoConsole hr={hr:#x}, child saw 100 columns: {resized}"));
    verdict.push(("resize through the rebuilt pseudoconsole reaches the child", resized));

    // kill: the job ends the shell tree, closing the signal pipe ends the console host
    unsafe {
        CloseHandle(job);
        let shell_ended = WaitForSingleObject(shell, 5_000) == WAIT_OBJECT_0;
        log.line(&format!("serverB: closing the job ended the shell: {shell_ended}"));
        verdict.push(("closing the duplicated job ends the shell", shell_ended));
        CloseHandle(signal);
        CloseHandle(reference);
        CloseHandle(in_write);
        let conhost_ended = WaitForSingleObject(conhost, 5_000) == WAIT_OBJECT_0;
        log.line(&format!("serverB: closing the console handles ended the console host: {conhost_ended}"));
        verdict.push(("closing the duplicated console handles ends the console host", conhost_ended));
    }

    let go = verdict.iter().all(|(_, ok)| *ok);
    let mut report = String::new();
    for (name, ok) in &verdict {
        report.push_str(&format!("{} {}\n", if *ok { "PASS" } else { "FAIL" }, name));
    }
    report.push_str(if go { "GO\n" } else { "NOGO\n" });
    fs::write(dir.join("result.txt"), report).unwrap();
    log.line(if go { "serverB: GO" } else { "serverB: NOGO" });
}

fn run(dir: &Path) -> i32 {
    let _ = fs::remove_dir_all(dir);
    fs::create_dir_all(dir).unwrap();
    let (p, t, _) = spawn(&format!("\"{}\" launcher \"{}\"", exe(), dir.display()), 0).expect("spawn launcher");
    unsafe {
        WaitForSingleObject(p, 30_000);
        CloseHandle(p);
        CloseHandle(t);
    }
    let result = dir.join("result.txt");
    let deadline = Instant::now() + Duration::from_secs(120);
    while !result.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
    }
    std::thread::sleep(Duration::from_millis(200));
    println!("--- probe.log\n{}", fs::read_to_string(dir.join("probe.log")).unwrap_or_default());
    let report = fs::read_to_string(&result).unwrap_or_else(|_| "NOGO (no result: timed out)\n".into());
    println!("--- result.txt\n{report}");
    if report.trim_end().ends_with("GO") && !report.contains("NOGO") { 0 } else { 1 }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = PathBuf::from(args.get(2).cloned().unwrap_or_else(|| "probe-out".into()));
    match args.get(1).map(String::as_str) {
        Some("run") => std::process::exit(run(&dir)),
        Some("launcher") => launcher(&dir),
        Some("serverA") => server_a(&dir),
        Some("serverB") => server_b(&dir),
        _ => {
            eprintln!("usage: probe run|launcher|serverA|serverB <dir>");
            std::process::exit(2);
        }
    }
}
```

## `mac-handoff/Cargo.toml`

macOS spawn and SCM_RIGHTS handoff probe: manifest.

```toml
[package]
name = "mac-handoff-probe"
version = "0.0.0"
edition = "2024"
publish = false

[[bin]]
name = "probe"
path = "src/main.rs"

[dependencies]
libc = "0.2.191"

[workspace]
```

## `mac-handoff/src/main.rs`

macOS spawn and SCM_RIGHTS handoff probe (`probe run <dir>`, `probe app <dir>`, `probe holders-setsid <dir>`).

```rust
//! Throwaway probe for Task 2 of docs/plans/20261018-zig-free-core.md (not merged), for macOS (it runs on Linux
//! too, without the responsibility SPI):
//!
//! `probe run <dir>`:
//! 1. `launcher` starts `server` with posix_spawn + POSIX_SPAWN_SETSID + responsibility_spawnattrs_setdisclaim and
//!    exits at once; the server must outlive it, lead its own session and be responsible for itself;
//! 2. `server` runs `/bin/sh` on a PTY, sets a variable in it, starts `import` the same way, passes the PTY master
//!    and the shell's pid over SCM_RIGHTS and exits;
//! 3. `import` checks the shell lives on, types into it and reads the variable back, resizes the PTY and sees the
//!    child read the new size, hangs the shell up, and writes `result.txt`.
//!
//! `probe app <dir>` (macOS): the herdr#4920 case. A LaunchServices app (`open -W`) starts two holders, one with a
//! plain posix_spawn and one with setsid + disclaim, then exits; `app-result.txt` says which holder outlived it.
#![allow(unsafe_code, clippy::all)]

use std::ffi::{CString, c_char, c_int, c_void};
use std::fs;
use std::io::{Read as _, Write as _};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

struct Log(PathBuf);
impl Log {
    fn line(&self, msg: &str) {
        let mut f = fs::OpenOptions::new().create(true).append(true).open(&self.0).unwrap();
        let _ = writeln!(f, "[{:>6}] {}", std::process::id(), msg);
    }
}

fn exe() -> String {
    std::env::current_exe().unwrap().to_string_lossy().into_owned()
}

// <spawn.h>: macOS defines POSIX_SPAWN_SETSID as 0x0400; the libc crate exports it for Linux only
#[cfg(target_os = "macos")]
const SPAWN_SETSID: c_int = 0x0400;
#[cfg(not(target_os = "macos"))]
const SPAWN_SETSID: c_int = libc::POSIX_SPAWN_SETSID as c_int;

type SetDisclaim = unsafe extern "C" fn(*mut libc::posix_spawnattr_t, c_int) -> c_int;
type Responsible = unsafe extern "C" fn(libc::pid_t) -> libc::pid_t;

fn sym<T>(name: &str) -> Option<T> {
    let c = CString::new(name).unwrap();
    let p = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c.as_ptr()) };
    if p.is_null() { None } else { Some(unsafe { std::mem::transmute_copy::<*mut c_void, T>(&p) }) }
}

fn responsible_for(pid: libc::pid_t) -> Option<libc::pid_t> {
    sym::<Responsible>("responsibility_get_pid_responsible_for_pid").map(|f| unsafe { f(pid) })
}

/// posix_spawn `argv` with stdio on /dev/null; `detach` adds POSIX_SPAWN_SETSID and, where the SPI exists, disclaim.
fn spawn(argv: &[String], detach: bool) -> Result<libc::pid_t, String> {
    unsafe {
        let mut attr: libc::posix_spawnattr_t = std::mem::zeroed();
        libc::posix_spawnattr_init(&mut attr);
        let mut disclaimed = false;
        if detach {
            libc::posix_spawnattr_setflags(&mut attr, SPAWN_SETSID as _);
            let disclaim_wanted = std::env::var_os("PROBE_NO_DISCLAIM").is_none();
            if let Some(f) = sym::<SetDisclaim>("responsibility_spawnattrs_setdisclaim").filter(|_| disclaim_wanted) {
                let rc = f(&mut attr, 1);
                if rc != 0 {
                    return Err(format!("setdisclaim rc {rc}"));
                }
                disclaimed = true;
            }
        }
        let mut actions: libc::posix_spawn_file_actions_t = std::mem::zeroed();
        libc::posix_spawn_file_actions_init(&mut actions);
        let devnull = CString::new("/dev/null").unwrap();
        for fd in 0..3 {
            libc::posix_spawn_file_actions_addopen(&mut actions, fd, devnull.as_ptr(), libc::O_RDWR, 0);
        }
        let c_args: Vec<CString> = argv.iter().map(|a| CString::new(a.as_str()).unwrap()).collect();
        let mut ptrs: Vec<*mut c_char> = c_args.iter().map(|a| a.as_ptr() as *mut c_char).collect();
        ptrs.push(std::ptr::null_mut());
        unsafe extern "C" {
            static environ: *const *mut c_char;
        }
        let mut pid = 0;
        let rc = libc::posix_spawn(&mut pid, c_args[0].as_ptr(), &actions, &attr, ptrs.as_ptr(), environ);
        libc::posix_spawn_file_actions_destroy(&mut actions);
        libc::posix_spawnattr_destroy(&mut attr);
        if rc != 0 {
            return Err(format!("posix_spawn rc {rc}"));
        }
        let _ = disclaimed;
        Ok(pid)
    }
}

fn alive(pid: libc::pid_t) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

fn read_until(fd: RawFd, needle: &str, timeout: Duration, seen: &mut Vec<u8>) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        if unsafe { libc::poll(&mut pfd, 1, 100) } > 0 {
            let mut buf = [0u8; 4096];
            let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                return false;
            }
            seen.extend_from_slice(&buf[..n as usize]);
        }
        if String::from_utf8_lossy(seen).contains(needle) {
            return true;
        }
    }
    false
}

fn write_fd(fd: RawFd, data: &[u8]) {
    unsafe { libc::write(fd, data.as_ptr().cast(), data.len()) };
}

fn launcher(dir: &Path) {
    let log = Log(dir.join("probe.log"));
    let pid = spawn(&[exe(), "server".into(), dir.display().to_string()], true).expect("spawn server");
    log.line(&format!("launcher: server pid {pid} spawned with setsid + disclaim; launcher exits now"));
}

fn server(dir: &Path) {
    let log = Log(dir.join("probe.log"));
    let me = unsafe { libc::getpid() };
    let sid = unsafe { libc::getsid(0) };
    log.line(&format!(
        "server: sid==pid {}, ppid {}, responsible pid {:?}",
        sid == me,
        unsafe { libc::getppid() },
        responsible_for(me)
    ));
    fs::write(dir.join("server.facts"), format!("{} {} {}\n", sid == me, me, responsible_for(me).unwrap_or(-1)))
        .unwrap();
    // let the launcher die first, so the checks below run with the server orphaned
    std::thread::sleep(Duration::from_millis(500));

    let (mut master, mut slave) = (0, 0);
    let ws = libc::winsize { ws_row: 25, ws_col: 80, ws_xpixel: 0, ws_ypixel: 0 };
    let rc = unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), &ws as *const _ as *mut _) };
    assert_eq!(rc, 0, "openpty");
    // close-on-exec: a stray inherited master copy (in the import server) that nobody reads blocks the shell's exit
    // on macOS, in the tty drain of its last output (state E)
    unsafe {
        libc::fcntl(master, libc::F_SETFD, libc::FD_CLOEXEC);
        libc::fcntl(slave, libc::F_SETFD, libc::FD_CLOEXEC);
    }
    let sh = CString::new("/bin/sh").unwrap();
    let child = unsafe { libc::fork() };
    if child == 0 {
        unsafe {
            libc::setsid();
            libc::ioctl(slave, libc::TIOCSCTTY as _, 0);
            libc::dup2(slave, 0);
            libc::dup2(slave, 1);
            libc::dup2(slave, 2);
            libc::close(master);
            libc::close(slave);
            let args = [sh.as_ptr(), std::ptr::null()];
            libc::execv(sh.as_ptr(), args.as_ptr());
            libc::_exit(127);
        }
    }
    unsafe { libc::close(slave) };
    let mut seen = Vec::new();
    write_fd(master, b"PROBE=alive-before-handoff; export PROBE; echo A-READY $PROBE\n");
    let ready = read_until(master, "A-READY alive-before-handoff", Duration::from_secs(10), &mut seen);
    log.line(&format!("server: shell pid {child} answered before handoff: {ready}"));

    // a short path: SUN_LEN caps socket paths near 104 bytes, and the probe dir is deep
    let sock = PathBuf::from(format!("/tmp/ag-spike-{me}.sock"));
    let _ = fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).unwrap();
    let import = spawn(&[exe(), "import".into(), dir.display().to_string()], true).expect("spawn import");
    log.line(&format!("server: import pid {import} spawned"));
    let (stream, _) = listener.accept().unwrap();
    send_fd(&stream, master, child).expect("sendmsg");
    let mut ack = [0u8; 3];
    let _ = (&stream).read_exact(&mut ack);
    log.line(&format!("server: import acked {:?}; exiting with the master closed", String::from_utf8_lossy(&ack)));
    let _ = fs::remove_file(&sock);
    unsafe { libc::close(master) };
    std::process::exit(0);
}

fn send_fd(stream: &UnixStream, fd: RawFd, pid: libc::pid_t) -> std::io::Result<()> {
    unsafe {
        let payload = pid.to_le_bytes();
        let mut iov = libc::iovec { iov_base: payload.as_ptr() as *mut c_void, iov_len: payload.len() };
        let space = libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) as usize;
        let mut control = vec![0u8; space];
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen = space as _;
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<RawFd>() as u32) as _;
        std::ptr::write_unaligned(libc::CMSG_DATA(cmsg) as *mut RawFd, fd);
        if libc::sendmsg(stream.as_raw_fd(), &msg, 0) < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

fn recv_fd(stream: &UnixStream) -> std::io::Result<(OwnedFd, libc::pid_t)> {
    unsafe {
        let mut payload = [0u8; 4];
        let mut iov = libc::iovec { iov_base: payload.as_mut_ptr().cast(), iov_len: payload.len() };
        let space = libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) as usize;
        let mut control = vec![0u8; space];
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen = space as _;
        if libc::recvmsg(stream.as_raw_fd(), &mut msg, 0) < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        if cmsg.is_null() || (*cmsg).cmsg_type != libc::SCM_RIGHTS {
            return Err(std::io::Error::other("no SCM_RIGHTS"));
        }
        let fd = std::ptr::read_unaligned(libc::CMSG_DATA(cmsg) as *const RawFd);
        Ok((OwnedFd::from_raw_fd(fd), libc::pid_t::from_le_bytes(payload)))
    }
}

fn import(dir: &Path) {
    let log = Log(dir.join("probe.log"));
    let mut verdict: Vec<(&str, bool)> = Vec::new();
    let me = unsafe { libc::getpid() };
    verdict.push(("import server leads its own session", unsafe { libc::getsid(0) } == me));
    if let Some(r) = responsible_for(me) {
        verdict.push(("import server is responsible for itself (disclaimed)", r == me));
    }
    let facts = fs::read_to_string(dir.join("server.facts")).unwrap();
    let old_pid = facts.split_whitespace().nth(1).unwrap().to_string();
    let stream = UnixStream::connect(format!("/tmp/ag-spike-{old_pid}.sock")).unwrap();
    let (master, shell) = recv_fd(&stream).expect("recvmsg");
    (&stream).write_all(b"ok\n").unwrap();
    let old = unsafe { libc::getppid() };
    log.line(&format!("import: got the master for shell pid {shell}; parent {old}"));

    // wait for the old server (the shell's parent) to be gone
    let facts = fs::read_to_string(dir.join("server.facts")).unwrap();
    let f: Vec<&str> = facts.split_whitespace().collect();
    let server_pid: libc::pid_t = f[1].parse().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while alive(server_pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(300));
    verdict.push(("server was detached from its launcher (setsid)", f[0] == "true"));
    if f[2] != "-1" {
        verdict.push(("server was responsible for itself (disclaimed)", f[2] == f[1]));
    }
    verdict.push(("old server exited", !alive(server_pid)));
    let shell_alive = alive(shell);
    log.line(&format!("import: old server gone, shell alive {shell_alive}"));
    verdict.push(("shell survives the old server", shell_alive));

    let fd = master.as_raw_fd();
    let mut seen = Vec::new();
    write_fd(fd, b"echo HANDOFF-OK $PROBE\n");
    let answered = read_until(fd, "HANDOFF-OK alive-before-handoff", Duration::from_secs(10), &mut seen);
    log.line(&format!("import: shell kept its state and answered: {answered}"));
    verdict.push(("PTY passes to the new server, shell state kept", answered));

    let ws = libc::winsize { ws_row: 30, ws_col: 100, ws_xpixel: 0, ws_ypixel: 0 };
    unsafe { libc::ioctl(fd, libc::TIOCSWINSZ as _, &ws) };
    write_fd(fd, b"echo SIZE=$(stty size)\n");
    let resized = read_until(fd, "SIZE=30 100", Duration::from_secs(10), &mut seen);
    log.line(&format!("import: child saw the resize: {resized}"));
    verdict.push(("resize from the new server reaches the child", resized));

    // the new server is not the shell's parent: it ends the session by signal and sees it go by kill(0)
    let ps = |label: &str| {
        let out = std::process::Command::new("/bin/ps")
            .args(["-o", "pid,ppid,pgid,sess,stat,command", "-p", &shell.to_string()])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        log.line(&format!("import: ps {label}:\n{out}"));
    };
    ps("before hangup");
    let rc = unsafe { libc::kill(-shell, libc::SIGHUP) };
    log.line(&format!("import: kill(-{shell}, SIGHUP) = {rc} ({})", std::io::Error::last_os_error()));
    // a zombie still answers kill(0) until its new parent (launchd, or a Linux subreaper) reaps it
    let deadline = Instant::now() + Duration::from_secs(5);
    while alive(shell) && Instant::now() < deadline {
        // keep draining the master, as a server's PTY reader does until EOF
        let _ = read_until(fd, "\u{0}never", Duration::from_millis(20), &mut seen);
    }
    ps("5 s after hangup");
    let mut ended = !alive(shell);
    if !ended {
        // drop the master too: the session then sees the hangup a PTY close gives
        drop(master);
        let deadline = Instant::now() + Duration::from_secs(5);
        while alive(shell) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        ps("5 s after closing the master");
        ended = !alive(shell);
        log.line(&format!("import: closing the master ended the shell: {ended}"));
        if !ended {
            unsafe { libc::kill(shell, libc::SIGKILL) };
            std::thread::sleep(Duration::from_secs(2));
            ps("after SIGKILL");
        }
        fs::write(dir.join("import.raw"), &seen).unwrap();
        verdict.push(("hangup by signal alone ends the shell", false));
    }
    log.line(&format!("import: SIGHUP to the shell's group ended it (reaped by its new parent): {ended}"));
    verdict.push(("hangup from the new server ends the shell", ended));
    if ended {
        fs::write(dir.join("import.raw"), &seen).unwrap();
    }

    let go = verdict.iter().all(|(_, ok)| *ok);
    let mut report = String::new();
    for (name, ok) in &verdict {
        report.push_str(&format!("{} {}\n", if *ok { "PASS" } else { "FAIL" }, name));
    }
    report.push_str(if go { "GO\n" } else { "NOGO\n" });
    fs::write(dir.join("result.txt"), report).unwrap();
}

fn run(dir: &Path) -> i32 {
    let _ = fs::remove_dir_all(dir);
    fs::create_dir_all(dir).unwrap();
    let mut launcher = std::process::Command::new(exe()).arg("launcher").arg(dir).spawn().unwrap();
    let _ = launcher.wait();
    let result = dir.join("result.txt");
    let deadline = Instant::now() + Duration::from_secs(60);
    while !result.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    std::thread::sleep(Duration::from_millis(100));
    println!("--- probe.log\n{}", fs::read_to_string(dir.join("probe.log")).unwrap_or_default());
    let report = fs::read_to_string(&result).unwrap_or_else(|_| "NOGO (no result: timed out)\n".into());
    println!("--- result.txt\n{report}");
    if report.contains("NOGO") { 1 } else { 0 }
}

/// A process that only stays alive and records its pid, for the app-quit case.
fn holder(dir: &Path, name: &str) {
    fs::write(dir.join(format!("{name}.pid")), unsafe { libc::getpid() }.to_string()).unwrap();
    std::thread::sleep(Duration::from_secs(60));
}

/// Runs as the app's executable: starts both holders, then exits as an app quitting would.
fn app_main(dir: &Path) {
    let log = Log(dir.join("app.log"));
    let plain = spawn(&[exe(), "holder".into(), dir.display().to_string(), "plain".into()], false);
    let detached = spawn(&[exe(), "holder".into(), dir.display().to_string(), "detached".into()], true);
    log.line(&format!("app: plain holder {plain:?}, setsid+disclaim holder {detached:?}; app exits in 2 s"));
    std::thread::sleep(Duration::from_secs(2));
}

fn app(dir: &Path) -> i32 {
    let _ = fs::remove_dir_all(dir);
    let contents = dir.join("ProbeApp.app/Contents");
    fs::create_dir_all(contents.join("MacOS")).unwrap();
    fs::write(
        contents.join("Info.plist"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>dev.agterm.spike.probeapp</string>
<key>CFBundleName</key><string>ProbeApp</string>
<key>CFBundleExecutable</key><string>ProbeApp</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>LSUIElement</key><true/>
</dict></plist>
"#,
    )
    .unwrap();
    // the app's executable is this probe itself, so it runs in the app's launchd job and coalition
    let app_exe = contents.join("MacOS/ProbeApp");
    fs::copy(exe(), &app_exe).unwrap();
    fs::write(dir.join("app.dir"), dir.display().to_string()).unwrap();
    let status = std::process::Command::new("/usr/bin/open")
        .arg("-W")
        .arg("-n")
        .arg(dir.join("ProbeApp.app"))
        .status();
    println!("open -W: {status:?}");
    std::thread::sleep(Duration::from_secs(5));
    let mut report = String::new();
    for name in ["plain", "detached"] {
        let pid: Option<libc::pid_t> =
            fs::read_to_string(dir.join(format!("{name}.pid"))).ok().and_then(|s| s.trim().parse().ok());
        let state = match pid {
            Some(pid) if alive(pid) => {
                unsafe { libc::kill(pid, libc::SIGTERM) };
                "ALIVE after the app exited"
            }
            Some(_) => "KILLED with the app",
            None => "never started",
        };
        report.push_str(&format!("{name}: {state}\n"));
    }
    report.push_str(&format!("--- app.log\n{}", fs::read_to_string(dir.join("app.log")).unwrap_or_default()));
    fs::write(dir.join("app-result.txt"), &report).unwrap();
    println!("{report}");
    0
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // inside the app bundle there are no arguments: find the probe dir beside the bundle
    let exe_path = std::env::current_exe().unwrap();
    if exe_path.to_string_lossy().contains("ProbeApp.app/Contents/MacOS") && args.len() < 2 {
        let dir = exe_path.ancestors().nth(4).unwrap().to_path_buf();
        app_main(&dir);
        return;
    }
    let dir = PathBuf::from(args.get(2).cloned().unwrap_or_else(|| "probe-out".into()));
    match args.get(1).map(String::as_str) {
        Some("run") => std::process::exit(run(&dir)),
        Some("launcher") => launcher(&dir),
        Some("server") => server(&dir),
        Some("import") => import(&dir),
        Some("app") => std::process::exit(app(&dir)),
        Some("holders") => app_main(&dir),
        // for a normal Terminal quit: only setsid holders, so the tab holds no process that makes Terminal ask
        Some("holders-setsid") => {
            let d = dir.display().to_string();
            let detached = spawn(&[exe(), "holder".into(), d.clone(), "detached".into()], true);
            // SAFETY: single-threaded here; the variable only steers the next spawn
            unsafe { std::env::set_var("PROBE_NO_DISCLAIM", "1") };
            let setsid = spawn(&[exe(), "holder".into(), d, "setsid".into()], true);
            unsafe { std::env::remove_var("PROBE_NO_DISCLAIM") };
            Log(dir.join("app.log")).line(&format!("holders: setsid+disclaim {detached:?}, setsid only {setsid:?}"));
        }
        Some("holder") => holder(&dir, args.get(3).map(String::as_str).unwrap_or("holder")),
        _ => {
            eprintln!("usage: probe run|app <dir>");
            std::process::exit(2);
        }
    }
}
```

## `mac-handoff/term-quit-normal.sh`

Terminal.app normal quit (run over ssh in the VM).

```bash
#!/bin/bash
# runs inside the macOS VM over ssh: herdr#4920 with a normal Terminal.app quit. The tab's script starts two setsid
# holders (one also disclaimed) and exits, so the tab holds no process and Terminal quits without asking; then
# Terminal is asked to quit through Apple Events (`osascript ... quit`), as Cmd-Q does.
G=/Users/admin/probe
D="$G/term2-out"
P="$G/mac-handoff/target/release/probe"
rm -rf "$D"; mkdir -p "$D"
cd "$G/mac-handoff" && PATH="$HOME/.cargo/bin:$PATH" cargo build --release -q || exit 3
pgrep -x Terminal >/dev/null && { echo "Terminal already running: quitting it first"; osascript -e 'quit app "Terminal"'; sleep 3; }
cat > "$D/holders.command" <<EOF
#!/bin/bash
"$P" holders-setsid "$D"
EOF
chmod +x "$D/holders.command"
open -a Terminal "$D/holders.command"
sleep 10
echo "holders: setsid+disclaim $(cat "$D/detached.pid" 2>/dev/null), setsid only $(cat "$D/setsid.pid" 2>/dev/null)"
cat "$D/app.log" 2>/dev/null
perl -e 'alarm 30; exec @ARGV' osascript -e 'quit app "Terminal"'
echo "osascript quit exit: $?"
sleep 5
if pgrep -x Terminal >/dev/null; then echo "Terminal STILL RUNNING after the quit request"; else echo "Terminal quit normally"; fi
for n in detached setsid; do
  p=$(cat "$D/$n.pid" 2>/dev/null)
  if [[ -n "$p" ]] && kill -0 "$p" 2>/dev/null; then echo "$n: ALIVE after Terminal quit"; kill "$p"; else echo "$n: KILLED with Terminal"; fi
done
```

## `mac-handoff/term-quit.sh`

Terminal.app termination by SIGTERM (run through vmrun in the VM).

```bash
#!/bin/bash
# runs inside the macOS VM: the herdr#4920 case with a real terminal app. Terminal.app runs a script that starts two
# holders (a plain posix_spawn child and a setsid + disclaim one), then Terminal quits; which holder outlives it?
G=/Users/admin/probe
D="$G/term-out"
P="$G/mac-handoff/target/release/probe"
rm -rf "$D"; mkdir -p "$D"
exec > "$G/term.log" 2>&1
cd "$G/mac-handoff" && PATH="$HOME/.cargo/bin:$PATH" cargo build --release -q || exit 3
cat > "$D/holders.command" <<EOF
#!/bin/bash
"$P" holders "$D"
sleep 600
EOF
chmod +x "$D/holders.command"
open -a Terminal "$D/holders.command"
sleep 10
echo "holders: plain $(cat "$D/plain.pid" 2>/dev/null) detached $(cat "$D/detached.pid" 2>/dev/null)"
cat "$D/app.log" 2>/dev/null
perl -e 'alarm 20; exec @ARGV' osascript -e 'tell application "Terminal" to quit saving no'
echo "osascript quit: $?"
sleep 3
if pgrep -x Terminal >/dev/null; then
  echo "Terminal still running after the quit request: sending SIGTERM"
  kill -TERM $(pgrep -x Terminal)
fi
sleep 5
for n in plain detached; do
  p=$(cat "$D/$n.pid" 2>/dev/null)
  if [[ -n "$p" ]] && kill -0 "$p" 2>/dev/null; then echo "$n: ALIVE after Terminal quit"; kill "$p"; else echo "$n: KILLED with Terminal"; fi
done
```

## `ci/spike-session-probes.yml`

The runner workflow (branch `spike/session-probes`, deleted after the run).

```yaml
# Throwaway: Task 2 of docs/plans/20261018-zig-free-core.md runs its session-server probes on the hosted runners.
# Lives only on the branch spike/session-probes, which is deleted once the results are recorded in
# docs/spikes/20261018-session-server.md.
name: spike-session-probes

on:
  push:
    branches: [spike/session-probes]

permissions:
  contents: read

jobs:
  windows:
    strategy:
      fail-fast: false
      matrix:
        os: [windows-latest, windows-2022]
    runs-on: ${{ matrix.os }}
    timeout-minutes: 15
    steps:
      - uses: actions/checkout@v7
      - name: Build and run the ConPTY handoff probe
        shell: pwsh
        run: |
          cmd /c ver
          rustc --version
          cd spike/win-handoff
          cargo build --release
          ./target/release/probe.exe run "$env:RUNNER_TEMP\probe-out"
          if ($LASTEXITCODE -ne 0) { exit 1 }

  macos:
    strategy:
      fail-fast: false
      matrix:
        os: [macos-26, macos-15]
    runs-on: ${{ matrix.os }}
    timeout-minutes: 15
    steps:
      - uses: actions/checkout@v7
      - name: Build the probe
        run: |
          sw_vers
          rustc --version
          cd spike/mac-handoff
          cargo build --release
      - name: Launcher exit and SCM_RIGHTS handoff
        run: spike/mac-handoff/target/release/probe run "$RUNNER_TEMP/probe-out"
      - name: App quit (herdr#4920), informational
        continue-on-error: true
        run: |
          perl -e 'alarm 120; exec @ARGV' spike/mac-handoff/target/release/probe app "$RUNNER_TEMP/app-out" || true
          cat "$RUNNER_TEMP/app-out/app-result.txt" || true
```

## `bench.sh`

agterm-term `bench.rs` driver.

```bash
#!/usr/bin/env bash
# Task 2 baselines: agterm-term's bench.rs (cat of 50 MiB plain and through zmx) and the idle RSS of N zmx daemons,
# with the live agterm's AGTERM_*/ZMX_* stripped and a private ZMX_DIR under the scratch dir.
set -uo pipefail
ROOT=/home/nick/develop/projects/my/agterm-rs/.claude/worktrees/zig-free-core
cd "$ROOT"
for v in $(compgen -e | grep -E '^(AGTERM|ZMX)_'); do unset "$v"; done
export CARGO_BUILD_JOBS=4
OUT="$ROOT/.scratch/session-spike/bench"
mkdir -p "$OUT"
# zmx sockets live under TERM_BENCH_DIR/zmx: keep that path short (SUN_LEN); only logs go to the scratch dir
BENCH_DIR=/tmp/ag-spike-bench
mkdir -p "$BENCH_DIR"
ZMX="$(scripts/build-zmx.sh --print-prefix)/bin/zmx"
case "${1:-bench}" in
  bench)
    TERM_BENCH_DIR="$BENCH_DIR" TERM_BENCH_ZMX="$ZMX" systemd-run --user --slice=agterm-agents.slice --scope --quiet -- \
      cargo test --release -p agterm-term --lib bench::read_path_plain_and_zmx -- --ignored --nocapture \
      --test-threads 1 2>&1 | tee "$OUT/bench.log"
    ;;
  ceiling)
    TERM_BENCH_DIR="$BENCH_DIR" TERM_BENCH_ZMX="$ZMX" systemd-run --user --slice=agterm-agents.slice --scope --quiet -- \
      cargo test --release -p agterm-term --lib bench::zmx_ceiling -- --ignored --nocapture \
      --test-threads 1 2>&1 | tee "$OUT/ceiling.log"
    ;;
esac
```

## `rss.sh`

Idle zmx daemon memory.

```bash
#!/usr/bin/env bash
# Task 2 baseline: idle memory of N zmx daemons (the pinned, patched zmx), each holding an idle /bin/sh that printed
# 20000 lines, in a private ZMX_DIR under the scratch dir; only that namespace is listed and reaped.
# usage: rss.sh N...   prints one JSON line per N
set -uo pipefail
ROOT=/home/nick/develop/projects/my/agterm-rs/.claude/worktrees/zig-free-core
cd "$ROOT"
for v in $(compgen -e | grep -E '^(AGTERM|ZMX)_'); do unset "$v"; done
ZMX="$(scripts/build-zmx.sh --print-prefix)/bin/zmx"
BASE="$ROOT/.scratch/session-spike/rss"
mkdir -p "$BASE"

daemon_pids() { # zmx processes whose environment carries this ZMX_DIR
  local needle="ZMX_DIR=$1" pid
  for pid in $(ls /proc | grep -E '^[0-9]+$'); do
    [[ "$(cat /proc/$pid/comm 2>/dev/null)" == zmx ]] || continue
    tr '\0' '\n' < /proc/$pid/environ 2>/dev/null | grep -qxF "$needle" && echo "$pid"
  done
}

for n in "$@"; do
  # a short path: the sockets inside must stay under SUN_LEN
  dir="/tmp/ag-spike-rss-$n-$$"
  mkdir -p "$dir"
  for i in $(seq 1 "$n"); do
    ZMX_DIR="$dir" SHELL=/bin/sh "$ZMX" run "s$i" seq 1 20000 >/dev/null 2>&1
  done
  sleep 3
  rss=0; pss=0; count=0
  for pid in $(daemon_pids "$dir"); do
    r=$(awk '/^VmRSS:/{print $2}' /proc/$pid/status)
    p=$(awk '/^Pss:/{print $2}' /proc/$pid/smaps_rollup)
    rss=$((rss + r)); pss=$((pss + p)); count=$((count + 1))
  done
  echo "{\"sessions\":$n,\"daemons\":$count,\"rss_kib\":$rss,\"pss_kib\":$pss,\"rss_per_daemon_kib\":$((count ? rss / count : 0)),\"pss_per_daemon_kib\":$((count ? pss / count : 0))}"
  names=$(seq -f 's%g' 1 "$n")
  # shellcheck disable=SC2086
  ZMX_DIR="$dir" "$ZMX" kill $names --force >/dev/null 2>&1
  sleep 1
  for pid in $(daemon_pids "$dir"); do kill -KILL "$pid" 2>/dev/null; done
  for pid in $(ls /proc | grep -E '^[0-9]+$'); do
    tr '\0' '\n' < /proc/$pid/environ 2>/dev/null | grep -qxF "ZMX_DIR=$dir" && kill -KILL "$pid" 2>/dev/null
  done
  rm -rf "$dir"
done
```

## `gpui-bench/protocol/test_spike_cat_bench.py`

GPUI 50 MiB `cat`, through the suite's Rust runner.

```python
"""Throwaway (Task 2 baseline, not merged): a 50 MiB `cat` in a GPUI pane, through a live zmx pane and a plain one.

Each run types a line that times `cat` with `date +%s%N` and prints the nanoseconds after a marker the command line
itself does not contain; the shell's `cat` ends only once the whole chain has consumed its output (zmx daemon, attach
client, pane PTY, agterm-term's IO thread), so the number is the chain's throughput. Results go to RESULTS as JSON
lines and to stdout."""

import json
import os
import time

import pytest
from agterm_conformance.match import assert_ok, find_session, session_ids

FILE = os.environ.get(
    "SPIKE_CAT_FILE",
    "/home/nick/develop/projects/my/agterm-rs/.claude/worktrees/zig-free-core/.scratch/session-spike/bench/text-50m.txt",
)
RESULTS = os.environ.get(
    "SPIKE_CAT_RESULTS",
    "/home/nick/develop/projects/my/agterm-rs/.claude/worktrees/zig-free-core/.scratch/session-spike/gpui-bench/results.jsonl",
)
RUNS = 3


def _measure(ctl, sid, label):
    for run in range(1, RUNS + 1):
        # through sh -c: the live pane runs the account's login shell (fish here), which has no `s=$(...)`
        line = (
            f"clear; sh -c 's=$(date +%s%N); cat {FILE}; e=$(date +%s%N); "
            f"printf \"M%sRK{run}_%s\\n\" A $((e - s))'\n"
        )
        started = time.monotonic()
        assert_ok(ctl.request("session.type", sid, text=line))
        match = ctl.wait_text(sid, rf"MARK{run}_(\d+)", timeout=300)
        wall = time.monotonic() - started
        record = {"scenario": label, "run": run, "cat_s": int(match.group(1)) / 1e9, "wall_s": round(wall, 3)}
        print(json.dumps(record))
        with open(RESULTS, "a", encoding="utf-8") as out:
            out.write(json.dumps(record) + "\n")


@pytest.fixture(scope="module")
def live(adapter):
    from agterm_conformance.plugin import Ctl

    instance = adapter.launch(settings={"restoreMode": "live"})
    try:
        yield Ctl(instance)
    finally:
        adapter.stop(instance)


def test_spike_cat_plain(ctl):
    (sid,) = session_ids(ctl.tree())
    ctl.wait_mounted(sid, "left")
    _measure(ctl, sid, "gpui-plain")


def test_spike_cat_live_zmx(live):
    assert assert_ok(live.request("restore.mode"))["restore"]["active"] == "live"
    sid = assert_ok(live.request("session.new"))["id"]
    live.wait_mounted(sid, "left")

    def backed(tree):
        node = find_session(tree, sid)
        assert node.get("backedByZmx") is True, node.get("backedByZmx")
        return True

    live.wait_tree(backed)
    _measure(live, sid, "gpui-live-zmx")
    assert_ok(live.request("session.close", sid))
```

## `gpui-bench/run.sh`

GPUI build, stage and measure driver.

```bash
#!/usr/bin/env bash
# Task 2 baseline: build and stage the GPUI release build, then run the throwaway cat-bench test through the
# suite's Rust runner (headless weston + private bus + lavapipe; private instances and zmx namespaces).
# usage: run.sh build | measure
set -uo pipefail
ROOT=/home/nick/develop/projects/my/agterm-rs/.claude/worktrees/zig-free-core
cd "$ROOT"
for v in $(compgen -e | grep -E '^(AGTERM|ZMX)_'); do unset "$v"; done
export CARGO_BUILD_JOBS=4
STAGE="$ROOT/target/conformance-stage"
HERE="$ROOT/.scratch/session-spike/gpui-bench"
case "$1" in
  build)
    # shellcheck source=/dev/null
    source scripts/lib/native.sh
    systemd-run --user --slice=agterm-agents.slice --scope --quiet -- \
      cargo build --release "${WORKSPACE_WITHOUT_WPE[@]}" || exit 1
    systemd-run --user --slice=agterm-agents.slice --scope --quiet -- cargo build --release -p agterm-gpui || exit 1
    rm -rf "$STAGE"
    scripts/stage.sh "$STAGE"
    ;;
  measure)
    rm -f "$HERE/results.jsonl"
    PYTHON=/home/nick/develop/projects/my/agterm-rs/.venv/bin/python SPIKE_CAT_FILE=/tmp/ag-spike-bench/text-50m.txt SPIKE_CAT_RESULTS="$HERE/results.jsonl" \
      systemd-run --user --slice=agterm-agents.slice --scope --quiet -- \
      conformance/tools/run-rust-linux.sh --stage "$STAGE" --tests "$HERE" --out "$HERE/report" -- -s -p no:cacheprovider
    echo "--- results"
    cat "$HERE/results.jsonl"
    ;;
esac
```
