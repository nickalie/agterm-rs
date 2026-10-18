//! Heap allocation that can fail: Zig's `try` growth over the standard containers.
//!
//! Ghostty: none of its own; this is the part of Zig's `std.mem.Allocator` and `std.ArrayList` the terminal relies on
//! (allocation failure is an error the caller handles, not an abort).
//! rustty: `crates/rustty-vt/src/alloc.rs` and `crates/rustty-vt/src/zig_list.rs` at `RUSTTY_REV` (MIT).
//! Changes: rustty carries Zig's allocator as a runtime handle (`VtAllocator`: the heap or a C embedder's vtable)
//! through allocator-api2 containers, with Zig's growth policy and allocation traces. agterm is the only embedder
//! and libghostty-vt was always given the heap, so the handle, the C ABI, allocator-api2 and the traces are gone:
//! containers are `std`'s, Zig's `try` growth is `std`'s `try_reserve*` behind [`TryVecExt`] and [`try_dupe`], and the
//! one fallible construction `std` has no stable form for is [`try_box`]. Failure injection for tests moved from the
//! handle to a test-only global allocator (`mem::testing`). The kitty PNG decode seam (`LimitedAllocator`, the raw
//! allocator interface) is not here: the decoder that replaces rustty's takes no allocator and caps itself.

use core::alloc::Layout;
use core::ptr::NonNull;
use std::collections::TryReserveError;

/// An allocation failed (`error.OutOfMemory`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OutOfMemory;

impl core::fmt::Display for OutOfMemory {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("out of memory")
    }
}

impl std::error::Error for OutOfMemory {}

impl From<TryReserveError> for OutOfMemory {
    fn from(_: TryReserveError) -> Self {
        Self
    }
}

/// `try alloc.create(T)` and its initialization: `value` in a fresh heap box, or [`OutOfMemory`] (with `value`
/// dropped) when the heap refuses.
pub(crate) fn try_box<T>(value: T) -> Result<Box<T>, OutOfMemory> {
    let layout = Layout::new::<T>();
    if layout.size() == 0 {
        return Ok(Box::new(value));
    }
    // SAFETY: `layout` has a non-zero size (checked above).
    let raw = unsafe { std::alloc::alloc(layout) };
    let ptr = NonNull::new(raw.cast::<T>()).ok_or(OutOfMemory)?;
    // SAFETY: `ptr` is a fresh, live allocation of `T`'s layout: valid and aligned for writing one `T`.
    unsafe { ptr.write(value) };
    // SAFETY: `ptr` was allocated by the global allocator with `T`'s layout and holds an initialized `T`, which is
    // what `Box::from_raw` requires; the box frees it with the same layout.
    Ok(unsafe { Box::from_raw(ptr.as_ptr()) })
}

/// `try alloc.dupe(T, src)`: one exact-sized, fallible copy of `src`. An empty `src` allocates nothing.
pub(crate) fn try_dupe<T: Copy>(src: &[T]) -> Result<Vec<T>, OutOfMemory> {
    let mut copy = Vec::new();
    copy.try_reserve_exact(src.len())?;
    copy.extend_from_slice(src);
    Ok(copy)
}

/// Zig's fallible list growth (`try list.append(..)`, `appendSlice`, `appendNTimes`) on `std`'s `Vec`: each fails
/// with [`OutOfMemory`] and leaves the vector unchanged instead of aborting.
pub(crate) trait TryVecExt<T> {
    /// `append`.
    fn try_push(&mut self, value: T) -> Result<(), OutOfMemory>;

    /// `appendSlice`.
    fn try_extend_copy(&mut self, src: &[T]) -> Result<(), OutOfMemory>
    where
        T: Copy;

    /// `resize` (`appendNTimes` when growing): `new_len` elements, new ones set to `value`.
    fn try_resize_copy(&mut self, new_len: usize, value: T) -> Result<(), OutOfMemory>
    where
        T: Copy;

    /// `ensureTotalCapacity`: room for `capacity` elements in all.
    fn try_ensure_total_capacity(&mut self, capacity: usize) -> Result<(), OutOfMemory>;

    /// `ensureUnusedCapacity`: room for `additional` more elements.
    fn try_ensure_unused_capacity(&mut self, additional: usize) -> Result<(), OutOfMemory>;
}

impl<T> TryVecExt<T> for Vec<T> {
    fn try_push(&mut self, value: T) -> Result<(), OutOfMemory> {
        self.try_reserve(1)?;
        self.push(value);
        Ok(())
    }

    fn try_extend_copy(&mut self, src: &[T]) -> Result<(), OutOfMemory>
    where
        T: Copy,
    {
        self.try_reserve(src.len())?;
        self.extend_from_slice(src);
        Ok(())
    }

    fn try_resize_copy(&mut self, new_len: usize, value: T) -> Result<(), OutOfMemory>
    where
        T: Copy,
    {
        if let Some(additional) = new_len.checked_sub(self.len()) {
            self.try_reserve(additional)?;
        }
        self.resize(new_len, value);
        Ok(())
    }

    fn try_ensure_total_capacity(&mut self, capacity: usize) -> Result<(), OutOfMemory> {
        self.try_reserve(capacity.saturating_sub(self.len()))?;
        Ok(())
    }

    fn try_ensure_unused_capacity(&mut self, additional: usize) -> Result<(), OutOfMemory> {
        self.try_reserve(additional)?;
        Ok(())
    }
}

/// `std.Io.Writer.Allocating`: a writer into a growing byte buffer whose growth fails with an error instead of
/// aborting, so formatting into it reports out-of-memory as the reference's `error.WriteFailed` does.
#[derive(Debug, Default)]
pub(crate) struct AllocatingWriter {
    buf: Vec<u8>,
}

impl AllocatingWriter {
    /// An empty writer; allocates nothing until the first write.
    pub(crate) const fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// The bytes written so far.
    pub(crate) fn written(&self) -> &[u8] {
        &self.buf
    }

    /// The bytes written so far, leaving the writer empty (`toOwnedSlice`).
    pub(crate) fn take(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.buf)
    }

    /// Writes one byte.
    pub(crate) fn write_byte(&mut self, byte: u8) -> Result<(), OutOfMemory> {
        self.buf.try_push(byte)
    }
}

impl std::io::Write for AllocatingWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.buf
            .try_extend_copy(bytes)
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::OutOfMemory))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::{AllocatingWriter, OutOfMemory, TryVecExt, try_box, try_dupe};
    use crate::mem::testing::{with_counting, with_failing};

    #[test]
    fn try_box_holds_the_value_and_fails_cleanly() {
        let boxed = try_box([7u64; 4]).unwrap();
        assert_eq!(*boxed, [7; 4]);
        assert_eq!(*try_box(()).unwrap(), ());
        let (result, stats) = with_failing(0, || try_box(5u32));
        assert_eq!(result, Err(OutOfMemory));
        assert!(stats.has_induced_failure);
        assert_eq!(stats.allocations, 0);
    }

    #[test]
    fn a_zero_sized_box_allocates_nothing() {
        let (result, stats) = with_failing(0, || try_box(()));
        assert!(result.is_ok());
        assert!(!stats.has_induced_failure);
    }

    #[test]
    fn try_box_drops_the_value_when_the_heap_refuses() {
        let value = vec![1u8, 2, 3];
        let (result, _) = with_failing(0, || try_box(value));
        assert_eq!(result.err(), Some(OutOfMemory));
    }

    #[test]
    fn try_dupe_copies_exactly() {
        let copy = try_dupe(b"hello").unwrap();
        assert_eq!(copy, b"hello");
        assert_eq!(copy.capacity(), 5);
        let (empty, stats) = with_failing(0, || try_dupe::<u8>(&[]));
        assert_eq!(empty, Ok(Vec::new()));
        assert!(!stats.has_induced_failure, "an empty copy allocates nothing");
        let (failed, _) = with_failing(0, || try_dupe(b"x"));
        assert_eq!(failed, Err(OutOfMemory));
    }

    #[test]
    fn fallible_growth_succeeds_like_the_std_methods() {
        let mut v: Vec<u16> = Vec::new();
        v.try_push(1).unwrap();
        v.try_extend_copy(&[2, 3]).unwrap();
        v.try_resize_copy(5, 9).unwrap();
        assert_eq!(v, [1, 2, 3, 9, 9]);
        v.try_resize_copy(2, 0).unwrap();
        assert_eq!(v, [1, 2]);
        v.try_ensure_total_capacity(10).unwrap();
        assert!(v.capacity() >= 10);
        v.try_ensure_unused_capacity(20).unwrap();
        assert!(v.capacity() >= 22);
    }

    #[test]
    fn fallible_growth_fails_and_leaves_the_vector_unchanged() {
        let mut v: Vec<u8> = vec![1, 2, 3];
        v.shrink_to_fit();
        let (results, stats) = with_failing(0, || {
            [
                v.try_push(4),
                v.try_extend_copy(&[4, 5]),
                v.try_resize_copy(10, 0),
                v.try_ensure_total_capacity(100),
                v.try_ensure_unused_capacity(100),
            ]
        });
        assert_eq!(results, [Err(OutOfMemory); 5]);
        assert!(stats.has_induced_failure);
        assert_eq!(v, [1, 2, 3]);
        // growth within capacity needs no allocation
        let (shrunk, _) = with_failing(0, || v.try_resize_copy(1, 0));
        assert_eq!(shrunk, Ok(()));
        assert_eq!(v, [1]);
    }

    #[test]
    fn the_writer_collects_bytes_and_reports_oom() {
        let mut w = AllocatingWriter::new();
        let middle = 'b';
        write!(w, "a{middle}c").unwrap();
        w.write_byte(b'!').unwrap();
        w.flush().unwrap();
        assert_eq!(w.written(), b"abc!");
        assert_eq!(w.take(), b"abc!");
        assert!(w.written().is_empty());

        let (result, _) = with_failing(0, || w.write_all(b"too much"));
        assert_eq!(result.map_err(|e| e.kind()), Err(std::io::ErrorKind::OutOfMemory));
        let (byte, _) = with_failing(0, || w.write_byte(b'x'));
        assert_eq!(byte, Err(OutOfMemory));
        assert!(w.written().is_empty());
    }

    #[test]
    fn the_writer_frees_what_it_took() {
        let ((), counts) = with_counting(|| {
            let mut w = AllocatingWriter::default();
            w.write_all(&[0; 100]).unwrap();
            drop(w.take());
        });
        assert_eq!(counts.live_bytes, 0);
        assert!(counts.alloc_calls >= 1);
        assert_eq!(OutOfMemory.to_string(), "out of memory");
    }
}
