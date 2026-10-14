//! The WPE side: FFI to `csrc/wpe_shim.c`, the host thread, and the hand-off of frames to the GPUI thread.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[repr(C)]
struct SpikeFrame {
    kind: u32,
    width: u32,
    height: u32,
    fourcc: u32,
    modifier: u64,
    n_planes: u32,
    fds: [i32; 4],
    offsets: [u32; 4],
    strides: [u32; 4],
    shm_data: *const u8,
    shm_len: usize,
    shm_stride: u32,
    shm_format: u32,
    damage: *const i32,
    n_damage: u32,
    buffer: *mut c_void,
    fence_wait_us: i64,
}

type FrameCb = extern "C" fn(*mut c_void, *const SpikeFrame);
type EventCb = extern "C" fn(*mut c_void, c_int, *const c_char);

#[repr(C)]
struct SpikeConfig {
    device: *const c_char,
    url: *const c_char,
    width: c_int,
    height: c_int,
    on_frame: FrameCb,
    on_event: EventCb,
    ud: *mut c_void,
}

unsafe extern "C" {
    fn spike_run(config: *const SpikeConfig) -> c_int;
    fn spike_ready() -> c_int;
    fn spike_frame_done(buffer: *mut c_void);
    fn spike_buffer_release(buffer: *mut c_void);
    fn spike_pointer(kind: c_int, x: f64, y: f64, button: c_int, press_count: c_int, modifiers: u32);
    fn spike_scroll(x: f64, y: f64, dx: f64, dy: f64, precise: c_int, modifiers: u32);
    fn spike_key(down: c_int, keyval: u32, modifiers: u32);
    fn spike_resize(width: c_int, height: c_int);
    fn spike_eval(script: *const c_char);
    fn spike_quit();
}

/// What the GPUI side draws.
#[derive(Clone)]
pub enum Frame {
    DmaBuf { frame: Arc<gpui::DmaBufFrame>, buffer: usize, generation: u64 },
    Pixels { frame: Arc<gpui::PixelFrame>, buffer: usize, generation: u64 },
}

impl Frame {
    pub fn buffer(&self) -> usize {
        match self {
            Frame::DmaBuf { buffer, .. } | Frame::Pixels { buffer, .. } => *buffer,
        }
    }

    /// The WebKit buffer set the frame belongs to; buffers of an older set are already destroyed.
    pub fn generation(&self) -> u64 {
        match self {
            Frame::DmaBuf { generation, .. } | Frame::Pixels { generation, .. } => *generation,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Event {
    LoadFinished(String),
    JsResult(String),
    Error(String),
}

#[derive(Default)]
pub struct Stats {
    pub frames: AtomicU64,
    pub dmabuf_frames: AtomicU64,
    pub shm_frames: AtomicU64,
    pub damage_px: AtomicU64,
    pub full_px: AtomicU64,
    pub fence_waits: AtomicU64,
    pub fence_wait_us: AtomicU64,
    pub shm_copy_us: AtomicU64,
    pub buffers_changed: AtomicU64,
    /// Width << 32 | height of the last frame.
    pub last_size: AtomicU64,
}

pub struct Shared {
    /// Bumped on every WebKit buffers-changed: earlier buffers must not be acknowledged any more.
    pub latest: Mutex<Option<Frame>>,
    pub frame_times: Mutex<Vec<Instant>>,
    pub stats: Stats,
    pub generation: AtomicU64,
    serial: AtomicU64,
    /// The SHM back buffer: damaged rects are copied in, the renderer uploads them.
    pixels: Mutex<Option<Arc<Vec<u8>>>>,
    wake: async_channel::Sender<()>,
    events: async_channel::Sender<Event>,
}

extern "C" fn on_frame(ud: *mut c_void, frame: *const SpikeFrame) {
    let shared = unsafe { &*(ud as *const Shared) };
    let frame = unsafe { &*frame };
    let stats = &shared.stats;
    stats.frames.fetch_add(1, Ordering::Relaxed);
    stats.last_size.store(u64::from(frame.width) << 32 | u64::from(frame.height), Ordering::Relaxed);
    if frame.fence_wait_us >= 0 {
        stats.fence_waits.fetch_add(1, Ordering::Relaxed);
        stats.fence_wait_us.fetch_add(frame.fence_wait_us as u64, Ordering::Relaxed);
    }
    let damage: Vec<[u32; 4]> = (0..frame.n_damage as usize)
        .map(|i| {
            let r = unsafe { std::slice::from_raw_parts(frame.damage.add(i * 4), 4) };
            [r[0].max(0) as u32, r[1].max(0) as u32, r[2].max(0) as u32, r[3].max(0) as u32]
        })
        .collect();
    stats.full_px.fetch_add(u64::from(frame.width) * u64::from(frame.height), Ordering::Relaxed);
    let damaged: u64 = if damage.is_empty() {
        u64::from(frame.width) * u64::from(frame.height)
    } else {
        damage.iter().map(|r| u64::from(r[2]) * u64::from(r[3])).sum()
    };
    stats.damage_px.fetch_add(damaged, Ordering::Relaxed);
    let buffer = frame.buffer as usize;
    let generation = shared.generation.load(Ordering::Relaxed);
    let next = match frame.kind {
        1 => {
            stats.dmabuf_frames.fetch_add(1, Ordering::Relaxed);
            let planes = (0..frame.n_planes.min(4) as usize)
                .map(|i| gpui::DmaBufPlane { fd: frame.fds[i], offset: frame.offsets[i], stride: frame.strides[i] })
                .collect();
            Frame::DmaBuf {
                frame: Arc::new(gpui::DmaBufFrame {
                    id: buffer as u64,
                    generation: shared.generation.load(Ordering::Relaxed),
                    width: frame.width,
                    height: frame.height,
                    fourcc: frame.fourcc,
                    modifier: frame.modifier,
                    planes,
                }),
                buffer,
                generation,
            }
        }
        _ => {
            stats.shm_frames.fetch_add(1, Ordering::Relaxed);
            let start = Instant::now();
            let src = unsafe { std::slice::from_raw_parts(frame.shm_data, frame.shm_len) };
            let stride = frame.shm_stride as usize;
            let mut guard = shared.pixels.lock().unwrap();
            let fresh = guard.as_ref().is_none_or(|p| p.len() != src.len());
            if fresh {
                *guard = Some(Arc::new(src.to_vec()));
            } else {
                let data = Arc::make_mut(guard.as_mut().unwrap());
                let rects = if damage.is_empty() { vec![[0, 0, frame.width, frame.height]] } else { damage.clone() };
                for [x, y, w, h] in rects {
                    let (x, w) = (x.min(frame.width) as usize, w as usize);
                    for row in y as usize..(y + h).min(frame.height) as usize {
                        let start = row * stride + x * 4;
                        let end = (start + w * 4).min(row * stride + frame.width as usize * 4);
                        data[start..end].copy_from_slice(&src[start..end]);
                    }
                }
            }
            let data = guard.as_ref().unwrap().clone();
            drop(guard);
            stats.shm_copy_us.fetch_add(start.elapsed().as_micros() as u64, Ordering::Relaxed);
            Frame::Pixels {
                frame: Arc::new(gpui::PixelFrame {
                    texture_key: 1,
                    serial: shared.serial.fetch_add(1, Ordering::Relaxed),
                    width: frame.width,
                    height: frame.height,
                    stride: frame.shm_stride,
                    // WPE_PIXEL_FORMAT_ARGB8888: B, G, R, A in memory on little endian.
                    bgra: true,
                    data,
                    damage: if fresh { Vec::new() } else { damage },
                }),
                buffer,
                generation,
            }
        }
    };
    shared.frame_times.lock().unwrap().push(Instant::now());
    let superseded = shared.latest.lock().unwrap().replace(next);
    if let Some(old) = superseded {
        // GPUI never drew it: hand it back right away.
        frame_done(old.buffer());
        if matches!(old, Frame::DmaBuf { .. }) {
            release(old.buffer());
        }
    }
    let _ = shared.wake.try_send(());
}

extern "C" fn on_event(ud: *mut c_void, kind: c_int, text: *const c_char) {
    let shared = unsafe { &*(ud as *const Shared) };
    let text = if text.is_null() { String::new() } else { unsafe { CStr::from_ptr(text) }.to_string_lossy().into_owned() };
    let event = match kind {
        2 => Event::LoadFinished(text),
        3 => Event::JsResult(text),
        4 | 6 => {
            if kind == 4 {
                shared.generation.fetch_add(1, Ordering::Relaxed);
            }
            shared.stats.buffers_changed.fetch_add(1, Ordering::Relaxed);
            log::info!("buffers changed: {text}");
            return;
        }
        _ => Event::Error(text),
    };
    let _ = shared.events.try_send(event);
}

pub struct Host {
    pub shared: Arc<Shared>,
    pub wake: async_channel::Receiver<()>,
    pub events: async_channel::Receiver<Event>,
}

pub fn start(device: Option<String>, url: String, width: i32, height: i32) -> Host {
    let (wake_tx, wake) = async_channel::bounded(1);
    let (events_tx, events) = async_channel::unbounded();
    let shared = Arc::new(Shared {
        latest: Mutex::new(None),
        frame_times: Mutex::new(Vec::new()),
        stats: Stats::default(),
        generation: AtomicU64::new(1),
        serial: AtomicU64::new(1),
        pixels: Mutex::new(None),
        wake: wake_tx,
        events: events_tx,
    });
    let ud = Arc::into_raw(shared.clone()) as usize;
    std::thread::Builder::new()
        .name("wpe".into())
        .spawn(move || {
            let device = device.map(|d| CString::new(d).unwrap());
            let url = CString::new(url).unwrap();
            let config = SpikeConfig {
                device: device.as_ref().map_or(std::ptr::null(), |d| d.as_ptr()),
                url: url.as_ptr(),
                width,
                height,
                on_frame,
                on_event,
                ud: ud as *mut c_void,
            };
            let code = unsafe { spike_run(&config) };
            log::info!("wpe thread exited with {code}");
        })
        .expect("spawn wpe thread");
    Host { shared, wake, events }
}

pub fn ready() -> bool {
    unsafe { spike_ready() != 0 }
}
pub fn frame_done(buffer: usize) {
    unsafe { spike_frame_done(buffer as *mut c_void) }
}
pub fn release(buffer: usize) {
    unsafe { spike_buffer_release(buffer as *mut c_void) }
}
/// kind: 0 move, 1 down, 2 up; button 1 left, 2 middle, 3 right.
pub fn pointer(kind: i32, x: f64, y: f64, button: i32, press_count: i32, modifiers: u32) {
    unsafe { spike_pointer(kind, x, y, button, press_count, modifiers) }
}
pub fn scroll(x: f64, y: f64, dx: f64, dy: f64, precise: bool, modifiers: u32) {
    unsafe { spike_scroll(x, y, dx, dy, c_int::from(precise), modifiers) }
}
pub fn key(down: bool, keyval: u32, modifiers: u32) {
    unsafe { spike_key(c_int::from(down), keyval, modifiers) }
}
pub fn resize(width: i32, height: i32) {
    unsafe { spike_resize(width, height) }
}
pub fn eval(script: &str) {
    let script = CString::new(script).unwrap();
    unsafe { spike_eval(script.as_ptr()) }
}
pub fn quit() {
    unsafe { spike_quit() }
}

/// WPEModifiers bits from GPUI modifiers.
pub fn modifiers(m: &gpui::Modifiers) -> u32 {
    (u32::from(m.control)) | (u32::from(m.shift) << 1) | (u32::from(m.alt) << 2) | (u32::from(m.platform) << 3)
}

/// XKB keysym for a GPUI key name; 0 for keys the spike does not map.
pub fn keyval(key: &str) -> u32 {
    match key {
        "enter" => 0xff0d,
        "backspace" => 0xff08,
        "tab" => 0xff09,
        "escape" => 0xff1b,
        "delete" => 0xffff,
        "left" => 0xff51,
        "up" => 0xff52,
        "right" => 0xff53,
        "down" => 0xff54,
        "pageup" => 0xff55,
        "pagedown" => 0xff56,
        "home" => 0xff50,
        "end" => 0xff57,
        "space" => 0x20,
        k if k.chars().count() == 1 => {
            let c = k.chars().next().unwrap();
            if c.is_ascii() { c as u32 } else { 0x0100_0000 | c as u32 }
        }
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyvals_follow_xkb() {
        assert_eq!(keyval("a"), 0x61);
        assert_eq!(keyval("enter"), 0xff0d);
        assert_eq!(keyval("ж"), 0x0100_0436);
        assert_eq!(keyval("f13"), 0);
    }

    #[test]
    fn modifier_bits_match_wpe() {
        let m = gpui::Modifiers { control: true, alt: true, ..Default::default() };
        assert_eq!(modifiers(&m), 0b101);
    }
}
