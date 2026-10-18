use super::ComptimeIntSet;

#[test]
fn empty() {
    const SET: ComptimeIntSet<u16, 0> = ComptimeIntSet::<u16, 0>::new([]);
    assert!(SET.index_of(0).is_none());
    assert!(SET.index_of(1).is_none());
}

#[test]
fn single() {
    const SET: ComptimeIntSet<u16, 1> = ComptimeIntSet::<u16, 1>::new([42]);
    assert_eq!(Some(0), SET.index_of(42));
    assert!(SET.index_of(41).is_none());
    assert!(SET.index_of(43).is_none());
}

#[test]
fn unsorted_keys_return_original_index() {
    const KEYS: [u16; 7] = [2026, 4, 1049, 25, 1, 65535, 0];
    const SET: ComptimeIntSet<u16, 7> = ComptimeIntSet::<u16, 7>::new(KEYS);
    for (i, &key) in KEYS.iter().enumerate() {
        assert_eq!(Some(i), SET.index_of(key));
    }
}

/// The keys `(i * 37 + 11) % 251` for `i < N`, and their set, both built at compile time.
struct Keys<const N: usize>;

impl<const N: usize> Keys<N> {
    const KEYS: [u16; N] = {
        let mut result = [0_u16; N];
        let mut i = 0;
        // `key` is `(i * 37 + 11) % 251`, stepped.
        let mut key: u16 = 11;
        while i < N {
            result[i] = key;
            key = (key + 37) % 251;
            i += 1;
        }
        result
    };
    const SET: ComptimeIntSet<u16, N> = ComptimeIntSet::<u16, N>::new(Self::KEYS);
}

fn check<const N: usize>() {
    let keys = Keys::<N>::KEYS;
    for v in 0..=u16::from(u8::MAX) {
        let expected = keys.iter().position(|&key| key == v);
        assert_eq!(expected, Keys::<N>::SET.index_of(v), "n={N} v={v}");
    }
}

/// The search unrolls differently for each length, so every length from 0 up past a few powers of two is checked
/// against a linear scan.
#[test]
fn exhaustive_against_linear_scan() {
    check::<0>();
    check::<1>();
    check::<2>();
    check::<3>();
    check::<4>();
    check::<5>();
    check::<6>();
    check::<7>();
    check::<8>();
    check::<9>();
    check::<10>();
    check::<11>();
    check::<12>();
    check::<13>();
    check::<14>();
    check::<15>();
    check::<16>();
    check::<17>();
    check::<18>();
    check::<19>();
}
