//! The external texture element against GPUI's real renderer, patched (`vendor/gpui-pre-wgpu`): dmabufs imported into
//! its Vulkan device (acquired from the foreign queue family and copied on the GPU), CPU pixels by damaged rectangle,
//! clipping, the paint order, frame outcomes and import lifetime, rendered by GPUI's headless wgpu renderer and read
//! back; and the coverage masks the terminal's sprites paint with (`Window::paint_alpha_mask`, patch 0003). No
//! display and no compositor: Vulkan only.
//!
//! GPUI's headless renderer opens one device per process, so this harness (no libtest) runs each GPU in a child
//! process of its own, with an environment built from scratch (no display, no session bus, no `AGTERM_*`/`ZMX_*`):
//! `lavapipe` (Mesa's software Vulkan) and `mesa`, the first render node a Mesa Vulkan driver runs (Intel, AMD), where
//! there is one. GPUI is kept to the Vulkan adapter: on lavapipe Mesa's GL is forced to software (a CPU adapter like
//! lavapipe, which Vulkan then outranks), on hardware `ZED_DEVICE_ID` names the node's PCI device (GL needs the
//! hardware there, for the GL producer). The synthetic dmabufs come from `/dev/udmabuf` and, on hardware, from GBM
//! buffers a GL context clears.
//!
//! A missing piece (no lavapipe, no udmabuf access, a lavapipe without dmabuf import) skips its cases with a message,
//! and fails them instead under `AGTERM_REQUIRE_DISPLAY_TESTS`, as CI sets it; only the hardware-only case on lavapipe
//! is never required.
//!
//! Arguments: a substring selects cases by name; `--list` lists them.

mod buffers;
mod cases;
mod gl;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

const GPU_ENV: &str = "AGTERM_EXTERNAL_TEXTURE_GPU";
const NODE_ENV: &str = "AGTERM_EXTERNAL_TEXTURE_NODE";
const REQUIRE_ENV: &str = "AGTERM_REQUIRE_DISPLAY_TESTS";
/// How long one GPU's child may run.
const CHILD_TIMEOUT: Duration = Duration::from_secs(300);

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--list") {
        for (name, _) in cases::CASES {
            println!("{name}: test");
        }
        return ExitCode::SUCCESS;
    }
    let filter = args.iter().find(|arg| !arg.starts_with('-')).cloned();
    match std::env::var(GPU_ENV) {
        Ok(gpu) => cases::run(&gpu, std::env::var_os(NODE_ENV).map(PathBuf::from), filter.as_deref()),
        Err(_) => parent(&args),
    }
}

/// A Vulkan driver to run the cases on.
struct Gpu {
    name: &'static str,
    icd: PathBuf,
    node: Option<PathBuf>,
}

fn parent(args: &[String]) -> ExitCode {
    let Ok(exe) = std::env::current_exe() else {
        eprintln!("the test binary cannot find itself");
        return ExitCode::FAILURE;
    };
    let mut gpus = Vec::new();
    let mut failed = Vec::new();
    match lavapipe_icd() {
        Some(icd) => gpus.push(Gpu {
            name: "lavapipe",
            icd,
            node: None,
        }),
        None if std::env::var_os(REQUIRE_ENV).is_some() => {
            println!("gpu lavapipe: FAILED (no lavapipe ICD, and {REQUIRE_ENV} is set)");
            failed.push("lavapipe");
        }
        None => println!("gpu lavapipe: skipped (no lavapipe ICD; AGTERM_LAVAPIPE_ICD names one)"),
    }
    match mesa_gpu() {
        Some(gpu) => gpus.push(gpu),
        None => println!("gpu mesa: skipped (no render node a Mesa Vulkan driver runs)"),
    }
    for gpu in &gpus {
        println!("gpu {} ({})", gpu.name, gpu.icd.display());
        if let Err(message) = run_child(&exe, gpu, args) {
            println!("gpu {}: FAILED ({message})", gpu.name);
            failed.push(gpu.name);
        }
    }
    if failed.is_empty() {
        println!("test result: ok");
        ExitCode::SUCCESS
    } else {
        println!("test result: FAILED on {}", failed.join(", "));
        ExitCode::FAILURE
    }
}

/// Mesa's lavapipe ICD: `AGTERM_LAVAPIPE_ICD`, a distribution's, or the one unpacked into the vendor dir.
fn lavapipe_icd() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("AGTERM_LAVAPIPE_ICD") {
        return Some(PathBuf::from(path)).filter(|path| path.is_file());
    }
    let system = icd_named(|name| name.starts_with("lvp_icd"));
    if system.is_some() {
        return system;
    }
    let vendor = std::env::var_os("AGTERM_VENDOR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache/agterm-rs/vendor")))?;
    let mut found: Vec<PathBuf> = std::fs::read_dir(vendor)
        .ok()?
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("lavapipe-"))
        .map(|entry| entry.path().join("lvp_icd.json"))
        .filter(|path| path.is_file())
        .collect();
    found.sort();
    found.pop()
}

/// The system ICD manifest whose file name `wanted` accepts.
fn icd_named(wanted: impl Fn(&str) -> bool) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir("/usr/share/vulkan/icd.d")
        .ok()?
        .flatten()
        .filter(|entry| wanted(&entry.file_name().to_string_lossy()))
        .map(|entry| entry.path())
        .collect();
    found.sort();
    found.into_iter().next()
}

/// The first render node a Mesa Vulkan driver runs (Intel's ANV, AMD's RADV), with that driver's ICD.
fn mesa_gpu() -> Option<Gpu> {
    let mut nodes: Vec<PathBuf> = std::fs::read_dir("/dev/dri")
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("renderD"))
        })
        .collect();
    nodes.sort();
    nodes.into_iter().find_map(|node| {
        let icd = match kernel_driver(&node)?.as_str() {
            "i915" | "xe" => icd_named(|name| name.starts_with("intel_icd")),
            "amdgpu" => icd_named(|name| name.starts_with("radeon_icd")),
            _ => None,
        }?;
        std::fs::OpenOptions::new().read(true).write(true).open(&node).ok()?;
        Some(Gpu {
            name: "mesa",
            icd,
            node: Some(node),
        })
    })
}

/// The PCI device id of `node`'s GPU (`0xa7a0`), as `ZED_DEVICE_ID` takes it.
fn pci_device(node: &Path) -> Option<String> {
    let name = node.file_name()?;
    let id = std::fs::read_to_string(Path::new("/sys/class/drm").join(name).join("device/device")).ok()?;
    Some(id.trim().to_owned())
}

fn kernel_driver(node: &Path) -> Option<String> {
    let name = node.file_name()?;
    let driver = std::fs::read_link(Path::new("/sys/class/drm").join(name).join("device/driver")).ok()?;
    Some(driver.file_name()?.to_string_lossy().into_owned())
}

/// Mesa's EGL vendor file, so wgpu's GL backend never loads another vendor's EGL.
fn mesa_egl_vendor() -> Option<PathBuf> {
    ["/usr/share/glvnd/egl_vendor.d", "/etc/glvnd/egl_vendor.d"]
        .iter()
        .flat_map(|dir| std::fs::read_dir(dir).into_iter().flatten().flatten())
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().contains("mesa"))
        })
}

fn run_child(exe: &Path, gpu: &Gpu, args: &[String]) -> Result<(), String> {
    let mut command = Command::new(exe);
    command.args(args).env_clear().stdin(Stdio::null());
    for kept in ["PATH", "HOME", "RUST_BACKTRACE", "RUST_LOG", "TMPDIR", REQUIRE_ENV] {
        if let Some(value) = std::env::var_os(kept) {
            command.env(kept, value);
        }
    }
    command
        .env(GPU_ENV, gpu.name)
        .env("VK_DRIVER_FILES", &gpu.icd)
        .env("VK_ICD_FILENAMES", &gpu.icd);
    if let Some(vendor) = mesa_egl_vendor() {
        command.env("__EGL_VENDOR_LIBRARY_FILENAMES", vendor);
    }
    match &gpu.node {
        Some(node) => {
            command.env(NODE_ENV, node);
            if let Some(device) = pci_device(node) {
                command.env("ZED_DEVICE_ID", device);
            }
        }
        None => {
            command.env("LIBGL_ALWAYS_SOFTWARE", "1");
        }
    }
    let mut child = command
        .spawn()
        .map_err(|err| format!("the child did not start: {err}"))?;
    let deadline = Instant::now() + CHILD_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(status.to_string()),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("timed out after {CHILD_TIMEOUT:?}"));
            }
            Err(err) => return Err(format!("waiting for the child: {err}")),
        }
    }
}
