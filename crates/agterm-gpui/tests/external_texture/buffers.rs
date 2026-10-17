//! Synthetic linear dmabufs: `udmabuf` over a sealed memfd, CPU memory the test writes and any Mesa Vulkan driver
//! imports.

use std::fs::File;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::fs::FileExt as _;
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
