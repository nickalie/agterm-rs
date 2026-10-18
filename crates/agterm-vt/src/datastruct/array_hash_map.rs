//! An insertion-ordered hash map: entries in a contiguous array in the order they were first inserted, with a side
//! index for lookups (the page list's tracked pins, the render state's cell sets).
//!
//! Zig: `lib/std/array_hash_map.zig` (Zig 0.15), `std.AutoArrayHashMapUnmanaged`.
//! rustty: `crates/rustty-zigstd/src/array_hash_map.rs` at `RUSTTY_REV` (MIT).
//! Changes: on `std` (a `Vec` of entries and a `HashMap` from key to index) instead of allocator-api2 and hashbrown
//! over an allocator handle. The observable semantics are Zig's: iteration in insertion order; `put` overwrites in
//! place; `swap_remove` moves the last entry into the hole (O(1), changes the order), `ordered_remove` shifts (O(n),
//! keeps it). The infallible methods abort on allocation failure like `std`'s; the `try_` ones are Zig's
//! `try map.put(..)`: they reserve first and fail with [`OutOfMemory`], leaving the map unchanged. The order and
//! sizes of allocations are not Zig's.

use std::collections::HashMap;
use std::hash::Hash;

use crate::mem::alloc::OutOfMemory;

/// A removed key and value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct KV<K, V> {
    pub(crate) key: K,
    pub(crate) value: V,
}

/// The result of a get-or-put: where the entry is, and whether it was there before.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GetOrPutResult {
    /// The entry's index in insertion order.
    pub(crate) index: usize,
    pub(crate) found_existing: bool,
}

/// `std.AutoArrayHashMapUnmanaged(K, V)`.
#[derive(Clone, Debug)]
pub(crate) struct AutoArrayHashMap<K, V> {
    entries: Vec<(K, V)>,
    index: HashMap<K, usize>,
}

impl<K, V> Default for AutoArrayHashMap<K, V> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            index: HashMap::new(),
        }
    }
}

impl<K: Eq + Hash + Clone, V> AutoArrayHashMap<K, V> {
    /// An empty map; allocates nothing.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The number of entries.
    pub(crate) fn count(&self) -> usize {
        self.entries.len()
    }

    /// Whether the map is empty.
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The keys in insertion order.
    pub(crate) fn keys(&self) -> impl Iterator<Item = &K> {
        self.entries.iter().map(|(key, _)| key)
    }

    /// The values in insertion order.
    pub(crate) fn values(&self) -> impl Iterator<Item = &V> {
        self.entries.iter().map(|(_, value)| value)
    }

    /// The values in insertion order, mutable.
    pub(crate) fn values_mut(&mut self) -> impl Iterator<Item = &mut V> {
        self.entries.iter_mut().map(|(_, value)| value)
    }

    /// The entries in insertion order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.entries.iter().map(|(key, value)| (key, value))
    }

    /// The index of `key`'s entry, if present.
    pub(crate) fn get_index(&self, key: &K) -> Option<usize> {
        self.index.get(key).copied()
    }

    /// Whether `key` is present.
    pub(crate) fn contains(&self, key: &K) -> bool {
        self.index.contains_key(key)
    }

    /// The value of `key`, if present.
    pub(crate) fn get(&self, key: &K) -> Option<&V> {
        self.get_index(key).map(|i| &self.entries[i].1)
    }

    /// The value of `key`, mutable, if present (`getPtr`).
    pub(crate) fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        let i = self.get_index(key)?;
        Some(&mut self.entries[i].1)
    }

    /// The stored key equal to `key`, if present.
    pub(crate) fn get_key(&self, key: &K) -> Option<&K> {
        self.get_index(key).map(|i| &self.entries[i].0)
    }

    /// The entry at `index` in insertion order.
    pub(crate) fn entry_at(&self, index: usize) -> Option<(&K, &V)> {
        self.entries.get(index).map(|(key, value)| (key, value))
    }

    /// The value at `index` in insertion order, which must exist.
    pub(crate) fn value_at(&self, index: usize) -> &V {
        &self.entries[index].1
    }

    /// The value at `index` in insertion order, mutable, which must exist.
    pub(crate) fn value_at_mut(&mut self, index: usize) -> &mut V {
        &mut self.entries[index].1
    }

    fn try_reserve_one(&mut self) -> Result<(), OutOfMemory> {
        self.entries.try_reserve(1)?;
        self.index.try_reserve(1)?;
        Ok(())
    }

    /// Room for `capacity` entries in all (`ensureTotalCapacity`), or [`OutOfMemory`].
    pub(crate) fn try_ensure_total_capacity(&mut self, capacity: usize) -> Result<(), OutOfMemory> {
        let additional = capacity.saturating_sub(self.entries.len());
        self.entries.try_reserve_exact(additional)?;
        self.index.try_reserve(additional)?;
        Ok(())
    }

    fn append(&mut self, key: K, value: V) -> usize {
        let i = self.entries.len();
        self.index.insert(key.clone(), i);
        self.entries.push((key, value));
        i
    }

    /// Inserts `key`, which must be absent, with room already ensured (`putAssumeCapacityNoClobber`).
    pub(crate) fn put_assume_capacity_no_clobber(&mut self, key: K, value: V) {
        debug_assert!(
            !self.contains(&key),
            "put_assume_capacity_no_clobber: the key is present"
        );
        debug_assert!(self.entries.len() < self.entries.capacity());
        self.append(key, value);
    }

    /// The entry of `key`, inserting `make()` when absent.
    pub(crate) fn get_or_put_with(&mut self, key: K, make: impl FnOnce() -> V) -> GetOrPutResult {
        if let Some(index) = self.get_index(&key) {
            return GetOrPutResult {
                index,
                found_existing: true,
            };
        }
        GetOrPutResult {
            index: self.append(key, make()),
            found_existing: false,
        }
    }

    /// [`get_or_put_with`](Self::get_or_put_with), failing with [`OutOfMemory`] (map unchanged) when it cannot grow.
    pub(crate) fn try_get_or_put_with(
        &mut self,
        key: K,
        make: impl FnOnce() -> V,
    ) -> Result<GetOrPutResult, OutOfMemory> {
        if let Some(index) = self.get_index(&key) {
            return Ok(GetOrPutResult {
                index,
                found_existing: true,
            });
        }
        self.try_reserve_one()?;
        Ok(GetOrPutResult {
            index: self.append(key, make()),
            found_existing: false,
        })
    }

    /// The entry of `key`, inserting `value` when absent.
    pub(crate) fn get_or_put_value(&mut self, key: K, value: V) -> GetOrPutResult {
        self.get_or_put_with(key, || value)
    }

    /// Inserts `key`, or overwrites its value in place.
    pub(crate) fn put(&mut self, key: K, value: V) {
        if let Some(i) = self.get_index(&key) {
            self.entries[i].1 = value;
        } else {
            self.append(key, value);
        }
    }

    /// [`put`](Self::put), failing with [`OutOfMemory`] (map unchanged) when it cannot grow.
    pub(crate) fn try_put(&mut self, key: K, value: V) -> Result<(), OutOfMemory> {
        if let Some(i) = self.get_index(&key) {
            self.entries[i].1 = value;
        } else {
            self.try_reserve_one()?;
            self.append(key, value);
        }
        Ok(())
    }

    /// Inserts `key`, which must be absent.
    pub(crate) fn put_no_clobber(&mut self, key: K, value: V) {
        assert!(!self.contains(&key), "put_no_clobber: the key is present");
        self.append(key, value);
    }

    /// [`put_no_clobber`](Self::put_no_clobber), failing with [`OutOfMemory`] (map unchanged) when it cannot grow.
    pub(crate) fn try_put_no_clobber(&mut self, key: K, value: V) -> Result<(), OutOfMemory> {
        assert!(!self.contains(&key), "put_no_clobber: the key is present");
        self.try_reserve_one()?;
        self.append(key, value);
        Ok(())
    }

    fn swap_remove_at(&mut self, index: usize) -> (K, V) {
        let removed = self.entries.swap_remove(index);
        self.index.remove(&removed.0);
        if let Some((moved, _)) = self.entries.get(index) {
            self.index.insert(moved.clone(), index);
        }
        removed
    }

    fn ordered_remove_at(&mut self, index: usize) -> (K, V) {
        let removed = self.entries.remove(index);
        self.index.remove(&removed.0);
        for (i, (key, _)) in self.entries.iter().enumerate().skip(index) {
            self.index.insert(key.clone(), i);
        }
        removed
    }

    /// Removes `key`, moving the last entry into its place; whether it was present.
    pub(crate) fn swap_remove(&mut self, key: &K) -> bool {
        self.fetch_swap_remove(key).is_some()
    }

    /// Removes `key`, shifting the entries after it; whether it was present.
    pub(crate) fn ordered_remove(&mut self, key: &K) -> bool {
        self.fetch_ordered_remove(key).is_some()
    }

    /// [`swap_remove`](Self::swap_remove), returning the removed entry.
    pub(crate) fn fetch_swap_remove(&mut self, key: &K) -> Option<KV<K, V>> {
        let (key, value) = self.swap_remove_at(self.get_index(key)?);
        Some(KV { key, value })
    }

    /// [`ordered_remove`](Self::ordered_remove), returning the removed entry.
    pub(crate) fn fetch_ordered_remove(&mut self, key: &K) -> Option<KV<K, V>> {
        let (key, value) = self.ordered_remove_at(self.get_index(key)?);
        Some(KV { key, value })
    }

    /// Removes the entry at `index`, which must exist, moving the last entry into its place.
    pub(crate) fn swap_remove_index(&mut self, index: usize) {
        assert!(index < self.entries.len(), "swap_remove_index: index out of bounds");
        self.swap_remove_at(index);
    }

    /// Removes the entry at `index`, which must exist, shifting the entries after it.
    pub(crate) fn ordered_remove_index(&mut self, index: usize) {
        assert!(index < self.entries.len(), "ordered_remove_index: index out of bounds");
        self.ordered_remove_at(index);
    }

    /// Removes and returns the last entry.
    pub(crate) fn pop(&mut self) -> Option<KV<K, V>> {
        let (key, value) = self.entries.pop()?;
        self.index.remove(&key);
        Some(KV { key, value })
    }

    /// Removes every entry, keeping the capacity.
    pub(crate) fn clear_retaining_capacity(&mut self) {
        self.entries.clear();
        self.index.clear();
    }

    /// Removes every entry and frees the storage.
    pub(crate) fn clear_and_free(&mut self) {
        *self = Self::default();
    }

    /// Sorts the entries in place; `less_than(a, b)` compares the entries at indices `a` and `b` of the order before
    /// the sort.
    pub(crate) fn sort(&mut self, mut less_than: impl FnMut(usize, usize) -> bool) {
        let mut order: Vec<usize> = (0..self.entries.len()).collect();
        order.sort_by(|&a, &b| {
            if less_than(a, b) {
                core::cmp::Ordering::Less
            } else if less_than(b, a) {
                core::cmp::Ordering::Greater
            } else {
                core::cmp::Ordering::Equal
            }
        });
        // Apply the permutation in place: slot `i` takes the entry at `order[i]`, following the chain of entries
        // already moved out of the way.
        for i in 0..order.len() {
            let mut j = order[i];
            while j < i {
                j = order[j];
            }
            self.entries.swap(i, j);
        }
        self.index.clear();
        for (i, (key, _)) in self.entries.iter().enumerate() {
            self.index.insert(key.clone(), i);
        }
    }
}

#[cfg(test)]
mod tests;
