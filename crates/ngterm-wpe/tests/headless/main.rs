//! The engine against real WPE `WebKit`, headless: pages load from `data:` URLs, the consumer's scheme and a local
//! HTTP server; frames arrive as dmabufs on a render node of this machine (Mesa) or in shared memory; the script
//! bridge, the policy questions, input, stores and downloads go through the public API.
//!
//! `WebKit` runs once per process, so this harness (no libtest) runs each frame mode in a child process of its own:
//! `shm` everywhere, `dmabuf` on the first DRM render node a Mesa driver runs (`WPE_TEST_RENDER_NODE` picks another;
//! skipped with a message where there is none), and `bad-device`, which starts on a render node that does not exist.
//! Each mode runs on both backends:
//!
//! - `worker`: the child is a plain process, outside any overlay (checked), and the engine runs in a worker
//!   (`Host::start_worker`: under the overlay for a vendored WPE `WebKit`, plainly for the system's): every case then
//!   crosses the worker's transport. `worker-death` adds a worker killed with an evaluation waiting.
//! - `direct`: the engine on a thread of the child (`Host::start`); for a vendored WPE `WebKit` the child itself runs
//!   under the overlay of `ngterm_wpe::launcher::command`, an engine-only reference of the in-process engine.
//!
//! Every child gets private XDG directories and an environment without `NGTERM_*`, `AGTERM_*` and `ZMX_*`.
//!
//! Arguments: a substring selects cases by name; `--list` lists them.

mod cases;
mod dmabuf;
mod interaction;
mod support;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use ngterm_wpe::{
    Backend, BufferFormat, DmaBufTarget, FrameMode, Host, HostConfig, MODIFIER_LINEAR, StartError, fourcc,
};

use crate::cases::{CASES, Context};
use crate::support::{Harness, SCHEME};

const MODE: &str = "WPE_TEST_MODE";
const BACKEND: &str = "WPE_TEST_BACKEND";
const RENDER_NODE: &str = "WPE_TEST_RENDER_NODE";
/// How long one mode's child may run.
const CHILD_TIMEOUT: Duration = Duration::from_secs(300);

fn main() -> ExitCode {
    // SAFETY: first thing in main, while the process has one thread.
    unsafe { ngterm_wpe::worker::serve_if_asked(|| {}) };
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--list") {
        for (name, _) in CASES {
            println!("{name}: test");
        }
        return ExitCode::SUCCESS;
    }
    let filter = args.iter().find(|arg| !arg.starts_with('-')).cloned();
    match (std::env::var(MODE), std::env::var(BACKEND)) {
        (Ok(mode), Ok(backend)) => child(&mode, &backend, filter.as_deref()),
        _ => parent(&args),
    }
}

// MARK: - the parent: one child per mode and backend

fn parent(args: &[String]) -> ExitCode {
    let Ok(exe) = std::env::current_exe() else {
        eprintln!("the test binary cannot find itself");
        return ExitCode::FAILURE;
    };
    let mut modes = vec![("shm".to_owned(), None)];
    match render_node() {
        Some(node) => modes.push(("dmabuf".to_owned(), Some(node))),
        None => println!("mode dmabuf: skipped (no DRM render node this test can open)"),
    }
    modes.push(("bad-device".to_owned(), None));
    let mut runs: Vec<(String, &str, Option<PathBuf>)> = Vec::new();
    for backend in ["worker", "direct"] {
        for (mode, node) in &modes {
            runs.push((mode.clone(), backend, node.clone()));
        }
    }
    runs.push(("worker-death".to_owned(), "worker", None));
    let mut failed = Vec::new();
    for (mode, backend, node) in runs {
        let label = format!("{mode} ({backend})");
        println!(
            "mode {label}{}",
            node.as_ref()
                .map(|node| format!(" on {}", node.display()))
                .unwrap_or_default()
        );
        if let Err(message) = run_child(&exe, &mode, backend, node.as_deref(), args) {
            println!("mode {label}: FAILED ({message})");
            failed.push(label);
        }
    }
    if failed.is_empty() {
        println!("test result: ok");
        ExitCode::SUCCESS
    } else {
        println!("test result: FAILED in {}", failed.join(", "));
        ExitCode::FAILURE
    }
}

/// The render node the dmabuf mode renders on: the environment's choice, else the first this process may open.
fn render_node() -> Option<PathBuf> {
    if let Some(node) = std::env::var_os(RENDER_NODE) {
        return Some(PathBuf::from(node));
    }
    let mut nodes: Vec<PathBuf> = std::fs::read_dir("/dev/dri")
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("renderD"))
        })
        .collect();
    nodes.sort();
    nodes
        .into_iter()
        .find(|node| mesa_driven(node) && std::fs::OpenOptions::new().read(true).write(true).open(node).is_ok())
}

/// Whether a Mesa driver runs `node`: NVIDIA's own driver allocates no linear buffer to render into and falls back to
/// its block-linear modifier, which the test cannot read back (the consumer's real modifiers are Task 29's).
fn mesa_driven(node: &Path) -> bool {
    let Some(name) = node.file_name() else { return false };
    let driver = Path::new("/sys/class/drm").join(name).join("device/driver");
    std::fs::read_link(driver)
        .ok()
        .and_then(|driver| driver.file_name().map(|name| name != "nvidia"))
        .unwrap_or(false)
}

fn run_child(exe: &Path, mode: &str, backend: &str, node: Option<&Path>, args: &[String]) -> Result<(), String> {
    let home = tempfile::tempdir().map_err(|err| format!("no scratch directory: {err}"))?;
    // a worker's consumer is a plain process; only the in-process reference enters the overlay itself
    let mut command = if backend == "direct" {
        ngterm_wpe::launcher::command(exe)
    } else {
        Command::new(exe)
    };
    command
        .args(args)
        .env(MODE, mode)
        .env(BACKEND, backend)
        .stdin(Stdio::null());
    for (name, _) in std::env::vars_os() {
        let text = name.to_string_lossy();
        if text.starts_with("NGTERM_") || text.starts_with("AGTERM_") || text.starts_with("ZMX_") {
            command.env_remove(&name);
        }
    }
    for (variable, dir) in [
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_CONFIG_HOME", "config"),
    ] {
        command.env(variable, home.path().join(dir));
    }
    if let Some(node) = node {
        command.env(RENDER_NODE, node);
    }
    let mut child = command
        .spawn()
        .map_err(|err| format!("the child did not start: {err}"))?;
    let deadline = Instant::now() + CHILD_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(status.to_string()),
            Ok(None) if Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("still running after {CHILD_TIMEOUT:?}"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(err) => return Err(err.to_string()),
        }
    }
}

// MARK: - a child: one mode's cases on one backend

/// Runs one mode's cases and ends the process through [`ngterm_wpe::exit_process`], as a program that ran the engine
/// must.
fn child(mode: &str, backend: &str, filter: Option<&str>) -> ExitCode {
    let outcome = backend_of(backend).and_then(|backend| match mode {
        "shm" => cases_in(FrameMode::Shm, backend, filter),
        "dmabuf" => match std::env::var_os(RENDER_NODE) {
            Some(node) => cases_in(dmabuf_mode(PathBuf::from(node)), backend, filter),
            None => Err(format!("{RENDER_NODE} is not set")),
        },
        "bad-device" => bad_device(backend),
        "worker-death" => worker_death(),
        other => Err(format!("unknown mode {other}")),
    });
    let code = match outcome {
        Ok(()) => 0,
        Err(message) => {
            println!("{message}");
            1
        }
    };
    ngterm_wpe::exit_process(code)
}

/// The backend a child runs, once it checked it stands where that backend's consumer stands.
fn backend_of(name: &str) -> Result<Backend, String> {
    let reachable = ngterm_wpe::launcher::helpers_reachable();
    let vendored = matches!(
        ngterm_wpe::launcher::provider(),
        ngterm_wpe::launcher::Provider::Vendored { .. }
    );
    match name {
        "direct" if reachable => Ok(Backend::InProcess),
        "direct" => {
            Err("WPE WebKit's web process is not where the library looks for it (ngterm_wpe::launcher)".to_owned())
        }
        "worker" if vendored && reachable => Err("a worker's consumer must run outside the overlay".to_owned()),
        "worker" => Ok(Backend::Worker),
        other => Err(format!("unknown backend {other}")),
    }
}

/// XRGB and ARGB, linear only: what every importer takes, and a layout the test can read back.
fn dmabuf_mode(render_node: PathBuf) -> FrameMode {
    let linear = |name: &[u8; 4]| BufferFormat {
        fourcc: fourcc(name),
        modifiers: vec![MODIFIER_LINEAR],
    };
    FrameMode::DmaBuf(DmaBufTarget {
        render_node,
        formats: vec![linear(b"XR24"), linear(b"AR24")],
    })
}

fn cases_in(frames: FrameMode, backend: Backend, filter: Option<&str>) -> Result<(), String> {
    let harness = Harness::start(
        HostConfig {
            frames,
            schemes: vec![SCHEME.to_owned()],
        },
        backend,
    )?;
    println!("engine: {:?}", harness.host.backend());
    let context = Context {
        server: support::Server::start()?,
        scratch: tempfile::tempdir().map_err(|err| err.to_string())?,
    };
    let mut failed = 0;
    for (name, case) in CASES {
        if filter.is_some_and(|filter| !name.contains(filter)) {
            continue;
        }
        let started = Instant::now();
        match case(&harness, &context) {
            Ok(()) => println!("test {name} ... ok ({:.1?})", started.elapsed()),
            Err(message) => {
                failed += 1;
                println!("test {name} ... FAILED: {message}");
            }
        }
    }
    harness.shutdown();
    if failed == 0 {
        Ok(())
    } else {
        Err(format!("{failed} failed"))
    }
}

fn start_on(
    backend: Backend,
    config: HostConfig,
) -> Result<(Host, async_channel::Receiver<ngterm_wpe::Event>), StartError> {
    match backend {
        Backend::InProcess => Host::start(config),
        Backend::Worker => Host::start_worker(config),
    }
}

/// A render node that does not exist fails the start with the display's reason, and the engine cannot start again.
fn bad_device(backend: Backend) -> Result<(), String> {
    let missing = dmabuf_mode(PathBuf::from("/dev/dri/renderD250"));
    let failed = start_on(
        backend,
        HostConfig {
            frames: missing,
            schemes: Vec::new(),
        },
    );
    match failed {
        Err(StartError::Display(message)) if message.contains("not found") => {
            println!("test a_missing_render_node_fails_the_start ... ok");
        }
        other => {
            return Err(format!(
                "test a_missing_render_node_fails_the_start ... FAILED: {other:?}"
            ));
        }
    }
    match start_on(
        backend,
        HostConfig {
            frames: FrameMode::Shm,
            schemes: Vec::new(),
        },
    ) {
        Err(StartError::AlreadyStarted) => {
            println!("test a_failed_start_still_counts_as_the_one_start ... ok");
            Ok(())
        }
        other => Err(format!(
            "test a_failed_start_still_counts_as_the_one_start ... FAILED: {other:?}"
        )),
    }
}

/// A worker killed while an evaluation waits: the evaluation fails, the event channel closes, the worker is reaped.
fn worker_death() -> Result<(), String> {
    let (host, events) = Host::start_worker(HostConfig {
        frames: FrameMode::Shm,
        schemes: Vec::new(),
    })
    .map_err(|err| format!("the worker did not start: {err}"))?;
    let pid = host
        .worker_pid()
        .and_then(|pid| i32::try_from(pid).ok())
        .ok_or("no worker pid")?;
    // stopped first: the evaluation cannot be answered before the worker dies
    // SAFETY: `kill` only sends a signal, to the worker this test started.
    unsafe { libc::kill(pid, libc::SIGSTOP) };
    let waiting = host.evaluate(ngterm_wpe::ViewId::from_raw(1), "1".to_owned(), None);
    // SAFETY: as above.
    unsafe { libc::kill(pid, libc::SIGKILL) };
    let deadline = Instant::now() + Duration::from_secs(10);
    let answer = loop {
        match waiting.try_recv() {
            Ok(answer) => break answer,
            Err(_) if Instant::now() > deadline => return Err("the evaluation was never answered".to_owned()),
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    if !matches!(&answer, Err(message) if message.contains("ended")) {
        return Err(format!(
            "test a_dead_worker_fails_what_waits ... FAILED: answered {answer:?}"
        ));
    }
    loop {
        match events.try_recv() {
            Err(async_channel::TryRecvError::Closed) => break,
            _ if Instant::now() > deadline => return Err("the event channel stayed open".to_owned()),
            _ => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    host.shutdown();
    // SAFETY: signal 0 only checks that the process exists.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return Err("test a_dead_worker_fails_what_waits ... FAILED: the worker was not reaped".to_owned());
    }
    println!("test a_dead_worker_fails_what_waits ... ok");
    Ok(())
}
