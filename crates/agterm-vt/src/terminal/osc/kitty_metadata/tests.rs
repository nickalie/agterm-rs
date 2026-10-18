use super::*;

#[test]
fn value_iterator_skips_malformed_and_prefix_matching_keys() {
    //     "id-extra=wrong:id: id = first :id=second",
    let mut it = ValueIterator::init(b"id", None, b"id-extra=wrong:id: id = first :id=second");

    assert_eq!(b"first".as_slice(), it.next().unwrap());
    assert_eq!(b"second".as_slice(), it.next().unwrap());
    assert!(it.next().is_none());
}

#[test]
fn value_iterator_skips_values_containing_disallowed_characters() {
    let mut it = ValueIterator::init(b"i", Some(b"abc"), b"i=a?:i=abc");

    assert_eq!(b"abc".as_slice(), it.next().unwrap());
    assert!(it.next().is_none());
}
