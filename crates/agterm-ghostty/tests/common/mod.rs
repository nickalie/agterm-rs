//! What the surface tests share: a surfaceless EGL context (Mesa's `EGL_MESA_platform_surfaceless`, no window system)
//! for libghostty's OpenGL renderer, and a runtime that answers every callback quietly.

use std::error::Error as StdError;
use std::ffi::{c_int, c_uint, c_void};

use agterm_ghostty::{
    Action, ClipboardConfirm, ClipboardContent, ClipboardKind, ClipboardRead, ClipboardReadResult, PendingRequest,
    Runtime, SurfaceKey, Target,
};

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

/// A surfaceless GL 4.3 core context, current on this thread while it lives.
pub struct EglContext {
    display: *mut c_void,
    context: *mut c_void,
}

impl EglContext {
    pub fn new() -> Result<Self, Box<dyn StdError>> {
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

/// A runtime that consumes every action and serves no clipboard.
pub struct Quiet;

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
