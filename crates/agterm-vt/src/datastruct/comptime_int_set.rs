//! A fixed set of integers that is known at compile time and can be searched quickly at runtime.
//!
//! Use this when you have a table of integer IDs defined at compile time and need to find which entry a runtime value
//! belongs to. A loop over the table compiles to one comparison per entry, so entries near the end of the table, and
//! values that aren't in it at all, pay for every comparison before them. This type sorts the keys at compile time and
//! does a binary search instead, so every lookup costs about log2(N) comparisons no matter which key is looked up. The
//! search has a fixed number of steps, which lets the compiler emit it without branches.
//!
//! [`ComptimeIntSet::index_of`] returns the position of the key in the keys passed in, not its position in the sorted
//! order, so the result indexes any other table that is in the same order as the keys. Keys can be in any order but
//! must be unique: a duplicate fails the build (the set is built in a `const` item). For a handful of keys a plain
//! `match` is just as fast and simpler; this is worth reaching for when the table has dozens of entries and the lookup
//! is on a hot path (the terminal modes).
//!
//! Ghostty: `src/datastruct/comptime_int_set.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-datastruct/src/comptime_int_set.rs` at `RUSTTY_REV` (MIT).
//! Changes: Zig generates a type per key list; here the set is a value holding the sorted `(key, original index)`
//! pairs, built by a `const fn` per integer type (stable `const fn` cannot compare a generic `T`). The sort is an
//! insertion sort, since `const fn` has none; the keys are unique, so its stability is unobservable. The original
//! index is a `u16` (Zig's smallest type that holds every index), widened to `usize` on the way out. `contains` is
//! not taken (no caller), and the set is built for the key types in use.

/// A fixed set of integers known at compile time; see the module docs.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ComptimeIntSet<T, const N: usize> {
    /// The keys in ascending order, each paired with its index in the original keys: `table[i].0 ==
    /// keys[table[i].1]`.
    table: [(T, u16); N],
}

macro_rules! impl_comptime_int_set {
    ($($t:ty),* $(,)?) => {$(
        impl<const N: usize> ComptimeIntSet<$t, N> {
            /// The set of `keys`. Evaluate it in a `const` item, so the sort and the uniqueness check run at compile
            /// time and a duplicate key fails the build.
            pub(crate) const fn new(keys: [$t; N]) -> Self {
                assert!(N <= u16::MAX as usize + 1, "ComptimeIntSet has too many keys");

                let mut table: [($t, u16); N] = [(0, 0); N];
                // `index` follows `i` as a `u16`; every index fits (asserted above), and the last step's wrap is never
                // stored.
                let mut i = 0;
                let mut index: u16 = 0;
                while i < N {
                    table[i] = (keys[i], index);
                    i += 1;
                    index = index.wrapping_add(1);
                }

                let mut i = 1;
                while i < N {
                    let mut j = i;
                    while j > 0 && table[j - 1].0 > table[j].0 {
                        let tmp = table[j - 1];
                        table[j - 1] = table[j];
                        table[j] = tmp;
                        j -= 1;
                    }
                    i += 1;
                }

                let mut i = 1;
                while i < N {
                    assert!(table[i - 1].0 != table[i].0, "ComptimeIntSet keys must be unique");
                    i += 1;
                }

                Self { table }
            }

            /// The index of `key` in the original keys, or `None` if `key` is not in the set.
            #[inline]
            pub(crate) fn index_of(&self, key: $t) -> Option<usize> {
                if N == 0 {
                    return None;
                }

                // Binary search for the last sorted key that is <= `key`. Each step halves the range that is left;
                // `N` is a constant, so this unrolls into a fixed number of steps.
                let mut base: usize = 0;
                let mut n: usize = N;
                while n > 1 {
                    let half = n / 2;
                    if self.table[base + half].0 <= key {
                        base += half;
                    }
                    n -= half;
                }

                (self.table[base].0 == key).then(|| usize::from(self.table[base].1))
            }
        }
    )*};
}

impl_comptime_int_set!(u8, u16, i8);

#[cfg(test)]
mod tests;
