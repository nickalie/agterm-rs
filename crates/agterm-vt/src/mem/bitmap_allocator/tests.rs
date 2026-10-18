//! The bitmap allocator's tests: Ghostty's (`bitmap_allocator.zig` at `LIBGHOSTTY_VT_REV`) as rustty ported them,
//! over offsets into a test buffer instead of pointers, plus the safe wrappers' error paths.

use super::{Allocation, BitmapAllocator, NotAllocated, OutOfMemory, find_free_chunks};
use crate::mem::offset::{Offset, OffsetBuf, OffsetSlice};

/// A zeroed, 8-aligned test buffer of `len` bytes (`alloc.alignedAlloc(u8, base_align, total_size)`).
struct Buffer(Vec<u64>);

impl Buffer {
    fn new(len: usize) -> Self {
        Self(vec![0; len.div_ceil(8)])
    }

    fn bytes(&mut self) -> &mut [u8] {
        bytemuck::cast_slice_mut(&mut self.0)
    }
}

/// An allocator of `cap` bytes over a fresh buffer.
fn allocator<const CHUNK: usize>(cap: usize) -> (BitmapAllocator<CHUNK>, Buffer) {
    let layout = BitmapAllocator::<CHUNK>::layout(cap);
    let mut buf = Buffer::new(layout.total_size);
    // garbage the bitmaps, so `init` must clear them
    buf.bytes().fill(0xFF);
    let bm = BitmapAllocator::<CHUNK>::init(buf.bytes(), OffsetBuf::init(), layout);
    (bm, buf)
}

fn offset_of<T>(allocation: &Allocation<T>) -> usize {
    allocation.as_offset_slice().offset().index()
}

#[test]
fn find_free_chunks_single_found() {
    let mut bitmaps = [0b0111_1111_1111_1111_1111_1111_1111_1111_1111_1111_1111_1111_1111_0001_1111_1111_u64];
    assert_eq!(find_free_chunks(&mut bitmaps, 2), Some(9));
    assert_eq!(
        bitmaps[0],
        0b0111_1111_1111_1111_1111_1111_1111_1111_1111_1111_1111_1111_1111_0111_1111_1111
    );
}

#[test]
fn find_free_chunks_single_not_found() {
    let mut bitmaps = [0b0111_1000_1111_1111_1111_1111_1111_1111_1111_1111_1111_1111_1111_1111_1111_1111_u64];
    assert_eq!(find_free_chunks(&mut bitmaps, 4), None);
}

#[test]
fn find_free_chunks_multiple_found() {
    let mut bitmaps = [
        0b0111_1000_1111_1111_1111_1111_1111_1111_1111_1111_1111_1111_1111_1111_1000_1111_u64,
        0b0111_1111_1100_0001_1111_1111_1111_1111_1111_1111_1111_1111_1100_0001_1111_1111,
    ];
    assert_eq!(find_free_chunks(&mut bitmaps, 4), Some(73));
    assert_eq!(
        bitmaps[1],
        0b0111_1111_1100_0001_1111_1111_1111_1111_1111_1111_1111_1111_1101_1111_1111_1111
    );
}

#[test]
fn find_free_chunks_exactly_64_chunks() {
    let mut bitmaps = [0u64];
    assert_eq!(find_free_chunks(&mut bitmaps, 64), Some(0));
    assert_eq!(bitmaps[0], u64::MAX);
}

#[test]
fn find_free_chunks_larger_than_64_chunks() {
    let mut bitmaps = [0u64, 0];
    assert_eq!(find_free_chunks(&mut bitmaps, 65), Some(0));
    assert_eq!(bitmaps, [u64::MAX, 1]);
}

#[test]
fn find_free_chunks_larger_than_64_chunks_not_at_beginning() {
    let mut bitmaps = [
        0b0000_0000_1111_1111_1111_1111_1111_1111_1111_1111_1111_1111_1111_1111_1111_1111_u64,
        0,
        0,
    ];
    assert_eq!(find_free_chunks(&mut bitmaps, 65), Some(56));
    assert_eq!(
        bitmaps,
        [
            u64::MAX,
            0b0000_0001_1111_1111_1111_1111_1111_1111_1111_1111_1111_1111_1111_1111_1111_1111,
            0
        ]
    );
}

#[test]
fn find_free_chunks_larger_than_64_chunks_exact() {
    let mut bitmaps = [0u64, 0];
    assert_eq!(find_free_chunks(&mut bitmaps, 128), Some(0));
    assert_eq!(bitmaps, [u64::MAX, u64::MAX]);
}

#[test]
fn find_free_chunks_large_run_at_the_end_of_the_bitmaps_is_not_found() {
    // free bits only at the end of the last word: a long run cannot finish there
    let mut bitmaps = [u64::MAX, 0x0000_FFFF_FFFF_FFFF];
    assert_eq!(find_free_chunks(&mut bitmaps, 65), None);
    assert_eq!(bitmaps, [u64::MAX, 0x0000_FFFF_FFFF_FFFF], "nothing marked");
}

#[test]
fn layout() {
    assert_eq!(BitmapAllocator::<4>::layout(64 * 4).bitmap_count, 1);
}

#[test]
fn layout_chunks_region_matches_bitmap_addressable_bytes() {
    fn check<const CHUNK: usize>() {
        for cap in [1, CHUNK, CHUNK + 1, 48, 64, 512, 1024, 2048, 8192, 8193] {
            let layout = BitmapAllocator::<CHUNK>::layout(cap);
            let chunks_size = layout.total_size - layout.chunks_start;
            assert_eq!(
                chunks_size,
                layout.bitmap_count * BitmapAllocator::<CHUNK>::BITMAP_BIT_SIZE * CHUNK
            );
            assert!(chunks_size >= cap);
        }
    }
    check::<1>();
    check::<2>();
    check::<4>();
    check::<16>();
    check::<32>();
}

#[test]
fn layout_small_capacity_cannot_alloc_out_of_bounds() {
    // 3 chunks of 16 bytes, but the one bitmap addresses 64
    let (mut bm, mut buf) = allocator::<16>(48);
    let total = BitmapAllocator::<16>::layout(48).total_size;
    let mut count = 0;
    loop {
        match bm.alloc::<u8>(buf.bytes(), 16) {
            Ok(allocation) => {
                let range = allocation.as_offset_slice().byte_range().unwrap();
                assert!(range.end <= total);
                allocation.slice_mut(buf.bytes()).fill(0xAA);
                let _ = allocation.into_offset_slice();
                count += 1;
            }
            Err(err) => {
                assert_eq!(err, OutOfMemory);
                break;
            }
        }
    }
    assert_eq!(count, BitmapAllocator::<16>::BITMAP_BIT_SIZE);
    assert_eq!(bm.used_bytes(buf.bytes()), bm.capacity_bytes());
}

#[test]
fn alloc_sequentially() {
    let (mut bm, mut buf) = allocator::<4>(64);
    let first = bm.alloc::<u8>(buf.bytes(), 1).unwrap();
    first.slice_mut(buf.bytes())[0] = b'A';
    let second = bm.alloc::<u8>(buf.bytes(), 1).unwrap();
    assert_eq!(offset_of(&first) + 4, offset_of(&second));

    let first_offset = offset_of(&first);
    bm.free(buf.bytes(), first);
    let third = bm.alloc::<u8>(buf.bytes(), 1).unwrap();
    assert_eq!(offset_of(&third), first_offset);
    assert_eq!(third.slice(buf.bytes()), b"A", "freeing does not clear");
    bm.free(buf.bytes(), second);
    bm.free(buf.bytes(), third);
    assert_eq!(bm.used_bytes(buf.bytes()), 0);
}

#[test]
fn alloc_non_byte() {
    let (mut bm, mut buf) = allocator::<4>(128);
    let first = bm.alloc::<u32>(buf.bytes(), 1).unwrap();
    first.slice_mut(buf.bytes())[0] = u32::from(b'A');
    let second = bm.alloc::<u32>(buf.bytes(), 1).unwrap();
    assert_eq!(offset_of(&first) + 4, offset_of(&second));

    let first_offset = offset_of(&first);
    bm.free(buf.bytes(), first);
    let third = bm.alloc::<u32>(buf.bytes(), 1).unwrap();
    assert_eq!(offset_of(&third), first_offset);
    assert_eq!(third.slice(buf.bytes()), &[u32::from(b'A')]);
    let _ = (second.into_offset_slice(), third.into_offset_slice());
}

#[test]
fn alloc_non_byte_multi_chunk() {
    const CHUNK: usize = 4 * size_of::<u32>();
    let (mut bm, mut buf) = allocator::<CHUNK>(128);
    let first = bm.alloc::<u32>(buf.bytes(), 6).unwrap();
    assert_eq!(first.as_offset_slice().len(), 6);
    first.slice_mut(buf.bytes()).fill(u32::from(b'A'));
    let second = bm.alloc::<u32>(buf.bytes(), 1).unwrap();
    assert_eq!(offset_of(&first) + size_of::<u32>() * 4 * 2, offset_of(&second));

    let first_offset = offset_of(&first);
    bm.free(buf.bytes(), first);
    let third = bm.alloc::<u32>(buf.bytes(), 1).unwrap();
    assert_eq!(offset_of(&third), first_offset);
    let _ = (second.into_offset_slice(), third.into_offset_slice());
}

#[test]
fn alloc_large() {
    let (mut bm, mut buf) = allocator::<2>(256);
    let allocation = bm.alloc::<u8>(buf.bytes(), 129).unwrap();
    allocation.slice_mut(buf.bytes())[0] = b'A';
    bm.free(buf.bytes(), allocation);
    assert_eq!(bm.used_bytes(buf.bytes()), 0);
}

/// Allocates `len` bytes, fills them with `fill` and returns them as a page record keeps them.
fn filled(bm: &mut BitmapAllocator<1>, buf: &mut Buffer, len: usize, fill: u8) -> OffsetSlice<u8> {
    let allocation = bm.alloc::<u8>(buf.bytes(), len).unwrap();
    assert_eq!(allocation.as_offset_slice().len(), len);
    allocation.slice_mut(buf.bytes()).fill(fill);
    assert!(allocation.slice(buf.bytes()).iter().all(|&b| b == fill));
    allocation.into_offset_slice()
}

/// Frees `slice`, checking it was allocated before and is not after.
fn free_checked(bm: &mut BitmapAllocator<1>, buf: &mut Buffer, slice: OffsetSlice<u8>) {
    assert!(bm.is_allocated(buf.bytes(), slice));
    bm.free_slice(buf.bytes(), slice).unwrap();
    assert!(!bm.is_allocated(buf.bytes(), slice));
}

/// Every bitmap word is zero again.
fn all_free(bm: BitmapAllocator<1>, buf: &mut Buffer, words: usize) {
    assert_eq!(bm.bitmaps(buf.bytes()), &vec![0u64; words][..]);
}

const BITS: usize = BitmapAllocator::<1>::BITMAP_BIT_SIZE;

#[test]
fn alloc_and_free_one_bitmap() {
    let (mut bm, mut buf) = allocator::<1>(BITS * 3);
    let slice = filled(&mut bm, &mut buf, BITS, 0x11);
    free_checked(&mut bm, &mut buf, slice);
    all_free(bm, &mut buf, 3);
}

#[test]
fn search_hint_skips_full_words() {
    let (mut bm, mut buf) = allocator::<1>(BITS * 3);
    assert_eq!(bm.search_start(), 0);

    // fill word 0 with single-chunk allocations
    let first = filled(&mut bm, &mut buf, 1, 0);
    for _ in 1..BITS {
        let _ = filled(&mut bm, &mut buf, 1, 0);
    }
    // word 0 is now full, so the hint moves past it
    assert_eq!(bm.search_start(), 1);

    // the next allocation lands at the start of word 1
    let next = filled(&mut bm, &mut buf, 1, 0);
    assert_eq!(next.offset().index(), first.offset().index() + BITS);

    // freeing in word 0 moves the hint back, and the chunk is found again
    bm.free_slice(buf.bytes(), first).unwrap();
    assert_eq!(bm.search_start(), 0);
    let again = filled(&mut bm, &mut buf, 1, 0);
    assert_eq!(again.offset(), first.offset());
}

#[test]
fn search_hint_does_not_skip_partial_words() {
    let (mut bm, mut buf) = allocator::<1>(BITS * 2);

    // leave the top 4 bits of word 0 free
    let first = filled(&mut bm, &mut buf, 1, 0);
    for _ in 1..BITS - 4 {
        let _ = filled(&mut bm, &mut buf, 1, 0);
    }

    // a run of 8 does not fit in word 0's 4 free bits, so it goes in word 1; word 0 still has free bits, so the hint
    // must stay on it
    let big = filled(&mut bm, &mut buf, 8, 0);
    assert_eq!(big.offset().index(), first.offset().index() + BITS);
    assert_eq!(bm.search_start(), 0);

    // a run of 4 still finds word 0's free bits, after which the hint moves on
    let small = filled(&mut bm, &mut buf, 4, 0);
    assert_eq!(small.offset().index(), first.offset().index() + BITS - 4);
    assert_eq!(bm.search_start(), 1);
}

#[test]
fn alloc_and_free_half_bitmap() {
    let (mut bm, mut buf) = allocator::<1>(BITS * 3);
    let slice = filled(&mut bm, &mut buf, BITS / 2, 0x11);
    free_checked(&mut bm, &mut buf, slice);
    all_free(bm, &mut buf, 3);
}

#[test]
fn alloc_and_free_two_half_bitmaps() {
    let (mut bm, mut buf) = allocator::<1>(BITS * 3);
    let slice = filled(&mut bm, &mut buf, BITS / 2, 0x11);
    let slice2 = filled(&mut bm, &mut buf, BITS / 2, 0x22);
    assert!(slice.slice(buf.bytes()).iter().all(|&b| b == 0x11));
    free_checked(&mut bm, &mut buf, slice2);
    free_checked(&mut bm, &mut buf, slice);
    all_free(bm, &mut buf, 3);
}

#[test]
fn alloc_and_free_1_5_bitmaps() {
    let (mut bm, mut buf) = allocator::<1>(BITS * 3);
    let slice = filled(&mut bm, &mut buf, 3 * BITS / 2, 0x11);
    free_checked(&mut bm, &mut buf, slice);
    all_free(bm, &mut buf, 3);
}

#[test]
fn alloc_and_free_two_1_5_bitmaps() {
    let (mut bm, mut buf) = allocator::<1>(BITS * 3);
    let slice = filled(&mut bm, &mut buf, 3 * BITS / 2, 0x11);
    let slice2 = filled(&mut bm, &mut buf, 3 * BITS / 2, 0x22);
    assert!(slice.slice(buf.bytes()).iter().all(|&b| b == 0x11));
    free_checked(&mut bm, &mut buf, slice2);
    free_checked(&mut bm, &mut buf, slice);
    all_free(bm, &mut buf, 3);
}

#[test]
fn alloc_and_free_1_5_bitmaps_offset_by_0_75() {
    let (mut bm, mut buf) = allocator::<1>(BITS * 3);
    let slice = filled(&mut bm, &mut buf, 3 * BITS / 4, 0x11);
    let slice2 = filled(&mut bm, &mut buf, 3 * BITS / 2, 0x22);
    assert!(slice.slice(buf.bytes()).iter().all(|&b| b == 0x11));
    free_checked(&mut bm, &mut buf, slice2);
    free_checked(&mut bm, &mut buf, slice);
    all_free(bm, &mut buf, 3);
}

#[test]
fn alloc_and_free_three_0_75_bitmaps() {
    let (mut bm, mut buf) = allocator::<1>(BITS * 3);
    let slice = filled(&mut bm, &mut buf, 3 * BITS / 4, 0x11);
    let slice2 = filled(&mut bm, &mut buf, 3 * BITS / 4, 0x22);
    let slice3 = filled(&mut bm, &mut buf, 3 * BITS / 4, 0x33);
    assert!(slice.slice(buf.bytes()).iter().all(|&b| b == 0x11));
    assert!(slice2.slice(buf.bytes()).iter().all(|&b| b == 0x22));
    free_checked(&mut bm, &mut buf, slice2);
    free_checked(&mut bm, &mut buf, slice);
    free_checked(&mut bm, &mut buf, slice3);
    all_free(bm, &mut buf, 3);
}

#[test]
fn alloc_and_free_two_1_5_bitmaps_offset_0_75() {
    let (mut bm, mut buf) = allocator::<1>(BITS * 4);
    let slice = filled(&mut bm, &mut buf, 3 * BITS / 4, 0x11);
    let slice2 = filled(&mut bm, &mut buf, 3 * BITS / 2, 0x22);
    let slice3 = filled(&mut bm, &mut buf, 3 * BITS / 2, 0x33);
    assert!(slice.slice(buf.bytes()).iter().all(|&b| b == 0x11));
    assert!(slice2.slice(buf.bytes()).iter().all(|&b| b == 0x22));
    free_checked(&mut bm, &mut buf, slice2);
    free_checked(&mut bm, &mut buf, slice);
    free_checked(&mut bm, &mut buf, slice3);
    all_free(bm, &mut buf, 4);
}

#[test]
fn bytes_required_16() {
    type Chunk16 = BitmapAllocator<16>;
    assert_eq!(Chunk16::bytes_required::<u8>(1), 16);
    assert_eq!(Chunk16::bytes_required::<u8>(16), 16);
    assert_eq!(Chunk16::bytes_required::<u8>(17), 32);
    assert_eq!(Chunk16::bytes_required::<u32>(1), 16);
    assert_eq!(Chunk16::bytes_required::<u32>(4), 16);
    assert_eq!(Chunk16::bytes_required::<u32>(5), 32);
    assert_eq!(Chunk16::bytes_required::<u32>(6), 32);
}

#[test]
fn bytes_required_4_and_32() {
    type Chunk4 = BitmapAllocator<4>;
    type Chunk32 = BitmapAllocator<32>;
    assert_eq!(Chunk4::bytes_required::<u8>(1), 4);
    assert_eq!(Chunk4::bytes_required::<u8>(4), 4);
    assert_eq!(Chunk4::bytes_required::<u8>(5), 8);
    assert_eq!(Chunk4::bytes_required::<u32>(1), 4);
    assert_eq!(Chunk4::bytes_required::<u32>(2), 8);
    assert_eq!(Chunk32::bytes_required::<u8>(1), 32);
    assert_eq!(Chunk32::bytes_required::<u8>(32), 32);
    assert_eq!(Chunk32::bytes_required::<u8>(33), 64);
}

#[test]
fn exhaustion_is_out_of_memory_and_changes_nothing() {
    let (mut bm, mut buf) = allocator::<4>(64);
    assert_eq!(bm.capacity_bytes(), 256);
    let all = bm.alloc::<u8>(buf.bytes(), 256).unwrap();
    assert_eq!(bm.alloc::<u8>(buf.bytes(), 1), Err(OutOfMemory));
    assert_eq!(
        bm.alloc::<u32>(buf.bytes(), usize::MAX),
        Err(OutOfMemory),
        "an overflowing size"
    );
    assert_eq!(bm.used_bytes(buf.bytes()), 256);
    bm.free(buf.bytes(), all);
    assert!(bm.alloc::<u8>(buf.bytes(), 1).is_ok());
}

/// Ambiguous for a type that is `Clone`, so naming its item for [`Allocation`] does not compile if the handle ever
/// becomes `Clone` or `Copy` (the technique of `static_assertions::assert_not_impl_any!`).
trait AmbiguousIfClone<A> {
    fn item() {}
}

impl<T> AmbiguousIfClone<()> for T {}

impl<T: Clone> AmbiguousIfClone<u8> for T {}

#[test]
fn a_double_free_of_an_allocation_is_impossible_by_type() {
    // `free` moves the handle, and the handle cannot be duplicated: `free(a); free(a)` does not compile
    <Allocation<u64> as AmbiguousIfClone<_>>::item();
    let (mut bm, mut buf) = allocator::<8>(64);
    let allocation = bm.alloc::<u64>(buf.bytes(), 1).unwrap();
    bm.free(buf.bytes(), allocation);
    assert_eq!(bm.used_bytes(buf.bytes()), 0);
}

#[test]
fn a_double_free_of_a_slice_is_refused() {
    let (mut bm, mut buf) = allocator::<4>(64);
    let slice = bm.alloc::<u8>(buf.bytes(), 6).unwrap().into_offset_slice();
    assert_eq!(bm.free_slice(buf.bytes(), slice), Ok(()));
    assert_eq!(bm.free_slice(buf.bytes(), slice), Err(NotAllocated));
    assert_eq!(bm.used_bytes(buf.bytes()), 0);
}

#[test]
fn a_slice_outside_the_chunks_is_refused() {
    let (mut bm, mut buf) = allocator::<4>(64);
    let layout = BitmapAllocator::<4>::layout(64);
    let kept = bm.alloc::<u8>(buf.bytes(), 4).unwrap().into_offset_slice();
    let before = buf.bytes().to_vec();

    // before the chunks (in the bitmaps), past the end, not on a chunk boundary, and empty
    let in_bitmaps = OffsetSlice::new(Offset::<u8>::new(0), 4);
    let past_end = OffsetSlice::new(Offset::<u8>::from_layout(layout.total_size), 4);
    let overlapping_end = OffsetSlice::new(Offset::<u8>::from_layout(layout.total_size - 4), 8);
    let unaligned = OffsetSlice::new(Offset::<u8>::from_layout(kept.offset().index() + 1), 2);
    let empty = OffsetSlice::new(kept.offset(), 0);
    for slice in [in_bitmaps, past_end, overlapping_end, unaligned, empty] {
        assert!(!bm.is_allocated(buf.bytes(), slice));
        assert_eq!(bm.free_slice(buf.bytes(), slice), Err(NotAllocated), "{slice:?}");
    }
    assert_eq!(buf.bytes(), &before[..], "nothing changed");
    assert!(bm.is_allocated(buf.bytes(), kept));
}

#[test]
fn init_assume_zeroed_writes_nothing() {
    let layout = BitmapAllocator::<4>::layout(64);
    let mut buf = Buffer::new(layout.total_size);
    let mut bm = BitmapAllocator::<4>::init_assume_zeroed(OffsetBuf::init(), layout);
    assert!(buf.bytes().iter().all(|&b| b == 0));
    let allocation = bm.alloc::<u8>(buf.bytes(), 1).unwrap();
    assert_eq!(offset_of(&allocation), layout.chunks_start);
    let copy = bm;
    assert_eq!(
        copy.used_bytes(buf.bytes()),
        4,
        "a copied descriptor sees the same buffer"
    );
    bm.free(buf.bytes(), allocation);
}

#[test]
fn an_allocator_at_an_offset_stores_offsets_against_the_true_base() {
    let layout = BitmapAllocator::<8>::layout(64);
    let mut buf = Buffer::new(64 + layout.total_size);
    let mut bm = BitmapAllocator::<8>::init(buf.bytes(), OffsetBuf::init_offset(64), layout);
    let allocation = bm.alloc::<u64>(buf.bytes(), 2).unwrap();
    assert_eq!(offset_of(&allocation), 64 + layout.chunks_start);
    allocation.slice_mut(buf.bytes()).copy_from_slice(&[1, 2]);
    assert_eq!(&buf.0[(64 + layout.chunks_start) / 8..][..2], &[1, 2]);
    bm.free(buf.bytes(), allocation);
}
