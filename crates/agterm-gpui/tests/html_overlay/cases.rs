//! The cases, run in one child process per GPU against one engine (`WebKit` starts once per process).
//!
//! A [`Fixture`] is a headless GPUI app with one window whose root shows a page's element; every turn runs GPUI's
//! tasks (the engine's events arrive through the overlay's pump), renders a frame and reads it back.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agterm_core::html::{HtmlOverlay, HtmlPageFlags, HtmlSource};
use agterm_gpui::external_texture::{FrameSource, negotiate};
use agterm_gpui::html_overlay::{self, FrameStats, PageView};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AppContext as _, Context, Entity, HeadlessAppContext, IntoElement, NoopTextSystem, ParentElement as _, Render,
    Styled as _, Window, WindowBounds, WindowHandle, WindowOptions, div, point, px, size,
};

/// What a case runs with.
pub struct Gpu {
    pub name: String,
}

pub type Case = fn(&mut Fixture, &Gpu) -> Result<(), String>;

pub const CASES: [(&str, Case); 4] = [
    ("the_engine_renders_on_the_renderers_gpu_in_its_mode", mode),
    ("a_page_loads_and_its_frames_reach_the_screen", loads),
    ("a_page_moved_to_another_window_draws_there", moved),
    (
        "an_animation_keeps_its_frame_rate_on_dmabufs_without_falling_back",
        animation,
    ),
];

/// Runs the cases `filter` selects on `gpu`; the child's exit code.
pub fn run(gpu: &str, filter: Option<&str>) -> i32 {
    let gpu = Gpu { name: gpu.to_owned() };
    let mut fixture = match Fixture::new() {
        Ok(fixture) => fixture,
        Err(message) => {
            println!("  the fixture did not start on {}: {message}", gpu.name);
            return 1;
        }
    };
    let mut failed = 0;
    for (name, case) in CASES {
        if filter.is_some_and(|filter| !name.contains(filter)) {
            continue;
        }
        let started = Instant::now();
        match case(&mut fixture, &gpu) {
            Ok(()) => println!("  {name} ... ok ({:.1?})", started.elapsed()),
            Err(message) => {
                println!("  {name} ... FAILED on {}: {message}", gpu.name);
                failed += 1;
            }
        }
    }
    html_overlay::shutdown();
    i32::from(failed > 0)
}

// MARK: - the fixture

const WAIT: Duration = Duration::from_secs(30);
const RED: [u8; 4] = [0xc0, 0x30, 0x30, 0xff];

/// The window's content: the page shown, at the window's size.
struct Stage {
    element: Option<Entity<PageView>>,
}

impl Render for Stage {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().bg(gpui_kit::black()).children(self.element.clone())
    }
}

pub struct Fixture {
    cx: HeadlessAppContext,
    window: WindowHandle<Stage>,
    scratch: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Result<Self, String> {
        let mut cx = HeadlessAppContext::with_platform(
            Arc::new(NoopTextSystem),
            Arc::new(()),
            gpui_kit::platform::current_headless_renderer,
        );
        // the engine's events wake the pump from its own thread
        cx.allow_parking();
        cx.update(|cx| {
            gpui_kit::init(cx);
            html_overlay::init(cx);
        });
        let window = cx
            .update(|cx| {
                cx.open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(gpui_kit::Bounds {
                            origin: point(px(0.), px(0.)),
                            size: size(px(320.), px(240.)),
                        })),
                        focus: false,
                        show: false,
                        ..Default::default()
                    },
                    |_, cx| cx.new(|_| Stage { element: None }),
                )
            })
            .map_err(|error| format!("the window did not open: {error:#}"))?;
        let scratch = tempfile::tempdir().map_err(|err| err.to_string())?;
        let mut fixture = Self { cx, window, scratch };
        // the first frame opens GPUI's device, which the engine renders on
        fixture.frame()?;
        Ok(fixture)
    }

    /// Mounts a chromeless page with its scripts on showing `html` (a file page without a grant: its text is loaded) in
    /// a window of its own `width` x `height`, and shows it.
    fn show(&mut self, html: &str, width: f32, height: f32) -> Result<Entity<PageView>, String> {
        let overlay = self.page(html)?;
        self.mount(&overlay, width, height)
    }

    /// A chromeless file page with its scripts on, showing `html`.
    fn page(&self, html: &str) -> Result<HtmlOverlay, String> {
        let path: PathBuf = self.scratch.path().join(format!("page-{}.html", uuid::Uuid::new_v4()));
        std::fs::write(&path, html).map_err(|err| err.to_string())?;
        Ok(HtmlOverlay::new(
            HtmlSource::File {
                path: path.to_string_lossy().into_owned(),
                grant_root: None,
            },
            HtmlPageFlags {
                chromeless: true,
                // the page's own scripts run (`--js`): the animation is one
                javascript: true,
                ..HtmlPageFlags::default()
            },
        ))
    }

    /// Mounts `overlay` in a new window `width` x `height`, made the fixture's, and shows it there.
    fn mount(&mut self, overlay: &HtmlOverlay, width: f32, height: f32) -> Result<Entity<PageView>, String> {
        // a window of its own at the page's size
        let window = self
            .cx
            .open_window(size(px(width), px(height)), |_, cx| cx.new(|_| Stage { element: None }))
            .map_err(|error| format!("the window did not open: {error:#}"))?;
        self.window = window;
        let element = self
            .cx
            .update(|cx| {
                window.update(cx, |stage, window, cx| {
                    let element = html_overlay::mount(overlay, None, window, cx);
                    stage.element.clone_from(&element);
                    cx.notify();
                    element
                })
            })
            .map_err(|error| format!("the window is gone: {error:#}"))?
            .ok_or("the build mounts no page")?;
        self.cx.update(|cx| html_overlay::apply(overlay, true, cx));
        Ok(element)
    }

    /// Runs GPUI's tasks and renders a frame.
    fn render(&mut self) -> Result<(), String> {
        self.cx.run_until_parked();
        let handle = self.window.into();
        self.cx
            .update_window(handle, |_, window, cx| window.render_frame(cx))
            .map_err(|error| format!("rendering: {error:#}"))
    }

    /// Runs GPUI's tasks and renders a frame, read back as RGBA rows.
    fn frame(&mut self) -> Result<(u32, Vec<u8>), String> {
        self.render()?;
        let handle = self.window.into();
        let image = self
            .cx
            .capture_screenshot(handle)
            .map_err(|error| format!("reading back: {error:#}"))?;
        Ok((image.width(), image.into_raw()))
    }

    fn stats(&mut self, element: &Entity<PageView>) -> FrameStats {
        self.cx.update(|cx| element.read(cx).frame_stats())
    }

    /// Turns until `done` holds over the stats and the frame read back, or fails naming `what`.
    fn until(
        &mut self,
        what: &str,
        element: &Entity<PageView>,
        mut done: impl FnMut(FrameStats, u32, &[u8]) -> bool,
    ) -> Result<(), String> {
        let deadline = Instant::now() + WAIT;
        loop {
            let (width, pixels) = self.frame()?;
            let stats = self.stats(element);
            if done(stats, width, &pixels) {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(format!("timed out waiting for {what} (frames: {stats:?})"));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

/// The RGBA pixel at (`x`, `y`) of a frame `width` wide.
fn pixel(pixels: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let at = ((y * width + x) * 4) as usize;
    pixels
        .get(at..at + 4)
        .map_or([0; 4], |rgba| [rgba[0], rgba[1], rgba[2], rgba[3]])
}

fn close(actual: [u8; 4], expected: [u8; 4]) -> bool {
    actual.iter().zip(expected).all(|(&a, e)| a.abs_diff(e) <= 6)
}

/// Whether the engine renders dmabufs here: GPUI's device names a render node and imports a drawn format.
fn dmabuf_mode() -> bool {
    matches!(
        negotiate(gpui_kit::dmabuf_import_caps().as_deref()),
        FrameSource::DmaBuf { .. }
    )
}

// MARK: - cases

/// The engine renders where the renderer imports: dmabufs on a Mesa GPU (its render node), shared memory on lavapipe,
/// which names none.
fn mode(_: &mut Fixture, gpu: &Gpu) -> Result<(), String> {
    let dmabuf = dmabuf_mode();
    match (gpu.name.as_str(), dmabuf) {
        ("mesa", true) | ("lavapipe", false) => Ok(()),
        (name, dmabuf) => Err(format!("{name} negotiated dmabuf={dmabuf}")),
    }
}

/// A page's frames come back through the element: its color on screen, in the GPU's mode, without a fall-back.
fn loads(fixture: &mut Fixture, _: &Gpu) -> Result<(), String> {
    let element = fixture.show(
        "<html><body style='margin:0;background:#c03030'></body></html>",
        320.,
        240.,
    )?;
    fixture.until("the page's color on screen", &element, |_, width, pixels| {
        close(pixel(pixels, width, width / 2, 100), RED)
    })?;
    let stats = fixture.stats(&element);
    if dmabuf_mode() {
        check(stats.dmabuf > 0 && stats.shm == 0 && !stats.fell_back, || {
            format!("dmabufs import here, yet the page drew {stats:?}")
        })
    } else {
        check(stats.shm > 0 && stats.dmabuf == 0, || {
            format!("no render node here, yet the page drew {stats:?}")
        })
    }
}

/// A page whose session moved to another window (mounted again from there) gets an element of that window, which the
/// engine draws anew.
fn moved(fixture: &mut Fixture, _: &Gpu) -> Result<(), String> {
    let overlay = fixture.page("<html><body style='margin:0;background:#c03030'></body></html>")?;
    let first = fixture.mount(&overlay, 320., 240.)?;
    fixture.until("the page in its first window", &first, |_, width, pixels| {
        close(pixel(pixels, width, width / 2, 100), RED)
    })?;
    let second = fixture.mount(&overlay, 200., 160.)?;
    check(first != second, || {
        "the second window reused the first window's element".to_owned()
    })?;
    fixture.until("the page in its second window", &second, |stats, width, pixels| {
        stats.dmabuf + stats.shm > 0 && close(pixel(pixels, width, width / 2, 80), RED)
    })
}

/// A canvas animation at 1080p keeps its frame rate on dmabufs (the path video takes) and never falls back to SHM;
/// on lavapipe it keeps running in SHM at a small size.
fn animation(fixture: &mut Fixture, _: &Gpu) -> Result<(), String> {
    let dmabuf = dmabuf_mode();
    let (width, height) = if dmabuf { (1920., 1080.) } else { (480., 270.) };
    let element = fixture.show(
        "<html><body style='margin:0'><canvas id=c></canvas><script>\
         const c = document.getElementById('c'); c.width = innerWidth; c.height = innerHeight;\
         const g = c.getContext('2d'); let n = 0;\
         function draw() { n++; g.fillStyle = n % 2 ? '#c03030' : '#3030c0'; g.fillRect(0, 0, c.width, c.height);\
         g.fillStyle = '#fff'; g.fillText(String(n), 10, 20); requestAnimationFrame(draw); }\
         requestAnimationFrame(draw);</script></body></html>",
        width,
        height,
    )?;
    fixture.until("the animation's first frames", &element, |stats, _, _| {
        stats.dmabuf + stats.shm > 3
    })?;
    let before = fixture.stats(&element);
    let started = Instant::now();
    let period = Duration::from_secs(2);
    // every frame rendered and read back: the headless renderer submits (and so completes frames) only for a read-back,
    // the headless counterpart of a window's present; at 1080p the read-back itself costs a good part of each frame
    while started.elapsed() < period {
        fixture.frame()?;
        std::thread::sleep(Duration::from_millis(1));
    }
    let after = fixture.stats(&element);
    let frames = (after.dmabuf + after.shm) - (before.dmabuf + before.shm);
    #[expect(clippy::cast_precision_loss, reason = "a frame count")]
    let fps = frames as f64 / started.elapsed().as_secs_f64();
    println!("    {width}x{height}: {fps:.1} frames/s, {after:?}");
    if dmabuf {
        check(!after.fell_back && after.shm == 0, || {
            format!("SHM was chosen while dmabufs import: {after:?}")
        })?;
        // ~40 here (Intel UHD, a debug build, a 1080p read-back per frame); SHM manages 2.5 at 1080p (Task 2)
        check(fps >= 24.0, || format!("{fps:.1} frames/s at 1080p on dmabufs"))
    } else {
        check(after.dmabuf == 0 && fps >= 2.0, || {
            format!("{fps:.1} frames/s in SHM, {after:?}")
        })
    }
}

fn check(condition: bool, message: impl FnOnce() -> String) -> Result<(), String> {
    if condition { Ok(()) } else { Err(message()) }
}
