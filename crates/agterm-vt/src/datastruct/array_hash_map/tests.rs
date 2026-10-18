//! The insertion-ordered map's tests, rustty's (`rustty-zigstd/src/array_hash_map.rs` at `RUSTTY_REV`) on `std`.

use super::{AutoArrayHashMap, GetOrPutResult, KV};
use crate::mem::alloc::OutOfMemory;
use crate::mem::testing::{with_counting, with_failing};

fn keys(m: &AutoArrayHashMap<u32, u32>) -> Vec<u32> {
    m.keys().copied().collect()
}

#[test]
fn insertion_order_iteration() {
    let mut m = AutoArrayHashMap::new();
    for (k, v) in [(30, 300), (10, 100), (20, 200), (5, 50)] {
        m.put(k, v);
    }
    assert_eq!(keys(&m), [30, 10, 20, 5]);
    assert_eq!(m.values().copied().collect::<Vec<_>>(), [300, 100, 200, 50]);
    let pairs: Vec<(u32, u32)> = m.iter().map(|(k, v)| (*k, *v)).collect();
    assert_eq!(pairs, [(30, 300), (10, 100), (20, 200), (5, 50)]);
}

#[test]
fn put_overwrites_in_place() {
    let mut m = AutoArrayHashMap::new();
    m.put("a", 1);
    m.put("b", 2);
    m.put("c", 3);
    m.put("b", 22);
    assert_eq!(
        m.keys().copied().collect::<Vec<_>>(),
        ["a", "b", "c"],
        "order unchanged"
    );
    assert_eq!(m.get(&"b"), Some(&22));
    assert_eq!(m.count(), 3);
}

#[test]
fn get_or_put_reports_existing() {
    let mut m = AutoArrayHashMap::new();
    assert_eq!(
        m.get_or_put_value(1, 10),
        GetOrPutResult {
            index: 0,
            found_existing: false
        }
    );
    assert_eq!(
        m.get_or_put_value(2, 20),
        GetOrPutResult {
            index: 1,
            found_existing: false
        }
    );
    let r = m.get_or_put_value(1, 99);
    assert_eq!(
        r,
        GetOrPutResult {
            index: 0,
            found_existing: true
        }
    );
    assert_eq!(m.get(&1), Some(&10), "an existing value is kept");
    *m.value_at_mut(r.index) = 11;
    assert_eq!(*m.value_at(0), 11);
    let r = m.get_or_put_with(3, || 30);
    assert!(!r.found_existing);
    assert_eq!(m.entry_at(2), Some((&3, &30)));
    assert_eq!(m.entry_at(3), None);
}

#[test]
#[should_panic(expected = "the key is present")]
fn put_no_clobber_panics_on_dup() {
    let mut m = AutoArrayHashMap::new();
    m.put_no_clobber(1u32, 10u32);
    m.put_no_clobber(1, 20);
}

#[test]
fn swap_remove_changes_order() {
    let mut m = AutoArrayHashMap::new();
    for k in [1, 2, 3, 4, 5] {
        m.put(k, k * 10);
    }
    assert!(m.swap_remove(&2));
    assert_eq!(keys(&m), [1, 5, 3, 4], "5 took 2's slot");
    assert_eq!(m.count(), 4);
    assert_eq!(m.get(&5), Some(&50));
    assert_eq!(m.get_index(&5), Some(1));
    assert_eq!(m.get(&2), None);
    assert!(!m.contains(&2));
    assert!(!m.swap_remove(&2));
}

#[test]
fn swap_remove_last() {
    let mut m = AutoArrayHashMap::new();
    for k in [1, 2, 3] {
        m.put(k, k);
    }
    assert!(m.swap_remove(&3));
    assert_eq!(keys(&m), [1, 2]);
    assert_eq!(m.get_index(&2), Some(1));
}

#[test]
fn ordered_remove_preserves_order() {
    let mut m = AutoArrayHashMap::new();
    for k in [1, 2, 3, 4, 5] {
        m.put(k, k * 10);
    }
    assert!(m.ordered_remove(&2));
    assert_eq!(keys(&m), [1, 3, 4, 5]);
    assert_eq!(m.count(), 4);
    for (i, k) in [1, 3, 4, 5].into_iter().enumerate() {
        assert_eq!(m.get_index(&k), Some(i));
    }
    assert_eq!(m.get(&5), Some(&50));
    assert!(!m.ordered_remove(&2));
}

#[test]
fn swap_vs_ordered_ordering_contrast() {
    let make = || {
        let mut m = AutoArrayHashMap::new();
        for k in [10, 20, 30, 40] {
            m.put(k, k);
        }
        m
    };
    let mut a = make();
    a.swap_remove(&20);
    assert_eq!(keys(&a), [10, 40, 30]);
    let mut b = make();
    b.ordered_remove(&20);
    assert_eq!(keys(&b), [10, 30, 40]);
}

#[test]
fn removal_by_index() {
    let mut m = AutoArrayHashMap::new();
    for k in [1, 2, 3, 4] {
        m.put(k, k);
    }
    m.swap_remove_index(0);
    assert_eq!(keys(&m), [4, 2, 3]);
    m.ordered_remove_index(0);
    assert_eq!(keys(&m), [2, 3]);
    assert_eq!(m.get_index(&3), Some(1));
}

#[test]
fn fetch_remove_variants() {
    let mut m = AutoArrayHashMap::new();
    for k in [1, 2, 3] {
        m.put(k, k * 100);
    }
    assert_eq!(m.fetch_swap_remove(&2), Some(KV { key: 2, value: 200 }));
    assert_eq!(m.fetch_swap_remove(&999), None);

    let mut m2 = AutoArrayHashMap::new();
    for k in [1, 2, 3] {
        m2.put(k, k * 100);
    }
    assert_eq!(m2.fetch_ordered_remove(&1), Some(KV { key: 1, value: 100 }));
    assert_eq!(keys(&m2), [2, 3]);
}

#[test]
fn accessors() {
    let mut m = AutoArrayHashMap::new();
    assert!(m.is_empty());
    m.put(7, 70);
    assert_eq!(m.count(), 1);
    assert!(!m.is_empty());
    assert!(m.contains(&7));
    assert_eq!(m.get(&7), Some(&70));
    assert_eq!(m.get_key(&7), Some(&7));
    assert_eq!(m.get_index(&7), Some(0));
    *m.get_mut(&7).unwrap() = 71;
    assert_eq!(m.get(&7), Some(&71));
    assert_eq!(m.get(&8), None);
    assert_eq!(m.get_mut(&8), None);
    for v in m.values_mut() {
        *v += 1;
    }
    assert_eq!(m.get(&7), Some(&72));
}

#[test]
fn a_set_of_unit_values() {
    let a = Box::new(1u8);
    let b = Box::new(2u8);
    let (pa, pb): (*const u8, *const u8) = (&raw const *a, &raw const *b);
    let mut set: AutoArrayHashMap<*const u8, ()> = AutoArrayHashMap::new();
    set.put(pa, ());
    set.put(pb, ());
    assert_eq!(set.count(), 2);
    assert!(set.swap_remove(&pa));
    assert!(!set.contains(&pa));
    assert!(set.contains(&pb));
    assert_eq!(set.count(), 1);
}

#[test]
fn pop_last() {
    let mut m = AutoArrayHashMap::new();
    m.put(1, 1);
    m.put(2, 2);
    assert_eq!(m.pop(), Some(KV { key: 2, value: 2 }));
    assert_eq!(m.count(), 1);
    assert!(!m.contains(&2));
    assert_eq!(m.pop(), Some(KV { key: 1, value: 1 }));
    assert_eq!(m.pop(), None);
}

#[test]
fn sort_applies_the_permutation_in_place() {
    fn permutations(v: &mut Vec<u32>, k: usize, out: &mut Vec<Vec<u32>>) {
        if k == v.len() {
            out.push(v.clone());
            return;
        }
        for i in k..v.len() {
            v.swap(k, i);
            permutations(v, k + 1, out);
            v.swap(k, i);
        }
    }
    let mut all = Vec::new();
    permutations(&mut vec![4, 1, 3, 0, 2], 0, &mut all);
    assert_eq!(all.len(), 120);
    for order in all {
        let mut m = AutoArrayHashMap::new();
        for &k in &order {
            m.put(k, k * 10);
        }
        m.sort(|a, b| order[a] < order[b]);
        assert_eq!(keys(&m), [0, 1, 2, 3, 4]);
        assert_eq!(m.values().copied().collect::<Vec<_>>(), [0, 10, 20, 30, 40]);
        for k in 0..5 {
            assert_eq!(m.get_index(&k), Some(usize::try_from(k).unwrap()));
        }
    }
}

#[test]
fn try_growth_fails_cleanly_and_storage_is_freed() {
    let ((), counts) = with_counting(|| {
        let mut m: AutoArrayHashMap<u32, u32> = AutoArrayHashMap::new();
        m.try_put_no_clobber(1, 10).unwrap();
        m.try_ensure_total_capacity(64).unwrap();
        while m.count() < 64 {
            let k = u32::try_from(m.count()).unwrap() + 1;
            m.try_put(k, k).unwrap();
        }
        // at capacity: the next insertion has to grow, and the refused growth leaves the map unchanged
        let (result, _) = with_failing(0, || m.try_put_no_clobber(1000, 1000));
        assert_eq!(result, Err(OutOfMemory));
        assert!(!m.contains(&1000));
        let (result, _) = with_failing(0, || m.try_get_or_put_with(1001, || 0));
        assert_eq!(result, Err(OutOfMemory));
        let (result, _) = with_failing(0, || m.try_ensure_total_capacity(1000));
        assert_eq!(result, Err(OutOfMemory));
        // an existing key needs no growth
        let before = m.count();
        let (result, stats) = with_failing(0, || m.try_put(1, 11));
        assert_eq!(result, Ok(()));
        assert!(!stats.has_induced_failure);
        assert_eq!(m.get(&1), Some(&11));
        assert_eq!(m.count(), before);
        let (found, _) = with_failing(0, || m.try_get_or_put_with(1, || 0));
        assert_eq!(
            found,
            Ok(GetOrPutResult {
                index: 0,
                found_existing: true
            })
        );

        m.clear_and_free();
        assert!(m.is_empty());
        m.put(7, 70);
        m.clear_retaining_capacity();
        assert!(m.is_empty());
        m.put_assume_capacity_no_clobber(8, 80);
        assert_eq!(m.get(&8), Some(&80));
        let copy = m.clone();
        assert_eq!(copy.get(&8), Some(&80));
    });
    assert_eq!(counts.live_bytes, 0, "dropping the maps frees everything");
}
