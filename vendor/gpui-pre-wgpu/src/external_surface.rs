//! External surfaces on Linux (an agterm-rs patch): draws `gpui::ExternalSurface` frames — dmabufs imported
//! zero-copy through wgpu-hal Vulkan, and CPU pixels uploaded by damaged rectangle into a texture kept per surface —
//! and opens the Vulkan device so that it can import them.
//!
//! - Device: wgpu enables `VK_KHR_external_memory_fd` and `VK_EXT_external_memory_dma_buf` where supported but never
//!   `VK_EXT_image_drm_format_modifier`, without which a buffer with a modifier (every GPU-rendered one) cannot be
//!   described, so [`open_device`] opens the adapter through wgpu-hal's `open_with_callback` adding it.
//!   [`record_caps`] publishes what the device imports (`gpui::dmabuf_import_caps`): its DRM render node
//!   (`VK_EXT_physical_device_drm`) and the formats and modifiers it samples from an imported dmabuf.
//! - Import: one `VkImage` per buffer with `VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT` and every memory plane's layout
//!   (a non-disjoint image: several planes in one buffer object, as Intel's CCS modifiers have), bound to a dedicated
//!   allocation importing the dmabuf. A buffer is checked against the device's capabilities first: a format or
//!   modifier the device does not import, or a plane count the modifier does not have, fails the frame instead of
//!   reaching Vulkan. The import is cached per `gpui::DmaBuffer` and dropped when the producer drops the buffer.
//! - Completion: a frame reports `Presented` once the GPU finished the first submission that drew it. The renderer
//!   signals a timeline semaphore of its own with that submission (wgpu-hal's `add_signal_semaphore`), and a thread
//!   waits on it with `vkWaitSemaphores`, outside every wgpu lock (`Device::poll` holds the device's fence lock while it
//!   waits, which would stall the next `Queue::submit`).
//! - Acquire: the producer's rendering must be finished before a frame is handed over (the caller waits on the
//!   producer's fence); the renderer adds no wait of its own.

use anyhow::{Context as _, Result, anyhow, bail, ensure};
use ash::vk;
use collections::FxHashMap;
use gpui::external_surface_counters as counters;
use gpui::{
    DmaBufFormat, DmaBufImportCaps, DmaBufModifier, DmaBuffer, ExternalSurface, FrameOutcome,
    PaintSurface, PixelFrame, PixelSurface,
};
use std::ffi::CStr;
use std::os::fd::{AsRawFd as _, IntoRawFd as _, OwnedFd};
use std::os::unix::fs::MetadataExt as _;
use std::path::PathBuf;
use std::sync::{Arc, Weak, mpsc};
use std::time::{Duration, Instant};
use wgpu::hal::{self, vulkan};

/// `fourcc_code` of `drm_fourcc.h`.
const fn fourcc(code: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*code)
}

/// A DRM format the renderer draws: its Vulkan and wgpu formats, and whether its fourth channel is padding.
struct Format {
    fourcc: u32,
    vk: vk::Format,
    wgpu: wgpu::TextureFormat,
    opaque: bool,
}

/// The formats an external surface may have: 8-bit RGB with alpha or padding, in either byte order. DRM names
/// channels from the most significant bit of a little-endian word, so `AR24` (ARGB8888) is B, G, R, A in memory.
const FORMATS: [Format; 4] = [
    Format {
        fourcc: fourcc(b"AR24"),
        vk: vk::Format::B8G8R8A8_UNORM,
        wgpu: wgpu::TextureFormat::Bgra8Unorm,
        opaque: false,
    },
    Format {
        fourcc: fourcc(b"XR24"),
        vk: vk::Format::B8G8R8A8_UNORM,
        wgpu: wgpu::TextureFormat::Bgra8Unorm,
        opaque: true,
    },
    Format {
        fourcc: fourcc(b"AB24"),
        vk: vk::Format::R8G8B8A8_UNORM,
        wgpu: wgpu::TextureFormat::Rgba8Unorm,
        opaque: false,
    },
    Format {
        fourcc: fourcc(b"XB24"),
        vk: vk::Format::R8G8B8A8_UNORM,
        wgpu: wgpu::TextureFormat::Rgba8Unorm,
        opaque: true,
    },
];

fn format_of(fourcc: u32) -> Option<&'static Format> {
    FORMATS.iter().find(|format| format.fourcc == fourcc)
}

fn fourcc_name(fourcc: u32) -> String {
    fourcc
        .to_le_bytes()
        .iter()
        .map(|&byte| if byte.is_ascii_graphic() { char::from(byte) } else { '?' })
        .collect()
}

/// The device extensions dmabuf import needs beyond what wgpu enables by itself.
const EXTENSIONS: [&CStr; 3] = [
    ash::khr::external_memory_fd::NAME,
    ash::ext::external_memory_dma_buf::NAME,
    ash::ext::image_drm_format_modifier::NAME,
];

/// How long the completion thread waits for one submission before it gives the frame up.
const COMPLETION_TIMEOUT: Duration = Duration::from_secs(10);

// MARK: - device

/// Opens `adapter` as a Vulkan device that imports dmabufs; `None` when it is not a Vulkan adapter or cannot import
/// them (the caller then opens it as wgpu would, and [`record_caps`] says why).
pub(crate) fn open_device(
    adapter: &wgpu::Adapter,
    desc: &wgpu::DeviceDescriptor<'_>,
) -> Option<(wgpu::Device, wgpu::Queue)> {
    let open = {
        // SAFETY: the guard is only read here and dropped before the adapter is used again.
        let hal_adapter = unsafe { adapter.as_hal::<vulkan::Api>() }?;
        if let Err(reason) = missing_requirements(&hal_adapter) {
            log::info!("dmabuf import unavailable on {}: {reason}", adapter.get_info().name);
            return None;
        }
        let callback: Box<vulkan::CreateDeviceCallback<'_>> = Box::new(|args| {
            for extension in EXTENSIONS {
                if !args.extensions.contains(&extension) {
                    args.extensions.push(extension);
                }
            }
        });
        // SAFETY: the callback only adds extensions the physical device supports (`missing_requirements` checked
        // each), which wgpu-hal allows; features, limits and hints are the descriptor's, as `request_device` passes.
        let opened = unsafe {
            hal_adapter.open_with_callback(
                desc.required_features,
                &desc.required_limits,
                &desc.memory_hints,
                Some(callback),
            )
        };
        match opened {
            Ok(open) => open,
            Err(error) => {
                log::warn!("opening the Vulkan device with dmabuf import failed: {error}");
                return None;
            }
        }
    };
    // SAFETY: `open` was opened from this adapter's own physical device with the descriptor's features and limits.
    match unsafe { adapter.create_device_from_hal::<vulkan::Api>(open, desc) } {
        Ok(pair) => Some(pair),
        Err(error) => {
            log::warn!("wrapping the Vulkan device with dmabuf import failed: {error}");
            None
        }
    }
}

/// Why `adapter` cannot import dmabufs, if it cannot.
fn missing_requirements(adapter: &vulkan::Adapter) -> Result<(), String> {
    let capabilities = adapter.physical_device_capabilities();
    let missing: Vec<String> = EXTENSIONS
        .iter()
        .filter(|extension| !capabilities.supports_extension(extension))
        .map(|extension| extension.to_string_lossy().into_owned())
        .collect();
    if !missing.is_empty() {
        return Err(format!("missing {}", missing.join(", ")));
    }
    // The completion thread waits with core 1.2's vkWaitSemaphores on a timeline semaphore; wgpu enables the
    // timeline feature whenever the physical device has it.
    let api = capabilities
        .properties()
        .api_version
        .min(adapter.shared_instance().instance_api_version());
    if api < vk::API_VERSION_1_2 {
        return Err(format!(
            "Vulkan {}.{} (timeline semaphores need 1.2)",
            vk::api_version_major(api),
            vk::api_version_minor(api)
        ));
    }
    let mut features12 = vk::PhysicalDeviceVulkan12Features::default();
    let mut features = vk::PhysicalDeviceFeatures2::default().push_next(&mut features12);
    // SAFETY: the physical device is the adapter's and the instance supports Vulkan 1.2 (checked above), so
    // vkGetPhysicalDeviceFeatures2 exists and accepts the 1.2 feature struct.
    unsafe {
        adapter
            .shared_instance()
            .raw_instance()
            .get_physical_device_features2(adapter.raw_physical_device(), &mut features);
    }
    if features12.timeline_semaphore != vk::TRUE {
        return Err("no timeline semaphores".to_owned());
    }
    Ok(())
}

/// Publishes what `device` (opened from `adapter`) imports, as `gpui::dmabuf_import_caps`.
pub(crate) fn record_caps(adapter: &wgpu::Adapter, device: &wgpu::Device) {
    let name = adapter.get_info().name;
    let caps = match import_caps(adapter, device) {
        Ok(caps) => caps,
        Err(reason) => DmaBufImportCaps {
            adapter: name,
            render_node: None,
            formats: Vec::new(),
            unsupported: Some(format!("{reason:#}")),
        },
    };
    match &caps.unsupported {
        None => log::info!(
            "dmabuf import on {} ({}): {}",
            caps.adapter,
            caps.render_node
                .as_ref()
                .map_or_else(|| "no render node".to_owned(), |node| node.display().to_string()),
            caps.formats
                .iter()
                .map(|format| format!("{} x{}", fourcc_name(format.fourcc), format.modifiers.len()))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Some(reason) => log::info!("no dmabuf import on {}: {reason}", caps.adapter),
    }
    gpui::set_dmabuf_import_caps(caps);
}

fn import_caps(adapter: &wgpu::Adapter, device: &wgpu::Device) -> Result<DmaBufImportCaps> {
    let info = adapter.get_info();
    ensure!(
        info.backend == wgpu::Backend::Vulkan,
        "the GPU renders through {:?}, not Vulkan",
        info.backend
    );
    {
        // SAFETY: the guard is only read and dropped at the end of this block.
        let hal_device = unsafe { device.as_hal::<vulkan::Api>() }.context("not a Vulkan device")?;
        let enabled = hal_device.enabled_device_extensions();
        let missing: Vec<_> = EXTENSIONS.iter().filter(|extension| !enabled.contains(extension)).collect();
        ensure!(
            missing.is_empty(),
            "the device was opened without {}",
            missing
                .iter()
                .map(|extension| extension.to_string_lossy())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    // SAFETY: the guard is only read and dropped at the end of this function.
    let hal_adapter = unsafe { adapter.as_hal::<vulkan::Api>() }.context("not a Vulkan adapter")?;
    let formats: Vec<DmaBufFormat> = FORMATS
        .iter()
        .filter_map(|format| {
            let modifiers = importable_modifiers(&hal_adapter, format.vk);
            (!modifiers.is_empty()).then(|| DmaBufFormat {
                fourcc: format.fourcc,
                modifiers,
            })
        })
        .collect();
    let unsupported = formats
        .is_empty()
        .then(|| "the device imports none of AR24, XR24, AB24, XB24 as a sampled image".to_owned());
    Ok(DmaBufImportCaps {
        adapter: info.name,
        render_node: render_node(&hal_adapter),
        formats,
        unsupported,
    })
}

/// The modifiers of `format` the adapter samples from an imported dmabuf, in the driver's order.
fn importable_modifiers(adapter: &vulkan::Adapter, format: vk::Format) -> Vec<DmaBufModifier> {
    let instance = adapter.shared_instance().raw_instance();
    let physical = adapter.raw_physical_device();
    let mut list = vk::DrmFormatModifierPropertiesListEXT::default();
    // SAFETY: Vulkan 1.2 and VK_EXT_image_drm_format_modifier are present (`missing_requirements`); the first call
    // only fills the count, the list pointing nowhere.
    unsafe {
        instance.get_physical_device_format_properties2(
            physical,
            format,
            &mut vk::FormatProperties2::default().push_next(&mut list),
        );
    }
    let mut properties = vec![vk::DrmFormatModifierPropertiesEXT::default(); list.drm_format_modifier_count as usize];
    let mut list = vk::DrmFormatModifierPropertiesListEXT::default().drm_format_modifier_properties(&mut properties);
    // SAFETY: as above; the list now points at `properties`, which holds the count the first call returned.
    unsafe {
        instance.get_physical_device_format_properties2(
            physical,
            format,
            &mut vk::FormatProperties2::default().push_next(&mut list),
        );
    }
    let count = list.drm_format_modifier_count as usize;
    properties.truncate(count);
    properties
        .iter()
        .filter(|properties| {
            properties
                .drm_format_modifier_tiling_features
                .contains(vk::FormatFeatureFlags::SAMPLED_IMAGE | vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_LINEAR)
                && imports(adapter, format, properties.drm_format_modifier)
        })
        .map(|properties| DmaBufModifier {
            modifier: properties.drm_format_modifier,
            plane_count: properties.drm_format_modifier_plane_count,
        })
        .collect()
}

/// Whether a dmabuf of `format` with `modifier` imports as a sampled 2D image.
fn imports(adapter: &vulkan::Adapter, format: vk::Format, modifier: u64) -> bool {
    let mut external_info =
        vk::PhysicalDeviceExternalImageFormatInfo::default().handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let mut modifier_info = vk::PhysicalDeviceImageDrmFormatModifierInfoEXT::default()
        .drm_format_modifier(modifier)
        .sharing_mode(vk::SharingMode::EXCLUSIVE);
    let info = vk::PhysicalDeviceImageFormatInfo2::default()
        .format(format)
        .ty(vk::ImageType::TYPE_2D)
        .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
        .usage(vk::ImageUsageFlags::SAMPLED)
        .push_next(&mut external_info)
        .push_next(&mut modifier_info);
    let mut external = vk::ExternalImageFormatProperties::default();
    let mut properties = vk::ImageFormatProperties2::default().push_next(&mut external);
    // SAFETY: Vulkan 1.2 and the external memory and modifier extensions are present (`missing_requirements`); every
    // struct of both chains lives until the call returns.
    let result = unsafe {
        adapter
            .shared_instance()
            .raw_instance()
            .get_physical_device_image_format_properties2(adapter.raw_physical_device(), &info, &mut properties)
    };
    result.is_ok()
        && external
            .external_memory_properties
            .external_memory_features
            .contains(vk::ExternalMemoryFeatureFlags::IMPORTABLE)
}

/// The adapter's DRM render node, from `VK_EXT_physical_device_drm`.
fn render_node(adapter: &vulkan::Adapter) -> Option<PathBuf> {
    if !adapter
        .physical_device_capabilities()
        .supports_extension(ash::ext::physical_device_drm::NAME)
    {
        return None;
    }
    let mut drm = vk::PhysicalDeviceDrmPropertiesEXT::default();
    let mut properties = vk::PhysicalDeviceProperties2::default().push_next(&mut drm);
    // SAFETY: the physical device supports VK_EXT_physical_device_drm (checked above), so the instance (Vulkan 1.2)
    // fills its properties struct.
    unsafe {
        adapter
            .shared_instance()
            .raw_instance()
            .get_physical_device_properties2(adapter.raw_physical_device(), &mut properties);
    }
    if drm.has_render != vk::TRUE {
        return None;
    }
    find_render_node(drm.render_major, drm.render_minor)
}

/// The `/dev/dri` node with device number `major:minor`.
fn find_render_node(major: i64, minor: i64) -> Option<PathBuf> {
    let wanted = libc_makedev(u64::try_from(major).ok()?, u64::try_from(minor).ok()?);
    std::fs::read_dir("/dev/dri")
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| std::fs::metadata(path).is_ok_and(|metadata| metadata.rdev() == wanted))
}

/// glibc's `makedev`.
fn libc_makedev(major: u64, minor: u64) -> u64 {
    ((major & 0xffff_f000) << 32) | ((major & 0x0000_0fff) << 8) | ((minor & 0xffff_ff00) << 12) | (minor & 0x0000_00ff)
}

// MARK: - the renderer's side

/// A dmabuf imported for a producer's buffer, kept while the buffer lives.
struct Import {
    buffer: Weak<DmaBuffer>,
    view: wgpu::TextureView,
    _texture: wgpu::Texture,
    opaque: bool,
}

/// The texture of a CPU-pixel surface.
struct PixelTexture {
    surface: Weak<PixelSurface>,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
    /// The serial of the frame uploaded last.
    serial: u64,
}

type Callback = Box<dyn FnOnce(FrameOutcome) + Send>;

/// The renderer's external surfaces: imports, pixel textures, this frame's draws and the frames waiting to complete.
#[derive(Default)]
pub(crate) struct ExternalSurfaces {
    /// By `Arc::as_ptr` of the buffer: the weak reference keeps the allocation, so the address is not reused while
    /// the entry exists.
    imports: FxHashMap<usize, Import>,
    pixels: FxHashMap<usize, PixelTexture>,
    /// One bind group per `scene.surfaces` entry, in scene order; `None` when that frame is not drawn.
    draws: Vec<Option<wgpu::BindGroup>>,
    /// The completions of frames this frame draws for the first time.
    completing: Vec<Callback>,
    timeline: Option<Timeline>,
    timeline_failed: bool,
}

/// Uniforms of one external surface draw (`ExternalSurfaceParams` in shaders.wgsl).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct ExternalSurfaceParams {
    pub(crate) bounds: [f32; 4],
    pub(crate) content_mask: [f32; 4],
    pub(crate) opaque: u32,
    pub(crate) pad: [u32; 3],
}

/// What [`ExternalSurfaces::prepare`] needs from the renderer.
pub(crate) struct Resources<'a> {
    pub(crate) device: &'a wgpu::Device,
    pub(crate) queue: &'a wgpu::Queue,
    pub(crate) layout: &'a wgpu::BindGroupLayout,
    pub(crate) sampler: &'a wgpu::Sampler,
}

impl ExternalSurfaces {
    /// The bind group drawing `scene.surfaces[index]`, if that frame is drawn.
    pub(crate) fn draw(&self, index: usize) -> Option<&wgpu::BindGroup> {
        self.draws.get(index).and_then(Option::as_ref)
    }

    /// Imports, uploads and binds this frame's surfaces, and forgets what their producers dropped.
    pub(crate) fn prepare(&mut self, resources: &Resources<'_>, surfaces: &[PaintSurface]) {
        self.forget_dropped();
        self.draws.clear();
        for surface in surfaces {
            let drawn = match &surface.source {
                ExternalSurface::DmaBuf(frame) => self.dmabuf_view(resources.device, &frame.buffer),
                ExternalSurface::Pixels(frame) => self.pixel_view(resources, frame).map(|view| (view, false)),
            };
            match drawn {
                Ok((view, opaque)) => {
                    let params = ExternalSurfaceParams {
                        bounds: bounds(surface.bounds),
                        content_mask: bounds(surface.content_mask.bounds),
                        opaque: u32::from(opaque),
                        pad: [0; 3],
                    };
                    self.draws.push(Some(bind_group(resources, &view, &params)));
                    if let Some(callback) = surface.source.completion().take() {
                        self.completing.push(callback);
                    }
                }
                Err(error) => {
                    self.draws.push(None);
                    surface.source.completion().complete(FrameOutcome::Failed(format!("{error:#}")));
                }
            }
        }
    }

    /// Before the frame's submission: has it signal the completion timeline when frames complete with it.
    pub(crate) fn before_submit(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> Option<u64> {
        if self.completing.is_empty() {
            return None;
        }
        if self.timeline.is_none() && !self.timeline_failed {
            match Timeline::new(device) {
                Ok(timeline) => self.timeline = Some(timeline),
                Err(error) => {
                    log::error!("external surfaces: no completion timeline ({error:#}); frames report presented at submit");
                    self.timeline_failed = true;
                }
            }
        }
        let timeline = self.timeline.as_mut()?;
        timeline.next += 1;
        let value = timeline.next;
        // SAFETY: the guard is only used to queue the signal and is dropped before `submit`.
        let hal_queue = unsafe { queue.as_hal::<vulkan::Api>() }?;
        hal_queue.add_signal_semaphore(timeline.semaphore, Some(value));
        Some(value)
    }

    /// After the frame's submission (`signal` from [`Self::before_submit`]): the frames it drew complete when the GPU
    /// finished it.
    pub(crate) fn after_submit(&mut self, signal: Option<u64>) {
        let callbacks = std::mem::take(&mut self.completing);
        if callbacks.is_empty() {
            return;
        }
        match (signal, &self.timeline) {
            (Some(value), Some(timeline)) => timeline.complete_at(value, callbacks),
            _ => {
                for callback in callbacks {
                    callback(FrameOutcome::Presented);
                }
            }
        }
    }

    fn forget_dropped(&mut self) {
        let before = self.imports.len();
        self.imports.retain(|_, import| import.buffer.strong_count() > 0);
        counters::add(&counters::IMPORTS_RELEASED, (before - self.imports.len()) as u64);
        self.pixels.retain(|_, texture| texture.surface.strong_count() > 0);
    }

    fn dmabuf_view(&mut self, device: &wgpu::Device, buffer: &Arc<DmaBuffer>) -> Result<(wgpu::TextureView, bool)> {
        let key = Arc::as_ptr(buffer) as usize;
        if let Some(import) = self.imports.get(&key) {
            return Ok((import.view.clone(), import.opaque));
        }
        let caps = gpui::dmabuf_import_caps().ok_or_else(|| anyhow!("the renderer has no device yet"))?;
        let imported = check_buffer(&caps, buffer).and_then(|format| {
            let texture = import_dmabuf(device, buffer, format)?;
            Ok((texture, format.opaque))
        });
        let (texture, opaque) = match imported {
            Ok(imported) => imported,
            Err(error) => {
                counters::add(&counters::IMPORT_FAILURES, 1);
                return Err(error.context(format!(
                    "dmabuf {}x{} {} modifier {:#018x} ({} planes) does not import",
                    buffer.width,
                    buffer.height,
                    fourcc_name(buffer.fourcc),
                    buffer.modifier,
                    buffer.planes.len()
                )));
            }
        };
        counters::add(&counters::IMPORTS, 1);
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.imports.insert(
            key,
            Import {
                buffer: Arc::downgrade(buffer),
                view: view.clone(),
                _texture: texture,
                opaque,
            },
        );
        Ok((view, opaque))
    }

    fn pixel_view(&mut self, resources: &Resources<'_>, frame: &PixelFrame) -> Result<wgpu::TextureView> {
        let expected = u64::from(frame.width) * u64::from(frame.height) * 4;
        ensure!(
            frame.width > 0 && frame.height > 0 && frame.pixels.len() as u64 == expected,
            "pixel frame {}x{} with {} bytes",
            frame.width,
            frame.height,
            frame.pixels.len()
        );
        let key = Arc::as_ptr(&frame.surface) as usize;
        let reusable = self
            .pixels
            .get(&key)
            .is_some_and(|texture| texture.width == frame.width && texture.height == frame.height);
        if !reusable {
            let texture = resources.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("external_pixels"),
                size: wgpu::Extent3d {
                    width: frame.width,
                    height: frame.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Bgra8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            self.pixels.insert(
                key,
                PixelTexture {
                    surface: Arc::downgrade(&frame.surface),
                    texture,
                    view,
                    width: frame.width,
                    height: frame.height,
                    serial: 0,
                },
            );
        }
        let texture = self
            .pixels
            .get_mut(&key)
            .ok_or_else(|| anyhow!("pixel texture missing"))?;
        if !reusable || texture.serial != frame.serial {
            let incremental = reusable && texture.serial.checked_add(1) == Some(frame.serial);
            upload(resources.queue, texture, frame, incremental);
            texture.serial = frame.serial;
        }
        Ok(texture.view.clone())
    }
}

impl Drop for ExternalSurfaces {
    fn drop(&mut self) {
        counters::add(&counters::IMPORTS_RELEASED, self.imports.len() as u64);
        // Frames bound for a submission that never happened: their completions report `Discarded` as they drop.
        self.completing.clear();
    }
}

fn bounds(bounds: gpui::Bounds<gpui::ScaledPixels>) -> [f32; 4] {
    [
        bounds.origin.x.0,
        bounds.origin.y.0,
        bounds.size.width.0,
        bounds.size.height.0,
    ]
}

fn bind_group(resources: &Resources<'_>, view: &wgpu::TextureView, params: &ExternalSurfaceParams) -> wgpu::BindGroup {
    use wgpu::util::DeviceExt as _;
    let uniform = resources.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("external_surface_params"),
        contents: bytemuck::bytes_of(params),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    resources.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("external_surface"),
        layout: resources.layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 4,
                resource: uniform.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: wgpu::BindingResource::TextureView(view),
            },
            wgpu::BindGroupEntry {
                binding: 6,
                resource: wgpu::BindingResource::Sampler(resources.sampler),
            },
        ],
    })
}

/// The bind group layout of external surface draws (group 1 of `vs_external_surface`/`fs_external_surface`).
pub(crate) fn bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("external_surfaces_layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: std::num::NonZeroU64::new(std::mem::size_of::<ExternalSurfaceParams>() as u64),
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 5,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 6,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
}

/// Uploads `frame` into `texture`: its damaged rectangles when the texture holds the frame before it, else all of it.
fn upload(queue: &wgpu::Queue, texture: &PixelTexture, frame: &PixelFrame, incremental: bool) {
    let whole = [gpui::PixelRect {
        x: 0,
        y: 0,
        width: frame.width,
        height: frame.height,
    }];
    let rects: &[gpui::PixelRect] = if incremental { &frame.damage } else { &whole };
    let stride = frame.width * 4;
    for rect in rects {
        let x = rect.x.min(frame.width);
        let y = rect.y.min(frame.height);
        let width = rect.width.min(frame.width - x);
        let height = rect.height.min(frame.height - y);
        if width == 0 || height == 0 {
            continue;
        }
        let start = y as usize * stride as usize + x as usize * 4;
        let end = (y + height - 1) as usize * stride as usize + (x + width) as usize * 4;
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            &frame.pixels[start..end],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        counters::add(&counters::PIXEL_BYTES_UPLOADED, u64::from(width) * u64::from(height) * 4);
    }
}

// MARK: - import

/// The format `buffer` is drawn as, if the device imports it as it is: its format and modifier among the device's,
/// with the modifier's plane count, every plane in one buffer object, a size the device takes.
fn check_buffer(caps: &DmaBufImportCaps, buffer: &DmaBuffer) -> Result<&'static Format> {
    if let Some(reason) = &caps.unsupported {
        bail!("the renderer's device imports no dmabufs: {reason}");
    }
    let format = format_of(buffer.fourcc).ok_or_else(|| anyhow!("unsupported format {}", fourcc_name(buffer.fourcc)))?;
    let modifiers = caps
        .formats
        .iter()
        .find(|candidate| candidate.fourcc == buffer.fourcc)
        .map(|candidate| candidate.modifiers.as_slice())
        .unwrap_or_default();
    let modifier = modifiers
        .iter()
        .find(|candidate| candidate.modifier == buffer.modifier)
        .ok_or_else(|| anyhow!("the device does not import {} with this modifier", fourcc_name(buffer.fourcc)))?;
    ensure!(
        buffer.planes.len() == modifier.plane_count as usize,
        "{} planes, the modifier has {}",
        buffer.planes.len(),
        modifier.plane_count
    );
    ensure!(buffer.width > 0 && buffer.height > 0, "empty buffer");
    ensure!(
        planes_share_one_object(buffer)?,
        "planes in separate buffer objects (a disjoint image is not supported)"
    );
    Ok(format)
}

/// Whether every plane's fd is the same dmabuf (one inode per exported buffer object).
fn planes_share_one_object(buffer: &DmaBuffer) -> Result<bool> {
    let identity = |fd: &OwnedFd| -> Result<(u64, u64)> {
        let file = std::fs::File::from(fd.try_clone().context("dup a plane fd")?);
        let metadata = file.metadata().context("fstat a plane fd")?;
        Ok((metadata.dev(), metadata.ino()))
    };
    let mut planes = buffer.planes.iter();
    let Some(first) = planes.next() else {
        return Ok(false);
    };
    let first = identity(&first.fd)?;
    for plane in planes {
        if identity(&plane.fd)? != first {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Imports `buffer` (checked by [`check_buffer`]) as a sampled texture of `device`.
fn import_dmabuf(device: &wgpu::Device, buffer: &DmaBuffer, format: &Format) -> Result<wgpu::Texture> {
    let size = wgpu::Extent3d {
        width: buffer.width,
        height: buffer.height,
        depth_or_array_layers: 1,
    };
    let hal_texture = {
        // SAFETY: the guard is dropped at the end of this block, before the device is used through wgpu again.
        let hal_device = unsafe { device.as_hal::<vulkan::Api>() }.context("the device is not Vulkan")?;
        let (image, memory) = create_image(&hal_device, buffer, format)?;
        let desc = hal::TextureDescriptor {
            label: Some("dmabuf"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: format.wgpu,
            usage: wgpu::TextureUses::RESOURCE,
            memory_flags: hal::MemoryFlags::empty(),
            view_formats: vec![],
        };
        // SAFETY: `image` was created on this device as `desc` describes (2D, one mip, one sample, the format's
        // `vk::Format`, sampled usage) and is bound to `memory`, which the texture now owns: wgpu-hal destroys the
        // image and frees the memory when wgpu drops the texture, after the GPU is done with it.
        unsafe { hal_device.texture_from_raw(image, &desc, None, vulkan::TextureMemory::Dedicated(memory)) }
    };
    // SAFETY: `hal_texture` belongs to this device and matches the descriptor; its memory is initialized (the
    // producer rendered it), so wgpu must not clear it, which a texture from hal never is.
    Ok(unsafe {
        device.create_texture_from_hal::<vulkan::Api>(
            hal_texture,
            &wgpu::TextureDescriptor {
                label: Some("dmabuf"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: format.wgpu,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
        )
    })
}

/// A `VkImage` over `buffer`'s memory: the modifier's tiling with every plane's layout, bound to a dedicated
/// allocation importing the buffer's dmabuf.
fn create_image(device: &vulkan::Device, buffer: &DmaBuffer, format: &Format) -> Result<(vk::Image, vk::DeviceMemory)> {
    let raw = device.raw_device();
    let plane_layouts: Vec<vk::SubresourceLayout> = buffer
        .planes
        .iter()
        .map(|plane| vk::SubresourceLayout {
            offset: u64::from(plane.offset),
            size: 0,
            row_pitch: u64::from(plane.stride),
            array_pitch: 0,
            depth_pitch: 0,
        })
        .collect();
    let mut modifier_info = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default()
        .drm_format_modifier(buffer.modifier)
        .plane_layouts(&plane_layouts);
    let mut external_info =
        vk::ExternalMemoryImageCreateInfo::default().handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let image_info = vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(format.vk)
        .extent(vk::Extent3D {
            width: buffer.width,
            height: buffer.height,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
        .usage(vk::ImageUsageFlags::SAMPLED)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .push_next(&mut external_info)
        .push_next(&mut modifier_info);
    // SAFETY: the device has VK_EXT_image_drm_format_modifier and the external memory extensions (`open_device`), and
    // `check_buffer` matched the format, the modifier and its plane count against what the device reports it imports
    // as a sampled image; every chained struct lives until the call returns.
    let image = unsafe { raw.create_image(&image_info, None) }.context("vkCreateImage")?;
    match bind_imported_memory(device, image, buffer) {
        Ok(memory) => Ok((image, memory)),
        Err(error) => {
            // SAFETY: the image was created above, bound to nothing and used by nothing.
            unsafe { raw.destroy_image(image, None) };
            Err(error)
        }
    }
}

/// Allocates memory importing `buffer`'s dmabuf for `image` and binds it.
fn bind_imported_memory(device: &vulkan::Device, image: vk::Image, buffer: &DmaBuffer) -> Result<vk::DeviceMemory> {
    let raw = device.raw_device();
    let instance = device.shared_instance().raw_instance();
    let first = buffer.planes.first().ok_or_else(|| anyhow!("no planes"))?;
    let fd = first.fd.try_clone().context("dup the dmabuf fd")?;
    let fd_device = ash::khr::external_memory_fd::Device::new(instance, raw);
    let mut fd_properties = vk::MemoryFdPropertiesKHR::default();
    // SAFETY: VK_KHR_external_memory_fd is enabled on this device; `fd` is an open dmabuf this function owns.
    unsafe {
        fd_device.get_memory_fd_properties(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT, fd.as_raw_fd(), &mut fd_properties)
    }
    .context("vkGetMemoryFdPropertiesKHR")?;
    // SAFETY: `image` was created on this device and is not bound yet.
    let requirements = unsafe { raw.get_image_memory_requirements(image) };
    let type_bits = requirements.memory_type_bits & fd_properties.memory_type_bits;
    ensure!(type_bits != 0, "no memory type fits both the image and the dmabuf");
    let mut import = vk::ImportMemoryFdInfoKHR::default()
        .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
        .fd(fd.as_raw_fd());
    let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
    let allocate = vk::MemoryAllocateInfo::default()
        .allocation_size(requirements.size)
        .memory_type_index(type_bits.trailing_zeros())
        .push_next(&mut import)
        .push_next(&mut dedicated);
    // SAFETY: the memory type is one both the image and the dmabuf accept, the allocation is dedicated to `image`,
    // and `fd` is open. On success Vulkan owns the fd (released from `OwnedFd` below without closing it); on failure
    // it stays ours and `fd` closes it as it drops.
    let memory = unsafe { raw.allocate_memory(&allocate, None) }.context("vkAllocateMemory (dmabuf import)")?;
    let _owned_by_vulkan = fd.into_raw_fd();
    // SAFETY: `memory` was allocated for `image` (dedicated, of its size) and neither is bound or in use.
    if let Err(error) = unsafe { raw.bind_image_memory(image, memory, 0) } {
        // SAFETY: `memory` was allocated above and is bound to nothing.
        unsafe { raw.free_memory(memory, None) };
        bail!("vkBindImageMemory: {error}");
    }
    Ok(memory)
}

// MARK: - completion

/// A timeline semaphore each drawing submission signals, and the thread that reports frames once their submission
/// finished.
struct Timeline {
    semaphore: vk::Semaphore,
    raw: ash::Device,
    next: u64,
    sender: Option<mpsc::Sender<(u64, Vec<Callback>)>>,
    thread: Option<std::thread::JoinHandle<bool>>,
    /// Keeps the device (and so `raw` and `semaphore`) alive until the thread ended.
    _device: wgpu::Device,
}

impl Timeline {
    fn new(device: &wgpu::Device) -> Result<Self> {
        let raw = {
            // SAFETY: the guard is dropped at the end of this block; the ash device it hands out stays valid while
            // `device` lives, which this value keeps.
            let hal_device = unsafe { device.as_hal::<vulkan::Api>() }.context("the device is not Vulkan")?;
            hal_device.raw_device().clone()
        };
        let mut timeline = vk::SemaphoreTypeCreateInfo::default()
            .semaphore_type(vk::SemaphoreType::TIMELINE)
            .initial_value(0);
        // SAFETY: the device was opened with Vulkan 1.2 and the timeline semaphore feature (`missing_requirements`).
        let semaphore = unsafe {
            raw.create_semaphore(&vk::SemaphoreCreateInfo::default().push_next(&mut timeline), None)
        }
        .context("vkCreateSemaphore (timeline)")?;
        let (sender, receiver) = mpsc::channel::<(u64, Vec<Callback>)>();
        let waiter = raw.clone();
        let thread = std::thread::Builder::new()
            .name("gpui-external-surfaces".to_owned())
            .spawn(move || wait_completions(&waiter, semaphore, &receiver));
        let thread = match thread {
            Ok(thread) => thread,
            Err(error) => {
                // SAFETY: the semaphore was created above and nothing waits on or signals it.
                unsafe { raw.destroy_semaphore(semaphore, None) };
                bail!("the completion thread did not start: {error}");
            }
        };
        Ok(Self {
            semaphore,
            raw,
            next: 0,
            sender: Some(sender),
            thread: Some(thread),
            _device: device.clone(),
        })
    }

    fn complete_at(&self, value: u64, callbacks: Vec<Callback>) {
        if let Some(sender) = &self.sender
            && let Err(mpsc::SendError((_, callbacks))) = sender.send((value, callbacks))
        {
            for callback in callbacks {
                callback(FrameOutcome::Failed("the completion thread ended".to_owned()));
            }
        }
    }
}

impl Drop for Timeline {
    fn drop(&mut self) {
        self.sender = None;
        let all_reached = self.thread.take().is_some_and(|thread| thread.join().unwrap_or(false));
        if all_reached {
            // SAFETY: the thread waited every value any submission signals (each value is sent to it right after
            // its submission), so no queue operation and no wait refers to the semaphore any more.
            unsafe { self.raw.destroy_semaphore(self.semaphore, None) };
        } else {
            log::warn!("external surfaces: a submission never finished; its timeline semaphore is leaked");
        }
    }
}

/// The completion thread: for each submission in order, waits until the GPU reached its value and reports its frames.
/// Returns whether every value was reached.
fn wait_completions(raw: &ash::Device, semaphore: vk::Semaphore, receiver: &mpsc::Receiver<(u64, Vec<Callback>)>) -> bool {
    let mut all_reached = true;
    for (value, callbacks) in receiver {
        let outcome = match wait_value(raw, semaphore, value) {
            Ok(()) => FrameOutcome::Presented,
            Err(error) => {
                all_reached = false;
                FrameOutcome::Failed(format!("{error:#}"))
            }
        };
        for callback in callbacks {
            callback(outcome.clone());
        }
    }
    all_reached
}

fn wait_value(raw: &ash::Device, semaphore: vk::Semaphore, value: u64) -> Result<()> {
    let semaphores = [semaphore];
    let values = [value];
    let info = vk::SemaphoreWaitInfo::default().semaphores(&semaphores).values(&values);
    let deadline = Instant::now() + COMPLETION_TIMEOUT;
    loop {
        // SAFETY: the semaphore and device stay alive while this thread runs (`Timeline` joins it before destroying
        // the semaphore and keeps the device); waiting needs no external synchronization.
        match unsafe { raw.wait_semaphores(&info, Duration::from_millis(100).as_nanos() as u64) } {
            Ok(()) => return Ok(()),
            Err(vk::Result::TIMEOUT) if Instant::now() < deadline => {}
            Err(vk::Result::TIMEOUT) => bail!("the GPU did not finish the frame in {COMPLETION_TIMEOUT:?}"),
            Err(error) => bail!("waiting for the frame's submission: {error}"),
        }
    }
}
