//! A bitmap allocator over a region of a page's buffer, addressed by offsets (a page's grapheme and string bytes).
//!
//! Ghostty: `src/terminal/bitmap_allocator.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/bitmap_allocator.rs` at `RUSTTY_REV` (MIT).
//! Changes: the methods take the page's bytes instead of a base pointer and are safe. `alloc` hands back an
//! [`Allocation`], a non-`Copy` handle that `free` consumes, so its holder cannot free it twice; a page record that
//! must stay `Copy` keeps the [`OffsetSlice`] and frees it with [`BitmapAllocator::free_slice`], which refuses a slice
//! outside the chunk region or with a free chunk in it (it cannot tell a stale offset whose chunks were handed out
//! again). The allocated values are `Pod` and read through checked views. A run longer than one word whose free
//! bits reach the end of the last word is "not found" instead of an index past the bitmaps. `dumpBitmaps` (debug
//! logging) is not ported.

use core::marker::PhantomData;

use bytemuck::Pod;

use crate::mem::offset::{Offset, OffsetBuf, OffsetSlice, view, view_mut};

/// The allocator ran out of chunks for a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OutOfMemory;

/// [`BitmapAllocator::free_slice`] refused a slice: it lies outside the chunk region, is not chunk-aligned, or covers
/// a chunk that is free.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NotAllocated;

/// The memory layout of a [`BitmapAllocator`] of some capacity: the bitmaps, then the chunks they address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Layout {
    pub(crate) total_size: usize,
    pub(crate) bitmap_count: usize,
    pub(crate) bitmap_start: usize,
    pub(crate) chunks_start: usize,
}

/// An allocation of `len` values of `T`: the only handle [`BitmapAllocator::free`] takes, consumed by it.
#[must_use = "an allocation that is dropped is never freed"]
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Allocation<T> {
    slice: OffsetSlice<T>,
}

impl<T> Allocation<T> {
    /// Where the allocation is, as a page record stores it.
    pub(crate) const fn as_offset_slice(&self) -> OffsetSlice<T> {
        self.slice
    }

    /// Gives up the handle for the plain offset slice a page record keeps; it is freed later with
    /// [`BitmapAllocator::free_slice`].
    pub(crate) const fn into_offset_slice(self) -> OffsetSlice<T> {
        self.slice
    }

    /// The allocated values.
    pub(crate) fn slice<'a>(&self, mem: &'a [u8]) -> &'a [T]
    where
        T: Pod,
    {
        self.slice.slice(mem)
    }

    /// The allocated values, mutable.
    pub(crate) fn slice_mut<'a>(&self, mem: &'a mut [u8]) -> &'a mut [T]
    where
        T: Pod,
    {
        self.slice.slice_mut(mem)
    }
}

/// A relatively naive bitmap allocator that uses offsets against a fixed backing buffer, so the buffer can be moved
/// without updating pointers.
///
/// The chunk size is the minimum distributed unit of memory: a 1-byte request takes a whole chunk, and with 4-byte
/// chunks a 5-byte request takes two. The allocator is susceptible to fragmentation: freeing in a way that leaves
/// small holes can make a large request fail even when enough memory is free in aggregate. A chunk size covering
/// most requests avoids that.
///
/// The descriptor is `Copy`, as a page's clone copies it along with the buffer.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BitmapAllocator<const CHUNK_SIZE: usize> {
    /// The bitmaps of the chunks, one bit per chunk: 0 is free, 1 is used, so an all-zero bitmap is a fully free
    /// allocator.
    bitmap: Offset<u64>,
    bitmap_count: usize,
    /// The lowest bitmap word that may have a free bit; the words below it are full, so a search starts here.
    search_start: usize,
    /// The contiguous chunks.
    chunks: Offset<u8>,
    _chunk: PhantomData<[u8; CHUNK_SIZE]>,
}

impl<const CHUNK_SIZE: usize> BitmapAllocator<CHUNK_SIZE> {
    const CHUNK_SIZE_IS_A_POWER_OF_TWO: () = assert!(CHUNK_SIZE.is_power_of_two());

    /// The alignment the buffer must have where the allocator starts.
    pub(crate) const BASE_ALIGN: usize = align_of::<u64>();

    /// Bits in one bitmap word.
    pub(crate) const BITMAP_BIT_SIZE: usize = u64::BITS as usize;

    /// An allocator over the memory `buf` starts at, laid out by `layout`, with every chunk marked free.
    pub(crate) fn init(mem: &mut [u8], buf: OffsetBuf, layout: Layout) -> Self {
        let allocator = Self::init_assume_zeroed(buf, layout);
        allocator.bitmaps_mut(mem).fill(0);
        allocator
    }

    /// An allocator over memory the caller knows is zeroed (fresh OS pages): it writes nothing, since an all-zero
    /// bitmap already marks every chunk free.
    pub(crate) fn init_assume_zeroed(buf: OffsetBuf, layout: Layout) -> Self {
        let () = Self::CHUNK_SIZE_IS_A_POWER_OF_TWO;
        debug_assert!(buf.start().is_multiple_of(Self::BASE_ALIGN));
        Self {
            bitmap: buf.member(layout.bitmap_start),
            bitmap_count: layout.bitmap_count,
            search_start: 0,
            chunks: buf.member(layout.chunks_start),
            _chunk: PhantomData,
        }
    }

    /// The bytes `n` values of `T` take, rounded up to whole chunks.
    pub(crate) const fn bytes_required<T>(n: usize) -> usize {
        (size_of::<T>() * n).next_multiple_of(CHUNK_SIZE)
    }

    fn bitmaps(self, mem: &[u8]) -> &[u64] {
        view(mem, self.bitmap.index(), self.bitmap_count)
    }

    fn bitmaps_mut(self, mem: &mut [u8]) -> &mut [u64] {
        view_mut(mem, self.bitmap.index(), self.bitmap_count)
    }

    /// The number of chunks the bitmaps address.
    const fn chunk_count(self) -> usize {
        self.bitmap_count * Self::BITMAP_BIT_SIZE
    }

    /// Allocates `n` values of `T`, or [`OutOfMemory`] when no run of free chunks is long enough.
    ///
    /// The chunk size must be a multiple of `T`'s alignment (the allocator does not align beyond its chunks), and `n`
    /// must not be zero.
    pub(crate) fn alloc<T: Pod>(&mut self, mem: &mut [u8], n: usize) -> Result<Allocation<T>, OutOfMemory> {
        debug_assert!(CHUNK_SIZE.is_multiple_of(align_of::<T>()));
        debug_assert!(n > 0);
        let byte_count = size_of::<T>().checked_mul(n).ok_or(OutOfMemory)?;
        let chunk_count = byte_count.div_ceil(CHUNK_SIZE);

        // Words below `search_start` have no free bit, so no free run can start in or cross them.
        let bitmaps = self.bitmaps_mut(mem);
        let start = self.search_start.min(bitmaps.len());
        let relative = find_free_chunks(&mut bitmaps[start..], chunk_count).ok_or(OutOfMemory)?;
        let index = start * Self::BITMAP_BIT_SIZE + relative;

        // Advance past the words the allocation just filled, so the next search starts at a word with a free bit.
        let full = bitmaps[start..].iter().take_while(|&&word| word == u64::MAX).count();
        self.search_start = start + full;

        let offset = Offset::from_layout(self.chunks.index() + index * CHUNK_SIZE);
        Ok(Allocation {
            slice: OffsetSlice::new(offset, n),
        })
    }

    /// Frees an allocation of this allocator.
    pub(crate) fn free<T>(&mut self, mem: &mut [u8], allocation: Allocation<T>) {
        let freed = self.free_slice(mem, allocation.into_offset_slice());
        debug_assert_eq!(
            freed,
            Ok(()),
            "an allocation handle frees what its allocator handed out"
        );
    }

    /// Frees the chunks an offset slice from [`alloc`](Self::alloc) covers, as a page record keeps it. Refused,
    /// with nothing changed, when the slice is outside the chunk region, does not start on a chunk, or covers a free
    /// chunk (a double free of chunks not handed out again since).
    pub(crate) fn free_slice<T>(&mut self, mem: &mut [u8], slice: OffsetSlice<T>) -> Result<(), NotAllocated> {
        let (chunk_index, chunk_count) = self.chunks_of(slice).ok_or(NotAllocated)?;
        if !self.chunks_allocated(mem, chunk_index, chunk_count) {
            return Err(NotAllocated);
        }

        // The freed word gains free bits, so searches must not skip it.
        self.search_start = self.search_start.min(chunk_index / Self::BITMAP_BIT_SIZE);
        clear_bits(self.bitmaps_mut(mem), chunk_index, chunk_count);
        Ok(())
    }

    /// The first chunk and the chunk count of `slice`, `None` when it is not a chunk-aligned run inside the region.
    fn chunks_of<T>(self, slice: OffsetSlice<T>) -> Option<(usize, usize)> {
        let bytes = size_of::<T>().checked_mul(slice.len())?;
        let chunk_count = bytes.div_ceil(CHUNK_SIZE);
        let relative = slice.offset().index().checked_sub(self.chunks.index())?;
        if !relative.is_multiple_of(CHUNK_SIZE) || chunk_count == 0 {
            return None;
        }
        let chunk_index = relative / CHUNK_SIZE;
        (chunk_index.checked_add(chunk_count)? <= self.chunk_count()).then_some((chunk_index, chunk_count))
    }

    /// Whether every chunk of the run is marked used.
    fn chunks_allocated(self, mem: &[u8], chunk_index: usize, chunk_count: usize) -> bool {
        let bitmaps = self.bitmaps(mem);
        (chunk_index..chunk_index + chunk_count).all(|chunk| {
            let bit = chunk % Self::BITMAP_BIT_SIZE;
            bitmaps[chunk / Self::BITMAP_BIT_SIZE] & (1 << bit) != 0
        })
    }

    /// Whether every chunk `slice` covers is allocated (and the slice is a chunk-aligned run inside the region).
    pub(crate) fn is_allocated<T>(self, mem: &[u8], slice: OffsetSlice<T>) -> bool {
        self.chunks_of(slice)
            .is_some_and(|(index, count)| self.chunks_allocated(mem, index, count))
    }

    /// The total capacity in bytes.
    pub(crate) const fn capacity_bytes(self) -> usize {
        self.chunk_count() * CHUNK_SIZE
    }

    /// The bytes currently in use.
    pub(crate) fn used_bytes(self, mem: &[u8]) -> usize {
        let used_chunks: u32 = self.bitmaps(mem).iter().map(|word| word.count_ones()).sum();
        used_chunks as usize * CHUNK_SIZE
    }

    /// The layout for a capacity of `cap` bytes, rounded up to whole chunks and whole bitmap words, so everything
    /// divides evenly.
    pub(crate) const fn layout(cap: usize) -> Layout {
        let () = Self::CHUNK_SIZE_IS_A_POWER_OF_TWO;
        // A full chunk at the end, and full bitmaps (one per 64 chunks) so none is partial.
        let chunk_count = cap.div_ceil(CHUNK_SIZE);
        let aligned_chunk_count = chunk_count.next_multiple_of(64);
        let bitmap_count = aligned_chunk_count / 64;

        let bitmap_start = 0;
        let bitmap_end = size_of::<u64>() * bitmap_count;
        let chunks_start = bitmap_end;

        // The chunks are exactly the bytes the bitmaps address: more would be unreachable waste, less would let
        // `alloc` hand out memory beyond the region, since `init` marks every bit free.
        let chunks_end = chunks_start + aligned_chunk_count * CHUNK_SIZE;
        Layout {
            total_size: chunks_end,
            bitmap_count,
            bitmap_start,
            chunks_start,
        }
    }

    /// The lowest bitmap word a search starts from.
    #[cfg(test)]
    pub(crate) const fn search_start(self) -> usize {
        self.search_start
    }
}

/// Clears `count` bits starting at bit `index` of `bitmaps`.
fn clear_bits(bitmaps: &mut [u64], index: usize, count: usize) {
    let mut word = index / 64;
    let mut remaining = count;

    // The bits in the starting word.
    let bit = index % 64;
    let bits = remaining.min(64 - bit);
    bitmaps[word] &= !((!0u64 >> (64 - bits)) << bit);
    remaining -= bits;

    // Whole words.
    word += 1;
    while remaining > 64 {
        bitmaps[word] = 0;
        remaining -= 64;
        word += 1;
    }

    // The bits at the start of the last word.
    if remaining > 0 {
        bitmaps[word] &= !(!0u64 >> (64 - remaining));
    }
}

/// Finds `n` consecutive free chunks in `bitmaps`, marks them used and returns the index of the first; `None` when no
/// run is long enough.
fn find_free_chunks(bitmaps: &mut [u64], n: usize) -> Option<usize> {
    // A naive search through the words; a faster bit-twiddling or SIMD search is welcome, with a benchmark.
    if n > 64 {
        return find_free_chunks_large(bitmaps, n);
    }

    for (index, word) in bitmaps.iter_mut().enumerate() {
        // Shift the inverted word (free chunks are 1 bits) onto itself to find `n` consecutive free chunks:
        //   n = 4
        //   shifted = 001111001011110010
        //           & 000111100101111001
        //           & 000011110010111100
        //           & 000001111001011110
        //           = 000001000000010000
        //                  ^       ^
        // Two places have at least 4 consecutive 1s.
        let free = !*word;
        let mut shifted = free;
        for i in 1..n {
            shifted &= free >> i;
        }
        if shifted == 0 {
            continue;
        }

        // The trailing zeroes are the first bit with at least `n` consecutive 1s (4 in the example).
        let bit = shifted.trailing_zeros() as usize;
        *word |= (u64::MAX >> (64 - n)) << bit;
        return Some(index * 64 + bit);
    }
    None
}

/// [`find_free_chunks`] for runs longer than one word: free bits at the end of a word, whole free words, then free
/// bits at the start of the last one.
fn find_free_chunks_large(bitmaps: &mut [u64], n: usize) -> Option<usize> {
    let mut i = 0;
    'search: while i < bitmaps.len() {
        // The free chunks at the end of this word; a run cannot start in a word that has none.
        let prefix = bitmaps[i].leading_zeros() as usize;
        if prefix == 0 {
            i += 1;
            continue;
        }

        // Where the run starts if it is found here, and how many more free chunks it needs.
        let start_bitmap = i;
        let start_bit = 64 - prefix;
        let mut remaining = n - prefix;

        i += 1;
        while remaining > 64 {
            // Out of words: no gap is large enough.
            if i >= bitmaps.len() {
                return None;
            }
            // A word with used chunks breaks the run; start again from it.
            if bitmaps[i] != 0 {
                continue 'search;
            }
            remaining -= 64;
            i += 1;
        }

        // Too few free chunks at the start of this word: start again from it.
        if i >= bitmaps.len() || (bitmaps[i].trailing_zeros() as usize) < remaining {
            continue;
        }

        // Found: mark everything from the start to the end used.
        let suffix = (n - prefix) % 64;
        bitmaps[start_bitmap] |= (!0u64 >> start_bit) << start_bit;
        let full_bitmaps = (n - prefix - suffix) / 64;
        bitmaps[start_bitmap + 1..][..full_bitmaps].fill(u64::MAX);
        if suffix > 0 {
            bitmaps[i] |= !0u64 >> (64 - suffix);
        }
        return Some(start_bitmap * 64 + start_bit);
    }
    None
}

#[cfg(test)]
mod tests;
