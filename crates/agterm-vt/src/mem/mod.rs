//! The terminal's memory core: the audited types page memory sits behind.
//!
//! Ghostty keeps a page in one buffer and its structures (the bitmap allocator for grapheme and string bytes, the
//! ref-counted style and hyperlink sets, the grapheme and hyperlink maps) inside it, addressed by offsets, so a page
//! can be copied or moved as one block. That layout is kept: it is what makes scrollback, resize and reflow behave
//! and perform as Ghostty's do.
//!
//! Here the buffer is a byte slice and every value stored in it is [`bytemuck::Pod`], read and written through
//! bounds- and alignment-checked casts ([`offset`]). So the structures over it ([`bitmap_allocator`],
//! [`ref_counted_set`], [`hash_map`]) are safe Rust without a single `unsafe` block, and a wrong offset is a wrong
//! value, never undefined behavior. That rests on the `Pod` and `Zeroable` impls for `Offset` and `OffsetSlice`
//! (four `unsafe impl`s in [`offset`]), which every cast of those types relies on. The other `unsafe` left in the
//! memory core is the OS page allocator (`crate::os::page_allocator`), the one fallible box (`alloc::try_box`) and
//! the test allocator (`testing`).

pub(crate) mod alloc;
pub(crate) mod bitmap_allocator;
pub(crate) mod hash_map;
pub(crate) mod offset;
pub(crate) mod ref_counted_set;
#[cfg(test)]
pub(crate) mod testing;
