//! Reading a pixel back from a linear dmabuf: mapped read-only around `DMA_BUF_IOCTL_SYNC`, which waits for the
//! GPU's writes (the headless display uses implicit sync).

use std::os::fd::AsRawFd;

use ngterm_wpe::{DmaBuf, MODIFIER_LINEAR};

/// `_IOW('b', 0, struct dma_buf_sync)`.
const DMA_BUF_IOCTL_SYNC: libc::c_ulong = 0x4008_6200;
const DMA_BUF_SYNC_READ: u64 = 1;
const DMA_BUF_SYNC_START: u64 = 0;
const DMA_BUF_SYNC_END: u64 = 4;

fn sync(fd: libc::c_int, flags: u64) -> Result<(), String> {
    let mut flags = flags;
    // SAFETY: a dmabuf fd and a `struct dma_buf_sync`, which is one u64.
    let result = unsafe { libc::ioctl(fd, DMA_BUF_IOCTL_SYNC, &raw mut flags) };
    if result == 0 {
        Ok(())
    } else {
        Err(format!("DMA_BUF_IOCTL_SYNC: {}", std::io::Error::last_os_error()))
    }
}

/// The (red, green, blue) at (`x`, `y`) of a linear 32-bit buffer (XRGB/ARGB8888: B G R X in memory).
pub fn pixel(buffer: &DmaBuf, x: u32, y: u32) -> Result<(u8, u8, u8), String> {
    if buffer.modifier != MODIFIER_LINEAR {
        return Err(format!("a modifier the test cannot read: {:#x}", buffer.modifier));
    }
    let plane = buffer.planes.first().ok_or("a dmabuf without planes")?;
    let fd = plane.fd.as_raw_fd();
    let start = plane.offset as usize + y as usize * plane.stride as usize + x as usize * 4;
    let length = plane.offset as usize + plane.stride as usize * buffer.height as usize;
    if start + 4 > length {
        return Err("the pixel lies outside the buffer".to_owned());
    }
    // SAFETY: maps `length` bytes of the dmabuf read-only; the mapping is unmapped below.
    let map = unsafe { libc::mmap(std::ptr::null_mut(), length, libc::PROT_READ, libc::MAP_SHARED, fd, 0) };
    if map == libc::MAP_FAILED {
        return Err(format!("mmap: {}", std::io::Error::last_os_error()));
    }
    let read = sync(fd, DMA_BUF_SYNC_START | DMA_BUF_SYNC_READ).map(|()| {
        // SAFETY: `start + 4 <= length`, the mapped size.
        let at = unsafe { map.cast::<u8>().add(start) };
        // SAFETY: the 4 bytes at `at` are mapped and readable until the unmap below.
        let bytes = unsafe { std::slice::from_raw_parts(at, 4) };
        (bytes[2], bytes[1], bytes[0])
    });
    let ended = sync(fd, DMA_BUF_SYNC_END | DMA_BUF_SYNC_READ);
    // SAFETY: unmaps the mapping made above.
    unsafe { libc::munmap(map, length) };
    let color = read?;
    ended?;
    Ok(color)
}
