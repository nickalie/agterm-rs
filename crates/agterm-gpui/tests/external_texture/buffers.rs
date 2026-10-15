//! Synthetic dmabufs: `udmabuf` over a sealed memfd (linear CPU memory the test writes, imported by any Mesa Vulkan
//! driver) and GBM buffer objects with the device's own modifiers (tiled, compressed: Intel's CCS has three memory
//! planes in one object), allocated on the render node through a `libgbm` loaded at run time.

use std::ffi::{CStr, c_int, c_void};
use std::fs::File;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::fs::FileExt as _;
use std::path::Path;
use std::sync::Arc;

use gpui_kit::{DmaBufPlane, DmaBuffer};

pub const XR24: u32 = u32::from_le_bytes(*b"XR24");
pub const AR24: u32 = u32::from_le_bytes(*b"AR24");

/// `UDMABUF_CREATE`: `_IOW('u', 0x42, struct udmabuf_create)`.
const UDMABUF_CREATE: libc::c_ulong = 0x4018_7542;
const UDMABUF_FLAGS_CLOEXEC: u32 = 0x01;

#[repr(C)]
struct UdmabufCreate {
    memfd: u32,
    flags: u32,
    offset: u64,
    size: u64,
}

/// A linear dmabuf over memory the test writes: `memfd` holds the pixels, `dmabuf` exports them.
pub struct Udmabuf {
    memfd: File,
    dmabuf: OwnedFd,
    pub width: u32,
    pub height: u32,
}

impl Udmabuf {
    /// Why udmabufs cannot be made here, if they cannot.
    pub fn unavailable() -> Option<String> {
        match std::fs::OpenOptions::new().read(true).write(true).open("/dev/udmabuf") {
            Ok(_) => None,
            Err(error) => Some(format!("/dev/udmabuf: {error}")),
        }
    }

    /// A `width` x `height` buffer of 32-bit pixels, all `bgra` (B, G, R, A bytes).
    pub fn new(width: u32, height: u32, bgra: [u8; 4]) -> Result<Self, String> {
        let page = 4096;
        let bytes = u64::from(width) * u64::from(height) * 4;
        let size = bytes.div_ceil(page) * page;
        let memfd = memfd(size)?;
        let device = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/udmabuf")
            .map_err(|error| format!("/dev/udmabuf: {error}"))?;
        let create = UdmabufCreate {
            memfd: u32::try_from(memfd.as_raw_fd()).map_err(|_| "a negative fd")?,
            flags: UDMABUF_FLAGS_CLOEXEC,
            offset: 0,
            size,
        };
        // SAFETY: UDMABUF_CREATE reads one `udmabuf_create`, which `create` is (repr(C), the kernel's layout), from a
        // pointer valid for the call; the memfd is sealed against shrinking as the driver requires.
        let fd = unsafe { libc::ioctl(device.as_raw_fd(), UDMABUF_CREATE, &raw const create) };
        if fd < 0 {
            return Err(format!("UDMABUF_CREATE: {}", std::io::Error::last_os_error()));
        }
        // SAFETY: the ioctl returned a new dmabuf fd this process owns and nothing else holds.
        let dmabuf = unsafe { OwnedFd::from_raw_fd(fd) };
        let buffer = Self {
            memfd,
            dmabuf,
            width,
            height,
        };
        buffer.fill(bgra)?;
        Ok(buffer)
    }

    /// Paints every pixel `bgra`.
    pub fn fill(&self, bgra: [u8; 4]) -> Result<(), String> {
        let pixels: Vec<u8> = bgra.repeat(self.width as usize * self.height as usize);
        self.memfd
            .write_all_at(&pixels, 0)
            .map_err(|error| format!("writing the pixels: {error}"))
    }

    /// Paints the columns from `x` on `bgra`.
    pub fn fill_from_column(&self, x: u32, bgra: [u8; 4]) -> Result<(), String> {
        let row: Vec<u8> = bgra.repeat((self.width - x) as usize);
        for y in 0..self.height {
            let offset = (u64::from(y) * u64::from(self.width) + u64::from(x)) * 4;
            self.memfd
                .write_all_at(&row, offset)
                .map_err(|error| format!("writing the pixels: {error}"))?;
        }
        Ok(())
    }

    /// The buffer as GPUI takes it, with `fourcc` and the linear modifier.
    pub fn buffer(&self, fourcc: u32) -> Result<Arc<DmaBuffer>, String> {
        Ok(Arc::new(DmaBuffer {
            width: self.width,
            height: self.height,
            fourcc,
            modifier: 0,
            planes: vec![DmaBufPlane {
                fd: self.dmabuf.try_clone().map_err(|error| format!("dup: {error}"))?,
                offset: 0,
                stride: self.width * 4,
            }],
        }))
    }
}

/// A sealed memfd of `size` bytes, as `udmabuf` takes one.
fn memfd(size: u64) -> Result<File, String> {
    let name = c"agterm-external-texture";
    // SAFETY: `name` is a NUL-terminated string valid for the call.
    let fd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_ALLOW_SEALING | libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(format!("memfd_create: {}", std::io::Error::last_os_error()));
    }
    // SAFETY: memfd_create returned a new fd this process owns.
    let file = File::from(unsafe { OwnedFd::from_raw_fd(fd) });
    file.set_len(size).map_err(|error| format!("ftruncate: {error}"))?;
    // SAFETY: F_ADD_SEALS takes an int argument on an fd this process owns.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, libc::F_SEAL_SHRINK) } < 0 {
        return Err(format!("F_ADD_SEALS: {}", std::io::Error::last_os_error()));
    }
    Ok(file)
}

// MARK: - GBM

const GBM_BO_USE_RENDERING: u32 = 1 << 2;

type CreateDevice = unsafe extern "C" fn(c_int) -> *mut c_void;
type DestroyDevice = unsafe extern "C" fn(*mut c_void);
type CreateWithModifiers2 = unsafe extern "C" fn(*mut c_void, u32, u32, u32, *const u64, u32, u32) -> *mut c_void;
type DestroyBo = unsafe extern "C" fn(*mut c_void);
type PlaneCount = unsafe extern "C" fn(*mut c_void) -> c_int;
type FdForPlane = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
type U32ForPlane = unsafe extern "C" fn(*mut c_void, c_int) -> u32;
type Modifier = unsafe extern "C" fn(*mut c_void) -> u64;

/// `libgbm`, loaded at run time (the test binary links nothing GBM).
struct Gbm {
    library: *mut c_void,
    create_device: CreateDevice,
    destroy_device: DestroyDevice,
    create_with_modifiers2: CreateWithModifiers2,
    destroy_bo: DestroyBo,
    plane_count: PlaneCount,
    fd_for_plane: FdForPlane,
    offset: U32ForPlane,
    stride: U32ForPlane,
    modifier: Modifier,
}

impl Gbm {
    fn load() -> Result<Self, String> {
        // SAFETY: the name is NUL-terminated; dlopen has no other precondition.
        let library = unsafe { libc::dlopen(c"libgbm.so.1".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if library.is_null() {
            return Err("libgbm.so.1 is not installed".to_owned());
        }
        Ok(Self {
            // SAFETY: gbm.h: struct gbm_device *gbm_create_device(int fd).
            create_device: unsafe { function(library, c"gbm_create_device")? },
            // SAFETY: gbm.h: void gbm_device_destroy(struct gbm_device *).
            destroy_device: unsafe { function(library, c"gbm_device_destroy")? },
            // SAFETY: gbm.h: struct gbm_bo *gbm_bo_create_with_modifiers2(struct gbm_device *, uint32_t width,
            // uint32_t height, uint32_t format, const uint64_t *modifiers, unsigned int count, uint32_t flags).
            create_with_modifiers2: unsafe { function(library, c"gbm_bo_create_with_modifiers2")? },
            // SAFETY: gbm.h: void gbm_bo_destroy(struct gbm_bo *).
            destroy_bo: unsafe { function(library, c"gbm_bo_destroy")? },
            // SAFETY: gbm.h: int gbm_bo_get_plane_count(struct gbm_bo *).
            plane_count: unsafe { function(library, c"gbm_bo_get_plane_count")? },
            // SAFETY: gbm.h: int gbm_bo_get_fd_for_plane(struct gbm_bo *, int plane).
            fd_for_plane: unsafe { function(library, c"gbm_bo_get_fd_for_plane")? },
            // SAFETY: gbm.h: uint32_t gbm_bo_get_offset(struct gbm_bo *, int plane).
            offset: unsafe { function(library, c"gbm_bo_get_offset")? },
            // SAFETY: gbm.h: uint32_t gbm_bo_get_stride_for_plane(struct gbm_bo *, int plane).
            stride: unsafe { function(library, c"gbm_bo_get_stride_for_plane")? },
            // SAFETY: gbm.h: uint64_t gbm_bo_get_modifier(struct gbm_bo *).
            modifier: unsafe { function(library, c"gbm_bo_get_modifier")? },
            library,
        })
    }
}

/// The function `name` of `library` as `T`.
///
/// # Safety
///
/// `T` is the function pointer type of the C signature the library declares for `name`.
unsafe fn function<T: Copy>(library: *mut c_void, name: &CStr) -> Result<T, String> {
    // SAFETY: `library` is a handle from dlopen and `name` is NUL-terminated.
    let address = unsafe { libc::dlsym(library, name.as_ptr()) };
    if address.is_null() {
        return Err(format!("libgbm has no {}", name.to_string_lossy()));
    }
    assert_eq!(size_of::<T>(), size_of::<*mut c_void>(), "a function pointer");
    // SAFETY: the caller guarantees `T` is the symbol's function pointer type, as large as a data pointer (asserted).
    Ok(unsafe { std::mem::transmute_copy::<*mut c_void, T>(&address) })
}

/// A buffer object GBM allocated on a render node with one of a device's modifiers, exported plane by plane.
pub struct GbmBuffer {
    pub buffer: Arc<DmaBuffer>,
}

impl GbmBuffer {
    /// A `width` x `height` `fourcc` buffer on `node` with one of `modifiers`.
    pub fn allocate(node: &Path, width: u32, height: u32, fourcc: u32, modifiers: &[u64]) -> Result<Self, String> {
        let gbm = Gbm::load()?;
        let node = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(node)
            .map_err(|error| format!("{}: {error}", node.display()))?;
        let count = u32::try_from(modifiers.len()).map_err(|_| "too many modifiers")?;
        // SAFETY: the fd is an open render node, kept open until the device is destroyed below.
        let device = unsafe { (gbm.create_device)(node.as_raw_fd()) };
        if device.is_null() {
            return Err("gbm_create_device failed".to_owned());
        }
        // SAFETY: `device` is live, `modifiers` holds `count` values for the call.
        let bo = unsafe {
            (gbm.create_with_modifiers2)(
                device,
                width,
                height,
                fourcc,
                modifiers.as_ptr(),
                count,
                GBM_BO_USE_RENDERING,
            )
        };
        let exported = if bo.is_null() {
            Err("gbm_bo_create_with_modifiers2 failed".to_owned())
        } else {
            let exported = export(&gbm, bo, width, height, fourcc);
            // SAFETY: `bo` is live and owned here; the exported fds keep the memory.
            unsafe { (gbm.destroy_bo)(bo) };
            exported
        };
        // SAFETY: `device` is live and has no buffer objects left.
        unsafe { (gbm.destroy_device)(device) };
        // SAFETY: the library handle came from dlopen; nothing of it is used after this.
        unsafe { libc::dlclose(gbm.library) };
        Ok(Self { buffer: exported? })
    }
}

fn export(gbm: &Gbm, bo: *mut c_void, width: u32, height: u32, fourcc: u32) -> Result<Arc<DmaBuffer>, String> {
    // SAFETY: `bo` is a live buffer object.
    let planes = unsafe { (gbm.plane_count)(bo) };
    // SAFETY: as above.
    let modifier = unsafe { (gbm.modifier)(bo) };
    let mut exported = Vec::new();
    for plane in 0..planes {
        // SAFETY: `bo` is live and `plane` below its plane count.
        let fd = unsafe { (gbm.fd_for_plane)(bo, plane) };
        if fd < 0 {
            return Err(format!("gbm_bo_get_fd_for_plane({plane}) failed"));
        }
        exported.push(DmaBufPlane {
            // SAFETY: the call returned a new fd this process owns.
            fd: unsafe { OwnedFd::from_raw_fd(fd) },
            // SAFETY: `bo` is live and `plane` below its plane count.
            offset: unsafe { (gbm.offset)(bo, plane) },
            // SAFETY: as above.
            stride: unsafe { (gbm.stride)(bo, plane) },
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
