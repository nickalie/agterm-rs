//! Offsets into a page's single buffer, and the checked typed views of that buffer they lead to.
//!
//! Ghostty: `src/terminal/size.zig` at `LIBGHOSTTY_VT_REV` (`Offset`, `Offset.Slice`, `OffsetBuf`, `getOffset`).
//! rustty: `crates/rustty-render-types/src/term_size.rs` at `RUSTTY_REV` (MIT).
//! Changes: an offset is an inert number, never a pointer. Ghostty's `ptr(base)` and `intFromBase` turn a base
//! pointer plus an offset into a typed pointer; here the base is always the page's byte slice, and a typed view is
//! a bounds- and alignment-checked cast of a sub-slice of it ([`view`], [`view_mut`], [`OffsetSlice::slice`]). Every
//! type read through a view is [`bytemuck::Pod`]: any bit pattern is a valid value, so an offset that is stale, from
//! another page or overlapping another structure's region yields wrong values, never undefined behavior. An offset
//! outside the buffer, or one the type's alignment refuses, is refused (`try_` forms) or panics, never read.
//! `OffsetBuf` keeps only the offset (the base is the slice every call is given), and `get_offset` takes a reference
//! into the slice. The cell-count types of `size.zig` come with the page.

use core::marker::PhantomData;
use core::ops::Range;

use bytemuck::{Pod, Zeroable};

/// The largest page, in bytes: offsets into a page fit an [`OffsetInt`].
pub(crate) const MAX_PAGE_SIZE: u32 = u32::MAX;

/// The integer an offset into a page is stored as (`std.math.IntFittingRange(0, max_page_size - 1)`).
pub(crate) type OffsetInt = u32;

/// Why a typed view of page memory was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ViewError {
    /// The range does not lie inside the buffer (or its end overflows).
    OutOfBounds,
    /// The range starts at an address the type's alignment does not allow.
    Misaligned,
}

/// An offset too large for [`OffsetInt`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OffsetOutOfRange;

/// The byte range of `len` values of `T` starting `offset` bytes into a buffer, `None` when it overflows.
fn byte_range<T>(offset: usize, len: usize) -> Option<Range<usize>> {
    let end = len.checked_mul(size_of::<T>())?.checked_add(offset)?;
    Some(offset..end)
}

/// A checked view of `len` values of `T` starting `offset` bytes into `mem`.
pub(crate) fn try_view<T: Pod>(mem: &[u8], offset: usize, len: usize) -> Result<&[T], ViewError> {
    let bytes = byte_range::<T>(offset, len)
        .and_then(|range| mem.get(range))
        .ok_or(ViewError::OutOfBounds)?;
    bytemuck::try_cast_slice(bytes).map_err(|_| ViewError::Misaligned)
}

/// A checked mutable view of `len` values of `T` starting `offset` bytes into `mem`.
pub(crate) fn try_view_mut<T: Pod>(mem: &mut [u8], offset: usize, len: usize) -> Result<&mut [T], ViewError> {
    let bytes = byte_range::<T>(offset, len)
        .and_then(|range| mem.get_mut(range))
        .ok_or(ViewError::OutOfBounds)?;
    bytemuck::try_cast_slice_mut(bytes).map_err(|_| ViewError::Misaligned)
}

/// [`try_view`] for ranges the caller laid out itself: panics when the range is outside `mem` or misaligned, which
/// is a layout bug, never undefined behavior.
pub(crate) fn view<T: Pod>(mem: &[u8], offset: usize, len: usize) -> &[T] {
    let end = len.saturating_mul(size_of::<T>()).saturating_add(offset);
    bytemuck::cast_slice(&mem[offset..end])
}

/// [`try_view_mut`] for ranges the caller laid out itself; panics like [`view`].
pub(crate) fn view_mut<T: Pod>(mem: &mut [u8], offset: usize, len: usize) -> &mut [T] {
    let end = len.saturating_mul(size_of::<T>()).saturating_add(offset);
    bytemuck::cast_slice_mut(&mut mem[offset..end])
}

/// The offset from the start of a page's buffer to some data of type `T`: a number, typed for ease of use, that
/// stays valid when the buffer is copied or moved.
#[repr(transparent)]
pub(crate) struct Offset<T> {
    offset: OffsetInt,
    _marker: PhantomData<fn() -> T>,
}

// SAFETY: `Offset<T>` is `repr(transparent)` over a `u32` (the marker is a zero-sized `PhantomData`), so the all-zero
// bit pattern is a valid value.
unsafe impl<T: 'static> Zeroable for Offset<T> {}
// SAFETY: as above: one `u32` field and a zero-sized marker, no padding, every bit pattern valid, `Copy + 'static`.
unsafe impl<T: 'static> Pod for Offset<T> {}

impl<T> Clone for Offset<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Offset<T> {}

impl<T> PartialEq for Offset<T> {
    fn eq(&self, other: &Self) -> bool {
        self.offset == other.offset
    }
}

impl<T> Eq for Offset<T> {}

impl<T> Default for Offset<T> {
    fn default() -> Self {
        Self::new(0)
    }
}

/// Hashes the offset's integer alone, as `std.hash.autoHash` hashes the packed `OffsetInt` (the page maps key on
/// `Offset(Cell)`).
impl<T> core::hash::Hash for Offset<T> {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.offset.hash(state);
    }
}

impl<T> core::fmt::Debug for Offset<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("Offset").field(&self.offset).finish()
    }
}

impl<T> Offset<T> {
    /// An offset of `offset` bytes.
    pub(crate) const fn new(offset: OffsetInt) -> Self {
        Self {
            offset,
            _marker: PhantomData,
        }
    }

    /// An offset of `offset` bytes, refused when it does not fit an [`OffsetInt`].
    pub(crate) fn try_from_usize(offset: usize) -> Result<Self, OffsetOutOfRange> {
        OffsetInt::try_from(offset).map(Self::new).map_err(|_| OffsetOutOfRange)
    }

    /// An offset of `offset` bytes, which the caller laid out inside a page; panics when it does not fit an
    /// [`OffsetInt`], which no page can hold.
    pub(crate) fn from_layout(offset: usize) -> Self {
        assert!(
            offset <= MAX_PAGE_SIZE as usize,
            "offset {offset} is beyond the largest page ({MAX_PAGE_SIZE} bytes)"
        );
        #[expect(clippy::cast_possible_truncation, reason = "the assert above keeps it within u32")]
        let offset = offset as OffsetInt;
        Self::new(offset)
    }

    /// The offset in bytes.
    pub(crate) const fn get(self) -> OffsetInt {
        self.offset
    }

    /// The offset in bytes, as an index into the page's buffer.
    pub(crate) const fn index(self) -> usize {
        self.offset as usize
    }

    /// The `T` at this offset.
    pub(crate) fn read(self, mem: &[u8]) -> T
    where
        T: Pod,
    {
        view::<T>(mem, self.index(), 1)[0]
    }

    /// Writes the `T` at this offset.
    pub(crate) fn write(self, mem: &mut [u8], value: T)
    where
        T: Pod,
    {
        view_mut::<T>(mem, self.index(), 1)[0] = value;
    }

    /// A checked view of `len` values of `T` starting at this offset.
    pub(crate) fn try_slice(self, mem: &[u8], len: usize) -> Result<&[T], ViewError>
    where
        T: Pod,
    {
        try_view(mem, self.index(), len)
    }

    /// A view of `len` values of `T` starting at this offset; panics like [`view`].
    pub(crate) fn slice(self, mem: &[u8], len: usize) -> &[T]
    where
        T: Pod,
    {
        view(mem, self.index(), len)
    }

    /// A mutable view of `len` values of `T` starting at this offset; panics like [`view`].
    pub(crate) fn slice_mut(self, mem: &mut [u8], len: usize) -> &mut [T]
    where
        T: Pod,
    {
        view_mut(mem, self.index(), len)
    }
}

/// A slice of `T` stored as an offset and a length (`Offset(T).Slice`).
///
/// It lives inside page memory (the hyperlink entries' strings, the grapheme map's values), so every byte of it is a
/// field: the four bytes after the offset that a `u32` beside a `usize` leaves on a 64-bit target are a named,
/// always-zero padding field. Size and alignment are Ghostty's (two words).
#[repr(C)]
pub(crate) struct OffsetSlice<T> {
    offset: Offset<T>,
    #[cfg(target_pointer_width = "64")]
    padding: u32,
    len: usize,
}

const _: () = assert!(size_of::<OffsetSlice<u8>>() == 2 * size_of::<usize>());
const _: () = assert!(align_of::<OffsetSlice<u8>>() == align_of::<usize>());

// SAFETY: `repr(C)` of an `Offset<T>` (a `u32`), on 64-bit an explicit `u32` padding field, and a `usize`: no implicit
// padding on either width (asserted above), and the all-zero bit pattern is a valid value of every field.
unsafe impl<T: 'static> Zeroable for OffsetSlice<T> {}
// SAFETY: as above: every byte belongs to an integer field, so every bit pattern is valid; `Copy + 'static`.
unsafe impl<T: 'static> Pod for OffsetSlice<T> {}

impl<T> Clone for OffsetSlice<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for OffsetSlice<T> {}

impl<T> PartialEq for OffsetSlice<T> {
    fn eq(&self, other: &Self) -> bool {
        self.offset == other.offset && self.len == other.len
    }
}

impl<T> Eq for OffsetSlice<T> {}

impl<T> Default for OffsetSlice<T> {
    fn default() -> Self {
        Self::new(Offset::default(), 0)
    }
}

impl<T> core::fmt::Debug for OffsetSlice<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("OffsetSlice")
            .field("offset", &self.offset.get())
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

impl<T> OffsetSlice<T> {
    /// `len` values of `T` starting at `offset`.
    pub(crate) const fn new(offset: Offset<T>, len: usize) -> Self {
        Self {
            offset,
            #[cfg(target_pointer_width = "64")]
            padding: 0,
            len,
        }
    }

    /// Where the slice starts.
    pub(crate) const fn offset(self) -> Offset<T> {
        self.offset
    }

    /// How many values of `T` the slice holds.
    pub(crate) const fn len(self) -> usize {
        self.len
    }

    /// Whether the slice is empty.
    pub(crate) const fn is_empty(self) -> bool {
        self.len == 0
    }

    /// The slice's byte range in the page's buffer, `None` when it overflows.
    pub(crate) fn byte_range(self) -> Option<Range<usize>> {
        byte_range::<T>(self.offset.index(), self.len)
    }

    /// A checked view of the slice.
    pub(crate) fn try_slice(self, mem: &[u8]) -> Result<&[T], ViewError>
    where
        T: Pod,
    {
        try_view(mem, self.offset.index(), self.len)
    }

    /// A checked mutable view of the slice.
    pub(crate) fn try_slice_mut(self, mem: &mut [u8]) -> Result<&mut [T], ViewError>
    where
        T: Pod,
    {
        try_view_mut(mem, self.offset.index(), self.len)
    }

    /// A view of the slice; panics like [`view`].
    pub(crate) fn slice(self, mem: &[u8]) -> &[T]
    where
        T: Pod,
    {
        view(mem, self.offset.index(), self.len)
    }

    /// A mutable view of the slice; panics like [`view`].
    pub(crate) fn slice_mut(self, mem: &mut [u8]) -> &mut [T]
    where
        T: Pod,
    {
        view_mut(mem, self.offset.index(), self.len)
    }
}

/// Where an offset-based structure starts inside a page's buffer: offset-based structures take one to initialize, so
/// they know which part of the buffer they own while keeping every offset they store relative to the buffer's start
/// (the "true base"), the only base any call is given.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct OffsetBuf {
    offset: usize,
}

impl OffsetBuf {
    /// The start of the buffer.
    pub(crate) const fn init() -> Self {
        Self { offset: 0 }
    }

    /// `offset` bytes into the buffer.
    pub(crate) const fn init_offset(offset: usize) -> Self {
        Self { offset }
    }

    /// Where the structure's own memory starts; anything before it is not its memory.
    pub(crate) const fn start(self) -> usize {
        self.offset
    }

    /// The offset of a member `len` bytes into the structure, against the true base so it can be stored.
    pub(crate) fn member<T>(self, len: usize) -> Offset<T> {
        Offset::from_layout(self.offset + len)
    }

    /// The buffer `offset` bytes further in.
    #[must_use]
    pub(crate) const fn add(self, offset: usize) -> Self {
        Self {
            offset: self.offset + offset,
        }
    }
}

/// The offset of `item` from the start of `base`, `None` when `item` does not lie inside `base`.
pub(crate) fn get_offset<T>(base: &[u8], item: &T) -> Option<Offset<T>> {
    let start = base.as_ptr().addr();
    let offset = core::ptr::from_ref(item).addr().checked_sub(start)?;
    if offset.checked_add(size_of::<T>())? > base.len() {
        return None;
    }
    Offset::try_from_usize(offset).ok()
}

#[cfg(test)]
mod tests {
    use super::{Offset, OffsetBuf, OffsetInt, OffsetOutOfRange, OffsetSlice, ViewError, get_offset, try_view};

    /// An 8-aligned zeroed buffer of `len` bytes.
    fn buffer(len: usize) -> Vec<u64> {
        vec![0; len.div_ceil(8)]
    }

    #[test]
    fn offset_int_is_u32() {
        // If Offset changes, this test makes the change visible, so its implications get thought about.
        assert_eq!(core::any::TypeId::of::<OffsetInt>(), core::any::TypeId::of::<u32>());
        assert_eq!(size_of::<Offset<u64>>(), 4);
    }

    #[test]
    fn offset_reads_and_writes_its_value() {
        let mut words = buffer(64);
        let mem: &mut [u8] = bytemuck::cast_slice_mut(&mut words);
        let offset = Offset::<u32>::new(40);
        offset.write(mem, 0xDEAD_BEEF);
        assert_eq!(offset.read(mem), 0xDEAD_BEEF);
        assert_eq!(&mem[40..44], &0xDEAD_BEEFu32.to_ne_bytes());
        offset.slice_mut(mem, 2)[1] = 7;
        assert_eq!(offset.slice(mem, 2), &[0xDEAD_BEEF, 7]);
    }

    #[test]
    fn offset_slice_views_its_values() {
        let mut words = buffer(64);
        let mem: &mut [u8] = bytemuck::cast_slice_mut(&mut words);
        let slice = OffsetSlice::new(Offset::<u16>::new(8), 4);
        slice.slice_mut(mem).copy_from_slice(&[1, 2, 3, 4]);
        assert_eq!(slice.slice(mem), &[1, 2, 3, 4]);
        assert_eq!(slice.try_slice(mem), Ok(&[1u16, 2, 3, 4][..]));
        assert_eq!(slice.byte_range(), Some(8..16));
        assert!(!slice.is_empty());
        assert!(OffsetSlice::<u16>::default().is_empty());
    }

    #[test]
    fn out_of_range_offsets_are_refused() {
        let mut words = buffer(16);
        let mem: &mut [u8] = bytemuck::cast_slice_mut(&mut words);
        // past the end, overlapping the end, and an overflowing length
        assert_eq!(try_view::<u32>(mem, 16, 1), Err(ViewError::OutOfBounds));
        assert_eq!(try_view::<u32>(mem, 12, 2), Err(ViewError::OutOfBounds));
        assert_eq!(try_view::<u32>(mem, 4, usize::MAX), Err(ViewError::OutOfBounds));
        assert_eq!(
            OffsetSlice::new(Offset::<u64>::new(8), 2).try_slice_mut(mem),
            Err(ViewError::OutOfBounds)
        );
        assert_eq!(Offset::<u8>::new(17).try_slice(mem, 0), Err(ViewError::OutOfBounds));
        assert_eq!(Offset::<u8>::try_from_usize(1 << 40), Err(OffsetOutOfRange));
        assert_eq!(Offset::<u8>::try_from_usize(7).map(Offset::get), Ok(7));
    }

    #[test]
    fn misaligned_offsets_are_refused() {
        let mut words = buffer(16);
        let mem: &mut [u8] = bytemuck::cast_slice_mut(&mut words);
        assert_eq!(try_view::<u32>(mem, 2, 1), Err(ViewError::Misaligned));
        assert_eq!(
            OffsetSlice::new(Offset::<u64>::new(4), 1).try_slice(mem),
            Err(ViewError::Misaligned)
        );
    }

    #[test]
    #[should_panic(expected = "out of range")]
    fn an_unchecked_view_past_the_end_panics() {
        let words = buffer(8);
        let mem: &[u8] = bytemuck::cast_slice(&words);
        let _ = Offset::<u32>::new(8).slice(mem, 1);
    }

    #[test]
    #[should_panic(expected = "beyond the largest page")]
    fn a_member_beyond_the_largest_page_panics() {
        let _ = OffsetBuf::init_offset(usize::MAX - 1).member::<u8>(0);
    }

    #[test]
    fn offset_buf_members_are_against_the_true_base() {
        let buf = OffsetBuf::init_offset(16);
        assert_eq!(buf.start(), 16);
        assert_eq!(buf.member::<u64>(8), Offset::new(24));
        assert_eq!(buf.add(4).member::<u8>(0), Offset::new(20));
        assert_eq!(OffsetBuf::init(), OffsetBuf::default());
    }

    #[test]
    fn get_offset_of_bytes() {
        let widgets: &[u8] = b"ABCD";
        assert_eq!(get_offset(widgets, &widgets[2]).map(Offset::get), Some(2));
    }

    #[test]
    fn get_offset_of_structs() {
        // a widget of two `u32`s
        let widgets: [[u32; 2]; 3] = [[1, 2], [3, 4], [5, 6]];
        let bytes: &[u8] = bytemuck::cast_slice(&widgets);
        let offset = get_offset(bytes, &widgets[2]).map(Offset::get);
        assert_eq!(offset, Some(u32::try_from(size_of::<[u32; 2]>() * 2).unwrap()));
        assert_eq!(get_offset(&bytes[..8], &widgets[2]), None, "outside the base");
    }

    #[test]
    fn offsets_compare_hash_and_print_by_their_integer() {
        use std::collections::HashSet;
        let set: HashSet<Offset<u8>> = [Offset::new(1), Offset::new(1), Offset::new(2)].into_iter().collect();
        assert_eq!(set.len(), 2);
        assert_eq!(format!("{:?}", Offset::<u8>::new(3)), "Offset(3)");
        let slice = OffsetSlice::new(Offset::<u8>::new(3), 2);
        assert_eq!(format!("{slice:?}"), "OffsetSlice { offset: 3, len: 2, .. }");
        assert_eq!(slice, slice.clone());
        assert_eq!(slice.offset().index(), 3);
        assert_eq!(slice.len(), 2);
    }
}
