//! Frames rendered outside GPUI — the HTML overlay's WPE `WebKit` pages — drawn in a GPUI element: dmabufs imported
//! into GPUI's own Vulkan device with no CPU copy (the GPU takes each frame over from the producer's foreign queue
//! family and copies it into a texture of its own), or, as a last-resort correctness fallback for documents, CPU pixels
//! updated by damaged rectangle (Technical Details > HTML frames in `docs/plans/20261014-gpui-frontend.md`).
//!
//! The import itself is GPUI patch 1 (`vendor/gpui-pre`, `vendor/gpui-pre-wgpu`): `Window::paint_external_surface`,
//! the renderer's copy and draw, and a device opened with `VK_EXT_image_drm_format_modifier` and
//! `VK_EXT_queue_family_foreign`. This module is the frontend's side of it:
//!
//! - [`negotiate`]: from what GPUI's device imports ([`gpui_kit::dmabuf_import_caps`]), the [`FrameSource`] a
//!   producer renders for: its render node (a dmabuf made on another GPU does not import) and the formats and
//!   modifiers to offer, or SHM when dmabuf import cannot work at all.
//! - [`ExternalTexture`]: one producer surface's frames. A dmabuf frame is shown once the producer's rendering into it
//!   finished (its `sync_file`, or the buffer's implicit fence), after its format and modifier were checked against the
//!   device (`WebKit` on NVIDIA's own driver may use a modifier it was not offered); every frame reports its
//!   [`FrameOutcome`] to the producer, `Presented` only once the GPU finished copying it, so the producer reuses a
//!   buffer only after the renderer's last read. A buffer that does not import asks the owner to fall back to SHM
//!   ([`ExternalTexture::take_fallback`]), logged as a warning; SHM is never chosen while dmabufs import.
//! - [`external_texture`]: the element painting the current frame in its bounds, clipped to the content mask like any
//!   GPUI primitive, at its place in the paint order, so overlays above it draw over it.

use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use gpui_kit::{
    DmaBufFrame, DmaBufImportCaps, DmaBuffer, ExternalSurface, FrameCompletion, FrameOutcome, IntoElement, PixelFrame,
    PixelRect, PixelSurface, Styled, Window, canvas,
};

/// `fourcc_code` of `drm_fourcc.h`: `fourcc(b"XR24")` is XRGB8888.
#[must_use]
pub const fn fourcc(code: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*code)
}

/// `DRM_FORMAT_MOD_LINEAR`.
pub const MODIFIER_LINEAR: u64 = 0;
/// `DRM_FORMAT_MOD_INVALID`: an implicit modifier, which an explicit-modifier import cannot describe.
pub const MODIFIER_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

/// The formats the renderer draws, in the order a producer is asked for them: with alpha first (a page may be
/// transparent), the native byte order first.
const DRAWN_FORMATS: [u32; 4] = [fourcc(b"AR24"), fourcc(b"XR24"), fourcc(b"AB24"), fourcc(b"XB24")];

// MARK: - negotiation

/// A format offered to a producer, with its modifiers, best first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfferedFormat {
    pub fourcc: u32,
    pub modifiers: Vec<u64>,
}

/// How a producer hands its frames over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameSource {
    /// Dmabufs allocated on `render_node`, GPUI's own GPU, in one of `formats`.
    DmaBuf {
        render_node: PathBuf,
        formats: Vec<OfferedFormat>,
    },
    /// CPU pixels: dmabuf import cannot work on this device, for `reason`. Documents only (Task 2: 80–115 % CPU on
    /// 1080p video, 2.5 fps on `YouTube`).
    Shm { reason: String },
}

/// The [`FrameSource`] for GPUI's device as `caps` describes it (`None`: no renderer has opened one yet); SHM, logged
/// as a warning, only when dmabufs cannot be imported at all.
#[must_use]
pub fn negotiate(caps: Option<&DmaBufImportCaps>) -> FrameSource {
    let source = match caps {
        None => shm("GPUI has not opened its GPU device yet"),
        Some(caps) => match (&caps.unsupported, &caps.render_node) {
            (Some(reason), _) => shm(&format!("{} imports no dmabufs: {reason}", caps.adapter)),
            (None, None) => shm(&format!("{} names no DRM render node to render on", caps.adapter)),
            (None, Some(node)) => offer(caps, node),
        },
    };
    if let FrameSource::Shm { reason } = &source {
        tracing::warn!(
            "external frames fall back to shared memory: {reason}; this mode is for documents only, video is not \
             supported in it"
        );
    }
    source
}

fn shm(reason: &str) -> FrameSource {
    FrameSource::Shm {
        reason: reason.to_owned(),
    }
}

/// The drawn formats `caps` imports, preferred first, each with its explicit modifiers, LINEAR last (a tiled or
/// compressed layout is the device's own and faster to sample).
fn offer(caps: &DmaBufImportCaps, node: &std::path::Path) -> FrameSource {
    let formats: Vec<OfferedFormat> = DRAWN_FORMATS
        .iter()
        .filter_map(|&fourcc| {
            let format = caps.formats.iter().find(|format| format.fourcc == fourcc)?;
            let mut modifiers: Vec<u64> = format
                .modifiers
                .iter()
                .map(|modifier| modifier.modifier)
                .filter(|&modifier| modifier != MODIFIER_INVALID)
                .collect();
            modifiers.sort_by_key(|&modifier| modifier == MODIFIER_LINEAR);
            modifiers.dedup();
            (!modifiers.is_empty()).then_some(OfferedFormat { fourcc, modifiers })
        })
        .collect();
    if formats.is_empty() {
        return shm(&format!("{} imports none of the drawn formats", caps.adapter));
    }
    FrameSource::DmaBuf {
        render_node: node.to_path_buf(),
        formats,
    }
}

/// Why a buffer does not import on the device `caps` describes, if it does not: the check the renderer makes before
/// Vulkan sees a buffer, made early so a producer that ignored the offer (NVIDIA's `WebKit` picking its own modifier)
/// is caught before its frame is queued.
#[must_use]
pub fn import_refusal(caps: &DmaBufImportCaps, buffer: &DmaBuffer) -> Option<String> {
    if let Some(reason) = &caps.unsupported {
        return Some(format!("the device imports no dmabufs: {reason}"));
    }
    let name = fourcc_name(buffer.fourcc);
    let Some(format) = caps.formats.iter().find(|format| format.fourcc == buffer.fourcc) else {
        return Some(format!("the device does not import {name}"));
    };
    let Some(modifier) = format
        .modifiers
        .iter()
        .find(|modifier| modifier.modifier == buffer.modifier)
    else {
        return Some(format!(
            "the device does not import {name} with modifier {:#018x}",
            buffer.modifier
        ));
    };
    if buffer.planes.len() != modifier.plane_count as usize {
        return Some(format!(
            "{name} modifier {:#018x} has {} planes, the buffer {}",
            buffer.modifier,
            modifier.plane_count,
            buffer.planes.len()
        ));
    }
    (buffer.width == 0 || buffer.height == 0).then(|| "an empty buffer".to_owned())
}

/// The four characters of a fourcc.
#[must_use]
pub fn fourcc_name(fourcc: u32) -> String {
    fourcc
        .to_le_bytes()
        .iter()
        .map(|&byte| if byte.is_ascii_graphic() { char::from(byte) } else { '?' })
        .collect()
}

// MARK: - frames

/// What a dmabuf frame waits for before it is shown: the producer's rendering into it.
#[derive(Debug)]
pub enum Acquire {
    /// The buffer's implicit fence: its dmabuf polls readable once every write to it finished.
    Implicit,
    /// An explicit fence (a `sync_file`), readable once signaled.
    SyncFile(OwnedFd),
}

/// A damaged region of an SHM frame: premultiplied BGRA pixels, rows `rect.width * 4` bytes apart.
#[derive(Debug, Clone, Copy)]
pub struct ShmRegion<'a> {
    pub rect: PixelRect,
    pub pixels: &'a [u8],
}

/// Whether frames come as dmabufs or have fallen back to SHM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fallback {
    /// Dmabufs import.
    None,
    /// A dmabuf did not import, for this reason; not yet taken by the owner.
    Requested(String),
    /// The owner took the request: dmabuf frames are refused from now on.
    Taken,
}

/// A dmabuf frame waiting for its acquire fence.
struct Pending {
    surface: ExternalSurface,
    acquire: Acquire,
    buffer: Arc<DmaBuffer>,
}

/// The CPU copies behind SHM frames, two of them in turn: the last frame's pixels stay with the renderer and GPUI's
/// retained scene until the next frame is drawn, so each frame is composed in the other copy, which first catches up
/// on the damage of the frame before (it holds the frame before that). Only when the other copy is still held too
/// (frames arriving faster than they are drawn) does an update copy the whole frame (`Arc::make_mut`).
struct ShmCanvas {
    surface: Arc<PixelSurface>,
    width: u32,
    height: u32,
    copies: [Arc<Vec<u8>>; 2],
    /// The copy holding the latest frame.
    latest: usize,
    /// The latest frame's damage, which the other copy lacks.
    latest_damage: Vec<PixelRect>,
    serial: u64,
}

/// The frames of one producer surface (an HTML overlay page) and what it shows now.
pub struct ExternalTexture {
    shown: Option<ExternalSurface>,
    pending: Option<Pending>,
    canvas: Option<ShmCanvas>,
    fallback: Fallback,
    /// Failures the renderer reported for this texture's frames, from its threads.
    failures: Arc<Mutex<Option<String>>>,
    /// The device capabilities to check buffers against; GPUI's own unless a test set them.
    caps: Option<Arc<DmaBufImportCaps>>,
}

impl Default for ExternalTexture {
    fn default() -> Self {
        Self::new()
    }
}

impl ExternalTexture {
    /// A texture showing nothing yet, checking buffers against GPUI's device.
    #[must_use]
    pub fn new() -> Self {
        Self {
            shown: None,
            pending: None,
            canvas: None,
            fallback: Fallback::None,
            failures: Arc::new(Mutex::new(None)),
            caps: None,
        }
    }

    /// A texture checking buffers against `caps` instead of GPUI's device.
    #[must_use]
    pub fn with_caps(caps: Arc<DmaBufImportCaps>) -> Self {
        Self {
            caps: Some(caps),
            ..Self::new()
        }
    }

    /// Queues a dmabuf frame of `buffer` (the producer keeps the `Arc` while the buffer is in its set: the renderer's
    /// import lives as long). It is shown once `acquire` signals; `on_done` gets its outcome. A buffer the device does
    /// not import fails at once and requests the fall-back; after the fall-back was taken, dmabuf frames are
    /// discarded.
    pub fn push_dmabuf(
        &mut self,
        buffer: Arc<DmaBuffer>,
        acquire: Acquire,
        on_done: impl FnOnce(FrameOutcome) + Send + 'static,
    ) {
        if self.fallback == Fallback::Taken {
            on_done(FrameOutcome::Discarded);
            return;
        }
        let caps = self.caps.clone().or_else(gpui_kit::dmabuf_import_caps);
        let refusal = match &caps {
            Some(caps) => import_refusal(caps, &buffer),
            None => Some("GPUI has not opened its GPU device yet".to_owned()),
        };
        if let Some(reason) = refusal {
            self.request_fallback(&reason);
            on_done(FrameOutcome::Failed(reason));
            return;
        }
        let failures = Arc::clone(&self.failures);
        let completion = FrameCompletion::new(move |outcome| {
            if let FrameOutcome::Failed(reason) = &outcome {
                failures
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get_or_insert_with(|| reason.clone());
            }
            on_done(outcome);
        });
        let surface = ExternalSurface::DmaBuf(Arc::new(DmaBufFrame {
            buffer: Arc::clone(&buffer),
            completion,
        }));
        // A frame still waiting is replaced; dropping it reports it discarded.
        self.pending = Some(Pending {
            surface,
            acquire,
            buffer,
        });
    }

    /// Shows an SHM frame of `width` x `height`: `regions` changed since the frame before (the whole frame when the
    /// size changed). Pixels outside the frame are ignored. `on_done` gets the frame's outcome.
    pub fn push_shm<'a>(
        &mut self,
        width: u32,
        height: u32,
        regions: impl IntoIterator<Item = ShmRegion<'a>>,
        on_done: impl FnOnce(FrameOutcome) + Send + 'static,
    ) {
        let canvas = match &mut self.canvas {
            Some(canvas) if canvas.width == width && canvas.height == height => canvas,
            slot => slot.insert(ShmCanvas::new(width, height)),
        };
        let (pixels, damage) = canvas.apply(regions);
        canvas.serial += 1;
        let frame = PixelFrame {
            surface: Arc::clone(&canvas.surface),
            serial: canvas.serial,
            width,
            height,
            pixels,
            damage,
            completion: FrameCompletion::new(on_done),
        };
        self.pending = None;
        self.shown = Some(ExternalSurface::Pixels(Arc::new(frame)));
    }

    /// The frame to paint now: a queued dmabuf frame replaces the shown one once its fence signaled. While one is
    /// still waiting, asks `window` for another frame to look again.
    pub fn prepare(&mut self, window: &mut Window) -> Option<ExternalSurface> {
        if self.promote_ready() {
            window.request_animation_frame();
        }
        self.shown.clone()
    }

    /// Promotes a queued frame whose fence signaled; whether one is still waiting.
    pub fn promote_ready(&mut self) -> bool {
        let Some(pending) = &self.pending else {
            return false;
        };
        let ready = match &pending.acquire {
            Acquire::Implicit => pending.buffer.planes.first().is_none_or(|plane| fd_signaled(&plane.fd)),
            Acquire::SyncFile(fence) => fd_signaled(fence),
        };
        if ready {
            self.shown = self.pending.take().map(|pending| pending.surface);
        }
        !ready
    }

    /// The frame shown now.
    #[must_use]
    pub fn shown(&self) -> Option<&ExternalSurface> {
        self.shown.as_ref()
    }

    /// Forgets the shown and the queued frame (the producer's page went away); their outcomes follow once the
    /// renderer let them go.
    pub fn clear(&mut self) {
        self.pending = None;
        self.shown = None;
        self.canvas = None;
    }

    /// The fall-back to SHM, once, when a dmabuf did not import (checked here, or `Failed` in the renderer): the owner
    /// then has its producer switch to SHM. Logged as a warning when requested. A frame `Lost` to the GPU (device lost,
    /// renderer gone) or `Discarded` says nothing about import and never asks for it.
    pub fn take_fallback(&mut self) -> Option<String> {
        let failed = self.failures.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(reason) = failed {
            self.request_fallback(&reason);
        }
        match std::mem::replace(&mut self.fallback, Fallback::Taken) {
            Fallback::Requested(reason) => Some(reason),
            other => {
                self.fallback = other;
                None
            }
        }
    }

    /// Where the fall-back stands.
    #[must_use]
    pub fn fallback(&self) -> &Fallback {
        &self.fallback
    }

    fn request_fallback(&mut self, reason: &str) {
        if self.fallback == Fallback::None {
            tracing::warn!(
                "a dmabuf frame does not import ({reason}): falling back to shared memory for this page; this mode is \
                 for documents only, video is not supported in it"
            );
            self.fallback = Fallback::Requested(reason.to_owned());
        }
    }
}

/// Whether `fd` (a dmabuf or a `sync_file`) polls readable: every write to the buffer finished, or the fence signaled.
/// A descriptor that cannot be polled counts as signaled, so a frame is never held forever.
fn fd_signaled(fd: &impl AsFd) -> bool {
    let mut poll = libc::pollfd {
        fd: fd.as_fd().as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: `poll` points at one initialized pollfd whose descriptor stays open for the call (borrowed from `fd`);
    // a zero timeout never blocks.
    let result = unsafe { libc::poll(&raw mut poll, 1, 0) };
    result != 0 && (result < 0 || poll.revents != 0)
}

impl ShmCanvas {
    fn new(width: u32, height: u32) -> Self {
        let size = usize::try_from(u64::from(width) * u64::from(height) * 4).unwrap_or(0);
        Self {
            surface: PixelSurface::new(),
            width,
            height,
            copies: [Arc::new(vec![0; size]), Arc::new(vec![0; size])],
            latest: 0,
            latest_damage: Vec::new(),
            serial: 0,
        }
    }

    /// Composes the next frame in the other copy: the latest frame's damage, then `regions`, clipped to the frame; the
    /// frame's pixels and the rectangles `regions` wrote.
    fn apply<'a>(&mut self, regions: impl IntoIterator<Item = ShmRegion<'a>>) -> (Arc<Vec<u8>>, Vec<PixelRect>) {
        let (width, height) = (self.width, self.height);
        let target = 1 - self.latest;
        let latest = Arc::clone(&self.copies[self.latest]);
        let pixels = Arc::make_mut(&mut self.copies[target]);
        let stride = width as usize * 4;
        for rect in &self.latest_damage {
            let row_bytes = rect.width as usize * 4;
            for row in rect.y as usize..(rect.y + rect.height) as usize {
                let start = row * stride + rect.x as usize * 4;
                pixels[start..start + row_bytes].copy_from_slice(&latest[start..start + row_bytes]);
            }
        }
        let damage: Vec<PixelRect> = regions
            .into_iter()
            .filter_map(|region| copy_region(pixels, width, height, region))
            .collect();
        self.latest = target;
        self.latest_damage.clone_from(&damage);
        (Arc::clone(&self.copies[target]), damage)
    }
}

/// Copies `region` into the `width` x `height` frame `pixels`, clipped to the frame; the rectangle written, `None` when
/// nothing was (empty after clipping, or fewer pixels than the rectangle needs).
fn copy_region(pixels: &mut [u8], width: u32, height: u32, region: ShmRegion<'_>) -> Option<PixelRect> {
    let rect = region.rect;
    let source_stride = rect.width as usize * 4;
    if region.pixels.len() < source_stride * rect.height as usize {
        tracing::warn!(
            "an SHM region of {}x{} came with {} bytes",
            rect.width,
            rect.height,
            region.pixels.len()
        );
        return None;
    }
    let x = rect.x.min(width);
    let y = rect.y.min(height);
    let clipped = PixelRect {
        x,
        y,
        width: rect.width.min(width - x),
        height: rect.height.min(height - y),
    };
    if clipped.width == 0 || clipped.height == 0 {
        return None;
    }
    let stride = width as usize * 4;
    let row_bytes = clipped.width as usize * 4;
    for row in 0..clipped.height as usize {
        let source = row * source_stride;
        let target = (y as usize + row) * stride + x as usize * 4;
        pixels[target..target + row_bytes].copy_from_slice(&region.pixels[source..source + row_bytes]);
    }
    Some(clipped)
}

// MARK: - the element

/// An element painting `surface` (from [`ExternalTexture::prepare`]) over its bounds; nothing while there is none.
/// Size it like any element (`size_full()` to fill its parent).
#[must_use]
pub fn external_texture(surface: Option<ExternalSurface>) -> impl IntoElement + Styled {
    canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            if let Some(surface) = surface {
                window.paint_external_surface(bounds, surface);
            }
        },
    )
}

#[cfg(test)]
mod tests;
