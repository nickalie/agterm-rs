//! The engine against real WPE `WebKit`, headless: pages load from `data:` URLs, the consumer's scheme and a local
//! HTTP server; frames arrive as dmabufs on a render node of this machine (Mesa) or in shared memory; the script
//! bridge, the policy questions, input, stores and downloads go through the public API.
//!
//! `WebKit` runs once per process, so this harness (no libtest) runs each frame mode in a child process of its own:
//! `shm` everywhere, `dmabuf` on the first DRM render node a Mesa driver runs (`WPE_TEST_RENDER_NODE` picks another;
//! skipped with a message where there is none), and `bad-device`, which starts on a render node that does not exist.
//! A vendored WPE `WebKit` runs its children under the overlay of `agterm_wpe::launcher`. Every child gets private XDG
//! directories and an environment without `AGTERM_*` and `ZMX_*`.
//!
//! Arguments: a substring selects cases by name; `--list` lists them.

mod cases;
mod dmabuf;
mod support;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use agterm_wpe::{BufferFormat, DmaBufTarget, FrameMode, Host, HostConfig, MODIFIER_LINEAR, StartError, fourcc};

use crate::cases::{CASES, Context};
use crate::support::{Harness, SCHEME};

const MODE: &str = "WPE_TEST_MODE";
const RENDER_NODE: &str = "WPE_TEST_RENDER_NODE";
/// How long one mode's child may run.
const CHILD_TIMEOUT: Duration = Duration::from_secs(300);

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--list") {
        for (name, _) in CASES {
            println!("{name}: test");
        }
        return ExitCode::SUCCESS;
    }
    let filter = args.iter().find(|arg| !arg.starts_with('-')).cloned();
    match std::env::var(MODE) {
        Ok(mode) => child(&mode, filter.as_deref()),
        Err(_) => parent(&args),
    }
}

// MARK: - the parent: one child per mode

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
    let mut failed = Vec::new();
    for (mode, node) in modes {
        println!(
            "mode {mode}{}",
            node.as_ref()
                .map(|node| format!(" on {}", node.display()))
                .unwrap_or_default()
        );
        if let Err(message) = run_child(&exe, &mode, node.as_deref(), args) {
            println!("mode {mode}: FAILED ({message})");
            failed.push(mode);
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

fn run_child(exe: &Path, mode: &str, node: Option<&Path>, args: &[String]) -> Result<(), String> {
    let home = tempfile::tempdir().map_err(|err| format!("no scratch directory: {err}"))?;
    let mut command: Command = agterm_wpe::launcher::command(exe);
    command.args(args).env(MODE, mode).stdin(Stdio::null());
    for (name, _) in std::env::vars_os() {
        let text = name.to_string_lossy();
        if text.starts_with("AGTERM_") || text.starts_with("ZMX_") {
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

// MARK: - a child: one mode's cases

/// Runs one mode's cases and ends the process through [`agterm_wpe::exit_process`], as a program that ran the engine
/// must.
fn child(mode: &str, filter: Option<&str>) -> ExitCode {
    let outcome = match mode {
        "shm" => cases_in(FrameMode::Shm, filter),
        "dmabuf" => match std::env::var_os(RENDER_NODE) {
            Some(node) => cases_in(dmabuf_mode(PathBuf::from(node)), filter),
            None => Err(format!("{RENDER_NODE} is not set")),
        },
        "bad-device" => bad_device(),
        other => Err(format!("unknown mode {other}")),
    };
    let code = match outcome {
        Ok(()) => 0,
        Err(message) => {
            println!("{message}");
            1
        }
    };
    agterm_wpe::exit_process(code)
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

fn cases_in(frames: FrameMode, filter: Option<&str>) -> Result<(), String> {
    if !agterm_wpe::launcher::helpers_reachable() {
        return Err("WPE WebKit's web process is not where the library looks for it (agterm_wpe::launcher)".to_owned());
    }
    let harness = Harness::start(HostConfig {
        frames,
        schemes: vec![SCHEME.to_owned()],
    })?;
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

/// A render node that does not exist fails the start with the display's reason, and the engine cannot start again.
fn bad_device() -> Result<(), String> {
    let missing = dmabuf_mode(PathBuf::from("/dev/dri/renderD250"));
    let failed = Host::start(HostConfig {
        frames: missing,
        schemes: Vec::new(),
    });
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
    match Host::start(HostConfig {
        frames: FrameMode::Shm,
        schemes: Vec::new(),
    }) {
        Err(StartError::AlreadyStarted) => {
            println!("test a_failed_start_still_counts_as_the_one_start ... ok");
            Ok(())
        }
        other => Err(format!(
            "test a_failed_start_still_counts_as_the_one_start ... FAILED: {other:?}"
        )),
    }
}
