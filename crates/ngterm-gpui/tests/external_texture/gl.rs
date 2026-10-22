//! A GL producer on a render node, as WPE `WebKit` is one: buffers GBM allocates with the device's own modifiers
//! (Intel's CCS ones keep main, aux and clear colour in one object), rendered into through EGL and GLES — a clear, which
//! the driver may turn into a fast clear, writing the clear colour plane — and handed over as dmabufs. `libgbm` and
//! `libEGL` are loaded at run time (the test binary links neither); GL functions come from `eglGetProcAddress`.

use std::ffi::{CStr, c_int, c_void};
use std::fs::File;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::path::Path;
use std::ptr::{null, null_mut};
use std::sync::Arc;

use gpui_kit::{DmaBufPlane, DmaBuffer};

type Handle = *mut c_void;

const GBM_BO_USE_RENDERING: u32 = 1 << 2;
const EGL_PLATFORM_GBM_KHR: u32 = 0x31D7;
const EGL_OPENGL_ES_API: u32 = 0x30A0;
const EGL_CONTEXT_MAJOR_VERSION: i32 = 0x3098;
const EGL_NONE: i32 = 0x3038;
const EGL_WIDTH: i32 = 0x3057;
const EGL_HEIGHT: i32 = 0x3056;
const EGL_LINUX_DMA_BUF_EXT: u32 = 0x3270;
const EGL_LINUX_DRM_FOURCC_EXT: i32 = 0x3271;
/// Per plane: fd, offset, pitch, modifier low and high words.
const EGL_PLANE_ATTRIBUTES: [[i32; 5]; 4] = [
    [0x3272, 0x3273, 0x3274, 0x3443, 0x3444],
    [0x3275, 0x3276, 0x3277, 0x3445, 0x3446],
    [0x3278, 0x3279, 0x327A, 0x3447, 0x3448],
    [0x3440, 0x3441, 0x3442, 0x3449, 0x344A],
];
const GL_RENDERBUFFER: u32 = 0x8D41;
const GL_FRAMEBUFFER: u32 = 0x8D40;
const GL_COLOR_ATTACHMENT0: u32 = 0x8CE0;
const GL_FRAMEBUFFER_COMPLETE: u32 = 0x8CD5;
const GL_COLOR_BUFFER_BIT: u32 = 0x4000;

/// The function `name` from `lookup` as `T`.
///
/// # Safety
///
/// `T` is the function pointer type of the C signature the library declares for `name`.
unsafe fn function<T: Copy>(lookup: &dyn Fn(&CStr) -> Handle, name: &CStr) -> Result<T, String> {
    let address = lookup(name);
    if address.is_null() {
        return Err(format!("no {}", name.to_string_lossy()));
    }
    assert_eq!(size_of::<T>(), size_of::<Handle>(), "a function pointer");
    // SAFETY: the caller guarantees `T` is the symbol's function pointer type, as large as a data pointer (asserted).
    Ok(unsafe { std::mem::transmute_copy::<Handle, T>(&address) })
}

/// A library loaded with `dlopen`, closed on drop.
struct Library(Handle);

impl Library {
    fn open(name: &CStr) -> Result<Self, String> {
        // SAFETY: the name is NUL-terminated; dlopen has no other precondition.
        let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if handle.is_null() {
            Err(format!("{} is not installed", name.to_string_lossy()))
        } else {
            Ok(Self(handle))
        }
    }

    fn symbol(&self, name: &CStr) -> Handle {
        // SAFETY: the handle is dlopen's and `name` is NUL-terminated.
        unsafe { libc::dlsym(self.0, name.as_ptr()) }
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: the handle is dlopen's; nothing from the library is used after its owner is gone.
        unsafe { libc::dlclose(self.0) };
    }
}

/// libgbm's functions (gbm.h).
struct Gbm {
    create_device: unsafe extern "C" fn(c_int) -> Handle,
    destroy_device: unsafe extern "C" fn(Handle),
    create_bo: unsafe extern "C" fn(Handle, u32, u32, u32, *const u64, u32, u32) -> Handle,
    destroy_bo: unsafe extern "C" fn(Handle),
    plane_count: unsafe extern "C" fn(Handle) -> c_int,
    fd_for_plane: unsafe extern "C" fn(Handle, c_int) -> c_int,
    offset: unsafe extern "C" fn(Handle, c_int) -> u32,
    stride: unsafe extern "C" fn(Handle, c_int) -> u32,
    modifier: unsafe extern "C" fn(Handle) -> u64,
    _library: Library,
}

impl Gbm {
    fn load() -> Result<Self, String> {
        let library = Library::open(c"libgbm.so.1")?;
        let lookup = |name: &CStr| library.symbol(name);
        Ok(Self {
            // SAFETY: gbm.h: struct gbm_device *gbm_create_device(int fd).
            create_device: unsafe { function(&lookup, c"gbm_create_device")? },
            // SAFETY: gbm.h: void gbm_device_destroy(struct gbm_device *).
            destroy_device: unsafe { function(&lookup, c"gbm_device_destroy")? },
            // SAFETY: gbm.h: struct gbm_bo *gbm_bo_create_with_modifiers2(struct gbm_device *, uint32_t width,
            // uint32_t height, uint32_t format, const uint64_t *modifiers, unsigned int count, uint32_t flags).
            create_bo: unsafe { function(&lookup, c"gbm_bo_create_with_modifiers2")? },
            // SAFETY: gbm.h: void gbm_bo_destroy(struct gbm_bo *).
            destroy_bo: unsafe { function(&lookup, c"gbm_bo_destroy")? },
            // SAFETY: gbm.h: int gbm_bo_get_plane_count(struct gbm_bo *).
            plane_count: unsafe { function(&lookup, c"gbm_bo_get_plane_count")? },
            // SAFETY: gbm.h: int gbm_bo_get_fd_for_plane(struct gbm_bo *, int plane).
            fd_for_plane: unsafe { function(&lookup, c"gbm_bo_get_fd_for_plane")? },
            // SAFETY: gbm.h: uint32_t gbm_bo_get_offset(struct gbm_bo *, int plane).
            offset: unsafe { function(&lookup, c"gbm_bo_get_offset")? },
            // SAFETY: gbm.h: uint32_t gbm_bo_get_stride_for_plane(struct gbm_bo *, int plane).
            stride: unsafe { function(&lookup, c"gbm_bo_get_stride_for_plane")? },
            // SAFETY: gbm.h: uint64_t gbm_bo_get_modifier(struct gbm_bo *).
            modifier: unsafe { function(&lookup, c"gbm_bo_get_modifier")? },
            _library: library,
        })
    }
}

/// libEGL's and GLES 2's functions (egl.h, gl2.h, the EGL dmabuf import and OES EGL image extensions).
struct Egl {
    get_platform_display: unsafe extern "C" fn(u32, Handle, *const isize) -> Handle,
    initialize: unsafe extern "C" fn(Handle, *mut i32, *mut i32) -> u32,
    bind_api: unsafe extern "C" fn(u32) -> u32,
    create_context: unsafe extern "C" fn(Handle, Handle, Handle, *const i32) -> Handle,
    make_current: unsafe extern "C" fn(Handle, Handle, Handle, Handle) -> u32,
    destroy_context: unsafe extern "C" fn(Handle, Handle) -> u32,
    terminate: unsafe extern "C" fn(Handle) -> u32,
    create_image: unsafe extern "C" fn(Handle, Handle, u32, Handle, *const i32) -> Handle,
    image_target_renderbuffer: unsafe extern "C" fn(u32, Handle),
    gen_renderbuffers: unsafe extern "C" fn(i32, *mut u32),
    bind_renderbuffer: unsafe extern "C" fn(u32, u32),
    gen_framebuffers: unsafe extern "C" fn(i32, *mut u32),
    bind_framebuffer: unsafe extern "C" fn(u32, u32),
    framebuffer_renderbuffer: unsafe extern "C" fn(u32, u32, u32, u32),
    check_framebuffer_status: unsafe extern "C" fn(u32) -> u32,
    viewport: unsafe extern "C" fn(i32, i32, i32, i32),
    clear_color: unsafe extern "C" fn(f32, f32, f32, f32),
    clear: unsafe extern "C" fn(u32),
    finish: unsafe extern "C" fn(),
    _library: Library,
}

impl Egl {
    fn load() -> Result<Self, String> {
        let library = Library::open(c"libEGL.so.1")?;
        // SAFETY: egl.h: __eglMustCastToProperFunctionPointerType eglGetProcAddress(const char *procname).
        let get_proc: unsafe extern "C" fn(*const libc::c_char) -> Handle =
            unsafe { function(&|name: &CStr| library.symbol(name), c"eglGetProcAddress")? };
        // SAFETY: eglGetProcAddress takes a NUL-terminated name and returns null or the function.
        let lookup = |name: &CStr| unsafe { get_proc(name.as_ptr()) };
        Ok(Self {
            // SAFETY: egl.h: EGLDisplay eglGetPlatformDisplay(EGLenum, void *native, const EGLAttrib *).
            get_platform_display: unsafe { function(&lookup, c"eglGetPlatformDisplay")? },
            // SAFETY: egl.h: EGLBoolean eglInitialize(EGLDisplay, EGLint *major, EGLint *minor).
            initialize: unsafe { function(&lookup, c"eglInitialize")? },
            // SAFETY: egl.h: EGLBoolean eglBindAPI(EGLenum).
            bind_api: unsafe { function(&lookup, c"eglBindAPI")? },
            // SAFETY: egl.h: EGLContext eglCreateContext(EGLDisplay, EGLConfig, EGLContext, const EGLint *).
            create_context: unsafe { function(&lookup, c"eglCreateContext")? },
            // SAFETY: egl.h: EGLBoolean eglMakeCurrent(EGLDisplay, EGLSurface, EGLSurface, EGLContext).
            make_current: unsafe { function(&lookup, c"eglMakeCurrent")? },
            // SAFETY: egl.h: EGLBoolean eglDestroyContext(EGLDisplay, EGLContext).
            destroy_context: unsafe { function(&lookup, c"eglDestroyContext")? },
            // SAFETY: egl.h: EGLBoolean eglTerminate(EGLDisplay).
            terminate: unsafe { function(&lookup, c"eglTerminate")? },
            // SAFETY: eglext.h: EGLImageKHR eglCreateImageKHR(EGLDisplay, EGLContext, EGLenum, EGLClientBuffer,
            // const EGLint *).
            create_image: unsafe { function(&lookup, c"eglCreateImageKHR")? },
            // SAFETY: gl2ext.h: void glEGLImageTargetRenderbufferStorageOES(GLenum, GLeglImageOES).
            image_target_renderbuffer: unsafe { function(&lookup, c"glEGLImageTargetRenderbufferStorageOES")? },
            // SAFETY: gl2.h: void glGenRenderbuffers(GLsizei, GLuint *).
            gen_renderbuffers: unsafe { function(&lookup, c"glGenRenderbuffers")? },
            // SAFETY: gl2.h: void glBindRenderbuffer(GLenum, GLuint).
            bind_renderbuffer: unsafe { function(&lookup, c"glBindRenderbuffer")? },
            // SAFETY: gl2.h: void glGenFramebuffers(GLsizei, GLuint *).
            gen_framebuffers: unsafe { function(&lookup, c"glGenFramebuffers")? },
            // SAFETY: gl2.h: void glBindFramebuffer(GLenum, GLuint).
            bind_framebuffer: unsafe { function(&lookup, c"glBindFramebuffer")? },
            // SAFETY: gl2.h: void glFramebufferRenderbuffer(GLenum, GLenum, GLenum, GLuint).
            framebuffer_renderbuffer: unsafe { function(&lookup, c"glFramebufferRenderbuffer")? },
            // SAFETY: gl2.h: GLenum glCheckFramebufferStatus(GLenum).
            check_framebuffer_status: unsafe { function(&lookup, c"glCheckFramebufferStatus")? },
            // SAFETY: gl2.h: void glViewport(GLint, GLint, GLsizei, GLsizei).
            viewport: unsafe { function(&lookup, c"glViewport")? },
            // SAFETY: gl2.h: void glClearColor(GLfloat, GLfloat, GLfloat, GLfloat).
            clear_color: unsafe { function(&lookup, c"glClearColor")? },
            // SAFETY: gl2.h: void glClear(GLbitfield).
            clear: unsafe { function(&lookup, c"glClear")? },
            // SAFETY: gl2.h: void glFinish(void).
            finish: unsafe { function(&lookup, c"glFinish")? },
            _library: library,
        })
    }
}

/// GBM and a current GLES context on one render node.
pub struct GlProducer {
    egl: Egl,
    display: Handle,
    context: Handle,
    gbm: Gbm,
    device: Handle,
    _node: File,
}

/// A buffer the producer renders into: GBM's object, exported, and the framebuffer GL renders it through.
pub struct GlBuffer {
    pub buffer: Arc<DmaBuffer>,
    framebuffer: u32,
}

impl GlProducer {
    /// A producer on `node`.
    pub fn new(node: &Path) -> Result<Self, String> {
        let gbm = Gbm::load()?;
        let egl = Egl::load()?;
        let node = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(node)
            .map_err(|error| format!("{}: {error}", node.display()))?;
        // SAFETY: the fd is an open render node, kept open while the device lives (`_node`).
        let device = unsafe { (gbm.create_device)(node.as_raw_fd()) };
        if device.is_null() {
            return Err("gbm_create_device failed".to_owned());
        }
        let mut producer = Self {
            egl,
            display: null_mut(),
            context: null_mut(),
            gbm,
            device,
            _node: node,
        };
        producer.start_egl()?;
        Ok(producer)
    }

    fn start_egl(&mut self) -> Result<(), String> {
        let egl = &self.egl;
        // SAFETY: `device` is a live GBM device, the native display of the GBM platform; no attributes.
        self.display = unsafe { (egl.get_platform_display)(EGL_PLATFORM_GBM_KHR, self.device, null()) };
        if self.display.is_null() {
            return Err("no EGL display on GBM".to_owned());
        }
        // SAFETY: the display was returned above; the version outputs may be null.
        if unsafe { (egl.initialize)(self.display, null_mut(), null_mut()) } == 0 {
            return Err("eglInitialize failed".to_owned());
        }
        // SAFETY: binding the GLES API needs no other state.
        if unsafe { (egl.bind_api)(EGL_OPENGL_ES_API) } == 0 {
            return Err("eglBindAPI(GLES) failed".to_owned());
        }
        let attributes = [EGL_CONTEXT_MAJOR_VERSION, 2, EGL_NONE];
        // SAFETY: an initialized display; no config (EGL_KHR_no_config_context), no share context, NONE-terminated
        // attributes alive for the call.
        self.context = unsafe { (egl.create_context)(self.display, null_mut(), null_mut(), attributes.as_ptr()) };
        if self.context.is_null() {
            return Err("eglCreateContext failed (no EGL_KHR_no_config_context?)".to_owned());
        }
        // SAFETY: the context is this display's; no surfaces (EGL_KHR_surfaceless_context).
        if unsafe { (egl.make_current)(self.display, null_mut(), null_mut(), self.context) } == 0 {
            return Err("eglMakeCurrent (surfaceless) failed".to_owned());
        }
        Ok(())
    }

    /// A `width` x `height` `fourcc` buffer with one of `modifiers`, exported plane by plane and bound to a framebuffer.
    pub fn allocate(&self, width: u32, height: u32, fourcc: u32, modifiers: &[u64]) -> Result<GlBuffer, String> {
        let count = u32::try_from(modifiers.len()).map_err(|_| "too many modifiers")?;
        // SAFETY: `device` is live, `modifiers` holds `count` values for the call.
        let bo = unsafe {
            (self.gbm.create_bo)(
                self.device,
                width,
                height,
                fourcc,
                modifiers.as_ptr(),
                count,
                GBM_BO_USE_RENDERING,
            )
        };
        if bo.is_null() {
            return Err("gbm_bo_create_with_modifiers2 failed".to_owned());
        }
        let exported = self.export(bo, width, height, fourcc);
        // SAFETY: `bo` is live and owned here; the exported fds keep its memory.
        unsafe { (self.gbm.destroy_bo)(bo) };
        let buffer = exported?;
        let framebuffer = self.framebuffer(&buffer)?;
        Ok(GlBuffer { buffer, framebuffer })
    }

    fn export(&self, bo: Handle, width: u32, height: u32, fourcc: u32) -> Result<Arc<DmaBuffer>, String> {
        // SAFETY: `bo` is a live buffer object.
        let planes = unsafe { (self.gbm.plane_count)(bo) };
        // SAFETY: as above.
        let modifier = unsafe { (self.gbm.modifier)(bo) };
        let mut exported = Vec::new();
        for plane in 0..planes {
            // SAFETY: `bo` is live and `plane` below its plane count.
            let fd = unsafe { (self.gbm.fd_for_plane)(bo, plane) };
            if fd < 0 {
                return Err(format!("gbm_bo_get_fd_for_plane({plane}) failed"));
            }
            exported.push(DmaBufPlane {
                // SAFETY: the call returned a new fd this process owns.
                fd: unsafe { OwnedFd::from_raw_fd(fd) },
                // SAFETY: `bo` is live and `plane` below its plane count.
                offset: unsafe { (self.gbm.offset)(bo, plane) },
                // SAFETY: as above.
                stride: unsafe { (self.gbm.stride)(bo, plane) },
            });
        }
        Ok(Arc::new(DmaBuffer {
            width,
            height,
            fourcc,
            modifier,
            planes: exported,
        }))
    }

    /// A framebuffer rendering into `buffer` through an EGL image of its dmabuf.
    fn framebuffer(&self, buffer: &DmaBuffer) -> Result<u32, String> {
        let egl = &self.egl;
        let mut attributes = vec![
            EGL_WIDTH,
            i32::try_from(buffer.width).map_err(|_| "width")?,
            EGL_HEIGHT,
            i32::try_from(buffer.height).map_err(|_| "height")?,
            EGL_LINUX_DRM_FOURCC_EXT,
            buffer.fourcc.cast_signed(),
        ];
        let [modifier_low, modifier_high] = [buffer.modifier & 0xffff_ffff, buffer.modifier >> 32]
            .map(|word| u32::try_from(word).unwrap_or_default().cast_signed());
        for (plane, names) in buffer.planes.iter().zip(EGL_PLANE_ATTRIBUTES) {
            attributes.extend([
                names[0],
                plane.fd.as_raw_fd(),
                names[1],
                plane.offset.cast_signed(),
                names[2],
                plane.stride.cast_signed(),
                names[3],
                modifier_low,
                names[4],
                modifier_high,
            ]);
        }
        attributes.push(EGL_NONE);
        // SAFETY: an initialized display, no context (dmabuf images take none), no client buffer, NONE-terminated
        // attributes naming open fds, all alive for the call (EGL dups what it keeps).
        let image = unsafe {
            (egl.create_image)(
                self.display,
                null_mut(),
                EGL_LINUX_DMA_BUF_EXT,
                null_mut(),
                attributes.as_ptr(),
            )
        };
        if image.is_null() {
            return Err("eglCreateImageKHR (dmabuf) failed".to_owned());
        }
        let mut renderbuffer = 0;
        let mut framebuffer = 0;
        // SAFETY: the context is current; one name is written.
        unsafe { (egl.gen_renderbuffers)(1, &raw mut renderbuffer) };
        // SAFETY: as above.
        unsafe { (egl.bind_renderbuffer)(GL_RENDERBUFFER, renderbuffer) };
        // SAFETY: a renderbuffer is bound and `image` is a live EGL image of this display.
        unsafe { (egl.image_target_renderbuffer)(GL_RENDERBUFFER, image) };
        // SAFETY: the context is current; one name is written.
        unsafe { (egl.gen_framebuffers)(1, &raw mut framebuffer) };
        // SAFETY: as above.
        unsafe { (egl.bind_framebuffer)(GL_FRAMEBUFFER, framebuffer) };
        // SAFETY: the framebuffer and the renderbuffer are this context's.
        unsafe { (egl.framebuffer_renderbuffer)(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_RENDERBUFFER, renderbuffer) };
        // SAFETY: a framebuffer is bound.
        let status = unsafe { (egl.check_framebuffer_status)(GL_FRAMEBUFFER) };
        if status != GL_FRAMEBUFFER_COMPLETE {
            return Err(format!("the dmabuf framebuffer is incomplete ({status:#x})"));
        }
        Ok(framebuffer)
    }

    /// Clears all of `buffer` to `rgba` and waits until GL finished.
    pub fn clear(&self, buffer: &GlBuffer, rgba: [f32; 4]) {
        let egl = &self.egl;
        let width = i32::try_from(buffer.buffer.width).unwrap_or(i32::MAX);
        let height = i32::try_from(buffer.buffer.height).unwrap_or(i32::MAX);
        // SAFETY: the context is current and the framebuffer is its own.
        unsafe { (egl.bind_framebuffer)(GL_FRAMEBUFFER, buffer.framebuffer) };
        // SAFETY: as above.
        unsafe { (egl.viewport)(0, 0, width, height) };
        // SAFETY: as above.
        unsafe { (egl.clear_color)(rgba[0], rgba[1], rgba[2], rgba[3]) };
        // SAFETY: as above.
        unsafe { (egl.clear)(GL_COLOR_BUFFER_BIT) };
        // SAFETY: as above.
        unsafe { (egl.finish)() };
    }
}

impl Drop for GlProducer {
    fn drop(&mut self) {
        let egl = &self.egl;
        if !self.context.is_null() {
            // SAFETY: release the current context of this display.
            unsafe { (egl.make_current)(self.display, null_mut(), null_mut(), null_mut()) };
            // SAFETY: the context is this display's and no longer current.
            unsafe { (egl.destroy_context)(self.display, self.context) };
        }
        if !self.display.is_null() {
            // SAFETY: the display was initialized here; its images and context go with it.
            unsafe { (egl.terminate)(self.display) };
        }
        // SAFETY: the GBM device has no buffer objects left and EGL no longer uses it.
        unsafe { (self.gbm.destroy_device)(self.device) };
    }
}
