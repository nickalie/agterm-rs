//! `Surface::release_gpu_resources` against the real, vendored libghostty on a real GL context: a surfaceless EGL
//! context (Mesa's `EGL_MESA_platform_surfaceless`, no window system) with the test's own framebuffer bound in place
//! of a `GtkGLArea`'s, which is what the host has current when it draws.
//!
//! The checks: a release frees the renderer's GL objects (counted with `glIs*` over the context's small name space,
//! since the context is this test's alone), repeating it is harmless, and the next draw re-creates everything and
//! renders the same frame pixel for pixel (a stale or empty font atlas would lose the glyphs) at the size current then.

use std::error::Error as StdError;
use std::ffi::{c_int, c_uint, c_void};
use std::num::NonZeroUsize;
use std::rc::Rc;
use std::time::{Duration, Instant};

use agterm_ghostty::{
    Action, App, ClipboardConfirm, ClipboardContent, ClipboardKind, ClipboardRead, ClipboardReadResult, Config,
    PendingRequest, Runtime, Surface, SurfaceKey, SurfaceOptions, Target,
};

type TestResult = Result<(), Box<dyn StdError>>;

#[link(name = "EGL")]
unsafe extern "C" {
    fn eglGetPlatformDisplay(platform: c_uint, native_display: *mut c_void, attribs: *const isize) -> *mut c_void;
    fn eglInitialize(display: *mut c_void, major: *mut c_int, minor: *mut c_int) -> c_uint;
    fn eglBindAPI(api: c_uint) -> c_uint;
    fn eglChooseConfig(
        display: *mut c_void,
        attribs: *const c_int,
        configs: *mut *mut c_void,
        size: c_int,
        count: *mut c_int,
    ) -> c_uint;
    fn eglCreateContext(
        display: *mut c_void,
        config: *mut c_void,
        share: *mut c_void,
        attribs: *const c_int,
    ) -> *mut c_void;
    fn eglMakeCurrent(display: *mut c_void, draw: *mut c_void, read: *mut c_void, context: *mut c_void) -> c_uint;
    fn eglDestroyContext(display: *mut c_void, context: *mut c_void) -> c_uint;
    fn eglTerminate(display: *mut c_void) -> c_uint;
}

#[link(name = "OpenGL")]
unsafe extern "C" {
    fn glGenFramebuffers(n: c_int, ids: *mut c_uint);
    fn glDeleteFramebuffers(n: c_int, ids: *const c_uint);
    fn glBindFramebuffer(target: c_uint, id: c_uint);
    fn glGenRenderbuffers(n: c_int, ids: *mut c_uint);
    fn glDeleteRenderbuffers(n: c_int, ids: *const c_uint);
    fn glBindRenderbuffer(target: c_uint, id: c_uint);
    fn glRenderbufferStorage(target: c_uint, format: c_uint, width: c_int, height: c_int);
    fn glFramebufferRenderbuffer(target: c_uint, attachment: c_uint, rb_target: c_uint, id: c_uint);
    fn glCheckFramebufferStatus(target: c_uint) -> c_uint;
    fn glViewport(x: c_int, y: c_int, width: c_int, height: c_int);
    fn glClearColor(r: f32, g: f32, b: f32, a: f32);
    fn glClear(mask: c_uint);
    fn glReadPixels(x: c_int, y: c_int, w: c_int, h: c_int, format: c_uint, kind: c_uint, data: *mut c_void);
    fn glIsTexture(id: c_uint) -> u8;
    fn glIsBuffer(id: c_uint) -> u8;
    fn glIsFramebuffer(id: c_uint) -> u8;
}

const EGL_PLATFORM_SURFACELESS_MESA: c_uint = 0x31DD;
const EGL_OPENGL_API: c_uint = 0x30A2;
const EGL_NONE: c_int = 0x3038;
const EGL_RENDERABLE_TYPE: c_int = 0x3040;
const EGL_OPENGL_BIT: c_int = 0x0008;
const EGL_SURFACE_TYPE: c_int = 0x3033;
const EGL_PBUFFER_BIT: c_int = 0x0001;
const EGL_CONTEXT_MAJOR_VERSION: c_int = 0x3098;
const EGL_CONTEXT_MINOR_VERSION: c_int = 0x30FB;
const EGL_CONTEXT_OPENGL_PROFILE_MASK: c_int = 0x30FD;
const EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT: c_int = 0x0001;

const GL_FRAMEBUFFER: c_uint = 0x8D40;
const GL_RENDERBUFFER: c_uint = 0x8D41;
const GL_RGBA8: c_uint = 0x8058;
const GL_COLOR_ATTACHMENT0: c_uint = 0x8CE0;
const GL_FRAMEBUFFER_COMPLETE: c_uint = 0x8CD5;
const GL_COLOR_BUFFER_BIT: c_uint = 0x4000;
const GL_RGBA: c_uint = 0x1908;
const GL_UNSIGNED_BYTE: c_uint = 0x1401;

/// The terminal background the config sets, as the framebuffer stores it.
const BACKGROUND: [u8; 4] = [0x10, 0x20, 0x30, 0xff];
/// What [`HostFramebuffer::frame`] clears to: no terminal pixel has it, so any left means a missing draw.
const MAGENTA: [u8; 4] = [0xff, 0x00, 0xff, 0xff];

/// A surfaceless GL 4.3 core context, current on this thread while it lives.
struct EglContext {
    display: *mut c_void,
    context: *mut c_void,
}

impl EglContext {
    fn new() -> Result<Self, Box<dyn StdError>> {
        // SAFETY: the surfaceless platform takes no native display; a null attribute list is an empty one.
        let display =
            unsafe { eglGetPlatformDisplay(EGL_PLATFORM_SURFACELESS_MESA, std::ptr::null_mut(), std::ptr::null()) };
        if display.is_null() {
            return Err("no surfaceless EGL display (EGL_MESA_platform_surfaceless)".into());
        }
        // SAFETY: a display from eglGetPlatformDisplay; null version out-pointers are allowed.
        let initialized = unsafe { eglInitialize(display, std::ptr::null_mut(), std::ptr::null_mut()) };
        // SAFETY: plain state call.
        let bound = unsafe { eglBindAPI(EGL_OPENGL_API) };
        if initialized == 0 || bound == 0 {
            return Err("eglInitialize or eglBindAPI(EGL_OPENGL_API) failed".into());
        }
        let config_attribs = [
            EGL_RENDERABLE_TYPE,
            EGL_OPENGL_BIT,
            EGL_SURFACE_TYPE,
            EGL_PBUFFER_BIT,
            EGL_NONE,
        ];
        let mut config = std::ptr::null_mut();
        let mut count = 0;
        // SAFETY: an initialized display, a NONE-terminated list and room for one config.
        let chosen = unsafe { eglChooseConfig(display, config_attribs.as_ptr(), &raw mut config, 1, &raw mut count) };
        if chosen == 0 || count == 0 {
            return Err("no EGL config renders desktop OpenGL".into());
        }
        let context_attribs = [
            EGL_CONTEXT_MAJOR_VERSION,
            4,
            EGL_CONTEXT_MINOR_VERSION,
            3,
            EGL_CONTEXT_OPENGL_PROFILE_MASK,
            EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT,
            EGL_NONE,
        ];
        // SAFETY: a config of this display and a NONE-terminated list.
        let context = unsafe { eglCreateContext(display, config, std::ptr::null_mut(), context_attribs.as_ptr()) };
        if context.is_null() {
            return Err("no OpenGL 4.3 core context".into());
        }
        let egl = Self { display, context };
        // SAFETY: a context of this display, current without surfaces (EGL_KHR_surfaceless_context).
        let current = unsafe { eglMakeCurrent(display, std::ptr::null_mut(), std::ptr::null_mut(), context) };
        if current == 0 {
            return Err("eglMakeCurrent without a surface failed".into());
        }
        Ok(egl)
    }
}

impl Drop for EglContext {
    fn drop(&mut self) {
        // SAFETY: releases this thread's current context.
        unsafe {
            eglMakeCurrent(
                self.display,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        // SAFETY: the context is no longer current anywhere.
        unsafe { eglDestroyContext(self.display, self.context) };
        // SAFETY: nothing of this display is used after this.
        unsafe { eglTerminate(self.display) };
    }
}

/// The framebuffer the host has bound when libghostty draws (a `GtkGLArea` binds its own the same way).
struct HostFramebuffer {
    framebuffer: c_uint,
    renderbuffer: c_uint,
    width: u32,
    height: u32,
}

impl HostFramebuffer {
    fn new(width: u32, height: u32) -> Result<Self, Box<dyn StdError>> {
        let mut target = Self {
            framebuffer: 0,
            renderbuffer: 0,
            width,
            height,
        };
        // SAFETY: the test's context is current; out-pointers to one name each.
        unsafe { glGenFramebuffers(1, &raw mut target.framebuffer) };
        // SAFETY: as above.
        unsafe { glGenRenderbuffers(1, &raw mut target.renderbuffer) };
        target.resize(width, height)?;
        Ok(target)
    }

    fn resize(&mut self, width: u32, height: u32) -> Result<(), Box<dyn StdError>> {
        let (w, h) = (c_int::try_from(width)?, c_int::try_from(height)?);
        (self.width, self.height) = (width, height);
        // SAFETY: names this target generated, on the current context.
        unsafe { glBindRenderbuffer(GL_RENDERBUFFER, self.renderbuffer) };
        // SAFETY: the bound renderbuffer gets storage of a valid size.
        unsafe { glRenderbufferStorage(GL_RENDERBUFFER, GL_RGBA8, w, h) };
        // SAFETY: binds this target's framebuffer for drawing and reading.
        unsafe { glBindFramebuffer(GL_FRAMEBUFFER, self.framebuffer) };
        // SAFETY: attaches the renderbuffer to the bound framebuffer.
        unsafe { glFramebufferRenderbuffer(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_RENDERBUFFER, self.renderbuffer) };
        // SAFETY: plain state query on the bound framebuffer.
        if unsafe { glCheckFramebufferStatus(GL_FRAMEBUFFER) } != GL_FRAMEBUFFER_COMPLETE {
            return Err("the test framebuffer is incomplete".into());
        }
        // SAFETY: plain state call; libghostty reads the drawable size from the viewport.
        unsafe { glViewport(0, 0, w, h) };
        Ok(())
    }

    /// Fill with [`MAGENTA`], let `surface` draw, and read the result back.
    fn frame(&self, surface: &Surface) -> Result<Vec<u8>, Box<dyn StdError>> {
        // SAFETY: plain state calls on the bound framebuffer.
        unsafe { glClearColor(1.0, 0.0, 1.0, 1.0) };
        // SAFETY: as above.
        unsafe { glClear(GL_COLOR_BUFFER_BIT) };
        surface.draw();
        let mut pixels = vec![0u8; usize::try_from(self.width * self.height * 4)?];
        let (w, h) = (c_int::try_from(self.width)?, c_int::try_from(self.height)?);
        // SAFETY: the bound framebuffer is w×h RGBA8 and the buffer holds exactly w×h×4 bytes.
        unsafe { glReadPixels(0, 0, w, h, GL_RGBA, GL_UNSIGNED_BYTE, pixels.as_mut_ptr().cast()) };
        Ok(pixels)
    }
}

impl Drop for HostFramebuffer {
    fn drop(&mut self) {
        // SAFETY: names this target generated, on the still-current context.
        unsafe { glDeleteFramebuffers(1, &raw const self.framebuffer) };
        // SAFETY: as above.
        unsafe { glDeleteRenderbuffers(1, &raw const self.renderbuffer) };
    }
}

/// How many textures, buffers and framebuffers exist among the context's first names. GL hands out the lowest free
/// names, and nothing but libghostty and [`HostFramebuffer`] creates objects in this context.
fn live_gl_objects() -> usize {
    (1..512)
        .map(|name| {
            // SAFETY: glIs* only query a name, whatever it is.
            let texture = unsafe { glIsTexture(name) };
            // SAFETY: as above.
            let buffer = unsafe { glIsBuffer(name) };
            // SAFETY: as above.
            let framebuffer = unsafe { glIsFramebuffer(name) };
            usize::from(texture) + usize::from(buffer) + usize::from(framebuffer)
        })
        .sum()
}

struct Quiet;

impl Runtime for Quiet {
    fn action(&self, _target: Target, _action: Action) -> bool {
        true
    }

    fn clipboard_read_result(&self, _read: &ClipboardRead) -> ClipboardReadResult {
        ClipboardReadResult::Unsupported
    }

    fn start_clipboard_read(&self, _read: ClipboardRead, _pending: PendingRequest) {}

    fn confirm_clipboard_request(&self, _confirm: ClipboardConfirm, _pending: PendingRequest) {}

    fn write_clipboard(&self, _: Option<SurfaceKey>, _: ClipboardKind, _: Vec<ClipboardContent>, _: bool) {}

    fn close_surface(&self, _surface: SurfaceKey, _process_alive: bool) {}
}

/// Draw until two frames in a row match, cover the whole framebuffer and show more than the background, so the
/// renderer thread has laid out the child's output at the current size; returns that frame.
fn settled_frame(app: &App, surface: &Surface, target: &HostFramebuffer) -> Result<Vec<u8>, Box<dyn StdError>> {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut previous = Vec::new();
    while Instant::now() < deadline {
        app.tick();
        let frame = target.frame(surface)?;
        let pixels = frame.as_chunks::<4>().0;
        let covered = pixels.iter().all(|pixel| *pixel != MAGENTA);
        let output = pixels.iter().any(|pixel| *pixel != BACKGROUND);
        if frame == previous && covered && output {
            return Ok(frame);
        }
        previous = frame;
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("the surface never settled on a frame with its output".into())
}

#[test]
fn a_released_surface_draws_the_same_frame_again() -> TestResult {
    let egl = EglContext::new()?;
    let mut config = Config::new()?;
    config.load_text("background = #102030\nforeground = #e0e0e0\ncursor-style-blink = false\nfont-size = 14\n");
    config.finalize();
    let app = App::new(&config, Rc::new(Quiet), Box::new(|| {}))?;
    let mut target = HostFramebuffer::new(640, 360)?;
    let surface = Surface::new(
        &app,
        SurfaceKey::new(NonZeroUsize::MIN),
        &SurfaceOptions {
            scale_factor: 1.0,
            command: Some("printf 'released GPU resources come back'; exec sleep 60"),
            ..SurfaceOptions::default()
        },
    )?;
    surface.set_size(target.width, target.height);

    let shown = settled_frame(&app, &surface, &target)?;
    let drawing = live_gl_objects();
    surface.release_gpu_resources();
    let released = live_gl_objects();
    assert!(
        released < drawing,
        "release freed nothing: {drawing} GL objects before, {released} after"
    );
    surface.release_gpu_resources();
    assert_eq!(live_gl_objects(), released, "a second release changed something");

    // shown again: the first draw rebuilds every resource and renders the frame from before the release
    assert!(
        target.frame(&surface)? == shown,
        "the redrawn frame differs from the one before the release"
    );
    assert_eq!(live_gl_objects(), drawing);

    // resized while released: the rebuilt targets take the new size, so the far corner is terminal, not magenta
    surface.release_gpu_resources();
    target.resize(800, 500)?;
    surface.set_size(target.width, target.height);
    let resized = settled_frame(&app, &surface, &target)?;
    assert_eq!(resized.len(), 800 * 500 * 4);
    assert_eq!(
        resized[resized.len() - 4..],
        BACKGROUND,
        "the top right corner was not drawn"
    );
    assert_eq!(resized[..4], BACKGROUND, "the bottom left corner was not drawn");

    drop(surface);
    drop(app);
    drop(target);
    drop(egl);
    Ok(())
}
