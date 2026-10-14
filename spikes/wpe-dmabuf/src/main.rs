//! Spike: WPE WebKit offscreen frames drawn by GPUI (docs/spikes/20261014-wpe-dmabuf.md).
//!
//! `wpe-dmabuf-spike --mode dmabuf|shm-texture|shm-image --url URL [--device /dev/dri/renderD129]
//!   [--size 1920x1080] [--bench none|video|youtube|scroll|latency] [--duration 30] [--out FILE]`
//!
//! Run it through `scripts/run.sh`, which supplies the WPE prefix (bwrap overlay) and strips AGTERM_*/ZMX_*.

mod wpe;

use gpui::{
    App, AppContext as _, Bounds, Context, Corners, ExternalSurface, FocusHandle, IntoElement, KeyDownEvent,
    KeyUpEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, Render,
    RenderImage, ScrollDelta, ScrollWheelEvent, Styled as _, Window, WindowBounds, WindowOptions, canvas, div,
    prelude::*, px, size,
};
use std::cell::Cell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    DmaBuf,
    ShmTexture,
    ShmImage,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Bench {
    None,
    Video,
    Youtube,
    Scroll,
    Latency,
}

#[derive(Clone, Debug)]
struct Args {
    mode: Mode,
    url: String,
    device: Option<String>,
    width: u32,
    height: u32,
    bench: Bench,
    duration: u64,
    out: Option<String>,
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut parsed = Args {
        mode: Mode::DmaBuf,
        url: "about:blank".into(),
        device: None,
        width: 1920,
        height: 1080,
        bench: Bench::None,
        duration: 30,
        out: None,
    };
    let mut args = args.peekable();
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--mode" => {
                parsed.mode = match value()?.as_str() {
                    "dmabuf" => Mode::DmaBuf,
                    "shm-texture" => Mode::ShmTexture,
                    "shm-image" => Mode::ShmImage,
                    other => return Err(format!("unknown mode {other}")),
                }
            }
            "--url" => parsed.url = value()?,
            "--device" => parsed.device = Some(value()?),
            "--size" => {
                let v = value()?;
                let (w, h) = v.split_once('x').ok_or("--size WxH")?;
                parsed.width = w.parse().map_err(|_| "bad width")?;
                parsed.height = h.parse().map_err(|_| "bad height")?;
            }
            "--bench" => {
                parsed.bench = match value()?.as_str() {
                    "none" => Bench::None,
                    "video" => Bench::Video,
                    "youtube" => Bench::Youtube,
                    "scroll" => Bench::Scroll,
                    "latency" => Bench::Latency,
                    other => return Err(format!("unknown bench {other}")),
                }
            }
            "--duration" => parsed.duration = value()?.parse().map_err(|_| "bad duration")?,
            "--out" => parsed.out = Some(value()?),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(parsed)
}

struct WebView {
    host: Arc<wpe::Shared>,
    mode: Mode,
    painted: Option<wpe::Frame>,
    pending_release: VecDeque<(usize, u64)>,
    image: Option<(u64, Arc<RenderImage>)>,
    stale_images: Vec<Arc<RenderImage>>,
    bounds: Rc<Cell<Bounds<Pixels>>>,
    wpe_size: (i32, i32),
    focus: FocusHandle,
    paints: Rc<std::cell::RefCell<Vec<Instant>>>,
}

impl WebView {
    fn local(&self, position: gpui::Point<Pixels>) -> (f64, f64) {
        let origin = self.bounds.get().origin;
        (f64::from(f32::from(position.x - origin.x)), f64::from(f32::from(position.y - origin.y)))
    }

    fn take_new_frame(&mut self) {
        let Some(frame) = self.host.latest.lock().unwrap().take() else { return };
        // The shim drops acknowledgements for buffers WebKit already destroyed.
        wpe::frame_done(frame.buffer());
        if let Some(old) = self.painted.replace(frame)
            && matches!(old, wpe::Frame::DmaBuf { .. })
        {
            // The previous buffer may still be sampled by the frame in flight: release it one frame later.
            self.pending_release.push_back((old.buffer(), old.generation()));
        }
        while self.pending_release.len() > 1 {
            let (buffer, _generation) = self.pending_release.pop_front().unwrap();
            wpe::release(buffer);
        }
    }

    fn image_for(&mut self, frame: &gpui::PixelFrame) -> Arc<RenderImage> {
        if let Some((serial, image)) = &self.image
            && *serial == frame.serial
        {
            return image.clone();
        }
        let mut bytes = frame.data.as_ref().clone();
        if frame.stride != frame.width * 4 {
            bytes = (0..frame.height as usize)
                .flat_map(|row| {
                    let start = row * frame.stride as usize;
                    frame.data[start..start + frame.width as usize * 4].iter().copied()
                })
                .collect();
        }
        // GPUI's RenderImage holds BGRA, which is what WPE's ARGB8888 SHM buffers carry.
        let buffer = image::RgbaImage::from_raw(frame.width, frame.height, bytes).expect("frame size");
        let image = Arc::new(RenderImage::new(vec![image::Frame::new(buffer)]));
        if let Some((_, old)) = self.image.replace((frame.serial, image.clone())) {
            self.stale_images.push(old);
        }
        image
    }
}

impl Render for WebView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.take_new_frame();
        for image in self.stale_images.drain(..) {
            let _ = window.drop_image(image);
        }
        let bounds = self.bounds.get();
        let wanted = (f32::from(bounds.size.width) as i32, f32::from(bounds.size.height) as i32);
        if wanted.0 > 0 && wanted.1 > 0 && wanted != self.wpe_size && wpe::ready() {
            self.wpe_size = wanted;
            wpe::resize(wanted.0, wanted.1);
        }
        enum Paint {
            Surface(ExternalSurface),
            Image(Arc<RenderImage>),
            Nothing,
        }
        let paint = match (self.painted.clone(), self.mode) {
            (Some(wpe::Frame::DmaBuf { frame, .. }), _) => Paint::Surface(ExternalSurface::DmaBuf(frame)),
            (Some(wpe::Frame::Pixels { frame, .. }), Mode::ShmImage) => Paint::Image(self.image_for(&frame)),
            (Some(wpe::Frame::Pixels { frame, .. }), _) => Paint::Surface(ExternalSurface::Pixels(frame)),
            (None, _) => Paint::Nothing,
        };
        let bounds_cell = self.bounds.clone();
        let paints = self.paints.clone();
        div()
            .size_full()
            .bg(gpui::rgb(0x202020))
            .track_focus(&self.focus)
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, _| {
                let (x, y) = this.local(e.position);
                wpe::pointer(0, x, y, 0, 0, wpe::modifiers(&e.modifiers));
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus, cx);
                    let (x, y) = this.local(e.position);
                    wpe::pointer(1, x, y, 1, e.click_count as i32, wpe::modifiers(&e.modifiers));
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, e: &MouseUpEvent, _, _| {
                    let (x, y) = this.local(e.position);
                    wpe::pointer(2, x, y, 1, e.click_count as i32, wpe::modifiers(&e.modifiers));
                }),
            )
            .on_scroll_wheel(cx.listener(|this, e: &ScrollWheelEvent, _, _| {
                let (x, y) = this.local(e.position);
                let (dx, dy, precise) = match e.delta {
                    ScrollDelta::Pixels(p) => (f64::from(f32::from(p.x)), f64::from(f32::from(p.y)), true),
                    ScrollDelta::Lines(l) => (f64::from(l.x), f64::from(l.y), false),
                };
                wpe::scroll(x, y, dx, dy, precise, wpe::modifiers(&e.modifiers));
            }))
            .on_key_down(cx.listener(|_, e: &KeyDownEvent, _, _| {
                let keyval = wpe::keyval(&e.keystroke.key);
                if keyval != 0 {
                    wpe::key(true, keyval, wpe::modifiers(&e.keystroke.modifiers));
                }
            }))
            .on_key_up(cx.listener(|_, e: &KeyUpEvent, _, _| {
                let keyval = wpe::keyval(&e.keystroke.key);
                if keyval != 0 {
                    wpe::key(false, keyval, wpe::modifiers(&e.keystroke.modifiers));
                }
            }))
            .child(
                canvas(
                    move |bounds, _, _| bounds,
                    move |bounds, _, window, _| {
                        bounds_cell.set(bounds);
                        match paint {
                            Paint::Surface(source) => {
                                window.paint_external_surface(bounds, source);
                                paints.borrow_mut().push(Instant::now());
                            }
                            Paint::Image(image) => {
                                let _ = window.paint_image(bounds, bounds, Corners::default(), image, 0, false);
                                paints.borrow_mut().push(Instant::now());
                            }
                            Paint::Nothing => {}
                        }
                    },
                )
                .size_full(),
            )
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn interval_stats(times: &[Instant]) -> serde_json::Value {
    let intervals: Vec<f64> = times.windows(2).map(|w| (w[1] - w[0]).as_secs_f64() * 1000.0).collect();
    let mut sorted = intervals.clone();
    sorted.sort_by(f64::total_cmp);
    let span = times.last().zip(times.first()).map_or(0.0, |(l, f)| (*l - *f).as_secs_f64());
    serde_json::json!({
        "count": times.len(),
        "fps": if span > 0.0 { (times.len() - 1) as f64 / span } else { 0.0 },
        "interval_ms_p50": percentile(&sorted, 0.5),
        "interval_ms_p95": percentile(&sorted, 0.95),
        "interval_ms_p99": percentile(&sorted, 0.99),
        "interval_ms_max": sorted.last().copied().unwrap_or(f64::NAN),
        "intervals_over_25ms": intervals.iter().filter(|i| **i > 25.0).count(),
    })
}

const VIDEO_PROBE: &str = r#"(() => {
  const v = document.querySelector('video');
  if (!v) return JSON.stringify({video: false});
  const q = v.getVideoPlaybackQuality ? v.getVideoPlaybackQuality() : {};
  const p = document.getElementById('movie_player');
  return JSON.stringify({video: true, t: v.currentTime, paused: v.paused, w: v.videoWidth, h: v.videoHeight,
    total: q.totalVideoFrames, dropped: q.droppedVideoFrames,
    quality: p && p.getPlaybackQuality ? p.getPlaybackQuality() : null,
    nerds: p && p.getStatsForNerds ? (({resolution, codecs, dropped_frames, buffer_health_seconds}) =>
      ({resolution, codecs, dropped_frames, buffer_health_seconds}))(p.getStatsForNerds()) : null,
    bench: window.__bench ? window.__bench() : null});
})()"#;

const YOUTUBE_START: &str = r#"(() => {
  const p = document.getElementById('movie_player');
  if (p && p.setPlaybackQualityRange) { p.setPlaybackQualityRange('hd1080', 'hd1080'); }
  const v = document.querySelector('video');
  if (v) { v.muted = true; v.play(); }
  for (const b of document.querySelectorAll('button')) {
    if (/accept all|reject all/i.test(b.textContent)) { b.click(); break; }
  }
  return JSON.stringify({player: !!p, video: !!v});
})()"#;

/// `next_js` bounded by 3 s: a page busy enough to starve evaluation must not hang the bench.
async fn next_js_within(events: &async_channel::Receiver<wpe::Event>, cx: &gpui::AsyncApp) -> Option<String> {
    let timeout = cx.background_executor().timer(Duration::from_secs(3));
    let result = std::pin::pin!(next_js(events));
    match futures::future::select(result, timeout).await {
        futures::future::Either::Left((text, _)) => text,
        futures::future::Either::Right(_) => {
            log::warn!("javascript evaluation timed out");
            None
        }
    }
}

async fn next_js(events: &async_channel::Receiver<wpe::Event>) -> Option<String> {
    while let Ok(event) = events.recv().await {
        match event {
            wpe::Event::JsResult(text) => return Some(text),
            wpe::Event::Error(text) => log::warn!("wpe: {text}"),
            wpe::Event::LoadFinished(uri) => log::info!("load finished: {uri}"),
        }
    }
    None
}

/// Runs the bench and returns its JSON part.
async fn run_bench(
    args: &Args,
    host: &Arc<wpe::Shared>,
    events: &async_channel::Receiver<wpe::Event>,
    cx: &mut gpui::AsyncApp,
) -> serde_json::Value {
    let timer = |d: Duration| cx.background_executor().timer(d);
    // Wait for the page.
    let started = Instant::now();
    loop {
        match events.recv().await {
            Ok(wpe::Event::LoadFinished(uri)) => {
                log::info!("load finished: {uri} after {:?}", started.elapsed());
                break;
            }
            Ok(wpe::Event::Error(text)) => log::warn!("wpe: {text}"),
            Ok(_) => {}
            Err(_) => return serde_json::json!({"error": "wpe gone"}),
        }
    }
    let mut samples = Vec::new();
    match args.bench {
        Bench::None => timer(Duration::from_secs(args.duration)).await,
        Bench::Video | Bench::Youtube => {
            if args.bench == Bench::Youtube {
                // Consent page (EU), then the watch page: retry the start script until a video plays.
                for _ in 0..10 {
                    timer(Duration::from_secs(2)).await;
                    wpe::eval(YOUTUBE_START);
                    log::info!("youtube start: {:?}", next_js_within(events, cx).await);
                    wpe::eval(VIDEO_PROBE);
                    let state = next_js_within(events, cx).await.unwrap_or_default();
                    log::info!("youtube state: {state}");
                    if state.contains("\"paused\":false") && !state.contains("\"t\":0,") {
                        break;
                    }
                }
            }
            for _ in 0..args.duration {
                timer(Duration::from_secs(1)).await;
                wpe::eval(VIDEO_PROBE);
                if let Some(text) = next_js_within(events, cx).await {
                    samples.push(serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text)));
                }
            }
        }
        Bench::Scroll => {
            timer(Duration::from_secs(2)).await;
            // Page-driven scroll first (the page scrolls itself per rAF when told to).
            wpe::eval("window.__autoscroll && window.__autoscroll(5000)");
            let _ = next_js_within(events, cx).await;
            let t0 = Instant::now();
            timer(Duration::from_millis(5500)).await;
            let page_frames = frames_between(host, t0, Instant::now());
            wpe::eval("JSON.stringify(window.__bench ? window.__bench() : null)");
            let page = next_js_within(events, cx).await;
            // Then input-driven: wheel events through the forwarding path at 120 Hz.
            wpe::eval("window.scrollTo(0, 0)");
            let _ = next_js_within(events, cx).await;
            timer(Duration::from_millis(500)).await;
            let t1 = Instant::now();
            while t1.elapsed() < Duration::from_secs(5) {
                wpe::scroll(400.0, 400.0, 0.0, -12.0, true, 0);
                timer(Duration::from_micros(8333)).await;
            }
            let wheel_frames = frames_between(host, t1, Instant::now());
            samples.push(serde_json::json!({
                "page_driven_frames": interval_stats(&page_frames),
                "page_report": page.and_then(|p| serde_json::from_str::<serde_json::Value>(&p).ok()),
                "wheel_driven_frames": interval_stats(&wheel_frames),
            }));
        }
        Bench::Latency => {
            timer(Duration::from_secs(2)).await;
            let mut key = Vec::new();
            let mut click = Vec::new();
            let mut pixels = Vec::new();
            for i in 0..40 {
                let sent = Instant::now();
                if i % 2 == 0 {
                    wpe::key(true, 0x61, 0);
                    wpe::key(false, 0x61, 0);
                } else {
                    wpe::pointer(1, 200.0, 200.0, 1, 1, 0);
                    wpe::pointer(2, 200.0, 200.0, 1, 1, 0);
                }
                timer(Duration::from_millis(300)).await;
                let pixel = gpui_wgpu::external_surface::LAST_CENTER_PIXEL.load(Ordering::Relaxed);
                if pixel != u64::MAX {
                    pixels.push(format!("{pixel:08x}"));
                }
                if let Some(first) = frames_between(host, sent, Instant::now()).first() {
                    let ms = (*first - sent).as_secs_f64() * 1000.0;
                    if i % 2 == 0 { key.push(ms) } else { click.push(ms) }
                }
            }
            let summarize = |mut v: Vec<f64>| {
                v.sort_by(f64::total_cmp);
                serde_json::json!({"n": v.len(), "p50_ms": percentile(&v, 0.5), "p95_ms": percentile(&v, 0.95),
                    "max_ms": v.last().copied().unwrap_or(f64::NAN)})
            };
            samples.push(serde_json::json!({"key_to_frame": summarize(key), "click_to_frame": summarize(click),
                "center_pixels_bgra_le": pixels}));
        }
    }
    serde_json::Value::Array(samples)
}

fn frames_between(host: &wpe::Shared, from: Instant, to: Instant) -> Vec<Instant> {
    host.frame_times.lock().unwrap().iter().copied().filter(|t| *t >= from && *t <= to).collect()
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };
    log::info!("{args:?}");
    let device = if args.mode == Mode::DmaBuf { args.device.clone() } else { None };
    if args.mode != Mode::DmaBuf {
        // Read by the shim (no DRM device, no dmabuf formats) and by WebKit (Skia CPU painting).
        unsafe {
            std::env::set_var("SPIKE_FORCE_SHM", "1");
            std::env::set_var("WEBKIT_SKIA_ENABLE_CPU_RENDERING", "1");
        }
    }
    let host = wpe::start(device, args.url.clone(), args.width as i32, args.height as i32);

    gpui_platform::application().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(args.width as f32), px(args.height as f32)), cx);
        let shared = host.shared.clone();
        let paints = Rc::new(std::cell::RefCell::new(Vec::new()));
        let view_paints = paints.clone();
        let mode = args.mode;
        let wake = host.wake.clone();
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    app_id: Some("io.github.agterm-rs.wpe-dmabuf-spike".into()),
                    ..Default::default()
                },
                move |window, cx| {
                    let view = cx.new(|cx| WebView {
                        host: shared,
                        mode,
                        painted: None,
                        pending_release: VecDeque::new(),
                        image: None,
                        stale_images: Vec::new(),
                        bounds: Rc::new(Cell::new(Bounds::default())),
                        wpe_size: (args.width as i32, args.height as i32),
                        focus: cx.focus_handle(),
                        paints: view_paints,
                    });
                    let focus = view.read(cx).focus.clone();
                    window.focus(&focus, cx);
                    view.update(cx, |_, cx| {
                        cx.spawn(async move |this, cx| {
                            while wake.recv().await.is_ok() {
                                if this.update(cx, |_, cx| cx.notify()).is_err() {
                                    break;
                                }
                            }
                        })
                        .detach();
                    });
                    view
                },
            )
            .expect("open window");
        let _ = window;
        cx.activate(true);

        let shared = host.shared.clone();
        let events = host.events.clone();
        let bench_args = args.clone();
        cx.spawn(async move |cx| {
            let t0 = Instant::now();
            let bench = run_bench(&bench_args, &shared, &events, cx).await;
            let stats = &shared.stats;
            let elapsed = t0.elapsed().as_secs_f64();
            let wpe_frames = shared.frame_times.lock().unwrap().clone();
            let gpui_paints = paints.borrow().clone();
            let size = stats.last_size.load(Ordering::Relaxed);
            let frame_size = format!("{}x{}", size >> 32, size & 0xffff_ffff);
            let result = serde_json::json!({
                "args": format!("{bench_args:?}"),
                "elapsed_s": elapsed,
                "bench": bench,
                "wpe_frames": interval_stats(&wpe_frames),
                "gpui_paints": interval_stats(&gpui_paints),
                "frames": stats.frames.load(Ordering::Relaxed),
                "frame_size": frame_size,
                "dmabuf_frames": stats.dmabuf_frames.load(Ordering::Relaxed),
                "shm_frames": stats.shm_frames.load(Ordering::Relaxed),
                "damage_ratio": stats.damage_px.load(Ordering::Relaxed) as f64
                    / stats.full_px.load(Ordering::Relaxed).max(1) as f64,
                "fence_waits": stats.fence_waits.load(Ordering::Relaxed),
                "fence_wait_us_total": stats.fence_wait_us.load(Ordering::Relaxed),
                "shm_copy_us_total": stats.shm_copy_us.load(Ordering::Relaxed),
                "buffers_changed": stats.buffers_changed.load(Ordering::Relaxed),
                "dmabuf_imports": gpui_wgpu::external_surface::DMABUF_IMPORTS.load(Ordering::Relaxed),
                "dmabuf_import_failures": gpui_wgpu::external_surface::DMABUF_IMPORT_FAILURES.load(Ordering::Relaxed),
                "pixel_upload_bytes": gpui_wgpu::external_surface::PIXEL_UPLOAD_BYTES.load(Ordering::Relaxed),
                "external_draws": gpui_wgpu::external_surface::EXTERNAL_DRAWS.load(Ordering::Relaxed),
            });
            let text = serde_json::to_string_pretty(&result).unwrap();
            println!("{text}");
            if let Some(out) = &bench_args.out {
                std::fs::write(out, &text).expect("write result");
            }
            if bench_args.bench != Bench::None || bench_args.duration > 0 {
                wpe::quit();
                cx.background_executor().timer(Duration::from_millis(300)).await;
                let _ = cx.update(|cx| cx.quit());
            }
        })
        .detach();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Result<Args, String> {
        parse_args(list.iter().map(|s| (*s).to_owned()))
    }

    #[test]
    fn parses_modes_and_sizes() {
        let a = args(&["--mode", "shm-image", "--size", "1280x720", "--bench", "scroll"]).unwrap();
        assert_eq!(a.mode, Mode::ShmImage);
        assert_eq!((a.width, a.height), (1280, 720));
        assert_eq!(a.bench, Bench::Scroll);
        assert!(args(&["--mode", "gl"]).is_err());
        assert!(args(&["--size", "12"]).is_err());
    }

    #[test]
    fn interval_stats_counts_fps() {
        let t0 = Instant::now();
        let times: Vec<Instant> = (0..61).map(|i| t0 + Duration::from_micros(16_667 * i)).collect();
        let stats = interval_stats(&times);
        assert!((stats["fps"].as_f64().unwrap() - 60.0).abs() < 0.1);
        assert_eq!(stats["intervals_over_25ms"], 0);
    }
}
