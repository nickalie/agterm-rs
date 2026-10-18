//! The ref-counted set's tests: Ghostty's (`ref_counted_set.zig` at `LIBGHOSTTY_VT_REV`) and rustty's, over offsets
//! into a test buffer, plus the safe API's own.

use super::{AddError, Item, Layout, RefCountedSet, RefCountedSetContext, RefItemGeometry, drop_ref};
use crate::mem::offset::OffsetBuf;

/// `u32` values: the reference's `Item` for `RefCountedSet(u32, u16, u16, ...)` is `{ value: u32, meta: { u16, u16,
/// u16 } }`, 10 bytes rounded to 12, aligned to 4.
impl RefItemGeometry for u32 {
    const REF_ITEM_SIZE: usize = 12;
    const REF_ITEM_ALIGN: usize = 4;
}

/// The reference test context: `std.hash.int(value)` and equality.
#[derive(Default)]
struct TestContext {
    deleted: Vec<u32>,
}

impl RefCountedSetContext<u32> for TestContext {
    fn hash(&self, _: &[u8], value: &u32) -> u64 {
        u64::from(hash_int_u32(*value))
    }

    fn eql(&self, _: &[u8], a: &u32, b: &u32) -> bool {
        a == b
    }

    fn deleted(&mut self, _: &mut [u8], value: u32) {
        self.deleted.push(value);
    }
}

/// Zig's `std.hash.int` for `u32` (a 32-bit integer finalizer).
fn hash_int_u32(input: u32) -> u32 {
    let mut x = input;
    x = (x ^ (x >> 16)).wrapping_mul(0x7feb_352d);
    x = (x ^ (x >> 15)).wrapping_mul(0x846c_a68b);
    x ^ (x >> 16)
}

type TestSet = RefCountedSet<u32, u16, u16>;

/// A zeroed test buffer aligned for the set (`alloc.alignedAlloc(u8, base_align, total_size)`).
struct Buffer(Vec<u64>);

impl Buffer {
    fn new(len: usize) -> Self {
        Self(vec![0; len.max(1).div_ceil(8)])
    }

    fn bytes(&mut self) -> &mut [u8] {
        bytemuck::cast_slice_mut(&mut self.0)
    }
}

/// A set for a table of `cap` buckets over a fresh buffer garbaged first, so `init` must clear it.
fn set(cap: usize) -> (TestSet, Buffer, TestContext) {
    let layout = Layout::init::<u32, u16>(cap);
    let mut buf = Buffer::new(layout.total_size);
    buf.bytes().fill(0xAA);
    let set = TestSet::init(buf.bytes(), OffsetBuf::init(), layout);
    (set, buf, TestContext::default())
}

#[test]
fn layout_uses_the_reference_item_geometry() {
    let layout = Layout::<u16>::init::<u32, u16>(8);
    assert_eq!(layout.table_cap, 8);
    assert_eq!(layout.cap, 6, "0.8125 of 8 buckets");
    assert_eq!(layout.table_mask, 7);
    assert_eq!(layout.items_start, 16);
    assert_eq!(layout.total_size, 16 + 6 * 12);
    assert_eq!(TestSet::base_align(), align_of::<usize>());
    assert_eq!(
        Item::<u32, u16, u16>::REF_COUNT + 2,
        10,
        "the record fits the reference's 12 bytes"
    );
    let empty = Layout::<u16>::init::<u32, u16>(0);
    assert_eq!((empty.cap, empty.table_cap, empty.total_size), (0, 0, 0));
    assert_eq!(TestSet::capacity_for_count(0), 0);
    assert_eq!(TestSet::capacity_for_count(12), 16);
}

#[test]
fn add_with_id_dead_id_resolving_to_an_existing_value() {
    let (mut set, mut buf, mut ctx) = set(8);
    let mem = buf.bytes();

    // A dead item between two live ones, which the trim in `add` cannot reap: a page's style set after a styled run
    // is erased.
    let live = set.add(mem, 11, &mut ctx).unwrap();
    let released = set.add(mem, 22, &mut ctx).unwrap();
    let _last = set.add(mem, 33, &mut ctx).unwrap();
    set.release(mem, released);
    assert_eq!(set.count(), 2);

    // The dead id requested for a value live under another id resolves to that item and references it, without
    // changing the live count.
    let resolved = set.add_with_id(mem, 11, released, &mut ctx).unwrap();
    assert_eq!(resolved, Some(live));
    assert_eq!(set.ref_count(mem, live), 2);
    assert_eq!(set.count(), 2);
    assert_eq!(
        set.count(),
        set.iter(mem).count(),
        "the live count agrees with the iterator"
    );

    // The dead id is still reusable for an absent value: the requested id is used.
    assert_eq!(set.add_with_id(mem, 44, released, &mut ctx), Ok(None));
    assert_eq!(set.ref_count(mem, released), 1);
    assert_eq!(set.count(), 3);
    assert_eq!(ctx.deleted, [11, 22], "the matched value, then the dead item replaced");
}

#[test]
fn iterator_visits_live_entries_in_id_order() {
    let (mut set, mut buf, mut ctx) = set(8);
    let mem = buf.bytes();
    let first = set.add(mem, 11, &mut ctx).unwrap();
    let released = set.add(mem, 22, &mut ctx).unwrap();
    let last = set.add(mem, 33, &mut ctx).unwrap();
    assert_eq!(set.add(mem, 11, &mut ctx), Ok(first));
    set.release(mem, released);
    let entries: Vec<(u16, u32)> = set.iter(mem).collect();
    assert_eq!(entries, [(first, 11), (last, 33)]);
    assert_eq!(set.get(mem, last), 33);
}

/// A set state for the pure-bump test: built in a set, the value and id then passed to `add_with_id`.
type Build = fn(&mut TestSet, &mut [u8], &mut TestContext) -> (u32, u16);

/// Fills the set with distinct values from 100 until `next_id` reaches the capacity.
fn fill_to_cap(set: &mut TestSet, mem: &mut [u8], ctx: &mut TestContext) {
    let mut value = 100;
    while usize::from(set.next_id) < set.layout().cap {
        set.add(mem, value, ctx).unwrap();
        value += 1;
    }
}

/// The states of `pure_ref_bump_target_matches_add_with_id_in_every_arm`: a label, the table capacity, whether the
/// predicate must claim a pure bump, and the builder.
fn pure_bump_states() -> Vec<(&'static str, usize, bool, Build)> {
    vec![
        ("live and equal", 8, true, |s, m, c| (11, s.add(m, 11, c).unwrap())),
        ("dead, value live elsewhere", 8, true, |s, m, c| {
            s.add(m, 11, c).unwrap();
            let dead = s.add(m, 22, c).unwrap();
            s.add(m, 33, c).unwrap();
            s.release(m, dead);
            (11, dead)
        }),
        ("dead, value absent (insert)", 8, false, |s, m, c| {
            s.add(m, 11, c).unwrap();
            let dead = s.add(m, 22, c).unwrap();
            s.add(m, 33, c).unwrap();
            s.release(m, dead);
            (44, dead)
        }),
        ("alive not equal, value present, live top", 8, true, |s, m, c| {
            let a = s.add(m, 11, c).unwrap();
            s.add(m, 22, c).unwrap();
            (22, a)
        }),
        (
            "alive not equal, value present, dead top (trim)",
            8,
            false,
            |s, m, c| {
                let a = s.add(m, 11, c).unwrap();
                s.add(m, 22, c).unwrap();
                let top = s.add(m, 33, c).unwrap();
                s.release(m, top);
                (22, a)
            },
        ),
        ("alive not equal, value absent (insert)", 8, false, |s, m, c| {
            (22, s.add(m, 11, c).unwrap())
        }),
        ("id >= next_id, value present", 8, true, |s, m, c| {
            s.add(m, 11, c).unwrap();
            s.add(m, 22, c).unwrap();
            (22, 5)
        }),
        ("id >= next_id, value absent (insert)", 8, false, |s, m, c| {
            s.add(m, 11, c).unwrap();
            (22, 5)
        }),
        ("at cap, value present", 8, true, |s, m, c| {
            fill_to_cap(s, m, c);
            (101, 1)
        }),
        ("at cap, all live, value absent (OutOfMemory)", 8, false, |s, m, c| {
            fill_to_cap(s, m, c);
            (7, 1)
        }),
        (
            "at cap, below the living threshold, value absent (NeedsRehash)",
            8,
            false,
            |s, m, c| {
                fill_to_cap(s, m, c);
                // ids that are not the top, so no trim reaps them
                s.release(m, 1);
                s.release(m, 2);
                (7, 3)
            },
        ),
        ("zero-capacity set", 0, false, |_, _, _| (7, 1)),
    ]
}

/// The set's own state beside its buffer.
fn header(set: &TestSet) -> (u16, usize, u16, [u16; 32]) {
    (set.next_id, set.living, set.max_psl, set.psl_stats)
}

/// Runs `add_with_id(value, id)` and returns the id it took exactly one reference on, if that was all it did.
fn pure_bump_of(
    set: &mut TestSet,
    buf: &mut Buffer,
    ctx: &mut TestContext,
    value: u32,
    id: u16,
) -> (Option<u16>, Result<Option<u16>, AddError>) {
    let before = buf.bytes().to_vec();
    let header_before = header(set);
    let result = set.add_with_id(buf.bytes(), value, id, ctx);
    let bumped = result.ok().and_then(|r| {
        let target = r.unwrap_or(id);
        let pure = header(set) == header_before
            && usize::from(target) < set.layout().cap
            && set.ref_count(buf.bytes(), target) >= 2
            && {
                set.release(buf.bytes(), target);
                buf.bytes() == &before[..]
            };
        pure.then_some(target)
    });
    (bumped, result)
}

#[test]
fn pure_ref_bump_target_matches_add_with_id_in_every_arm() {
    // For each state, `Some(x)` from the predicate must mean `add_with_id` hands back `x` (`None` for the requested
    // id) and changes nothing but `x`'s count by one; `None` must mean the call does more (or fails).
    let mut claimed = 0;
    for (label, cap, expect_pure, build) in pure_bump_states() {
        let (mut set, mut buf, mut ctx) = set(cap);
        let (value, id) = build(&mut set, buf.bytes(), &mut ctx);
        let target = set.add_with_id_pure_ref_bump_target(buf.bytes(), &value, id, &ctx);
        let (bumped, result) = pure_bump_of(&mut set, &mut buf, &mut ctx, value, id);
        assert_eq!(target, bumped, "{label}");
        if let Some(x) = target {
            claimed += 1;
            assert_eq!(result, Ok((x != id).then_some(x)), "{label}");
        }
        assert_eq!(
            target.is_some(),
            expect_pure,
            "{label}: the state did not exercise its arm"
        );
    }
    assert_eq!(claimed, 5);
    let (set, mut buf, ctx) = set(8);
    assert_eq!(
        set.add_with_id_pure_ref_bump_target(buf.bytes(), &1, 0, &ctx),
        None,
        "id 0 is reserved"
    );
}

#[test]
fn the_dead_id_arm_saturates_at_the_ceiling() {
    let (mut set, mut buf, mut ctx) = set(32);
    let mem = buf.bytes();
    let near = near_the_ceiling;

    // `add_with_id`'s dead-id arm references the live item of the value
    let live = set.add(mem, 11, &mut ctx).unwrap();
    let dead = set.add(mem, 22, &mut ctx).unwrap();
    let _keep = set.add(mem, 33, &mut ctx).unwrap();
    set.release(mem, dead);
    near(&set, mem, live);
    assert_eq!(set.add_with_id(mem, 11, dead, &mut ctx), Ok(Some(live)));
    assert_eq!(set.ref_count(mem, live), u16::MAX, "exactly the ceiling");
    assert_eq!(set.add_with_id(mem, 11, dead, &mut ctx), Ok(Some(live)));
    assert_eq!(set.ref_count(mem, live), u16::MAX, "saturated");
}

/// Raises an item's count to one below the ceiling.
fn near_the_ceiling(set: &TestSet, mem: &mut [u8], id: u16) {
    let count = set.ref_count(mem, id);
    set.use_multiple(mem, id, u16::MAX - 1 - count);
    assert_eq!(set.ref_count(mem, id), u16::MAX - 1);
}

#[test]
fn the_other_increment_arms_saturate_and_pin_at_the_ceiling() {
    let (mut set, mut buf, mut ctx) = set(32);
    let mem = buf.bytes();
    let near = near_the_ceiling;

    // `add`'s existing-item arm
    let a = set.add(mem, 44, &mut ctx).unwrap();
    near(&set, mem, a);
    for _ in 0..2 {
        assert_eq!(set.add(mem, 44, &mut ctx), Ok(a));
    }
    assert_eq!(set.ref_count(mem, a), u16::MAX);

    // `add_with_id`'s equal-id arm
    let b = set.add(mem, 55, &mut ctx).unwrap();
    near(&set, mem, b);
    for _ in 0..2 {
        assert_eq!(set.add_with_id(mem, 55, b, &mut ctx), Ok(None));
    }
    assert_eq!(set.ref_count(mem, b), u16::MAX);

    // `use_id`
    let c = set.add(mem, 66, &mut ctx).unwrap();
    near(&set, mem, c);
    set.use_id(mem, c);
    set.use_id(mem, c);
    assert_eq!(set.ref_count(mem, c), u16::MAX);
    assert!(set.ref_count_saturated(mem, c));

    // `use_multiple`, past the ceiling in one call
    let d = set.add(mem, 77, &mut ctx).unwrap();
    set.use_multiple(mem, d, u16::MAX);
    assert_eq!(set.ref_count(mem, d), u16::MAX);

    // a pinned count is never decremented again
    set.release(mem, d);
    set.release_multiple(mem, d, 100);
    assert_eq!(set.ref_count(mem, d), u16::MAX, "pinned");
}

#[test]
fn init_assume_zeroed_writes_nothing_and_matches_init() {
    let layout = Layout::init::<u32, u16>(16);
    let mut zeroed = Buffer::new(layout.total_size);
    let mut dirty = Buffer::new(layout.total_size);
    dirty.bytes().fill(0xAA);
    let mut zset = TestSet::init_assume_zeroed(OffsetBuf::init(), layout);
    assert!(zeroed.bytes().iter().all(|&b| b == 0));
    let mut dset = TestSet::init(dirty.bytes(), OffsetBuf::init(), layout);
    let (mut zctx, mut dctx) = (TestContext::default(), TestContext::default());

    let mut ids = Vec::new();
    for v in 1..=10 {
        let z = zset.add(zeroed.bytes(), v * 7, &mut zctx).unwrap();
        let d = dset.add(dirty.bytes(), v * 7, &mut dctx).unwrap();
        assert_eq!(d, z);
        ids.push(z);
    }
    for &id in &ids[2..6] {
        zset.release(zeroed.bytes(), id);
        dset.release(dirty.bytes(), id);
    }
    assert_eq!(dset.count(), zset.count());
    for v in 20..=22 {
        assert_eq!(
            dset.add(dirty.bytes(), v, &mut dctx),
            zset.add(zeroed.bytes(), v, &mut zctx)
        );
    }
    for v in (1..=10).map(|v| v * 7).chain(20..=22) {
        assert_eq!(
            dset.lookup(dirty.bytes(), &v, &dctx),
            zset.lookup(zeroed.bytes(), &v, &zctx)
        );
    }
    assert_eq!(dset.next_id, zset.next_id);
    assert_eq!(dctx.deleted, zctx.deleted);
}

#[test]
fn exhaustion_reports_out_of_memory_then_needs_rehash() {
    let (mut set, mut buf, mut ctx) = set(8);
    let mem = buf.bytes();
    // ids 1..=5 of the 6-item capacity
    let ids: Vec<u16> = (0..5).map(|v| set.add(mem, v, &mut ctx).unwrap()).collect();
    assert_eq!(ids, [1, 2, 3, 4, 5]);
    assert_eq!(set.add(mem, 99, &mut ctx), Err(AddError::OutOfMemory), "all live");
    // releasing items below the top leaves too few live ones: rehash
    set.release(mem, 1);
    set.release(mem, 2);
    assert_eq!(set.add(mem, 99, &mut ctx), Err(AddError::NeedsRehash));
    // a released top item is trimmed, which frees its id
    set.release(mem, 5);
    assert_eq!(set.add(mem, 99, &mut ctx), Ok(5));
    assert_eq!(ctx.deleted, [4]);
    assert_eq!(set.lookup(mem, &4, &ctx), None);
}

#[test]
fn a_zero_capacity_set_holds_nothing() {
    let (mut set, mut buf, mut ctx) = set(0);
    let mem = buf.bytes();
    assert_eq!(set.lookup(mem, &1, &ctx), None);
    assert_eq!(set.add(mem, 1, &mut ctx), Err(AddError::OutOfMemory));
    assert_eq!(set.iter(mem).count(), 0);
}

#[test]
fn many_values_survive_releases_and_reinsertions() {
    let (mut set, mut buf, mut ctx) = set(256);
    let mem = buf.bytes();
    let mut ids = std::collections::HashMap::new();
    for v in 0..150u32 {
        ids.insert(v, set.add(mem, v * 3, &mut ctx).unwrap());
    }
    for v in (0..150u32).filter(|v| v % 3 == 0) {
        set.release(mem, ids[&v]);
    }
    for v in 0..150u32 {
        let found = set.lookup(mem, &(v * 3), &ctx);
        assert_eq!(found, (v % 3 != 0).then_some(ids[&v]), "value {}", v * 3);
    }
    // re-adding a released value before its slot is reused resurrects it
    let resurrected = set.add(mem, 0, &mut ctx).unwrap();
    assert_eq!(set.get(mem, resurrected), 0);
    assert_eq!(set.count(), 101);
    let copy = set;
    assert_eq!(
        copy.count(),
        set.count(),
        "the descriptor is a copy of offsets and counters"
    );
}

#[test]
fn a_release_reports_death_once() {
    assert_eq!(drop_ref(2u16, 1), (1, false));
    assert_eq!(drop_ref(1u16, 1), (0, true), "the last reference kills the item");
    assert_eq!(
        drop_ref(1u16, 3),
        (0, true),
        "an over-release stops at zero, reported once"
    );
    assert_eq!(
        drop_ref(0u16, 1),
        (0, false),
        "a release of a dead item is no second death"
    );
    assert_eq!(drop_ref(u16::MAX, 1), (u16::MAX, false), "a pinned count stays pinned");
}

/// A double release is a caller bug: debug builds assert on it, and without debug assertions it leaves the living
/// count alone instead of dropping it below the live items (or wrapping it from zero).
#[test]
#[cfg_attr(
    debug_assertions,
    should_panic(expected = "release of more references than the item has")
)]
fn a_double_release_keeps_the_living_count() {
    let (mut set, mut buf, mut ctx) = set(8);
    let mem = buf.bytes();
    let a = set.add(mem, 1, &mut ctx).unwrap();
    set.release(mem, a);
    assert_eq!(set.count(), 0);
    set.release(mem, a);
    assert_eq!(set.count(), 0, "the dead item is not counted dead twice");
    let b = set.add(mem, 2, &mut ctx).unwrap();
    assert_eq!(set.count(), 1);
    assert_eq!(set.ref_count(mem, b), 1);
}
