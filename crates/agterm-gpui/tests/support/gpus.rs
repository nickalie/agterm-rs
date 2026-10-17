//! The Vulkan drivers the GPUI tests that render run on, each in a child process of its own: lavapipe (`lavapipe.rs`)
//! and the first render node a Mesa driver runs (Intel's ANV, AMD's RADV), with what a child needs to stay on it
//! (`ZED_DEVICE_ID`, Mesa's EGL vendor).

use std::path::{Path, PathBuf};

/// A Vulkan driver to run cases on.
pub struct Gpu {
    pub name: &'static str,
    pub icd: PathBuf,
    /// The render node of a hardware GPU.
    pub node: Option<PathBuf>,
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
pub fn mesa_gpu() -> Option<Gpu> {
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
pub fn pci_device(node: &Path) -> Option<String> {
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
pub fn mesa_egl_vendor() -> Option<PathBuf> {
    ["/usr/share/glvnd/egl_vendor.d", "/etc/glvnd/egl_vendor.d"]
        .iter()
        .flat_map(|dir| std::fs::read_dir(dir).into_iter().flatten().flatten())
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().contains("mesa"))
        })
}
