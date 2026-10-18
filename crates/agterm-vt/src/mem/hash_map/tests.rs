//! The hash map's tests: Ghostty's (`hash_map.zig` at `LIBGHOSTTY_VT_REV`) as rustty ported them, over offsets into a
//! test buffer, plus the safe API's own. The `void` values of two reference tests are `u8` here (the map takes no
//! zero-sized types), and rustty's `removeByPtr` tests are covered by removal by key.

use super::{
    AutoContext, AutoOffsetHashMap, DEFAULT_MAX_LOAD_PERCENTAGE, HashMapContext, KV, Layout, OffsetHashMap,
    OutOfMemory, Size,
};
use crate::mem::offset::OffsetBuf;

/// A zeroed test buffer aligned for every map here (`alloc.alignedAlloc(u8, base_align, total_size)`).
struct Buffer(Vec<u64>);

impl Buffer {
    fn new(len: usize) -> Self {
        Self(vec![0; len.div_ceil(8)])
    }

    fn bytes(&mut self) -> &mut [u8] {
        bytemuck::cast_slice_mut(&mut self.0)
    }
}

/// A map laid out by `layout` in a fresh buffer.
fn map_in<K: bytemuck::Pod, V: bytemuck::Pod, C: HashMapContext<K>, const P: u8>(
    layout: Layout,
) -> (OffsetHashMap<K, V, C, P>, Buffer) {
    let mut buf = Buffer::new(layout.total_size);
    let map = OffsetHashMap::init(buf.bytes(), OffsetBuf::init(), layout);
    (map, buf)
}

/// A map with the automatic context and `capacity` slots.
fn auto_map<K: bytemuck::Pod + Eq, V: bytemuck::Pod>(capacity: Size) -> (AutoOffsetHashMap<K, V, 100>, Buffer) {
    map_in(AutoOffsetHashMap::<K, V, 100>::layout_for_capacity(capacity))
}

fn ctx<K>() -> AutoContext<K> {
    AutoContext::default()
}

/// Zig's `std.Random.DefaultPrng` (`Xoshiro256++` seeded through `SplitMix64`) with the draws the reference tests make,
/// so they see the reference's key orders (rustty's `rustty-synthetic` `rand`).
struct ZigPrng {
    s: [u64; 4],
}

impl ZigPrng {
    fn new(seed: u64) -> Self {
        let mut split = seed;
        let mut next = || {
            split = split.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = split;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        };
        Self {
            s: [next(), next(), next(), next()],
        }
    }

    fn next(&mut self) -> u64 {
        let s = &mut self.s;
        let r = s[0].wrapping_add(s[3]).rotate_left(23).wrapping_add(s[0]);
        let t = s[1] << 17;
        s[2] ^= s[0];
        s[3] ^= s[1];
        s[1] ^= s[2];
        s[0] ^= s[3];
        s[2] ^= t;
        s[3] = s[3].rotate_left(45);
        r
    }

    /// `random.int(T)` for a `bits`-wide unsigned `T`: the low bytes of one draw.
    fn int(&mut self, bits: u32) -> u64 {
        let value = self.next();
        if bits == 64 { value } else { value & ((1 << bits) - 1) }
    }

    /// `random.uintLessThan(T, less_than)` for a `bits`-wide `T` (Lemire's method with the extra tweak).
    fn uint_less_than(&mut self, bits: u32, less_than: u64) -> u64 {
        let mask = if bits == 64 {
            u128::from(u64::MAX)
        } else {
            (1u128 << bits) - 1
        };
        let lt = u128::from(less_than);
        let mut m = u128::from(self.int(bits)) * lt;
        let mut l = m & mask;
        if l < lt {
            let mut t = lt.wrapping_neg() & mask;
            if t >= lt {
                t -= lt;
                if t >= lt {
                    t %= lt;
                }
            }
            while l < t {
                m = u128::from(self.int(bits)) * lt;
                l = m & mask;
            }
        }
        u64::try_from(m >> bits).unwrap()
    }

    /// `random.shuffle(T, buf)`.
    fn shuffle<T>(&mut self, buf: &mut [T]) {
        for i in 0..buf.len().saturating_sub(1) {
            let span = u64::try_from(buf.len() - i).unwrap();
            let j = i + usize::try_from(self.uint_less_than(64, span)).unwrap();
            buf.swap(i, j);
        }
    }
}

#[test]
fn basic_usage() {
    let (map, mut buf) = auto_map::<u32, u32>(16);
    let c = ctx();
    let mut total = 0;
    for i in 0..5 {
        map.put(buf.bytes(), i, i, &c).unwrap();
        total += i;
    }
    assert_eq!(map.keys(buf.bytes()).sum::<u32>(), total);
    let mut sum = 0;
    for i in 0..5 {
        assert_eq!(map.get(buf.bytes(), i, &c), Some(i));
        sum += map.get(buf.bytes(), i, &c).unwrap();
    }
    assert_eq!(sum, total);
}

#[test]
fn key_and_value_iterators() {
    let (map, mut buf) = auto_map::<u32, u32>(16);
    let c = ctx();
    assert_eq!(map.keys(buf.bytes()).next(), None);
    assert_eq!(map.values(buf.bytes()).next(), None);

    for i in 0..5 {
        map.put(buf.bytes(), i, i * 10, &c).unwrap();
    }
    assert!(map.remove(buf.bytes(), 2, &c));

    let entries: Vec<(u32, u32)> = map.iter(buf.bytes()).collect();
    let keys: Vec<u32> = map.keys(buf.bytes()).collect();
    let values: Vec<u32> = map.values(buf.bytes()).collect();
    assert_eq!(keys.len(), 4);
    assert_eq!(entries.iter().map(|e| e.0).collect::<Vec<_>>(), keys);
    assert_eq!(entries.iter().map(|e| e.1).collect::<Vec<_>>(), values);

    // the lent values write through to the map
    for (_, value) in map.iter_mut(buf.bytes()) {
        *value += 1;
    }
    assert_eq!(map.get(buf.bytes(), 3, &c), Some(31));
}

#[test]
fn auto_context_matches_reference_wyhash_layout() {
    // The map lives in page memory, so the home slot (`hash & mask`), the fingerprint and the slot each key lands in
    // are Ghostty's: this is the slot order Ghostty produces for this insertion order at capacity 8.
    let (map, mut buf) = auto_map::<u32, u32>(8);
    let c = ctx::<u32>();
    for (key, expected) in [
        (0u32, 0x14b8_3016_dc46_0955u64),
        (1, 0xd145_1a1b_34d5_fb64),
        (2, 0xb52d_3031_bc13_56f4),
        (3, 0x3579_1793_81f3_5bc6),
        (4, 0x63b2_677c_5d80_f97a),
        (8, 0xe850_de3f_7e60_b2fa),
        (16, 0x052a_446e_fa88_3433),
        (24, 0xa226_4b2b_07f9_9c5a),
    ] {
        assert_eq!(c.hash(key), expected, "hash({key})");
    }
    for key in [0u32, 1, 2, 3, 4, 8, 16, 24] {
        map.put(buf.bytes(), key, 1000 + key, &c).unwrap();
    }
    assert_eq!(map.count(buf.bytes()), 8);
    let order: Vec<(u32, u32)> = map.iter(buf.bytes()).collect();
    assert_eq!(
        order,
        [
            (16, 1016),
            (24, 1024),
            (4, 1004),
            (8, 1008),
            (1, 1001),
            (0, 1000),
            (2, 1002),
            (3, 1003)
        ]
    );
    for key in [0u32, 1, 2, 3, 4, 8, 16, 24] {
        assert_eq!(map.get(buf.bytes(), key, &c), Some(1000 + key));
    }
}

#[test]
fn ensure_total_capacity() {
    let (map, mut buf) = auto_map::<i32, i32>(32);
    let c = ctx();
    let initial = map.capacity();
    assert!(initial >= 20);
    for i in 0..20 {
        assert_eq!(map.fetch_put_assume_capacity(buf.bytes(), i, i + 10, &c), None);
    }
    // putAssumeCapacity does not resize
    assert_eq!(map.capacity(), initial);
}

#[test]
fn ensure_unused_capacity_with_removals() {
    let (map, mut buf) = auto_map::<i32, i32>(32);
    let c = ctx();
    for i in 0..100 {
        map.ensure_unused_capacity(buf.bytes(), 1).unwrap();
        map.put_assume_capacity(buf.bytes(), i, i, &c);
        assert!(map.remove(buf.bytes(), i, &c));
    }
}

#[test]
fn clear_retaining_capacity() {
    let (map, mut buf) = auto_map::<u32, u32>(16);
    let c = ctx();
    map.clear_retaining_capacity(buf.bytes());

    map.put(buf.bytes(), 1, 1, &c).unwrap();
    assert_eq!(map.get(buf.bytes(), 1, &c), Some(1));
    assert_eq!(map.count(buf.bytes()), 1);

    map.clear_retaining_capacity(buf.bytes());
    map.put_assume_capacity(buf.bytes(), 1, 1, &c);
    assert_eq!(map.get(buf.bytes(), 1, &c), Some(1));
    assert_eq!(map.count(buf.bytes()), 1);

    let capacity = map.capacity();
    assert!(capacity > 0);
    map.clear_retaining_capacity(buf.bytes());
    map.clear_retaining_capacity(buf.bytes());
    assert_eq!(map.count(buf.bytes()), 0);
    assert_eq!(map.capacity(), capacity);
    assert!(!map.contains(buf.bytes(), 1, &c));
}

#[test]
fn ensure_total_capacity_with_existing_elements() {
    // `MINIMAL_CAPACITY`, the first grow of the standard library's map
    const MINIMAL_CAPACITY: Size = 8;
    let (map, mut buf) = auto_map::<u32, u32>(MINIMAL_CAPACITY);
    let c = ctx();
    map.put(buf.bytes(), 0, 0, &c).unwrap();
    assert_eq!(map.count(buf.bytes()), 1);
    assert_eq!(map.capacity(), MINIMAL_CAPACITY);

    assert_eq!(map.ensure_total_capacity(buf.bytes(), 65), Err(OutOfMemory));
    assert_eq!(map.count(buf.bytes()), 1);
    assert_eq!(map.capacity(), MINIMAL_CAPACITY);
}

#[test]
fn remove() {
    let (map, mut buf) = auto_map::<u32, u32>(32);
    let c = ctx();
    for i in 0..16 {
        map.put(buf.bytes(), i, i, &c).unwrap();
    }
    for i in (0..16).filter(|i| i % 3 == 0) {
        assert!(map.remove(buf.bytes(), i, &c));
    }
    assert_eq!(map.count(buf.bytes()), 10);
    for (key, value) in map.iter(buf.bytes()) {
        assert_eq!(key, value);
        assert_ne!(key % 3, 0);
    }
    for i in 0..16 {
        if i % 3 == 0 {
            assert!(!map.contains(buf.bytes(), i, &c));
        } else {
            assert_eq!(map.get(buf.bytes(), i, &c), Some(i));
        }
    }
}

#[test]
fn reverse_removes() {
    let (map, mut buf) = auto_map::<u32, u32>(32);
    let c = ctx();
    for i in 0..16 {
        map.put_no_clobber(buf.bytes(), i, i, &c).unwrap();
    }
    for i in (1..=16).rev() {
        assert!(map.remove(buf.bytes(), i - 1, &c));
        assert!(!map.contains(buf.bytes(), i - 1, &c));
        for j in 0..i - 1 {
            assert_eq!(map.get(buf.bytes(), j, &c), Some(j));
        }
    }
    assert_eq!(map.count(buf.bytes()), 0);
}

#[test]
fn multiple_removes_on_same_metadata() {
    let (map, mut buf) = auto_map::<u32, u32>(32);
    let c = ctx();
    for i in 0..16 {
        map.put(buf.bytes(), i, i, &c).unwrap();
    }
    for key in [7, 15, 14, 13] {
        assert!(map.remove(buf.bytes(), key, &c));
        assert!(!map.contains(buf.bytes(), key, &c));
    }
    for i in 0..13 {
        if i == 7 {
            assert!(!map.contains(buf.bytes(), i, &c));
        } else {
            assert_eq!(map.get(buf.bytes(), i, &c), Some(i));
        }
    }
    for key in [15, 13, 14, 7] {
        map.put(buf.bytes(), key, key, &c).unwrap();
    }
    for i in 0..16 {
        assert_eq!(map.get(buf.bytes(), i, &c), Some(i));
    }
}

#[test]
fn put_and_remove_loop_in_random_order() {
    let (map, mut buf) = auto_map::<u32, u32>(64);
    let c = ctx();
    let size = 32;
    let mut keys: Vec<u32> = (0..size).collect();
    let mut prng = ZigPrng::new(0);
    for i in size..100 {
        prng.shuffle(&mut keys);
        // ground truth: `std.Random.DefaultPrng.init(0)` and `random.shuffle(u32, keys)` under zig 0.16
        if i == size {
            assert_eq!(
                keys,
                [
                    10, 12, 1, 3, 17, 5, 28, 6, 15, 0, 16, 2, 14, 11, 8, 21, 26, 18, 22, 24, 23, 4, 13, 25, 27, 19, 9,
                    30, 31, 29, 20, 7
                ]
            );
        }
        if i == size + 1 {
            assert_eq!(
                keys,
                [
                    14, 2, 30, 31, 0, 22, 16, 27, 10, 4, 26, 5, 11, 15, 18, 12, 21, 23, 25, 9, 24, 29, 28, 13, 20, 6,
                    3, 8, 19, 1, 7, 17
                ]
            );
        }
        for &key in &keys {
            map.put(buf.bytes(), key, key, &c).unwrap();
        }
        assert_eq!(map.count(buf.bytes()), size);
        for &key in &keys {
            assert!(map.remove(buf.bytes(), key, &c));
        }
        assert_eq!(map.count(buf.bytes()), 0);
    }
}

#[test]
fn put() {
    let (map, mut buf) = auto_map::<u32, u32>(32);
    let c = ctx();
    for i in 0..16 {
        map.put(buf.bytes(), i, i, &c).unwrap();
    }
    for i in 0..16 {
        assert_eq!(map.get(buf.bytes(), i, &c), Some(i));
    }
    for i in 0..16 {
        map.put(buf.bytes(), i, i * 16 + 1, &c).unwrap();
    }
    for i in 0..16 {
        assert_eq!(map.get(buf.bytes(), i, &c), Some(i * 16 + 1));
    }
}

#[test]
fn put_full_load() {
    let (map, mut buf) = auto_map::<usize, usize>(16);
    let c = ctx();
    for i in 0..16 {
        map.put(buf.bytes(), i, i, &c).unwrap();
    }
    for i in 0..16 {
        assert_eq!(map.get(buf.bytes(), i, &c), Some(i));
    }
    assert_eq!(map.put(buf.bytes(), 16, 16, &c), Err(OutOfMemory));
    assert_eq!(map.put_no_clobber(buf.bytes(), 17, 17, &c), Err(OutOfMemory));
}

#[test]
fn put_assume_capacity() {
    let (map, mut buf) = auto_map::<u32, u32>(32);
    let c = ctx();
    for i in 0..20 {
        map.put_assume_capacity_no_clobber(buf.bytes(), i, i, &c);
    }
    let mut sum = 0;
    for i in 0..20 {
        sum += *map.get_mut(buf.bytes(), i, &c).unwrap();
    }
    assert_eq!(sum, 190);
    for i in 0..20 {
        map.put_assume_capacity(buf.bytes(), i, 1, &c);
    }
    let sum: u32 = (0..20).map(|i| map.get(buf.bytes(), i, &c).unwrap()).sum();
    assert_eq!(sum, 20);
}

#[test]
fn repeat_put_assume_capacity_remove() {
    let (map, mut buf) = auto_map::<u32, u32>(32);
    let c = ctx();
    let limit = 32;
    for i in 0..limit {
        map.put_assume_capacity_no_clobber(buf.bytes(), i, i, &c);
    }
    // Repeatedly delete and insert an entry without resizing, under different keys so entries do not land in the
    // freed slot.
    for i in 0..10 * limit {
        assert!(map.remove(buf.bytes(), i, &c));
        if i % 2 == 0 {
            map.put_assume_capacity_no_clobber(buf.bytes(), limit + i, i, &c);
        } else {
            map.put_assume_capacity(buf.bytes(), limit + i, i, &c);
        }
    }
    for i in 9 * limit..10 * limit {
        assert_eq!(map.get(buf.bytes(), limit + i, &c), Some(i));
    }
    assert_eq!(map.count(buf.bytes()), limit);
}

/// Hashes a key to itself.
struct IdentityContext;

impl HashMapContext<u32> for IdentityContext {
    fn hash(&self, key: u32) -> u64 {
        u64::from(key)
    }

    fn eql(&self, a: u32, b: u32) -> bool {
        a == b
    }
}

#[test]
fn no_clobber_move_after_remove_at_max_load() {
    let (map, mut buf) = map_in::<u32, u32, IdentityContext, 80>(
        OffsetHashMap::<u32, u32, IdentityContext, 80>::layout_for_capacity(16),
    );
    let c = IdentityContext;
    let max_load = map.max_load();
    for i in 0..max_load {
        map.put_assume_capacity_no_clobber(buf.bytes(), i, i, &c);
    }
    // A managed cell's move: remove the source, insert at a destination known to be absent. At the maximum load
    // this works for any number of moves, since removal frees a slot.
    for i in 0..100 {
        let dst = i + max_load;
        assert!(map.remove(buf.bytes(), i, &c));
        map.put_assume_capacity_no_clobber(buf.bytes(), dst, dst, &c);
        assert_eq!(map.count(buf.bytes()), max_load);
        assert_eq!(map.get(buf.bytes(), dst, &c), Some(dst));
        map.expect_canonical(buf.bytes(), &c);
    }
}

/// Hashes every key to slot 14, near the end of a 16-slot table, so clusters wrap around the index mask.
struct CollidingContext;

impl HashMapContext<u32> for CollidingContext {
    fn hash(&self, _: u32) -> u64 {
        14
    }

    fn eql(&self, a: u32, b: u32) -> bool {
        a == b
    }
}

#[test]
fn removal_keeps_colliding_clusters_findable() {
    type Map = OffsetHashMap<u32, u32, CollidingContext, DEFAULT_MAX_LOAD_PERCENTAGE>;
    let (map, mut buf) = map_in::<u32, u32, CollidingContext, 100>(Map::layout_for_capacity(16));
    let c = CollidingContext;
    // half the table: the cluster spans the wraparound
    for i in 0..8 {
        map.put_assume_capacity_no_clobber(buf.bytes(), i, i, &c);
    }
    // remove from the middle of the cluster; everything left stays findable after each removal
    for (removed, key) in [3, 0, 7, 4, 1, 6, 2, 5].into_iter().enumerate() {
        assert!(map.remove(buf.bytes(), key, &c));
        for k in 0..8 {
            if map.contains(buf.bytes(), k, &c) {
                assert_eq!(map.get(buf.bytes(), k, &c), Some(k));
            }
        }
        assert_eq!(map.count(buf.bytes()) as usize, 8 - removed - 1);
        map.expect_canonical(buf.bytes(), &c);
    }
}

#[test]
fn removal_from_a_completely_full_table() {
    // a 100% load fills every slot, so the removal scan has no free slot to end at
    let (map, mut buf) = auto_map::<u32, u32>(64);
    let c = ctx();
    for i in 0..64 {
        map.put_assume_capacity_no_clobber(buf.bytes(), i, i, &c);
    }
    assert_eq!(map.count(buf.bytes()), 64);
    let mut expected = 64;
    for i in (0..64).step_by(2) {
        assert!(map.remove(buf.bytes(), i, &c));
        expected -= 1;
        assert_eq!(map.count(buf.bytes()), expected);
    }
    for i in 0..64 {
        let want = (i % 2 != 0).then_some(i);
        assert_eq!(map.get(buf.bytes(), i, &c), want);
    }
    map.expect_canonical(buf.bytes(), &c);
}

/// One reference draw: a key, an operation and, for a put, the value.
fn draw(prng: &mut ZigPrng, key_space: u32) -> (u32, u8, Option<u32>) {
    let key = u32::try_from(prng.uint_less_than(32, u64::from(key_space))).unwrap();
    let op = u8::try_from(prng.uint_less_than(8, 4)).unwrap();
    let value = (op < 2).then(|| u32::try_from(prng.int(32)).unwrap());
    (key, op, value)
}

#[test]
fn random_operations_against_an_oracle() {
    let (map, mut buf) = auto_map::<u32, u32>(64);
    let c = ctx();
    let mut oracle = std::collections::HashMap::new();
    let mut prng = ZigPrng::new(0xdead_beef);
    // ground truth for the first draws: `std.Random.DefaultPrng.init(0xdeadbeef)` under zig 0.16
    let expected = [
        (95, 3, None),
        (73, 2, None),
        (7, 0, Some(2_525_345_492)),
        (44, 1, Some(3_199_506_276)),
        (65, 1, Some(1_110_749_104)),
        (26, 1, Some(4_217_228_002)),
        (49, 2, None),
        (34, 0, Some(3_001_515_670)),
    ];
    // a small key space: frequent hits, misses and re-insertions at every load from empty to full
    let key_space = 64 + 32;
    // fewer iterations under Miri, which is about a thousand times slower; the pinned draws still run
    let iterations = if cfg!(miri) { 1_000 } else { 20_000 };
    for iteration in 0..iterations {
        let (key, op, value) = draw(&mut prng, key_space);
        if let Some(&want) = expected.get(iteration) {
            assert_eq!((key, op, value), want, "draw {iteration}");
        }
        match (op, value) {
            (0 | 1, Some(value)) => {
                if map.put(buf.bytes(), key, value, &c).is_ok() {
                    oracle.insert(key, value);
                } else {
                    // full: a put on an existing key always succeeds
                    assert!(!oracle.contains_key(&key));
                    assert_eq!(map.count(buf.bytes()), map.capacity());
                }
            }
            (2, _) => assert_eq!(map.remove(buf.bytes(), key, &c), oracle.remove(&key).is_some()),
            _ => assert_eq!(map.get(buf.bytes(), key, &c), oracle.get(&key).copied()),
        }
        assert_eq!(map.count(buf.bytes()) as usize, oracle.len());
    }
    for (&key, &value) in &oracle {
        assert_eq!(map.get(buf.bytes(), key, &c), Some(value));
    }
    map.expect_canonical(buf.bytes(), &c);
}

#[test]
fn get_or_put() {
    let (map, mut buf) = auto_map::<u32, u32>(32);
    let c = ctx();
    for i in 0..10 {
        map.put(buf.bytes(), i * 2, 2, &c).unwrap();
    }
    for i in 0..20 {
        let _ = map.get_or_put_value(buf.bytes(), i, 1, &c).unwrap();
    }
    let sum: u32 = (0..20).map(|i| map.get(buf.bytes(), i, &c).unwrap()).sum();
    assert_eq!(sum, 30);
}

#[test]
fn basic_hash_map_usage() {
    let (map, mut buf) = auto_map::<i32, i32>(32);
    let c = ctx();
    for (key, value) in [(1, 11), (2, 22), (3, 33), (4, 44)] {
        assert_eq!(map.fetch_put(buf.bytes(), key, value, &c), Ok(None));
    }
    map.put_no_clobber(buf.bytes(), 5, 55, &c).unwrap();
    assert_eq!(
        map.fetch_put(buf.bytes(), 5, 66, &c),
        Ok(Some(KV { key: 5, value: 55 }))
    );
    assert_eq!(
        map.fetch_put(buf.bytes(), 5, 55, &c),
        Ok(Some(KV { key: 5, value: 66 }))
    );
}

#[test]
fn basic_hash_map_usage_get_or_put_and_remove() {
    let (map, mut buf) = auto_map::<i32, i32>(32);
    let c = ctx();
    for (key, value) in [(1, 11), (2, 22), (3, 33), (4, 44), (5, 55)] {
        map.put(buf.bytes(), key, value, &c).unwrap();
    }

    let gop1 = map.get_or_put(buf.bytes(), 5, &c).unwrap();
    assert!(gop1.found_existing);
    assert_eq!(*gop1.value, 55);
    *gop1.value = 77;
    assert_eq!(map.get(buf.bytes(), 5, &c), Some(77));

    let gop2 = map.get_or_put(buf.bytes(), 99, &c).unwrap();
    assert!(!gop2.found_existing);
    *gop2.value = 42;
    assert_eq!(map.get(buf.bytes(), 99, &c), Some(42));

    assert_eq!(*map.get_or_put_value(buf.bytes(), 5, 5, &c).unwrap(), 77);
    assert_eq!(*map.get_or_put_value(buf.bytes(), 100, 41, &c).unwrap(), 41);
}

#[test]
fn basic_hash_map_usage_remove() {
    let (map, mut buf) = auto_map::<i32, i32>(32);
    let c = ctx();
    for (key, value) in [(1, 11), (2, 22), (3, 33), (4, 44), (5, 55)] {
        map.put(buf.bytes(), key, value, &c).unwrap();
    }
    assert!(map.contains(buf.bytes(), 2, &c));
    assert_eq!(map.get_key(buf.bytes(), 2, &c), Some(2));
    assert_eq!(map.get(buf.bytes(), 2, &c), Some(22));

    assert_eq!(map.fetch_remove(buf.bytes(), 2, &c), Some(KV { key: 2, value: 22 }));
    assert_eq!(map.fetch_remove(buf.bytes(), 2, &c), None);
    assert!(!map.remove(buf.bytes(), 2, &c));
    assert_eq!(map.get_key(buf.bytes(), 2, &c), None);
    assert_eq!(map.get(buf.bytes(), 2, &c), None);
    assert!(map.remove(buf.bytes(), 3, &c));
}

#[test]
fn ensure_unused_capacity() {
    let (map, mut buf) = auto_map::<u64, u64>(64);
    map.ensure_unused_capacity(buf.bytes(), 32).unwrap();
    assert_eq!(map.ensure_unused_capacity(buf.bytes(), 65), Err(OutOfMemory));
}

#[test]
fn remove_each_after_lookup() {
    // rustty's `removeByPtr` tests: look each key up, then remove it
    let (map, mut buf) = auto_map::<i32, u64>(64);
    let c = ctx();
    for i in 0..10 {
        map.put(buf.bytes(), i, 0, &c).unwrap();
    }
    assert_eq!(map.count(buf.bytes()), 10);
    for i in 0..10 {
        assert_eq!(map.get_key(buf.bytes(), i, &c), Some(i));
        assert!(map.remove(buf.bytes(), i, &c));
    }
    assert_eq!(map.count(buf.bytes()), 0);
}

#[test]
fn repeat_fetch_remove() {
    let (map, mut buf) = auto_map::<u64, u8>(64);
    let c = ctx();
    for key in 0..4 {
        map.put_assume_capacity(buf.bytes(), key, 0, &c);
    }
    // fetchRemove makes slots available
    for _ in 0..10 {
        assert!(map.fetch_remove(buf.bytes(), 3, &c).is_some());
        map.put_assume_capacity(buf.bytes(), 3, 0, &c);
    }
    for key in 0..4 {
        assert!(map.get(buf.bytes(), key, &c).is_some());
    }
}

#[test]
fn offset_map_remakes_from_the_buffer() {
    // the descriptor is a copy of offsets: any copy of it over the same bytes is the same map
    let layout = AutoOffsetHashMap::<u32, u32, 100>::layout(16);
    let (map, mut buf) = map_in::<u32, u32, AutoContext<u32>, 100>(layout);
    let c = ctx();
    map.put(buf.bytes(), 5, 5, &c).unwrap();
    let copy = map;
    assert_eq!(copy.get(buf.bytes(), 5, &c), Some(5));
    let default = AutoOffsetHashMap::<u32, u32, 100>::default();
    assert_eq!(default.capacity(), 0);
    assert!(format!("{map:?}").starts_with("OffsetHashMap { metadata: 4"));
}

#[test]
fn offset_map_maximum_load_leaves_probe_headroom() {
    type Map = AutoOffsetHashMap<u32, u32, 80>;
    let requested = 16;
    let (map, mut buf) = map_in::<u32, u32, AutoContext<u32>, 80>(Map::layout(requested));
    let c = ctx();
    assert!(map.capacity() > requested);
    assert!(map.max_load() >= requested);
    assert!(map.max_load() < map.capacity());
    for i in 0..requested {
        map.put(buf.bytes(), i, i, &c).unwrap();
    }
    for _ in 0..100 {
        for i in 0..requested {
            assert!(map.remove(buf.bytes(), i, &c));
            map.put(buf.bytes(), i, i, &c).unwrap();
        }
    }
    for i in 0..requested {
        assert_eq!(map.get(buf.bytes(), i, &c), Some(i));
    }
}

#[test]
fn offset_map_init_assume_zeroed_writes_nothing() {
    type Map = AutoOffsetHashMap<u32, u32, DEFAULT_MAX_LOAD_PERCENTAGE>;
    let layout = Map::layout(16);
    let mut buf = Buffer::new(layout.total_size);
    let map = Map::init_assume_zeroed(OffsetBuf::init(), layout);
    assert!(buf.bytes().iter().all(|&b| b == 0));
    assert_eq!(map.capacity(), layout.capacity);
    assert_eq!(map.count(buf.bytes()), 0);
    assert_eq!(map.keys(buf.bytes()).next(), None);
    assert!(buf.bytes().iter().all(|&b| b == 0));

    let c = ctx();
    for i in 0..5 {
        map.put(buf.bytes(), i, i * 10, &c).unwrap();
    }
    assert_eq!(map.count(buf.bytes()), 5);
    for i in 0..5 {
        assert_eq!(map.get(buf.bytes(), i, &c), Some(i * 10));
    }
    assert!(map.remove(buf.bytes(), 3, &c));
    assert_eq!(map.count(buf.bytes()), 4);
    map.expect_canonical(buf.bytes(), &c);
}

#[test]
fn offset_map_init_clears_dirty_memory() {
    type Map = AutoOffsetHashMap<u32, u32, DEFAULT_MAX_LOAD_PERCENTAGE>;
    let layout = Map::layout(16);
    let mut buf = Buffer::new(layout.total_size);
    buf.bytes().fill(0xAA);
    let map = Map::init(buf.bytes(), OffsetBuf::init(), layout);
    let c = ctx();
    assert_eq!(map.count(buf.bytes()), 0);
    assert_eq!(map.keys(buf.bytes()).next(), None);
    // the size counter and the metadata are zero; the entries are not touched (a free slot's are never read)
    let metadata_end = layout.metadata_start + layout.capacity as usize;
    assert!(buf.bytes()[..metadata_end].iter().all(|&b| b == 0));
    assert!(buf.bytes()[metadata_end..].iter().all(|&b| b == 0xAA));
    map.put(buf.bytes(), 7, 70, &c).unwrap();
    assert_eq!(map.get(buf.bytes(), 7, &c), Some(70));
    assert_eq!(map.count(buf.bytes()), 1);
}

#[test]
fn layout_offsets_are_from_buffer_start() {
    type Map = AutoOffsetHashMap<u32, u64, DEFAULT_MAX_LOAD_PERCENTAGE>;
    let layout = Map::layout_for_capacity(16);
    // the header is only the size counter
    assert_eq!(layout.metadata_start, 4);
    // metadata ends at 4 + 16 = 20; u32 keys aligned to 4
    assert_eq!(layout.keys_start, 20);
    // keys end at 20 + 64 = 84; u64 values aligned to 8
    assert_eq!(layout.vals_start, 88);
    assert_eq!(layout.total_size, 88 + 16 * 8);
    assert_eq!(Map::base_align(), 8);
    assert_eq!(AutoOffsetHashMap::<u8, u16, 100>::base_align(), 4);
}

#[test]
fn layout_for_capacity_no_overflow_for_large_capacity() {
    // Ghostty #9862: the capacity was multiplied in u32 before widening, which wrapped once
    // `capacity * size_of::<K>()` passed 2^32.
    type Map = AutoOffsetHashMap<u64, u64, DEFAULT_MAX_LOAD_PERCENTAGE>;
    let large: Size = 1 << 30;
    let layout = Map::layout_for_capacity(large);
    assert!(layout.total_size >= large as usize * (size_of::<u64>() + size_of::<u64>()));
    assert!(layout.keys_start > 0);
    assert!(layout.vals_start > layout.keys_start);
}

#[test]
fn layout_for_size_rounds_up_and_clamps() {
    type Map = AutoOffsetHashMap<u32, u32, 80>;
    assert_eq!(Map::layout_for_size(0).capacity, 0);
    // 13 entries at 80% need 17 slots, rounded up to 32
    assert_eq!(Map::layout_for_size(13).capacity, 32);
    assert_eq!(Map::layout_for_size(12).capacity, 16);
    assert_eq!(Map::layout_for_size(Size::MAX).capacity, 1 << 31);
}

#[test]
fn a_full_map_still_finds_existing_keys() {
    let (map, mut buf) = auto_map::<u32, u32>(8);
    let c = ctx();
    for i in 0..8 {
        map.put(buf.bytes(), i, i, &c).unwrap();
    }
    let entry = map.get_or_put(buf.bytes(), 3, &c).unwrap();
    assert!(entry.found_existing);
    assert_eq!(*entry.value, 3);
    assert!(map.get_or_put(buf.bytes(), 100, &c).is_err());
    assert_eq!(map.get_or_put_value(buf.bytes(), 100, 1, &c), Err(OutOfMemory));
    assert_eq!(map.fetch_put(buf.bytes(), 100, 1, &c), Err(OutOfMemory));
    assert_eq!(map.get_mut(buf.bytes(), 100, &c), None);
}

#[test]
fn a_zero_capacity_map_is_empty_and_full() {
    let (map, mut buf) = auto_map::<u32, u32>(0);
    let c = ctx();
    assert_eq!(map.count(buf.bytes()), 0);
    assert_eq!(map.max_load(), 0);
    assert_eq!(map.get(buf.bytes(), 1, &c), None);
    assert_eq!(map.put(buf.bytes(), 1, 1, &c), Err(OutOfMemory));
    assert_eq!(map.iter(buf.bytes()).count(), 0);
    map.expect_canonical(buf.bytes(), &c);
}

#[test]
fn the_automatic_context_hashes_keys_by_their_bytes() {
    let c = ctx::<u32>();
    assert_eq!(
        c.hash(7),
        crate::datastruct::wyhash::Wyhash::hash(0, &7u32.to_ne_bytes())
    );
    assert!(c.eql(7, 7));
    assert!(!c.eql(7, 8));
}
