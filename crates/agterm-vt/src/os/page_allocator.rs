//! Page memory straight from the OS: page-aligned, zeroed and committed lazily, owned by a [`PageMemory`].
//!
//! Ghostty: `src/terminal/page.zig` (`PageAlloc`: `AllocPosix`, `AllocWindows`) and `src/terminal/PageList.zig`
//! (`pageAllocator`, the macOS-tagged `src/os/mach.zig` `taggedPageAllocator`) at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-os/src/page_allocator.rs`, `crates/rustty-os/src/mach.rs` and the `page_alloc` module of
//! `crates/rustty-vt/src/page.rs` at `RUSTTY_REV` (MIT).
//! Changes: one allocator for both of Ghostty's users, returning an owned mapping that unmaps itself on drop instead
//! of a raw slice the caller must free. Its forms, all in this file:
//! - Linux and the other Unix systems: `mmap(MAP_PRIVATE | MAP_ANONYMOUS)` / `munmap`;
//! - macOS: the same mapping tagged `VM_MEMORY_APPLICATION_SPECIFIC_1` (the tag goes in `mmap`'s file descriptor),
//!   so the terminal's pages show as its own in vmmap and Instruments, as Ghostty tags them;
//! - Windows: `VirtualAlloc(MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE)` / `VirtualFree(MEM_RELEASE)`, as Ghostty's
//!   `AllocWindows` (rustty's `NtAllocateVirtualMemory` placeholder dance served alignments beyond the allocation
//!   granularity, which no caller asks for: page memory needs the page size at most);
//! - Miri: the zeroed heap at page alignment, since Miri cannot map pages.
//!
//! All of them hand out zeroed memory aligned to at least [`page_size`], which page layouts rely on.

use core::ptr::NonNull;

use crate::mem::alloc::OutOfMemory;

/// The smallest page size of every supported target (`std.heap.page_size_min` on x86-64 and aarch64 Linux and
/// Windows; 16 KiB on aarch64 macOS, which is larger): every mapping is aligned to at least this.
pub(crate) const PAGE_SIZE_MIN: usize = 4096;

/// The OS page size: every mapping starts on a page boundary and covers whole pages.
pub(crate) fn page_size() -> usize {
    sys::page_size()
}

/// Zeroed, page-aligned memory mapped from the OS, unmapped on drop. It derefs to its bytes.
pub(crate) struct PageMemory {
    ptr: NonNull<u8>,
    len: usize,
}

// SAFETY: a `PageMemory` owns its mapping exclusively, like a `Box<[u8]>`; nothing else refers to it, so moving it to
// another thread or sharing `&PageMemory` (read-only bytes) between threads is sound.
unsafe impl Send for PageMemory {}
// SAFETY: as above.
unsafe impl Sync for PageMemory {}

impl PageMemory {
    /// `len` zeroed bytes in a mapping of their own, page-aligned. A zero `len` maps nothing.
    pub(crate) fn alloc(len: usize) -> Result<Self, OutOfMemory> {
        #[cfg(test)]
        if crate::mem::testing::refuse_page() {
            return Err(OutOfMemory);
        }
        if len == 0 {
            return Ok(Self {
                ptr: NonNull::dangling(),
                len: 0,
            });
        }
        if len > isize::MAX.unsigned_abs() - page_size() {
            return Err(OutOfMemory);
        }
        let ptr = sys::map(len).ok_or(OutOfMemory)?;
        Ok(Self { ptr, len })
    }

    /// The length in bytes.
    pub(crate) const fn len(&self) -> usize {
        self.len
    }

    /// Whether the mapping is empty.
    pub(crate) const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The bytes.
    pub(crate) fn as_bytes(&self) -> &[u8] {
        // SAFETY: `ptr` is a live mapping of `len` readable bytes (or dangling with `len == 0`), initialized because
        // the OS zeroes fresh pages, owned by `self` and only written through `&mut self`, so no `&mut` aliases it.
        unsafe { core::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }

    /// The bytes, mutable.
    pub(crate) fn as_bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: as in `as_bytes`, and `&mut self` makes this the only reference to the mapping.
        unsafe { core::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

impl core::ops::Deref for PageMemory {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl core::ops::DerefMut for PageMemory {
    fn deref_mut(&mut self) -> &mut [u8] {
        self.as_bytes_mut()
    }
}

impl Drop for PageMemory {
    fn drop(&mut self) {
        if self.len != 0 {
            sys::unmap(self.ptr, self.len);
        }
    }
}

impl core::fmt::Debug for PageMemory {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PageMemory")
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

/// Miri: the zeroed heap, page-aligned. Miri cannot run `mmap` or `VirtualAlloc`.
#[cfg(miri)]
mod sys {
    use core::alloc::Layout;
    use core::ptr::NonNull;

    use super::PAGE_SIZE_MIN;

    pub(super) fn page_size() -> usize {
        PAGE_SIZE_MIN
    }

    /// The layout a mapping of `len` bytes is allocated and freed with: whole pages, page-aligned.
    fn layout(len: usize) -> Option<Layout> {
        Layout::from_size_align(len.next_multiple_of(PAGE_SIZE_MIN), PAGE_SIZE_MIN).ok()
    }

    pub(super) fn map(len: usize) -> Option<NonNull<u8>> {
        // SAFETY: the layout's size is non-zero (`len > 0`, rounded up to a whole page).
        NonNull::new(unsafe { std::alloc::alloc_zeroed(layout(len)?) })
    }

    pub(super) fn unmap(ptr: NonNull<u8>, len: usize) {
        if let Some(layout) = layout(len) {
            // SAFETY: `ptr` came from `map(len)`, which allocated it with this same layout.
            unsafe { std::alloc::dealloc(ptr.as_ptr(), layout) };
        }
    }
}

/// Unix: anonymous private mappings, tagged on macOS.
#[cfg(all(unix, not(miri)))]
mod sys {
    use core::ptr::NonNull;

    /// The file descriptor argument of an anonymous mapping: on macOS it carries the VM tag that attributes the
    /// pages to the terminal (`VM_MAKE_TAG(VM_MEMORY_APPLICATION_SPECIFIC_1)`, `mach.zig`'s `application_specific_1`).
    #[cfg(target_os = "macos")]
    const MAP_FD: libc::c_int = (240u32 << 24).cast_signed();
    /// The file descriptor argument of an anonymous mapping: none.
    #[cfg(not(target_os = "macos"))]
    const MAP_FD: libc::c_int = -1;

    pub(super) fn page_size() -> usize {
        // SAFETY: `sysconf` with a valid name has no preconditions.
        let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        usize::try_from(size).unwrap_or(super::PAGE_SIZE_MIN)
    }

    pub(super) fn map(len: usize) -> Option<NonNull<u8>> {
        // SAFETY: an anonymous private mapping at an address of the kernel's choosing touches no existing memory; the
        // arguments are valid for `mmap`.
        let ptr = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                MAP_FD,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return None;
        }
        NonNull::new(ptr.cast::<u8>())
    }

    pub(super) fn unmap(ptr: NonNull<u8>, len: usize) {
        // SAFETY: `ptr` and `len` are a whole mapping `map` returned, and its owner is gone, so nothing refers to it.
        let unmapped = unsafe { libc::munmap(ptr.as_ptr().cast(), len) };
        debug_assert_eq!(unmapped, 0, "munmap of a mapping we own cannot fail");
    }
}

/// Windows: committed, reserved read-write pages.
#[cfg(all(windows, not(miri)))]
mod sys {
    use core::ptr::NonNull;

    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc, VirtualFree,
    };

    pub(super) fn page_size() -> usize {
        // `std.heap.pageSize()` folds to 4 KiB on every Windows target Zig supports (`page_size_min ==
        // page_size_max`); `VirtualAlloc` hands out regions aligned to the 64 KiB allocation granularity anyway.
        super::PAGE_SIZE_MIN
    }

    pub(super) fn map(len: usize) -> Option<NonNull<u8>> {
        // SAFETY: a fresh region at an address of the system's choosing touches no existing memory; the arguments are
        // valid for `VirtualAlloc`.
        let ptr = unsafe { VirtualAlloc(core::ptr::null(), len, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE) };
        NonNull::new(ptr.cast::<u8>())
    }

    pub(super) fn unmap(ptr: NonNull<u8>, _len: usize) {
        // SAFETY: `ptr` is the base of a region `map` reserved, and its owner is gone, so nothing refers to it; a
        // release takes size 0.
        let released = unsafe { VirtualFree(ptr.as_ptr().cast(), 0, MEM_RELEASE) };
        debug_assert_ne!(released, 0, "VirtualFree of a region we own cannot fail");
    }
}

#[cfg(test)]
mod tests {
    use super::{PAGE_SIZE_MIN, PageMemory, page_size};
    use crate::mem::alloc::OutOfMemory;
    use crate::mem::testing::with_failing_pages;

    #[test]
    fn a_page_is_aligned_zeroed_writable_and_freed() {
        let page = page_size();
        assert!(page >= PAGE_SIZE_MIN);
        assert!(page.is_power_of_two());
        let mut mem = PageMemory::alloc(page).unwrap();
        assert_eq!(mem.len(), page);
        assert!(!mem.is_empty());
        assert_eq!(mem.as_ptr().addr() % page, 0, "page-aligned");
        assert!(mem.iter().all(|&b| b == 0), "a fresh page reads as zeroes");
        mem[0] = 0xAB;
        mem[page - 1] = 0xCD;
        assert_eq!((mem[0], mem[page - 1]), (0xAB, 0xCD));
        drop(mem);
    }

    #[test]
    fn an_odd_length_rounds_to_pages_but_reports_its_own() {
        let mut mem = PageMemory::alloc(3 * PAGE_SIZE_MIN + 17).unwrap();
        assert_eq!(mem.len(), 3 * PAGE_SIZE_MIN + 17);
        assert_eq!(mem.as_ptr().addr() % PAGE_SIZE_MIN, 0);
        let len = mem.len();
        mem.as_bytes_mut()[len - 1] = 1;
        assert_eq!(mem.as_bytes().iter().map(|&b| usize::from(b)).sum::<usize>(), 1);
    }

    #[test]
    fn an_empty_mapping_maps_nothing() {
        let mem = PageMemory::alloc(0).unwrap();
        assert!(mem.is_empty());
        assert!(mem.as_bytes().is_empty());
        assert_eq!(format!("{mem:?}"), "PageMemory { len: 0, .. }");
    }

    #[test]
    fn an_impossible_length_is_refused() {
        assert!(matches!(PageMemory::alloc(usize::MAX), Err(OutOfMemory)));
    }

    #[test]
    fn page_failures_are_injected() {
        let results = with_failing_pages(1, || [PageMemory::alloc(16).is_ok(), PageMemory::alloc(16).is_ok()]);
        assert_eq!(results, [true, false]);
        assert!(PageMemory::alloc(16).is_ok());
    }

    #[test]
    fn page_memory_moves_between_threads() {
        let mut mem = PageMemory::alloc(PAGE_SIZE_MIN).unwrap();
        mem[5] = 5;
        let mem = std::thread::spawn(move || {
            mem[6] = 6;
            mem
        })
        .join()
        .unwrap();
        assert_eq!((mem[5], mem[6]), (5, 6));
    }
}
