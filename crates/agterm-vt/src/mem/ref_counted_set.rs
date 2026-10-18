//! A reference-counted set inside a page's buffer, addressed by offsets (a page's styles and hyperlinks).
//!
//! Ghostty: `src/terminal/ref_counted_set.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/ref_counted_set.rs` at `RUSTTY_REV` (MIT).
//! Changes: the methods take the page's bytes instead of a base pointer and are safe; the table is a checked view,
//! and items are records of the reference's stride whose fields (the `Pod` value, the bucket, the probe length, the
//! count) are read and written at fixed offsets, so a value type's compact encoding never changes the layout. The
//! context is passed to each call, not stored, and is handed the page's bytes: a context that reads strings in the
//! page (the hyperlink set) needs no page pointer, and one comparing against another page holds that page's bytes.
//! Lookups return copies instead of pointers. A `deleted` callback runs right after the operation that dropped the
//! item has finished with the table, not in the middle of it (it frees the item's own resources, which the table
//! never reads). Kept from rustty: increments saturate and a count at the maximum is pinned (never decremented), so
//! a style referenced by more cells than its count holds stays alive instead of wrapping (a page of more than 65,535
//! styled cells, reachable on a very wide terminal); and the predicate of a pure reference bump, which the page's
//! clone uses to batch a run of identical adds. The disabled integrity check is not ported.

use core::marker::PhantomData;

use bytemuck::Pod;

use crate::mem::offset::{Offset, OffsetBuf, view, view_mut};

/// The unsigned integers a set's ids and reference counts can be.
pub(crate) trait SetInt: Pod + Ord + core::fmt::Debug {
    const MAX: Self;
    const ZERO: Self;
    const ONE: Self;
    fn to_usize(self) -> usize;
    /// `value`, which the caller keeps in range (a masked table position, an id below the capacity).
    fn from_usize(value: usize) -> Self;
    fn saturating_add(self, other: Self) -> Self;
    fn checked_sub(self, other: Self) -> Option<Self>;
}

macro_rules! set_int {
    ($($t:ty),*) => {$(
        impl SetInt for $t {
            const MAX: Self = <$t>::MAX;
            const ZERO: Self = 0;
            const ONE: Self = 1;

            fn to_usize(self) -> usize {
                usize::try_from(self).unwrap_or(usize::MAX)
            }

            fn from_usize(value: usize) -> Self {
                debug_assert!(<$t>::try_from(value).is_ok(), "{value} out of range");
                <$t>::try_from(value).unwrap_or(<$t>::MAX)
            }

            fn saturating_add(self, other: Self) -> Self {
                <$t>::saturating_add(self, other)
            }

            fn checked_sub(self, other: Self) -> Option<Self> {
                <$t>::checked_sub(self, other)
            }
        }
    )*};
}

set_int!(u8, u16, u32, u64);

/// The reference's size and alignment of the set's item (`struct { value: T, meta: Metadata }`) for a value type.
/// The item array's size feeds the page layout, and so the page size, the rows a page holds, the page split points
/// and the scrollback kept: each value type states the reference's `@sizeOf(Item)` and `@alignOf(Item)`, and the set
/// lays its items out with that stride and alignment whatever the value's own encoding.
pub(crate) trait RefItemGeometry {
    /// The reference's `@sizeOf(Item)`.
    const REF_ITEM_SIZE: usize;
    /// The reference's `@alignOf(Item)`.
    const REF_ITEM_ALIGN: usize;
}

/// How a set hashes, compares and drops its values (Zig's duck-typed `Context`), handed the page's bytes.
pub(crate) trait RefCountedSetContext<T> {
    /// The value's hash.
    fn hash(&self, mem: &[u8], value: &T) -> u64;
    /// Whether two values are equal: `a` is a value passed to `add` or `lookup`, `b` one resident in the set.
    fn eql(&self, mem: &[u8], a: &T, b: &T) -> bool;
    /// Called when a value is dropped for good (its item deleted, or a value passed to `add` that matched a resident
    /// one), to free what it owns.
    fn deleted(&mut self, _mem: &mut [u8], _value: T) {}
}

/// An item's metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Metadata<Id, R> {
    /// The table bucket referencing the item.
    bucket: Id,
    /// The probe sequence length between the item's starting bucket and the bucket it is in (Robin Hood hashing).
    psl: Id,
    /// The reference count.
    ref_count: R,
}

impl<Id: SetInt, R: SetInt> Metadata<Id, R> {
    /// `.{}`: no bucket, no probe, no references.
    const DEFAULT: Self = Self {
        bucket: Id::MAX,
        psl: Id::ZERO,
        ref_count: R::ZERO,
    };
}

/// The memory layout of a set of some capacity: the table, then the items.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Layout<Id> {
    /// The item capacity, one more than the items the set stores (id 0 is reserved).
    pub(crate) cap: usize,
    pub(crate) table_cap: usize,
    pub(crate) table_mask: Id,
    pub(crate) table_start: usize,
    pub(crate) items_start: usize,
    pub(crate) total_size: usize,
}

/// Why `add` refused a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AddError {
    /// No room for a new item: remove items, or grow and reinitialize.
    OutOfMemory,
    /// Many dead items with lower ids are out of reach for reuse: rehash.
    NeedsRehash,
}

/// The load until the set reports [`AddError::OutOfMemory`] and needs more capacity; works well in practice.
const LOAD_FACTOR: f64 = 0.8125;

/// Increments a count by `n`, stopping at the maximum.
fn bump_ref<R: SetInt>(count: R, n: R) -> R {
    count.saturating_add(n)
}

/// Decrements a count by `n` unless it is pinned at the maximum; the new count, and whether it reached zero.
fn drop_ref<R: SetInt>(count: R, n: R) -> (R, bool) {
    if count == R::MAX {
        return (count, false);
    }
    let dropped = count.checked_sub(n).unwrap_or(R::ZERO);
    (dropped, dropped == R::ZERO)
}

/// The probe position `i` steps from `hash`, masked to the table.
#[expect(
    clippy::cast_possible_truncation,
    reason = "masked by the table mask, which fits usize"
)]
fn probe<Id: SetInt>(hash: u64, i: usize, mask: Id) -> usize {
    (hash.wrapping_add(i as u64) & mask.to_usize() as u64) as usize
}

/// A reference-counted set.
///
/// Made for some capacity (its exact memory need is [`Layout::init`]'s `total_size`); beyond it, the memory-using
/// methods report [`AddError`] and the caller decides how to go on. Each item has a reference count the caller
/// keeps: an item at 0 references stays until another item takes its bucket, so it can be resurrected if added again
/// before that.
///
/// The structure is an open-addressed hash table with linear probing and Robin Hood hashing, over a flat array of
/// items. The table maps values to item ids, indices in the item array holding the value and its count; with an id a
/// caller reaches an item and its count without hashing. Id 0 is reserved and never assigned. Items with more
/// references are placed earlier in their probe sequence, on the assumption that they are looked up more often.
///
/// The descriptor is `Copy`, as a page's clone copies it along with the buffer.
pub(crate) struct RefCountedSet<T, Id, R> {
    /// The table of item ids.
    table: Offset<Id>,
    /// The longest probe sequence, so a lookup of an absent value stops early.
    max_psl: Id,
    /// How many items have each probe length, so `max_psl` can shrink on deletion. A probe of 32 or more is
    /// astronomically unlikely with a uniform hash, but a crafted input can subvert the hash: any item in the last
    /// slot here makes an insertion report out of memory instead.
    psl_stats: [Id; 32],
    /// The item records.
    items: Offset<u8>,
    /// The live items.
    living: usize,
    /// The next id to store an item at.
    next_id: Id,
    layout: Layout<Id>,
    _marker: PhantomData<fn() -> (T, R)>,
}

impl<T, Id: Copy, R> Clone for RefCountedSet<T, Id, R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T, Id: Copy, R> Copy for RefCountedSet<T, Id, R> {}

impl<Id: SetInt> Layout<Id> {
    /// The layout for a table of `cap` buckets (rounded up to a power of two), with items for the load factor.
    /// The item capacity is one more than the items it stores, since id 0 is reserved.
    pub(crate) fn init<T: RefItemGeometry, R: SetInt>(cap: usize) -> Self {
        debug_assert!(cap <= Id::MAX.to_usize() + 1);
        if cap == 0 {
            return Self {
                cap: 0,
                table_cap: 0,
                table_mask: Id::ZERO,
                table_start: 0,
                items_start: 0,
                total_size: 0,
            };
        }
        let table_cap = cap.next_power_of_two();
        let items_cap = items_for_table(table_cap);
        let table_mask = Id::from_usize(table_cap - 1);
        let table_start = 0;
        let table_end = table_start + table_cap * size_of::<Id>();
        let items_start = table_end.next_multiple_of(Item::<T, Id, R>::ALIGN);
        let items_end = items_start + items_cap * Item::<T, Id, R>::STRIDE;
        Self {
            cap: items_cap,
            table_cap,
            table_mask,
            table_start,
            items_start,
            total_size: items_end,
        }
    }
}

/// The items a table of `table_cap` buckets holds at the load factor (`@intFromFloat(load_factor * table_cap)`).
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "a table capacity is at most 2^32, exact in f64, and the product is non-negative and smaller"
)]
fn items_for_table(table_cap: usize) -> usize {
    (LOAD_FACTOR * table_cap as f64) as usize
}

/// The item record of a set: where each field sits in a slot of the reference's stride.
struct Item<T, Id, R>(PhantomData<(T, Id, R)>);

impl<T: RefItemGeometry, Id, R> Item<T, Id, R> {
    const VALUE: usize = 0;
    const BUCKET: usize = size_of::<T>().next_multiple_of(align_of::<Id>());
    const PSL: usize = Self::BUCKET + size_of::<Id>();
    const REF_COUNT: usize = (Self::PSL + size_of::<Id>()).next_multiple_of(align_of::<R>());

    /// The distance between items: the reference's, which holds this record.
    const STRIDE: usize = {
        assert!(
            Self::REF_COUNT + size_of::<R>() <= T::REF_ITEM_SIZE,
            "the item does not fit the reference's"
        );
        assert!(T::REF_ITEM_ALIGN.is_power_of_two());
        assert!(T::REF_ITEM_SIZE.is_multiple_of(T::REF_ITEM_ALIGN));
        T::REF_ITEM_SIZE
    };

    /// The item array's alignment: the reference's.
    const ALIGN: usize = T::REF_ITEM_ALIGN;
}

impl<T: RefItemGeometry + Pod, Id: SetInt, R: SetInt> Item<T, Id, R> {
    fn read<P: Pod>(items: &[u8], id: usize, field: usize) -> P {
        let start = id * Self::STRIDE + field;
        bytemuck::pod_read_unaligned(&items[start..start + size_of::<P>()])
    }

    fn write<P: Pod>(items: &mut [u8], id: usize, field: usize, value: &P) {
        let start = id * Self::STRIDE + field;
        items[start..start + size_of::<P>()].copy_from_slice(bytemuck::bytes_of(value));
    }

    fn value(items: &[u8], id: usize) -> T {
        Self::read(items, id, Self::VALUE)
    }

    fn set_value(items: &mut [u8], id: usize, value: &T) {
        Self::write(items, id, Self::VALUE, value);
    }

    fn meta(items: &[u8], id: usize) -> Metadata<Id, R> {
        Metadata {
            bucket: Self::read(items, id, Self::BUCKET),
            psl: Self::read(items, id, Self::PSL),
            ref_count: Self::read(items, id, Self::REF_COUNT),
        }
    }

    fn set_meta(items: &mut [u8], id: usize, meta: Metadata<Id, R>) {
        Self::write(items, id, Self::BUCKET, &meta.bucket);
        Self::write(items, id, Self::PSL, &meta.psl);
        Self::write(items, id, Self::REF_COUNT, &meta.ref_count);
    }

    fn ref_count(items: &[u8], id: usize) -> R {
        Self::read(items, id, Self::REF_COUNT)
    }

    fn set_ref_count(items: &mut [u8], id: usize, count: R) {
        Self::write(items, id, Self::REF_COUNT, &count);
    }

    /// The metadata of the item an insertion holds.
    fn held_meta(items: &[u8], insertion: &Insertion<Id, R>) -> Metadata<Id, R> {
        match insertion.held {
            Held::New => insertion.new_meta,
            Held::Resident(index) => Self::meta(items, index),
        }
    }

    /// Moves the held item one probe step further.
    fn advance_held(items: &mut [u8], insertion: &mut Insertion<Id, R>) {
        match insertion.held {
            Held::New => insertion.new_meta.psl = insertion.new_meta.psl.saturating_add(Id::ONE),
            Held::Resident(index) => {
                let mut meta = Self::meta(items, index);
                meta.psl = meta.psl.saturating_add(Id::ONE);
                Self::set_meta(items, index, meta);
            }
        }
    }

    /// `items[id] = .{}`: zero bytes for the value (the reference leaves it undefined), the default metadata.
    fn reset(items: &mut [u8], id: usize) {
        items[id * Self::STRIDE..][..Self::STRIDE].fill(0);
        Self::set_meta(items, id, Metadata::DEFAULT);
    }
}

/// The item an insertion currently holds: the new one, or a resident one it displaced.
#[derive(Clone, Copy)]
enum Held {
    New,
    Resident(usize),
}

/// The state of one Robin Hood insertion.
struct Insertion<Id, R> {
    new_meta: Metadata<Id, R>,
    held: Held,
    held_id: Id,
}

impl<T: RefItemGeometry + Pod, Id: SetInt, R: SetInt> RefCountedSet<T, Id, R> {
    /// The alignment the set's memory must start at.
    pub(crate) const fn base_align() -> usize {
        // the reference's `@max(@alignOf(Context), @alignOf(Layout), @alignOf(Item), @alignOf(Id))`; no context is
        // stored, and a context's pointer is no more aligned than `Layout`'s `usize`
        let aligns = [align_of::<Layout<Id>>(), Item::<T, Id, R>::ALIGN, align_of::<Id>()];
        let mut max = 0;
        let mut i = 0;
        while i < aligns.len() {
            if aligns[i] > max {
                max = aligns[i];
            }
            i += 1;
        }
        max
    }

    /// The table capacity needed to store `n` items, with the load factor and the reserved id 0.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "item counts are small (an id type's range), exact in f64, and the result is non-negative"
    )]
    pub(crate) fn capacity_for_count(n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        ((n + 1) as f64 / LOAD_FACTOR).ceil() as usize
    }

    /// A set in the memory `base` starts at, laid out by `layout`, emptied.
    pub(crate) fn init(mem: &mut [u8], base: OffsetBuf, layout: Layout<Id>) -> Self {
        let set = Self::init_assume_zeroed(base, layout);
        let (table, items) = set.parts_mut(mem);
        table.fill(Id::ZERO);
        items.fill(0);
        for id in 0..layout.cap {
            Item::<T, Id, R>::set_meta(items, id, Metadata::DEFAULT);
        }
        set
    }

    /// A set in memory the caller knows is zeroed (fresh OS pages): it writes nothing, so the pages stay untouched
    /// until the first `add`. Zeroed items have bucket 0 rather than `init`'s maximum, which nothing reads: only
    /// items below `next_id`, all placed by an insertion, are ever looked at.
    pub(crate) fn init_assume_zeroed(base: OffsetBuf, layout: Layout<Id>) -> Self {
        let _ = Item::<T, Id, R>::STRIDE;
        Self {
            table: base.member(layout.table_start),
            max_psl: Id::ZERO,
            psl_stats: [Id::ZERO; 32],
            items: base.member(layout.items_start),
            living: 0,
            next_id: Id::ONE,
            layout,
            _marker: PhantomData,
        }
    }

    fn parts<'a>(&self, mem: &'a [u8]) -> (&'a [Id], &'a [u8]) {
        let items = self.items.index();
        let table = view(mem, self.table.index(), self.layout.table_cap);
        (table, &mem[items..items + self.layout.cap * Item::<T, Id, R>::STRIDE])
    }

    fn parts_mut<'a>(&self, mem: &'a mut [u8]) -> (&'a mut [Id], &'a mut [u8]) {
        // The table comes first (`Layout::init`).
        let (head, tail) = mem.split_at_mut(self.items.index());
        let table = view_mut(head, self.table.index(), self.layout.table_cap);
        (table, &mut tail[..self.layout.cap * Item::<T, Id, R>::STRIDE])
    }

    fn items_mut<'a>(&self, mem: &'a mut [u8]) -> &'a mut [u8] {
        self.parts_mut(mem).1
    }

    /// Adds `value` if absent and takes a reference on its item; returns the item's id.
    pub(crate) fn add<C: RefCountedSetContext<T>>(
        &mut self,
        mem: &mut [u8],
        value: T,
        ctx: &mut C,
    ) -> Result<Id, AddError> {
        // Trim dead items from the end of the list.
        while self.next_id > Id::ONE && self.item_ref_count(mem, self.next_id.to_usize() - 1) == R::ZERO {
            self.next_id = Id::from_usize(self.next_id.to_usize() - 1);
            if let Some(dropped) = self.delete_item(mem, self.next_id) {
                ctx.deleted(mem, dropped);
            }
        }

        // An existing item: the passed value is dropped (the context frees what it owns), the item referenced.
        if let Some(id) = self.lookup(mem, &value, ctx) {
            ctx.deleted(mem, value);
            self.bump(mem, id, R::ONE);
            return Ok(id);
        }

        self.check_room()?;
        let hash = ctx.hash(mem, &value);
        let (id, reaped) = self.insert(mem, &value, self.next_id, hash);
        if let Some(dropped) = reaped {
            ctx.deleted(mem, dropped);
        }
        let items = self.items_mut(mem);
        let count = Item::<T, Id, R>::ref_count(items, id.to_usize()).saturating_add(R::ONE);
        debug_assert_eq!(count, R::ONE);
        Item::<T, Id, R>::set_ref_count(items, id.to_usize(), count);
        self.living += 1;

        // An insertion may reuse a dead item's id, so `next_id` moves only when it was used.
        if id == self.next_id {
            self.next_id = self.next_id.saturating_add(Id::ONE);
        }
        Ok(id)
    }

    /// Whether an insertion has room: a free id, and no probe at the end of the statistics (where a crafted input
    /// could push one past them).
    fn check_room(&self) -> Result<(), AddError> {
        if self.psl_stats[self.psl_stats.len() - 1] > Id::ZERO {
            return Err(AddError::OutOfMemory);
        }
        if self.next_id.to_usize() >= self.layout.cap {
            // Rehash when fewer than 90% of the ids in use are live; otherwise more memory is needed, since a rehash
            // with only a few ids to win back would soon be needed again.
            let threshold = self.layout.cap.saturating_mul(9) / 10;
            if self.living < threshold {
                return Err(AddError::NeedsRehash);
            }
            return Err(AddError::OutOfMemory);
        }
        Ok(())
    }

    /// Adds `value` if absent and takes a reference, using `id` if possible: `Ok(None)` when `id` was used, the id
    /// used otherwise.
    pub(crate) fn add_with_id<C: RefCountedSetContext<T>>(
        &mut self,
        mem: &mut [u8],
        value: T,
        id: Id,
        ctx: &mut C,
    ) -> Result<Option<Id>, AddError> {
        debug_assert!(id > Id::ZERO);
        if id < self.next_id {
            let index = id.to_usize();
            if self.item_ref_count(mem, index) == R::ZERO {
                return self.add_with_dead_id(mem, value, id, ctx);
            }
            let resident = Item::<T, Id, R>::value(self.parts(mem).1, index);
            if ctx.eql(mem, &value, &resident) {
                ctx.deleted(mem, value);
                self.bump(mem, id, R::ONE);
                return Ok(None);
            }
        }
        self.add(mem, value, ctx).map(Some)
    }

    /// `add_with_id` for a requested id whose item is dead.
    fn add_with_dead_id<C: RefCountedSetContext<T>>(
        &mut self,
        mem: &mut [u8],
        value: T,
        id: Id,
        ctx: &mut C,
    ) -> Result<Option<Id>, AddError> {
        // The value already exists under another id: reference that one.
        if let Some(existing) = self.lookup(mem, &value, ctx) {
            ctx.deleted(mem, value);
            self.bump(mem, existing, R::ONE);
            return Ok(Some(existing));
        }
        if self.psl_stats[self.psl_stats.len() - 1] > Id::ZERO {
            return Err(AddError::OutOfMemory);
        }
        if let Some(dropped) = self.delete_item(mem, id) {
            ctx.deleted(mem, dropped);
        }
        let hash = ctx.hash(mem, &value);
        let (added, reaped) = self.insert(mem, &value, id, hash);
        if let Some(dropped) = reaped {
            ctx.deleted(mem, dropped);
        }
        self.bump(mem, added, R::ONE);
        self.living += 1;
        Ok((added != id).then_some(added))
    }

    /// The id `add_with_id(value, id)` would only take one more reference on, if the call would be exactly that
    /// (its other effects at most the context's `deleted` on `value`); `None` when it would do anything else (an
    /// insertion, a trim, a deletion, an error). A pure call changes nothing this reads, so once it holds it holds
    /// for any run of identical calls, all bumping the same id: the page's clone batches such a run into one
    /// [`use_multiple`](Self::use_multiple), which is only equivalent when the context's `deleted` does nothing.
    pub(crate) fn add_with_id_pure_ref_bump_target<C: RefCountedSetContext<T>>(
        &self,
        mem: &[u8],
        value: &T,
        id: Id,
        ctx: &C,
    ) -> Option<Id> {
        if id <= Id::ZERO {
            return None;
        }
        let items = self.parts(mem).1;
        if id < self.next_id {
            let index = id.to_usize();
            if Item::<T, Id, R>::ref_count(items, index) == R::ZERO {
                // The dead-id arm: pure only when the value exists elsewhere.
                return self.lookup(mem, value, ctx);
            }
            if ctx.eql(mem, value, &Item::<T, Id, R>::value(items, index)) {
                return Some(id);
            }
        }
        // `add`: its trim must not run, and the value must be present.
        if self.next_id > Id::ONE && Item::<T, Id, R>::ref_count(items, self.next_id.to_usize() - 1) == R::ZERO {
            return None;
        }
        self.lookup(mem, value, ctx)
    }

    fn item_ref_count(&self, mem: &[u8], index: usize) -> R {
        Item::<T, Id, R>::ref_count(self.parts(mem).1, index)
    }

    fn bump(&self, mem: &mut [u8], id: Id, n: R) {
        let items = self.items_mut(mem);
        let count = Item::<T, Id, R>::ref_count(items, id.to_usize());
        Item::<T, Id, R>::set_ref_count(items, id.to_usize(), bump_ref(count, n));
    }

    /// Takes one more reference on a live item. An item at 0 references means a reference was forgotten, released
    /// too early or lied about.
    pub(crate) fn use_id(&self, mem: &mut [u8], id: Id) {
        self.use_multiple(mem, id, R::ONE);
    }

    /// Takes `n` more references on a live item.
    pub(crate) fn use_multiple(&self, mem: &mut [u8], id: Id, n: R) {
        debug_assert!(id > Id::ZERO && id.to_usize() < self.layout.cap);
        debug_assert!(
            self.item_ref_count(mem, id.to_usize()) > R::ZERO,
            "use of an item with no references"
        );
        self.bump(mem, id, n);
    }

    /// The value of a live item, without taking a reference.
    pub(crate) fn get(&self, mem: &[u8], id: Id) -> T {
        debug_assert!(id > Id::ZERO && id.to_usize() < self.layout.cap);
        let items = self.parts(mem).1;
        debug_assert!(
            Item::<T, Id, R>::ref_count(items, id.to_usize()) > R::ZERO,
            "get of an item with no references"
        );
        Item::<T, Id, R>::value(items, id.to_usize())
    }

    /// Releases one reference on a live item.
    pub(crate) fn release(&mut self, mem: &mut [u8], id: Id) {
        self.release_multiple(mem, id, R::ONE);
    }

    /// Releases `n` references on a live item that has at least that many. A count pinned at the maximum is not
    /// decremented.
    pub(crate) fn release_multiple(&mut self, mem: &mut [u8], id: Id, n: R) {
        debug_assert!(id > Id::ZERO && id.to_usize() < self.layout.cap);
        let items = self.items_mut(mem);
        let count = Item::<T, Id, R>::ref_count(items, id.to_usize());
        debug_assert!(count >= n, "release of more references than the item has");
        let (count, dead) = drop_ref(count, n);
        Item::<T, Id, R>::set_ref_count(items, id.to_usize(), count);
        if dead {
            self.living -= 1;
        }
    }

    /// An item's reference count.
    pub(crate) fn ref_count(&self, mem: &[u8], id: Id) -> R {
        debug_assert!(id > Id::ZERO && id.to_usize() < self.layout.cap);
        self.item_ref_count(mem, id.to_usize())
    }

    /// Whether an item's count is pinned at the maximum.
    pub(crate) fn ref_count_saturated(&self, mem: &[u8], id: Id) -> bool {
        self.ref_count(mem, id) == R::MAX
    }

    /// The live items.
    pub(crate) const fn count(&self) -> usize {
        self.living
    }

    /// The set's layout.
    pub(crate) const fn layout(&self) -> Layout<Id> {
        self.layout
    }

    /// The live items in ascending id order, by value; released items at 0 references are skipped.
    pub(crate) fn iter<'a>(&self, mem: &'a [u8]) -> impl Iterator<Item = (Id, T)> + 'a {
        let items = self.parts(mem).1;
        (1..self.next_id.to_usize())
            .filter(move |&index| Item::<T, Id, R>::ref_count(items, index) > R::ZERO)
            .map(move |index| (Id::from_usize(index), Item::<T, Id, R>::value(items, index)))
    }

    /// Deletes an item: removes it from the table and frees its id; returns its value for the context's `deleted`,
    /// `None` for an item that was never placed.
    fn delete_item(&mut self, mem: &mut [u8], id: Id) -> Option<T> {
        let mask = self.layout.table_mask.to_usize();
        let (table, items) = self.parts_mut(mem);
        let index = id.to_usize();
        let meta = Item::<T, Id, R>::meta(items, index);
        let bucket = meta.bucket.to_usize();
        if bucket > self.layout.table_cap {
            return None;
        }
        debug_assert_eq!(table[bucket], id);
        let value = Item::<T, Id, R>::value(items, index);
        self.stat_sub(meta.psl);
        table[bucket] = Id::ZERO;
        Item::<T, Id, R>::reset(items, index);

        // Shift the rest of the probe sequence back one bucket.
        let mut p = bucket;
        let mut n = (p + 1) & mask;
        while table[n] != Id::ZERO {
            let moved = table[n].to_usize();
            let mut moved_meta = Item::<T, Id, R>::meta(items, moved);
            if moved_meta.psl == Id::ZERO {
                break;
            }
            moved_meta.bucket = Id::from_usize(p);
            self.stat_sub(moved_meta.psl);
            moved_meta.psl = Id::from_usize(moved_meta.psl.to_usize() - 1);
            self.stat_add(moved_meta.psl);
            Item::<T, Id, R>::set_meta(items, moved, moved_meta);
            table[p] = table[n];
            p = n;
            n = (p + 1) & mask;
        }
        while self.max_psl > Id::ZERO && self.psl_stats[self.max_psl.to_usize()] == Id::ZERO {
            self.max_psl = Id::from_usize(self.max_psl.to_usize() - 1);
        }
        table[p] = Id::ZERO;
        Some(value)
    }

    fn stat_add(&mut self, psl: Id) {
        let stat = &mut self.psl_stats[psl.to_usize()];
        *stat = stat.saturating_add(Id::ONE);
    }

    fn stat_sub(&mut self, psl: Id) {
        let stat = &mut self.psl_stats[psl.to_usize()];
        *stat = stat.checked_sub(Id::ONE).unwrap_or(Id::ZERO);
    }

    /// The id of the live item equal to `value`, if any.
    pub(crate) fn lookup<C: RefCountedSetContext<T>>(&self, mem: &[u8], value: &T, ctx: &C) -> Option<Id> {
        // A zero-capacity set holds nothing and has no table to probe.
        if self.layout.table_cap == 0 {
            return None;
        }
        let (table, items) = self.parts(mem);
        let hash = ctx.hash(mem, value);
        for i in 0..=self.max_psl.to_usize() {
            let id = table[probe(hash, i, self.layout.table_mask)];
            // An empty bucket: the item cannot have probed past it.
            if id == Id::ZERO {
                return None;
            }
            let meta = Item::<T, Id, R>::meta(items, id.to_usize());
            // An item with a shorter probe sequence is never in the middle of another sequence (it would have been
            // swapped out when inserted before, or not swapped in when inserted after), so ours is absent.
            if meta.psl.to_usize() < i {
                return None;
            }
            if meta.psl.to_usize() == i
                && meta.ref_count > R::ZERO
                && ctx.eql(mem, value, &Item::<T, Id, R>::value(items, id.to_usize()))
            {
                return Some(id);
            }
        }
        None
    }

    /// Inserts `value` (hash `hash`, absent from the table) with id `new_id`; returns the id used (a dead item's,
    /// if a smaller one was reaped) and the reaped item's value for the context's `deleted`.
    fn insert(&mut self, mem: &mut [u8], value: &T, new_id: Id, hash: u64) -> (Id, Option<T>) {
        let mut insertion = Insertion {
            new_meta: Metadata::DEFAULT,
            held: Held::New,
            held_id: new_id,
        };
        let mut chosen_id = new_id;
        let mut reaped = None;
        let (table, items) = self.parts_mut(mem);
        for i in 0..self.layout.table_cap - 1 {
            let p = probe(hash, i, self.layout.table_mask);
            let id = table[p];

            // An empty bucket takes the held item.
            if id == Id::ZERO {
                table[p] = insertion.held_id;
                self.place(items, &mut insertion, p);
                break;
            }

            // A dead item is reaped for the held item, its id reused if smaller than the one given.
            let meta = Item::<T, Id, R>::meta(items, id.to_usize());
            if meta.ref_count == R::ZERO {
                reaped = Some(Item::<T, Id, R>::value(items, id.to_usize()));
                self.stat_sub(meta.psl);
                Item::<T, Id, R>::reset(items, id.to_usize());
                if id < new_id {
                    chosen_id = id;
                }
                table[p] = insertion.held_id;
                self.place(items, &mut insertion, p);
                break;
            }

            // An item with a shorter probe, or an equal probe and fewer references, swaps with the held item.
            let held = Item::<T, Id, R>::held_meta(items, &insertion);
            if meta.psl < held.psl || (meta.psl == held.psl && meta.ref_count < held.ref_count) {
                table[p] = insertion.held_id;
                self.place(items, &mut insertion, p);
                insertion.held_id = id;
                insertion.held = Held::Resident(id.to_usize());
                self.stat_sub(meta.psl);
            }
            Item::<T, Id, R>::advance_held(items, &mut insertion);
        }

        // The chosen id may be a reaped item's, so the new item's bucket gets it.
        table[insertion.new_meta.bucket.to_usize()] = chosen_id;
        Item::<T, Id, R>::set_value(items, chosen_id.to_usize(), value);
        Item::<T, Id, R>::set_meta(items, chosen_id.to_usize(), insertion.new_meta);
        (chosen_id, reaped)
    }

    /// Puts the held item in bucket `p`.
    fn place(&mut self, items: &mut [u8], insertion: &mut Insertion<Id, R>, p: usize) {
        let bucket = Id::from_usize(p);
        let psl = match insertion.held {
            Held::New => {
                insertion.new_meta.bucket = bucket;
                insertion.new_meta.psl
            }
            Held::Resident(index) => {
                let mut meta = Item::<T, Id, R>::meta(items, index);
                meta.bucket = bucket;
                Item::<T, Id, R>::set_meta(items, index, meta);
                meta.psl
            }
        };
        self.stat_add(psl);
        self.max_psl = self.max_psl.max(psl);
    }
}

#[cfg(test)]
mod tests;
