//! A fixed-capacity hash map that lives inside a page's buffer, addressed by offsets (a page's grapheme and
//! hyperlink maps).
//!
//! Ghostty: `src/terminal/hash_map.zig` at `LIBGHOSTTY_VT_REV`, its fork of the Zig 0.12 standard library's
//! `HashMapUnmanaged`: offsets instead of pointers, so the whole page can be copied and the map keeps working; its
//! memory size per capacity is public; and removal is backward-shift deletion (Knuth vol. 3, section 6.4, algorithm
//! R) instead of tombstones. A fixed-capacity map cannot outgrow tombstones the way an allocating map does; backward
//! shifting restores the table after every removal to the state it would have had without the removed key, so probe
//! chains stay minimal and lookup cost depends only on the load. Insertion never moves entries; removal may move
//! other entries of its cluster.
//! rustty: `crates/rustty-vt/src/hash_map.rs` at `RUSTTY_REV` (MIT).
//! Changes: Ghostty's pointer map (`HashMapUnmanaged`, built from the offsets by `OffsetHashMap.map(base)`) and its
//! offset wrapper are one type, [`OffsetHashMap`], whose safe methods take the page's bytes: the size counter, the
//! slot metadata, the keys and the values are checked views of them, keys and values are `Pod`, and nothing hands
//! out a pointer: lookups return copies, [`OffsetHashMap::get_mut`] and [`OffsetHashMap::get_or_put`] lend the value
//! mutably for as long as the bytes are borrowed, and keys are never lent mutably (a changed key would break the
//! table). `removeByPtr` is [`OffsetHashMap::remove`] by key. The automatic context hashes a key's bytes with seed-0
//! Wyhash, which is Zig's `getAutoHashFn` for keys without padding, so slots, fingerprints and probe order are
//! Ghostty's; its wasm branch is not ported. Zero-sized keys and values are not supported (no page map has them).
//! The canonical-placement check runs after every removal in debug builds (rustty's), besides the reference's test
//! helper.

use core::marker::PhantomData;

use bytemuck::Pod;

use crate::datastruct::wyhash::Wyhash;
use crate::mem::offset::{Offset, OffsetBuf, view, view_mut};

/// The map is full: it has a fixed capacity, so the caller must grow the backing memory and rebuild the map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OutOfMemory;

/// How a map hashes and compares its keys (Zig's duck-typed `Context`).
pub(crate) trait HashMapContext<K> {
    /// The key's hash.
    fn hash(&self, key: K) -> u64;
    /// Whether two keys are equal.
    fn eql(&self, a: K, b: K) -> bool;
}

/// The default maximum load: every slot may be used. A map whose removals churn should choose less, to bound probe
/// lengths.
pub(crate) const DEFAULT_MAX_LOAD_PERCENTAGE: u8 = 100;

/// The context Zig derives for a key type (`AutoContext`): seed-0 Wyhash over the key's bytes, and equality.
pub(crate) struct AutoContext<K>(PhantomData<fn(K)>);

impl<K> Default for AutoContext<K> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<K: Pod + Eq> HashMapContext<K> for AutoContext<K> {
    #[inline]
    fn hash(&self, key: K) -> u64 {
        Wyhash::hash(0, bytemuck::bytes_of(&key))
    }

    fn eql(&self, a: K, b: K) -> bool {
        a == b
    }
}

/// [`OffsetHashMap`] with the automatic context.
pub(crate) type AutoOffsetHashMap<K, V, const MAX_LOAD_PERCENTAGE: u8> =
    OffsetHashMap<K, V, AutoContext<K>, MAX_LOAD_PERCENTAGE>;

/// A map's capacity and size: this map is designed for sizes that fit a `u32`.
pub(crate) type Size = u32;

/// A slot's metadata byte: free (all zero), or used with 7 bits of its key's hash.
///
/// The fingerprint is the top 7 bits of the 64-bit hash, which the slot index (its low `log2(capacity)` bits) never
/// uses, so comparing fingerprints rejects most colliding keys without comparing keys. Zig packs it least
/// significant first: the fingerprint is bits 0..6, the used flag bit 7.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Metadata(u8);

impl Metadata {
    const FREE: Self = Self(0);

    const fn fingerprint(self) -> u8 {
        self.0 & 0x7F
    }

    const fn is_used(self) -> bool {
        self.0 & 0x80 != 0
    }

    /// A free slot is always the zero byte (filling sets the used bit, removal zeroes the byte), so one compare
    /// tells it.
    const fn is_free(self) -> bool {
        self.0 == 0
    }

    const fn take_fingerprint(hash: u64) -> u8 {
        (hash >> (64 - 7)) as u8
    }

    const fn used(fingerprint: u8) -> Self {
        Self((fingerprint & 0x7F) | 0x80)
    }
}

/// A removed or replaced entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct KV<K, V> {
    pub(crate) key: K,
    pub(crate) value: V,
}

/// The result of [`OffsetHashMap::get_or_put`]: the entry's value, lent for as long as the bytes are borrowed.
#[derive(Debug)]
pub(crate) struct GetOrPut<'a, V> {
    pub(crate) value: &'a mut V,
    pub(crate) found_existing: bool,
}

/// The memory layout of a map of some capacity: the size counter, the slot metadata, the keys, the values. Every
/// offset is from the start of the map's memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Layout {
    /// The bytes the map needs; its memory must be aligned to [`OffsetHashMap::base_align`].
    pub(crate) total_size: usize,
    /// Where the slot metadata starts; the size counter is the bytes before it.
    pub(crate) metadata_start: usize,
    /// Where the keys start.
    pub(crate) keys_start: usize,
    /// Where the values start.
    pub(crate) vals_start: usize,
    /// The capacity the layout is for.
    pub(crate) capacity: Size,
}

/// The size counter's bytes, right before the metadata (Zig's `Header`).
const HEADER_SIZE: usize = size_of::<Size>();

/// A hash map inside a page's buffer, addressed by offsets, so the buffer can be copied or moved and the map keeps
/// working. The descriptor is `Copy`, as a page's clone copies it along with the buffer; what changes after init
/// (the size, the slots) lives in the buffer, so an all-zero buffer is an empty map.
pub(crate) struct OffsetHashMap<K, V, C, const MAX_LOAD_PERCENTAGE: u8> {
    /// The slot metadata; the size counter sits right before it.
    metadata: Offset<u8>,
    keys: Offset<K>,
    values: Offset<V>,
    /// The number of slots: zero or a power of two, fixed at init.
    capacity: Size,
    _context: PhantomData<fn(C)>,
}

impl<K, V, C, const P: u8> Clone for OffsetHashMap<K, V, C, P> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K, V, C, const P: u8> Copy for OffsetHashMap<K, V, C, P> {}

impl<K, V, C, const P: u8> Default for OffsetHashMap<K, V, C, P> {
    fn default() -> Self {
        Self {
            metadata: Offset::default(),
            keys: Offset::default(),
            values: Offset::default(),
            capacity: 0,
            _context: PhantomData,
        }
    }
}

impl<K, V, C, const P: u8> core::fmt::Debug for OffsetHashMap<K, V, C, P> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("OffsetHashMap")
            .field("metadata", &self.metadata.get())
            .field("keys", &self.keys.get())
            .field("values", &self.values.get())
            .field("capacity", &self.capacity)
            .finish()
    }
}

/// The map's memory as checked views, borrowed from the page's bytes.
struct Table<'a, K, V> {
    size: &'a mut Size,
    metadata: &'a mut [u8],
    keys: &'a mut [K],
    values: &'a mut [V],
}

/// [`Table`], read-only.
struct TableRef<'a, K, V> {
    size: Size,
    metadata: &'a [u8],
    keys: &'a [K],
    values: &'a [V],
}

impl<K: Pod, V: Pod, C: HashMapContext<K>, const MAX_LOAD_PERCENTAGE: u8> OffsetHashMap<K, V, C, MAX_LOAD_PERCENTAGE> {
    const VALID: () = {
        assert!(MAX_LOAD_PERCENTAGE > 0 && MAX_LOAD_PERCENTAGE <= 100);
        assert!(
            size_of::<K>() > 0 && size_of::<V>() > 0,
            "zero-sized keys and values are not supported"
        );
    };

    /// The alignment the map's memory must start at.
    pub(crate) const fn base_align() -> usize {
        let header = align_of::<Size>();
        let keys = align_of::<K>();
        let values = align_of::<V>();
        let max = if header > keys { header } else { keys };
        if max > values { max } else { values }
    }

    /// The layout of a map holding `size` entries at the maximum load: the capacity is rounded up to a power of
    /// two, so it may hold more.
    pub(crate) fn layout(size: Size) -> Layout {
        Self::layout_for_size(size)
    }

    /// The layout of a map with `capacity` slots, zero or a power of two.
    pub(crate) fn layout_for_capacity(capacity: Size) -> Layout {
        debug_assert!(capacity == 0 || capacity.is_power_of_two());
        // In `usize`, so a large capacity times a key's size cannot overflow (Ghostty #9862).
        let cap = capacity as usize;
        let metadata_start = HEADER_SIZE;
        let metadata_end = metadata_start + cap;
        let keys_start = metadata_end.next_multiple_of(align_of::<K>());
        let keys_end = keys_start + cap * size_of::<K>();
        let vals_start = keys_end.next_multiple_of(align_of::<V>());
        let vals_end = vals_start + cap * size_of::<V>();
        Layout {
            total_size: vals_end.next_multiple_of(Self::base_align()),
            metadata_start,
            keys_start,
            vals_start,
            capacity,
        }
    }

    /// The layout with enough slots for `size` entries at the maximum load.
    pub(crate) fn layout_for_size(size: Size) -> Layout {
        if size == 0 {
            return Self::layout_for_capacity(0);
        }
        // Scale up to the raw slots the load factor needs, in `u64` so `size * 100` cannot overflow.
        let minimum = (u64::from(size) * 100).div_ceil(u64::from(MAX_LOAD_PERCENTAGE));
        // Capacities are powers of two, so the largest that fits a `Size` is its highest bit.
        let max_capacity = 1u64 << (Size::BITS - 1);
        let capacity = minimum.next_power_of_two().min(max_capacity);
        Self::layout_for_capacity(Size::try_from(capacity).unwrap_or(Size::MAX))
    }

    /// A map in the memory `buf` starts at, laid out by `layout`, emptied.
    pub(crate) fn init(mem: &mut [u8], buf: OffsetBuf, layout: Layout) -> Self {
        let map = Self::init_assume_zeroed(buf, layout);
        map.clear_retaining_capacity(mem);
        map
    }

    /// A map in memory the caller knows is zeroed (fresh OS pages): it writes nothing, since zero metadata is free
    /// slots and a zero size counter is an empty map, so the pages stay untouched until the first insert.
    pub(crate) fn init_assume_zeroed(buf: OffsetBuf, layout: Layout) -> Self {
        let () = Self::VALID;
        debug_assert!(buf.start().is_multiple_of(Self::base_align()));
        Self {
            metadata: buf.member(layout.metadata_start),
            keys: buf.member(layout.keys_start),
            values: buf.member(layout.vals_start),
            capacity: layout.capacity,
            _context: PhantomData,
        }
    }

    fn table<'a>(&self, mem: &'a [u8]) -> TableRef<'a, K, V> {
        let cap = self.capacity as usize;
        let metadata = self.metadata.index();
        TableRef {
            size: view::<Size>(mem, metadata - HEADER_SIZE, 1)[0],
            metadata: &mem[metadata..metadata + cap],
            keys: view(mem, self.keys.index(), cap),
            values: view(mem, self.values.index(), cap),
        }
    }

    fn table_mut<'a>(&self, mem: &'a mut [u8]) -> Table<'a, K, V> {
        let cap = self.capacity as usize;
        let metadata = self.metadata.index();
        let keys = self.keys.index();
        let values = self.values.index();
        // The regions are in this order (`layout_for_capacity`): header, metadata, keys, values.
        let (_, rest) = mem.split_at_mut(metadata - HEADER_SIZE);
        let (header, rest) = rest.split_at_mut(HEADER_SIZE);
        let (metadata_bytes, rest) = rest.split_at_mut(cap);
        let (_, rest) = rest.split_at_mut(keys - (metadata + cap));
        let (key_bytes, rest) = rest.split_at_mut(cap * size_of::<K>());
        let (_, rest) = rest.split_at_mut(values - (keys + cap * size_of::<K>()));
        Table {
            size: &mut view_mut::<Size>(header, 0, 1)[0],
            metadata: metadata_bytes,
            keys: view_mut(key_bytes, 0, cap),
            values: view_mut(rest, 0, cap),
        }
    }

    /// The number of entries.
    pub(crate) fn count(&self, mem: &[u8]) -> Size {
        self.table(mem).size
    }

    /// The number of slots.
    pub(crate) const fn capacity(&self) -> Size {
        self.capacity
    }

    /// The most entries the map holds: below the capacity when the maximum load is under 100%, which keeps free
    /// slots in every probe chain and bounds probe lengths.
    pub(crate) fn max_load(&self) -> Size {
        max_load_for_capacity(self.capacity, MAX_LOAD_PERCENTAGE)
    }

    /// Removes every entry, keeping the capacity.
    pub(crate) fn clear_retaining_capacity(&self, mem: &mut [u8]) {
        let table = self.table_mut(mem);
        table.metadata.fill(0);
        *table.size = 0;
    }

    /// Fails unless the map can hold `size` entries in all.
    pub(crate) fn ensure_total_capacity(&self, mem: &[u8], size: Size) -> Result<(), OutOfMemory> {
        let count = self.count(mem);
        if size > count {
            self.check_capacity(count, size - count)?;
        }
        Ok(())
    }

    /// Fails unless the map can hold `additional` more entries.
    pub(crate) fn ensure_unused_capacity(&self, mem: &[u8], additional: Size) -> Result<(), OutOfMemory> {
        self.ensure_total_capacity(mem, self.count(mem).saturating_add(additional))
    }

    fn check_capacity(&self, count: Size, new: Size) -> Result<(), OutOfMemory> {
        if new > self.max_load() - count {
            return Err(OutOfMemory);
        }
        Ok(())
    }

    /// The slot holding `key`, if any. Inline, so the caller's branch on the result fuses with the paths here.
    #[inline]
    fn get_index(&self, table: &TableRef<'_, K, V>, key: K, ctx: &C) -> Option<usize> {
        if table.size == 0 {
            return None;
        }
        let hash = ctx.hash(key);
        let mask = self.capacity as usize - 1;
        let fingerprint = Metadata::take_fingerprint(hash);
        // Bounded, so a table with no free slot ends.
        let mut limit = self.capacity;
        let mut index = home(hash, mask);
        while !Metadata(table.metadata[index]).is_free() && limit != 0 {
            let metadata = Metadata(table.metadata[index]);
            if metadata.is_used() && metadata.fingerprint() == fingerprint && ctx.eql(key, table.keys[index]) {
                return Some(index);
            }
            limit -= 1;
            index = (index + 1) & mask;
        }
        None
    }

    /// The value of `key`, if present.
    #[inline]
    pub(crate) fn get(&self, mem: &[u8], key: K, ctx: &C) -> Option<V> {
        let table = self.table(mem);
        self.get_index(&table, key, ctx).map(|index| table.values[index])
    }

    /// The stored key equal to `key`, if present.
    pub(crate) fn get_key(&self, mem: &[u8], key: K, ctx: &C) -> Option<K> {
        let table = self.table(mem);
        self.get_index(&table, key, ctx).map(|index| table.keys[index])
    }

    /// The value of `key`, lent mutably, if present.
    pub(crate) fn get_mut<'a>(&self, mem: &'a mut [u8], key: K, ctx: &C) -> Option<&'a mut V> {
        let index = self.get_index(&self.table(mem), key, ctx)?;
        Some(&mut self.table_mut(mem).values[index])
    }

    /// Whether `key` is present.
    pub(crate) fn contains(&self, mem: &[u8], key: K, ctx: &C) -> bool {
        self.get_index(&self.table(mem), key, ctx).is_some()
    }

    /// The entry of `key`, inserting it (with an all-zero value) when absent, or [`OutOfMemory`] when it is absent
    /// and the map is at its maximum load.
    pub(crate) fn get_or_put<'a>(&self, mem: &'a mut [u8], key: K, ctx: &C) -> Result<GetOrPut<'a, V>, OutOfMemory> {
        let count = self.count(mem);
        if let Err(full) = self.check_capacity(count, 1) {
            // Full, but an existing entry can still be returned.
            let index = self.get_index(&self.table(mem), key, ctx).ok_or(full)?;
            return Ok(GetOrPut {
                value: &mut self.table_mut(mem).values[index],
                found_existing: true,
            });
        }
        Ok(self.get_or_put_assume_capacity(mem, key, ctx))
    }

    /// The entry of `key`, inserting it when absent; the caller guarantees room for one more entry.
    pub(crate) fn get_or_put_assume_capacity<'a>(&self, mem: &'a mut [u8], key: K, ctx: &C) -> GetOrPut<'a, V> {
        let hash = ctx.hash(key);
        let mask = self.capacity as usize - 1;
        let fingerprint = Metadata::take_fingerprint(hash);
        let table = self.table_mut(mem);
        let mut limit = self.capacity;
        let mut index = home(hash, mask);
        while !Metadata(table.metadata[index]).is_free() && limit != 0 {
            let metadata = Metadata(table.metadata[index]);
            if metadata.is_used() && metadata.fingerprint() == fingerprint && ctx.eql(key, table.keys[index]) {
                return GetOrPut {
                    value: &mut table.values[index],
                    found_existing: true,
                };
            }
            limit -= 1;
            index = (index + 1) & mask;
        }

        // With room for one more entry, the probe ended at a free slot; anything else would overwrite a live entry.
        debug_assert!(
            Metadata(table.metadata[index]).is_free(),
            "get_or_put_assume_capacity on a full map"
        );
        table.metadata[index] = Metadata::used(fingerprint).0;
        table.keys[index] = key;
        table.values[index] = V::zeroed();
        *table.size += 1;
        GetOrPut {
            value: &mut table.values[index],
            found_existing: false,
        }
    }

    /// Inserts `key` with `value`, or keeps the existing value of `key`; returns the entry's value.
    pub(crate) fn get_or_put_value<'a>(
        &self,
        mem: &'a mut [u8],
        key: K,
        value: V,
        ctx: &C,
    ) -> Result<&'a mut V, OutOfMemory> {
        let entry = self.get_or_put(mem, key, ctx)?;
        if !entry.found_existing {
            *entry.value = value;
        }
        Ok(entry.value)
    }

    /// Inserts `key` with `value`, or replaces the value of `key`.
    pub(crate) fn put(&self, mem: &mut [u8], key: K, value: V, ctx: &C) -> Result<(), OutOfMemory> {
        *self.get_or_put(mem, key, ctx)?.value = value;
        Ok(())
    }

    /// Inserts `key`, which the caller knows is absent.
    pub(crate) fn put_no_clobber(&self, mem: &mut [u8], key: K, value: V, ctx: &C) -> Result<(), OutOfMemory> {
        debug_assert!(!self.contains(mem, key, ctx));
        self.check_capacity(self.count(mem), 1)?;
        self.put_assume_capacity_no_clobber(mem, key, value, ctx);
        Ok(())
    }

    /// Inserts or replaces `key`; the caller guarantees room for one more entry.
    pub(crate) fn put_assume_capacity(&self, mem: &mut [u8], key: K, value: V, ctx: &C) {
        *self.get_or_put_assume_capacity(mem, key, ctx).value = value;
    }

    /// Inserts `key`, which the caller knows is absent, with room for it guaranteed: no lookup, the first free slot
    /// of its probe sequence.
    pub(crate) fn put_assume_capacity_no_clobber(&self, mem: &mut [u8], key: K, value: V, ctx: &C) {
        debug_assert!(!self.contains(mem, key, ctx));
        let table = self.table_mut(mem);
        // A free slot must exist for the probe to end; a violated contract panics instead of overwriting an entry.
        debug_assert!(
            *table.size < self.capacity,
            "put_assume_capacity_no_clobber on a full map"
        );
        let hash = ctx.hash(key);
        let mask = self.capacity as usize - 1;
        let mut index = home(hash, mask);
        while Metadata(table.metadata[index]).is_used() {
            index = (index + 1) & mask;
        }
        table.metadata[index] = Metadata::used(Metadata::take_fingerprint(hash)).0;
        table.keys[index] = key;
        table.values[index] = value;
        *table.size += 1;
    }

    /// Inserts or replaces `key`, returning the entry it replaced.
    pub(crate) fn fetch_put(&self, mem: &mut [u8], key: K, value: V, ctx: &C) -> Result<Option<KV<K, V>>, OutOfMemory> {
        let entry = self.get_or_put(mem, key, ctx)?;
        let previous = entry.found_existing.then_some(KV {
            key,
            value: *entry.value,
        });
        *entry.value = value;
        Ok(previous)
    }

    /// [`fetch_put`](Self::fetch_put) with room for an insertion guaranteed.
    pub(crate) fn fetch_put_assume_capacity(&self, mem: &mut [u8], key: K, value: V, ctx: &C) -> Option<KV<K, V>> {
        let entry = self.get_or_put_assume_capacity(mem, key, ctx);
        let previous = entry.found_existing.then_some(KV {
            key,
            value: *entry.value,
        });
        *entry.value = value;
        previous
    }

    /// Removes `key`, returning its entry. Removal may move other entries.
    pub(crate) fn fetch_remove(&self, mem: &mut [u8], key: K, ctx: &C) -> Option<KV<K, V>> {
        let table = self.table(mem);
        let index = self.get_index(&table, key, ctx)?;
        let removed = KV {
            key: table.keys[index],
            value: table.values[index],
        };
        self.remove_by_index(mem, index, ctx);
        Some(removed)
    }

    /// Removes `key`; whether it was present. Removal may move other entries.
    pub(crate) fn remove(&self, mem: &mut [u8], key: K, ctx: &C) -> bool {
        self.fetch_remove(mem, key, ctx).is_some()
    }

    /// Removes the entry in slot `index` by backward-shift deletion: every entry of the cluster whose probe path
    /// passes over the hole moves into it, moving the hole along, until the cluster ends at a free slot.
    #[inline]
    fn remove_by_index(&self, mem: &mut [u8], index: usize, ctx: &C) {
        let mask = self.capacity as usize - 1;
        let table = self.table_mut(mem);
        // A completely full table has no free slot to end the scan, so it is bounded to one cycle. That suffices:
        // the hole only moves forward onto slots already visited, so each entry is considered once.
        let mut hole = index;
        let mut slot = index;
        for _ in 0..self.capacity - 1 {
            slot = (slot + 1) & mask;
            if Metadata(table.metadata[slot]).is_free() {
                break;
            }
            // The entry may move into the hole only if the hole is on its probe path, cyclically within
            // [home, slot); otherwise it would land before its home and lookups would miss it.
            let home_slot = home(ctx.hash(table.keys[slot]), mask);
            if hole.wrapping_sub(home_slot) & mask < slot.wrapping_sub(home_slot) & mask {
                table.metadata[hole] = table.metadata[slot];
                table.keys[hole] = table.keys[slot];
                table.values[hole] = table.values[slot];
                hole = slot;
            }
        }
        table.metadata[hole] = Metadata::FREE.0;
        *table.size -= 1;

        // The standing invariant lookups depend on, checked after every removal in debug builds.
        if cfg!(debug_assertions) {
            let violation = self.canonical_violation(mem, ctx);
            debug_assert!(violation.is_none(), "{}", violation.unwrap_or_default());
        }
    }

    /// The entries in slot order, by value. Any mutation of the map ends the borrow, so the iteration cannot see
    /// entries move.
    pub(crate) fn iter<'a>(&self, mem: &'a [u8]) -> impl Iterator<Item = (K, V)> + 'a {
        let table = self.table(mem);
        table
            .metadata
            .iter()
            .zip(table.keys.iter().zip(table.values))
            .filter(|(metadata, _)| Metadata(**metadata).is_used())
            .map(|(_, (&key, &value))| (key, value))
    }

    /// The keys in slot order.
    pub(crate) fn keys<'a>(&self, mem: &'a [u8]) -> impl Iterator<Item = K> + 'a {
        self.iter(mem).map(|(key, _)| key)
    }

    /// The values in slot order.
    pub(crate) fn values<'a>(&self, mem: &'a [u8]) -> impl Iterator<Item = V> + 'a {
        self.iter(mem).map(|(_, value)| value)
    }

    /// The entries in slot order, values lent mutably (keys by value: a changed key would break the table).
    pub(crate) fn iter_mut<'a>(&self, mem: &'a mut [u8]) -> impl Iterator<Item = (K, &'a mut V)> + 'a {
        let table = self.table_mut(mem);
        table
            .metadata
            .iter()
            .zip(table.keys.iter().zip(table.values.iter_mut()))
            .filter(|(metadata, _)| Metadata(**metadata).is_used())
            .map(|(_, (&key, value))| (key, value))
    }

    /// The first break of the canonical placement backward-shift deletion keeps: every used entry is reachable from
    /// its home slot without crossing a free slot, which is what lookups rely on.
    fn canonical_violation(&self, mem: &[u8], ctx: &C) -> Option<String> {
        if self.capacity == 0 {
            return None;
        }
        let table = self.table(mem);
        let mask = self.capacity as usize - 1;
        let mut used: Size = 0;
        for (slot, &metadata) in table.metadata.iter().enumerate() {
            if !Metadata(metadata).is_used() {
                continue;
            }
            used += 1;
            let mut probe = home(ctx.hash(table.keys[slot]), mask);
            while probe != slot {
                if !Metadata(table.metadata[probe]).is_used() {
                    return Some(format!(
                        "canonical placement violated: entry at slot {slot} is separated from its home slot by the \
                         free slot {probe}"
                    ));
                }
                probe = (probe + 1) & mask;
            }
        }
        (table.size != used).then(|| {
            format!(
                "canonical placement violated: count() == {} but {used} slots are used",
                table.size
            )
        })
    }

    /// Asserts the canonical placement (`expectCanonical`), in every build.
    #[cfg(test)]
    pub(crate) fn expect_canonical(&self, mem: &[u8], ctx: &C) {
        let violation = self.canonical_violation(mem, ctx);
        assert!(violation.is_none(), "{}", violation.unwrap_or_default());
    }
}

/// A key's home slot: the low bits of its hash.
#[expect(
    clippy::cast_possible_truncation,
    reason = "masked to the capacity, which fits usize"
)]
const fn home(hash: u64, mask: usize) -> usize {
    (hash & mask as u64) as usize
}

/// The most entries a map of `capacity` slots holds at `percentage` load.
fn max_load_for_capacity(capacity: Size, percentage: u8) -> Size {
    let load = u64::from(capacity) * u64::from(percentage) / 100;
    Size::try_from(load).unwrap_or(capacity)
}

#[cfg(test)]
mod tests;
