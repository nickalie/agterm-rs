//! The cases, run in one child process per GPU on GPUI's headless renderer.
//!
//! A [`Fixture`] is a headless GPUI app with one 64x32 window (128x64 device pixels at the test window's scale of 2)
//! showing an [`ExternalTexture`]; every frame is rendered and read back. Counters come from
//! `gpui::external_surface_stats`, process-wide, so cases compare before and after. The last two cases paint, each in
//! a window of its own on the same renderer, the coverage masks of GPUI patch 0003 (the terminal's sprites) and a
//! Kitty placement's source rectangle (the terminal's images, through GPUI's own `paint_image`).

use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    App, AppContext as _, Bounds, Context, Corners, DevicePixels, DmaBufImportCaps, ExternalSurfaceStats, FrameOutcome,
    HeadlessAppContext, Hsla, IntoElement, NoopTextSystem, ParentElement as _, PixelRect, Pixels, Render,
    RenderAlphaMaskParams, RenderImage, Styled as _, Window, WindowBounds, WindowHandle, WindowOptions, canvas, div,
    external_surface_stats, point, px, size,
};
use ngterm_gpui::external_texture::{
    Acquire, ExternalTexture, Fallback, FrameSource, ShmRegion, external_texture, negotiate,
};
use ngterm_gpui::terminal::kitty;
use ngterm_term::SourceRect;

use crate::buffers::{AR24, Udmabuf, XR24};
use crate::gl::{GlBuffer, GlProducer};

type Outcome = Result<Verdict, String>;

/// How a case ended when it did not fail.
pub enum Verdict {
    Passed,
    /// Something this machine lacks: fails under `NGTERM_REQUIRE_DISPLAY_TESTS`.
    Skipped(String),
    /// The case does not apply to this GPU (hardware modifiers on lavapipe): never required.
    NotApplicable(String),
}

/// What a case runs with.
pub struct Gpu {
    pub name: String,
    /// The render node of the `mesa` GPU.
    pub node: Option<PathBuf>,
}

pub type Case = fn(&Gpu) -> Outcome;

pub const CASES: [(&str, Case); 13] = [
    ("caps", caps),
    ("dmabuf_draws", dmabuf_draws),
    ("dmabuf_import_is_kept_per_buffer", dmabuf_import_is_kept_per_buffer),
    ("dmabuf_released_with_its_buffer", dmabuf_released_with_its_buffer),
    ("dmabuf_waits_for_its_fence", dmabuf_waits_for_its_fence),
    ("dmabuf_with_alpha_blends", dmabuf_with_alpha_blends),
    ("dmabuf_outside_the_caps_is_refused", dmabuf_outside_the_caps_is_refused),
    (
        "dmabuf_that_fails_to_import_falls_back",
        dmabuf_that_fails_to_import_falls_back,
    ),
    (
        "dmabuf_rendered_by_gl_with_the_devices_modifiers",
        dmabuf_rendered_by_gl_with_the_devices_modifiers,
    ),
    ("shm_uploads_its_damage", shm_uploads_its_damage),
    ("clipped_and_drawn_under_overlays", clipped_and_drawn_under_overlays),
    (
        "alpha_masks_cover_whole_device_pixels",
        alpha_masks_cover_whole_device_pixels,
    ),
    (
        "image_source_rects_fill_their_placement",
        image_source_rects_fill_their_placement,
    ),
];

/// Runs the cases `filter` selects on `gpu`; the child's exit code.
pub fn run(gpu: &str, node: Option<PathBuf>, filter: Option<&str>) -> ExitCode {
    let gpu = Gpu {
        name: gpu.to_owned(),
        node,
    };
    let required = std::env::var_os(crate::REQUIRE_ENV).is_some();
    let mut failed = 0;
    for (name, case) in CASES {
        if filter.is_some_and(|filter| !name.contains(filter)) {
            continue;
        }
        match case(&gpu) {
            Ok(Verdict::Passed) => println!("  {name} ... ok"),
            Ok(Verdict::NotApplicable(reason)) => println!("  {name} ... not applicable ({reason})"),
            Ok(Verdict::Skipped(reason)) if required => {
                println!(
                    "  {name} ... FAILED on {}: skipped under {}: {reason}",
                    gpu.name,
                    crate::REQUIRE_ENV
                );
                failed += 1;
            }
            Ok(Verdict::Skipped(reason)) => println!("  {name} ... skipped ({reason})"),
            Err(message) => {
                println!("  {name} ... FAILED on {}: {message}", gpu.name);
                failed += 1;
            }
        }
    }
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

// MARK: - the fixture

const WAIT: Duration = Duration::from_secs(10);
const RED: [u8; 4] = [255, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const BLACK: [u8; 4] = [0, 0, 0, 255];
/// B, G, R, X bytes of an XR24 pixel, the padding byte zero (it must not read as transparent).
const RED_X: [u8; 4] = [0, 0, 255, 0];
const BLUE_X: [u8; 4] = [255, 0, 0, 0];
const GREEN_X: [u8; 4] = [0, 255, 0, 0];

#[derive(Clone, Copy)]
enum Layout {
    /// The texture fills the window.
    Fill,
    /// The texture (64 wide) in a 32-wide box that clips it, under an 8x8 green square drawn after it.
    Clipped,
}

struct Stage {
    texture: ExternalTexture,
    layout: Layout,
    background: Hsla,
}

impl Render for Stage {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let surface = self.texture.prepare(window);
        let root = div().size_full().bg(self.background);
        match self.layout {
            Layout::Fill => root.child(external_texture(surface).size_full()),
            Layout::Clipped => root
                .relative()
                .child(
                    div()
                        .w(px(32.))
                        .h_full()
                        .overflow_hidden()
                        .child(external_texture(surface).w(px(64.)).h_full().flex_shrink_0()),
                )
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size(px(8.))
                        .bg(gpui_kit::rgb(0x00_ff00)),
                ),
        }
    }
}

/// A frame read back: RGBA rows.
struct Shot {
    width: u32,
    raw: Vec<u8>,
}

impl Shot {
    fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let at = (y * self.width + x) as usize * 4;
        [self.raw[at], self.raw[at + 1], self.raw[at + 2], self.raw[at + 3]]
    }

    /// Fails unless the device pixel at (`x`, `y`) is `expected`, give or take rounding.
    fn expect(&self, x: u32, y: u32, expected: [u8; 4], what: &str) -> Result<(), String> {
        let actual = self.pixel(x, y);
        let close = actual.iter().zip(expected).all(|(&a, e)| a.abs_diff(e) <= 3);
        if close {
            Ok(())
        } else {
            Err(format!("{what}: pixel ({x}, {y}) is {actual:?}, expected {expected:?}"))
        }
    }
}

struct Fixture {
    cx: HeadlessAppContext,
    window: WindowHandle<Stage>,
}

impl Fixture {
    fn new(layout: Layout, background: Hsla) -> Result<Self, String> {
        let mut cx = HeadlessAppContext::with_platform(
            Arc::new(NoopTextSystem),
            Arc::new(()),
            gpui_kit::platform::current_headless_renderer,
        );
        let window = cx
            .update(|cx| {
                cx.open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(gpui_kit::Bounds {
                            origin: point(px(0.), px(0.)),
                            size: size(px(64.), px(32.)),
                        })),
                        focus: false,
                        show: false,
                        ..Default::default()
                    },
                    |_, cx| {
                        cx.new(|_| Stage {
                            texture: ExternalTexture::new(),
                            layout,
                            background,
                        })
                    },
                )
            })
            .map_err(|error| format!("the window did not open: {error:#}"))?;
        let mut fixture = Self { cx, window };
        // The first frame opens GPUI's device, which the texture checks buffers against.
        fixture.frame()?;
        Ok(fixture)
    }

    fn texture<R>(&mut self, f: impl FnOnce(&mut ExternalTexture) -> R) -> Result<R, String> {
        let window = self.window;
        self.cx
            .update(|cx| {
                window.update(cx, |stage, _, cx| {
                    cx.notify();
                    f(&mut stage.texture)
                })
            })
            .map_err(|error| format!("the window is gone: {error:#}"))
    }

    /// Renders a frame and reads it back.
    fn frame(&mut self) -> Result<Shot, String> {
        let handle = self.window.into();
        self.cx
            .update_window(handle, |_, window, cx| window.render_frame(cx))
            .map_err(|error| format!("rendering: {error:#}"))?;
        let image = self
            .cx
            .capture_screenshot(handle)
            .map_err(|error| format!("reading back: {error:#}"))?;
        let width = image.width();
        Ok(Shot {
            width,
            raw: image.into_raw(),
        })
    }
}

/// An outcome callback and where its outcome lands.
fn listener() -> (impl FnOnce(FrameOutcome) + Send + 'static, mpsc::Receiver<FrameOutcome>) {
    let (sender, receiver) = mpsc::channel();
    (
        move |outcome| {
            let _ = sender.send(outcome);
        },
        receiver,
    )
}

fn outcome_of(receiver: &mpsc::Receiver<FrameOutcome>, what: &str) -> Result<FrameOutcome, String> {
    receiver
        .recv_timeout(WAIT)
        .map_err(|_| format!("{what}: no outcome in {WAIT:?}"))
}

fn presented(receiver: &mpsc::Receiver<FrameOutcome>, what: &str) -> Result<(), String> {
    match outcome_of(receiver, what)? {
        FrameOutcome::Presented => Ok(()),
        other => Err(format!("{what}: {other:?}, expected Presented")),
    }
}

fn caps_now() -> Result<Arc<DmaBufImportCaps>, String> {
    gpui_kit::dmabuf_import_caps().ok_or_else(|| "GPUI recorded no import capabilities".to_owned())
}

fn black() -> Hsla {
    gpui_kit::black()
}

fn check(condition: bool, message: impl FnOnce() -> String) -> Result<(), String> {
    if condition { Ok(()) } else { Err(message()) }
}

/// A udmabuf, or why the case cannot run (no access to `/dev/udmabuf`, a device that imports no dmabufs).
fn udmabuf(width: u32, height: u32, bgra: [u8; 4]) -> Result<Result<Udmabuf, Verdict>, String> {
    if let Some(reason) = Udmabuf::unavailable() {
        return Ok(Err(Verdict::Skipped(reason)));
    }
    if let Some(reason) = caps_now()?.unsupported.clone() {
        return Ok(Err(Verdict::Skipped(format!(
            "the device imports no dmabufs: {reason}"
        ))));
    }
    Udmabuf::new(width, height, bgra).map(Ok)
}

macro_rules! or_skip {
    ($made:expr) => {
        match $made? {
            Ok(made) => made,
            Err(verdict) => return Ok(verdict),
        }
    };
}

fn stats() -> ExternalSurfaceStats {
    external_surface_stats()
}

// MARK: - capabilities

fn caps(gpu: &Gpu) -> Outcome {
    Fixture::new(Layout::Fill, black())?;
    let caps = caps_now()?;
    if let (Some(reason), None) = (&caps.unsupported, &gpu.node) {
        // An older lavapipe (before VK_EXT_image_drm_format_modifier): SHM, never a dmabuf it cannot import.
        check(matches!(negotiate(Some(&caps)), FrameSource::Shm { .. }), || {
            "dmabufs offered".to_owned()
        })?;
        return Ok(Verdict::Skipped(format!(
            "{} imports no dmabufs: {reason}",
            caps.adapter
        )));
    }
    check(caps.unsupported.is_none(), || {
        format!("{} imports no dmabufs: {:?}", caps.adapter, caps.unsupported)
    })?;
    let xr24 = caps
        .formats
        .iter()
        .find(|format| format.fourcc == XR24)
        .ok_or_else(|| format!("{} imports no XR24: {caps:?}", caps.adapter))?;
    check(xr24.modifiers.iter().any(|modifier| modifier.modifier == 0), || {
        format!("XR24 without LINEAR: {xr24:?}")
    })?;
    let Some(node) = &gpu.node else {
        return Ok(Verdict::Passed);
    };
    check(
        caps.render_node.as_deref().map(canonical) == Some(canonical(node)),
        || format!("render node {:?}, the GPU's is {}", caps.render_node, node.display()),
    )?;
    check(xr24.modifiers.iter().any(|modifier| modifier.modifier != 0), || {
        format!("a hardware GPU with only LINEAR: {xr24:?}")
    })?;
    match negotiate(Some(&caps)) {
        FrameSource::DmaBuf { render_node, formats } => check(
            canonical(&render_node) == canonical(node) && formats.iter().any(|format| format.fourcc == XR24),
            || format!("offered {formats:?} on {}", render_node.display()),
        )?,
        FrameSource::Shm { reason } => return Err(format!("SHM chosen while dmabufs import: {reason}")),
    }
    Ok(Verdict::Passed)
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

// MARK: - dmabufs

fn dmabuf_draws(_: &Gpu) -> Outcome {
    let mut fixture = Fixture::new(Layout::Fill, black())?;
    let pixels = or_skip!(udmabuf(64, 32, RED_X));
    pixels.fill_from_column(32, BLUE_X)?;
    let before = stats();
    let (done, outcome) = listener();
    let buffer = pixels.buffer(XR24)?;
    fixture.texture(|texture| texture.push_dmabuf(Arc::clone(&buffer), Acquire::Implicit, done))?;
    std::thread::sleep(Duration::from_millis(50));
    check(outcome.try_recv().is_err(), || {
        "an outcome before the frame was drawn".to_owned()
    })?;
    let shot = fixture.frame()?;
    shot.expect(16, 32, RED, "the left half")?;
    shot.expect(112, 32, BLUE, "the right half")?;
    presented(&outcome, "the frame")?;
    let after = stats();
    check(after.imports == before.imports + 1, || {
        format!("{before:?} -> {after:?}: one import expected")
    })?;
    check(fixture.texture(ExternalTexture::take_fallback)?.is_none(), || {
        "a fall-back was requested".to_owned()
    })?;
    Ok(Verdict::Passed)
}

fn dmabuf_import_is_kept_per_buffer(_: &Gpu) -> Outcome {
    let mut fixture = Fixture::new(Layout::Fill, black())?;
    let pixels = or_skip!(udmabuf(64, 32, RED_X));
    let buffer = pixels.buffer(XR24)?;
    let before = stats();
    let (done, first) = listener();
    fixture.texture(|texture| texture.push_dmabuf(Arc::clone(&buffer), Acquire::Implicit, done))?;
    fixture.frame()?.expect(64, 32, RED, "the first frame")?;
    presented(&first, "the first frame")?;
    // The producer renders the next frame into the same buffer: no new import, the new pixels show.
    pixels.fill(GREEN_X)?;
    let (done, second) = listener();
    fixture.texture(|texture| texture.push_dmabuf(Arc::clone(&buffer), Acquire::Implicit, done))?;
    fixture
        .frame()?
        .expect(64, 32, GREEN, "the second frame, same buffer")?;
    presented(&second, "the second frame")?;
    // Presented: the buffer is the producer's again. What it renders now does not reach a redraw of the frame.
    pixels.fill(BLUE_X)?;
    fixture
        .frame()?
        .expect(64, 32, GREEN, "a redraw after the producer rendered again")?;
    let after = stats();
    check(
        after.imports == before.imports + 1 && after.copies == before.copies + 2,
        || format!("{before:?} -> {after:?}: one import for one buffer, one copy per frame"),
    )?;
    Ok(Verdict::Passed)
}

fn dmabuf_released_with_its_buffer(_: &Gpu) -> Outcome {
    let mut fixture = Fixture::new(Layout::Fill, black())?;
    let first_pixels = or_skip!(udmabuf(64, 32, RED_X));
    let second_pixels = Udmabuf::new(64, 32, BLUE_X)?;
    let first = first_pixels.buffer(XR24)?;
    let live = stats().live_imports();
    let (done, first_done) = listener();
    fixture.texture(|texture| texture.push_dmabuf(Arc::clone(&first), Acquire::Implicit, done))?;
    fixture.frame()?;
    presented(&first_done, "the first buffer's frame")?;
    check(stats().live_imports() == live + 1, || {
        "the first buffer is not imported".to_owned()
    })?;

    let (done, second_done) = listener();
    let second = second_pixels.buffer(XR24)?;
    fixture.texture(|texture| texture.push_dmabuf(second, Acquire::Implicit, done))?;
    fixture.frame()?.expect(64, 32, BLUE, "the second buffer")?;
    presented(&second_done, "the second buffer's frame")?;
    // The first buffer is the producer's again: what it renders into it never reaches the screen.
    first_pixels.fill(GREEN_X)?;
    fixture
        .frame()?
        .expect(64, 32, BLUE, "after the first buffer was reused")?;
    check(stats().live_imports() == live + 2, || {
        "the first import went while its buffer is in the set".to_owned()
    })?;
    // It leaves the producer's set: the import goes once no frame holds it.
    drop(first);
    fixture.frame()?;
    fixture.frame()?.expect(64, 32, BLUE, "after the first buffer left")?;
    let after = stats().live_imports();
    check(after == live + 1, || {
        format!("live imports {after}, expected {}", live + 1)
    })?;
    Ok(Verdict::Passed)
}

fn dmabuf_waits_for_its_fence(_: &Gpu) -> Outcome {
    let mut fixture = Fixture::new(Layout::Fill, black())?;
    let first_pixels = or_skip!(udmabuf(64, 32, RED_X));
    let second_pixels = Udmabuf::new(64, 32, BLUE_X)?;
    let (done, first) = listener();
    let buffer = first_pixels.buffer(XR24)?;
    fixture.texture(|texture| texture.push_dmabuf(buffer, Acquire::Implicit, done))?;
    fixture.frame()?;
    presented(&first, "the first frame")?;
    let (fence, mut signal) = std::io::pipe().map_err(|error| format!("a pipe: {error}"))?;
    let (done, second) = listener();
    let buffer = second_pixels.buffer(XR24)?;
    fixture.texture(|texture| texture.push_dmabuf(buffer, Acquire::SyncFile(OwnedFd::from(fence)), done))?;
    fixture.frame()?.expect(64, 32, RED, "before the fence signaled")?;
    std::io::Write::write_all(&mut signal, b"1").map_err(|error| format!("signaling: {error}"))?;
    fixture.frame()?.expect(64, 32, BLUE, "after the fence signaled")?;
    presented(&second, "the fenced frame")?;
    Ok(Verdict::Passed)
}

fn dmabuf_with_alpha_blends(_: &Gpu) -> Outcome {
    let mut fixture = Fixture::new(Layout::Fill, gpui_kit::white())?;
    // Premultiplied half-transparent red over white: red stays full, green and blue halve.
    let pixels = or_skip!(udmabuf(64, 32, [0, 0, 128, 128]));
    let (done, outcome) = listener();
    let buffer = pixels.buffer(AR24)?;
    fixture.texture(|texture| texture.push_dmabuf(buffer, Acquire::Implicit, done))?;
    let shot = fixture.frame()?;
    presented(&outcome, "the frame")?;
    let [red, green, blue, _] = shot.pixel(64, 32);
    check(
        red >= 250 && (120..=136).contains(&green) && (120..=136).contains(&blue),
        || format!("half-transparent red over white is {:?}", shot.pixel(64, 32)),
    )?;
    Ok(Verdict::Passed)
}

fn dmabuf_outside_the_caps_is_refused(_: &Gpu) -> Outcome {
    let mut fixture = Fixture::new(Layout::Fill, black())?;
    let pixels = or_skip!(udmabuf(64, 32, RED_X));
    let before = stats();
    // As WebKit on NVIDIA's driver does: a block-linear modifier nobody offered.
    let buffer = pixels.buffer(XR24)?;
    let foreign = Arc::new(gpui_kit::DmaBuffer {
        modifier: 0x0300_0000_00e0_8014,
        planes: vec![gpui_kit::DmaBufPlane {
            fd: buffer.planes[0].fd.try_clone().map_err(|error| error.to_string())?,
            offset: 0,
            stride: 256,
        }],
        width: 64,
        height: 32,
        fourcc: XR24,
    });
    let (done, outcome) = listener();
    fixture.texture(|texture| texture.push_dmabuf(foreign, Acquire::Implicit, done))?;
    check(
        matches!(outcome_of(&outcome, "the frame")?, FrameOutcome::Failed(_)),
        || "not failed".to_owned(),
    )?;
    fixture.frame()?.expect(64, 32, BLACK, "nothing drawn")?;
    let fallback = fixture.texture(ExternalTexture::take_fallback)?;
    check(fallback.is_some_and(|reason| reason.contains("modifier")), || {
        "no fall-back".to_owned()
    })?;
    let after = stats();
    check(
        after.import_failures == before.import_failures && after.imports == before.imports,
        || "the renderer saw the refused buffer".to_owned(),
    )?;
    Ok(Verdict::Passed)
}

fn dmabuf_that_fails_to_import_falls_back(_: &Gpu) -> Outcome {
    let mut fixture = Fixture::new(Layout::Fill, black())?;
    if let Some(reason) = caps_now()?.unsupported.clone() {
        return Ok(Verdict::Skipped(format!("the device imports no dmabufs: {reason}")));
    }
    let before = stats();
    // A buffer the caps accept whose fd is no dmabuf: only the renderer's import can tell.
    let (reader, _writer) = std::io::pipe().map_err(|error| format!("a pipe: {error}"))?;
    let bogus = Arc::new(gpui_kit::DmaBuffer {
        width: 64,
        height: 32,
        fourcc: XR24,
        modifier: 0,
        planes: vec![gpui_kit::DmaBufPlane {
            fd: OwnedFd::from(reader),
            offset: 0,
            stride: 256,
        }],
    });
    let (done, outcome) = listener();
    let fence = signaled_fence()?;
    fixture.texture(|texture| texture.push_dmabuf(bogus, Acquire::SyncFile(fence), done))?;
    fixture.frame()?.expect(64, 32, BLACK, "nothing drawn")?;
    match outcome_of(&outcome, "the frame")? {
        FrameOutcome::Failed(reason) => check(reason.contains("does not import"), || reason.clone())?,
        other => return Err(format!("{other:?}, expected Failed")),
    }
    // Shown again, the buffer is not imported again.
    fixture.frame()?;
    let after = stats();
    check(after.import_failures == before.import_failures + 1, || {
        format!("{before:?} -> {after:?}")
    })?;
    check(fixture.texture(ExternalTexture::take_fallback)?.is_some(), || {
        "no fall-back".to_owned()
    })?;
    check(
        fixture.texture(|texture| texture.fallback().clone())? == Fallback::Taken,
        || "not taken".to_owned(),
    )?;
    Ok(Verdict::Passed)
}

/// A fence that has signaled: a pipe with a byte in it.
fn signaled_fence() -> Result<OwnedFd, String> {
    let (reader, mut writer) = std::io::pipe().map_err(|error| format!("a pipe: {error}"))?;
    std::io::Write::write_all(&mut writer, b"1").map_err(|error| format!("signaling: {error}"))?;
    Ok(OwnedFd::from(reader))
}

fn dmabuf_rendered_by_gl_with_the_devices_modifiers(gpu: &Gpu) -> Outcome {
    let Some(node) = &gpu.node else {
        return Ok(Verdict::NotApplicable("a hardware GPU's own modifiers".to_owned()));
    };
    let mut fixture = Fixture::new(Layout::Fill, black())?;
    let caps = caps_now()?;
    let format = caps
        .formats
        .iter()
        .find(|format| format.fourcc == XR24)
        .ok_or_else(|| "no XR24".to_owned())?;
    let producer = match GlProducer::new(node) {
        Ok(producer) => producer,
        Err(reason) => return Ok(Verdict::Skipped(format!("no GL producer: {reason}"))),
    };
    let mut tested = Vec::new();
    for modifier in format.modifiers.iter().filter(|modifier| modifier.modifier != 0) {
        // A modifier GBM does not allocate for rendering here is not this test's to judge.
        let Ok(buffer) = producer.allocate(64, 32, XR24, &[modifier.modifier]) else {
            continue;
        };
        check(buffer.buffer.modifier == modifier.modifier, || {
            format!(
                "asked for {:#018x}, GBM allocated {:#018x}",
                modifier.modifier, buffer.buffer.modifier
            )
        })?;
        let label = format!(
            "modifier {:#018x} ({} planes)",
            modifier.modifier,
            buffer.buffer.planes.len()
        );
        frames_of_one_gl_buffer(&mut fixture, &producer, &buffer).map_err(|error| format!("{label}: {error}"))?;
        tested.push(label);
    }
    if tested.is_empty() {
        return Ok(Verdict::Skipped(
            "GBM allocated no buffer with a non-linear modifier".to_owned(),
        ));
    }
    println!("    rendered by GL and imported: {}", tested.join(", "));
    Ok(Verdict::Passed)
}

/// GL clears one buffer red and the renderer shows it; GL clears the same buffer blue (a fast clear may only rewrite
/// the clear colour plane) and the renderer shows that; GL clears it green without handing it over, and the renderer
/// still shows blue from its own copy, without copying again.
fn frames_of_one_gl_buffer(fixture: &mut Fixture, producer: &GlProducer, buffer: &GlBuffer) -> Result<(), String> {
    let before = stats();
    for (rgba, expected, what) in [
        ([1.0, 0.0, 0.0, 1.0], RED, "the first clear"),
        ([0.0, 0.0, 1.0, 1.0], BLUE, "the second clear"),
    ] {
        producer.clear(buffer, rgba);
        let (done, outcome) = listener();
        fixture.texture(|texture| texture.push_dmabuf(Arc::clone(&buffer.buffer), Acquire::Implicit, done))?;
        let shot = fixture.frame()?;
        presented(&outcome, what)?;
        shot.expect(4, 4, expected, what)?;
        shot.expect(124, 60, expected, what)?;
    }
    producer.clear(buffer, [0.0, 1.0, 0.0, 1.0]);
    fixture
        .frame()?
        .expect(64, 32, BLUE, "a redraw after the producer rendered again")?;
    let after = stats();
    check(
        after.imports == before.imports + 1 && after.copies == before.copies + 2,
        || format!("{before:?} -> {after:?}: one import, one copy per frame handed over"),
    )
}

// MARK: - SHM and layout

fn rect(x: u32, y: u32, width: u32, height: u32) -> PixelRect {
    PixelRect { x, y, width, height }
}

fn solid(rect: PixelRect, bgra: [u8; 4]) -> Vec<u8> {
    bgra.repeat(rect.width as usize * rect.height as usize)
}

fn shm_uploads_its_damage(_: &Gpu) -> Outcome {
    let mut fixture = Fixture::new(Layout::Fill, black())?;
    let whole = rect(0, 0, 64, 32);
    let red = solid(whole, [0, 0, 255, 255]);
    let before = stats();
    let (done, first) = listener();
    fixture.texture(|texture| {
        texture.push_shm(
            64,
            32,
            [ShmRegion {
                rect: whole,
                pixels: &red,
            }],
            done,
        );
    })?;
    fixture.frame()?.expect(64, 32, RED, "the whole frame")?;
    presented(&first, "the first SHM frame")?;
    let middle = stats();
    check(
        middle.pixel_bytes_uploaded == before.pixel_bytes_uploaded + 64 * 32 * 4,
        || format!("{before:?} -> {middle:?}: the whole frame once"),
    )?;
    let corner = rect(0, 0, 16, 16);
    let blue = solid(corner, [255, 0, 0, 255]);
    let (done, second) = listener();
    fixture.texture(|texture| {
        texture.push_shm(
            64,
            32,
            [ShmRegion {
                rect: corner,
                pixels: &blue,
            }],
            done,
        );
    })?;
    let shot = fixture.frame()?;
    shot.expect(8, 8, BLUE, "the damaged corner")?;
    shot.expect(100, 50, RED, "the rest")?;
    presented(&second, "the second SHM frame")?;
    let after = stats();
    check(
        after.pixel_bytes_uploaded == middle.pixel_bytes_uploaded + 16 * 16 * 4,
        || format!("{middle:?} -> {after:?}: only the damaged corner"),
    )?;
    check(after.imports == before.imports, || {
        "SHM frames imported a dmabuf".to_owned()
    })?;
    Ok(Verdict::Passed)
}

fn clipped_and_drawn_under_overlays(_: &Gpu) -> Outcome {
    let mut fixture = Fixture::new(Layout::Clipped, black())?;
    let whole = rect(0, 0, 64, 32);
    let red = solid(whole, [0, 0, 255, 255]);
    let (done, outcome) = listener();
    fixture.texture(|texture| {
        texture.push_shm(
            64,
            32,
            [ShmRegion {
                rect: whole,
                pixels: &red,
            }],
            done,
        );
    })?;
    let shot = fixture.frame()?;
    presented(&outcome, "the frame")?;
    shot.expect(8, 8, GREEN, "the overlay drawn after the texture")?;
    shot.expect(40, 40, RED, "the texture inside its box")?;
    shot.expect(100, 40, BLACK, "the texture clipped by its box")?;
    Ok(Verdict::Passed)
}

// MARK: - coverage masks (GPUI patch 0003, the terminal's sprites)

/// A 10x4 device-pixel mask: full coverage but for a hole at (3, 1).
const MASK: (i32, i32) = (10, 4);

fn mask_bytes() -> Vec<u8> {
    let mut bytes = vec![u8::MAX; 40];
    bytes[10 + 3] = 0;
    bytes
}

/// Two masks side by side, ten device pixels apart, as two sprite cells meet.
struct Masks;

impl Render for Masks {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let paint = |_: Bounds<Pixels>, (): (), window: &mut Window, _: &mut App| {
            let scale = window.scale_factor();
            for left in [0.0, 10.0] {
                let params = RenderAlphaMaskParams {
                    owner: "ngterm.test",
                    id: 1,
                };
                let mask = size(DevicePixels(MASK.0), DevicePixels(MASK.1));
                let origin = point(px(left / scale), px(0.));
                if let Err(error) = window.paint_alpha_mask(origin, params, red(), || Some((mask, mask_bytes()))) {
                    eprintln!("the mask was not painted: {error:#}");
                }
            }
        };
        div()
            .size_full()
            .bg(black())
            .child(canvas(|_, _, _| {}, paint).size_full())
    }
}

fn red() -> Hsla {
    gpui_kit::rgb(0xff_0000).into()
}

/// The masks land on whole device pixels, one texel each, tinted, the second reusing the first one's tile: two cells
/// meet without a gap or an overlap, and a hole stays a hole.
fn alpha_masks_cover_whole_device_pixels(_: &Gpu) -> Outcome {
    let mut cx = HeadlessAppContext::with_platform(
        Arc::new(NoopTextSystem),
        Arc::new(()),
        gpui_kit::platform::current_headless_renderer,
    );
    let window = cx
        .update(|cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(gpui_kit::Bounds {
                        origin: point(px(0.), px(0.)),
                        size: size(px(64.), px(32.)),
                    })),
                    focus: false,
                    show: false,
                    ..Default::default()
                },
                |_, cx| cx.new(|_| Masks),
            )
        })
        .map_err(|error| format!("the window did not open: {error:#}"))?;
    let handle = window.into();
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .map_err(|error| format!("rendering: {error:#}"))?;
    let image = cx
        .capture_screenshot(handle)
        .map_err(|error| format!("reading back: {error:#}"))?;
    let shot = Shot {
        width: image.width(),
        raw: image.into_raw(),
    };
    for y in 0..5 {
        for x in 0..21 {
            let hole = y == 1 && (x == 3 || x == 13);
            let expected = if x < 20 && y < 4 && !hole { RED } else { BLACK };
            shot.expect(x, y, expected, "the masks' coverage")?;
        }
    }
    Ok(Verdict::Passed)
}

// MARK: - Kitty placements (the terminal's images)

/// An 8x8 image, its left half red and its right half blue.
fn halves() -> Arc<RenderImage> {
    let mut bgra = Vec::with_capacity(8 * 8 * 4);
    for _ in 0..8 {
        for x in 0..8 {
            // BGRA
            bgra.extend_from_slice(if x < 4 { &[0, 0, 255, 255] } else { &[255, 0, 0, 255] });
        }
    }
    let buffer = image::RgbaImage::from_raw(8, 8, bgra).unwrap_or_default();
    Arc::new(RenderImage::new(vec![image::Frame::new(buffer)]))
}

/// The blue half of [`halves`] placed over device pixels 8..24 both ways, as the terminal draws a placement's source
/// rectangle.
struct Placement(Arc<RenderImage>);

impl Render for Placement {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let image = Arc::clone(&self.0);
        let paint = move |_: Bounds<Pixels>, (): (), window: &mut Window, _: &mut App| {
            let scale = window.scale_factor();
            let dest = Bounds::new(
                point(px(8. / scale), px(8. / scale)),
                size(px(16. / scale), px(16. / scale)),
            );
            let source = SourceRect {
                x: 4,
                y: 0,
                width: 4,
                height: 8,
            };
            let whole = kitty::image_bounds(dest, source, (8, 8));
            if let Err(error) = window.paint_image(dest, whole, Corners::default(), Arc::clone(&image), 0, false) {
                eprintln!("the image was not painted: {error:#}");
            }
        };
        div()
            .size_full()
            .bg(black())
            .child(canvas(|_, _, _| {}, paint).size_full())
    }
}

/// The source rectangle fills the placement and nothing of the image outside it shows (the whole image is painted
/// scaled, clipped to the placement).
fn image_source_rects_fill_their_placement(_: &Gpu) -> Outcome {
    let mut cx = HeadlessAppContext::with_platform(
        Arc::new(NoopTextSystem),
        Arc::new(()),
        gpui_kit::platform::current_headless_renderer,
    );
    let image = halves();
    let window = cx
        .update(|cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(gpui_kit::Bounds {
                        origin: point(px(0.), px(0.)),
                        size: size(px(32.), px(32.)),
                    })),
                    focus: false,
                    show: false,
                    ..Default::default()
                },
                |_, cx| cx.new(|_| Placement(image)),
            )
        })
        .map_err(|error| format!("the window did not open: {error:#}"))?;
    let handle = window.into();
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .map_err(|error| format!("rendering: {error:#}"))?;
    let shot = cx
        .capture_screenshot(handle)
        .map_err(|error| format!("reading back: {error:#}"))?;
    let shot = Shot {
        width: shot.width(),
        raw: shot.into_raw(),
    };
    // inside, away from the edges where linear sampling blends in the neighbouring texel
    for y in [10, 16, 21] {
        for x in [10, 16, 21] {
            shot.expect(x, y, BLUE, "the source rectangle")?;
        }
    }
    // the red half would land left of the placement: clipped
    for (x, y) in [(2, 16), (6, 16), (16, 4), (28, 16), (16, 28)] {
        shot.expect(x, y, BLACK, "outside the placement")?;
    }
    Ok(Verdict::Passed)
}
